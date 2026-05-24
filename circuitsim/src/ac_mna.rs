/// ac_mna.rs — Small-signal AC (frequency-domain) MNA
///
/// At each frequency ω = 2π·f we build a complex-valued system
///
///   A(ω) · X = Z
///
/// where A and Z have entries in ℂ.  The stamp rules are:
///
///   Resistor  R  →  G = 1/R             (real conductance)
///   Capacitor C  →  Y = jωC             (imaginary admittance)
///   Inductor  L  →  stamp as a voltage-source branch with impedance jωL
///   Voltage source →  ideal branch (same as DC, but evaluated at DC value)
///   Diode        →  linearised at DC operating point → gd conductance (real)
///
/// The solution vector X contains:
///   [V₁ … V_{n-1} | I_{vs0} … I_{vs_{k-1}} | I_{L0} … I_{L_{m-1}}]
///
/// which matches the transient matrix layout so index arithmetic is shared.

use nalgebra::{DMatrix, DVector};
use num_complex::Complex64;

use crate::circuit::{Circuit, NodeId};
use crate::error::{Result, SimError};

pub struct AcSystem {
    pub size: usize,
    pub a: DMatrix<Complex64>,
    pub z: DVector<Complex64>,
}

impl AcSystem {
    pub fn new(size: usize) -> Self {
        Self {
            size,
            a: DMatrix::zeros(size, size),
            z: DVector::zeros(size),
        }
    }

    /// Solve A·x = z using LU decomposition.
    pub fn solve(&self) -> Result<DVector<Complex64>> {
        self.a
            .clone()
            .lu()
            .solve(&self.z)
            .ok_or_else(|| SimError::Algebra("singular AC MNA matrix".into()))
    }
}

/// Build the complex MNA matrix for frequency `freq` (Hz).
/// `dc_solution` is the real DC operating-point vector used to linearise diodes.
pub fn build_ac(
    circuit: &Circuit,
    freq: f64,
    dc_solution: &DVector<f64>,
) -> Result<AcSystem> {
    let omega = 2.0 * std::f64::consts::PI * freq;
    let size = circuit.tran_matrix_size();
    let mut sys = AcSystem::new(size);
    let n_nodes = circuit.nodes.saturating_sub(1);

    // ── Resistors ────────────────────────────────────────────────────────────
    for r in &circuit.resistors {
        let g = Complex64::new(1.0 / r.resistance, 0.0);
        stamp_admittance(&mut sys, r.n1, r.n2, g);
    }

    // ── Capacitors ───────────────────────────────────────────────────────────
    for c in &circuit.capacitors {
        // Y_C = jωC
        let y = Complex64::new(0.0, omega * c.capacitance);
        stamp_admittance(&mut sys, c.n1, c.n2, y);
    }

    // ── Inductors (as branch unknowns, Z_L = jωL) ────────────────────────────
    for (i, l) in circuit.inductors.iter().enumerate() {
        let branch = n_nodes + circuit.voltage_sources.len() + i;
        let z_l = Complex64::new(0.0, omega * l.inductance);

        if let Some(i1) = node_index(l.n1) {
            sys.a[(i1, branch)] += Complex64::new(1.0, 0.0);
            sys.a[(branch, i1)] += Complex64::new(1.0, 0.0);
        }
        if let Some(i2) = node_index(l.n2) {
            sys.a[(i2, branch)] -= Complex64::new(1.0, 0.0);
            sys.a[(branch, i2)] -= Complex64::new(1.0, 0.0);
        }
        // V_L = Z_L · I_L  →  -Z_L · I_L + V_{n1} - V_{n2} = 0
        sys.a[(branch, branch)] -= z_l;
        // z[branch] = 0  (no independent source in the inductor branch)
    }

    // ── Diodes (linearised at DC OP) ─────────────────────────────────────────
    for d in &circuit.diodes {
        let model = circuit.diode_models.get(&d.model)
            .expect("diode model not found during AC build");
        let v_a = node_voltage_real(dc_solution, d.anode);
        let v_c = node_voltage_real(dc_solution, d.cathode);
        let vd = (v_a - v_c).clamp(-100.0, 0.8);
        let vt_n = model.n * 0.02585;
        let gd = (model.is / vt_n) * (vd / vt_n).exp();
        stamp_admittance(&mut sys, d.anode, d.cathode, Complex64::new(gd, 0.0));
    }

    // ── Voltage sources ───────────────────────────────────────────────────────
    // In AC analysis, all *independent* DC voltage sources become short circuits
    // (0 V).  Only sources that are explicitly the AC stimulus are driven; here
    // we set all to 0 V so the caller can patch the stimulus source afterwards.
    for (i, v) in circuit.voltage_sources.iter().enumerate() {
        let branch = n_nodes + i;
        if let Some(i1) = node_index(v.n1) {
            sys.a[(i1, branch)] += Complex64::new(1.0, 0.0);
            sys.a[(branch, i1)] += Complex64::new(1.0, 0.0);
        }
        if let Some(i2) = node_index(v.n2) {
            sys.a[(i2, branch)] -= Complex64::new(1.0, 0.0);
            sys.a[(branch, i2)] -= Complex64::new(1.0, 0.0);
        }
        // z[branch] set to 0 by default; the caller drives the stimulus source
        // by writing sys.z[branch] = Complex64::new(amplitude, 0.0).
        // We expose the branch index so analysis.rs can find it.
        let _ = v; // suppress unused warning
    }

    Ok(sys)
}

