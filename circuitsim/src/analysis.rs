use std::collections::HashMap;

use crate::circuit::{Circuit, ElementFamily};
use crate::error::{Result, SimError};
use crate::mna::{
    advance_transient_state, build_system, compute_ac_branch_currents, compute_branch_currents,
    compute_branch_currents_at, device_model, node_voltage, seed_transient_state, BranchCurrents,
    BranchLayout, DcJunctionLimits, MnaSystem, Mode, TransientState,
};
use crate::behavior::BehavioralRuntime;
use crate::circuit::IntegrationMethod;
use crate::netlist::{
    AcCommand, AcScale, DcCommand, DcSweepValues, Netlist, NoiseCommand, NodesetEntry, StepSpec,
    StepValues, TfCommand, TfOutput,
};
use crate::circuit::MosfetType;
use crate::model::{BOLTZMANN, ELECTRON_CHARGE, ZERO_CELSIUS_K};
use num_complex::Complex64;

#[derive(Debug, Clone)]
pub struct DcResult {
    pub node_voltages:   Vec<f64>,
    pub source_currents: Vec<f64>,
    /// Branch current through every circuit element (n1→n2 convention).
    pub branch_currents: BranchCurrents<f64>,
}

/// One point of a `.dc` sweep: the swept values plus the operating point
/// solved with the sources/parameters at those values.
#[derive(Debug, Clone)]
pub struct DcSweepPoint {
    /// The driven value per sweep spec, outer first (1 or 2 entries).
    pub values: Vec<f64>,
    pub node_voltages: Vec<f64>,
    pub source_currents: Vec<f64>,
    pub branch_currents: BranchCurrents<f64>,
}

/// Result of a real `.dc` sweep — the sweep arguments are never optional
/// decoration: an analysed sweep produces points, and parse errors on the
/// arguments happened long before this.
#[derive(Debug, Clone)]
pub struct DcSweepResult {
    /// Sweep target names as written (`V1`, `I2`, or a `.param` key).
    pub targets: Vec<String>,
    /// Points in sweep order: the primary sweep outermost, the optional
    /// second sweep innermost (SPICE nesting convention).
    pub points: Vec<DcSweepPoint>,
}

#[derive(Debug, Clone)]
pub struct TranPoint {
    pub time:            f64,
    pub node_voltages:   Vec<f64>,
    /// Branch current through every circuit element at this time point.
    pub branch_currents: BranchCurrents<f64>,
}

#[derive(Debug, Clone)]
pub struct TranResult {
    pub points: Vec<TranPoint>,
}

/// One frequency point in an AC sweep.
#[derive(Debug, Clone)]
pub struct AcPoint {
    pub freq:            f64,
    /// Complex phasor voltage at each node.
    pub node_voltages:   Vec<Complex64>,
    /// Complex phasor branch currents through every circuit element.
    pub branch_currents: BranchCurrents<Complex64>,
}

/// Result of a small-signal AC (frequency sweep) analysis.
#[derive(Debug, Clone)]
pub struct AcResult {
    pub points: Vec<AcPoint>,
    /// Index of the source node that was driven (the n1 of the first AC-tagged source,
    /// or the first voltage source as a fallback).
    pub stimulus_node: usize,
}

#[derive(Debug, Clone)]
pub struct SimulationResult {
    pub dc: Option<DcResult>,
    pub dc_sweep: Option<DcSweepResult>,
    pub tran: Option<TranResult>,
    pub ac: Option<AcResult>,
    /// `.tf` DC transfer function result.
    pub tf: Option<TfResult>,
    /// Names of the stepped parameters, outer first (empty when the deck has
    /// no `.step` cards).
    pub step_targets: Vec<String>,
    /// One entry per `.step` run, including the first. Empty when the deck is
    /// not stepped; `runs[i].values` line up with `step_targets`.
    pub runs: Vec<StepRun>,
    /// `.meas`/`.measure` results for this (first) run.
    pub measurements: Vec<MeasureResult>,
    /// `.four` Fourier result, when requested.
    pub four: Option<FourResult>,
    /// `.noise` spectrum, when requested.
    pub noise: Option<NoiseResult>,
}

/// One harmonic of a `.four` analysis.
#[derive(Debug, Clone)]
pub struct Harmonic {
    pub number: usize,
    pub frequency: f64,
    pub magnitude: f64,
    /// Phase in degrees (cosine reference).
    pub phase_deg: f64,
    /// Magnitude relative to the fundamental.
    pub normalized_magnitude: f64,
    /// Phase relative to the fundamental, in degrees.
    pub normalized_phase_deg: f64,
}

/// Fourier decomposition of one output waveform.
#[derive(Debug, Clone)]
pub struct FourOutput {
    pub name: String,
    /// DC average over the analysed period.
    pub dc: f64,
    pub harmonics: Vec<Harmonic>,
}

/// `.four` result: the analysed fundamental and every requested output.
#[derive(Debug, Clone)]
pub struct FourResult {
    pub fundamental: f64,
    pub outputs: Vec<FourOutput>,
}

/// One frequency point of a `.noise` sweep. Densities are per √Hz.
#[derive(Debug, Clone)]
pub struct NoisePoint {
    pub freq: f64,
    /// Output-referred noise spectral density (V/√Hz at the output nodes).
    pub output_density: f64,
    /// Input-referred noise spectral density (V/√Hz at the input source).
    pub input_density: f64,
}

/// `.noise` result: the spectrum plus integrated RMS totals over the sweep.
#[derive(Debug, Clone)]
pub struct NoiseResult {
    pub output: String,
    pub input: String,
    pub points: Vec<NoisePoint>,
    /// √(∫ output PSD df) over the swept band.
    pub total_output_rms: f64,
    /// √(∫ input-referred PSD df) over the swept band.
    pub total_input_rms: f64,
}

/// One `.meas` result. A measurement that cannot be evaluated (no crossing,
/// no transient result to read, …) carries an explanation instead of aborting
/// the whole simulation — the reference tools report it the same way.
#[derive(Debug, Clone)]
pub struct MeasureResult {
    pub name: String,
    pub value: Option<f64>,
    pub error: Option<String>,
}

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

/// One `.tf` result: the small-signal DC transfer from `input` to `output`,
/// plus the driving-point and output impedances at the bias point.
#[derive(Debug, Clone)]
pub struct TfResult {
    /// Output operand as written (`V(out)`, `I(V1)`, …).
    pub output: String,
    /// Input source name as written.
    pub input: String,
    /// `output / input` (V/V, V/A, A/V, or A/A depending on the operands).
    pub transfer: f64,
    /// Driving-point impedance seen by the input source (Ω); infinite when
    /// the input draws no current.
    pub input_impedance: f64,
    /// Output impedance seen at the output (Ω); `NaN` when the output is a
    /// branch current (no unique driving point).
    pub output_impedance: f64,
}

/// Newton-Raphson convergence tolerance (max |Δx| between iterates).
const NR_TOLERANCE: f64 = 1e-6;

/// One Newton-Raphson solve, shared by `.op` and every transient step:
/// stamp the system at the current iterate, solve, repeat until the iterate
/// moves less than [`NR_TOLERANCE`].
///
/// `build` receives the current iterate and MAINTAINS all other mode data
/// (bias point, dt, transient state) by closure; the returned system owns its
/// matrix, so no lifetimes leak through the seam. `max_iters` is the
/// iteration budget the deck asked for (`itl1` at DC, `itl4` per transient
/// point; both default to the pre-options 100).
fn newton_raphson(
    mut build: impl FnMut(&nalgebra::DVector<f64>) -> Result<MnaSystem<f64>>,
    seed: &nalgebra::DVector<f64>,
    failure: &str,
    max_iters: usize,
) -> Result<nalgebra::DVector<f64>> {
    let mut x = seed.clone();
    // Current linearization. The system is rebuilt after every accepted
    // iterate; the line search below reuses it when possible.
    for _ in 0..max_iters {
        let sys = build(&x)?;
        let x_raw = sys.solve()?;
        let res0 = sys.residual(&x);
        let delta = &x_raw - &x;
        let mut alpha = 1.0f64;
        let mut x_next = x_raw;
        // Damped Newton: backtrack while the nonlinear residual grows. This
        // is what keeps a strongly conducting diode (diffusion capacitance)
        // from oscillating between iterates at a tiny timestep.
        for _ in 0..10 {
            let candidate_sys = build(&x_next)?;
            let res = candidate_sys.residual(&x_next);
            if res <= res0.max(1e-12) || alpha <= 1.0 / 1024.0 {
                break;
            }
            alpha *= 0.5;
            x_next = &x + alpha * &delta;
        }
        let diff = (&x_next - &x).amax();
        let converged = diff < NR_TOLERANCE;
        x = x_next;
        if converged {
            return Ok(x);
        }
    }
    Err(SimError::Analysis(failure.into()))
}

/// Node voltages extracted from a solved unknown vector, ground pinned to 0.
fn extract_voltages<T: crate::mna::Field>(
    circuit: &Circuit,
    x: &nalgebra::DVector<T>,
) -> Vec<T> {
    let mut node_voltages = vec![T::lift(0.0); circuit.nodes];
    for i in 1..circuit.nodes {
        node_voltages[i] = node_voltage(x, crate::circuit::NodeId(i));
    }
    node_voltages
}

