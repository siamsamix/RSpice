//! MNA — one stamping module for DC, transient, and AC analysis.
//!
//! The unknown vector layout is owned by [`BranchLayout`]; a system is
//! [`MnaSystem<T>`] over a field: `f64` for `.op` / `.tran`,
//! [`Complex64`] for `.ac`. The single `build_system` constructor covers all
//! three analyses through a [`Mode`]; the AC stimulus patch (which source to
//! drive, what amplitude) stays with the analysis driver.

use nalgebra::{DMatrix, DVector};
use num_complex::Complex64;

use crate::behavior::BehavioralRuntime;
use crate::circuit::{
    BehavioralKind, BjtType, Circuit, ControlVar, CurrentSource, IntegrationMethod, JfetType,
    MosfetType, NodeId, SwitchControl, VoltageSource,
};
use crate::error::{Result, SimError};
use crate::model::thermal_voltage;

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
    pub capacitor_currents: Vec<f64>,
    /// Voltages one accepted step further back (Gear II).
    pub capacitor_voltages_older: Vec<f64>,
    pub inductor_currents: Vec<f64>,
    pub inductor_voltages: Vec<f64>,
    pub inductor_currents_older: Vec<f64>,
    /// Mutual-inductance voltage history (one per `K` card).
    pub mutual_voltages: Vec<f64>,
    /// Diode junction/diffusion-capacitance history.
    pub diode_voltages: Vec<f64>,
    pub diode_charges: Vec<f64>,
    pub diode_charges_older: Vec<f64>,
    pub diode_cap_currents: Vec<f64>,
    /// MOSFET terminal capacitance history: Cgs, Cgd, Cgb, Cdb, Csb.
    pub mosfet_cap_voltages: Vec<[f64; 5]>,
    pub mosfet_cap_voltages_older: Vec<[f64; 5]>,
    pub mosfet_cap_currents: Vec<[f64; 5]>,
    /// BJT junction capacitance history: B-E and B-C.
    pub bjt_cap_voltages: Vec<[f64; 2]>,
    pub bjt_cap_voltages_older: Vec<[f64; 2]>,
    pub bjt_cap_currents: Vec<[f64; 2]>,
    /// JFET gate-junction capacitance history: G-S and G-D.
    pub jfet_cap_voltages: Vec<[f64; 2]>,
    pub jfet_cap_voltages_older: Vec<[f64; 2]>,
    pub jfet_cap_currents: Vec<[f64; 2]>,
    /// Last settled state of every switch (`S`/`W`), for hysteresis.
    pub switch_states: Vec<Option<bool>>,
    /// Step size of the last accepted step (0 before the first step).
    pub last_dt: f64,
    /// Per-source runtime state of the behavioral sources (`B`): `idt`
    /// integrators and `delay` histories, one each, in card order.
    pub behavioral: Vec<BehavioralRuntime>,
}

/// BDF2 coefficients `(a0, a1, a2)` for `v' ≈ a0·v_{n+1} + a1·v_n + a2·v_{n−1}`
/// with the previous step `h_prev` and the current step `h`. Reduces to the
/// familiar `(1.5, −2, 0.5)/h` when the step is constant; adapts exactly when
/// it is not (the step-doubling controller changes steps often).
fn bdf2_coefficients(h_prev: f64, h: f64) -> (f64, f64, f64) {
    if h_prev <= 0.0 {
        // First Gear-II step has no history: fall back to Gear I.
        return (1.0 / h, -1.0 / h, 0.0);
    }
    // Coordinates anchored at t_{n+1}: t_n = −h, t_{n−1} = −(h + h_prev).
    let a0 = (2.0 * h + h_prev) / (h * (h + h_prev));
    let a1 = -(h + h_prev) / (h * h_prev);
    let a2 = h / (h_prev * (h + h_prev));
    (a0, a1, a2)
}

// ─── TODO §3: DC junction limiting ──────────────────────────────────────────

/// The classic SPICE2 pn-junction excursion limiter.
///
/// Entry needs *both* conditions: the iterate bias runs forward past the
/// junction's critical voltage [`critical_voltage`] (the knee where the
/// exponential's slope starts to swallow the Newton step), and the excursion
/// from the previous build's linearization point exceeds two thermal
/// voltages — the swing that makes the iteration sawtooth or evaluate
/// `exp()` at absurd arguments.
///
/// The limited value does not jump back to the old point (that would stall),
/// it hands the model a bias on the knee: from a forward start `v_old > 0`
/// the excursion is compressed logarithmically to
/// `v_old + vt·ln(Δ/vt + 1)`, and from a reversed or zero start the build
/// resumes exactly at `vcrit`.
pub fn pnjlim(v_new: f64, v_old: f64, vt: f64, vcrit: f64) -> f64 {
    if v_new > vcrit && (v_new - v_old).abs() > 2.0 * vt {
        if v_old > 0.0 {
            // A logarithm of a non-positive argument is a NaN; a walk that
            // jumped * backwards * past a forward linearization point takes
            // the reference tools' fallback to the knee (ngspice DEVpnjlim).
            let arg = (v_new - v_old) / vt + 1.0;
            if arg > 0.0 {
                v_old + vt * arg.ln()
            } else {
                vcrit
            }
        } else {
            vcrit
        }
    } else {
        v_new
    }
}

/// The classic SPICE2 critical junction voltage — the off point of the limiting
/// knee: `vcrit = vt·ln(vt / (√2·IS))`, where the exponential's slope equals the
/// linear-solve threshold. A nonpositive saturation current means there is no
/// knee at all; the limiter then never fires (`vcrit = ∞`).
pub fn critical_voltage(vt: f64, is: f64) -> f64 {
    if is <= 0.0 {
        return f64::INFINITY;
    }
    vt * (vt / (std::f64::consts::SQRT_2 * is)).ln()
}

/// Junction-bias memory for one DC Newton build: what each device's pn
/// junctions were linearized at in the previous build (the limiter's input)
/// and what the current build limited them to (its output, feeding the next
/// build). Biases for the BJT and JFET junctions are stored in the models'
/// *normalized* (NPN/NJF) space so the limiter sees the knee-opening side.
///
/// TODO §3 convergence suite: this table is what turns the free-swinging
/// semiconductor Newton iterates into the bounded, knee-updating walk the
/// reference tools use.
#[derive(Debug, Clone, Default)]
pub struct DcJunctionLimits {
    /// Diode anode→cathode biases (element order).
    pub diode: Vec<f64>,
    /// BJT normalized junction biases `[vbe, vbc]` (element order).
    pub bjt: Vec<[f64; 2]>,
    /// JFET normalized gate-junction biases `[vgs, vgd]` (element order).
    pub jfet: Vec<[f64; 2]>,
}

impl DcJunctionLimits {
    /// A freshly zeroed table with one slot per element (matches `seed`).
    pub fn zeroed(circuit: &Circuit) -> Self {
        Self {
            diode: vec![0.0; circuit.diodes.len()],
            bjt: vec![[0.0; 2]; circuit.bjts.len()],
            jfet: vec![[0.0; 2]; circuit.jfets.len()],
        }
    }

    /// Bias tables read off one iterate (the seed's junction voltages).
    pub fn seed(circuit: &Circuit, guess: &DVector<f64>) -> Self {
        let diode = circuit
            .diodes
            .iter()
            .map(|d| node_voltage(guess, d.anode) - node_voltage(guess, d.cathode))
            .collect();
        let bjt = circuit
            .bjts
            .iter()
            .map(|q| {
                let sign = match circuit.bjt_models.get(&q.model) {
                    Some(model) if model.kind == BjtType::Pnp => -1.0,
                    _ => 1.0,
                };
                let vbe = node_voltage(guess, q.nb) - node_voltage(guess, q.ne);
                let vbc = node_voltage(guess, q.nb) - node_voltage(guess, q.nc);
                [sign * vbe, sign * vbc]
            })
            .collect();
        let jfet = circuit
            .jfets
            .iter()
            .map(|j| {
                let sign = match circuit.jfet_models.get(&j.model) {
                    Some(model) if model.kind == JfetType::Pjf => -1.0,
                    _ => 1.0,
                };
                let vgs = node_voltage(guess, j.ng) - node_voltage(guess, j.ns);
                let vgd = node_voltage(guess, j.ng) - node_voltage(guess, j.nd);
                [sign * vgs, sign * vgd]
            })
            .collect();
        Self { diode, bjt, jfet }
    }
}

/// How one Newton build damps a junction's linearization bias.
enum JunctionDamp<'a> {
    /// Evaluate the model at the raw iterate bias (the AC build and the
    /// BJT/JFET transient stamps: no per-iterate limit in those paths).
    Raw,
    /// Transient diode convention kept from before the convergence suite:
    /// hold the bias within `window` volts of the previously *accepted*
    /// junction voltage, both directions.
    Clamp {
        previous: &'a [f64],
        window: f64,
    },
    /// DC `pnjlim`: limit the excursion against the previous build's
    /// linearization points and record the used limits.
    Pnjlim {
        previous: &'a DcJunctionLimits,
        used: &'a mut DcJunctionLimits,
    },
}

/// The `pnjlim` hand-off for the two-junction devices (BJT, JFET): the
/// previous build's linearization points in, this build's used limits out.
struct PnjlimIo<'a> {
    previous: &'a DcJunctionLimits,
    used: &'a mut DcJunctionLimits,
}

/// Record one scalar limit slot, guarding against a shorter table.
fn record_diode_limit(used: &mut DcJunctionLimits, index: usize, value: f64) {
    if let Some(slot) = used.diode.get_mut(index) {
        *slot = value;
    }
}

/// Record one pair of BJT limits, guarding against a shorter table.
fn record_bjt_limit(used: &mut DcJunctionLimits, index: usize, pair: [f64; 2]) {
    if let Some(slot) = used.bjt.get_mut(index) {
        *slot = pair;
    }
}

/// Record one pair of JFET gate-junction limits, guarding against a shorter
/// table.
fn record_jfet_limit(used: &mut DcJunctionLimits, index: usize, pair: [f64; 2]) {
    if let Some(slot) = used.jfet.get_mut(index) {
        *slot = pair;
    }
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
    /// First row of the `E` (VCVS) branch block.
    pub vcvs_base: usize,
    /// First row of the `H` (CCVS) branch block.
    pub ccvs_base: usize,
    /// First row of the voltage-flavour behavioral (`B`) branch block.
    pub behavioral_base: usize,
}

impl BranchLayout {
    pub fn for_circuit(circuit: &Circuit) -> Self {
        let n_nodes = circuit.nodes.saturating_sub(1);
        let vsrc_base = n_nodes;
        let ind_base = vsrc_base + circuit.voltage_sources.len();
        let vcvs_base = ind_base + circuit.inductors.len();
        let ccvs_base = vcvs_base + circuit.vcvs.len();
        let behavioral_base = ccvs_base + circuit.ccvs.len();
        Self { n_nodes, vsrc_base, ind_base, vcvs_base, ccvs_base, behavioral_base }
    }

    /// Row index of the current unknown of voltage source `i`.
    pub fn src_branch(&self, i: usize) -> usize {
        self.vsrc_base + i
    }

    /// Row index of the current unknown of inductor `i`.
    pub fn ind_branch(&self, i: usize) -> usize {
        self.ind_base + i
    }

    /// Row index of the output current unknown of VCVS `i`.
    pub fn vcvs_branch(&self, i: usize) -> usize {
        self.vcvs_base + i
    }