/// Return the index into the MNA unknown vector for a node.
/// Ground (node 0) has no row — returns None.
#[inline]
fn node_index(node: NodeId) -> Option<usize> {
    if node.0 == 0 { None } else { Some(node.0 - 1) }
}

/// Stamp a two-terminal admittance (works for complex Y).
fn stamp_admittance(sys: &mut AcSystem, n1: NodeId, n2: NodeId, y: Complex64) {
    if let Some(i) = node_index(n1) { sys.a[(i, i)] += y; }
    if let Some(j) = node_index(n2) { sys.a[(j, j)] += y; }
    if let (Some(i), Some(j)) = (node_index(n1), node_index(n2)) {
        sys.a[(i, j)] -= y;
        sys.a[(j, i)] -= y;
    }
}

/// Read a real node voltage from a DC solution vector.
#[inline]
pub fn node_voltage_real(solution: &DVector<f64>, node: NodeId) -> f64 {
    node_index(node).map(|i| solution[i]).unwrap_or(0.0)
}

/// Read a complex node voltage from an AC solution vector.
#[inline]
pub fn node_voltage_complex(solution: &DVector<Complex64>, node: NodeId) -> Complex64 {
    node_index(node).map(|i| solution[i]).unwrap_or(Complex64::new(0.0, 0.0))
}

/// Complex branch currents for every element in the circuit (AC small-signal).
/// Each Vec is parallel to the corresponding Vec in `Circuit`.
/// Positive convention: phasor current flows from n1 to n2 through the element.
#[derive(Debug, Clone, Default)]
pub struct AcBranchCurrents {
    pub resistors:       Vec<Complex64>,
    pub capacitors:      Vec<Complex64>,
    pub inductors:       Vec<Complex64>,
    pub voltage_sources: Vec<Complex64>,
    pub diodes:          Vec<Complex64>,
}

/// Compute small-signal AC branch currents from a solved AC system.
///
/// * `solution`    — solved complex MNA unknown vector at this frequency.
/// * `freq`        — frequency in Hz (used to compute ωC for capacitors).
/// * `dc_solution` — DC operating-point vector (linearises diode conductance).
///
/// Positive convention: phasor current flows n1 → n2 through the element.
pub fn compute_ac_branch_currents(
    circuit: &Circuit,
    solution: &DVector<Complex64>,
    freq: f64,
    dc_solution: &DVector<f64>,
) -> AcBranchCurrents {
    let omega   = 2.0 * std::f64::consts::PI * freq;
    let n_nodes = circuit.nodes.saturating_sub(1);
    let one     = Complex64::new(1.0, 0.0);

    // ── Resistors: I = (V_n1 - V_n2) / R ────────────────────────────────
    let resistors = circuit.resistors.iter().map(|r| {
        let v1 = node_voltage_complex(solution, r.n1);
        let v2 = node_voltage_complex(solution, r.n2);
        (v1 - v2) * (one / r.resistance)
    }).collect();

    // ── Capacitors: I = jωC * (V_n1 - V_n2) ─────────────────────────────
    let capacitors = circuit.capacitors.iter().map(|c| {
        let v1 = node_voltage_complex(solution, c.n1);
        let v2 = node_voltage_complex(solution, c.n2);
        Complex64::new(0.0, omega * c.capacitance) * (v1 - v2)
    }).collect();

    // ── Inductors: branch unknown in solution vector ──────────────────────
    let inductor_branch_base = n_nodes + circuit.voltage_sources.len();
    let inductors = circuit.inductors.iter().enumerate().map(|(i, _)| {
        solution[inductor_branch_base + i]
    }).collect();

    // ── Voltage sources: branch unknown in solution vector ────────────────
    // Negated to match n1→n2 passive convention (see mna.rs note).
    let voltage_sources = circuit.voltage_sources.iter().enumerate().map(|(i, _)| {
        -solution[n_nodes + i]
    }).collect();

    // ── Diodes: linearised small-signal conductance gd * (V_a - V_c) ─────
    let diodes = circuit.diodes.iter().map(|d| {
        let model = circuit.diode_models.get(&d.model)
            .expect("diode model not found in compute_ac_branch_currents");
        let v_a_dc = node_voltage_real(dc_solution, d.anode);
        let v_c_dc = node_voltage_real(dc_solution, d.cathode);
        let vd     = (v_a_dc - v_c_dc).clamp(-100.0, 0.8);
        let vt_n   = model.n * 0.02585;
        let gd     = (model.is / vt_n) * (vd / vt_n).exp();
        let va_ac  = node_voltage_complex(solution, d.anode);
        let vc_ac  = node_voltage_complex(solution, d.cathode);
        Complex64::new(gd, 0.0) * (va_ac - vc_ac)
    }).collect();

    AcBranchCurrents { resistors, capacitors, inductors, voltage_sources, diodes }
}
