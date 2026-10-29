//! Monte Carlo (TODO §8) — the consumers of the parsed `LOT`/`DEV` tolerance
//! slots and the per-run statistics.
//!
//! The card vocabulary lives in [`crate::netlist::McCommand`]; this module
//! owns the draws it drives. Per run the deck is re-parsed (the same route
//! the `.step` loop takes, so `random()`/`gauss()` expressions re-roll with
//! it), then every model card carrying tolerance slots is *re-bound
//! per instance*: a scaled clone replaces the model under the element's own
//! pointer, so stamps resolve the run's luck through the ordinary
//! `device_model` lookup and nothing downstream needs to know MC exists.
//!
//! ## Documented interpretation (PSpice grammar, unverifiable here)
//!
//! - `LOT=<n>` and `DEV=<n>` are **percentages in both spellings** (`3` and
//!   `3%` — the flag carries the card's spelling; like every tolerance slot
//!   today, emit does not re-print these, the deck keeps its own card).
//! - They combine additively: an instance's factor is `1 + lot + dev`, where
//!   `lot` is one draw per (run, model) — shared by every element of that
//!   model within the run — and `dev` is one draw per (run, model, element).
//! - Draws are uniform over ±the tolerance (PSpice's default distribution),
//!   keyed on the run seed so spreads are bit-reproducible from `.options
//!   seed=` / `.mc seed=`.
//! - A family's factor multiplies its σ-dominant parameters: diodes `IS`,
//!   MOSFETs `KP`+`VTO`, BJTs `IS`, JFETs `BETA`+`VTO`, op-amps `GAIN`,
//!   and passives the nominal value (`R=`/`C=`/`L=` plus an element-carried
//!   value). Everything else on the card — temperature coefficients,
//!   parasitic slots and the arms they materialized into, geometry — stays
//!   nominal on purpose, and `GenericModel` cards (nothing stamps them) are
//!   skipped.

use std::collections::HashMap;

use crate::analysis::{MeasureResult, SimulationResult};
use crate::circuit::{
    Bjt, BjtModel, Capacitor, Circuit, Diode, DiodeModel, Inductor, Jfet, JfetModel,
    ModelTolerance, Mosfet, MosfetModel, OpAmp, OpAmpModel, PassiveModel, Resistor,
};
use crate::netlist::{McCommand, McOp};
use crate::params::mix64;

// ─── Run seeding ────────────────────────────────────────────────────────────

/// Salt the run index mixes into the base seed: every MC run lands on a
/// different draw landscape, bit-reproducibly.
pub(crate) const RUN_SALT: u64 = 0x4D_43_52_55_4E_31;
/// The hidden override key the re-parse uses to re-roll the parameter draws
/// per run (a name no `.param` can legally carry). See `seed_from_overrides`.
pub(crate) const RUN_KEY: &str = " mc-run";
/// The seed for MC run `run`: the base mixed with the run index. Drives both
/// the per-run model value draws and the runtime keyed draws (it becomes the
/// run deck's `mc_seed`).
pub(crate) fn run_mix(base: u64, run: usize) -> u64 {
    mix64(mix64(base ^ RUN_SALT) ^ u64::try_from(run).unwrap_or(0))
}

// ─── Seeded draws ───────────────────────────────────────────────────────────

/// One uniform draw from `[−1, 1)` keyed on the sequence of `tags` under
/// `seed`: the same (family, model, element) rolls the same number for a
/// given run seed and rolls independently of anything else.
fn draw(seed: u64, tags: &[&str]) -> f64 {
    let mut word = seed;
    for tag in tags {
        for &byte in tag.as_bytes() {
            word = mix64(word ^ u64::from(byte));
        }
    }
    // Top 53 bits → [0, 1) → [−1, 1).
    (mix64(word) >> 11) as f64 * (2.0 / (1u64 << 53) as f64) - 1.0
}

