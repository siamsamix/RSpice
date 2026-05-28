use nalgebra::{DMatrix, DVector};

use crate::circuit::{Circuit, MosfetType, NodeId};
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
    if node.0 == 0 { None } else { Some(node.0 - 1) }
}

pub fn build_dc(circuit: &Circuit, guess: &DVector<f64>) -> Result<MnaSystem> {
    let size = circuit.tran_matrix_size();
    let mut sys = MnaSystem::new(size);
    let n_nodes = circuit.nodes.saturating_sub(1);

    stamp_resistors(circuit, &mut sys);
    stamp_diodes(circuit, &mut sys, guess);
    stamp_mosfets(circuit, &mut sys, guess);

    // Inductors are shorts at DC — stamp as 0 V voltage sources.
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
    }

    stamp_voltage_sources(circuit, &mut sys, n_nodes, None);
    Ok(sys)
}

pub fn build_transient(
    circuit: &Circuit,
    dt: f64,
    t: f64,
    state: &TransientState,
    guess: &DVector<f64>,
) -> Result<MnaSystem> {
    if dt <= 0.0 {
        return Err(SimError::Analysis("time step must be positive".into()));
    }
    let size = circuit.tran_matrix_size();
    let mut sys = MnaSystem::new(size);
    let n_nodes = circuit.nodes.saturating_sub(1);

    stamp_resistors(circuit, &mut sys);
    stamp_diodes(circuit, &mut sys, guess);
    stamp_mosfets(circuit, &mut sys, guess);
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

fn stamp_diodes(circuit: &Circuit, sys: &mut MnaSystem, guess: &DVector<f64>) {
    for d in &circuit.diodes {
        let model = circuit.diode_models.get(&d.model).expect("Diode model not found");
        let v_anode   = node_voltage(guess, d.anode);
        let v_cathode = node_voltage(guess, d.cathode);
        let vd = v_anode - v_cathode;

        let vt = 0.02585;
        let vd_clamped = vd.clamp(-100.0, 0.8);
        let vt_n = model.n * vt;
        let exp_term = (vd_clamped / vt_n).exp();

        let id  = model.is * (exp_term - 1.0);
        let gd  = (model.is / vt_n) * exp_term;
        let ieq = id - gd * vd_clamped;

        stamp_conductance(sys, d.anode, d.cathode, gd);
        stamp_current_source(sys, d.anode, d.cathode, ieq);
    }
}

/// Stamp a single MOSFET using the level-1 Shichman-Hodges model.
///
/// The Newton-Raphson companion model consists of:
///   - A drain-source conductance  `gds`
///   - A transconductance current  `gm * Vgs`  (modelled as a controlled source)
///   - A body-effect corrected threshold `Vth`
///   - A DC equivalent current    `Ids_eq`
///
/// For a PMOS all terminal voltages are negated before applying the NMOS
/// equations, and the resulting current is negated back.
fn stamp_mosfets(circuit: &Circuit, sys: &mut MnaSystem, guess: &DVector<f64>) {
    for m in &circuit.mosfets {
        let model = circuit.mosfet_models.get(&m.model).expect("MOSFET model not found");

        // Raw node voltages from the current Newton-Raphson guess.
        let vd_raw = node_voltage(guess, m.nd);
        let vg_raw = node_voltage(guess, m.ng);
        let vs_raw = node_voltage(guess, m.ns);
        let vb_raw = node_voltage(guess, m.nb);

        // For PMOS, mirror all voltages so the standard NMOS equations apply.
        let sign = if model.kind == MosfetType::Pmos { -1.0 } else { 1.0 };
        let vd = sign * vd_raw;
        let vg = sign * vg_raw;
        let vs = sign * vs_raw;
        let vb = sign * vb_raw;

        // Effective width-over-length ratio times KP gives β (A/V²).
        let beta = model.kp * (m.w / m.l);

        // Body-effect: Vth = Vth0 + γ*(sqrt(|2φF - Vbs|) - sqrt(|2φF|))
        let vsb = vs - vb;
        let phi = model.phi;
        let vth = model.vth0
            + model.gamma * ((phi + vsb).abs().sqrt() - phi.abs().sqrt());

        let vgs = vg - vs;
        let vds = (vd - vs).max(0.0); // clamp: negative Vds → treat as Vds=0 (boundary of cut-off)
        let vov = vgs - vth; // overdrive voltage

        // Determine operating region and compute drain current + linearisation.
        let (ids, gm, gds, gbs) = if vov <= 0.0 {
            // ── Cut-off: device is OFF ────────────────────────────────────
            // Use a tiny leakage conductance for numerical stability.
            let g_leak = 1e-12;
            (0.0, g_leak, g_leak, 0.0)
        } else {
            let vds_sat = vov; // Vds at saturation boundary

            if vds < vds_sat {
                // ── Linear (triode) region ───────────────────────────────
                // Ids = β * [(Vgs - Vth)*Vds - Vds²/2] * (1 + λ·Vds)
                let ids_lin = beta * ((vov * vds) - 0.5 * vds * vds)
                    * (1.0 + model.lambda * vds);
                let gm_lin  = beta * vds * (1.0 + model.lambda * vds);
                let gds_lin = beta * (vov - vds)
                    * (1.0 + model.lambda * vds)
                    + beta * ((vov * vds) - 0.5 * vds * vds) * model.lambda;
                // Body effect: d(Ids)/d(Vbs) ≈ -d(Ids)/d(Vth) * d(Vth)/d(Vbs)
                let dvth_dvsb = model.gamma / (2.0 * (phi + vsb).abs().sqrt().max(1e-9));
                let gbs_lin = gm_lin * dvth_dvsb;
                (ids_lin, gm_lin.max(0.0), gds_lin.max(1e-12), gbs_lin)
            } else {
                // ── Saturation region ────────────────────────────────────
                // Ids = (β/2) * (Vgs - Vth)² * (1 + λ·Vds)
                let ids_sat = 0.5 * beta * vov * vov * (1.0 + model.lambda * vds);
                let gm_sat  = beta * vov * (1.0 + model.lambda * vds);
                let gds_sat = 0.5 * beta * vov * vov * model.lambda;
                let dvth_dvsb = model.gamma / (2.0 * (phi + vsb).abs().sqrt().max(1e-9));
                let gbs_sat = gm_sat * dvth_dvsb;
                (ids_sat, gm_sat.max(0.0), gds_sat.max(1e-12), gbs_sat)
            }
        };

        // ── Linearised companion model (Newton-Raphson stamp) ─────────────
        //
        // The terminal current into drain: Ids_lin = gds*Vds + gm*Vgs + gbs*Vbs + Ieq
        //
        // where: Ieq = Ids - gds*Vds_op - gm*Vgs_op - gbs*Vbs_op
        //
        // We apply sign to account for PMOS polarity.

        let vgs_op = vgs;
        let vds_op = vds;
        let vbs_op = vb - vs;
        let ids_eq = ids - gds * vds_op - gm * vgs_op - gbs * vbs_op;

        // Helper: determine actual node pairs, honouring PMOS polarity.
        // For PMOS the stamped sense of current is reversed.
        let (drain, source, gate, bulk) = (m.nd, m.ns, m.ng, m.nb);

        // -- gds: drain-source conductance (stamp between nd and ns) --
        stamp_conductance(sys, drain, source, gds);

        // -- gm: transconductance controlled by Vgs --
        //   Current gm*Vgs flows from drain to source.
        //   In MNA: stamp as voltage-controlled current source.
        //   Contribution to G matrix: sign convention
        //     +gm at (nd, ng), -gm at (nd, ns), -gm at (ns, ng), +gm at (ns, ns)
        let s = sign; // +1 for NMOS, -1 for PMOS
        if let Some(id_idx) = node_index(drain) {
            if let Some(ig_idx) = node_index(gate) {
                sys.a[(id_idx, ig_idx)] += s * gm;
            }
            if let Some(is_idx) = node_index(source) {
                sys.a[(id_idx, is_idx)] -= s * gm;
            }
        }
        if let Some(is_idx) = node_index(source) {
            if let Some(ig_idx) = node_index(gate) {
                sys.a[(is_idx, ig_idx)] -= s * gm;
            }
            if let Some(is2_idx) = node_index(source) {
                sys.a[(is_idx, is2_idx)] += s * gm;
            }
        }

        // -- gbs: bulk transconductance controlled by Vbs --
        //   Current gbs*Vbs flows from drain to source.
        if let Some(id_idx) = node_index(drain) {
            if let Some(ib_idx) = node_index(bulk) {
                sys.a[(id_idx, ib_idx)] += s * gbs;
            }
            if let Some(is_idx) = node_index(source) {
                sys.a[(id_idx, is_idx)] -= s * gbs;
            }
        }
        if let Some(is_idx) = node_index(source) {
            if let Some(ib_idx) = node_index(bulk) {
                sys.a[(is_idx, ib_idx)] -= s * gbs;
            }
            if let Some(is2_idx) = node_index(source) {
                sys.a[(is_idx, is2_idx)] += s * gbs;
            }
        }

        // -- Ieq: DC equivalent current injected at drain / extracted at source --
        stamp_current_source(sys, drain, source, s * ids_eq);
    }
}

fn stamp_capacitors(circuit: &Circuit, sys: &mut MnaSystem, dt: f64, state: &TransientState) {
    for (i, c) in circuit.capacitors.iter().enumerate() {
        let g_eq   = c.capacitance / dt;
        let v_prev = state.capacitor_voltages.get(i).copied().unwrap_or(0.0);
        let i_eq   = g_eq * v_prev;
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
        let r_eq   = l.inductance / dt;
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

fn stamp_voltage_sources(
    circuit: &Circuit,
    sys: &mut MnaSystem,
    n_nodes: usize,
    t: Option<f64>,
) {
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
    node_index(node).map(|i| solution[i]).unwrap_or(0.0)
}

// ─── Branch currents ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct BranchCurrents {
    pub resistors:       Vec<f64>,
    pub capacitors:      Vec<f64>,
    pub inductors:       Vec<f64>,
    pub voltage_sources: Vec<f64>,
    pub diodes:          Vec<f64>,
    /// Drain current (from drain to source) for each MOSFET.
    pub mosfets:         Vec<f64>,
}

pub fn compute_branch_currents(
    circuit: &Circuit,
    solution: &DVector<f64>,
    dt: Option<f64>,
    prev_cap_voltages: Option<&[f64]>,
) -> BranchCurrents {
    let n_nodes = circuit.nodes.saturating_sub(1);

    let resistors = circuit.resistors.iter().map(|r| {
        let v1 = node_voltage(solution, r.n1);
        let v2 = node_voltage(solution, r.n2);
        (v1 - v2) / r.resistance
    }).collect();

    let capacitors = circuit.capacitors.iter().enumerate().map(|(i, c)| {
        match (dt, prev_cap_voltages) {
            (Some(dt_val), Some(prev)) => {
                let v1 = node_voltage(solution, c.n1);
                let v2 = node_voltage(solution, c.n2);
                let v_now  = v1 - v2;
                let v_prev = prev.get(i).copied().unwrap_or(0.0);
                c.capacitance * (v_now - v_prev) / dt_val
            }
            _ => 0.0,
        }
    }).collect();

    let inductor_branch_base = n_nodes + circuit.voltage_sources.len();
    let inductors = circuit.inductors.iter().enumerate().map(|(i, _)| {
        solution[inductor_branch_base + i]
    }).collect();

    let voltage_sources = circuit.voltage_sources.iter().enumerate().map(|(i, _)| {
        -solution[n_nodes + i]
    }).collect();

    let diodes = circuit.diodes.iter().map(|d| {
        let model = circuit.diode_models.get(&d.model)
            .expect("diode model not found in compute_branch_currents");
        let v_a = node_voltage(solution, d.anode);
        let v_c = node_voltage(solution, d.cathode);
        let vd = (v_a - v_c).clamp(-100.0, 0.8);
        let vt_n = model.n * 0.02585;
        model.is * ((vd / vt_n).exp() - 1.0)
    }).collect();

    // MOSFET: re-evaluate Ids at the converged solution.
    let mosfets = circuit.mosfets.iter().map(|m| {
        let model = circuit.mosfet_models.get(&m.model)
            .expect("MOSFET model not found in compute_branch_currents");
        let sign = if model.kind == MosfetType::Pmos { -1.0 } else { 1.0 };
        let vd = sign * node_voltage(solution, m.nd);
        let vg = sign * node_voltage(solution, m.ng);
        let vs = sign * node_voltage(solution, m.ns);
        let vb = sign * node_voltage(solution, m.nb);

        let beta = model.kp * (m.w / m.l);
        let vsb  = vs - vb;
        let vth  = model.vth0
            + model.gamma * ((model.phi + vsb).abs().sqrt() - model.phi.abs().sqrt());
        let vgs  = vg - vs;
        let vds  = vd - vs;
        let vov  = vgs - vth;

        let ids = if vov <= 0.0 {
            0.0
        } else if vds < vov {
            beta * ((vov * vds) - 0.5 * vds * vds) * (1.0 + model.lambda * vds)
        } else {
            0.5 * beta * vov * vov * (1.0 + model.lambda * vds)
        };
        sign * ids
    }).collect();

    BranchCurrents { resistors, capacitors, inductors, voltage_sources, diodes, mosfets }
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

// ─── Public helpers used by analysis::seed_initial_guess ─────────────────────

/// Public wrapper around the private `node_index` — maps NodeId → MNA row index.
#[inline]
pub fn node_index_pub(node: NodeId) -> Option<usize> {
    node_index(node)
}

/// Public wrapper around the private `stamp_conductance`.
#[inline]
pub fn stamp_conductance_pub(sys: &mut MnaSystem, n1: NodeId, n2: NodeId, g: f64) {
    stamp_conductance(sys, n1, n2, g);
}
