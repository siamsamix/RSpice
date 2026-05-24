use crate::ac_mna::{build_ac, compute_ac_branch_currents, node_voltage_complex, AcBranchCurrents};
use crate::circuit::Circuit;
use crate::error::{Result, SimError};
use crate::mna::{
    build_dc, build_transient, compute_branch_currents, node_voltage, update_transient_state,
    BranchCurrents, TransientState,
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

impl AcResult {
    /// Magnitude in volts at `node_id` for the point at `index`.
    pub fn magnitude(&self, point_index: usize, node_id: usize) -> f64 {
        self.points[point_index].node_voltages[node_id].norm()
    }

    /// Phase in degrees at `node_id` for the point at `index`.
    pub fn phase_deg(&self, point_index: usize, node_id: usize) -> f64 {
        self.points[point_index].node_voltages[node_id].arg().to_degrees()
    }

    /// Magnitude in dB (20·log₁₀(|V|)) at `node_id` for the point at `index`.
    pub fn magnitude_db(&self, point_index: usize, node_id: usize) -> f64 {
        let m = self.magnitude(point_index, node_id);
        if m > 0.0 { 20.0 * m.log10() } else { f64::NEG_INFINITY }
    }
}

#[derive(Debug, Clone)]
pub struct SimulationResult {
    pub dc: Option<DcResult>,
    pub tran: Option<TranResult>,
    pub ac: Option<AcResult>,
}

pub fn run(circuit: &Circuit, analysis: &AnalysisCommands) -> Result<SimulationResult> {
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
        result.tran = Some(run_transient(circuit, step, analysis.tran_start, stop)?);
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
pub fn solve_dc_op(circuit: &Circuit) -> Result<(DcResult, nalgebra::DVector<f64>)> {
    let size = circuit.tran_matrix_size();
    let mut x = nalgebra::DVector::zeros(size);
    let tol = 1e-6;
    let mut converged = false;
    for _ in 0..100 {
        let sys = build_dc(circuit, &x)?;
        let x_next = sys.solve()?;
        let diff = (&x_next - &x).amax();
        x = x_next;
        if diff < tol {
            converged = true;
            break;
        }
    }
    if !converged {
        return Err(SimError::Analysis("DC operating point failed to converge".into()));
    }
    let result = extract_dc(circuit, &x);
    Ok((result, x))
}

/// Public convenience wrapper that discards the raw vector.
pub fn run_dc_op(circuit: &Circuit) -> Result<DcResult> {
    solve_dc_op(circuit).map(|(r, _)| r)
}

fn extract_dc(circuit: &Circuit, x: &nalgebra::DVector<f64>) -> DcResult {
    let mut node_voltages = vec![0.0; circuit.nodes];
    for i in 1..circuit.nodes {
        node_voltages[i] = node_voltage(x, crate::circuit::NodeId(i));
    }
    let n_nodes = circuit.nodes.saturating_sub(1);
    let source_currents = circuit
        .voltage_sources
        .iter()
        .enumerate()
        .map(|(i, _)| x[n_nodes + i])
        .collect();
    // DC: no capacitor current (steady state), so pass None for dt/prev.
    let branch_currents = compute_branch_currents(circuit, x, None, None);
    DcResult { node_voltages, source_currents, branch_currents }
}

pub fn run_transient(
    circuit: &Circuit,
    dt: f64,
    t_start: f64,
    t_stop: f64,
) -> Result<TranResult> {
    if t_stop < t_start {
        return Err(SimError::Analysis("tstop must be >= tstart".into()));
    }
    let size = circuit.tran_matrix_size();
    let dc_x = nalgebra::DVector::zeros(size);

    let mut state = TransientState::default();

    // FIX: Because build_dc now solves for inductor branches natively, we can safely
    // extract the exact starting currents directly from the DC solution vector.
    update_transient_state(circuit, &dc_x, &mut state);

    let mut points = Vec::new();

    let mut initial_voltages = vec![0.0; circuit.nodes];
    for i in 1..circuit.nodes {
        initial_voltages[i] = node_voltage(&dc_x, crate::circuit::NodeId(i));
    }
    points.push(TranPoint {
        time: t_start,
        node_voltages: initial_voltages,
        // DC steady-state: capacitor current is zero.
        branch_currents: compute_branch_currents(circuit, &dc_x, None, None),
    });

    let mut t = t_start + dt;
    let mut step = 1usize;
    const MAX_STEPS: usize = 10_000_000;

    while t <= t_stop + 0.5 * dt {
        if step > MAX_STEPS {
            return Err(SimError::Analysis("transient exceeded maximum steps".into()));
        }

        let mut x = dc_x.clone(); // Use the last known state as the initial guess
        let mut converged = false;

        for _ in 0..100 { // Newton-Raphson loop for this specific time step
            let sys = build_transient(circuit, dt, t, &state, &x)?;
            let x_next = sys.solve()?;

            let diff = (&x_next - &x).amax();
            x = x_next;

            if diff < 1e-6 {
                converged = true;
                break;
            }
        }

        if !converged {
            return Err(SimError::Analysis(format!("Transient failed to converge at t={}", t)));
        }

        // Capture previous cap voltages before updating state, needed for I_C.
        let prev_cap_voltages = state.capacitor_voltages.clone();
        update_transient_state(circuit, &x, &mut state);

        let branch_currents = compute_branch_currents(
            circuit, &x, Some(dt), Some(&prev_cap_voltages),
        );

        let mut node_voltages = vec![0.0; circuit.nodes];
        for i in 1..circuit.nodes {
            node_voltages[i] = node_voltage(&x, crate::circuit::NodeId(i));
        }
        points.push(TranPoint { time: t, node_voltages, branch_currents });

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
    /// Peak amplitude of the AC stimulus (volts).  SPICE default = 1 V.
    pub stimulus_amplitude: f64,
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
    let stim_branch = circuit.nodes.saturating_sub(1) + stim_idx;
    let stim_node = circuit.voltage_sources[stim_idx].n1;

    // Step 2 — sweep.
    let freqs = ac_frequencies(cmd);
    let mut points = Vec::with_capacity(freqs.len());

    for &freq in &freqs {
        let mut sys = build_ac(circuit, freq, &dc_x)?;

        // Drive the stimulus source with the AC amplitude.
        // All other sources remain at 0 V (short circuit in small-signal model).
        sys.z[stim_branch] = num_complex::Complex64::new(cmd.stimulus_amplitude, 0.0);

        let x = sys.solve()?;

        let mut node_voltages = vec![Complex64::new(0.0, 0.0); circuit.nodes];
        for i in 1..circuit.nodes {
            node_voltages[i] = node_voltage_complex(&x, crate::circuit::NodeId(i));
        }
        let branch_currents = compute_ac_branch_currents(circuit, &x, freq, dc_x);

        points.push(AcPoint { freq, node_voltages, branch_currents });
    }

    Ok(AcResult {
        points,
       stimulus_node: stim_node.0,
    })
}