    /// Row index of the output current unknown of CCVS `i`.
    pub fn ccvs_branch(&self, i: usize) -> usize {
        self.ccvs_base + i
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
            branch: BranchLayout {
                n_nodes: 0,
                vsrc_base: 0,
                ind_base: 0,
                vcvs_base: 0,
                ccvs_base: 0,
                behavioral_base: 0,
            },
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
///
/// The DC arm carries the TODO §3 convergence-suite state: the junction
/// limiter tables (previous linearization points in, used limits out), the
/// `gmin` floor stamped across every pn junction and capacitor, and the
/// `src_scale` factor that source stepping uses to bring the independent
/// sources up.
#[derive(Debug)]
pub enum Mode<'a> {
    /// DC operating point: nonlinear devices linearised at `guess` (or at the
    /// `pnjlim`-limited junction biases), capacitors open, inductors as short
    /// branches.
    Dc {
        guess: &'a DVector<f64>,
        /// Limiter state carried between builds: linearizes against what the
        /// previous build used.
        limits: &'a DcJunctionLimits,
        /// This build's used limits are written back here as they are applied.
        used: &'a mut DcJunctionLimits,
        /// Conductance floor across pn junctions and capacitors (already
        /// multiplied by any stepping factor the caller wants).
        gmin: f64,
        /// Fraction all independent source values are multiplied by (1.0 outside
        /// source stepping).
        src_scale: f64,
    },
    /// One transient step at time `t` with step `dt`. `startup` ramps every
    /// independent V/I source from 0 to its card value over the first 10 µs
    /// (the `.tran … STARTUP` flag).
    Tran {
        dt: f64,
        t: f64,
        state: &'a TransientState,
        method: IntegrationMethod,
        guess: &'a DVector<f64>,
        startup: bool,
    },
    /// Small-signal AC point at `freq`, linearised at the DC bias point.
    /// Independent voltage sources become short circuits; the analysis driver
    /// patches the stimulus source afterwards. Current sources with an AC
    /// magnitude inject their phasor directly.
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
    // Controlled sources are linear in every analysis: their coefficients do
    // not depend on the bias point (a `POLY` output is linear in the control
    // variables), so one stamp serves DC, transient, and AC.
    stamp_controlled_sources(circuit, &mut sys);

    match mode {
        Mode::Dc { guess, limits, used, gmin, src_scale } => {
            stamp_switches(circuit, &mut sys, guess, &[])?;
            stamp_diodes(
                circuit,
                &mut sys,
                guess,
                true,
                JunctionDamp::Pnjlim { previous: limits, used },
                gmin,
            )?;
            stamp_bjts(
                circuit,
                &mut sys,
                guess,
                true,
                Some(PnjlimIo { previous: limits, used }),
                gmin,
            )?;
            stamp_mosfets(circuit, &mut sys, guess, true)?;
            stamp_jfets(
                circuit,
                &mut sys,
                guess,
                true,
                Some(PnjlimIo { previous: limits, used }),
                gmin,
            )?;

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

            stamp_voltage_sources(circuit, &mut sys, |v| v.voltage * src_scale);
            stamp_current_sources(circuit, &mut sys, |i| i.current * src_scale);
            stamp_behavioral(circuit, &mut sys, guess, true, 0.0, 0.0, &[])?;

            // The gmin floor across capacitors anchors the classic R–C island
            // (a node reachable only through capacitors would otherwise leave
            // its row unstamped and the matrix singular). MOSFETs need no
            // separate gmin: their `gds` floor plays that role already.
            if gmin > 0.0 {
                for c in &circuit.capacitors {
                    stamp_admittance(&mut sys, c.n1, c.n2, T::lift(gmin));
                }
            }
        }
        Mode::Tran { dt, t, state, method, guess, startup } => {
            if dt <= 0.0 {
                return Err(SimError::Analysis("time step must be positive".into()));
            }
            stamp_switches(circuit, &mut sys, guess, &state.switch_states)?;
            // The transient diode path keeps its pre-limiter convention: one
            // linearization point at most 0.1 V from the last accepted
            // junction voltage, in both directions.
            stamp_diodes(
                circuit,
                &mut sys,
                guess,
                true,
                JunctionDamp::Clamp { previous: &state.diode_voltages, window: 0.1 },
                0.0,
            )?;
            stamp_diode_capacitances(circuit, &mut sys, guess, state, dt, method)?;
            stamp_bjts(circuit, &mut sys, guess, true, None, 0.0)?;
            stamp_bjt_capacitances(circuit, &mut sys, guess, state, dt, method)?;
            stamp_mosfets(circuit, &mut sys, guess, true)?;
            stamp_mosfet_capacitances(circuit, &mut sys, state, dt, method)?;
            stamp_jfets(circuit, &mut sys, guess, true, None, 0.0)?;
            stamp_jfet_capacitances(circuit, &mut sys, guess, state, dt, method)?;
            stamp_capacitors(circuit, &mut sys, dt, state, method);
            stamp_inductors(circuit, &mut sys, dt, state, method);
            stamp_voltage_sources(circuit, &mut sys, |v| v.value_at(t) * startup_ramp(t, startup));
            stamp_current_sources(circuit, &mut sys, |i| i.value_at(t) * startup_ramp(t, startup));
            stamp_behavioral(circuit, &mut sys, guess, true, t, dt, &state.behavioral)?;
        }
        Mode::Ac { freq, bias } => {
            let omega = 2.0 * std::f64::consts::PI * freq;
            stamp_switches(circuit, &mut sys, bias, &[])?;

            // Y_C = jωC
            for c in &circuit.capacitors {
                let y = T::imag(omega * circuit.capacitance_at(c));
                stamp_admittance(&mut sys, c.n1, c.n2, y);
            }

            // Inductors (as branch unknowns, Z_L = jωL):
            //   V_L = Z_L · I_L  →  -Z_L · I_L + V_{n1} - V_{n2} = 0
            // z[branch] = 0 (no independent source in the inductor branch).
            for (i, l) in circuit.inductors.iter().enumerate() {
                let branch = sys.branch.ind_branch(i);
                let z_l = T::imag(omega * circuit.inductance_at(l));
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
            stamp_ac_mutual_inductances(circuit, &mut sys, omega);

            // Small-signal stamps at the DC bias: linearisation only, no
            // Newton-Raphson companion currents (and no limiter or gmin —
            // those live in the operating point the bias came from).
            stamp_diodes(circuit, &mut sys, bias, false, JunctionDamp::Raw, 0.0)?;
            stamp_diode_capacitances_ac(circuit, &mut sys, bias, omega)?;
            stamp_bjts(circuit, &mut sys, bias, false, None, 0.0)?;
            stamp_bjt_capacitances_ac(circuit, &mut sys, bias, omega);
            stamp_mosfets(circuit, &mut sys, bias, false)?;
            stamp_mosfet_capacitances_ac(circuit, &mut sys, omega);
            stamp_jfets(circuit, &mut sys, bias, false, None, 0.0)?;
            stamp_jfet_capacitances_ac(circuit, &mut sys, bias, omega);

            // Independent DC voltage sources become short circuits (0 V); the
            // caller drives the stimulus source by writing sys.z[src_branch(s)].
            stamp_voltage_sources(circuit, &mut sys, |_| 0.0);

            // Current sources: zero unless the card carries an AC magnitude —
            // then the phasor `mag·∠phase` is injected at the source terminals.
            for i in &circuit.current_sources {
                if let Some(ac) = &i.ac {
                    let phasor = T::lift(ac.mag * ac.phase.to_radians().cos())
                        + T::imag(ac.mag * ac.phase.to_radians().sin());
                    stamp_current_source(&mut sys, i.n1, i.n2, phasor);
                }
            }

            // Behavioral sources linearize at the DC bias with real-difference
            // Jacobians; time/history terms behave as constants there.
            stamp_behavioral(circuit, &mut sys, bias, false, 0.0, 0.0, &[])?;
        }
    }

    Ok(sys)
}

// ─── Element stamps ─────────────────────────────────────────────────────────

fn stamp_resistors<T: Field>(circuit: &Circuit, sys: &mut MnaSystem<T>) {
    for r in &circuit.resistors {
        let g = T::lift(1.0 / circuit.resistance_at(r));
        stamp_admittance(sys, r.n1, r.n2, g);
    }
}

/// Resolve a controlling voltage-source name to its index (case-insensitive).
fn voltage_source_index(circuit: &Circuit, name: &str) -> Option<usize> {
    circuit
        .voltage_sources
        .iter()
        .position(|v| v.name.eq_ignore_ascii_case(name))
}

/// Stamp every `E`/`G`/`F`/`H` controlled source.
///
/// `E`/`H` gain a branch row (`V(out) = polynomial of controls`); `G`/`F`
/// contribute a dependent current between their output nodes. For current
/// controls the MNA branch unknown `j` and the element-convention current
/// differ by a sign (`I = −j`), folded into the coefficient below.
fn stamp_controlled_sources<T: Field>(circuit: &Circuit, sys: &mut MnaSystem<T>) {
    let layout = sys.branch;

    // VCVS (E): one branch unknown, output voltage equals the polynomial.
    for (i, c) in circuit.vcvs.iter().enumerate() {
        let row = layout.vcvs_branch(i);
        stamp_branch_nodes(sys, row, c.out_p, c.out_n);
        for (k, control) in c.controls.iter().enumerate() {
            let coeff = c.coeff(k);
            if let ControlVar::Voltage(p, n) = control {
                if let Some(col) = node_index(*p) {
                    sys.a[(row, col)] -= T::lift(coeff);
                }
                if let Some(col) = node_index(*n) {
                    sys.a[(row, col)] += T::lift(coeff);
                }
            }
        }
        sys.z[row] += T::lift(c.constant());
    }

    // CCVS (H): branch row driven by controlling source currents.
    for (i, c) in circuit.ccvs.iter().enumerate() {
        let row = layout.ccvs_branch(i);
        stamp_branch_nodes(sys, row, c.out_p, c.out_n);
        for (k, control) in c.controls.iter().enumerate() {
            let coeff = c.coeff(k);
            if let ControlVar::Current(name) = control {
                if let Some(idx) = voltage_source_index(circuit, name) {
                    let col = layout.src_branch(idx);
                    sys.a[(row, col)] -= T::lift(coeff);
                }
            }
        }
        sys.z[row] += T::lift(c.constant());
    }

    // VCCS (G): transconductance into the output nodes.
    for c in &circuit.vccs {
        for (k, control) in c.controls.iter().enumerate() {
            let coeff = c.coeff(k);
            if let ControlVar::Voltage(p, n) = control {
                stamp_transconductance(sys, c.out_p, c.out_n, *p, *n, T::lift(coeff));
            }
        }
        let c0 = T::lift(c.constant());
        stamp_current_source(sys, c.out_p, c.out_n, c0);
    }

    // CCCS (F): dependent current driven by a source branch unknown.
    for c in &circuit.cccs {
        for (k, control) in c.controls.iter().enumerate() {
            let coeff = c.coeff(k);
            // The MNA branch unknown j *is* the element-convention current
            // n1 → n2 through the controlling source, so no sign flip.
            if let ControlVar::Current(name) = control {
                if let Some(idx) = voltage_source_index(circuit, name) {
                    let col = layout.src_branch(idx);
                    if let Some(out) = node_index(c.out_p) {
                        sys.a[(out, col)] += T::lift(coeff);
                    }
                    if let Some(out) = node_index(c.out_n) {
                        sys.a[(out, col)] -= T::lift(coeff);
                    }
                }
            }
        }
        let c0 = T::lift(c.constant());
        stamp_current_source(sys, c.out_p, c.out_n, c0);
    }
}

/// `V(a) − V(b) = 0` row plus the symmetric branch couplings.
fn stamp_branch_nodes<T: Field>(sys: &mut MnaSystem<T>, row: usize, p: NodeId, n: NodeId) {
    if let Some(i) = node_index(p) {
        sys.a[(i, row)] += T::lift(1.0);
        sys.a[(row, i)] += T::lift(1.0);
    }
    if let Some(j) = node_index(n) {
        sys.a[(j, row)] -= T::lift(1.0);
        sys.a[(row, j)] -= T::lift(1.0);
    }
}

/// Dependent current `g·(V(cp) − V(cn))` flowing `out_p → out_n`.
fn stamp_transconductance<T: Field>(
    sys: &mut MnaSystem<T>,
    out_p: NodeId,
    out_n: NodeId,
    cp: NodeId,
    cn: NodeId,
    g: T,
) {
    if let Some(row) = node_index(out_p) {
        if let Some(col) = node_index(cp) { sys.a[(row, col)] += g; }
        if let Some(col) = node_index(cn) { sys.a[(row, col)] -= g; }
    }
    if let Some(row) = node_index(out_n) {
        if let Some(col) = node_index(cp) { sys.a[(row, col)] -= g; }
        if let Some(col) = node_index(cn) { sys.a[(row, col)] += g; }
    }
}

// ─── Behavioral sources (`B`) ────────────────────────────────────────────────

/// One behavioral source's linearized companion model at a probe snapshot.
///
/// The value is `f(x) ≈ c0 + Σ dk·xk` with `c0 = f0 − Σ dk·x0`; the snapshot
/// is recovered as part of the returned coefficients' meaning, so callers
/// stamp `dk` into `A` and `c0` into `z` exactly once. History/time terms are
/// frozen for a solve: they change only between accepted steps, never inside
/// a Newton iteration.
struct BehavioralLin {
    /// `f(x0)` at the snapshot.
    f0: f64,
    /// `∂f/∂probe_k` by central differences on the probe values.
    dk: Vec<f64>,
}

fn behavioral_probe_lin(
    source: &crate::circuit::BehavioralSource,
    probe_values: &[f64],
    t: f64,
    dt: f64,
    runtime: &BehavioralRuntime,
) -> Result<BehavioralLin> {
    let (f0, dk) = source.behavior.linearize(probe_values, t, dt, runtime).map_err(|e| {
        SimError::Analysis(format!("behavioral source '{}': {e}", source.name))
    })?;
    Ok(BehavioralLin { f0, dk })
}

/// Map every probe of `source` to its value in a solved/iterate vector.
///
/// Node probes read voltages, `I(src)`/`I(L…)` read the raw branch unknown —
/// the element-convention current (a delivering voltage source reads
/// negative), the same quantity every other `I(…)` consumer sees.
fn behavioral_probe_snapshot(
    circuit: &Circuit,
    source: &crate::circuit::BehavioralSource,
    values: &DVector<f64>,
) -> Result<Vec<f64>> {
    let branch = BranchLayout::for_circuit(circuit);
    source
        .behavior
        .probes
        .iter()
        .map(|probe| {
            let v = match probe {
                crate::behavior::ProbeBinding::Voltage(p, m) => {
                    node_voltage(values, *p) - node_voltage(values, *m)
                }
                crate::behavior::ProbeBinding::SourceCurrent(i) => {
                    values[branch.src_branch(*i)]
                }
                crate::behavior::ProbeBinding::InductorCurrent(i) => {
                    values[branch.ind_branch(*i)]
                }
                crate::behavior::ProbeBinding::Unresolved(spec) => {
                    return Err(SimError::Analysis(format!(
                        "behavioral source '{}': probe {spec:?} was never bound — the parse \
                         pipeline must run resolve_behavioral() before stamping",
                        source.name
                    )));
                }
            };
            if !v.is_finite() {
                return Err(SimError::Analysis(format!(
                    "behavioral source '{}': probe value is not finite",
                    source.name
                )));
            }
            Ok(v)
        })
        .collect()
}

/// Stamp every behavioral source at the given iterate.
///
/// * `with_companion` — false only for AC, where the small-signal stamp is
///   pure linearization (`z` gets nothing, like the other devices).
/// * `runtimes` — one entry per source in card order (empty in DC/AC; the
///   stamp reads zeroed history, and `.get(i)` falls back to a default).
fn stamp_behavioral<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    values: &DVector<f64>,
    with_companion: bool,
    t: f64,
    dt: f64,
    runtimes: &[BehavioralRuntime],
) -> Result<()> {
    if circuit.behavioral.is_empty() {
        return Ok(());
    }
    let branch = sys.branch;
    // Voltage-flavour sources own branch rows in card order, independent of
    // the current-flavour ones interleaved between them.
    let mut voltage_row = branch.behavioral_base;
    for (i, b) in circuit.behavioral.iter().enumerate() {
        let empty = BehavioralRuntime::default();
        let runtime = runtimes.get(i).unwrap_or(&empty);
        let snapshot = behavioral_probe_snapshot(circuit, b, values)?;
        let lin = behavioral_probe_lin(b, &snapshot, t, dt, runtime)?;
        let constant = lin.f0
            - lin
                .dk
                .iter()
                .zip(&snapshot)
                .map(|(d, x)| d * x)
                .sum::<f64>();

        match b.kind {
            BehavioralKind::Voltage => {
                let row = voltage_row;
                voltage_row += 1;
                // `V(out_p) − V(out_n) = f(x)`: the ±1 branch-node couplings
                // first, then each Jacobian column, then the companion.
                stamp_branch_nodes(sys, row, b.out_p, b.out_n);
                for (probe, &dk) in b.behavior.probes.iter().zip(&lin.dk) {
                    if dk == 0.0 {
                        continue;
                    }
                    match probe {
                        crate::behavior::ProbeBinding::Voltage(p, m) => {
                            if let Some(col) = node_index(*p) {
                                sys.a[(row, col)] -= T::lift(dk);
                            }
                            if let Some(col) = node_index(*m) {
                                sys.a[(row, col)] += T::lift(dk);
                            }
                        }
                        crate::behavior::ProbeBinding::SourceCurrent(idx) => {
                            let col = branch.src_branch(*idx);
                            sys.a[(row, col)] -= T::lift(dk);
                        }
                        crate::behavior::ProbeBinding::InductorCurrent(idx) => {
                            let col = branch.ind_branch(*idx);
                            sys.a[(row, col)] -= T::lift(dk);
                        }
                        crate::behavior::ProbeBinding::Unresolved(_) => unreachable!(
                            "probes are bound before stamping (resolve_behavioral)"
                        ),
                    }
                }
                if with_companion {
                    sys.z[row] = T::lift(constant);
                }
            }
            BehavioralKind::Current => {
                // Current `f(x)` flows `out_p → out_n`: drawn from `out_p`,
                // delivered into `out_n` — the element convention of the `I`
                // card. The linear terms ride the node rows; the companion
                // goes into `z` exactly once.
                if let Some(row) = node_index(b.out_p) {
                    for (probe, &dk) in b.behavior.probes.iter().zip(&lin.dk) {
                        if dk == 0.0 {
                            continue;
                        }
                        match probe {
                            crate::behavior::ProbeBinding::Voltage(p, m) => {
                                if let Some(col) = node_index(*p) {
                                    sys.a[(row, col)] += T::lift(dk);
                                }
                                if let Some(col) = node_index(*m) {
                                    sys.a[(row, col)] -= T::lift(dk);
                                }
                            }
                            crate::behavior::ProbeBinding::SourceCurrent(idx) => {
                                sys.a[(row, branch.src_branch(*idx))] += T::lift(dk);
                            }
                            crate::behavior::ProbeBinding::InductorCurrent(idx) => {
                                sys.a[(row, branch.ind_branch(*idx))] += T::lift(dk);
                            }
                            crate::behavior::ProbeBinding::Unresolved(_) => unreachable!(
                                "probes are bound before stamping (resolve_behavioral)"
                            ),
                        }
                    }
                }
                if let Some(row) = node_index(b.out_n) {
                    for (probe, &dk) in b.behavior.probes.iter().zip(&lin.dk) {
                        if dk == 0.0 {
                            continue;
                        }
                        match probe {
                            crate::behavior::ProbeBinding::Voltage(p, m) => {
                                if let Some(col) = node_index(*p) {
                                    sys.a[(row, col)] -= T::lift(dk);
                                }
                                if let Some(col) = node_index(*m) {
                                    sys.a[(row, col)] += T::lift(dk);
                                }
                            }
                            crate::behavior::ProbeBinding::SourceCurrent(idx) => {
                                sys.a[(row, branch.src_branch(*idx))] -= T::lift(dk);
                            }
                            crate::behavior::ProbeBinding::InductorCurrent(idx) => {
                                sys.a[(row, branch.ind_branch(*idx))] -= T::lift(dk);
                            }
                            crate::behavior::ProbeBinding::Unresolved(_) => unreachable!(
                                "probes are bound before stamping (resolve_behavioral)"
                            ),
                        }
                    }
                }
                if with_companion {
                    stamp_current_source(sys, b.out_p, b.out_n, T::lift(constant));
                }
            }
        }
    }
    Ok(())
}

/// Conductance of one switch at the current iterate.
fn switch_conductance(
    circuit: &Circuit,
    index: usize,
    guess: &DVector<f64>,
    previous: &[Option<bool>],
) -> Result<f64> {
    let s = &circuit.switches[index];
    let model = circuit.switch_models.get(&s.model).ok_or_else(|| {
        SimError::Circuit(format!(
            "switch '{}' references unknown model '{}'",
            s.name, s.model
        ))
    })?;
    let layout = BranchLayout::for_circuit(circuit);
    let control = match &s.control {
        SwitchControl::Voltage(p, n) => node_voltage(guess, *p) - node_voltage(guess, *n),
        SwitchControl::Current(name) => {
            let idx = voltage_source_index(circuit, name).ok_or_else(|| {
                SimError::Circuit(format!(
                    "switch '{}' cannot read current through '{}'",
                    s.name, name
                ))
            })?;
            layout.src_row(guess, idx)
        }
    };
    let was_on = previous
        .get(index)
        .copied()
        .flatten()
        .or(s.initial);
    Ok(model.conductance(control, was_on).0)
}

/// Stamp every switch as a conductance, using the previous state for
/// hysteresis. An empty `previous` (DC/AC) falls back to the card's `ON`/
/// `OFF` keyword or the threshold comparison.
fn stamp_switches<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    guess: &DVector<f64>,
    previous: &[Option<bool>],
) -> Result<()> {
    for i in 0..circuit.switches.len() {
        let g = switch_conductance(circuit, i, guess, previous)?;
        let s = &circuit.switches[i];
        stamp_admittance(sys, s.n1, s.n2, T::lift(g));
    }
    Ok(())
}