/// The per-instance multiplier for one element of `model` in `family`:
/// `1 + lot_frac·u_lot + dev_frac·u_dev` (see the module doc). `u_lot` keys
/// off the model only, so it is shared by every element within the run;
/// `u_dev` keys off the element too.
fn instance_factor(
    tolerance: &ModelTolerance,
    seed: u64,
    family: &str,
    model: &str,
    element: &str,
) -> f64 {
    let lot_pct = tolerance.lot.map_or(0.0, |(v, _)| v);
    let dev_pct = tolerance.dev.map_or(0.0, |(v, _)| v);
    let u_lot = draw(seed, &[family, "lot", model]);
    let u_dev = draw(seed, &[family, "dev", model, element]);
    1.0 + u_lot * lot_pct / 100.0 + u_dev * dev_pct / 100.0
}

// ─── Per-family scaling ─────────────────────────────────────────────────────

/// A model card that can be scaled per MC run. [`McModel::scaled`] multiplies
/// the family's documented parameter set and preserves every other field.
trait McModel: Clone {
    /// The card's tolerance slots.
    fn tolerance(&self) -> &ModelTolerance;
    /// A scaled clone for one instance.
    fn scaled(&self, factor: f64) -> Self;
}

macro_rules! device_mc {
    ($m:ty, $($fields:ident),+) => {
        impl McModel for $m {
            fn tolerance(&self) -> &ModelTolerance {
                &self.tolerance
            }
            fn scaled(&self, factor: f64) -> Self {
                let mut card = self.clone();
                $( card.$fields *= factor; )*
                card
            }
        }
    };
}

device_mc!(DiodeModel, is);
device_mc!(MosfetModel, kp, vth0);
device_mc!(BjtModel, is);
device_mc!(JfetModel, beta, vto);
device_mc!(OpAmpModel, gain);

impl McModel for PassiveModel {
    fn tolerance(&self) -> &ModelTolerance {
        &self.tolerance
    }
    fn scaled(&self, factor: f64) -> Self {
        let mut card = self.clone();
        card.value = card.value.map(|v| v * factor);
        card
    }
}

/// An element family that points at its model card by name (or at none).
trait RefsModel {
    /// The element's own name (a draw key).
    fn element_name(&self) -> &str;
    /// The referenced model name; `None` when there is nothing to draw on
    /// (parasitic arms carry no reference, a bare `U` uses the defaults).
    fn model_of(&self) -> Option<&str>;
    /// Re-point the element at the per-run clone.
    fn set_model(&mut self, name: String);
}

macro_rules! refs_str {
    ($t:ty) => {
        impl RefsModel for $t {
            fn element_name(&self) -> &str {
                &self.name
            }
            fn model_of(&self) -> Option<&str> {
                Some(self.model.as_str()).filter(|m| !m.is_empty())
            }
            fn set_model(&mut self, name: String) {
                self.model = name;
            }
        }
    };
}

macro_rules! refs_opt {
    ($t:ty) => {
        impl RefsModel for $t {
            fn element_name(&self) -> &str {
                &self.name
            }
            fn model_of(&self) -> Option<&str> {
                self.model.as_deref().filter(|m| !m.is_empty())
            }
            fn set_model(&mut self, name: String) {
                self.model = Some(name);
            }
        }
    };
}

refs_str!(Diode);
refs_str!(Mosfet);
refs_str!(Bjt);
refs_str!(Jfet);
refs_str!(OpAmp);
refs_opt!(Resistor);
refs_opt!(Capacitor);
refs_opt!(Inductor);

/// Passive elements additionally scale their own card value when they carry
/// one (value 0 means "resolve through the model card" — those re-bind a
/// scaled model clone inside [`draw_passive_family`] instead).
trait PassiveElement: RefsModel {
    fn has_nominal(&self) -> bool;
    fn scale_nominal(&mut self, factor: f64);
}

macro_rules! passive_mc {
    ($t:ty, $value:ident) => {
        impl PassiveElement for $t {
            fn has_nominal(&self) -> bool {
                self.$value > 0.0
            }
            fn scale_nominal(&mut self, factor: f64) {
                self.$value *= factor;
            }
        }
    };
}

