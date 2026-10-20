//! MNA — one stamping module for DC, transient, and AC analysis.
//!
//! The unknown vector layout is owned by [`BranchLayout`]; a system is
//! [`MnaSystem<T>`] over a field: `f64` for `.op` / `.tran`,
//! [`Complex64`] for `.ac`. The single `build_system` constructor covers all
//! three analyses through a [`Mode`]; the AC stimulus patch (which source to
//! drive, what amplitude) stays with the analysis driver.

use nalgebra::{DMatrix, DVector};
use num_complex::Complex64;

use crate::circuit::{Circuit, MosfetType, NodeId, VoltageSource};
use crate::error::{Result, SimError};

/// Field arithmetic for one MNA system: [`f64`] for DC/transient,
/// [`Complex64`] for the AC sweep. One stamping implementation serves both.
///
/// The interface is [`nalgebra::Scalar`] + [`nalgebra::ComplexField`] (the LU
/// solve is uniform under those bounds) plus two hooks: `lift` (real-axis
/// lift) and `imag` (j·v).
pub trait Field: nalgebra::Scalar + nalgebra::ComplexField + Copy {
    /// Lift `v` from the real axis into the field: `v` for `f64`, `v + 0j` for
    /// [`Complex64`]. (Named `lift`, not `from_real`/`from_f64`, because
    /// simba's `Complex` trait generates methods under those names with an
    /// associated real-field parameter that a generic `T` can't call.)
    fn lift(v: f64) -> Self;

    /// The imaginary axis `j·v`. The `f64` impl returns `0.0` — physically
    /// meaningless, but never exercised: the simulator builds `Mode::Ac`
    /// exclusively over `Complex64`.
    fn imag(v: f64) -> Self;
}

impl Field for f64 {
    fn lift(v: f64) -> Self {
        v
    }

    fn imag(_v: f64) -> Self {
        0.0
    }
}

impl Field for Complex64 {
    fn lift(v: f64) -> Self {
        Complex64::new(v, 0.0)
    }

    fn imag(v: f64) -> Self {
        Complex64::new(0.0, v)
    }
}

/// Transient state carried between backward-Euler steps: capacitor voltages
/// and inductor currents from the previously converged solution.
#[derive(Debug, Clone, Default)]
pub struct TransientState {
    pub capacitor_voltages: Vec<f64>,
    pub inductor_currents: Vec<f64>,
}

// ─── Branch-row layout ──────────────────────────────────────────────────────

/// The layout of the MNA unknown vector: node-voltage rows first, then
/// voltage-source branch rows, then inductor branch rows.
///
/// This knowledge used to be implicit, re-derived by index arithmetic at eight
/// sites across three modules; `BranchLayout` is its one owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchLayout {
    /// Number of node rows (ground excluded — a NodeId of 0 has no row).
    pub n_nodes: usize,
    /// First row of the voltage-source branch block.
    pub vsrc_base: usize,
    /// First row of the inductor branch block.
    pub ind_base: usize,
}

impl BranchLayout {
    pub fn for_circuit(circuit: &Circuit) -> Self {
        let n_nodes = circuit.nodes.saturating_sub(1);
        Self {
            n_nodes,
            vsrc_base: n_nodes,
            ind_base: n_nodes + circuit.voltage_sources.len(),
        }
    }

    /// Row index of the current unknown of voltage source `i`.
    pub fn src_branch(&self, i: usize) -> usize {
        self.vsrc_base + i
    }

    /// Row index of the current unknown of inductor `i`.
    pub fn ind_branch(&self, i: usize) -> usize {
        self.ind_base + i
    }

    /// Value of the voltage-source `i` row in a solved unknown vector.
    /// Signed as the MNA branch unknown; element-convention negation stays
    /// with the caller.
    pub fn src_row<T>(&self, solution: &DVector<T>, i: usize) -> T
    where
        T: Copy,
    {
        solution[self.src_branch(i)]
    }

    /// Value of the inductor `i` row in a solved unknown vector.
    pub fn ind_row<T>(&self, solution: &DVector<T>, i: usize) -> T
    where
        T: Copy,
    {
        solution[self.ind_branch(i)]
    }
}

// ─── System ─────────────────────────────────────────────────────────────────

pub struct MnaSystem<T: Field> {
    pub size: usize,
    pub a: DMatrix<T>,
    pub z: DVector<T>,
    /// Where the branch rows live in `a`/`z`.
    pub branch: BranchLayout,
}

