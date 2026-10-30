//! The run drivers (`.step`, `.mc`): how many decks, minted how, assembled
//! into what.
//!
//! `analysis.rs` holds the seven engines and
//! [`run_single`](crate::analysis::run_single) — what ONE deck produces.
//! This module holds the other question. The driver protocol — re-parse
//! with the row's overrides (or a straight clone), strip the deck's own
//! driver card, run once, land one [`StepRun`], then mirror the first run
//! into the top-level result — lives here exactly once ([`run_plans`]), so
//! the mirror invariant ("runs present ⇔ top-level result fields are the
//! first run's") is enforced by construction instead of spelled out per
//! driver and re-inferred downstream (the stepped `.raw` plot has to read
//! it).

use std::collections::HashMap;

use crate::analysis::{
    run_single, sweep_values, AcResult, DcResult, DcSweepResult, FourResult, MeasureResult,
    NoiseResult, SimulationResult, TfResult, TranResult,
};
use crate::error::{Result, SimError};
use crate::mc;
use crate::netlist::{DcSweepValues, Netlist, StepSpec, StepValues};

// ─── Result shape ────────────────────────────────────────────────────────────

/// The result of one `.step` combination.
#[derive(Debug, Clone)]
pub struct StepRun {
    /// Swept value per step target, in `step_targets` order.
    pub values: Vec<f64>,
    pub dc: Option<DcResult>,
    pub dc_sweep: Option<DcSweepResult>,
    pub tran: Option<TranResult>,
    pub ac: Option<AcResult>,
    pub tf: Option<TfResult>,
    /// `.meas` results for this step run.
    pub measurements: Vec<MeasureResult>,
    /// `.four` result for this step run.
    pub four: Option<FourResult>,
    /// `.noise` result for this step run.
    pub noise: Option<NoiseResult>,
}

// ─── Drivers ─────────────────────────────────────────────────────────────────

/// Run every analysis once per `.step` combination, or a single pass when
/// the deck has no `.step` cards. A `.mc` deck instead runs once per seeded
/// Monte Carlo run ([`mc::apply_model_draws`] and the re-parse's draw seed do
/// the worrying — TODO §8).
pub fn run(netlist: &Netlist) -> Result<SimulationResult> {
    if netlist.analysis.mc.is_some() {
        return run_mc(netlist);
    }
    let mut specs = netlist.analysis.steps.clone();
    if specs.is_empty() {
        // A single `.temp` value simply sets the run temperature; several
        // values are the same thing as `.step temp LIST …`.
        if netlist.analysis.temperatures.len() <= 1 {
            return run_single(netlist);
        }
        specs.push(StepSpec {
            target: "temp".to_string(),
            values: StepValues::List(netlist.analysis.temperatures.clone()),
        });
    } else if netlist.analysis.temperatures.len() > 1
        && !specs.iter().any(|s| s.target == "temp")
    {
        return Err(SimError::Analysis(
            "several .temp values and .step cards were both given —              use .step temp LIST … or a single .temp"
                .into(),
        ));
    }
    let targets: Vec<String> = specs.iter().map(|s| s.target.clone()).collect();
    let base_temperature = netlist.circuit.temperature;
    let plans = step_combinations(&specs)?
        .into_iter()
        .map(|combo| {
            let temperature = Some(
                targets
                    .iter()
                    .position(|t| t == "temp")
                    .map(|i| combo[i])
                    .unwrap_or(base_temperature),
            );
            let overrides: Vec<(String, f64)> = targets
                .iter()
                .cloned()
                .zip(combo.iter().copied())
                .filter(|(target, _)| target != "temp")
                .collect();
            RunPlan::Step {
                values: combo,
                temperature,
                overrides,
            }
        })
        .collect();
    run_plans(netlist, plans, targets, ".step produced no runs")
}