passive_mc!(Resistor, resistance);
passive_mc!(Capacitor, capacitance);
passive_mc!(Inductor, inductance);

/// Re-bind every element that names a tolerance-bearing model of `map` to a
/// per-instance scaled clone (`model::element`), device families.
fn rebind_family<M: McModel, E: RefsModel>(
    map: &mut HashMap<String, M>,
    elems: &mut [E],
    seed: u64,
    family: &str,
) {
    let tied: Vec<(usize, String)> = elems
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.model_of().map(|m| (i, m.to_string())))
        .collect();
    for (i, model) in tied {
        let Some(card) = map.get(&model) else {
            continue;
        };
        let tol = card.tolerance();
        if tol.lot.is_none() && tol.dev.is_none() {
            continue;
        }
        let key = format!("{model}::{}", elems[i].element_name());
        let factor = instance_factor(tol, seed, family, &model, elems[i].element_name());
        let clone = card.scaled(factor);
        map.insert(key.clone(), clone);
        elems[i].set_model(key);
    }
}

/// The passive version: card-valued elements scale their value in place (the
/// model only shapes the draw key), value-0 elements re-bind a scaled model
/// clone. The expanded parasitic arms carry no model reference and are
/// therefore never drawn on — the nominal value is what MC scales.
fn draw_passive_family<P: PassiveElement>(
    map: &mut HashMap<String, PassiveModel>,
    elems: &mut [P],
    seed: u64,
    family: &str,
) {
    let tied: Vec<(usize, String)> = elems
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            if e.has_nominal() {
                return None;
            }
            e.model_of().map(|m| (i, m.to_string()))
        })
        .collect();
    let nominal: Vec<(usize, String)> = elems
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            if !e.has_nominal() {
                return None;
            }
            e.model_of().map(|m| (i, m.to_string()))
        })
        .collect();
    for (i, model) in tied {
        let Some(card) = map.get(&model) else {
            continue;
        };
        if card.tolerance.lot.is_none() && card.tolerance.dev.is_none() {
            continue;
        }
        let factor = instance_factor(
            &card.tolerance,
            seed,
            family,
            &model,
            elems[i].element_name(),
        );
        let key = format!("{model}::{}", elems[i].element_name());
        let clone = card.scaled(factor);
        map.insert(key.clone(), clone);
        elems[i].set_model(key);
    }
    for (i, model) in nominal {
        let Some(card) = map.get(&model) else {
            continue;
        };
        if card.tolerance.lot.is_none() && card.tolerance.dev.is_none() {
            continue;
        }
        let factor = instance_factor(
            &card.tolerance,
            seed,
            family,
            &model,
            elems[i].element_name(),
        );
        elems[i].scale_nominal(factor);
    }
}

/// Apply one run's draws to `circuit`, in place. Call it once per run, right
/// after that run's re-parse, on the throwaway deck.
pub(crate) fn apply_model_draws(circuit: &mut Circuit, seed: u64) {
    let Circuit {
        resistor_models,
        resistors,
        capacitor_models,
        capacitors,
        inductor_models,
        inductors,
        diode_models,
        diodes,
        mosfet_models,
        mosfets,
        bjt_models,
        bjts,
        jfet_models,
        jfets,
        opamp_models,
        opamps,
        ..
    } = circuit;

    draw_passive_family(resistor_models, resistors, seed, "r");
    draw_passive_family(capacitor_models, capacitors, seed, "c");
    draw_passive_family(inductor_models, inductors, seed, "l");
    rebind_family(diode_models, diodes, seed, "d");
    rebind_family(mosfet_models, mosfets, seed, "m");
    rebind_family(bjt_models, bjts, seed, "q");
    rebind_family(jfet_models, jfets, seed, "j");
    rebind_family(opamp_models, opamps, seed, "u");
}

// ─── Run statistics ─────────────────────────────────────────────────────────