/// `with_companion`: DC/transient stamping linearises for Newton-Raphson and
/// therefore also stamps the equivalent current source `i_eq`; the AC
/// small-signal stamp is pure linearisation about the bias and skips it —
/// exactly as the pre-consolidation paths did.
///
/// `damp` selects the junction excursion limiter; `gmin` (when > 0) is the
/// conductance floor stamped in parallel with the junction (TODO §3: it keeps
/// a converged junction's matrix regular and power-limits the exponential's
/// pull from the very first iterate).
fn stamp_diodes<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    guess: &DVector<f64>,
    with_companion: bool,
    mut damp: JunctionDamp,
    gmin: f64,
) -> Result<()> {
    for (index, d) in circuit.diodes.iter().enumerate() {
        let model = circuit.diode_models.get(&d.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "diode '{}' references unknown model '{}'",
                d.name, d.model
            ))
        })?;
        let v_anode = node_voltage(guess, d.anode);
        let v_cathode = node_voltage(guess, d.cathode);
        // Linearization, clamp, limit, and companion current all come from
        // the model module's conventions — the physics lives in one place
        // (model.rs). `area` is the element's AREA multiplier (default 1).
        let raw = v_anode - v_cathode;
        let vd = match &mut damp {
            JunctionDamp::Raw => raw,
            JunctionDamp::Clamp { previous, window } => {
                let window = *window;
                match previous.get(index) {
                    Some(previous) => raw.clamp(previous - window, previous + window),
                    None => raw,
                }
            }
            JunctionDamp::Pnjlim { previous, used } => {
                // The SPICE2 limiter in the emission-coefficient-scaled
                // thermal voltage, against the previous build's knee point.
                let vt_j = model.n * thermal_voltage(circuit.temperature);
                let vcrit = critical_voltage(vt_j, model.is_at(circuit.temperature) * d.area);
                let limited =
                    pnjlim(raw, previous.diode.get(index).copied().unwrap_or(0.0), vt_j, vcrit);
                record_diode_limit(*used, index, limited);
                limited
            }
        };
        let ev = model.eval_for_at(vd, d.area, circuit.temperature);

        stamp_admittance(sys, d.anode, d.cathode, T::lift(ev.gd));
        if with_companion {
            stamp_current_source(sys, d.anode, d.cathode, T::lift(ev.i_eq));
        }
        if gmin > 0.0 {
            stamp_admittance(sys, d.anode, d.cathode, T::lift(gmin));
        }
    }

    Ok(())
}

/// Diode junction + transit-time capacitance for a transient companion.
///
/// The stored charge is history-based (BE/trapezoidal/BDF2 chose the same
/// linearization), so the diode contributes `dQ/dV` as an admittance plus the
/// charge-history current as an equivalent source.
fn stamp_diode_capacitances<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    guess: &DVector<f64>,
    state: &TransientState,
    dt: f64,
    method: IntegrationMethod,
) -> Result<()> {
    for (i, d) in circuit.diodes.iter().enumerate() {
        let model = circuit.diode_models.get(&d.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "diode '{}' references unknown model '{}'",
                d.name, d.model
            ))
        })?;
        if model.cjo == 0.0 && model.tt == 0.0 {
            continue;
        }
        let vd = node_voltage(guess, d.anode) - node_voltage(guess, d.cathode);
        let q = model.total_charge_at(vd, d.area, circuit.temperature);
        let dqdv = model.dcharge_dv_at(vd, d.area, circuit.temperature);
        let q_prev = state.diode_charges.get(i).copied().unwrap_or(q);
        let q_older = state.diode_charges_older.get(i).copied().unwrap_or(q_prev);
        let i_prev = state.diode_cap_currents.get(i).copied().unwrap_or(0.0);
        let (g_eq, i_eq) = match method {
            IntegrationMethod::Trapezoidal => {
                let g = 2.0 * dqdv / dt;
                // i = g·v + i_eq is the companion; at v = vd it must equal
                // the charge-history current 2(Q−Qprev)/dt − iprev.
                (g, g * vd - (2.0 * (q - q_prev) / dt - i_prev))
            }
            IntegrationMethod::Gear1 => {
                let g = dqdv / dt;
                (g, g * vd - (q - q_prev) / dt)
            }
            IntegrationMethod::Gear2 => {
                let (a0, a1, a2) = bdf2_coefficients(state.last_dt, dt);
                let g = dqdv * a0;
                (g, g * vd - (a0 * q + a1 * q_prev + a2 * q_older))
            }
        };
        if g_eq.is_finite() && i_eq.is_finite() {
            stamp_admittance(sys, d.anode, d.cathode, T::lift(g_eq));
            // Companion sources inject n2 → n1, matching the linear
            // capacitor stamp's sign convention.
            stamp_current_source(sys, d.cathode, d.anode, T::lift(i_eq));
        }
    }
    Ok(())
}

/// Small-signal diode capacitance at the DC bias (`jωC`), including the
/// transit-time diffusion term.
fn stamp_diode_capacitances_ac<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    bias: &DVector<f64>,
    omega: f64,
) -> Result<()> {
    for d in &circuit.diodes {
        let model = circuit.diode_models.get(&d.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "diode '{}' references unknown model '{}'",
                d.name, d.model
            ))
        })?;
        if model.cjo == 0.0 && model.tt == 0.0 {
            continue;
        }
        let vd = node_voltage(bias, d.anode) - node_voltage(bias, d.cathode);
        let c = model.junction_capacitance(vd, d.area)
            + model.tt * model.eval_for_at(vd, d.area, circuit.temperature).gd;
        if c > 0.0 && omega > 0.0 {
            stamp_admittance(sys, d.anode, d.cathode, T::imag(omega * c));
        }
    }
    Ok(())
}

/// Stamp a BJT from its Ebers–Moll terminal currents.
///
/// The Jacobian is formed by central differences on `(Vbe, Vbc)` — the two
/// independent junction biases — and each terminal's row is the chain rule
/// of those derivatives into the real collector/base/emitter voltages.
///
/// `dc_limits` arms the TODO §3 junction limiter for the DC build: each
/// junction is limited in the *normalized* (NPN) space — where a forward bias
/// is positive for either polarity — with its own emission coefficient
/// (`NF`/`NR`) and critical voltage. The transient path passes `None` (the
/// BJT junctions keep their model-internal relaxation only).
fn stamp_bjts<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    guess: &DVector<f64>,
    with_companion: bool,
    mut dc_limits: Option<PnjlimIo>,
    gmin: f64,
) -> Result<()> {
    for (index, q) in circuit.bjts.iter().enumerate() {
        let model = circuit.bjt_models.get(&q.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "BJT '{}' references unknown model '{}'",
                q.name, q.model
            ))
        })?;
        let temp = circuit.temperature;
        let sign = if model.kind == BjtType::Pnp { -1.0 } else { 1.0 };
        // Normalized junction biases: forward is positive in this space for
        // every polarity, which is what the limiter walks.
        let mut norm_be = sign * (node_voltage(guess, q.nb) - node_voltage(guess, q.ne));
        let mut norm_bc = sign * (node_voltage(guess, q.nb) - node_voltage(guess, q.nc));
        if let Some(io) = dc_limits.as_mut() {
            let vt = thermal_voltage(temp);
            let is_area = model.is_at(temp) * q.area;
            let [be_old, bc_old] = io.previous.bjt.get(index).copied().unwrap_or([0.0, 0.0]);
            norm_be = pnjlim(norm_be, be_old, model.nf * vt, critical_voltage(model.nf * vt, is_area));
            norm_bc = pnjlim(norm_bc, bc_old, model.nr * vt, critical_voltage(model.nr * vt, is_area));
            record_bjt_limit(io.used, index, [norm_be, norm_bc]);
        }
        let vbe = sign * norm_be;
        let vbc = sign * norm_bc;
        let current = model.currents(vbe, vbc, q.area, temp);
        let h = 1e-6;
        let plus_be = model.currents(vbe + h, vbc, q.area, temp);
        let minus_be = model.currents(vbe - h, vbc, q.area, temp);
        let plus_bc = model.currents(vbe, vbc + h, q.area, temp);
        let minus_bc = model.currents(vbe, vbc - h, q.area, temp);

        // (I0, dI/dVbe, dI/dVbc) per terminal current.
        let terms = [
            (q.nc, current.ic, (plus_be.ic - minus_be.ic) / (2.0 * h), (plus_bc.ic - minus_bc.ic) / (2.0 * h)),
            (q.nb, current.ib, (plus_be.ib - minus_be.ib) / (2.0 * h), (plus_bc.ib - minus_bc.ib) / (2.0 * h)),
            (q.ne, current.ie, (plus_be.ie - minus_be.ie) / (2.0 * h), (plus_bc.ie - minus_bc.ie) / (2.0 * h)),
        ];
        for (node, i0, dbe, dbc) in terms {
            // Chain rule into real terminal voltages.
            let vb = dbe + dbc;
            let ve = -dbe;
            let vc = -dbc;
            if let Some(row) = node_index(node) {
                if let Some(col) = node_index(q.nb) { sys.a[(row, col)] += T::lift(vb); }
                if let Some(col) = node_index(q.ne) { sys.a[(row, col)] += T::lift(ve); }
                if let Some(col) = node_index(q.nc) { sys.a[(row, col)] += T::lift(vc); }
                if with_companion {
                    let constant = i0 - dbe * vbe - dbc * vbc;
                    sys.z[row] -= T::lift(constant);
                }
            }
        }
        if gmin > 0.0 {
            stamp_admittance(sys, q.nb, q.ne, T::lift(gmin));
            stamp_admittance(sys, q.nb, q.nc, T::lift(gmin));
        }
    }
    Ok(())
}