impl<T: Field> MnaSystem<T> {
    pub fn new(size: usize) -> Self {
        Self {
            size,
            a: DMatrix::zeros(size, size),
            z: DVector::zeros(size),
            branch: BranchLayout { n_nodes: 0, vsrc_base: 0, ind_base: 0 },
        }
    }

    /// A fresh system sized for `circuit`, with its branch layout set.
    pub fn for_circuit(circuit: &Circuit) -> Self {
        let mut sys = Self::new(circuit.tran_matrix_size());
        sys.branch = BranchLayout::for_circuit(circuit);
        sys
    }

    /// Solve A·x = z using LU decomposition.
    pub fn solve(&self) -> Result<DVector<T>> {
        self.a
            .clone()
            .lu()
            .solve(&self.z)
            .ok_or_else(|| SimError::Algebra("singular MNA matrix".into()))
    }
}

// ─── What to build ──────────────────────────────────────────────────────────

/// Which analysis the system is for, and the data that arm needs.
#[derive(Debug)]
pub enum Mode<'a> {
    /// DC operating point: nonlinear devices linearised at `guess`,
    /// capacitors open, inductors as short branches.
    Dc { guess: &'a DVector<f64> },
    /// One backward-Euler transient step at time `t` with step `dt`.
    Tran { dt: f64, t: f64, state: &'a TransientState, guess: &'a DVector<f64> },
    /// Small-signal AC point at `freq`, linearised at the DC bias point.
    /// All independent sources become short circuits; the analysis driver
    /// patches the stimulus source afterwards.
    Ac { freq: f64, bias: &'a DVector<f64> },
}

/// Build a system for `circuit` in the given `mode`.
///
/// DC and transient build over `f64`; the AC sweep builds over
/// [`Complex64`] (no Newton-Raphson runs in AC — everything there is
/// already linearised at the bias point).
pub fn build_system<T: Field>(circuit: &Circuit, mode: Mode) -> Result<MnaSystem<T>> {
    let mut sys = MnaSystem::<T>::for_circuit(circuit);

    stamp_resistors(circuit, &mut sys);

    match mode {
        Mode::Dc { guess } => {
            stamp_diodes(circuit, &mut sys, guess, true);
            stamp_mosfets(circuit, &mut sys, guess, true);

            // Inductors are shorts at DC — stamp as 0 V voltage sources.
            for (i, l) in circuit.inductors.iter().enumerate() {
                let branch = sys.branch.ind_branch(i);
                if let Some(i1) = node_index(l.n1) {
                    sys.a[(i1, branch)] += T::lift(1.0);
                    sys.a[(branch, i1)] += T::lift(1.0);
                }
                if let Some(i2) = node_index(l.n2) {
                    sys.a[(i2, branch)] -= T::lift(1.0);
                    sys.a[(branch, i2)] -= T::lift(1.0);
                }
            }

            stamp_voltage_sources(circuit, &mut sys, |v| v.voltage);
        }
        Mode::Tran { dt, t, state, guess } => {
            if dt <= 0.0 {
                return Err(SimError::Analysis("time step must be positive".into()));
            }
            stamp_diodes(circuit, &mut sys, guess, true);
            stamp_mosfets(circuit, &mut sys, guess, true);
            stamp_capacitors(circuit, &mut sys, dt, state);
            stamp_inductors(circuit, &mut sys, dt, state);
            stamp_voltage_sources(circuit, &mut sys, |v| v.value_at(t));
        }
        Mode::Ac { freq, bias } => {
            let omega = 2.0 * std::f64::consts::PI * freq;

            // Y_C = jωC
            for c in &circuit.capacitors {
                let y = T::imag(omega * c.capacitance);
                stamp_admittance(&mut sys, c.n1, c.n2, y);
            }

            // Inductors (as branch unknowns, Z_L = jωL):
            //   V_L = Z_L · I_L  →  -Z_L · I_L + V_{n1} - V_{n2} = 0
            // z[branch] = 0 (no independent source in the inductor branch).
            for (i, l) in circuit.inductors.iter().enumerate() {
                let branch = sys.branch.ind_branch(i);
                let z_l = T::imag(omega * l.inductance);
                if let Some(i1) = node_index(l.n1) {
                    sys.a[(i1, branch)] += T::lift(1.0);
                    sys.a[(branch, i1)] += T::lift(1.0);
                }
                if let Some(i2) = node_index(l.n2) {
                    sys.a[(i2, branch)] -= T::lift(1.0);
                    sys.a[(branch, i2)] -= T::lift(1.0);
                }
                sys.a[(branch, branch)] -= z_l;
            }

            // Small-signal stamps at the DC bias: linearisation only, no
            // Newton-Raphson companion currents.
            stamp_diodes(circuit, &mut sys, bias, false);
            stamp_mosfets(circuit, &mut sys, bias, false);

            // Independent DC sources become short circuits (0 V); the caller
            // drives the stimulus source by writing sys.z[src_branch(s)].
            stamp_voltage_sources(circuit, &mut sys, |_| 0.0);
        }
    }

    Ok(sys)
}

// ─── Element stamps ─────────────────────────────────────────────────────────

fn stamp_resistors<T: Field>(circuit: &Circuit, sys: &mut MnaSystem<T>) {
    for r in &circuit.resistors {
        let g = T::lift(1.0 / r.resistance);
        stamp_admittance(sys, r.n1, r.n2, g);
    }
}

/// `with_companion`: DC/transient stamping linearises for Newton-Raphson and
/// therefore also stamps the equivalent current source `i_eq`; the AC
/// small-signal stamp is pure linearisation about the bias and skips it —
/// exactly as the pre-consolidation paths did.
fn stamp_diodes<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    guess: &DVector<f64>,
    with_companion: bool,
) {
    for d in &circuit.diodes {
        let model = circuit.diode_models.get(&d.model).expect("Diode model not found");
        let v_anode = node_voltage(guess, d.anode);
        let v_cathode = node_voltage(guess, d.cathode);
        // Linearization, clamp, and companion current all come from the model —
        // the physics lives in one place (model.rs).
        let ev = model.eval(v_anode - v_cathode);

        stamp_admittance(sys, d.anode, d.cathode, T::lift(ev.gd));
        if with_companion {
            stamp_current_source(sys, d.anode, d.cathode, T::lift(ev.i_eq));
        }
    }
}