/// One measurement's value distribution across the MC runs.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasStat {
    /// The measurement card name (lowercase).
    pub name: String,
    /// How many runs recorded a value; failed measurements drop out.
    pub n: usize,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    /// Population sigma over the runs; 0 for a single run.
    pub sigma: f64,
}

/// A measurement value histogram across the runs (`.mc … hist=…`).
#[derive(Debug, Clone, PartialEq)]
pub struct McHistogram {
    pub name: String,
    pub bins: usize,
    /// Lowest sample — the first bin's left edge.
    pub lo: f64,
    /// Highest sample — the last bin is closed on the right.
    pub hi: f64,
    /// Sample counts per bin: `counts[i]` is samples in `[lo + i·w, …)`.
    pub counts: Vec<usize>,
}

/// The pass fraction of a `.mc … yield=<measure>{<|>}<limit>` criterion.
#[derive(Debug, Clone, PartialEq)]
pub struct McYield {
    pub name: String,
    pub op: McOp,
    pub limit: f64,
    /// Runs whose measured value satisfied the comparison.
    pub passes: usize,
    /// Runs that recorded a value at all.
    pub total: usize,
}

/// Everything `.mc` promised, aggregated over the runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct McReport {
    /// One entry per measurement name, in card order.
    pub stats: Vec<MeasStat>,
    pub histogram: Option<McHistogram>,
    pub criterion: Option<McYield>,
}

/// Values recorded per measurement name, first-seen card order.
fn collect_values(result: &SimulationResult) -> (Vec<String>, HashMap<String, Vec<f64>>) {
    let mut order: Vec<String> = Vec::new();
    let mut values: HashMap<String, Vec<f64>> = HashMap::new();
    for run in &result.runs {
        for MeasureResult { name, value, .. } in &run.measurements {
            let Some(value) = value else { continue };
            let key = name.to_ascii_lowercase();
            if !values.contains_key(&key) {
                order.push(key.clone());
            }
            values.entry(key).or_default().push(*value);
        }
    }
    (order, values)
}

/// Aggregate the runs' measurement values the way `.mc` asked: per-measurement
/// statistics, the optional histogram, and the optional pass criterion.
/// Measurement names match case-insensitively (cards are lowercased at
/// parse); a run whose measurement failed contributes nothing.
pub fn summarize(result: &SimulationResult, mc: &McCommand) -> McReport {
    let (order, values) = collect_values(result);
    let stats = order
        .into_iter()
        .map(|name| {
            let samples = &values[&name];
            let n = samples.len();
            let min = samples.iter().copied().fold(f64::INFINITY, f64::min);
            let max = samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let mean = samples.iter().sum::<f64>() / n.max(1) as f64;
            let var =
                samples.iter().map(|s| (s - mean) * (s - mean)).sum::<f64>() / n.max(1) as f64;
            MeasStat {
                name,
                n,
                min,
                max,
                mean,
                sigma: var.sqrt(),
            }
        })
        .collect();
    let histogram = mc.hist.as_ref().map(|name| {
        let samples = values.get(name).cloned().unwrap_or_default();
        hist_of(&samples, name.clone(), mc.bins)
    });
    let criterion = mc.criterion.as_ref().map(|(name, op, limit)| {
        let samples = values.get(name).cloned().unwrap_or_default();
        let passes = samples
            .iter()
            .filter(|s| match op {
                McOp::Below => **s < *limit,
                McOp::Above => **s > *limit,
            })
            .count();
        McYield {
            name: name.clone(),
            op: *op,
            limit: *limit,
            passes,
            total: samples.len(),
        }
    });
    McReport {
        stats,
        histogram,
        criterion,
    }
}