/// Transient companions for the B-E and B-C junction capacitances.
fn stamp_bjt_capacitances<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    guess: &DVector<f64>,
    state: &TransientState,
    dt: f64,
    method: IntegrationMethod,
) -> Result<()> {
    for (i, q) in circuit.bjts.iter().enumerate() {
        let Some(model) = circuit.bjt_models.get(&q.model) else {
            continue;
        };
        let vbe = node_voltage(guess, q.nb) - node_voltage(guess, q.ne);
        let vbc = node_voltage(guess, q.nb) - node_voltage(guess, q.nc);
        let caps = [
            model.cbe(vbe, q.area, circuit.temperature),
            model.cbc(vbc, q.area, circuit.temperature),
        ];
        let nodes = [(q.nb, q.ne), (q.nb, q.nc)];
        for k in 0..2 {
            let cap = caps[k];
            if cap <= 0.0 {
                continue;
            }
            let v_prev = state.bjt_cap_voltages.get(i).map(|v| v[k]).unwrap_or(0.0);
            let v_older = state
                .bjt_cap_voltages_older
                .get(i)
                .map(|v| v[k])
                .unwrap_or(v_prev);
            let i_prev = state.bjt_cap_currents.get(i).map(|v| v[k]).unwrap_or(0.0);
            let (g_eq, i_eq) = match method {
                IntegrationMethod::Trapezoidal => {
                    let g = 2.0 * cap / dt;
                    (g, g * v_prev + i_prev)
                }
                IntegrationMethod::Gear1 => {
                    let g = cap / dt;
                    (g, g * v_prev)
                }
                IntegrationMethod::Gear2 => {
                    let (a0, a1, a2) = bdf2_coefficients(state.last_dt, dt);
                    (cap * a0, -cap * (a1 * v_prev + a2 * v_older))
                }
            };
            stamp_admittance(sys, nodes[k].0, nodes[k].1, T::lift(g_eq));
            stamp_current_source(sys, nodes[k].1, nodes[k].0, T::lift(i_eq));
        }
    }
    Ok(())
}

fn stamp_bjt_capacitances_ac<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    bias: &DVector<f64>,
    omega: f64,
) {
    if omega <= 0.0 {
        return;
    }
    for q in &circuit.bjts {
        let Some(model) = circuit.bjt_models.get(&q.model) else {
            continue;
        };
        let vbe = node_voltage(bias, q.nb) - node_voltage(bias, q.ne);
        let vbc = node_voltage(bias, q.nb) - node_voltage(bias, q.nc);
        let cbe = model.cbe(vbe, q.area, circuit.temperature);
        let cbc = model.cbc(vbc, q.area, circuit.temperature);
        if cbe > 0.0 {
            stamp_admittance(sys, q.nb, q.ne, T::imag(omega * cbe));
        }
        if cbc > 0.0 {
            stamp_admittance(sys, q.nb, q.nc, T::imag(omega * cbc));
        }
    }
}

/// Stamp every JFET from its quadratic-channel terminal currents with a
/// central-difference Jacobian of the model currents in the two gate biases,
/// shaped exactly like the BJT stamp: one terminal current per terminal node
/// row, chained into the real terminal voltages, companion constant into `z`
/// when the caller runs Newton-Raphson (skipped for the AC arm).
///
/// `dc_limits` arms the TODO §3 junction limiter for the DC build: the two
/// gate junctions are limited in the *normalized* (N-channel) space with the
/// emission-coefficient-1 thermal voltage. The transient path passes `None`
/// (the gate junctions keep their model-internal relaxation only).
fn stamp_jfets<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    guess: &DVector<f64>,
    with_companion: bool,
    mut dc_limits: Option<PnjlimIo>,
    gmin: f64,
) -> Result<()> {
    for (index, j) in circuit.jfets.iter().enumerate() {
        let model = circuit.jfet_models.get(&j.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "JFET '{}' references unknown model '{}'",
                j.name, j.model
            ))
        })?;
        let temp = circuit.temperature;
        let sign = if model.kind == JfetType::Pjf { -1.0 } else { 1.0 };
        // Normalized gate-junction biases (forward is positive in this space).
        let mut norm_gs = sign * (node_voltage(guess, j.ng) - node_voltage(guess, j.ns));
        let mut norm_gd = sign * (node_voltage(guess, j.ng) - node_voltage(guess, j.nd));
        if let Some(io) = dc_limits.as_mut() {
            let vt = thermal_voltage(temp);
            let is_area = model.is_at(temp) * j.area;
            let vcrit = critical_voltage(vt, is_area);
            let [gs_old, gd_old] = io
                .previous
                .jfet
                .get(index)
                .copied()
                .unwrap_or([0.0, 0.0]);
            norm_gs = pnjlim(norm_gs, gs_old, vt, vcrit);
            norm_gd = pnjlim(norm_gd, gd_old, vt, vcrit);
            record_jfet_limit(io.used, index, [norm_gs, norm_gd]);
        }
        let vgs = sign * norm_gs;
        let vgd = sign * norm_gd;
        let current = model.currents(vgs, vgd, j.area, temp);
        let h = 1e-6;
        let plus_gs = model.currents(vgs + h, vgd, j.area, temp);
        let minus_gs = model.currents(vgs - h, vgd, j.area, temp);
        let plus_gd = model.currents(vgs, vgd + h, j.area, temp);
        let minus_gd = model.currents(vgs, vgd - h, j.area, temp);

        // (I0, dI/dVgs, dI/dVgd) per terminal current.
        let terms = [
            (j.nd, current.id,
             (plus_gs.id - minus_gs.id) / (2.0 * h), (plus_gd.id - minus_gd.id) / (2.0 * h)),
            (j.ng, current.ig,
             (plus_gs.ig - minus_gs.ig) / (2.0 * h), (plus_gd.ig - minus_gd.ig) / (2.0 * h)),
            (j.ns, current.is,
             (plus_gs.is - minus_gs.is) / (2.0 * h), (plus_gd.is - minus_gd.is) / (2.0 * h)),
        ];
        for (node, i0, dgs, dgd) in terms {
            // Chain rule into real terminal voltages.
            let vg = dgs + dgd;
            let vs = -dgs;
            let vd = -dgd;
            if let Some(row) = node_index(node) {
                if let Some(col) = node_index(j.ng) { sys.a[(row, col)] += T::lift(vg); }
                if let Some(col) = node_index(j.ns) { sys.a[(row, col)] += T::lift(vs); }
                if let Some(col) = node_index(j.nd) { sys.a[(row, col)] += T::lift(vd); }
                if with_companion {
                    let constant = i0 - dgs * vgs - dgd * vgd;
                    sys.z[row] -= T::lift(constant);
                }
            }
        }
        if gmin > 0.0 {
            stamp_admittance(sys, j.ng, j.ns, T::lift(gmin));
            stamp_admittance(sys, j.ng, j.nd, T::lift(gmin));
        }
    }
    Ok(())
}

/// Transient companions for the gate-source and gate-drain junction
/// capacitances (same shape as the BJT pair).
fn stamp_jfet_capacitances<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    guess: &DVector<f64>,
    state: &TransientState,
    dt: f64,
    method: IntegrationMethod,
) -> Result<()> {
    for (i, j) in circuit.jfets.iter().enumerate() {
        let Some(model) = circuit.jfet_models.get(&j.model) else {
            continue;
        };
        let vgs = node_voltage(guess, j.ng) - node_voltage(guess, j.ns);
        let vgd = node_voltage(guess, j.ng) - node_voltage(guess, j.nd);
        let caps = [model.cgs_at(vgs, j.area), model.cgd_at(vgd, j.area)];
        let nodes = [(j.ng, j.ns), (j.ng, j.nd)];
        for k in 0..2 {
            let cap = caps[k];
            if cap <= 0.0 {
                continue;
            }
            let v_prev = state.jfet_cap_voltages.get(i).map(|v| v[k]).unwrap_or(0.0);
            let v_older = state
                .jfet_cap_voltages_older
                .get(i)
                .map(|v| v[k])
                .unwrap_or(v_prev);
            let i_prev = state.jfet_cap_currents.get(i).map(|v| v[k]).unwrap_or(0.0);
            let (g_eq, i_eq) = match method {
                IntegrationMethod::Trapezoidal => {
                    let g = 2.0 * cap / dt;
                    (g, g * v_prev + i_prev)
                }
                IntegrationMethod::Gear1 => {
                    let g = cap / dt;
                    (g, g * v_prev)
                }
                IntegrationMethod::Gear2 => {
                    let (a0, a1, a2) = bdf2_coefficients(state.last_dt, dt);
                    (cap * a0, -cap * (a1 * v_prev + a2 * v_older))
                }
            };
            stamp_admittance(sys, nodes[k].0, nodes[k].1, T::lift(g_eq));
            stamp_current_source(sys, nodes[k].1, nodes[k].0, T::lift(i_eq));
        }
    }
    Ok(())
}

fn stamp_jfet_capacitances_ac<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    bias: &DVector<f64>,
    omega: f64,
) {
    if omega <= 0.0 {
        return;
    }
    for j in &circuit.jfets {
        let Some(model) = circuit.jfet_models.get(&j.model) else {
            continue;
        };
        let vgs = node_voltage(bias, j.ng) - node_voltage(bias, j.ns);
        let vgd = node_voltage(bias, j.ng) - node_voltage(bias, j.nd);
        let cgs = model.cgs_at(vgs, j.area);
        let cgd = model.cgd_at(vgd, j.area);
        if cgs > 0.0 {
            stamp_admittance(sys, j.ng, j.ns, T::imag(omega * cgs));
        }
        if cgd > 0.0 {
            stamp_admittance(sys, j.ng, j.nd, T::imag(omega * cgd));
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
) -> Result<()> {
    for m in &circuit.mosfets {
        let model = circuit.mosfet_models.get(&m.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "MOSFET '{}' references unknown model '{}'",
                m.name, m.model
            ))
        })?;

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
        let ev = model.eval_at(beta, vg - vs, (vd - vs).max(0.0), vb - vs, circuit.temperature);

        // ── Linearised companion model (Newton-Raphson stamp) ─────────────
        //
        // The terminal current into drain: Ids_lin = gds*Vds + gm*Vgs + gbs*Vbs + Ieq,
        // where all coefficients and Ieq come from model.rs at the same
        // linearization point.
        //
        // The mirroring already carries the sign in the *voltages*; the
        // linearized drain-current coefficients are the same in real-node
        // space for both polarities. Only the companion constant flips.
        let s = 1.0;
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
            // PMOS: I_ds = −i_eq_mirror + (linear terms), so the constant flips.
            let companion = if sign < 0.0 { -ev.i_eq } else { ev.i_eq };
            stamp_current_source(sys, drain, source, T::lift(companion));
        }
    }

    Ok(())
}

/// Terminal capacitances of one MOSFET: Cgs, Cgd, Cgb, Cdb, Csb.
///
/// Overlap terms are constant per width/length; the junction terms use the
/// zero-bias area/perimeter products (the grading coefficients are carried in
/// the model for a future bias-dependent upgrade).
fn mosfet_cap_values(circuit: &Circuit, m: &crate::circuit::Mosfet) -> [f64; 5] {
    let Some(model) = circuit.mosfet_models.get(&m.model) else {
        return [0.0; 5];
    };
    let cgs = model.cgso.unwrap_or(0.0) * m.w;
    let cgd = model.cgdo.unwrap_or(0.0) * m.w;
    let cgb = model.cgbo.unwrap_or(0.0) * m.l;
    let cj = model.cj.unwrap_or(0.0);
    let cjsw = model.cjsw.unwrap_or(0.0);
    let cdb = m.ad * cj + m.ps * cjsw;
    let csb = m.asr * cj + m.pd * cjsw;
    [cgs.max(0.0), cgd.max(0.0), cgb.max(0.0), cdb.max(0.0), csb.max(0.0)]
}