/// Stamp a single MOSFET using the level-1 Shichman-Hodges model.
///
/// The Newton-Raphson companion model consists of:
///   - A drain-source conductance  `gds`
///   - A transconductance current  `gm * Vgs`  (modelled as a controlled source)
///   - A body-effect corrected threshold `Vth`
///   - A DC equivalent current    `Ids_eq`     (skipped in AC — see
///     [`stamp_diodes`] for the `with_companion` rationale)
///
/// For a PMOS all terminal voltages are negated before applying the NMOS
/// equations, and the resulting current is negated back.
fn stamp_mosfets<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    guess: &DVector<f64>,
    with_companion: bool,
) {
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

        // β = KP·(W/L). The DC/transient stamp linearizes at Vds ≥ 0 — the
        // clamp belongs to this stamp's companion-model convention, not to the
        // model itself.
        let beta = model.kp * (m.w / m.l);
        let ev = model.eval(beta, vg - vs, (vd - vs).max(0.0), vb - vs);

        // ── Linearised companion model (Newton-Raphson stamp) ─────────────
        //
        // The terminal current into drain: Ids_lin = gds*Vds + gm*Vgs + gbs*Vbs + Ieq,
        // where all coefficients and Ieq come from model.rs at the same
        // linearization point.
        //
        // We apply sign to account for PMOS polarity.
        let s = sign; // +1 for NMOS, -1 for PMOS
        let gm = ev.gm;
        let gds = ev.gds;
        let gbs = ev.gbs;

        // Helper: determine actual node pairs, honouring PMOS polarity.
        // For PMOS the stamped sense of current is reversed.
        let (drain, source, gate, bulk) = (m.nd, m.ns, m.ng, m.nb);

        // -- gds: drain-source conductance (stamp between nd and ns) --
        stamp_admittance(sys, drain, source, T::lift(gds));

        // -- gm: transconductance controlled by Vgs --
        //   Current gm*Vgs flows from drain to source.
        //   In MNA: stamp as voltage-controlled current source.
        //   Contribution to G matrix: sign convention
        //     +gm at (nd, ng), -gm at (nd, ns), -gm at (ns, ng), +gm at (ns, ns)
        if let Some(id_idx) = node_index(drain) {
            if let Some(ig_idx) = node_index(gate) {
                sys.a[(id_idx, ig_idx)] += T::lift(s * gm);
            }
            if let Some(is_idx) = node_index(source) {
                sys.a[(id_idx, is_idx)] -= T::lift(s * gm);
            }
        }
        if let Some(is_idx) = node_index(source) {
            if let Some(ig_idx) = node_index(gate) {
                sys.a[(is_idx, ig_idx)] -= T::lift(s * gm);
            }
            if let Some(is2_idx) = node_index(source) {
                sys.a[(is_idx, is2_idx)] += T::lift(s * gm);
            }
        }

        // -- gbs: bulk transconductance controlled by Vbs --
        //   Current gbs*Vbs flows from drain to source.
        if let Some(id_idx) = node_index(drain) {
            if let Some(ib_idx) = node_index(bulk) {
                sys.a[(id_idx, ib_idx)] += T::lift(s * gbs);
            }
            if let Some(is_idx) = node_index(source) {
                sys.a[(id_idx, is_idx)] -= T::lift(s * gbs);
            }
        }
        if let Some(is_idx) = node_index(source) {
            if let Some(ib_idx) = node_index(bulk) {
                sys.a[(is_idx, ib_idx)] -= T::lift(s * gbs);
            }
            if let Some(is2_idx) = node_index(source) {
                sys.a[(is_idx, is2_idx)] += T::lift(s * gbs);
            }
        }

        // -- Ieq: DC equivalent current injected at drain / extracted at source --
        if with_companion {
            stamp_current_source(sys, drain, source, T::lift(s * ev.i_eq));
        }
    }
}

