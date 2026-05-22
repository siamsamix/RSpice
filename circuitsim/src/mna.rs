use nalgebra::{DMatrix, DVector};

use crate::circuit::{Circuit, NodeId};
use crate::error::{Result, SimError};

#[derive(Debug, Clone, Default)]
pub struct TransientState {
    pub capacitor_voltages: Vec<f64>,
    pub inductor_currents: Vec<f64>,
}

pub struct MnaSystem {
    pub size: usize,
    pub a: DMatrix<f64>,
    pub z: DVector<f64>,
}

impl MnaSystem {
    pub fn new(size: usize) -> Self {
        Self {
            size,
            a: DMatrix::zeros(size, size),
            z: DVector::zeros(size),
        }
    }

    pub fn clear(&mut self) {
        self.a.fill(0.0);
        self.z.fill(0.0);
    }

    pub fn solve(&self) -> Result<DVector<f64>> {
        let lu = self
        .a
        .clone()
        .lu()
        .solve(&self.z)
        .ok_or_else(|| SimError::Algebra("singular MNA matrix".into()))?;
        Ok(lu)
    }
}

fn node_index(node: NodeId) -> Option<usize> {
    if node.0 == 0 {
        None
    } else {
        Some(node.0 - 1)
    }
}

pub fn build_dc(circuit: &Circuit) -> Result<MnaSystem> {
    // FIX: Use the full transient matrix size so inductors get their own branch current rows
    let size = circuit.tran_matrix_size();
    let mut sys = MnaSystem::new(size);
    let n_nodes = circuit.nodes.saturating_sub(1);

    stamp_resistors(circuit, &mut sys);

    // FIX: Inductors are shorts at DC. We stamp them as 0V voltage sources
    // to cleanly solve for their exact steady-state DC current without precision loss.
    for (i, l) in circuit.inductors.iter().enumerate() {
        let branch = n_nodes + circuit.voltage_sources.len() + i;
        if let Some(i1) = node_index(l.n1) {
            sys.a[(i1, branch)] += 1.0;
            sys.a[(branch, i1)] += 1.0;
        }
        if let Some(i2) = node_index(l.n2) {
            sys.a[(i2, branch)] -= 1.0;
            sys.a[(branch, i2)] -= 1.0;
        }
        // z[branch] remains 0.0 (acting as a 0V short circuit)
    }

    stamp_voltage_sources(circuit, &mut sys, n_nodes, None);
    Ok(sys)
}

pub fn build_transient(
    circuit: &Circuit,
    dt: f64,
    t: f64,
    state: &TransientState,
) -> Result<MnaSystem> {
    if dt <= 0.0 {
        return Err(SimError::Analysis("time step must be positive".into()));
    }
    let size = circuit.tran_matrix_size();
    let mut sys = MnaSystem::new(size);
    let n_nodes = circuit.nodes.saturating_sub(1);

    stamp_resistors(circuit, &mut sys);
    stamp_capacitors(circuit, &mut sys, dt, state);
    stamp_inductors(circuit, &mut sys, dt, state, n_nodes, circuit.voltage_sources.len());
    stamp_voltage_sources(circuit, &mut sys, n_nodes, Some(t));

    Ok(sys)
}

fn stamp_resistors(circuit: &Circuit, sys: &mut MnaSystem) {
    for r in &circuit.resistors {
        let g = 1.0 / r.resistance;
        stamp_conductance(sys, r.n1, r.n2, g);
    }
}

fn stamp_capacitors(circuit: &Circuit, sys: &mut MnaSystem, dt: f64, state: &TransientState) {
    for (i, c) in circuit.capacitors.iter().enumerate() {
        let g_eq = c.capacitance / dt;
        let v_prev = state.capacitor_voltages.get(i).copied().unwrap_or(0.0);
        let i_eq = g_eq * v_prev;
        stamp_conductance(sys, c.n1, c.n2, g_eq);
        stamp_current_source(sys, c.n2, c.n1, i_eq);
    }
}

fn stamp_inductors(
    circuit: &Circuit,
    sys: &mut MnaSystem,
    dt: f64,
    state: &TransientState,
    n_nodes: usize,
    n_vsources: usize,
) {
    for (i, l) in circuit.inductors.iter().enumerate() {
        let r_eq = l.inductance / dt;
        let i_prev = state.inductor_currents.get(i).copied().unwrap_or(0.0);
        let branch = n_nodes + n_vsources + i;

        if let Some(i1) = node_index(l.n1) {
            sys.a[(i1, branch)] += 1.0;
            sys.a[(branch, i1)] += 1.0;
        }
        if let Some(i2) = node_index(l.n2) {
            sys.a[(i2, branch)] -= 1.0;
            sys.a[(branch, i2)] -= 1.0;
        }

        sys.a[(branch, branch)] -= r_eq;
        sys.z[branch] = -r_eq * i_prev;
    }
}

fn stamp_voltage_sources(circuit: &Circuit, sys: &mut MnaSystem, n_nodes: usize, t: Option<f64>) {
    for (i, v) in circuit.voltage_sources.iter().enumerate() {
        let branch = n_nodes + i;
        if let Some(i1) = node_index(v.n1) {
            sys.a[(i1, branch)] += 1.0;
            sys.a[(branch, i1)] += 1.0;
        }
        if let Some(i2) = node_index(v.n2) {
            sys.a[(i2, branch)] -= 1.0;
            sys.a[(branch, i2)] -= 1.0;
        }

        sys.z[branch] = match t {
            Some(time) => v.value_at(time),
            None => v.voltage,
        };
    }
}

fn stamp_conductance(sys: &mut MnaSystem, n1: NodeId, n2: NodeId, g: f64) {
    if let Some(i) = node_index(n1) {
        sys.a[(i, i)] += g;
    }
    if let Some(j) = node_index(n2) {
        sys.a[(j, j)] += g;
    }
    if let (Some(i), Some(j)) = (node_index(n1), node_index(n2)) {
        sys.a[(i, j)] -= g;
        sys.a[(j, i)] -= g;
    }
}

fn stamp_current_source(sys: &mut MnaSystem, n1: NodeId, n2: NodeId, current: f64) {
    if let Some(i) = node_index(n1) {
        sys.z[i] -= current;
    }
    if let Some(j) = node_index(n2) {
        sys.z[j] += current;
    }
}

pub fn node_voltage(solution: &DVector<f64>, node: NodeId) -> f64 {
    node_index(node)
    .map(|i| solution[i])
    .unwrap_or(0.0)
}

pub fn update_transient_state(
    circuit: &Circuit,
    solution: &DVector<f64>,
    state: &mut TransientState,
) {
    state.capacitor_voltages = circuit
    .capacitors
    .iter()
    .map(|c| node_voltage(solution, c.n1) - node_voltage(solution, c.n2))
    .collect();

    let n_nodes = circuit.nodes.saturating_sub(1);
    let branch_base = n_nodes + circuit.voltage_sources.len();
    state.inductor_currents = circuit
    .inductors
    .iter()
    .enumerate()
    .map(|(i, _)| solution[branch_base + i])
    .collect();
}