fn mosfet_cap_nodes(m: &crate::circuit::Mosfet) -> [(NodeId, NodeId); 5] {
    [
        (m.ng, m.ns),
        (m.ng, m.nd),
        (m.ng, m.nb),
        (m.nd, m.nb),
        (m.ns, m.nb),
    ]
}

/// Transient companion for the MOSFET terminal capacitances.
fn stamp_mosfet_capacitances<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    state: &TransientState,
    dt: f64,
    method: IntegrationMethod,
) -> Result<()> {
    for (i, m) in circuit.mosfets.iter().enumerate() {
        let caps = mosfet_cap_values(circuit, m);
        let nodes = mosfet_cap_nodes(m);
        for k in 0..5 {
            let cap = caps[k];
            if cap <= 0.0 {
                continue;
            }
            let v_prev = state
                .mosfet_cap_voltages
                .get(i)
                .map(|v| v[k])
                .unwrap_or(0.0);
            let v_older = state
                .mosfet_cap_voltages_older
                .get(i)
                .map(|v| v[k])
                .unwrap_or(v_prev);
            let i_prev = state
                .mosfet_cap_currents
                .get(i)
                .map(|v| v[k])
                .unwrap_or(0.0);
            let (g_eq, i_eq) = match method {
                IntegrationMethod::Trapezoidal => {
                    let g = 2.0 * cap / dt;
                    (g, g * v_prev + i_prev)
                }
                IntegrationMethod::Gear1 => {
                    let g = cap / dt;
                    (g, g * v_prev)
                }
                IntegrationMethod::Gear2 => {
                    let (a0, a1, a2) = bdf2_coefficients(state.last_dt, dt);
                    (cap * a0, -cap * (a1 * v_prev + a2 * v_older))
                }
            };
            stamp_admittance(sys, nodes[k].0, nodes[k].1, T::lift(g_eq));
            stamp_current_source(sys, nodes[k].1, nodes[k].0, T::lift(i_eq));
        }
    }
    Ok(())
}

/// Small-signal MOSFET terminal capacitances (`jωC`).
fn stamp_mosfet_capacitances_ac<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    omega: f64,
) {
    if omega <= 0.0 {
        return;
    }
    for m in &circuit.mosfets {
        let caps = mosfet_cap_values(circuit, m);
        let nodes = mosfet_cap_nodes(m);
        for k in 0..5 {
            if caps[k] > 0.0 {
                stamp_admittance(sys, nodes[k].0, nodes[k].1, T::imag(omega * caps[k]));
            }
        }
    }
}

/// Capacitor companion model for the selected integrator./// Capacitor companion model for the selected integrator.
///
/// With `g` the companion conductance and `i_eq` the equivalent current
/// injected `n2 → n1` (the same convention the backward-Euler path used):
///
/// * trapezoidal: `g = 2C/dt`, `i_eq = −g·v_prev − i_prev`
/// * Gear I (backward Euler): `g = C/dt`, `i_eq = g·v_prev`
/// * Gear II (constant step): `g = 1.5C/dt`,
///   `i_eq = −(2C/dt)·v_prev + (0.5C/dt)·v_older`
fn stamp_capacitors<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    dt: f64,
    state: &TransientState,
    method: IntegrationMethod,
) {
    for (i, c) in circuit.capacitors.iter().enumerate() {
        let cap = circuit.capacitance_at(c);
        let v_prev = state.capacitor_voltages.get(i).copied().unwrap_or(0.0);
        let i_prev = state.capacitor_currents.get(i).copied().unwrap_or(0.0);
        let v_older = state.capacitor_voltages_older.get(i).copied().unwrap_or(v_prev);
        let (g_eq, i_eq) = match method {
            IntegrationMethod::Trapezoidal => {
                let g = 2.0 * cap / dt;
                (g, g * v_prev + i_prev)
            }
            IntegrationMethod::Gear1 => {
                let g = cap / dt;
                (g, g * v_prev)
            }
            IntegrationMethod::Gear2 => {
                let (a0, a1, a2) = bdf2_coefficients(state.last_dt, dt);
                (cap * a0, -cap * (a1 * v_prev + a2 * v_older))
            }
        };
        stamp_admittance(sys, c.n1, c.n2, T::lift(g_eq));
        stamp_current_source(sys, c.n2, c.n1, T::lift(i_eq));
    }
}

/// Inductor companion model for the selected integrator (`r_eq` is the
/// companion resistance, `v_eq` the history voltage in the branch row).
fn stamp_inductors<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    dt: f64,
    state: &TransientState,
    method: IntegrationMethod,
) {
    for (i, l) in circuit.inductors.iter().enumerate() {
        let ind = circuit.inductance_at(l);
        let i_prev = state.inductor_currents.get(i).copied().unwrap_or(0.0);
        let i_older = state.inductor_currents_older.get(i).copied().unwrap_or(i_prev);
        let v_prev = state.inductor_voltages.get(i).copied().unwrap_or(0.0);
        let (r_eq, v_eq) = match method {
            IntegrationMethod::Trapezoidal => {
                let r = 2.0 * ind / dt;
                (r, -r * i_prev - v_prev)
            }
            IntegrationMethod::Gear1 => {
                let r = ind / dt;
                (r, -r * i_prev)
            }
            IntegrationMethod::Gear2 => {
                let (a0, a1, a2) = bdf2_coefficients(state.last_dt, dt);
                (ind * a0, ind * (a1 * i_prev + a2 * i_older))
            }
        };
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
        sys.z[branch] = T::lift(v_eq);
    }
    stamp_mutual_inductances(circuit, sys, dt, state, method);
}

/// Inductor index by card name (case-insensitive).
fn inductor_index(circuit: &Circuit, name: &str) -> Option<usize> {
    circuit
        .inductors
        .iter()
        .position(|l| l.name.eq_ignore_ascii_case(name))
}

/// Mutual coupling `M = k·√(L1·L2)` for the backward-Euler companion:
/// each inductor's voltage gains `(M/dt)·(i_other − i_other,prev)`.
fn stamp_mutual_inductances<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    dt: f64,
    state: &TransientState,
    method: IntegrationMethod,
) {
    for (kidx, k) in circuit.mutual_inductances.iter().enumerate() {
        let (Some(i), Some(j)) = (
            inductor_index(circuit, &k.inductor1),
            inductor_index(circuit, &k.inductor2),
        ) else {
            continue;
        };
        let m = k.coupling
            * (circuit.inductance_at(&circuit.inductors[i])
                * circuit.inductance_at(&circuit.inductors[j]))
                .sqrt();
        let prev_i = state.inductor_currents.get(i).copied().unwrap_or(0.0);
        let prev_j = state.inductor_currents.get(j).copied().unwrap_or(0.0);
        let older_i = state.inductor_currents_older.get(i).copied().unwrap_or(prev_i);
        let older_j = state.inductor_currents_older.get(j).copied().unwrap_or(prev_j);
        let v_mutual = state.mutual_voltages.get(kidx).copied().unwrap_or(0.0);
        let (coef, eq_i, eq_j) = match method {
            IntegrationMethod::Trapezoidal => {
                let c = 2.0 * m / dt;
                (c, -c * prev_j - v_mutual, -c * prev_i - v_mutual)
            }
            IntegrationMethod::Gear1 => {
                let c = m / dt;
                (c, -c * prev_j, -c * prev_i)
            }
            IntegrationMethod::Gear2 => {
                let (a0, a1, a2) = bdf2_coefficients(state.last_dt, dt);
                (
                    m * a0,
                    m * (a1 * prev_j + a2 * older_j),
                    m * (a1 * prev_i + a2 * older_i),
                )
            }
        };
        let bi = sys.branch.ind_branch(i);
        let bj = sys.branch.ind_branch(j);
        sys.a[(bi, bj)] -= T::lift(coef);
        sys.a[(bj, bi)] -= T::lift(coef);
        sys.z[bi] += T::lift(eq_i);
        sys.z[bj] += T::lift(eq_j);
    }
}

/// AC mutual coupling: cross terms `−jωM` in the two branch rows.
fn stamp_ac_mutual_inductances<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    omega: f64,
) {
    for k in &circuit.mutual_inductances {
        let (Some(i), Some(j)) = (
            inductor_index(circuit, &k.inductor1),
            inductor_index(circuit, &k.inductor2),
        ) else {
            continue;
        };
        let m = k.coupling
            * (circuit.inductance_at(&circuit.inductors[i])
                * circuit.inductance_at(&circuit.inductors[j]))
                .sqrt();
        let zm = T::imag(omega * m);
        let bi = sys.branch.ind_branch(i);
        let bj = sys.branch.ind_branch(j);
        sys.a[(bi, bj)] -= zm;
        sys.a[(bj, bi)] -= zm;
    }
}

/// `.tran … STARTUP`: every independent V/I source ramps from 0 to its card
/// value over the first 10 µs (the LTspice startup ramp), after which the
/// card value drives unchanged. Waveform-flavoured sources (PULSE/SIN/…)
/// scale their evaluated value the same way, so their time base stays put
/// while the amplitude comes up.
pub fn startup_ramp(t: f64, with_startup: bool) -> f64 {
    const RAMP: f64 = 10e-6;
    if !with_startup || t >= RAMP {
        1.0
    } else if t <= 0.0 {
        0.0
    } else {
        t / RAMP
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

/// Independent current sources: pure right-hand-side injections, no branch
/// rows. The value flows `n1 → n2` through the source, so `n2` receives it.
fn stamp_current_sources<T, F>(circuit: &Circuit, sys: &mut MnaSystem<T>, value_at: F)
where
    T: Field,
    F: Fn(&CurrentSource) -> f64,
{
    for i in &circuit.current_sources {
        let value = T::lift(value_at(i));
        stamp_current_source(sys, i.n1, i.n2, value);
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
    pub current_sources: Vec<f64>,
    pub diodes: Vec<f64>,
    /// Drain current (from drain to source) for each MOSFET.
    pub mosfets: Vec<f64>,
    /// Collector current (into the collector) for each BJT.
    pub bjts: Vec<f64>,
    /// Drain current (into the drain) for each JFET.
    pub jfets: Vec<f64>,
    /// Output current of each behavioral source (`n_p → n_n` element
    /// convention; for the voltage flavour, the branch unknown).
    pub behavioral: Vec<f64>,
}

pub fn compute_branch_currents(
    circuit: &Circuit,
    solution: &DVector<f64>,
    dt: Option<f64>,
    prev_cap_voltages: Option<&[f64]>,
    t: Option<f64>,
) -> Result<BranchCurrents> {
    let branch = BranchLayout::for_circuit(circuit);

    let resistors = circuit.resistors.iter().map(|r| {
        let v1 = node_voltage(solution, r.n1);
        let v2 = node_voltage(solution, r.n2);
        (v1 - v2) / circuit.resistance_at(r)
    }).collect();

    let capacitors = circuit.capacitors.iter().enumerate().map(|(i, c)| {
        match (dt, prev_cap_voltages) {
            (Some(dt_val), Some(prev)) => {
                let v1 = node_voltage(solution, c.n1);
                let v2 = node_voltage(solution, c.n2);
                let v_now = v1 - v2;
                let v_prev = prev.get(i).copied().unwrap_or(0.0);
                circuit.capacitance_at(c) * (v_now - v_prev) / dt_val
            }
            _ => 0.0,
        }
    }).collect();

    let inductors = circuit.inductors.iter().enumerate().map(|(i, _)| {
        branch.ind_row(solution, i)
    }).collect();

    // The branch unknown is the element-convention current n1 → n2.
    let voltage_sources = circuit.voltage_sources.iter().enumerate().map(|(i, _)| {
        branch.src_row(solution, i)
    }).collect();

    let current_sources = circuit.current_sources.iter().map(|i_src| {
        // Waveform cards carry the value at `t`; plain sources use their DC
        // value (n1→n2 through the source, by construction of the stamp).
        match t {
            Some(t) => i_src.value_at(t),
            None => i_src.current,
        }
    }).collect();

    let mut diodes = Vec::with_capacity(circuit.diodes.len());
    for d in &circuit.diodes {
        let model = circuit.diode_models.get(&d.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "diode '{}' references unknown model '{}'",
                d.name, d.model
            ))
        })?;
        let v_a = node_voltage(solution, d.anode);
        let v_c = node_voltage(solution, d.cathode);
        diodes.push(model.eval_for_at(v_a - v_c, d.area, circuit.temperature).i_d);
    }

    // MOSFET: re-evaluate Ids at the converged solution.
    let mut mosfets = Vec::with_capacity(circuit.mosfets.len());
    for m in &circuit.mosfets {
        let model = circuit.mosfet_models.get(&m.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "MOSFET '{}' references unknown model '{}'",
                m.name, m.model
            ))
        })?;
        let sign = if model.kind == MosfetType::Pmos { -1.0 } else { 1.0 };
        let vd = sign * node_voltage(solution, m.nd);
        let vg = sign * node_voltage(solution, m.ng);
        let vs = sign * node_voltage(solution, m.ns);
        let vb = sign * node_voltage(solution, m.nb);
        let beta = model.kp * (m.w / m.l);
        // Raw Vds here: reports the physical model current, as before.
        mosfets.push(sign * model.eval_at(beta, vg - vs, vd - vs, vb - vs, circuit.temperature).ids);
    }

    let mut bjts = Vec::with_capacity(circuit.bjts.len());
    for q in &circuit.bjts {
        let model = circuit.bjt_models.get(&q.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "BJT '{}' references unknown model '{}'",
                q.name, q.model
            ))
        })?;
        let vbe = node_voltage(solution, q.nb) - node_voltage(solution, q.ne);
        let vbc = node_voltage(solution, q.nb) - node_voltage(solution, q.nc);
        bjts.push(model.currents(vbe, vbc, q.area, circuit.temperature).ic);
    }

    let mut jfets = Vec::with_capacity(circuit.jfets.len());
    for j in &circuit.jfets {
        let model = circuit.jfet_models.get(&j.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "JFET '{}' references unknown model '{}'",
                j.name, j.model
            ))
        })?;
        let vgs = node_voltage(solution, j.ng) - node_voltage(solution, j.ns);
        let vgd = node_voltage(solution, j.ng) - node_voltage(solution, j.nd);
        jfets.push(model.currents(vgs, vgd, j.area, circuit.temperature).id);
    }

    // Behavioral sources: voltage-flavour cards read their branch unknown;
    // current-flavour cards re-evaluate `f(x)` at the converged point with no
    // pending step (dt = 0), so an `idt` reports what it has already
    // integrated. The report time matches the caller's `t` (waveforms sample
    // there; DC/op reports use t = 0).
    let mut behavioral = Vec::with_capacity(circuit.behavioral.len());
    let mut voltage_row = branch.behavioral_base;
    for b in &circuit.behavioral {
        match b.kind {
            BehavioralKind::Voltage => {
                behavioral.push(solution[voltage_row]);
                voltage_row += 1;
            }
            BehavioralKind::Current => {
                let snapshot = behavioral_probe_snapshot(circuit, b, solution)?;
                let runtime = BehavioralRuntime::default();
                let t_now = t.unwrap_or(0.0);
                behavioral.push(b.behavior.evaluate(&snapshot, t_now, 0.0, &runtime));
            }
        }
    }

    Ok(BranchCurrents {
        resistors,
        capacitors,
        inductors,
        voltage_sources,
        current_sources,
        diodes,
        mosfets,
        bjts,
        jfets,
        behavioral,
    })
}

