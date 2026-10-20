use crate::circuit::Circuit;
use crate::error::{Result, SimError};
use crate::mna::{
    build_system, compute_ac_branch_currents, compute_branch_currents, node_voltage,
    update_transient_state, AcBranchCurrents, BranchCurrents, BranchLayout, MnaSystem, Mode,
    TransientState,
};
use crate::netlist::AnalysisCommands;
use num_complex::Complex64;

#[derive(Debug, Clone)]
pub struct DcResult {
    pub node_voltages:   Vec<f64>,
    pub source_currents: Vec<f64>,
    /// Branch current through every circuit element (n1→n2 convention).
    pub branch_currents: BranchCurrents,
}

#[derive(Debug, Clone)]
pub struct TranPoint {
    pub time:            f64,
    pub node_voltages:   Vec<f64>,
    /// Branch current through every circuit element at this time point.
    pub branch_currents: BranchCurrents,
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
    pub branch_currents: AcBranchCurrents,
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
    pub tran: Option<TranResult>,
    pub ac: Option<AcResult>,
}

/// Newton-Raphson convergence tolerance (max |Δx| between iterates).
const NR_TOLERANCE: f64 = 1e-6;
/// Newton-Raphson iteration budget per solve.
const NR_MAX_ITERS: usize = 100;

/// One Newton-Raphson solve, shared by `.op` and every transient step:
/// stamp the system at the current iterate, solve, repeat until the iterate
/// moves less than [`NR_TOLERANCE`].
///
/// `build` receives the current iterate and MAINTAINS all other mode data
/// (bias point, dt, transient state) by closure; the returned system owns its
/// matrix, so no lifetimes leak through the seam.
fn newton_raphson(
    mut build: impl FnMut(&nalgebra::DVector<f64>) -> Result<MnaSystem<f64>>,
    seed: &nalgebra::DVector<f64>,
    failure: &str,
) -> Result<nalgebra::DVector<f64>> {
    let mut x = seed.clone();
    for _ in 0..NR_MAX_ITERS {
        let sys = build(&x)?;
        let x_next = sys.solve()?;
        let diff = (&x_next - &x).amax();
        x = x_next;
        if diff < NR_TOLERANCE {
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

pub fn run(circuit: &Circuit, analysis: &AnalysisCommands) -> Result<SimulationResult> {
    circuit.validate()?;

    let mut result = SimulationResult {
        dc: None,
        tran: None,
        ac: None,
    };

    let run_dc = analysis.dc_op
    || (analysis.tran_step.is_none() && analysis.tran_stop.is_none() && analysis.ac.is_none());
    let run_tran = analysis.tran_step.is_some() && analysis.tran_stop.is_some();
    let run_ac = analysis.ac.is_some();

    if !run_dc && !run_tran && !run_ac {
        return Err(SimError::Analysis(
            "no analysis specified; add .op, .tran, or .ac".into(),
        ));
    }

    // Solve the DC operating point once. It is always needed:
    // as the primary result for .op, as the initial state for .tran,
    // and as the linearisation point for .ac.
    let (dc_result, dc_x) = solve_dc_op(circuit)?;

    if run_dc {
        result.dc = Some(dc_result);
    }
    if run_tran {
        let step = analysis.tran_step.unwrap();
        let stop = analysis.tran_stop.unwrap();
        result.tran = Some(run_transient(circuit, &dc_x, step, analysis.tran_start, stop)?);
    }
    if run_ac {
        let ac_cmd = analysis.ac.as_ref().unwrap();
        result.ac = Some(run_ac_sweep(circuit, ac_cmd, &dc_x)?);
    }

    Ok(result)
}

/// Solve the DC operating point and return both the summary struct and the
/// raw solution vector. The raw vector is needed by AC analysis (linearisation)
/// and can seed transient initial conditions.
fn solve_dc_op(circuit: &Circuit) -> Result<(DcResult, nalgebra::DVector<f64>)> {
    let seed = crate::mna::initial_guess(circuit);
    let x = newton_raphson(
        |guess| build_system::<f64>(circuit, Mode::Dc { guess }),
        &seed,
        "DC operating point failed to converge",
    )?;
    let result = extract_dc(circuit, &x)?;
    Ok((result, x))
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
    let branch_currents = compute_branch_currents(circuit, x, None, None)?;
    Ok(DcResult { node_voltages, source_currents, branch_currents })
}

/// Run the backward-Euler transient sweep, seeded from the DC operating-point
/// solution `dc_x` that `run` has already computed (so the t=0 point and the
/// inductor starting currents reflect the true DC bias, not zero).
fn run_transient(
    circuit: &Circuit,
    dc_x: &nalgebra::DVector<f64>,
    dt: f64,
    t_start: f64,
    t_stop: f64,
) -> Result<TranResult> {
    if t_stop < t_start {
        return Err(SimError::Analysis("tstop must be >= tstart".into()));
    }

    let mut state = TransientState::default();
    update_transient_state(circuit, dc_x, &mut state);

    let mut points = Vec::new();

    points.push(TranPoint {
        time: t_start,
        node_voltages: extract_voltages(circuit, dc_x),
        // DC steady-state: capacitor current is zero.
        branch_currents: compute_branch_currents(circuit, dc_x, None, None)?,
    });

    let mut t = t_start + dt;
    let mut step = 1usize;
    const MAX_STEPS: usize = 10_000_000;

    while t <= t_stop + 0.5 * dt {
        if step > MAX_STEPS {
            return Err(SimError::Analysis("transient exceeded maximum steps".into()));
        }

        let x = newton_raphson(
            |guess| build_system::<f64>(circuit, Mode::Tran { dt, t, state: &state, guess }),
            dc_x,
            &format!("Transient failed to converge at t={}", t),
        )?;

        // Capture previous cap voltages before updating state, needed for I_C.
        let prev_cap_voltages = state.capacitor_voltages.clone();
        update_transient_state(circuit, &x, &mut state);

        let branch_currents = compute_branch_currents(
            circuit, &x, Some(dt), Some(&prev_cap_voltages),
        )?;

        points.push(TranPoint {
            time: t,
            node_voltages: extract_voltages(circuit, &x),
            branch_currents,
        });

        t += dt;
        step += 1;
    }

    Ok(TranResult { points })
}

// ─── AC frequency sweep ───────────────────────────────────────────────────────

/// Supported sweep scale (mirrors SPICE .ac syntax).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AcScale {
    /// Logarithmically spaced points per decade.
    Dec,
    /// Logarithmically spaced points per octave.
    Oct,
    /// Linearly spaced points.
    Lin,
}

/// Parameters extracted from a `.ac` command.
#[derive(Debug, Clone)]
pub struct AcCommand {
    pub scale: AcScale,
    /// Points per decade/octave, or total linear points.
    pub points: usize,
    /// Start frequency (Hz).
    pub f_start: f64,
    /// Stop frequency (Hz).
    pub f_stop: f64,
    /// Index of the voltage source to use as the AC stimulus (0-based into
    /// `circuit.voltage_sources`).  `None` = drive the first source.
    pub stimulus_source: Option<usize>,
}

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
    let stim_branch = BranchLayout::for_circuit(circuit).src_branch(stim_idx);
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
        sys.z[stim_branch] = Complex64::new(stim_amp, 0.0);

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