/// Backward-Euler capacitors: companion conductance `C/dt` plus the current
/// source carrying the previous step's voltage.
fn stamp_capacitors<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    dt: f64,
    state: &TransientState,
) {
    for (i, c) in circuit.capacitors.iter().enumerate() {
        let g_eq = c.capacitance / dt;
        let v_prev = state.capacitor_voltages.get(i).copied().unwrap_or(0.0);
        let i_eq = g_eq * v_prev;
        stamp_admittance(sys, c.n1, c.n2, T::lift(g_eq));
        stamp_current_source(sys, c.n2, c.n1, T::lift(i_eq));
    }
}

/// Backward-Euler inductors as branch unknowns with companion `L/dt`.
fn stamp_inductors<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    dt: f64,
    state: &TransientState,
) {
    for (i, l) in circuit.inductors.iter().enumerate() {
        let r_eq = l.inductance / dt;
        let i_prev = state.inductor_currents.get(i).copied().unwrap_or(0.0);
        let branch = sys.branch.ind_branch(i);

        if let Some(i1) = node_index(l.n1) {
            sys.a[(i1, branch)] += T::lift(1.0);
            sys.a[(branch, i1)] += T::lift(1.0);
        }
        if let Some(i2) = node_index(l.n2) {
            sys.a[(i2, branch)] -= T::lift(1.0);
            sys.a[(branch, i2)] -= T::lift(1.0);
        }
        sys.a[(branch, branch)] -= T::lift(r_eq);
        sys.z[branch] = T::lift(-r_eq * i_prev);
    }
}

fn stamp_voltage_sources<T, F>(circuit: &Circuit, sys: &mut MnaSystem<T>, value_at: F)
where
    T: Field,
    F: Fn(&VoltageSource) -> f64,
{
    for (i, v) in circuit.voltage_sources.iter().enumerate() {
        let branch = sys.branch.src_branch(i);
        if let Some(i1) = node_index(v.n1) {
            sys.a[(i1, branch)] += T::lift(1.0);
            sys.a[(branch, i1)] += T::lift(1.0);
        }
        if let Some(i2) = node_index(v.n2) {
            sys.a[(i2, branch)] -= T::lift(1.0);
            sys.a[(branch, i2)] -= T::lift(1.0);
        }
        sys.z[branch] = T::lift(value_at(v));
    }
}

fn stamp_admittance<T: Field>(sys: &mut MnaSystem<T>, n1: NodeId, n2: NodeId, y: T) {
    if let Some(i) = node_index(n1) {
        sys.a[(i, i)] += y;
    }
    if let Some(j) = node_index(n2) {
        sys.a[(j, j)] += y;
    }
    if let (Some(i), Some(j)) = (node_index(n1), node_index(n2)) {
        sys.a[(i, j)] -= y;
        sys.a[(j, i)] -= y;
    }
}