/// Branch currents at one transient point: the real computation plus the
/// integrator-consistent capacitor currents from the accepted state.
pub fn compute_branch_currents_at(
    circuit: &Circuit,
    solution: &DVector<f64>,
    t: f64,
    state: &TransientState,
) -> Result<BranchCurrents> {
    let mut currents = compute_branch_currents(circuit, solution, None, None, Some(t))?;
    currents.capacitors = state.capacitor_currents.clone();
    currents.inductors = state.inductor_currents.clone();
    if currents.diodes.len() == state.diode_cap_currents.len() {
        for (total, displacement) in currents
            .diodes
            .iter_mut()
            .zip(&state.diode_cap_currents)
        {
            *total += *displacement;
        }
    }
    // Behavioral current-flavour cards report from the accepted integrator
    // state (dt = 0 collapses `idt` to the completed integral; `delay` reads
    // the accepted samples). Voltage-flavour cards read their branch rows,
    // which the base call already filled correctly.
    recompute_behavioral_current_reports(circuit, solution, t, state, &mut currents.behavioral);
    Ok(currents)
}

/// Overwrite only the current-flavour behavioral entries of a report with
/// values evaluated in the accepted state (dt = 0 → `idt` collapses to its
/// accumulated integral, `delay` interpolates the accepted samples).
fn recompute_behavioral_current_reports(
    circuit: &Circuit,
    solution: &DVector<f64>,
    t: f64,
    state: &TransientState,
    reports: &mut [f64],
) {
    for (i, b) in circuit.behavioral.iter().enumerate() {
        if b.kind != BehavioralKind::Current {
            continue;
        }
        let Some(runtime) = state.behavioral.get(i) else {
            continue;
        };
        let Ok(snapshot) = behavioral_probe_snapshot(circuit, b, solution) else {
            continue;
        };
        if let Some(slot) = reports.get_mut(i) {
            *slot = b.behavior.evaluate(&snapshot, t, 0.0, runtime);
        }
    }
}

/// State implied by a solved operating point / initial vector: voltages and
/// currents known, every history term zero (DC steady state).
pub fn seed_transient_state(circuit: &Circuit, solution: &DVector<f64>) -> TransientState {
    let branch = BranchLayout::for_circuit(circuit);
    let mut state = TransientState {
        capacitor_voltages: circuit
            .capacitors
            .iter()
            .map(|c| node_voltage(solution, c.n1) - node_voltage(solution, c.n2))
            .collect(),
        inductor_currents: circuit
            .inductors
            .iter()
            .enumerate()
            .map(|(i, _)| branch.ind_row(solution, i))
            .collect(),
        ..Default::default()
    };
    state.capacitor_currents = vec![0.0; circuit.capacitors.len()];
    state.inductor_voltages = vec![0.0; circuit.inductors.len()];
    state.mutual_voltages = vec![0.0; circuit.mutual_inductances.len()];
    state.diode_voltages = circuit
        .diodes
        .iter()
        .map(|d| node_voltage(solution, d.anode) - node_voltage(solution, d.cathode))
        .collect();
    state.diode_charges = circuit
        .diodes
        .iter()
        .map(|d| {
            let model = circuit.diode_models.get(&d.model);
            let v = node_voltage(solution, d.anode) - node_voltage(solution, d.cathode);
            model
                .map(|m| m.total_charge_at(v, d.area, circuit.temperature))
                .unwrap_or(0.0)
        })
        .collect();
    state.diode_cap_currents = vec![0.0; circuit.diodes.len()];
    state.mosfet_cap_voltages = circuit
        .mosfets
        .iter()
        .map(|m| {
            let nodes = mosfet_cap_nodes(m);
            let mut v = [0.0; 5];
            for k in 0..5 {
                v[k] = node_voltage(solution, nodes[k].0) - node_voltage(solution, nodes[k].1);
            }
            v
        })
        .collect();
    state.mosfet_cap_currents = vec![[0.0; 5]; circuit.mosfets.len()];
    state.bjt_cap_voltages = circuit
        .bjts
        .iter()
        .map(|q| {
            [
                node_voltage(solution, q.nb) - node_voltage(solution, q.ne),
                node_voltage(solution, q.nb) - node_voltage(solution, q.nc),
            ]
        })
        .collect();
    state.bjt_cap_currents = vec![[0.0; 2]; circuit.bjts.len()];
    state.jfet_cap_voltages = circuit
        .jfets
        .iter()
        .map(|j| {
            [
                node_voltage(solution, j.ng) - node_voltage(solution, j.ns),
                node_voltage(solution, j.ng) - node_voltage(solution, j.nd),
            ]
        })
        .collect();
    state.jfet_cap_currents = vec![[0.0; 2]; circuit.jfets.len()];
    state.switch_states = switch_states_after(circuit, solution, &[]);

    // Behavioral sources: sized runtime, `idt` previous values read from the
    // DC solution (so the first accepted step's trapezoid starts from the
    // real operating point), and every `delay` queue seeded with the arg's
    // DC value at t = 0 (delays then interpolate from the true initial
    // history instead of the zero-order hold).
    state.behavioral = circuit
        .behavioral
        .iter()
        .map(|b| {
            let mut runtime = BehavioralRuntime::sized_for(&b.behavior);
            let snapshot = behavioral_probe_snapshot(circuit, b, solution)
                .unwrap_or_else(|_| vec![0.0; b.behavior.probes.len()]);
            for (site, arg) in b.behavior.idt.iter().enumerate() {
                runtime.idt_prev[site] = b.behavior.eval_expr(arg, &snapshot, 0.0, 0.0, &runtime);
            }
            for (site, (arg, _td)) in b.behavior.delay.iter().enumerate() {
                let value = b.behavior.eval_expr(arg, &snapshot, 0.0, 0.0, &runtime);
                runtime.delay_queues[site].push_back((0.0, value));
            }
            runtime
        })
        .collect();
    state
}