/// Run every analysis once per `.step` combination, or a single pass when
/// the deck has no `.step` cards.
pub fn run(netlist: &Netlist) -> Result<SimulationResult> {
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
    let combos = step_combinations(&specs)?;
    let base_temperature = netlist.circuit.temperature;
    let mut runs = Vec::with_capacity(combos.len());
    for combo in combos {
        let temp_position = targets.iter().position(|t| t == "temp");
        let temperature = temp_position.map(|i| combo[i]);
        let overrides: HashMap<String, f64> = targets
            .iter()
            .cloned()
            .zip(combo.iter().copied())
            .filter(|(target, _)| target != "temp")
            .collect();
        let mut inner = if overrides.is_empty() {
            netlist.clone()
        } else {
            crate::netlist::parse_with_overrides(&netlist.source, &netlist.options, &overrides)?
        };
        if let Some(t) = temperature {
            inner.circuit.temperature = t;
        } else {
            inner.circuit.temperature = base_temperature;
        }
        // The inner parse sees the same `.step`/`.temp` cards; running them
        // again would nest the sweep inside itself.
        inner.analysis.steps.clear();
        inner.analysis.temperatures.clear();
        let single = run_single(&inner)?;
        runs.push(StepRun {
            values: combo,
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
        .ok_or_else(|| SimError::Analysis(".step produced no runs".into()))?;
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

fn run_single(netlist: &Netlist) -> Result<SimulationResult> {
    let circuit = &netlist.circuit;
    let analysis = &netlist.analysis;
    circuit.validate()?;

    let mut result = SimulationResult {
        dc: None,
        dc_sweep: None,
        tran: None,
        ac: None,
        tf: None,
        step_targets: Vec::new(),
        runs: Vec::new(),
        measurements: Vec::new(),
        four: None,
        noise: None,
    };

    let want_dc_sweep = analysis.dc.is_some();
    let want_tf = analysis.tf.is_some();
    let run_noise = analysis.noise.is_some();
    let run_dc = analysis.dc_op
    || (analysis.dc.is_none()
        && analysis.tran_step.is_none() && analysis.tran_stop.is_none()
        && analysis.ac.is_none() && analysis.tf.is_none() && !run_noise);
    let run_tran = analysis.tran_step.is_some() && analysis.tran_stop.is_some();
    let run_ac = analysis.ac.is_some();

    if !run_dc && !want_dc_sweep && !run_tran && !run_ac && !want_tf && !run_noise {
        return Err(SimError::Analysis(
            "no analysis specified; add .op, .dc, .tran, .ac, .tf, or .noise".into(),
        ));
    }

    // UIC transient: the operating point is skipped entirely — the deck's
    // `.ic` entries pin the t=t_start state and every other unknown starts
    // at 0 (TODO §2). Without UIC the transient seeds from the DC solution
    // like AC does.
    let uic = analysis.tran_uic && run_tran;
    let uic_state = if uic {
        Some(build_uic_state(circuit, &analysis.ic)?)
    } else {
        None
    };

    // The DC solve is seeded by `.nodeset` entries in full, and by `.ic`
    // entries too whenever UIC does not claim them.
    let mut seed_entries: Vec<NodesetEntry> = analysis.nodeset.clone();
    if !uic {
        seed_entries.extend(analysis.ic.clone());
    }

    // Solve the DC operating point once, when anything needs it: as the
    // primary result for .op, as the initial state for .tran, and as the
    // linearisation point for .ac. (A .dc-sweep-only run solves per point
    // and skips this one.)
    let needs_op = run_dc || run_ac || want_tf || run_noise || (run_tran && !uic);
    if needs_op {
        let conv = DcConvergence::from_analysis(analysis);
        let seeds = resolve_seed(circuit, &seed_entries)?;
        let (dc_result, dc_x) = solve_dc_op(circuit, &seeds, conv, None)?;
        if run_dc {
            result.dc = Some(dc_result);
        }
        if run_ac {
            let ac_cmd = analysis.ac.as_ref().unwrap();
            result.ac = Some(run_ac_sweep(circuit, ac_cmd, &dc_x)?);
        }
        if want_tf {
            result.tf = Some(run_tf_analysis(
                circuit,
                analysis.tf.as_ref().unwrap(),
                &dc_x,
            )?);
        }
        if run_noise {
            result.noise = Some(run_noise_analysis(
                circuit,
                analysis.noise.as_ref().unwrap(),
                &dc_x,
            )?);
        }
        if run_tran && !uic {
            let stop = analysis.tran_stop.unwrap();
            let startup = analysis.tran_startup;
            // STARTUP seeds from the source-suppressed operating point: every
            // independent V/I card starts at zero and ramps over the first
            // 10 µs, so the pre-transient op must be solved the same way.
            let x_seed = if startup {
                let mut quiet = circuit.clone();
                for v in &mut quiet.voltage_sources {
                    v.voltage = 0.0;
                }
                for i in &mut quiet.current_sources {
                    i.current = 0.0;
                }
                let (_, x0) = solve_dc_op(&quiet, &seeds, conv, None)?;
                x0
            } else {
                dc_x.clone()
            };
            result.tran = Some(run_transient(
                circuit,
                analysis,
                &x_seed,
                stop,
                startup,
                netlist.mc_seed,
            )?);
        }
    }

    if uic && run_tran {
        let stop = analysis.tran_stop.unwrap();
        result.tran = Some(run_transient(
            circuit,
            analysis,
            uic_state.as_ref().unwrap(),
            stop,
            false,
            netlist.mc_seed,
        )?);
    }

    if want_dc_sweep {
        result.dc_sweep = Some(run_dc_sweep(netlist, analysis.dc.as_ref().unwrap(), &seed_entries)?);
    }

    result.measurements = evaluate_measurements(
        netlist,
        result.dc.as_ref(),
        result.dc_sweep.as_ref(),
        result.tran.as_ref(),
        result.ac.as_ref(),
    );
    if let Some(cmd) = &netlist.analysis.four {
        result.four = Some(evaluate_fourier(netlist, cmd, result.tran.as_ref())?);
    }
    Ok(result)
}

/// Turn the raw `.ic`/`.nodeset` entries into a seed map from the
/// *unknown-vector row* of a node to its initial value. Node names resolve
/// case-insensitively against the netlist's canonical node table; naming an
/// undefined node (or ground) is a loud error — silent-zero seeds have
/// burned too many SPICE decks.
fn resolve_seed(
    circuit: &Circuit,
    entries: &[NodesetEntry],
) -> Result<HashMap<usize, f64>> {
    let mut seeds: HashMap<usize, f64> = HashMap::new();
    for e in entries {
        let name = e.node.trim();
        if name.is_empty() {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if lower == "0" || lower == "gnd" {
            return Err(SimError::Analysis(format!(
                "node '{name}' is ground — its voltage cannot be seeded (V(0) is fixed at 0)"
            )));
        }
        let id = circuit
            .node_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                SimError::Analysis(format!(
                    "initial condition names node '{name}', which the netlist does not define"
                ))
            })?;
        // Node index `id` maps to unknown-vector row `id - 1` (ground has no
        // row) — the same layout `BranchLayout` publishes.
        seeds.insert(id - 1, e.value);
    }
    Ok(seeds)
}

/// The unknown-vector state an UIC transient starts from: every node
/// voltage at 0 except the entries pinned in `.ic`, branch rows all 0
/// (inductor currents start collapsed, per TODO §2).
fn build_uic_state(
    circuit: &Circuit,
    entries: &[NodesetEntry],
) -> Result<nalgebra::DVector<f64>> {
    let seeds = resolve_seed(circuit, entries)?;
    let mut x = nalgebra::DVector::zeros(circuit.tran_matrix_size());
    for (&row, &value) in &seeds {
        x[row] = value;
    }
    Ok(x)
}

/// DC operating-point convergence knobs, from `.options` (TODO §3 convergence
/// suite: the knobs LTspice and PSpice expose for hard-to-converge decks).
#[derive(Debug, Clone, Copy)]
pub struct DcConvergence {
    /// Conductance floor kept across every pn junction and capacitor.
    gmin: f64,
    /// Depth of the gmin-stepping retry ladder (one decade per rung).
    gminsteps: usize,
    /// Depth of the source-stepping retry ladder (one equal fraction per rung).
    srcsteps: usize,
    /// Newton-Raphson iteration budget per DC solve.
    itl1: usize,
}

impl DcConvergence {
    fn from_analysis(analysis: &crate::netlist::AnalysisCommands) -> Self {
        Self {
            gmin: analysis.config.gmin,
            gminsteps: analysis.config.gminsteps,
            srcsteps: analysis.config.srcsteps,
            itl1: analysis.config.itl1,
        }
    }
}

/// One DC Newton-Raphson solve at a given gmin floor and source scale, with
/// the junction limiter's memory seeded from `x_seed`. Used by the plain
/// operating point and every step of the gmin/source-stepping ladders.
fn dc_stage(
    circuit: &Circuit,
    x_seed: &nalgebra::DVector<f64>,
    gmin: f64,
    src_scale: f64,
    conv: DcConvergence,
) -> Result<nalgebra::DVector<f64>> {
    let mut prev = DcJunctionLimits::seed(circuit, x_seed);
    // Fresh table with one slot per element so the stamps can record the
    // limits they used; `Default` (empty vectors) would silently drop every
    // record and reset the limiting memory after the first swap.
    let mut used = DcJunctionLimits::zeroed(circuit);
    newton_raphson(
        |guess| {
            let sys = build_system::<f64>(
                circuit,
                Mode::Dc {
                    guess,
                    limits: &prev,
                    used: &mut used,
                    gmin,
                    src_scale,
                },
            )?;
            // The next build limits against what this one actually used
            // (the SPICE device-junction-state convention).
            std::mem::swap(&mut prev, &mut used);
            Ok(sys)
        },
        x_seed,
        "DC operating point failed to converge",
        conv.itl1,
    )
}

/// Solve the DC operating point and return both the summary struct and the
/// raw solution vector. The raw vector is needed by AC analysis (linearisation)
/// and can seed transient initial conditions.
///
/// `seeds` pin the Newton-Raphson starting iterate: `.nodeset` entries push
/// the DC iteration toward the requested point without forcing the solution
/// (the converged state is still the true operating point). `warm` seeds from
/// an already-solved point — what a `.dc` sweep hands on from the previous
/// sweep value.
///
/// **The retry ladder** (TODO §3 convergence suite, the Autoconverge shape):
/// first a plain solve; on failure, gmin stepping — the circuit is repeatedly
/// solved with the conductance floor a decade higher per rung and the floor
/// then walks back down to the deck's own `gmin`, each rung seeding the next;
/// on failure, source stepping — the independent sources come up from a small
/// fraction to full value, again each fraction seeding the next. A rung that
/// fails to converge inside `itl1` iterations is simply left behind: the next
/// rung starts from the last good iterate. If even the full-source final rung
/// fails everywhere, the plain classic error is reported.
fn solve_dc_op(
    circuit: &Circuit,
    seeds: &HashMap<usize, f64>,
    conv: DcConvergence,
    warm: Option<&nalgebra::DVector<f64>>,
) -> Result<(DcResult, nalgebra::DVector<f64>)> {
    let plain = match warm {
        Some(x) => x.clone(),
        None => {
            let mut seed = crate::mna::initial_guess(circuit);
            for (&row, &value) in seeds {
                seed[row] = value;
            }
            seed
        }
    };

    // Stage 1: the plain operating point (the overwhelmingly common path).
    if let Ok(x) = dc_stage(circuit, &plain, conv.gmin, 1.0, conv) {
        let result = extract_dc(circuit, &x)?;
        return Ok((result, x));
    }

    // Stage 2: gmin stepping. Rungs run from `gmin·10^gminsteps` down to the
    // deck's own gmin, one decade per rung; the last rung IS the real circuit.
    let mut x = plain.clone();
    for rung in (1..=conv.gminsteps.max(1)).rev() {
        let gmin_rung = conv.gmin * 10f64.powi(rung as i32);
        if let Ok(solved) = dc_stage(circuit, &x, gmin_rung, 1.0, conv) {
            x = solved;
        }
    }
    if let Ok(x) = dc_stage(circuit, &x, conv.gmin, 1.0, conv) {
        let result = extract_dc(circuit, &x)?;
        return Ok((result, x));
    }

    // Stage 3: source stepping, restarted from the plain seed. Every fraction
    // `k/srcsteps` of the independent sources is solved in order, each
    // fraction seeded by the last converged one; the final fraction
    // (`k = srcsteps`, scale exactly 1) is the full-reality deck.
    let steps = conv.srcsteps.max(1);
    let mut x = plain.clone();
    for k in 1..=steps {
        let scale = k as f64 / steps as f64;
        if let Ok(solved) = dc_stage(circuit, &x, conv.gmin, scale, conv) {
            x = solved;
            if k == steps {
                let result = extract_dc(circuit, &x)?;
                return Ok((result, x));
            }
        }
    }

    Err(SimError::Analysis("DC operating point failed to converge".into()))
}

fn extract_dc(circuit: &Circuit, x: &nalgebra::DVector<f64>) -> Result<DcResult> {
    let node_voltages = extract_voltages(circuit, x);
    let branch = BranchLayout::for_circuit(circuit);
    let source_currents = circuit
        .voltage_sources
        .iter()
        .enumerate()
        .map(|(i, _)| branch.src_row(x, i))
        .collect();
    // DC: no capacitor current (steady state), so pass None for dt/prev.
    let branch_currents = compute_branch_currents(circuit, x, None, None, None)?;
    Ok(DcResult { node_voltages, source_currents, branch_currents })
}

// ─── Annotated operating points (`.op all`) ─────────────────────────────────

/// One device's annotated operating point (TODO §4, the `.op all` view).
#[derive(Debug, Clone)]
pub struct OpPoint {
    /// Element name as the deck spells it.
    pub name: String,
    /// Parameters at the operating point, in display order: short labels
    /// with their quantity (`vd`, `ic`, `p`), engineering values. A switch's
    /// state is encoded `s = 1` (on) / `s = 0` (off).
    pub entries: Vec<(String, f64)>,
}

/// Annotated device operating points at a solved DC state (`.op all`):
/// every stamped element line's card-order operating parameters derived
/// from the reported node voltages and branch currents — the same data the
/// reference tools show when clicking a component in op mode.
pub fn device_op_points(
    circuit: &Circuit,
    node_voltages: &[f64],
    currents: &BranchCurrents<f64>,
    source_currents: &[f64],
) -> Vec<OpPoint> {
    let v = |node: crate::circuit::NodeId| node_voltages.get(node.0).copied().unwrap_or(0.0);
    // (label, value) pusher keeping every family's loop body flat.
    let at = |name: &str, entries: Vec<(&'static str, f64)>| -> OpPoint {
        OpPoint {
            name: name.to_string(),
            entries: entries.into_iter().map(|(k, val)| (k.to_string(), val)).collect(),
        }
    };
    let mut out = Vec::new();
    for (i, r) in circuit.resistors.iter().enumerate() {
        let vr = v(r.n1) - v(r.n2);
        let ir = currents.resistors.get(i).copied().unwrap_or(0.0);
        out.push(at(&r.name, vec![
            ("r", circuit.resistance_at(r)),
            ("v", vr),
            ("i", ir),
            ("p", vr * ir),
        ]));
    }
    for c in circuit.capacitors.iter() {
        // Steady state: a capacitor is an open circuit (current 0), so only
        // the held voltage is annotated.
        out.push(at(&c.name, vec![
            ("c", circuit.capacitance_at(c)),
            ("v", v(c.n1) - v(c.n2)),
        ]));
    }
    for (i, l) in circuit.inductors.iter().enumerate() {
        out.push(at(&l.name, vec![
            ("l", circuit.inductance_at(l)),
            ("i", currents.inductors.get(i).copied().unwrap_or(0.0)),
        ]));
    }
    for s in &circuit.switches {
        let control = match &s.control {
            crate::circuit::SwitchControl::Voltage(p, n) => v(*p) - v(*n),
            crate::circuit::SwitchControl::Current(name) => {
                // The controlling source's branch unknown, raw — exactly the
                // quantity the switch stamps threshold (missing source ⇒ 0).
                circuit
                    .voltage_sources
                    .iter()
                    .position(|vs| vs.name.eq_ignore_ascii_case(name))
                    .and_then(|idx| source_currents.get(idx).copied())
                    .unwrap_or(0.0)
            }
        };
        let was_on = s.initial;
        let (_g, on) = circuit
            .switch_models
            .get(&s.model)
            .map(|m| m.conductance(control, was_on))
            .unwrap_or((0.0, false));
        let vs = v(s.n1) - v(s.n2);
        out.push(at(&s.name, vec![
            ("s", if on { 1.0 } else { 0.0 }),
            ("v", vs),
        ]));
    }
    for (i, d) in circuit.diodes.iter().enumerate() {
        let vd = v(d.anode) - v(d.cathode);
        let id = currents.diodes.get(i).copied().unwrap_or(0.0);
        out.push(at(&d.name, vec![("vd", vd), ("id", id), ("p", vd * id)]));
    }
    for (i, m) in circuit.mosfets.iter().enumerate() {
        let id = currents.mosfets.get(i).copied().unwrap_or(0.0);
        out.push(at(&m.name, vec![
            ("vgs", v(m.ng) - v(m.ns)),
            ("vds", v(m.nd) - v(m.ns)),
            ("vbs", v(m.nb) - v(m.ns)),
            ("id", id),
        ]));
    }
    for (i, q) in circuit.bjts.iter().enumerate() {
        let ic = currents.bjts.get(i).copied().unwrap_or(0.0);
        out.push(at(&q.name, vec![
            ("vbe", v(q.nb) - v(q.ne)),
            ("vbc", v(q.nb) - v(q.nc)),
            ("vce", v(q.nc) - v(q.ne)),
            ("ic", ic),
            ("p", (v(q.nc) - v(q.ne)) * ic),
        ]));
    }
    for (i, j) in circuit.jfets.iter().enumerate() {
        let id = currents.jfets.get(i).copied().unwrap_or(0.0);
        out.push(at(&j.name, vec![
            ("vgs", v(j.ng) - v(j.ns)),
            ("vds", v(j.nd) - v(j.ns)),
            ("id", id),
        ]));
    }
    for cs in &circuit.current_sources {
        let is = match &cs.waveform {
            Some(w) => w.value_at(0.0),
            None => cs.current,
        };
        // Delivered power: the card convention drives `is` from μn1 through
        // the source into μn2, so it supplies `is·(v(n2) − v(n1))`.
        let vc = v(cs.n2) - v(cs.n1);
        out.push(at(&cs.name, vec![("i", is), ("v", vc), ("p", is * vc)]));
    }
    for (idx, vs) in circuit.voltage_sources.iter().enumerate() {
        let card = vs.value_at(0.0);
        // `source_currents` is the raw branch unknown (element convention —
        // a delivering source reads negative), so `p` is negative while the
        // source is supplying, matching the classic SPICE sign.
        let total = source_currents.get(idx).copied().unwrap_or(0.0);
        out.push(at(
            &vs.name,
            vec![("v", card), ("i", total), ("p", card * total)],
        ));
    }
    // E/F/G/H: the output value at the operating point — the polynomial
    // evaluated on the converged controls. Branch-side controlled devices
    // carry their current only in the solver's raw vector, so the annotated
    // view reports the source's own output value, which the deck's transfer
    // is expressed in.
    for cs in circuit
        .vcvs
        .iter()
        .chain(circuit.vccs.iter())
        .chain(circuit.ccvs.iter())
        .chain(circuit.cccs.iter())
    {
        let val = controlled_value(circuit, cs, node_voltages, source_currents);
        out.push(at(&cs.name, vec![("val", val)]));
    }
    // Op-amps: output voltage, output current (the branch unknown) and the
    // element-convention power product, with the driving gain recorded.
    for (i, u) in circuit.opamps.iter().enumerate() {
        let params = circuit.opamp_params_at(&u.model);
        let vout = v(u.out_p) - v(u.out_n);
        let io = currents.opamps.get(i).copied().unwrap_or(0.0);
        out.push(at(&u.name, vec![
            ("a", params.gain),
            ("v", vout),
            ("i", io),
            ("p", vout * io),
        ]));
    }
    for (i, b) in circuit.behavioral.iter().enumerate() {
        // Evaluate the expression against the same probe snapshot the MNA
        // stamps read: node differences and the raw branch unknowns.
        let probe_values: Vec<f64> = b
            .behavior
            .probes
            .iter()
            .map(|probe| match probe {
                crate::behavior::ProbeBinding::Voltage(p, m) => v(*p) - v(*m),
                crate::behavior::ProbeBinding::SourceCurrent(i) => source_currents
                    .get(*i)
                    .copied()
                    .unwrap_or(0.0),
                crate::behavior::ProbeBinding::InductorCurrent(i) => {
                    currents.inductors.get(*i).copied().unwrap_or(0.0)
                }
                crate::behavior::ProbeBinding::Unresolved(_) => {
                    unreachable!("probes bind before any report runs")
                }
            })
            .collect();
        let value = b.behavior.evaluate(&probe_values, 0.0, 0.0, &BehavioralRuntime::default())
            // Dynamic ABM forms report the DC transfer of the frequency data
            // everywhere outside AC.
            * b.abm.as_ref().map(|a| a.dc_gain()).unwrap_or(1.0);
        let current = currents.behavioral.get(i).copied().unwrap_or(0.0);
        match b.kind {
            crate::circuit::BehavioralKind::Voltage => {
                out.push(at(&b.name, vec![("v", value), ("i", current)]));
            }
            crate::circuit::BehavioralKind::Current => {
                let vb = v(b.out_p) - v(b.out_n);
                out.push(at(
                    &b.name,
                    vec![("i", value), ("v", vb), ("p", vb * value)],
                ));
            }
        }
    }
    out
}

/// One controlled source's output value at the operating point.
fn controlled_value(
    circuit: &Circuit,
    cs: &crate::circuit::ControlledSource,
    node_voltages: &[f64],
    source_currents: &[f64],
) -> f64 {
    let v = |node: crate::circuit::NodeId| node_voltages.get(node.0).copied().unwrap_or(0.0);
    let mut val = cs.coeffs.first().copied().unwrap_or(0.0);
    for (k, var) in cs.controls.iter().enumerate() {
        let x = match var {
            crate::circuit::ControlVar::Voltage(p, n) => v(*p) - v(*n),
            crate::circuit::ControlVar::Current(name) => circuit
                .voltage_sources
                .iter()
                .position(|vs| vs.name.eq_ignore_ascii_case(name))
                .and_then(|idx| source_currents.get(idx).copied())
                .unwrap_or(0.0),
        };
        val += cs.coeffs.get(k + 1).copied().unwrap_or(0.0) * x;
    }
    val
}

// ─── .dc sweep ────────────────────────────────────────────────────────────────

/// Where one sweep spec's values get applied.
#[derive(Debug, Clone)]
enum SweepTarget {
    VoltageSource(usize),
    CurrentSource(usize),
    Param(String),
}

/// Hard ceiling so a fat-fingered increment cannot fork-bomb the machine.
const MAX_SWEEP_POINTS: usize = 1_000_000;

/// Materialise a sweep spec's value list (also validates the ramp).
fn sweep_values(spec: &DcSweepValues, target: &str) -> Result<Vec<f64>> {
    match spec {
        DcSweepValues::List(v) => Ok(v.clone()),
        DcSweepValues::Linear { start, stop, step } => {
            let sign = step.signum();
            let eps = (stop - start).abs() * 1e-9 + 1e-12;
            let mut values = Vec::new();
            let mut v = *start;
            while ((sign > 0.0 && v <= stop + eps) || (sign < 0.0 && v >= stop - eps))
                && values.len() <= MAX_SWEEP_POINTS
            {
                values.push(v);
                v += step;
            }
            if values.len() > MAX_SWEEP_POINTS {
                return Err(SimError::Analysis(format!(
                    ".dc {} would produce more than {MAX_SWEEP_POINTS} points — lower the resolution",
                    target
                )));
            }
            Ok(values)
        }
    }
}

/// Match a sweep target against the independent sources and the `.param`
/// table. Unknown names are loud errors, never silently ignored.
fn resolve_target(netlist: &Netlist, target: &str) -> Result<SweepTarget> {
    let lower = target.to_ascii_lowercase();
    if let Some(i) = netlist
        .circuit
        .voltage_sources
        .iter()
        .position(|v| v.name.eq_ignore_ascii_case(target))
    {
        return Ok(SweepTarget::VoltageSource(i));
    }
    if let Some(i) = netlist
        .circuit
        .current_sources
        .iter()
        .position(|i_src| i_src.name.eq_ignore_ascii_case(target))
    {
        return Ok(SweepTarget::CurrentSource(i));
    }
    if netlist.params.contains_key(&lower) {
        return Ok(SweepTarget::Param(lower));
    }
    Err(SimError::Analysis(format!(
        ".dc cannot sweep '{target}': not an independent source (V/I) or a .param"
    )))
}

/// Produce the circuit for one sweep point: parameters go through a re-parse
/// of the original deck with the override pinned; independent sources are
/// patched in place.
fn materialize_circuit(
    netlist: &Netlist,
    targets: &[SweepTarget],
    values: &[f64],
) -> Result<Circuit> {
    // Start from the enclosing `.step` pins, then overlay the `.dc` point —
    // a parameter sweep must not drop the stepped value.
    let mut params: HashMap<String, f64> = netlist.overrides.clone();
    for (target, value) in targets.iter().zip(values) {
        if let SweepTarget::Param(name) = target {
            params.insert(name.clone(), *value);
        }
    }
    let mut circuit = if params.is_empty() {
        netlist.circuit.clone()
    } else {
        let inner =
            crate::netlist::parse_with_overrides(&netlist.source, &netlist.options, &params)
                .map_err(|e| SimError::Analysis(format!("sweep point evaluation failed: {e}")))?;
        inner.circuit
    };
    for (target, value) in targets.iter().zip(values) {
        match target {
            SweepTarget::VoltageSource(i) => circuit.voltage_sources[*i].voltage = *value,
            SweepTarget::CurrentSource(i) => circuit.current_sources[*i].current = *value,
            SweepTarget::Param(_) => {}
        }
    }
    Ok(circuit)
}

/// Run a real `.dc` sweep: one operating point per swept value, with the
/// optional second sweep nested inside the first. `seed_entries` are the
/// `.nodeset`/`.ic` cards resolved against every materialized circuit — a
/// swept parameter can rebuild the net, so the rows are resolved per point.
fn run_dc_sweep(
    netlist: &Netlist,
    cmd: &DcCommand,
    seed_entries: &[NodesetEntry],
) -> Result<DcSweepResult> {
    let mut targets = Vec::with_capacity(cmd.sweeps.len());
    for spec in &cmd.sweeps {
        targets.push(resolve_target(netlist, &spec.target)?);
    }

    let primary = sweep_values(&cmd.sweeps[0].values, &cmd.sweeps[0].target)?;
    let secondary = match cmd.sweeps.get(1) {
        Some(spec) => Some(sweep_values(&spec.values, &spec.target)?),
        None => None,
    };
    if primary.len().saturating_mul(secondary.as_ref().map_or(1, |s| s.len())) > MAX_SWEEP_POINTS {
        return Err(SimError::Analysis(format!(
            ".dc would produce more than {MAX_SWEEP_POINTS} total points — lower the resolution"
        )));
    }

    let mut points = Vec::new();
    let mut expected_nodes: Option<usize> = None;
    // Each sweep point seeds from the previous point's solution (the SPICE
    // DC-transfer convention): a stiff sweep walks smoothly and the retry
    // ladder only fires on the first point if at all.
    let mut warm: Option<nalgebra::DVector<f64>> = None;
    let conv = DcConvergence::from_analysis(
        // The sweep re-materialises the same deck, so the options it was
        // parsed with carry the convergence knobs.
        &netlist.analysis,
    );
    for &pv in &primary {
        // The second sweep is the inner loop (SPICE nesting convention); a
        // single value stands in when there is no nested sweep.
        let inner: &[f64] = secondary.as_deref().unwrap_or(&[f64::NAN]);
        for &sv in inner {
            let values: Vec<f64> = if secondary.is_some() {
                vec![pv, sv]
            } else {
                vec![pv]
            };
            let circuit = materialize_circuit(netlist, &targets, &values)?;
            let seeds = resolve_seed(&circuit, seed_entries)?;
            let (dc, x) = solve_dc_op(&circuit, &seeds, conv, warm.as_ref())?;
            warm = Some(x);
            match expected_nodes {
                None => expected_nodes = Some(dc.node_voltages.len()),
                Some(n) if n != dc.node_voltages.len() => {
                    return Err(SimError::Analysis(
                        "sweep points disagree on the node count — the swept parameter \
                         changes the circuit's connectivity"
                            .to_string(),
                    ));
                }
                _ => {}
            }
            points.push(DcSweepPoint {
                values,
                node_voltages: dc.node_voltages,
                source_currents: dc.source_currents,
                branch_currents: dc.branch_currents,
            });
        }
    }

    Ok(DcSweepResult {
        targets: cmd.sweeps.iter().map(|s| s.target.clone()).collect(),
        points,
    })
}

/// Run the backward-Euler transient sweep, seeded from `x_start` — the DC
/// operating-point solution, or the UIC pin-state vector when `.tran … UIC`
/// puts the deck in charge of the initial state (then node voltages beyond
/// the `.ic` entries start at 0 and inductor currents are 0). With `startup`
/// every independent V/I source is ramped from 0 to its card value over the
/// first 10 µs and `x_start` arrived from the zero-source operating point.
fn run_transient(
    circuit: &Circuit,
    analysis: &crate::netlist::AnalysisCommands,
    x_start: &nalgebra::DVector<f64>,
    t_stop: f64,
    startup: bool,
    noise_seed: u64,
) -> Result<TranResult> {
    let dt_out = analysis.tran_step.ok_or_else(|| {
        SimError::Analysis("transient needs a print interval (.tran <tstep> <tstop>)".into())
    })?;
    if dt_out <= 0.0 {
        return Err(SimError::Analysis(".tran tstep must be > 0".into()));
    }
    let t_start = analysis.tran_start;
    if t_stop < 0.0 || t_start < 0.0 {
        return Err(SimError::Analysis("tstart and tstop must be >= 0".into()));
    }
    if t_start > t_stop {
        return Err(SimError::Analysis("tstop must be >= tstart".into()));
    }

    let method = analysis.config.method;
    let max_step = analysis.tran_maxstep.unwrap_or(dt_out).min(t_stop.max(dt_out));
    let min_step = (t_stop * 1e-12).max(1e-18);

    let mut x = x_start.clone();
    let mut state = seed_transient_state(circuit, &x, noise_seed);
    let mut points = Vec::new();
    let eps = t_stop.abs() * 1e-12 + 1e-18;

    if t_start <= eps {
        points.push(TranPoint {
            time: 0.0,
            node_voltages: extract_voltages(circuit, &x),
            branch_currents: compute_branch_currents_at(circuit, &x, 0.0, &state)?,
        });
    }

    // Source edges are hard breakpoints: clip every step to land on them so a
    // narrow pulse is never stepped over (TODO §3 discontinuity handling).
    let mut breakpoints = source_breakpoints(circuit, t_stop);
    breakpoints.retain(|t| *t > eps && *t < t_stop - eps);
    breakpoints.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    breakpoints.dedup_by(|a, b| (*a - *b).abs() <= eps);
    let mut bp_index = 0usize;

    let mut next_grid = dt_out;
    let mut t = 0.0;
    let mut h = (dt_out * 0.1).min(max_step).min(t_stop.max(dt_out));
    let order = if method == IntegrationMethod::Gear1 { 1.0 } else { 2.0 };
    let mut iterations = 0usize;
    const MAX_STEPS: usize = 10_000_000;
    // The transient path's iteration budget: `.options itl4=` (per time
    // point; the defaults keep the pre-options 100).
    let max_iters = analysis.config.itl4;

    while t < t_stop - eps {
        if iterations > MAX_STEPS {
            return Err(SimError::Analysis("transient exceeded maximum steps".into()));
        }
        iterations += 1;

        while bp_index < breakpoints.len() && breakpoints[bp_index] <= t + eps {
            bp_index += 1;
        }
        let next_break = breakpoints.get(bp_index).copied().unwrap_or(t_stop);
        while next_grid <= t + eps {
            next_grid += dt_out;
        }
        let boundary = t_stop.min(next_break).min(next_grid).max(t + min_step);
        let h_try = h.min(boundary - t).max(min_step);

        // Step doubling: one full step and two half steps. Their difference
        // estimates the local truncation error; the half-step solution (the
        // more accurate one) is what we advance with.
        let attempt = (|| -> Result<(nalgebra::DVector<f64>, TransientState, f64)> {
            let (x_full, s_full) =
                transient_step(circuit, &x, &state, t, h_try, method, startup, max_iters)?;
            let (x1, s1) =
                transient_step(circuit, &x, &state, t, h_try * 0.5, method, startup, max_iters)?;
            let (x2, _s2) = transient_step(
                circuit,
                &x1,
                &s1,
                t + h_try * 0.5,
                h_try * 0.5,
                method,
                startup,
                max_iters,
            )?;
            let err = transient_error(&x_full, &x2, circuit, analysis);
            if x_full.iter().any(|v| !v.is_finite()) {
                return Ok((x_full, s_full, f64::INFINITY));
            }
            Ok((x_full, s_full, err))
        })();

        match attempt {
            Ok((x2, s2, err)) if err <= 1.0 || h_try <= min_step * 1.01 => {
                x = x2;
                state = s2;
                t += h_try;
                if t >= t_start - eps {
                    points.push(TranPoint {
                        time: t,
                        node_voltages: extract_voltages(circuit, &x),
                        branch_currents: compute_branch_currents_at(circuit, &x, t, &state)?,
                    });
                }
                let mut factor = growth_factor(err, order);
                if method == IntegrationMethod::Gear2 {
                    // BDF2 is only conditionally stable under large step
                    // ratios; keep consecutive steps within 2×.
                    factor = factor.min(1.2);
                }
                h = (h_try * factor).clamp(min_step, max_step);
            }
            Ok((_, _, err)) => {
                // Rejected: shrink from the attempted step, not from the
                // (possibly clipped) proposal.
                let factor = growth_factor(err, order).min(0.5);
                h = (h_try * factor).clamp(min_step, max_step);
            }
            Err(_) => {
                // Convergence failure at this step size: halve and retry.
                h = (h_try * 0.5).max(min_step);
                if h >= h_try {
                    return Err(SimError::Analysis(format!(
                        "Transient failed to converge at t={t} (step {h_try})"
                    )));
                }
            }
        }
    }

    Ok(TranResult { points })
}

/// One NR transient step plus its history update.
fn transient_step(
    circuit: &Circuit,
    x_guess: &nalgebra::DVector<f64>,
    state: &TransientState,
    t: f64,
    dt: f64,
    method: IntegrationMethod,
    startup: bool,
    max_iters: usize,
) -> Result<(nalgebra::DVector<f64>, TransientState)> {
    // The step solves for the *end* of the interval; sources must be
    // evaluated there (the previous code sampled the start time).
    let t_new = t + dt;
    let x = newton_raphson(
        |guess| {
            build_system::<f64>(
                circuit,
                Mode::Tran { dt, t: t_new, state, method, guess, startup },
            )
        },
        x_guess,
        &format!("Transient failed to converge at t={t_new}"),
        max_iters,
    )?;
    let state = advance_transient_state(circuit, &x, state, dt, method, t_new);
    Ok((x, state))
}

/// Normalised LTE estimate between the full step and the two half steps.
fn transient_error(
    full: &nalgebra::DVector<f64>,
    half: &nalgebra::DVector<f64>,
    circuit: &Circuit,
    analysis: &crate::netlist::AnalysisCommands,
) -> f64 {
    let n_nodes = circuit.nodes.saturating_sub(1);
    let mut worst = 0.0f64;
    // Node voltages carry the physical accuracy; branch currents are derived
    // from them and their absolute tolerance (abstol) would otherwise force
    // absurdly small steps whenever a displacement current is a few hundred
    // nanoamps. Restrict the LTE norm to the node rows.
    for i in 0..n_nodes.min(full.len()).min(half.len()) {
        let a = full[i];
        let b = half[i];
        let tol = analysis.config.reltol * a.abs().max(b.abs()) + analysis.config.vntol;
        let e = (a - b).abs() / tol.max(1e-30);
        worst = worst.max(e);
    }
    worst
}

fn growth_factor(err: f64, order: f64) -> f64 {
    if !err.is_finite() {
        return 0.2;
    }
    if err <= 0.0 {
        return 5.0;
    }
    (0.9 * err.powf(-1.0 / (order + 1.0))).clamp(0.2, 5.0)
}

/// Every source edge instant inside the run window.
fn source_breakpoints(circuit: &Circuit, t_stop: f64) -> Vec<f64> {
    let mut out = Vec::new();
    for v in &circuit.voltage_sources {
        out.extend(v.breakpoints());
    }
    for i in &circuit.current_sources {
        out.extend(i.breakpoints());
    }
    out.retain(|t| t.is_finite() && *t > 0.0 && *t < t_stop);
    out
}

// ─── AC frequency sweep ───────────────────────────────────────────────────────

/// Params note: `AcScale`/`AcCommand` live in `netlist.rs` — the command
/// grammar is owned by the netlist module and analysis merely consumes it.
/// (The old analysis-local definitions created the mutual-import tangle.)
///
/// Build the frequency list for the sweep.
pub fn ac_frequencies(cmd: &AcCommand) -> Vec<f64> {
    match cmd.scale {
        AcScale::Lin => {
            let n = cmd.points.max(2);
            (0..n)
            .map(|i| cmd.f_start + (cmd.f_stop - cmd.f_start) * i as f64 / (n - 1) as f64)
            .collect()
        }
        AcScale::Dec => {
            let decades = (cmd.f_stop / cmd.f_start).log10();
            let total = (decades * cmd.points as f64).round() as usize + 1;
            (0..total)
            .map(|i| {
                cmd.f_start * 10f64.powf(decades * i as f64 / (total - 1) as f64)
            })
            .collect()
        }
        AcScale::Oct => {
            let octaves = (cmd.f_stop / cmd.f_start).log2();
            let total = (octaves * cmd.points as f64).round() as usize + 1;
            (0..total)
            .map(|i| {
                cmd.f_start * 2f64.powf(octaves * i as f64 / (total - 1) as f64)
            })
            .collect()
        }
    }
}

/// Run a small-signal AC frequency sweep.
///
/// Steps:
/// 1. Solve the DC operating point to linearise nonlinear elements.
/// 2. For each frequency, build the complex MNA, inject the stimulus, solve.
/// 3. Return complex node voltages at every frequency point.
pub fn run_ac_sweep(circuit: &Circuit, cmd: &AcCommand, dc_x: &nalgebra::DVector<f64>) -> Result<AcResult> {
    if cmd.f_start <= 0.0 {
        return Err(SimError::Analysis("AC start frequency must be > 0 Hz".into()));
    }
    if cmd.f_stop < cmd.f_start {
        return Err(SimError::Analysis("AC stop frequency must be >= start frequency".into()));
    }
    if cmd.points == 0 {
        return Err(SimError::Analysis("AC analysis requires at least 1 point".into()));
    }
    if circuit.voltage_sources.is_empty() {
        return Err(SimError::Analysis(
            "AC analysis requires at least one voltage source as a stimulus".into(),
        ));
    }

    // Which voltage source is the AC stimulus?
    let stim_idx = cmd.stimulus_source.unwrap_or(0);
    if stim_idx >= circuit.voltage_sources.len() {
        return Err(SimError::Analysis(format!(
            "stimulus source index {} out of range (circuit has {} voltage sources)",
                                              stim_idx,
                                              circuit.voltage_sources.len()
        )));
    }
    let stim_node = circuit.voltage_sources[stim_idx].n1;
    let stim_amp = circuit.voltage_sources[stim_idx].ac.as_ref().ok_or_else(|| {
        SimError::Analysis(format!(
            "stimulus source '{}' has no AC magnitude; add an 'AC <amp>' field",
            circuit.voltage_sources[stim_idx].name
        ))
    })?.mag;

    // Step 2 — sweep.
    let freqs = ac_frequencies(cmd);
    let mut points = Vec::with_capacity(freqs.len());

    for &freq in &freqs {
        let mut sys = build_system::<Complex64>(circuit, Mode::Ac { freq, bias: dc_x })?;
        // Drive the stimulus source with the AC amplitude.
        // All other sources remain at 0 V (short circuit in small-signal model).
        sys.drive_source(stim_idx, Complex64::new(stim_amp, 0.0));

        let x = sys.solve()?;

        let node_voltages = extract_voltages(circuit, &x);
        let branch_currents = compute_ac_branch_currents(circuit, &x, freq, dc_x)?;

        points.push(AcPoint { freq, node_voltages, branch_currents });
    }

    Ok(AcResult {
        points,
       stimulus_node: stim_node.0,
    })
}

// ─── .tf — DC transfer function ───────────────────────────────────────────────

/// Resolve a `.tf` node operand to a node id (case-insensitive, ground
/// included); an unknown name is a loud error.
fn tf_node(circuit: &Circuit, name: &str) -> Result<usize> {
    let lower = name.trim().to_ascii_lowercase();
    if lower == "0" || lower == "gnd" {
        return Ok(0);
    }
    circuit
        .node_names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(name.trim()))
        .ok_or_else(|| {
            SimError::Analysis(format!(
                ".tf names node '{}', which the netlist does not define",
                name.trim()
            ))
        })
}

/// Evaluate the `.tf` output operand from a solved (real, zero-frequency)
/// system.
fn tf_output_value(
    circuit: &Circuit,
    layout: BranchLayout,
    x: &nalgebra::DVector<f64>,
    output: &TfOutput,
) -> Result<(f64, String)> {
    match output {
        TfOutput::Voltage { node, reference } => {
            let p = tf_node(circuit, node)?;
            let n = match reference {
                Some(r) => tf_node(circuit, r)?,
                None => 0,
            };
            if p == 0 && n == 0 {
                return Err(SimError::Analysis(
                    ".tf output V(0) is identically zero — name a real node".into(),
                ));
            }
            let value =
                node_voltage(x, crate::circuit::NodeId(p)) - node_voltage(x, crate::circuit::NodeId(n));
            let label = match reference {
                Some(r) => format!("V({node},{r})"),
                None => format!("V({node})"),
            };
            Ok((value, label))
        }
        TfOutput::Current(name) => {
            let idx = circuit
                .voltage_sources
                .iter()
                .position(|v| v.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    SimError::Analysis(format!(
                        ".tf output I({name}) must name a voltage source"
                    ))
                })?;
            Ok((layout.src_row(x, idx), format!("I({name})")))
        }
    }
}

/// DC transfer function: solve the small-signal system at the operating
/// point with a unit input, then report the transfer and both impedances.
///
/// The zero-frequency `Ac` build gives exactly the linearised DC network:
/// capacitors open, inductors short, nonlinear devices linearised at `bias`.
pub fn run_tf_analysis(
    circuit: &Circuit,
    cmd: &TfCommand,
    bias: &nalgebra::DVector<f64>,
) -> Result<TfResult> {
    let input_idx = circuit
        .voltage_sources
        .iter()
        .position(|v| v.name.eq_ignore_ascii_case(&cmd.input))
        .ok_or_else(|| {
            SimError::Analysis(format!(
                ".tf input '{}' is not an independent voltage source",
                cmd.input
            ))
        })?;

    let mut sys = build_system::<f64>(circuit, Mode::Ac { freq: 0.0, bias })?;
    // Small-signal: every independent source (and every constant offset) is
    // zero except the unit input.
    sys.clear_rhs();
    let layout = sys.branch;
    sys.drive_source(input_idx, 1.0);
    let x = sys.solve()?;

    let (transfer, label) = tf_output_value(circuit, layout, &x, &cmd.output)?;

    // Driving-point impedance: 1 V drives `-j` into the network.
    let i_in = -layout.src_row(&x, input_idx);
    let input_impedance = if i_in.abs() > 1e-30 { 1.0 / i_in } else { f64::INFINITY };

    // Output impedance: inject 1 A at the output port with every independent
    // source zeroed. A branch-current output has no single driving point.
    let output_impedance = match &cmd.output {
        TfOutput::Voltage { node, reference } => {
            let p = tf_node(circuit, node)?;
            let n = reference.as_deref().map(|r| tf_node(circuit, r)).transpose()?.unwrap_or(0);
            if p == 0 && n == 0 {
                f64::NAN
            } else {
                let mut probe = build_system::<f64>(circuit, Mode::Ac { freq: 0.0, bias })?;
                probe.clear_rhs();
                probe.drive_node(crate::circuit::NodeId(p), 1.0);
                probe.drive_node(crate::circuit::NodeId(n), -1.0);
                let xp = probe.solve()?;
                node_voltage(&xp, crate::circuit::NodeId(p))
                    - node_voltage(&xp, crate::circuit::NodeId(n))
            }
        }
        TfOutput::Current(_) => f64::NAN,
    };

    Ok(TfResult {
        output: label,
        input: cmd.input.clone(),
        transfer,
        input_impedance,
        output_impedance,
    })
}

// ─── .meas — measurement evaluation ─────────────────────────────────────────

use crate::netlist::{MeasAnalysis, MeasEdge, MeasFunc, MeasKind, Measurement};
use crate::params::{ParamEnv, ParamValue};

/// One point of the measured waveform: the abscissa plus every probed
/// quantity, collapsed to real values (AC currents/voltages use magnitude).
/// Branch reports are the simulator's own [`BranchCurrents<f64>`] — no
/// per-consumer family copy to fall out of step with (the old `MeasCurrents`
/// clone dropped `bjts` when the BJT landed, so `I(Q1)` never resolved).
#[derive(Debug, Clone)]
struct MeasSample {
    x: f64,
    nodes: Vec<f64>,
    currents: BranchCurrents<f64>,
}

/// AC branch currents collapse to magnitudes for measurement arithmetic: a
/// `.meas` on an AC analysis reads the magnitudes the AC report prints.
fn collapse_ac(b: &BranchCurrents<Complex64>) -> BranchCurrents<f64> {
    let mag = |v: &[Complex64]| v.iter().map(|c| c.norm()).collect();
    BranchCurrents {
        resistors: mag(&b.resistors),
        capacitors: mag(&b.capacitors),
        inductors: mag(&b.inductors),
        voltage_sources: mag(&b.voltage_sources),
        current_sources: mag(&b.current_sources),
        diodes: mag(&b.diodes),
        mosfets: mag(&b.mosfets),
        bjts: mag(&b.bjts),
        jfets: mag(&b.jfets),
        behavioral: mag(&b.behavioral),
        opamps: mag(&b.opamps),
    }
}

/// Gather the samples one `.meas` analysis reads.
fn measurement_samples(
    analysis: MeasAnalysis,
    dc: Option<&DcResult>,
    dc_sweep: Option<&DcSweepResult>,
    tran: Option<&TranResult>,
    ac: Option<&AcResult>,
) -> std::result::Result<Vec<MeasSample>, String> {
    match analysis {
        MeasAnalysis::Tran => {
            let tran = tran.ok_or_else(|| "no .tran result to measure".to_string())?;
            Ok(tran
                .points
                .iter()
                .map(|p| MeasSample {
                    x: p.time,
                    nodes: p.node_voltages.clone(),
                    currents: p.branch_currents.clone(),
                })
                .collect())
        }
        MeasAnalysis::Dc => {
            let sweep = dc_sweep
                .ok_or_else(|| "no .dc sweep result to measure (add a .dc sweep)".to_string())?;
            Ok(sweep
                .points
                .iter()
                .map(|p| MeasSample {
                    x: p.values.first().copied().unwrap_or(0.0),
                    nodes: p.node_voltages.clone(),
                    currents: p.branch_currents.clone(),
                })
                .collect())
        }
        MeasAnalysis::Ac => {
            let ac = ac.ok_or_else(|| "no .ac result to measure".to_string())?;
            Ok(ac
                .points
                .iter()
                .map(|p| MeasSample {
                    x: p.freq,
                    nodes: p.node_voltages.iter().map(|v| v.norm()).collect(),
                    currents: collapse_ac(&p.branch_currents),
                })
                .collect())
        }
        MeasAnalysis::Op => {
            let dc = dc.ok_or_else(|| "no operating point to measure".to_string())?;
            Ok(vec![MeasSample {
                x: 0.0,
                nodes: dc.node_voltages.clone(),
                currents: dc.branch_currents.clone(),
            }])
        }
    }
}

/// One operand of a measurement expression, resolved once against the
/// circuit: the parameter arithmetic stays text, while every `V()`/`I()`
/// probe becomes a typed position its value is read from — the
/// `behavior::ProbeSpec` pattern applied to `.meas`.
enum MeasPart {
    /// Literal arithmetic text between probes.
    Text(String),
    /// `V(net)` — the node id its sample value is read at (0 is ground).
    Node(usize),
    /// `V(a,b)` — the differential read between two node ids.
    NodeDiff(usize, usize),
    /// `I(el)` — family plus index into the family's report `Vec`.
    Current(ElementFamily, usize),
}

/// A measurement expression compiled once per run: probes resolved and
/// validated when the analysis starts, so per-sample work is value lookup
/// plus the parameter engine — not a fresh syntax scan per point.
struct MeasProgram {
    parts: Vec<MeasPart>,
}

impl MeasProgram {
    /// Compile `expr`: split out the `V(…)`/`I(…)` operands (balanced-paren
    /// aware, so nested calls like `f(V(a))` stay intact) and bind them to
    /// the circuit. Unknown nets or elements fail loudly here, once.
    fn compile(expr: &str, circuit: &Circuit) -> std::result::Result<Self, String> {
        let chars: Vec<char> = expr.chars().collect();
        let mut parts: Vec<MeasPart> = Vec::new();
        let mut text = String::with_capacity(expr.len());
        let mut i = 0usize;
        while i < chars.len() {
            let c = chars[i];
            if matches!(c, 'v' | 'V' | 'i' | 'I') && chars.get(i + 1) == Some(&'(') {
                let mut depth = 1usize;
                let mut j = i + 2;
                while j < chars.len() {
                    match chars[j] {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                if j >= chars.len() {
                    return Err(format!("expression '{expr}' has an unbalanced probe"));
                }
                let inner: String = chars[i + 2..j].iter().collect();
                let probe = match c.to_ascii_lowercase() {
                    'v' => Self::compile_voltage(circuit, &inner)?,
                    _ => {
                        let name = inner.trim();
                        let (family, index) = circuit
                            .resolve_current_probe(name)
                            .ok_or_else(|| {
                                format!("measurement names no element '{name}' for I(…)")
                            })?;
                        MeasPart::Current(family, index)
                    }
                };
                if !text.is_empty() {
                    parts.push(MeasPart::Text(std::mem::take(&mut text)));
                }
                parts.push(probe);
                i = j + 1;
                continue;
            }
            text.push(c);
            i += 1;
        }
        if !text.is_empty() {
            parts.push(MeasPart::Text(text));
        }
        Ok(MeasProgram { parts })
    }

    /// Bind `V(net)` / `V(a,b)`: names resolve through `Circuit::node_id`
    /// (the ground/case alias lore in one place), with the numeric-id
    /// fallback the reference tools accept.
    fn compile_voltage(circuit: &Circuit, inner: &str) -> std::result::Result<MeasPart, String> {
        let mut nets = inner.split(',').map(str::trim);
        let resolve = |name: &str| -> std::result::Result<usize, String> {
            if let Some(crate::circuit::NodeId(id)) = circuit.node_id(name) {
                return Ok(id);
            }
            if let Ok(id) = name.to_ascii_lowercase().parse::<usize>() {
                if id < circuit.nodes {
                    return Ok(id);
                }
                return Err(format!("node '{name}' is out of range"));
            }
            Err(format!("measurement names unknown node '{name}'"))
        };
        let a = resolve(nets.next().unwrap_or(""))?;
        match nets.next() {
            Some(b) => Ok(MeasPart::NodeDiff(a, resolve(b)?)),
            None => Ok(MeasPart::Node(a)),
        }
    }

    /// Evaluate at one sample: substitute the bound probe values, inject
    /// `time`, and run the parameter engine. `env` comes from
    /// `Netlist::param_env()` — complete, so user `.func` calls work.
    fn eval(
        &self,
        expr: &str,
        env: &ParamEnv,
        sample: &MeasSample,
    ) -> std::result::Result<f64, String> {
        let read_node = |id: usize| -> std::result::Result<f64, String> {
            sample
                .nodes
                .get(id)
                .copied()
                .ok_or_else(|| format!("node '{id}' has no sample value"))
        };
        let mut out = String::with_capacity(128);
        for part in &self.parts {
            match part {
                MeasPart::Text(t) => out.push_str(t),
                MeasPart::Node(a) => {
                    out.push_str(&format!("({})", read_node(*a)?));
                }
                MeasPart::NodeDiff(a, b) => {
                    out.push_str(&format!("({})", read_node(*a)? - read_node(*b)?));
                }
                MeasPart::Current(family, index) => {
                    let value = sample
                        .currents
                        .slice(*family)
                        .and_then(|s| s.get(*index).copied())
                        .ok_or_else(|| {
                            format!("measurement names no {} for I(…)", family.label())
                        })?;
                    out.push_str(&format!("({value})"));
                }
            }
        }
        let mut env = env.clone();
        env.params
            .insert("time".to_string(), ParamValue::Num(sample.x));
        env.eval_num(&out).map_err(|e| format!("expression '{expr}': {e}"))
    }
}

fn indices_in_window(samples: &[MeasSample], from: Option<f64>, to: Option<f64>) -> Vec<usize> {
    samples
        .iter()
        .enumerate()
        .filter(|(_, s)| from.is_none_or(|f| s.x >= f) && to.is_none_or(|t| s.x <= t))
        .map(|(i, _)| i)
        .collect()
}

/// Linear interpolation of a sampled trace at `x`.
fn interpolate_at(xs: &[f64], ys: &[f64], x: f64) -> std::result::Result<f64, String> {
    if xs.len() != ys.len() || xs.is_empty() {
        return Err("no samples to interpolate".into());
    }
    if x <= xs[0] {
        return Ok(ys[0]);
    }
    for i in 1..xs.len() {
        if x <= xs[i] {
            let span = xs[i] - xs[i - 1];
            if span <= 0.0 {
                return Ok(ys[i]);
            }
            let frac = (x - xs[i - 1]) / span;
            return Ok(ys[i - 1] + (ys[i] - ys[i - 1]) * frac);
        }
    }
    Ok(*ys.last().unwrap())
}

/// Crossing abscissas of `ys` through `target`.
fn crossing_points(
    xs: &[f64],
    ys: &[f64],
    target: f64,
    edge: Option<(MeasEdge, usize)>,
) -> Vec<f64> {
    let kind = edge.map(|(e, _)| e).unwrap_or(MeasEdge::Cross);
    let mut out = Vec::new();
    for i in 0..xs.len().saturating_sub(1) {
        let f0 = ys[i] - target;
        let f1 = ys[i + 1] - target;
        let hit = match kind {
            MeasEdge::Rise => f0 <= 0.0 && f1 > 0.0,
            MeasEdge::Fall => f0 >= 0.0 && f1 < 0.0,
            MeasEdge::Cross => (f0 < 0.0 && f1 > 0.0) || (f0 > 0.0 && f1 < 0.0),
        };
        if hit {
            let span = xs[i + 1] - xs[i];
            let frac = if f0 == f1 {
                0.0
            } else {
                f0 / (f0 - f1)
            };
            out.push(xs[i] + span * frac);
        }
    }
    out
}

fn pick_occurrence(
    mut crossings: Vec<f64>,
    edge: Option<(MeasEdge, usize)>,
    what: &str,
) -> std::result::Result<f64, String> {
    crossings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let wanted = edge.map(|(_, n)| n).unwrap_or(1);
    crossings
        .get(wanted.saturating_sub(1))
        .copied()
        .ok_or_else(|| format!("no {what} crossing number {wanted} in the measurement window"))
}

fn scalar_stat(
    func: MeasFunc,
    xs: &[f64],
    ys: &[f64],
) -> std::result::Result<f64, String> {
    if xs.is_empty() {
        return Err("no samples in the measurement window".into());
    }
    let duration = xs.last().copied().unwrap_or(0.0) - xs[0];
    let integral = |power: u32| -> f64 {
        let mut sum = 0.0;
        for i in 1..xs.len() {
            let y0 = ys[i - 1].powi(power as i32);
            let y1 = ys[i].powi(power as i32);
            sum += 0.5 * (y0 + y1) * (xs[i] - xs[i - 1]);
        }
        sum
    };
    let finite: Vec<(f64, f64)> = xs
        .iter()
        .copied()
        .zip(ys.iter().copied())
        .filter(|(_, y)| y.is_finite())
        .collect();
    if finite.is_empty() {
        return Err("every sample in the measurement window is non-finite".into());
    }
    match func {
        MeasFunc::Avg if duration > 0.0 => Ok(integral(1) / duration),
        MeasFunc::Rms if duration > 0.0 => Ok((integral(2) / duration).sqrt()),
        MeasFunc::Avg | MeasFunc::Rms => Ok(*ys.first().unwrap_or(&0.0)),
        MeasFunc::Max => Ok(finite.iter().map(|(_, y)| *y).fold(f64::NEG_INFINITY, f64::max)),
        MeasFunc::Min => Ok(finite.iter().map(|(_, y)| *y).fold(f64::INFINITY, f64::min)),
        MeasFunc::Pp => Ok(finite.iter().map(|(_, y)| *y).fold(f64::NEG_INFINITY, f64::max)
            - finite.iter().map(|(_, y)| *y).fold(f64::INFINITY, f64::min)),
        MeasFunc::MaxAt => Ok(finite
            .iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(x, _)| *x)
            .unwrap_or(xs[0])),
        MeasFunc::MinAt => Ok(finite
            .iter()
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(x, _)| *x)
            .unwrap_or(xs[0])),
    }
}

fn evaluate_measurement(
    netlist: &Netlist,
    measurement: &Measurement,
    dc: Option<&DcResult>,
    dc_sweep: Option<&DcSweepResult>,
    tran: Option<&TranResult>,
    ac: Option<&AcResult>,
) -> std::result::Result<f64, String> {
    let samples = measurement_samples(measurement.analysis, dc, dc_sweep, tran, ac)?;
    let circuit = &netlist.circuit;
    // The deck's complete parameter environment: values, `.func` tables and
    // the Monte-Carlo seed, compiled once per measurement.
    let params = netlist.param_env();
    match &measurement.kind {
        MeasKind::Scalar { func, expr, from, to } => {
            let program = MeasProgram::compile(expr, circuit)?;
            let idx = indices_in_window(&samples, *from, *to);
            let xs: Vec<f64> = idx.iter().map(|&i| samples[i].x).collect();
            let ys: Vec<f64> = idx
                .iter()
                .map(|&i| program.eval(expr, &params, &samples[i]))
                .collect::<std::result::Result<_, _>>()?;
            scalar_stat(*func, &xs, &ys)
        }
        MeasKind::FindWhen { expr, when, when_value, edge, from, to } => {
            let program = MeasProgram::compile(expr, circuit)?;
            let when_program = MeasProgram::compile(when, circuit)?;
            let idx = indices_in_window(&samples, *from, *to);
            let xs: Vec<f64> = idx.iter().map(|&i| samples[i].x).collect();
            let when_ys: Vec<f64> = idx
                .iter()
                .map(|&i| when_program.eval(when, &params, &samples[i]))
                .collect::<std::result::Result<_, _>>()?;
            let expr_ys: Vec<f64> = idx
                .iter()
                .map(|&i| program.eval(expr, &params, &samples[i]))
                .collect::<std::result::Result<_, _>>()?;
            let crossings = crossing_points(&xs, &when_ys, *when_value, *edge);
            let x = pick_occurrence(crossings, *edge, "WHEN")?;
            interpolate_at(&xs, &expr_ys, x)
        }
        MeasKind::Cross { expr, value, edge, from, to } => {
            let program = MeasProgram::compile(expr, circuit)?;
            let idx = indices_in_window(&samples, *from, *to);
            let xs: Vec<f64> = idx.iter().map(|&i| samples[i].x).collect();
            let ys: Vec<f64> = idx
                .iter()
                .map(|&i| program.eval(expr, &params, &samples[i]))
                .collect::<std::result::Result<_, _>>()?;
            let crossings = crossing_points(&xs, &ys, *value, *edge);
            pick_occurrence(crossings, *edge, "CROSS")
        }
        MeasKind::TrigTarg {
            trig_expr,
            trig_value,
            trig_edge,
            targ_expr,
            targ_value,
            targ_edge,
            from,
            to,
        } => {
            let trig_program = MeasProgram::compile(trig_expr, circuit)?;
            let targ_program = MeasProgram::compile(targ_expr, circuit)?;
            let idx = indices_in_window(&samples, *from, *to);
            let xs: Vec<f64> = idx.iter().map(|&i| samples[i].x).collect();
            let trig_ys: Vec<f64> = idx
                .iter()
                .map(|&i| trig_program.eval(trig_expr, &params, &samples[i]))
                .collect::<std::result::Result<_, _>>()?;
            let targ_ys: Vec<f64> = idx
                .iter()
                .map(|&i| targ_program.eval(targ_expr, &params, &samples[i]))
                .collect::<std::result::Result<_, _>>()?;
            let trig_crossings = crossing_points(&xs, &trig_ys, *trig_value, *trig_edge);
            let targ_crossings = crossing_points(&xs, &targ_ys, *targ_value, *targ_edge);
            let trig_x = pick_occurrence(trig_crossings, *trig_edge, "TRIG")?;
            let after: Vec<f64> = targ_crossings.into_iter().filter(|x| *x >= trig_x).collect();
            let targ_x = pick_occurrence(after, *targ_edge, "TARG")?;
            Ok(targ_x - trig_x)
        }
    }
}

fn evaluate_measurements(
    netlist: &Netlist,
    dc: Option<&DcResult>,
    dc_sweep: Option<&DcSweepResult>,
    tran: Option<&TranResult>,
    ac: Option<&AcResult>,
) -> Vec<MeasureResult> {
    netlist
        .analysis
        .measurements
        .iter()
        .map(|m| match evaluate_measurement(netlist, m, dc, dc_sweep, tran, ac) {
            Ok(value) => MeasureResult { name: m.name.clone(), value: Some(value), error: None },
            Err(error) => MeasureResult { name: m.name.clone(), value: None, error: Some(error) },
        })
        .collect()
}

// ─── .four — Fourier decomposition of a transient waveform ──────────────────

/// Evaluate `.four`: the last `1/fundamental` seconds of the transient are
/// decomposed into a DC term plus harmonics 1..=9 (the reference tools print
/// nine), using trapezoidal integration on the sampled trace.
fn evaluate_fourier(
    netlist: &Netlist,
    cmd: &crate::netlist::FourCommand,
    tran: Option<&TranResult>,
) -> Result<FourResult> {
    let tran = tran.ok_or_else(|| {
        SimError::Analysis(".four needs a .tran analysis to decompose".into())
    })?;
    let samples = measurement_samples(MeasAnalysis::Tran, None, None, Some(tran), None)
        .map_err(SimError::Analysis)?;
    let Some(last) = samples.last() else {
        return Err(SimError::Analysis(".four has no transient samples".into()));
    };
    let t_end = last.x;
    let t_start = t_end - 1.0 / cmd.frequency;
    if samples.first().is_some_and(|s| s.x > t_start) {
        return Err(SimError::Analysis(format!(
            ".four needs at least one full period of {} Hz ({} s) of transient data",
            cmd.frequency,
            1.0 / cmd.frequency
        )));
    }
    let window: Vec<&MeasSample> =
        samples.iter().filter(|s| s.x >= t_start - 1e-15).collect();
    if window.len() < 2 {
        return Err(SimError::Analysis(
            ".four has fewer than two samples in the analysed period".into(),
        ));
    }
    let period = t_end - t_start;
    let omega = 2.0 * std::f64::consts::PI * cmd.frequency;

    let params = netlist.param_env();
    let mut outputs = Vec::with_capacity(cmd.outputs.len());
    for spec in &cmd.outputs {
        let program =
            MeasProgram::compile(spec, &netlist.circuit).map_err(SimError::Analysis)?;
        let mut xs = Vec::with_capacity(window.len());
        let mut ys = Vec::with_capacity(window.len());
        for sample in &window {
            xs.push(sample.x);
            ys.push(
                program
                    .eval(spec, &params, sample)
                    .map_err(SimError::Analysis)?,
            );
        }
        let mut dc_sum = 0.0;
        for i in 1..xs.len() {
            dc_sum += 0.5 * (ys[i] + ys[i - 1]) * (xs[i] - xs[i - 1]);
        }
        let dc = dc_sum / period;
        let mut harm = Vec::new();
        let mut fundamental = (0.0, 0.0);
        for n in 1..=9usize {
            let mut a = 0.0;
            let mut b = 0.0;
            for i in 1..xs.len() {
                let tau_i = xs[i] - t_start;
                let tau_p = xs[i - 1] - t_start;
                let ci = (n as f64 * omega * tau_i).cos();
                let cp = (n as f64 * omega * tau_p).cos();
                let si = (n as f64 * omega * tau_i).sin();
                let sp = (n as f64 * omega * tau_p).sin();
                let dx = xs[i] - xs[i - 1];
                a += (2.0 / period) * 0.5 * (ys[i] * ci + ys[i - 1] * cp) * dx;
                b += (2.0 / period) * 0.5 * (ys[i] * si + ys[i - 1] * sp) * dx;
            }
            let magnitude = a.hypot(b);
            let phase = b.atan2(a).to_degrees();
            if n == 1 {
                fundamental = (magnitude, phase);
            }
            harm.push((n, magnitude, phase));
        }
        let (fund_mag, fund_phase) = fundamental;
        let harmonics = harm
            .into_iter()
            .map(|(number, magnitude, phase_deg)| {
                let mut norm_phase = phase_deg - fund_phase;
                while norm_phase > 180.0 {
                    norm_phase -= 360.0;
                }
                while norm_phase < -180.0 {
                    norm_phase += 360.0;
                }
                Harmonic {
                    number,
                    frequency: number as f64 * cmd.frequency,
                    magnitude,
                    phase_deg,
                    normalized_magnitude: if fund_mag > 0.0 { magnitude / fund_mag } else { 0.0 },
                    normalized_phase_deg: norm_phase,
                }
            })
            .collect();
        outputs.push(FourOutput { name: spec.clone(), dc, harmonics });
    }
    Ok(FourResult { fundamental: cmd.frequency, outputs })
}

// ─── .noise — small-signal noise sweep ───────────────────────────────────────

/// Resolve the two nodes of a voltage output operand.
fn tf_output_nodes(circuit: &Circuit, output: &TfOutput) -> Result<(usize, usize)> {
    match output {
        TfOutput::Voltage { node, reference } => {
            let p = tf_node(circuit, node)?;
            let n = reference.as_deref().map(|r| tf_node(circuit, r)).transpose()?.unwrap_or(0);
            Ok((p, n))
        }
        TfOutput::Current(name) => Err(SimError::Analysis(format!(
            ".noise output I({name}) is not supported — use a node voltage"
        ))),
    }
}

/// One frequency's output-referred noise power spectral density (V²/Hz).
///
/// The adjoint method solves `Aᵀ y = e_out` once; `y[a] − y[b]` is then the
/// transfer from a current injected between nodes `a,b` to the output, so
/// every device's current-noise PSD contributes `|H|²·S`.
fn noise_psd_at(
    circuit: &Circuit,
    bias: &nalgebra::DVector<f64>,
    freq: f64,
    out_p: usize,
    out_n: usize,
) -> Result<(f64, f64)> {
    let mut sys = build_system::<Complex64>(circuit, Mode::Ac { freq, bias })?;
    // Adjoint excitation: unit current into the output port (solve Aᵀx = z).
    sys.clear_rhs();
    sys.drive_node(crate::circuit::NodeId(out_p), Complex64::new(1.0, 0.0));
    sys.drive_node(crate::circuit::NodeId(out_n), Complex64::new(-1.0, 0.0));
    let adjoint = sys.solve_transposed()?;

    let transfer = |a: crate::circuit::NodeId, b: crate::circuit::NodeId| -> Complex64 {
        node_voltage(&adjoint, a) - node_voltage(&adjoint, b)
    };

    // `circuit.temperature` is in °C; the 4kT thermal terms and the device
    // noise currents below must read the *same* temperature, so both go
    // through the deck's `.temp` value (0 °C = 273.15 K).
    let kt = BOLTZMANN * (circuit.temperature + ZERO_CELSIUS_K);
    let mut psd = 0.0f64;

    for r in &circuit.resistors {
        let h = transfer(r.n1, r.n2).norm_sqr();
        psd += h * 4.0 * kt / r.resistance;
    }

    for d in &circuit.diodes {
        let model = device_model(&circuit.diode_models, "diode", &d.name, &d.model)?;
        let v = node_voltage(bias, d.anode) - node_voltage(bias, d.cathode);
        let current = model.eval_for_at(v, d.area, circuit.temperature).i_d.abs();
        let h = transfer(d.anode, d.cathode).norm_sqr();
        let shot = 2.0 * ELECTRON_CHARGE * current;
        let flicker = if model.kf > 0.0 && freq > 0.0 {
            model.kf * current.powf(model.af) / freq
        } else {
            0.0
        };
        psd += h * (shot + flicker);
    }

    for m in &circuit.mosfets {
        let model = device_model(&circuit.mosfet_models, "MOSFET", &m.name, &m.model)?;
        let sign = if model.kind == MosfetType::Pmos { -1.0 } else { 1.0 };
        let vd = sign * node_voltage(bias, m.nd);
        let vg = sign * node_voltage(bias, m.ng);
        let vs = sign * node_voltage(bias, m.ns);
        let vb = sign * node_voltage(bias, m.nb);
        let beta = model.kp * (m.w / m.l);
        let gm = model.eval_at(beta, vg - vs, vd - vs, vb - vs, circuit.temperature).gm;
        let h = transfer(m.nd, m.ns).norm_sqr();
        // Long-channel channel thermal noise (the 2/3 factor).
        psd += h * 4.0 * kt * (2.0 / 3.0) * gm.max(0.0);
    }

    Ok((psd, 0.0))
}

pub fn run_noise_analysis(
    circuit: &Circuit,
    cmd: &NoiseCommand,
    bias: &nalgebra::DVector<f64>,
) -> Result<NoiseResult> {
    if cmd.f_start <= 0.0 || cmd.f_stop < cmd.f_start {
        return Err(SimError::Analysis(
            ".noise needs 0 < fstart <= fstop".into(),
        ));
    }
    let (out_p, out_n) = tf_output_nodes(circuit, &cmd.output)?;
    if out_p == 0 && out_n == 0 {
        return Err(SimError::Analysis(
            ".noise output V(0) is identically zero — name a real node".into(),
        ));
    }
    let input_idx = circuit
        .voltage_sources
        .iter()
        .position(|v| v.name.eq_ignore_ascii_case(&cmd.input))
        .ok_or_else(|| {
            SimError::Analysis(format!(
                ".noise input '{}' is not an independent voltage source",
                cmd.input
            ))
        })?;

    let freqs = ac_frequencies(&AcCommand {
        scale: cmd.scale,
        points: cmd.points,
        f_start: cmd.f_start,
        f_stop: cmd.f_stop,
        stimulus_source: None,
    });

    let mut points = Vec::with_capacity(freqs.len());
    let mut output_psds = Vec::with_capacity(freqs.len());
    let mut input_psds = Vec::with_capacity(freqs.len());
    for &freq in &freqs {
        let (output_psd, _) = noise_psd_at(circuit, bias, freq, out_p, out_n)?;

        // Gain from the input source to the output: drive 1 V and read the
        // output phasor.
        let mut gain_sys = build_system::<Complex64>(circuit, Mode::Ac { freq, bias })?;
        gain_sys.clear_rhs();
        gain_sys.drive_source(input_idx, Complex64::new(1.0, 0.0));
        let gx = gain_sys.solve()?;
        let gain = node_voltage(&gx, crate::circuit::NodeId(out_p))
            - node_voltage(&gx, crate::circuit::NodeId(out_n));
        let gain2 = gain.norm_sqr();
        let input_psd = if gain2 > 0.0 { output_psd / gain2 } else { f64::NAN };

        output_psds.push(output_psd);
        input_psds.push(input_psd);
        points.push(NoisePoint {
            freq,
            output_density: output_psd.sqrt(),
            input_density: input_psd.sqrt(),
        });
    }

    let integrate = |psds: &[f64]| -> f64 {
        let mut sum = 0.0;
        for i in 1..psds.len() {
            sum += 0.5 * (psds[i] + psds[i - 1]) * (freqs[i] - freqs[i - 1]);
        }
        sum
    };

    let output = match &cmd.output {
        TfOutput::Voltage { node, reference } => match reference {
            Some(reference) => format!("V({node},{reference})"),
            None => format!("V({node})"),
        },
        TfOutput::Current(name) => format!("I({name})"),
    };

    Ok(NoiseResult {
        output,
        input: cmd.input.clone(),
        points,
        total_output_rms: integrate(&output_psds).max(0.0).sqrt(),
        total_input_rms: integrate(&input_psds).max(0.0).sqrt(),
    })
}