/// The Monte Carlo driver: `.mc <runs> …` re-runs the deck `runs` times, once
/// per run index. Each run re-parses the source with the hidden
/// [`mc::RUN_KEY`] override (so `random()`/`gauss()` sorts of parameter draws
/// re-roll the way `.step` runs do), then [`mc::apply_model_draws`] re-binds
/// every tolerance-bearing model card to per-instance clones and the run is
/// solved as-is. Runs land in the same `runs` vector `.step` fills, tagged
/// with their run index (`step_targets: ["mc"]`), so the CLI statistics and
/// overlays read them like step runs.
fn run_mc(netlist: &Netlist) -> Result<SimulationResult> {
    if !netlist.analysis.steps.is_empty() || netlist.analysis.temperatures.len() > 1 {
        return Err(SimError::Analysis(
            "RSpice runs .mc on an unstepped deck; .step and .mc are separate run \
             drivers — comment out the card you are not using"
                .into(),
        ));
    }
    let mc = netlist
        .analysis
        .mc
        .as_ref()
        .ok_or_else(|| SimError::Analysis("run_mc without an .mc card".into()))?;
    if mc.runs > MAX_STEP_RUNS {
        return Err(SimError::Analysis(format!(
            ".mc would run {} simulations, over the {MAX_STEP_RUNS}-run ceiling",
            mc.runs
        )));
    }
    let base_seed = mc.seed.unwrap_or(netlist.mc_seed);
    let plans = (0..mc.runs)
        .map(|run| RunPlan::Mc {
            index: run,
            seed: mc::run_mix(base_seed, run),
        })
        .collect();
    run_plans(
        netlist,
        plans,
        vec!["mc".to_string()],
        ".mc produced no runs",
    )
}

// ─── The mint choreography ───────────────────────────────────────────────────

/// One run's minted facts. The whole per-run choreography — clone vs
/// re-parse, which card to strip, the temperature pin, the seed/draw
/// order — is keyed on the plan and applied in one place, so a driver never
/// re-learns it and a third driver arrives as one new arm here.
enum RunPlan {
    /// One `.step` combination: `values` in `step_targets` order,
    /// `overrides` naming every non-temp target, and the temperature to pin
    /// the deck to (`None` leaves the deck's own standing).
    Step {
        values: Vec<f64>,
        temperature: Option<f64>,
        overrides: Vec<(String, f64)>,
    },
    /// One seeded Monte Carlo run: the run index (the hidden `.mc` override
    /// and the recorded value) and its mixed seed.
    Mc { index: usize, seed: u64 },
}

impl RunPlan {
    /// The value row the `StepRun` records, in `step_targets` order.
    fn values(&self) -> Vec<f64> {
        match self {
            RunPlan::Step { values, .. } => values.clone(),
            RunPlan::Mc { index, .. } => vec![*index as f64],
        }
    }
}

/// Mint one run's inner deck: apply the plan's overrides through a re-parse
/// (or a straight clone when a `.step` row overrides nothing — the source
/// text produces exactly the deck we are holding), then strip the deck's
/// own driver card and pin the plan's per-run facts. The `Mc` arm's
/// statement order is load-bearing (the re-parse's parameter draws first,
/// the runtime seed second, the model-card draws third: see
/// `mc::apply_model_draws`), as is leaving the caller's deck untouched —
/// the next mint re-reads the original cards off it.
fn mint_inner(netlist: &Netlist, plan: &RunPlan) -> Result<Netlist> {
    let mut inner = match plan {
        RunPlan::Step { overrides, .. } => {
            if overrides.is_empty() {
                netlist.clone()
            } else {
                let overrides: HashMap<String, f64> = overrides.iter().cloned().collect();
                crate::netlist::parse_with_overrides(&netlist.source, &netlist.options, &overrides)?
            }
        }
        RunPlan::Mc { index, .. } => {
            let mut overrides: HashMap<String, f64> = HashMap::new();
            overrides.insert(mc::RUN_KEY.to_string(), *index as f64);
            crate::netlist::parse_with_overrides(&netlist.source, &netlist.options, &overrides)?
        }
    };
    match plan {
        // The inner parse sees the same `.step`/`.temp` cards; running them
        // again would nest the sweep inside itself.
        RunPlan::Step { temperature, .. } => {
            inner.analysis.steps.clear();
            inner.analysis.temperatures.clear();
            if let Some(t) = temperature {
                inner.circuit.temperature = *t;
            }
        }
        // The inner parse sees the same `.mc` card; running it again would
        // nest Monte Carlo inside itself. Re-rolled parameter draws ride the
        // re-parse; the runtime keyed draws follow `mc_seed`, and the model
        // cards follow the draws.
        RunPlan::Mc { seed, .. } => {
            inner.analysis.mc = None;
            inner.mc_seed = *seed;
            mc::apply_model_draws(&mut inner.circuit, *seed);
        }
    }
    Ok(inner)
}