/// Advance the integrator history after one accepted step ending at `t_end`.
pub fn advance_transient_state(
    circuit: &Circuit,
    solution: &DVector<f64>,
    previous: &TransientState,
    dt: f64,
    method: IntegrationMethod,
    t_end: f64,
) -> TransientState {
    let branch = BranchLayout::for_circuit(circuit);
    let capacitor_voltages: Vec<f64> = circuit
        .capacitors
        .iter()
        .map(|c| node_voltage(solution, c.n1) - node_voltage(solution, c.n2))
        .collect();
    let inductor_currents: Vec<f64> = circuit
        .inductors
        .iter()
        .enumerate()
        .map(|(i, _)| branch.ind_row(solution, i))
        .collect();
    let inductor_voltages: Vec<f64> = circuit
        .inductors
        .iter()
        .map(|l| node_voltage(solution, l.n1) - node_voltage(solution, l.n2))
        .collect();

    let capacitor_currents: Vec<f64> = circuit
        .capacitors
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let cap = circuit.capacitance_at(c);
            let v_new = capacitor_voltages[i];
            let v_prev = previous.capacitor_voltages.get(i).copied().unwrap_or(v_new);
            let i_prev = previous.capacitor_currents.get(i).copied().unwrap_or(0.0);
            let v_older = previous
                .capacitor_voltages_older
                .get(i)
                .copied()
                .unwrap_or(v_prev);
            match method {
                IntegrationMethod::Trapezoidal => {
                    (2.0 * cap / dt) * (v_new - v_prev) - i_prev
                }
                IntegrationMethod::Gear1 => cap / dt * (v_new - v_prev),
                IntegrationMethod::Gear2 => {
                    let (a0, a1, a2) = bdf2_coefficients(previous.last_dt, dt);
                    cap * (a0 * v_new + a1 * v_prev + a2 * v_older)
                }
            }
        })
        .collect();

    let diode_voltages: Vec<f64> = circuit
        .diodes
        .iter()
        .map(|d| node_voltage(solution, d.anode) - node_voltage(solution, d.cathode))
        .collect();
    let diode_charges: Vec<f64> = circuit
        .diodes
        .iter()
        .map(|d| {
            let model = circuit.diode_models.get(&d.model);
            let v = node_voltage(solution, d.anode) - node_voltage(solution, d.cathode);
            model
                .map(|m| m.total_charge_at(v, d.area, circuit.temperature))
                .unwrap_or(0.0)
        })
        .collect();
    let diode_cap_currents: Vec<f64> = (0..circuit.diodes.len())
        .map(|i| {
            let q = diode_charges[i];
            let q_prev = previous.diode_charges.get(i).copied().unwrap_or(q);
            let q_older = previous.diode_charges_older.get(i).copied().unwrap_or(q_prev);
            let i_prev = previous.diode_cap_currents.get(i).copied().unwrap_or(0.0);
            match method {
                IntegrationMethod::Trapezoidal => 2.0 * (q - q_prev) / dt - i_prev,
                IntegrationMethod::Gear1 => (q - q_prev) / dt,
                IntegrationMethod::Gear2 => {
                    let (a0, a1, a2) = bdf2_coefficients(previous.last_dt, dt);
                    a0 * q + a1 * q_prev + a2 * q_older
                }
            }
        })
        .collect();

    let mosfet_cap_voltages_new: Vec<[f64; 5]> = circuit
        .mosfets
        .iter()
        .map(|m| {
            let nodes = mosfet_cap_nodes(m);
            let mut v = [0.0; 5];
            for k in 0..5 {
                v[k] = node_voltage(solution, nodes[k].0) - node_voltage(solution, nodes[k].1);
            }
            v
        })
        .collect();
    let mosfet_cap_currents_new: Vec<[f64; 5]> = (0..circuit.mosfets.len())
        .map(|i| {
            let caps = mosfet_cap_values(circuit, &circuit.mosfets[i]);
            let mut currents = [0.0; 5];
            let new_v = mosfet_cap_voltages_new[i];
            let prev_v = previous
                .mosfet_cap_voltages
                .get(i)
                .copied()
                .unwrap_or(new_v);
            let older_v = previous
                .mosfet_cap_voltages_older
                .get(i)
                .copied()
                .unwrap_or(prev_v);
            let prev_i = previous
                .mosfet_cap_currents
                .get(i)
                .copied()
                .unwrap_or([0.0; 5]);
            for k in 0..5 {
                let cap = caps[k];
                if cap <= 0.0 {
                    continue;
                }
                currents[k] = match method {
                    IntegrationMethod::Trapezoidal => {
                        (2.0 * cap / dt) * (new_v[k] - prev_v[k]) - prev_i[k]
                    }
                    IntegrationMethod::Gear1 => cap / dt * (new_v[k] - prev_v[k]),
                    IntegrationMethod::Gear2 => {
                        let (a0, a1, a2) = bdf2_coefficients(previous.last_dt, dt);
                        cap * (a0 * new_v[k] + a1 * prev_v[k] + a2 * older_v[k])
                    }
                };
            }
            currents
        })
        .collect();

    let bjt_cap_voltages_new: Vec<[f64; 2]> = circuit
        .bjts
        .iter()
        .map(|q| {
            [
                node_voltage(solution, q.nb) - node_voltage(solution, q.ne),
                node_voltage(solution, q.nb) - node_voltage(solution, q.nc),
            ]
        })
        .collect();
    let bjt_cap_currents_new: Vec<[f64; 2]> = (0..circuit.bjts.len())
        .map(|i| {
            let q = &circuit.bjts[i];
            let caps = circuit
                .bjt_models
                .get(&q.model)
                .map(|m| {
                    [
                        m.cbe(
                            node_voltage(solution, q.nb) - node_voltage(solution, q.ne),
                            q.area,
                            circuit.temperature,
                        ),
                        m.cbc(
                            node_voltage(solution, q.nb) - node_voltage(solution, q.nc),
                            q.area,
                            circuit.temperature,
                        ),
                    ]
                })
                .unwrap_or([0.0; 2]);
            let new_v = bjt_cap_voltages_new[i];
            let prev_v = previous.bjt_cap_voltages.get(i).copied().unwrap_or(new_v);
            let older_v = previous
                .bjt_cap_voltages_older
                .get(i)
                .copied()
                .unwrap_or(prev_v);
            let prev_i = previous.bjt_cap_currents.get(i).copied().unwrap_or([0.0; 2]);
            let mut currents = [0.0; 2];
            for k in 0..2 {
                if caps[k] <= 0.0 {
                    continue;
                }
                currents[k] = match method {
                    IntegrationMethod::Trapezoidal => {
                        (2.0 * caps[k] / dt) * (new_v[k] - prev_v[k]) - prev_i[k]
                    }
                    IntegrationMethod::Gear1 => caps[k] / dt * (new_v[k] - prev_v[k]),
                    IntegrationMethod::Gear2 => {
                        let (a0, a1, a2) = bdf2_coefficients(previous.last_dt, dt);
                        caps[k] * (a0 * new_v[k] + a1 * prev_v[k] + a2 * older_v[k])
                    }
                };
            }
            currents
        })
        .collect();

    let jfet_cap_voltages_new: Vec<[f64; 2]> = circuit
        .jfets
        .iter()
        .map(|j| {
            [
                node_voltage(solution, j.ng) - node_voltage(solution, j.ns),
                node_voltage(solution, j.ng) - node_voltage(solution, j.nd),
            ]
        })
        .collect();
    let jfet_cap_currents_new: Vec<[f64; 2]> = (0..circuit.jfets.len())
        .map(|i| {
            let j = &circuit.jfets[i];
            let caps = circuit
                .jfet_models
                .get(&j.model)
                .map(|m| {
                    [
                        m.cgs_at(
                            node_voltage(solution, j.ng) - node_voltage(solution, j.ns),
                            j.area,
                        ),
                        m.cgd_at(
                            node_voltage(solution, j.ng) - node_voltage(solution, j.nd),
                            j.area,
                        ),
                    ]
                })
                .unwrap_or([0.0; 2]);
            let new_v = jfet_cap_voltages_new[i];
            let prev_v = previous.jfet_cap_voltages.get(i).copied().unwrap_or(new_v);
            let older_v = previous
                .jfet_cap_voltages_older
                .get(i)
                .copied()
                .unwrap_or(prev_v);
            let prev_i = previous.jfet_cap_currents.get(i).copied().unwrap_or([0.0; 2]);
            let mut currents = [0.0; 2];
            for k in 0..2 {
                if caps[k] <= 0.0 {
                    continue;
                }
                currents[k] = match method {
                    IntegrationMethod::Trapezoidal => {
                        (2.0 * caps[k] / dt) * (new_v[k] - prev_v[k]) - prev_i[k]
                    }
                    IntegrationMethod::Gear1 => caps[k] / dt * (new_v[k] - prev_v[k]),
                    IntegrationMethod::Gear2 => {
                        let (a0, a1, a2) = bdf2_coefficients(previous.last_dt, dt);
                        caps[k] * (a0 * new_v[k] + a1 * prev_v[k] + a2 * older_v[k])
                    }
                };
            }
            currents
        })
        .collect();

    let mut mutual_voltages = Vec::with_capacity(circuit.mutual_inductances.len());
    for k in &circuit.mutual_inductances {
        let (Some(i), Some(j)) = (
            inductor_index(circuit, &k.inductor1),
            inductor_index(circuit, &k.inductor2),
        ) else {
            mutual_voltages.push(0.0);
            continue;
        };
        let m = k.coupling
            * (circuit.inductance_at(&circuit.inductors[i])
                * circuit.inductance_at(&circuit.inductors[j]))
                .sqrt();
        let new_j = inductor_currents[j];
        let prev_j = previous.inductor_currents.get(j).copied().unwrap_or(new_j);
        let older_j = previous
            .inductor_currents_older
            .get(j)
            .copied()
            .unwrap_or(prev_j);
        let previous_mutual = {
            // The history slot is indexed by the K card; recover it below.
            previous.mutual_voltages.get(mutual_voltages.len()).copied().unwrap_or(0.0)
        };
        let v = match method {
            IntegrationMethod::Trapezoidal => {
                (2.0 * m / dt) * (new_j - prev_j) - previous_mutual
            }
            IntegrationMethod::Gear1 => (m / dt) * (new_j - prev_j),
            IntegrationMethod::Gear2 => {
                let (a0, a1, a2) = bdf2_coefficients(previous.last_dt, dt);
                m * (a0 * new_j + a1 * prev_j + a2 * older_j)
            }
        };
        mutual_voltages.push(v);
    }

    // Behavioral sources: each `idt` site completes one trapezoid over the
    // just-accepted step (arg evaluated at the *converged* end-of-step
    // values); each `delay` site records its accepted sample. Sites are
    // walked in parse order and the inflight runtime is updated as we go, so
    // a nested `idt` evaluation reads the just-completed accumulator (dt = 0
    // in the inner evaluation collapses the inner node to exactly that).
    let behavioral: Vec<BehavioralRuntime> = circuit
        .behavioral
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let mut runtime = previous
                .behavioral
                .get(i)
                .cloned()
                .unwrap_or_else(|| BehavioralRuntime::sized_for(&b.behavior));
            let snapshot =
                behavioral_probe_snapshot(circuit, b, solution)
                    .unwrap_or_else(|_| vec![0.0; b.behavior.probes.len()]);
            for (site, arg) in b.behavior.idt.iter().enumerate() {
                // dt = 0 on the argument evaluation: a nested `idt` site
                // collapses to its (already updated) accumulator — the
                // step-end value of the inner integral, which is exactly the
                // outer site's argument value at `t_end`.
                let now = b.behavior.eval_expr(arg, &snapshot, t_end, 0.0, &runtime);
                let prev = runtime.idt_prev.get(site).copied().unwrap_or(now);
                runtime.idt_acc[site] =
                    runtime.idt_acc.get(site).copied().unwrap_or(0.0) + 0.5 * dt * (now + prev);
                runtime.idt_prev[site] = now;
            }
            for (site, (arg, _td)) in b.behavior.delay.iter().enumerate() {
                let value = b.behavior.eval_expr(arg, &snapshot, t_end, 0.0, &runtime);
                let queue = &mut runtime.delay_queues[site];
                queue.push_back((t_end, value));
                while queue.len() > crate::behavior::MAX_DELAY_SAMPLES {
                    queue.pop_front();
                }
            }
            runtime
        })
        .collect();

    TransientState {
        capacitor_voltages,
        capacitor_currents,
        capacitor_voltages_older: previous.capacitor_voltages.clone(),
        inductor_currents,
        inductor_voltages,
        inductor_currents_older: previous.inductor_currents.clone(),
        mutual_voltages,
        diode_voltages,
        diode_charges,
        diode_charges_older: previous.diode_charges.clone(),
        diode_cap_currents,
        mosfet_cap_voltages: mosfet_cap_voltages_new,
        mosfet_cap_voltages_older: previous.mosfet_cap_voltages.clone(),
        mosfet_cap_currents: mosfet_cap_currents_new,
        bjt_cap_voltages: bjt_cap_voltages_new,
        bjt_cap_voltages_older: previous.bjt_cap_voltages.clone(),
        bjt_cap_currents: bjt_cap_currents_new,
        jfet_cap_voltages: jfet_cap_voltages_new,
        jfet_cap_voltages_older: previous.jfet_cap_voltages.clone(),
        jfet_cap_currents: jfet_cap_currents_new,
        switch_states: switch_states_after(circuit, solution, &previous.switch_states),
        last_dt: dt,
        behavioral,
    }
}

/// Settle every switch state after a solve, using the previous state for
/// hysteresis.
fn switch_states_after(
    circuit: &Circuit,
    solution: &DVector<f64>,
    previous: &[Option<bool>],
) -> Vec<Option<bool>> {
    let branch = BranchLayout::for_circuit(circuit);
    let mut switch_states = Vec::with_capacity(circuit.switches.len());
    for (i, s) in circuit.switches.iter().enumerate() {
        let control = match &s.control {
            SwitchControl::Voltage(p, n) => {
                node_voltage(solution, *p) - node_voltage(solution, *n)
            }
            SwitchControl::Current(name) => match voltage_source_index(circuit, name) {
                Some(idx) => branch.src_row(solution, idx),
                None => 0.0,
            },
        };
        let was_on = previous.get(i).copied().flatten().or(s.initial);
        let state_now = circuit
            .switch_models
            .get(&s.model)
            .map(|m| m.conductance(control, was_on).1)
            .unwrap_or(false);
        switch_states.push(Some(state_now));
    }
    switch_states
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
    pub current_sources: Vec<Complex64>,
    pub diodes: Vec<Complex64>,
    /// Small-signal drain current phasor for each MOSFET: Id = gm·Vgs + gds·Vds + gbs·Vbs
    pub mosfets: Vec<Complex64>,
    /// Small-signal collector current phasor for each BJT.
    pub bjts: Vec<Complex64>,
    /// Small-signal drain current phasor for each JFET.
    pub jfets: Vec<Complex64>,
    /// Small-signal output of each behavioral source. Voltage flavour: the
    /// complex branch unknown; current flavour: `Σ dk·(probe phasor)` with the
    /// real-difference Jacobian evaluated at the DC bias.
    pub behavioral: Vec<Complex64>,
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
) -> Result<AcBranchCurrents> {
    let omega = 2.0 * std::f64::consts::PI * freq;
    let branch = BranchLayout::for_circuit(circuit);
    let one = Complex64::new(1.0, 0.0);

    // ── Resistors: I = (V_n1 - V_n2) / R ────────────────────────────────
    let resistors = circuit.resistors.iter().map(|r| {
        let v1 = node_voltage(solution, r.n1);
        let v2 = node_voltage(solution, r.n2);
        (v1 - v2) * (one / circuit.resistance_at(r))
    }).collect();

    // ── Capacitors: I = jωC * (V_n1 - V_n2) ─────────────────────────────
    let capacitors = circuit.capacitors.iter().map(|c| {
        let v1 = node_voltage(solution, c.n1);
        let v2 = node_voltage(solution, c.n2);
        Complex64::new(0.0, omega * circuit.capacitance_at(c)) * (v1 - v2)
    }).collect();

    // ── Inductors: branch unknown in solution vector ──────────────────────
    let inductors = circuit.inductors.iter().enumerate().map(|(i, _)| {
        branch.ind_row(solution, i)
    }).collect();

    // ── Voltage sources: branch unknown in solution vector ────────────────
    // Negated to match n1→n2 passive convention (DC path note).
    // The branch unknown is the element-convention current n1 → n2.
    let voltage_sources = circuit.voltage_sources.iter().enumerate().map(|(i, _)| {
        branch.src_row(solution, i)
    }).collect();

    // ── Current sources: the injected phasor itself ───────────────────────
    // Same `mag∠phase` the AC stamp injects; zero when the card has no AC
    // magnitude (an open circuit in the small-signal model).
    let current_sources = circuit.current_sources.iter().map(|i_src| {
        i_src.ac.as_ref().map(|ac| {
            Complex64::from_polar(ac.mag, ac.phase.to_radians())
        }).unwrap_or_default()
    }).collect();

    // ── Diodes: linearised small-signal conductance gd * (V_a - V_c) ─────
    let mut diodes = Vec::with_capacity(circuit.diodes.len());
    for d in &circuit.diodes {
        let model = circuit.diode_models.get(&d.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "diode '{}' references unknown model '{}'",
                d.name, d.model
            ))
        })?;
        let gd = model
            .eval_for_at(
                node_voltage(dc_solution, d.anode) - node_voltage(dc_solution, d.cathode),
                d.area,
                circuit.temperature,
            )
            .gd;
        let va_ac = node_voltage(solution, d.anode);
        let vc_ac = node_voltage(solution, d.cathode);
        // Conduction plus junction/transit-time displacement current.
        let vd_bias = node_voltage(dc_solution, d.anode) - node_voltage(dc_solution, d.cathode);
        let cap = model.junction_capacitance(vd_bias, d.area)
            + model.tt * model.eval_for_at(vd_bias, d.area, circuit.temperature).gd;
        let y = Complex64::new(gd, omega * cap);
        diodes.push(y * (va_ac - vc_ac));
    }

    // ── MOSFETs: small-signal drain current phasor ────────────────────────
    //
    // Id = gm·Vgs + gds·Vds + gbs·Vbs   (all conductances real at level-1)
    //
    // Conductances come from model.rs's single copy of the level-1 equations,
    // linearised at the DC bias point, so this stays self-contained.
    let mut mosfets = Vec::with_capacity(circuit.mosfets.len());
    for m in &circuit.mosfets {
        let model = circuit.mosfet_models.get(&m.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "MOSFET '{}' references unknown model '{}'",
                m.name, m.model
            ))
        })?;

        let sign = if model.kind == MosfetType::Pmos { -1.0 } else { 1.0 };

        // DC bias voltages (real) — mirroring as in build_system's AC arm.
        let vd_dc = sign * node_voltage(dc_solution, m.nd);
        let vg_dc = sign * node_voltage(dc_solution, m.ng);
        let vs_dc = sign * node_voltage(dc_solution, m.ns);
        let vb_dc = sign * node_voltage(dc_solution, m.nb);

        let beta = model.kp * (m.w / m.l);
        let ev = model.eval_at(beta, vg_dc - vs_dc, vd_dc - vs_dc, vb_dc - vs_dc, circuit.temperature);
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

        // Id = gm·Vgs + gds·Vds + gbs·Vbs in real-node space (the mirrored
        // bias only changes the coefficients, not their sign).
        mosfets.push(
            Complex64::new(gm, 0.0) * vgs_ac
                + Complex64::new(gds, 0.0) * vds_ac
                + Complex64::new(gbs, 0.0) * vbs_ac,
        );
    }

    let mut bjts = Vec::with_capacity(circuit.bjts.len());
    for q in &circuit.bjts {
        let model = circuit.bjt_models.get(&q.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "BJT '{}' references unknown model '{}'",
                q.name, q.model
            ))
        })?;
        let vbe_dc = node_voltage(dc_solution, q.nb) - node_voltage(dc_solution, q.ne);
        let vbc_dc = node_voltage(dc_solution, q.nb) - node_voltage(dc_solution, q.nc);
        let h = 1e-6;
        let pbe = model.currents(vbe_dc + h, vbc_dc, q.area, circuit.temperature).ic;
        let mbe = model.currents(vbe_dc - h, vbc_dc, q.area, circuit.temperature).ic;
        let pbc = model.currents(vbe_dc, vbc_dc + h, q.area, circuit.temperature).ic;
        let mbc = model.currents(vbe_dc, vbc_dc - h, q.area, circuit.temperature).ic;
        let dbe = (pbe - mbe) / (2.0 * h);
        let dbc = (pbc - mbc) / (2.0 * h);
        let vbe_ac = node_voltage(solution, q.nb) - node_voltage(solution, q.ne);
        let vbc_ac = node_voltage(solution, q.nb) - node_voltage(solution, q.nc);
        bjts.push(Complex64::new(dbe, 0.0) * vbe_ac + Complex64::new(dbc, 0.0) * vbc_ac);
    }

    let mut jfets = Vec::with_capacity(circuit.jfets.len());
    for j in &circuit.jfets {
        let model = circuit.jfet_models.get(&j.model).ok_or_else(|| {
            SimError::Circuit(format!(
                "JFET '{}' references unknown model '{}'",
                j.name, j.model
            ))
        })?;
        let vgs_dc = node_voltage(dc_solution, j.ng) - node_voltage(dc_solution, j.ns);
        let vgd_dc = node_voltage(dc_solution, j.ng) - node_voltage(dc_solution, j.nd);
        let h = 1e-6;
        let pgs = model.currents(vgs_dc + h, vgd_dc, j.area, circuit.temperature).id;
        let mgs = model.currents(vgs_dc - h, vgd_dc, j.area, circuit.temperature).id;
        let pgd = model.currents(vgs_dc, vgd_dc + h, j.area, circuit.temperature).id;
        let mgd = model.currents(vgs_dc, vgd_dc - h, j.area, circuit.temperature).id;
        let dgs = (pgs - mgs) / (2.0 * h);
        let dgd = (pgd - mgd) / (2.0 * h);
        let vgs_ac = node_voltage(solution, j.ng) - node_voltage(solution, j.ns);
        let vgd_ac = node_voltage(solution, j.ng) - node_voltage(solution, j.nd);
        jfets.push(Complex64::new(dgs, 0.0) * vgs_ac + Complex64::new(dgd, 0.0) * vgd_ac);
    }

    let mut behavioral = Vec::with_capacity(circuit.behavioral.len());
    let mut voltage_row = branch.behavioral_base;
    for b in &circuit.behavioral {
        match b.kind {
            BehavioralKind::Voltage => {
                behavioral.push(solution[voltage_row]);
                voltage_row += 1;
            }
            BehavioralKind::Current => {
                // `i = c0 + Σ dk·probe` at the bias; the small-signal part is
                // the coefficient-weighted sum of the probe *phasors*. The
                // coefficients are real (central differences on the DC bias),
                // lifted here.
                let snapshot = behavioral_probe_snapshot(circuit, b, dc_solution)?;
                let lin = behavioral_probe_lin(b, &snapshot, 0.0, 0.0, &BehavioralRuntime::default())?;
                let mut phasor = Complex64::new(0.0, 0.0);
                for (probe, &dk) in b.behavior.probes.iter().zip(&lin.dk) {
                    if dk == 0.0 {
                        continue;
                    }
                    let dv = node_voltage_complex(probe, solution, &branch);
                    phasor += Complex64::new(dk, 0.0) * dv;
                }
                behavioral.push(phasor);
            }
        }
    }

    Ok(AcBranchCurrents {
        resistors,
        capacitors,
        inductors,
        voltage_sources,
        current_sources,
        diodes,
        mosfets,
        bjts,
        jfets,
        behavioral,
    })
}