fn stamp_current_source<T: Field>(sys: &mut MnaSystem<T>, n1: NodeId, n2: NodeId, current: T) {
    if let Some(i) = node_index(n1) {
        sys.z[i] -= current;
    }
    if let Some(j) = node_index(n2) {
        sys.z[j] += current;
    }
}

/// Return the index into the MNA unknown vector for a node.
/// Ground (node 0) has no row — its voltage is identically zero.
fn node_index(node: NodeId) -> Option<usize> {
    if node.0 == 0 { None } else { Some(node.0 - 1) }
}

pub fn node_voltage<T: Field>(solution: &DVector<T>, node: NodeId) -> T {
    node_index(node).map(|i| solution[i]).unwrap_or_else(|| T::lift(0.0))
}

// ─── Branch currents (DC / transient, real) ─────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct BranchCurrents {
    pub resistors: Vec<f64>,
    pub capacitors: Vec<f64>,
    pub inductors: Vec<f64>,
    pub voltage_sources: Vec<f64>,
    pub diodes: Vec<f64>,
    /// Drain current (from drain to source) for each MOSFET.
    pub mosfets: Vec<f64>,
}

pub fn compute_branch_currents(
    circuit: &Circuit,
    solution: &DVector<f64>,
    dt: Option<f64>,
    prev_cap_voltages: Option<&[f64]>,
) -> BranchCurrents {
    let branch = BranchLayout::for_circuit(circuit);

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
                let v_now = v1 - v2;
                let v_prev = prev.get(i).copied().unwrap_or(0.0);
                c.capacitance * (v_now - v_prev) / dt_val
            }
            _ => 0.0,
        }
    }).collect();

    let inductors = circuit.inductors.iter().enumerate().map(|(i, _)| {
        branch.ind_row(solution, i)
    }).collect();

    let voltage_sources = circuit.voltage_sources.iter().enumerate().map(|(i, _)| {
        -branch.src_row(solution, i)
    }).collect();

    let diodes = circuit.diodes.iter().map(|d| {
        let model = circuit.diode_models.get(&d.model)
            .expect("diode model not found in compute_branch_currents");
        let v_a = node_voltage(solution, d.anode);
        let v_c = node_voltage(solution, d.cathode);
        model.eval(v_a - v_c).i_d
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
        // Raw Vds here: reports the physical model current, as before.
        sign * model.eval(beta, vg - vs, vd - vs, vb - vs).ids
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

    let branch = BranchLayout::for_circuit(circuit);
    state.inductor_currents = circuit
        .inductors
        .iter()
        .enumerate()
        .map(|(i, _)| branch.ind_row(solution, i))
        .collect();
}

// ─── Branch currents (AC, complex) ──────────────────────────────────────────

/// Small-signal AC branch currents for one frequency point.
///
/// Positive convention: phasor current flows from n1 to n2 through the element.
#[derive(Debug, Clone, Default)]
pub struct AcBranchCurrents {
    pub resistors: Vec<Complex64>,
    pub capacitors: Vec<Complex64>,
    pub inductors: Vec<Complex64>,
    pub voltage_sources: Vec<Complex64>,
    pub diodes: Vec<Complex64>,
    /// Small-signal drain current phasor for each MOSFET: Id = gm·Vgs + gds·Vds + gbs·Vbs
    pub mosfets: Vec<Complex64>,
}

/// Compute small-signal AC branch currents from a solved AC system.
///
/// * `solution`    — solved complex MNA unknown vector at this frequency.
/// * `freq`        — frequency in Hz (used to compute ωC for capacitors).
/// * `dc_solution` — DC operating-point vector (linearises the devices).
///
/// Positive convention: phasor current flows n1 → n2 through the element.
pub fn compute_ac_branch_currents(
    circuit: &Circuit,
    solution: &DVector<Complex64>,
    freq: f64,
    dc_solution: &DVector<f64>,
) -> AcBranchCurrents {
    let omega = 2.0 * std::f64::consts::PI * freq;
    let branch = BranchLayout::for_circuit(circuit);
    let one = Complex64::new(1.0, 0.0);

    // ── Resistors: I = (V_n1 - V_n2) / R ────────────────────────────────
    let resistors = circuit.resistors.iter().map(|r| {
        let v1 = node_voltage(solution, r.n1);
        let v2 = node_voltage(solution, r.n2);
        (v1 - v2) * (one / r.resistance)
    }).collect();

    // ── Capacitors: I = jωC * (V_n1 - V_n2) ─────────────────────────────
    let capacitors = circuit.capacitors.iter().map(|c| {
        let v1 = node_voltage(solution, c.n1);
        let v2 = node_voltage(solution, c.n2);
        Complex64::new(0.0, omega * c.capacitance) * (v1 - v2)
    }).collect();

    // ── Inductors: branch unknown in solution vector ──────────────────────
    let inductors = circuit.inductors.iter().enumerate().map(|(i, _)| {
        branch.ind_row(solution, i)
    }).collect();

    // ── Voltage sources: branch unknown in solution vector ────────────────
    // Negated to match n1→n2 passive convention (DC path note).
    let voltage_sources = circuit.voltage_sources.iter().enumerate().map(|(i, _)| {
        -branch.src_row(solution, i)
    }).collect();

    // ── Diodes: linearised small-signal conductance gd * (V_a - V_c) ─────
    let diodes = circuit.diodes.iter().map(|d| {
        let model = circuit.diode_models.get(&d.model)
            .expect("diode model not found in compute_ac_branch_currents");
        let gd = model
            .eval(node_voltage(dc_solution, d.anode) - node_voltage(dc_solution, d.cathode))
            .gd;
        let va_ac = node_voltage(solution, d.anode);
        let vc_ac = node_voltage(solution, d.cathode);
        Complex64::new(gd, 0.0) * (va_ac - vc_ac)
    }).collect();

    // ── MOSFETs: small-signal drain current phasor ────────────────────────
    //
    // Id = gm·Vgs + gds·Vds + gbs·Vbs   (all conductances real at level-1)
    //
    // Conductances come from model.rs's single copy of the level-1 equations,
    // linearised at the DC bias point, so this stays self-contained.
    let mosfets = circuit.mosfets.iter().map(|m| {
        let model = circuit.mosfet_models.get(&m.model)
            .expect("MOSFET model not found in compute_ac_branch_currents");

        let sign = if model.kind == MosfetType::Pmos { -1.0 } else { 1.0 };

        // DC bias voltages (real) — mirroring as in build_system's AC arm.
        let vd_dc = sign * node_voltage(dc_solution, m.nd);
        let vg_dc = sign * node_voltage(dc_solution, m.ng);
        let vs_dc = sign * node_voltage(dc_solution, m.ns);
        let vb_dc = sign * node_voltage(dc_solution, m.nb);

        let beta = model.kp * (m.w / m.l);
        let ev = model.eval(beta, vg_dc - vs_dc, vd_dc - vs_dc, vb_dc - vs_dc);
        let gm = ev.gm;
        let gds = ev.gds;
        let gbs = ev.gbs;

        // AC (small-signal) terminal voltages.
        let vd_ac = node_voltage(solution, m.nd);
        let vg_ac = node_voltage(solution, m.ng);
        let vs_ac = node_voltage(solution, m.ns);
        let vb_ac = node_voltage(solution, m.nb);

        let vgs_ac = vg_ac - vs_ac;
        let vds_ac = vd_ac - vs_ac;
        let vbs_ac = vb_ac - vs_ac;

        // Id = sign·(gm·Vgs + gds·Vds + gbs·Vbs).
        Complex64::new(sign, 0.0)
            * (Complex64::new(gm, 0.0) * vgs_ac
                + Complex64::new(gds, 0.0) * vds_ac
                + Complex64::new(gbs, 0.0) * vbs_ac)
    }).collect();

    AcBranchCurrents { resistors, capacitors, inductors, voltage_sources, diodes, mosfets }
}

// ─── Public helpers used by analysis::seed_initial_guess ─────────────────────

/// Public wrapper around the private `node_index` — maps NodeId → MNA row index.
#[inline]
pub fn node_index_pub(node: NodeId) -> Option<usize> {
    node_index(node)
}

/// Public wrapper around the private `stamp_admittance`.
#[inline]
pub fn stamp_conductance_pub<T: Field>(sys: &mut MnaSystem<T>, n1: NodeId, n2: NodeId, g: f64) {
    stamp_admittance(sys, n1, n2, T::lift(g));
}