/// Bin `samples` over their own spread; degenerate spreads (all equal) put
/// every sample in bin 0.
fn hist_of(samples: &[f64], name: String, bins: usize) -> McHistogram {
    let lo = samples.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let width = (hi - lo) / bins.max(1) as f64;
    let mut counts = vec![0usize; bins];
    for &v in samples {
        let pos = if width <= 0.0 || v >= hi {
            bins - 1
        } else {
            (((v - lo) / width) as usize).min(bins - 1)
        };
        counts[pos] += 1;
    }
    McHistogram {
        name,
        bins,
        lo,
        hi,
        counts,
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::StepRun;
    use crate::circuit::NodeId;

    fn diode_deck(seed: u64, lot: Option<(f64, bool)>, dev: Option<(f64, bool)>) -> Circuit {
        let mut circuit = Circuit::new("mc");
        let model = DiodeModel {
            tolerance: ModelTolerance { dev, lot },
            ..DiodeModel::default()
        };
        circuit.diode_models.insert("dmod".to_string(), model);
        for (i, name) in ["D1", "D2"].iter().enumerate() {
            circuit.diodes.push(Diode {
                name: (*name).to_string(),
                anode: NodeId(i + 1),
                cathode: NodeId(0),
                model: "dmod".to_string(),
                area: 1.0,
            });
        }
        apply_model_draws(&mut circuit, seed);
        circuit
    }

    #[test]
    fn draws_are_deterministic_keyed_and_inside_the_unit_interval() {
        let a = draw(42, &["d", "dev", "dmod", "D1"]);
        assert_eq!(a, draw(42, &["d", "dev", "dmod", "D1"]));
        assert_ne!(a, draw(43, &["d", "dev", "dmod", "D1"]));
        assert_ne!(a, draw(42, &["d", "dev", "dmod", "D2"]));
        assert!((-1.0..1.0).contains(&a), "u in [−1, 1): {a}");
    }

    #[test]
    fn lot_draws_once_per_model_and_dev_per_instance() {
        let tol = ModelTolerance {
            dev: Some((10.0, true)),
            lot: Some((10.0, false)),
        };
        let first = diode_deck(9, tol.lot, tol.dev);
        let is1 = first.diode_models["dmod::D1"].is;
        let is2 = first.diode_models["dmod::D2"].is;
        // Both instances live inside ±(10% + 10%), and the per-instance dev
        // draws separate them.
        assert!((0.8e-14..=1.2e-14).contains(&is1), "is1 within ±20%: {is1}");
        assert!((0.8e-14..=1.2e-14).contains(&is2), "is2 within ±20%: {is2}");
        assert_ne!(is1, is2);
        // Same seed → bit-identical; other seed → a different landscape.
        let again = diode_deck(9, tol.lot, tol.dev);
        assert_eq!(again.diode_models["dmod::D1"].is, is1);
        let shifted = diode_deck(10, tol.lot, tol.dev);
        assert_ne!(shifted.diode_models["dmod::D1"].is, is1);
    }

    #[test]
    fn passives_scale_their_nominal_value() {
        let mut circuit = Circuit::new("mc");
        let model = PassiveModel {
            value: Some(10.0e3),
            tolerance: ModelTolerance {
                dev: Some((20.0, true)),
                lot: None,
            },
            ..PassiveModel::default()
        };
        circuit.resistor_models.insert("rmod".to_string(), model);
        circuit.resistors = vec![
            Resistor {
                name: "R1".to_string(),
                n1: NodeId(1),
                n2: NodeId(0),
                resistance: 0.0,
                model: Some("rmod".to_string()),
            },
            Resistor {
                name: "R2".to_string(),
                n1: NodeId(1),
                n2: NodeId(0),
                resistance: 5.0e3,
                model: Some("rmod".to_string()),
            },
        ];
        apply_model_draws(&mut circuit, 5);
        // Model-valued R1 re-binds to a scaled clone carrying the drawn R=.
        assert_eq!(circuit.resistors[0].model.as_deref(), Some("rmod::R1"));
        let drawn = circuit.resistor_models["rmod::R1"].value.unwrap_or(0.0);
        assert!(
            (8.0e3..=12.0e3).contains(&drawn),
            "R1's R= within ±20%: {drawn}"
        );
        // Card-valued R2 scales its own value through the same model.
        let own = circuit.resistors[1].resistance;
        assert!(
            (4.0e3..=6.0e3).contains(&own),
            "R2 value within ±20%: {own}"
        );
    }

    #[test]
    fn summarize_reports_stats_hist_and_criterion() {
        fn run_with(values: Vec<Option<f64>>) -> StepRun {
            let measurements = values
                .into_iter()
                .enumerate()
                .map(|(i, v)| MeasureResult {
                    name: format!("m{}", i + 1),
                    value: v,
                    error: None,
                })
                .collect();
            StepRun {
                values: vec![0.0],
                dc: None,
                dc_sweep: None,
                tran: None,
                ac: None,
                tf: None,
                four: None,
                noise: None,
                measurements,
            }
        }
        let result = SimulationResult {
            dc: None,
            dc_sweep: None,
            tran: None,
            ac: None,
            tf: None,
            noise: None,
            step_targets: vec!["mc".to_string()],
            runs: vec![
                run_with(vec![Some(1.0), Some(1.5)]),
                run_with(vec![Some(1.5), None]),
                run_with(vec![Some(3.0), Some(0.0)]),
            ],
            measurements: Vec::new(),
            four: None,
        };
        let mc = McCommand {
            runs: 3,
            seed: None,
            hist: Some("m1".to_string()),
            bins: 4,
            criterion: Some(("m1".to_string(), McOp::Below, 2.5)),
        };
        let report = summarize(&result, &mc);
        // m1: 1.0, 1.5, 3.0 — the failed-and-empty runs never enter it.
        let m1 = &report.stats[0];
        assert_eq!(m1.name, "m1");
        assert_eq!(m1.n, 3);
        assert_eq!(m1.min, 1.0);
        assert_eq!(m1.max, 3.0);
        assert!((m1.mean - 11.0 / 6.0).abs() < 1e-12, "mean: {}", m1.mean);
        assert!((m1.sigma - (13.0f64 / 18.0).sqrt()).abs() < 1e-12);
        // Histogram over [1, 3] in 4 equal bins: 1.0 → bin 0, 1.5 → bin 1,
        // 3.0 rides the closed top edge of bin 3.
        let hist = report.histogram.expect("histogram was asked for");
        assert_eq!(
            (hist.lo, hist.hi, hist.counts.as_slice()),
            (1.0, 3.0, &[1, 1, 0, 1][..])
        );
        // yield m1<2.5: two of the three runs pass.
        let criterion = report.criterion.expect("criterion was asked for");
        assert_eq!((criterion.passes, criterion.total), (2, 3));
        assert_eq!(report.stats[1].name, "m2");
        assert_eq!(report.stats[1].n, 2, "the failed run dropped out");
    }

    #[test]
    fn no_model_reference_means_no_draw() {
        let mut circuit = Circuit::new("mc");
        let model = JfetModel {
            tolerance: ModelTolerance {
                dev: Some((30.0, true)),
                lot: None,
            },
            ..JfetModel::default()
        };
        circuit.jfet_models.insert("jmod".to_string(), model);
        circuit.jfets.push(Jfet {
            name: "J1".into(),
            nd: NodeId(1),
            ng: NodeId(2),
            ns: NodeId(0),
            model: "jmod".into(),
            area: 1.0,
        });
        circuit.resistors.push(Resistor {
            name: "R1".into(),
            n1: NodeId(2),
            n2: NodeId(0),
            resistance: 1.0e3,
            model: None,
        });
        apply_model_draws(&mut circuit, 3);
        let beta = circuit.jfet_models["jmod::J1"].beta;
        assert!(
            (0.7e-4..=1.3e-4).contains(&beta),
            "beta within ±30%: {beta}"
        );
        // Bare `R1 1k` has nothing to attach a tolerance to: the card value
        // rides every run unchanged.
        assert_eq!(circuit.resistors[0].resistance, 1.0e3);
    }
}