/// The complex small-signal value of one bound probe (a node-pair phasor or a
/// branch-unknown phasor).
fn node_voltage_complex(
    probe: &crate::behavior::ProbeBinding,
    solution: &DVector<Complex64>,
    branch: &BranchLayout,
) -> Complex64 {
    match probe {
        crate::behavior::ProbeBinding::Voltage(p, m) => {
            node_voltage(solution, *p) - node_voltage(solution, *m)
        }
        crate::behavior::ProbeBinding::SourceCurrent(i) => solution[branch.src_branch(*i)],
        crate::behavior::ProbeBinding::InductorCurrent(i) => solution[branch.ind_branch(*i)],
        crate::behavior::ProbeBinding::Unresolved(_) => Complex64::new(0.0, 0.0),
    }
}

// ─── Operating-point seed ────────────────────────────────────────────────────

/// Build a reasonable initial guess for the Newton-Raphson iteration.
///
/// Stamps only the linear elements (resistors + voltage sources) plus a small
/// conductance stub for every nonlinear device, then solves once. This gives a
/// voltage profile close to the final answer without any nonlinear maths, and
/// avoids cut-off initialisation issues with MOSFETs.
///
/// If the matrix is still singular (e.g. an all-floating island), the guess
/// falls back to zero rather than failing.
pub fn initial_guess(circuit: &Circuit) -> DVector<f64> {
    let mut sys = MnaSystem::<f64>::for_circuit(circuit);

    // Resistors.
    for r in &circuit.resistors {
        stamp_admittance(&mut sys, r.n1, r.n2, 1.0 / circuit.resistance_at(r));
    }

    // Voltage sources at their DC value.
    for (i, v) in circuit.voltage_sources.iter().enumerate() {
        let branch = sys.branch.src_branch(i);
        if let Some(i1) = node_index(v.n1) {
            sys.a[(i1, branch)] += 1.0;
            sys.a[(branch, i1)] += 1.0;
        }
        if let Some(i2) = node_index(v.n2) {
            sys.a[(i2, branch)] -= 1.0;
            sys.a[(branch, i2)] -= 1.0;
        }
        sys.z[branch] = v.voltage;
    }

    // Inductors as 0 V shorts (same as the DC mode).
    for (i, l) in circuit.inductors.iter().enumerate() {
        let branch = sys.branch.ind_branch(i);
        if let Some(i1) = node_index(l.n1) {
            sys.a[(i1, branch)] += 1.0;
            sys.a[(branch, i1)] += 1.0;
        }
        if let Some(i2) = node_index(l.n2) {
            sys.a[(i2, branch)] -= 1.0;
            sys.a[(branch, i2)] -= 1.0;
        }
    }

    // Diodes and MOSFETs: small-conductance stub so every node has a path.
    let g_stub = 1e-6;
    for d in &circuit.diodes {
        stamp_admittance(&mut sys, d.anode, d.cathode, g_stub);
    }
    for m in &circuit.mosfets {
        // Stub between drain and source keeps the drain node from floating.
        stamp_admittance(&mut sys, m.nd, m.ns, g_stub);
    }
    for j in &circuit.jfets {
        stamp_admittance(&mut sys, j.nd, j.ns, g_stub);
    }

    // Current source DC values make the guess match the driven problem.
    for i in &circuit.current_sources {
        stamp_current_source(&mut sys, i.n1, i.n2, i.current);
    }

    // Linear controlled sources and switches keep their branch rows out of
    // the singular-matrix corner (a VCVS output with no other path would
    // otherwise leave its row empty in the seed solve).
    stamp_controlled_sources(circuit, &mut sys);
    let zero = DVector::zeros(sys.size);
    let _ = stamp_switches(circuit, &mut sys, &zero, &[]);
    // Behavioral sources: their rows/coefficients need a converged point to
    // linearize at, which the seed solve does not have; stamping at the zero
    // snapshot keeps the branch rows coupled. Errors here are fine — the
    // caller falls back to a zeroed vector anyway.
    let zero_probes = DVector::zeros(sys.size);
    let _ = stamp_behavioral(circuit, &mut sys, &zero_probes, true, 0.0, 0.0, &[]);

    match sys.solve() {
        Ok(x0) => x0,
        Err(_) => DVector::zeros(sys.size),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    /// TODO §3 limiter pins, mirrors of the Python reference that fixes the
    /// exact float op order (`BOLTZMANN·(273.15 + T)/q`, then the same
    /// `ln`/compare sequence as [`pnjlim`]). Every literal is the
    /// full-precision double the mirror produced; the tolerances only guard
    /// against a differently rounded `ln` in some libm, not against logic
    /// drift.
    const VT: f64 = 0.025864925786328753;

    /// `vcrit = vt·ln(vt/(√2·IS))` for IS = 1e-14: the knee voltage 15-digit
    /// mirror pinned, plus the knee less degenerate cases.
    #[test]
    fn critical_voltage_is_the_sqrt2_knee() {
        assert_relative_eq!(
            critical_voltage(VT, 1e-14),
            0.7302897202472336,
            max_relative = 1e-12
        );
        // Higher saturation current → lower knee (the exponential is steeper
        // earlier), and a nonpositive IS has no exponential to protect at all.
        assert!(critical_voltage(VT, 1e-13) < critical_voltage(VT, 1e-14));
        assert_eq!(critical_voltage(VT, 0.0), f64::INFINITY);
        assert_eq!(critical_voltage(VT, -1.0), f64::INFINITY);
    }

    /// Biases inside/at the knee window pass through untouched: the limiter
    /// exists for sawtooth excursions, not for a slow honest climb.
    #[test]
    fn pnjlim_passes_innocent_biases_through() {
        let vcrit = critical_voltage(VT, 1e-14);
        // A fresh iterate on a reverse-biased-looking start (v_old = 0):
        // still below the knee.
        assert_relative_eq!(pnjlim(0.5, 0.0, VT, 0.5), 0.5, epsilon = 0.0);
        // A 0.02 V climb at a forward point — well inside the 2·vt window.
        assert_relative_eq!(pnjlim(0.72, 0.7, VT, vcrit), 0.72, epsilon = 0.0);
        // v_new == vcrit is not *past* the knee (the firing test is strict).
        assert_relative_eq!(pnjlim(vcrit, 0.0, VT, vcrit), vcrit, epsilon = 0.0);
    }

    /// Forward excursions past the knee: at the exact 2·vt boundary the
    /// subtract-one-add-one dance rounds the difference a hair *over*
    /// (0.051729851572657526 vs 2·vt = 0.051729851572657505), so the limiter
    /// fires and compresses the step to `v_old + vt·ln(3)`; a big jump walks
    /// the log all the way to `v_old + vt·ln(50.16…)`. Both literals come
    /// from the mirror.
    #[test]
    fn pnjlim_compresses_forward_excursions_logarithmically() {
        let vcrit = critical_voltage(VT, 1e-14);
        assert_relative_eq!(
            pnjlim(0.7 + 2.0 * VT, 0.7, VT, vcrit),
            0.7284155253143494,
            max_relative = 1e-12
        );
        assert_relative_eq!(pnjlim(2.0, 0.7, VT, vcrit), 0.801828466044731,
                            max_relative = 1e-12);
    }

    /// From a reversed or zero linearization point the limiter snaps to the
    /// knee — and the backward-past-a-forward-point case is exactly the
    /// `ln(negative)` = NaN hazard that the `arg > 0` guard (mirroring
    /// ngspice DEVpnjlim) closes: 0.97 from 1.2 is a 0.23 swing *backward*
    /// that still leaves the bias past the knee, so without the guard the
    /// log branch would produce a NaN iterate.
    #[test]
    fn pnjlim_snaps_to_the_knee_from_reversed_starts() {
        assert_relative_eq!(pnjlim(5.0, -0.5, VT, 0.5), 0.5, epsilon = 0.0);
        assert_relative_eq!(pnjlim(5.0, 0.0, VT, 0.5), 0.5, epsilon = 0.0);
        // The NaN-guard regression: a backward excursion beyond 2·vt from a
        // forward point must take the vcrit fallback, not `ln(−7.89…)`.
        assert_relative_eq!(pnjlim(0.97, 1.2, VT, 0.5), 0.5, epsilon = 0.0);
    }

    /// The junction-bias memory is sized per device *before* the first build
    /// runs: an empty default table would no-op every limit recording and
    /// strand `pnjlim`'s `v_old` on the (wrong) seed value every other build
    /// — the period-6 limit cycling the hard forward diode decks used to
    /// show. `zeroed` is sized from the parsed circuit; `seed` mirrors that
    /// shape.
    #[test]
    fn dc_junction_limit_tables_match_the_element_lists() {
        let circuit = crate::netlist::parse(
            "* limiter table shapes\n\
             .model dm D(IS=1e-14)\n\
             .model qm NPN(IS=1e-14)\n\
             .model jm NJF(VTO=-2 BETA=1e-4 IS=1e-14)\n\
             D1 a b dm\n\
             Q1 c 0 e qm\n\
             J1 d g s jm\n\
             .op\n\
             .end\n",
        )
        .unwrap()
        .circuit;
        assert_eq!(circuit.diodes.len(), 1);
        assert_eq!(circuit.bjts.len(), 1);
        assert_eq!(circuit.jfets.len(), 1);

        let zeroed = DcJunctionLimits::zeroed(&circuit);
        assert_eq!(zeroed.diode.len(), circuit.diodes.len());
        assert_eq!(zeroed.bjt.len(), circuit.bjts.len());
        assert_eq!(zeroed.jfet.len(), circuit.jfets.len());
        assert!(zeroed.diode.iter().all(|v| *v == 0.0));
        assert!(zeroed.bjt.iter().all(|v| *v == [0.0, 0.0]));
        assert!(zeroed.jfet.iter().all(|v| *v == [0.0, 0.0]));

        // The seed reads its junction biases off the same one-slot-per-device
        // layout, so a build-1 `used` and a build-2 `prev` always line up.
        let guess = nalgebra::DVector::zeros(circuit.nodes);
        let seeded = DcJunctionLimits::seed(&circuit, &guess);
        assert_eq!(seeded.diode.len(), circuit.diodes.len());
        assert_eq!(seeded.bjt.len(), circuit.bjts.len());
        assert_eq!(seeded.jfet.len(), circuit.jfets.len());
    }
}
