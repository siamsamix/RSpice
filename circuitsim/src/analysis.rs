use crate::circuit::Circuit;
use crate::error::{Result, SimError};
use crate::mna::{
    build_dc, build_transient, node_voltage, update_transient_state, TransientState,
};
use crate::netlist::AnalysisCommands;

#[derive(Debug, Clone)]
pub struct DcResult {
    pub node_voltages: Vec<f64>,
    pub source_currents: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct TranPoint {
    pub time: f64,
    pub node_voltages: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct TranResult {
    pub points: Vec<TranPoint>,
}

#[derive(Debug, Clone)]
pub struct SimulationResult {
    pub dc: Option<DcResult>,
    pub tran: Option<TranResult>,
}

pub fn run(circuit: &Circuit, analysis: &AnalysisCommands) -> Result<SimulationResult> {
    let mut result = SimulationResult {
        dc: None,
        tran: None,
    };

    let run_dc = analysis.dc_op
    || (analysis.tran_step.is_none() && analysis.tran_stop.is_none());
    let run_tran = analysis.tran_step.is_some() && analysis.tran_stop.is_some();

    if !run_dc && !run_tran {
        return Err(SimError::Analysis(
            "no analysis specified; add .op or .tran".into(),
        ));
    }

    if run_dc {
        result.dc = Some(run_dc_op(circuit)?);
    }
    if run_tran {
        let step = analysis.tran_step.unwrap();
        let stop = analysis.tran_stop.unwrap();
        result.tran = Some(run_transient(
            circuit,
            step,
            analysis.tran_start,
            stop,
        )?);
    }

    Ok(result)
}

pub fn run_dc_op(circuit: &Circuit) -> Result<DcResult> {
    let sys = build_dc(circuit)?;
    let x = sys.solve()?;
    Ok(extract_dc(circuit, &x))
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
    DcResult {
        node_voltages,
        source_currents,
    }
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

    let dc_sys = build_dc(circuit)?;
    let dc_x = dc_sys.solve()?;

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
    });

    let mut t = t_start + dt;
    let mut step = 1usize;
    const MAX_STEPS: usize = 10_000_000;

    while t <= t_stop + 0.5 * dt {
        if step > MAX_STEPS {
            return Err(SimError::Analysis("transient exceeded maximum steps".into()));
        }

        let sys = build_transient(circuit, dt, t, &state)?;
        let x = sys.solve()?;
        update_transient_state(circuit, &x, &mut state);

        let mut node_voltages = vec![0.0; circuit.nodes];
        for i in 1..circuit.nodes {
            node_voltages[i] = node_voltage(&x, crate::circuit::NodeId(i));
        }
        points.push(TranPoint { time: t, node_voltages });

        t += dt;
        step += 1;
    }

    Ok(TranResult { points })
}