/// The protocol, owned once: per plan, mint the inner deck, run it once,
/// land one `StepRun`; then mirror the first run into the top level.
fn run_plans(
    netlist: &Netlist,
    plans: Vec<RunPlan>,
    targets: Vec<String>,
    no_runs: &str,
) -> Result<SimulationResult> {
    let mut runs = Vec::with_capacity(plans.len());
    for plan in &plans {
        let inner = mint_inner(netlist, plan)?;
        let single = run_single(&inner)?;
        runs.push(StepRun {
            values: plan.values(),
            dc: single.dc,
            dc_sweep: single.dc_sweep,
            tran: single.tran,
            ac: single.ac,
            tf: single.tf,
            measurements: single.measurements,
            four: single.four,
            noise: single.noise,
        });
    }
    let first = runs
        .first()
        .cloned()
        .ok_or_else(|| SimError::Analysis(no_runs.to_string()))?;
    Ok(SimulationResult {
        dc: first.dc,
        dc_sweep: first.dc_sweep,
        tran: first.tran,
        ac: first.ac,
        tf: first.tf,
        step_targets: targets,
        runs,
        measurements: first.measurements,
        four: first.four,
        noise: first.noise,
    })
}

// ─── Value lists ─────────────────────────────────────────────────────────────

/// Hard ceiling on the number of `.step` combinations (each is a full
/// analysis), so a fat-fingered sweep cannot run for days.
const MAX_STEP_RUNS: usize = 10_000;

/// The cartesian product of every `.step` value list, outer spec first.
fn step_combinations(specs: &[StepSpec]) -> Result<Vec<Vec<f64>>> {
    let mut lists: Vec<Vec<f64>> = Vec::with_capacity(specs.len());
    for spec in specs {
        lists.push(step_values(spec)?);
    }
    let total: usize = lists.iter().map(Vec::len).product();
    if total > MAX_STEP_RUNS {
        return Err(SimError::Analysis(format!(
            ".step would run {total} simulations, over the {MAX_STEP_RUNS}-run ceiling"
        )));
    }
    let mut out: Vec<Vec<f64>> = vec![Vec::with_capacity(specs.len())];
    for list in lists {
        let mut next = Vec::with_capacity(out.len() * list.len());
        for prefix in &out {
            for &v in &list {
                let mut combo = prefix.clone();
                combo.push(v);
                next.push(combo);
            }
        }
        out = next;
    }
    Ok(out)
}

fn step_values(spec: &StepSpec) -> Result<Vec<f64>> {
    let target = &spec.target;
    match &spec.values {
        StepValues::List(v) => Ok(v.clone()),
        StepValues::Linear { start, stop, step } => {
            sweep_values(
                &DcSweepValues::Linear { start: *start, stop: *stop, step: *step },
                target,
            )
        }
        StepValues::Decade { start, stop, points } => {
            let decades = (stop / start).log10();
            let total = (decades * *points as f64).round() as usize + 1;
            Ok((0..total)
                .map(|i| start * 10f64.powf(decades * i as f64 / (total - 1).max(1) as f64))
                .collect())
        }
        StepValues::Octave { start, stop, points } => {
            let octaves = (stop / start).log2();
            let total = (octaves * *points as f64).round() as usize + 1;
            Ok((0..total)
                .map(|i| start * 2f64.powf(octaves * i as f64 / (total - 1).max(1) as f64))
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{mint_inner, run, run_plans, RunPlan};
    use crate::error::SimError;
    use crate::mc;
    use crate::parse;

    /// runs present ⇔ the top-level fields are the first run's — and the
    /// unstepped path stays unmirrored (runs empty, top level filled
    /// directly by `run_single`).
    #[test]
    fn mirror_makes_the_top_level_the_first_run() {
        let netlist = parse(
            "* .step mirror\n\
             .param r=1k\n\
             V1 in 0 DC 1\n\
             R1 in out {r}\n\
             R2 out 0 1k\n\
             .step param r list 1k 2k 3k\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let result = run(&netlist).unwrap();
        assert_eq!(result.step_targets, vec!["r".to_string()]);
        assert_eq!(result.runs.len(), 3);
        for (i, step) in result.runs.iter().enumerate() {
            assert_eq!(step.values, vec![1000.0 * (i as f64 + 1.0)]);
            assert!(step.dc.is_some());
        }
        // Bit-for-bit: the top level is the first run's clone.
        assert!(result
            .dc
            .as_ref()
            .unwrap()
            .node_voltages
            .iter()
            .zip(&result.runs[0].dc.as_ref().unwrap().node_voltages)
            .all(|(top, first)| top == first));
        assert!(result.noise.is_none());

        let plain = parse(
            "* unstepped: no plans, no mirror\n\
             V1 in 0 DC 1\n\
             R1 in out 1k\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let result = run(&plain).unwrap();
        assert!(result.runs.is_empty());
        assert!(result.step_targets.is_empty());
        assert!(result.dc.is_some());
    }

    /// The empty-plan error speaks in the deck's own words, byte for byte.
    #[test]
    fn no_runs_errors_are_byte_exact() {
        let netlist = parse("* empty: nothing to run\n.op\n.end\n").unwrap();
        let err = run_plans(&netlist, Vec::new(), vec![], ".step produced no runs").unwrap_err();
        match err {
            SimError::Analysis(message) => assert_eq!(message, ".step produced no runs"),
            other => panic!("unexpected error shape: {other}"),
        }
        let err = run_plans(&netlist, Vec::new(), vec![], ".mc produced no runs").unwrap_err();
        match err {
            SimError::Analysis(message) => assert_eq!(message, ".mc produced no runs"),
            other => panic!("unexpected error shape: {other}"),
        }
    }

    /// A `.step` mint strips exactly the deck's own cards and pins the
    /// temperature without touching the caller's deck.
    #[test]
    fn step_mint_strips_its_own_cards_and_pins_the_temperature() {
        let netlist = parse(
            "* stepped, then minted\n\
             V1 in 0 DC 1\n\
             R1 in out 1k\n\
             .step temp list 47 57\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let plan = RunPlan::Step {
            values: vec![47.0],
            temperature: Some(47.0),
            overrides: Vec::new(),
        };
        let inner = mint_inner(&netlist, &plan).unwrap();
        assert!(inner.analysis.steps.is_empty());
        assert!(inner.analysis.temperatures.is_empty());
        assert_eq!(inner.circuit.temperature, 47.0);
        assert!(!netlist.analysis.steps.is_empty());
        // `None` leaves the deck's own temperature standing.
        let plan = RunPlan::Step {
            values: vec![0.0],
            temperature: None,
            overrides: Vec::new(),
        };
        let inner = mint_inner(&netlist, &plan).unwrap();
        assert_eq!(inner.circuit.temperature, netlist.circuit.temperature);
    }

    /// A `.mc` mint inerts the card and carries the mixed seed; the caller's
    /// deck still owns the card it was minted from.
    #[test]
    fn mc_mint_inerts_the_mc_card_and_carries_the_seed() {
        let netlist = parse(
            "* minted montes\n\
             V1 in 0 DC 1\n\
             R1 in out 1k\n\
             .mc 25 seed=7\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let plan = RunPlan::Mc {
            index: 3,
            seed: mc::run_mix(7, 3),
        };
        let inner = mint_inner(&netlist, &plan).unwrap();
        assert!(inner.analysis.mc.is_none());
        assert_eq!(inner.mc_seed, mc::run_mix(7, 3));
        assert!(netlist.analysis.mc.is_some());
    }
}
