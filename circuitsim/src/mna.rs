//! MNA — one stamping module for DC, transient, and AC analysis.
//!
//! The unknown vector layout is owned by [`BranchLayout`]; a system is
//! [`MnaSystem<T>`] over a field: `f64` for `.op` / `.tran`,
//! [`Complex64`] for `.ac`. The single `build_system` constructor covers all
//! three analyses through a [`Mode`]; the stimulus patch (which source to
//! drive, at what amplitude) is [`MnaSystem::drive_source`], called from the
//! analysis driver.

use nalgebra::{DMatrix, DVector};
use num_complex::Complex64;

use crate::behavior::BehavioralRuntime;
use crate::circuit::{
    BehavioralKind, BjtType, Circuit, ControlVar, CurrentSource, IntegrationMethod,
    JfetType, MosfetType, NodeId, SwitchControl, VoltageSource,
};
use crate::error::{Result, SimError};
use crate::model::thermal_voltage;
use crate::sparse::StampBits;

pub use crate::currents::{
    compute_ac_branch_currents, compute_branch_currents, compute_branch_currents_at, initial_guess,
    BranchCurrents,
};

/// Model lookup shared by every stamp and current-extraction loop: the one
/// canonical `ok_or` for a device card whose model name does not resolve.
///
/// [`Circuit::validate`] runs the same cross-references before any solve
/// (at parse time and again per analysis pass), so decks that reach the
/// stamps through `analysis::run` can never fail here — the loud error
/// stays for direct matrix users and keeps the message in exactly one
/// place.
pub(crate) fn device_model<'a, M>(
    models: &'a std::collections::HashMap<String, M>,
    kind: &str,
    device: &str,
    model: &str,
) -> Result<&'a M> {
    models.get(model).ok_or_else(|| {
        SimError::Circuit(format!(
            "{kind} '{}' references unknown model '{}'",
            device, model
        ))
    })
}

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
    /// Lossless-line method-of-characteristics history, one entry per `T`
    /// card: the accepted `(t, v, i)` samples of ports A and B
    /// ([`TlineHistory`]), seeded from the DC operating point.
    pub tline_histories: Vec<TlineHistory>,
    /// Last settled state of every switch (`S`/`W`), for hysteresis.
    pub switch_states: Vec<Option<bool>>,
    /// Step size of the last accepted step (0 before the first step).
    pub last_dt: f64,
    /// Per-source runtime state of the behavioral sources (`B`): `idt`
    /// integrators and `delay` histories, one each, in card order.
    pub behavioral: Vec<BehavioralRuntime>,
}

/// One method-of-characteristics port sample: the accepted time, the port
/// voltage (terminal pair difference), and the port current into the element.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TlineSample {
    pub t: f64,
    pub v: f64,
    pub i: f64,
}

/// Method-of-characteristics history of one lossless line: port A's and
/// port B's accepted samples, pushed by [`advance_transient_state`] after
/// every step and queried at `t − TD` by the port stamps.
#[derive(Debug, Clone, Default)]
pub struct TlineHistory {
    /// Port A = the `n1`/`n2` pair.
    pub a: Vec<TlineSample>,
    /// Port B = the `n3`/`n4` pair.
    pub b: Vec<TlineSample>,
}

/// Linear interpolation of one port's history at `at`, clamping at both
/// ends (the oldest sample answers queries in the seeded past, the newest
/// answers queries past the last accepted step — a delay shorter than the
/// current step).
pub(crate) fn tline_port_sample(ring: &[TlineSample], at: f64) -> TlineSample {
    let Some(first) = ring.first().copied() else {
        return TlineSample {
            t: at,
            v: 0.0,
            i: 0.0,
        };
    };
    let Some(last) = ring.last().copied() else {
        unreachable!()
    };
    if at >= last.t {
        return last;
    }
    if at <= first.t {
        return first;
    }
    let hi = ring.partition_point(|s| s.t < at);
    let lo = &ring[hi - 1];
    let span = ring[hi].t - lo.t;
    if span <= 0.0 {
        return *lo;
    }
    let w = (at - lo.t) / span;
    TlineSample {
        t: at,
        v: lo.v + w * (ring[hi].v - lo.v),
        i: lo.i + w * (ring[hi].i - lo.i),
    }
}

/// Append an accepted port sample and prune samples no future query can
/// reach: every upcoming query happens at a step's end minus TD, strictly
/// after `horizon`, so only the last sample at/before `horizon` must stay
/// on as that query's left interpolation bracket.
fn push_tline_sample(ring: &mut Vec<TlineSample>, sample: TlineSample, horizon: f64) {
    ring.push(sample);
    if sample.t <= horizon {
        return;
    }
    let cut = ring.partition_point(|s| s.t <= horizon);
    let keep_from = cut.saturating_sub(1);
    if keep_from > 0 {
        ring.drain(..keep_from);
    }
}

/// The lossless two-port's trigonometric pair `(sinθ, cosθ)` at `θ = ω·TD`
/// with the resonance clamp applied: `sinθ = 0` (integer half-wavelengths)
/// makes the exact admittances unbounded, so they bound at |sin| = 1e-12
/// instead of dividing by zero (a documented interpretation — stamp and
/// extraction call this one function, so they can never drift apart).
pub(crate) fn tline_ac_angles(theta: f64) -> (f64, f64) {
    let s = theta.sin();
    let s = if s.abs() < 1e-12 {
        if s < 0.0 {
            -1e-12
        } else {
            1e-12
        }
    } else {
        s
    };
    (s, theta.cos())
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

// ─── The integrator's companion law (one owner) ─────────────────────────────

/// The step coefficients of the selected integration method: what multiplies
/// the newest state (`alpha`), the previous two states (`beta1`, `beta2`),
/// and the previous derivative (`gamma` — the trapezoidal memory the Gear
/// methods don't carry).
///
/// Every device companion is this one law applied to its own capacity term:
/// linear capacitors, inductors, mutual coupling, and every junction
/// capacitance — the stamp model *and* the accepted-step report. The
/// per-family `match` towers this replaces each re-derived the arithmetic,
/// which is how the families were free to drift apart.
struct Companion {
    alpha: f64,
    beta1: f64,
    beta2: f64,
    gamma: f64,
}

impl Companion {
    /// Coefficients for one step of `method` (BDF2 adapts to the step
    /// change; the others look only at `dt`).
    fn for_step(method: IntegrationMethod, dt: f64, last_dt: f64) -> Self {
        match method {
            IntegrationMethod::Trapezoidal => Self {
                alpha: 2.0 / dt,
                beta1: -2.0 / dt,
                beta2: 0.0,
                gamma: 1.0,
            },
            IntegrationMethod::Gear1 => Self {
                alpha: 1.0 / dt,
                beta1: -1.0 / dt,
                beta2: 0.0,
                gamma: 0.0,
            },
            IntegrationMethod::Gear2 => {
                let (a0, a1, a2) = bdf2_coefficients(last_dt, dt);
                Self { alpha: a0, beta1: a1, beta2: a2, gamma: 0.0 }
            }
        }
    }

    /// The history law: `k·d(state)/dt` per the method, `history` the
    /// trapezoid's previous derivative. One evaluation of this is both the
    /// accepted-step report and the anchor the companion models linearize
    /// ("iterate against the stored history").
    fn derivative(&self, k: f64, new: f64, prev: f64, older: f64, history: f64) -> f64 {
        k * (self.alpha * new + self.beta1 * prev + self.beta2 * older) - self.gamma * history
    }

    /// Voltage-state capacitor companion for capacity `k`: admittance `g`
    /// plus the injected current `i_eq` (`n2 → n1`), anchored at the
    /// previous state, so the solved branch sees `g·v − i_eq` == this law.
    /// All the junction caps (BJT/MOSFET/JFET) are the same evaluation.
    fn capacitor(&self, k: f64, prev: f64, older: f64, history: f64) -> (f64, f64) {
        let g = self.alpha * k;
        (g, g * prev - self.derivative(k, prev, prev, older, history))
    }

    /// Charge-state capacitor companion (the diode's junction + transit
    /// charge): the law runs on the stored charge `q(v)`, linearized at the
    /// guess `v` (`dqdv = dq/dv` there).
    fn charge_capacitor(
        &self,
        dqdv: f64,
        v: f64,
        q: f64,
        q_prev: f64,
        q_older: f64,
        i_prev: f64,
    ) -> (f64, f64) {
        let g = self.alpha * dqdv;
        (g, g * v - self.derivative(1.0, q, q_prev, q_older, i_prev))
    }

    /// The dual (inductor-side) companion: companion resistance `r_eq` and
    /// history voltage `v_eq` for the branch row. Mutual coupling reuses it
    /// with the partner's current history in the state slots and its own
    /// `v_mutual` as the history derivative.
    fn dual(&self, m: f64, prev: f64, older: f64, history: f64) -> (f64, f64) {
        (self.alpha * m, self.derivative(m, 0.0, prev, older, history))
    }
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
    /// First row of the op-amp (`U`) branch block.
    pub opamp_base: usize,
}

impl BranchLayout {
    pub fn for_circuit(circuit: &Circuit) -> Self {
        let [vsrc, ind, vcvs, ccvs, behavioral, _opamp] = circuit.branch_family_counts();
        let n_nodes = circuit.nodes.saturating_sub(1);
        let vsrc_base = n_nodes;
        let ind_base = vsrc_base + vsrc;
        let vcvs_base = ind_base + ind;
        let ccvs_base = vcvs_base + vcvs;
        let behavioral_base = ccvs_base + ccvs;
        let opamp_base = behavioral_base + behavioral;
        Self {
            n_nodes,
            vsrc_base,
            ind_base,
            vcvs_base,
            ccvs_base,
            behavioral_base,
            opamp_base,
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

    /// Row index of the output current unknown of VCVS `i`.
    pub fn vcvs_branch(&self, i: usize) -> usize {
        self.vcvs_base + i
    }

    /// Row index of the output current unknown of CCVS `i`.
    pub fn ccvs_branch(&self, i: usize) -> usize {
        self.ccvs_base + i
    }

    /// Row index of the output current unknown of op-amp `i`.
    pub fn opamp_branch(&self, i: usize) -> usize {
        self.opamp_base + i
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

/// Dense-kernel crossover for [`MnaSystem::solve`]: systems smaller than
/// this skip the sparse attempt — planning and workspace setup outweigh a
/// dense LU that costs ~n³/3 flops at these sizes. Measured A/B: the
/// benchmark's tiny decks (n ≤ 9) lost ~30% wall to the sparse overhead
/// while the 200-node ladder won ~5×; pooling the factor workspace inside
/// the plan pool is the follow-up that would retire this gate.
const SMALL_SYSTEM: usize = 16;

pub struct MnaSystem<T: Field> {
    pub(crate) size: usize,
    pub(crate) a: DMatrix<T>,
    pub(crate) z: DVector<T>,
    /// Structural record of this build: every `a` slot any stamp wrote into
    /// (via [`Self::stamp_add`] / [`Self::stamp_sub`]). A *superset* of the
    /// numerical non-zeros — a visited slot may still hold 0.0 (cancelling
    /// stamps, a switch conductance landing exactly on a stub) — which is
    /// the sound direction for pattern compilation; see `crate::sparse`.
    pub(crate) touched: StampBits,
    /// Where the branch rows live in `a`/`z`.
    pub(crate) branch: BranchLayout,
}

impl<T: Field> MnaSystem<T> {
    pub fn new(size: usize) -> Self {
        Self {
            size,
            a: DMatrix::zeros(size, size),
            z: DVector::zeros(size),
            touched: StampBits::zeros(size),
            branch: BranchLayout {
                n_nodes: 0,
                vsrc_base: 0,
                ind_base: 0,
                vcvs_base: 0,
                ccvs_base: 0,
                behavioral_base: 0,
                opamp_base: 0,
            },
        }
    }

    /// A fresh system sized for `circuit`, with its branch layout set.
    pub fn for_circuit(circuit: &Circuit) -> Self {
        let mut sys = Self::new(circuit.tran_matrix_size());
        sys.branch = BranchLayout::for_circuit(circuit);
        sys
    }

    /// The stamp seam: whatever the build writes into `a` goes through here
    /// (add) or [`Self::stamp_sub`] (subtract) so one build leaves a complete
    /// structural record in `touched` for the sparse compiler (TODO §3).
    pub fn stamp_add(&mut self, row: usize, col: usize, v: T) {
        self.a[(row, col)] += v;
        self.touched.visit(row, col);
    }

    /// The subtracting twin of [`Self::stamp_add`] — the stamps accumulate
    /// both signs, and spelling `-v` generically would need a `Neg` bound on
    /// [`Field`].
    pub fn stamp_sub(&mut self, row: usize, col: usize, v: T) {
        self.a[(row, col)] -= v;
        self.touched.visit(row, col);
    }

    /// Solve A·x = z using LU decomposition.
    ///
    /// The structural record compiles to a pooled sparse plan (see
    /// [`crate::sparse`]); when the pattern admits a Markowitz order and the
    /// numeric factor succeeds, the sparse path answers. Anything else —
    /// no order, a numerically zero pivot — rides the dense LU unchanged,
    /// which keeps the sparse layer free of any behavioral say in what
    /// comes out.
    ///
    /// Below [`SMALL_SYSTEM`] the dense LU is the better kernel and the
    /// sparse attempt does not even run: measured A/B on the benchmark
    /// suite showed the tiny decks (n ≤ 9) paying the plan/workspace setup
    /// (~30% wall) while the 200-node ladder already wins ~5×.
    pub fn solve(&self) -> Result<DVector<T>> {
        if self.size >= SMALL_SYSTEM {
            let pattern = self.touched.compress();
            if let Some(x) = crate::sparse::sparse_plan(&pattern)
                .and_then(|plan| plan.factor_solve(&self.a, &self.z))
            {
                return Ok(x);
            }
        }
        self.a
            .clone()
            .lu()
            .solve(&self.z)
            .ok_or_else(|| SimError::Algebra("singular MNA matrix".into()))
    }

    /// Zero the RHS: every source silent. Small-signal analyses clear this
    /// once and then patch exactly one excitation in (see
    /// [`Self::drive_source`] and [`Self::drive_node`]).
    pub fn clear_rhs(&mut self) {
        self.z.fill(T::lift(0.0));
    }

    /// The RHS branch entry for independent source `i`: its branch unknown
    /// solves `V(+) − V(−) = amp`. `.ac` uses this to sweep the stimulus at
    /// its card amplitude; `.tf`/`.noise` unit-drive the input source.
    pub fn drive_source(&mut self, i: usize, amp: T) {
        self.z[self.branch.src_branch(i)] = amp;
    }

    /// Add `amp` to node `node`'s RHS row; ground contributes nothing. This
    /// is the port-current probe: `.tf`'s output-impedance drive and the
    /// noise adjoint both push unit current into the output port this way.
    pub fn drive_node(&mut self, node: NodeId, amp: T) {
        if node.0 != 0 {
            self.z[node.0 - 1] += amp;
        }
    }

    /// Solve Aᵀ·x = z, the adjoint system. Noise analysis drives unit
    /// current into the output port of the *transposed* network and reads
    /// every device port's response off `x`, all in one solve.
    pub fn solve_transposed(&self) -> Result<DVector<T>> {
        self.a
            .transpose()
            .lu()
            .solve(&self.z)
            .ok_or_else(|| SimError::Algebra("singular adjoint MNA matrix".into()))
    }
}

impl MnaSystem<f64> {
    /// Max |A·x − z|: the residual the damped-Newton line search tracks.
    /// Real-field only — the metric is a magnitude, and AC solves are
    /// single-shot linear systems with no iteration to measure.
    pub fn residual(&self, x: &DVector<f64>) -> f64 {
        let ax = &self.a * x;
        (&ax - &self.z).amax()
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
                    sys.stamp_add(i1, branch, T::lift(1.0));
                    sys.stamp_add(branch, i1, T::lift(1.0));
                }
                if let Some(i2) = node_index(l.n2) {
                    sys.stamp_sub(i2, branch, T::lift(1.0));
                    sys.stamp_sub(branch, i2, T::lift(1.0));
                }
            }

            // Lossless lines at DC: the steady state is a straight-through
            // connection (documented interpretation, see CONTEXT.md). Each
            // wire pair stamps as a stiff admittance g_wire = 1/(1e-6·Z0),
            // leaving a through-current drop of ΔV ≈ I·1e-6·Z0.
            for line in &circuit.tlines {
                let g_wire = T::lift(1.0 / (1.0e-6 * line.z0));
                stamp_admittance(&mut sys, line.n1, line.n3, g_wire);
                stamp_admittance(&mut sys, line.n2, line.n4, g_wire);
            }

            stamp_voltage_sources(circuit, &mut sys, |v| v.voltage * src_scale);
            stamp_current_sources(circuit, &mut sys, |i| i.current * src_scale);
            stamp_behavioral(circuit, &mut sys, guess, true, 0.0, 0.0, &[], 0.0)?;

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
            // Lossless lines (method of characteristics): each port is its
            // characteristic admittance Y = 1/Z0 in parallel with the
            // opposite port's delayed incident wave
            //   E_A(t) = v_B(t−TD) + Z0·i_B(t−TD)   (E_B symmetric),
            // so port A draws I_A = Y·(V_A − E_A). The wave pushes Y·E out
            // of the + terminal, which lands on the nodes in the current
            // helper's second-then-first order.
            for (i, line) in circuit.tlines.iter().enumerate() {
                let fallback_rings = TlineHistory::default();
                let rings = state.tline_histories.get(i).unwrap_or(&fallback_rings);
                let y = 1.0 / line.z0;
                let t_query = t - line.delay;
                let far = tline_port_sample(&rings.b, t_query);
                let incoming_a = far.v + line.z0 * far.i;
                let near = tline_port_sample(&rings.a, t_query);
                let incoming_b = near.v + line.z0 * near.i;
                stamp_admittance(&mut sys, line.n1, line.n2, T::lift(y));
                stamp_admittance(&mut sys, line.n3, line.n4, T::lift(y));
                stamp_current_source(&mut sys, line.n2, line.n1, T::lift(incoming_a * y));
                stamp_current_source(&mut sys, line.n4, line.n3, T::lift(incoming_b * y));
            }
            stamp_voltage_sources(circuit, &mut sys, |v| v.value_at(t) * startup_ramp(t, startup));
            stamp_current_sources(circuit, &mut sys, |i| i.value_at(t) * startup_ramp(t, startup));
            stamp_behavioral(circuit, &mut sys, guess, true, t, dt, &state.behavioral, 0.0)?;
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
                    sys.stamp_add(i1, branch, T::lift(1.0));
                    sys.stamp_add(branch, i1, T::lift(1.0));
                }
                if let Some(i2) = node_index(l.n2) {
                    sys.stamp_sub(i2, branch, T::lift(1.0));
                    sys.stamp_sub(branch, i2, T::lift(1.0));
                }
                sys.stamp_sub(branch, branch, z_l);
            }
            stamp_ac_mutual_inductances(circuit, &mut sys, omega);

            // Lossless lines: the exact lossless two-port. With θ = ω·TD
            // the port admittance matrix is
            //   Y = 1/(Z0·j·sinθ) · [[cosθ, −1], [−1, cosθ]]
            // (the singularity at sinθ = 0 clamps — see `tline_ac_angles`).
            // The cross terms y12 act on the *opposite* port's voltages,
            // stamped symmetrically; ground terminals drop their share.
            for line in &circuit.tlines {
                let (s, c) = tline_ac_angles(omega * line.delay);
                // 1/(Z0·j·sinθ) = −j/(Z0·sinθ): y11 = cosθ·(−j/(Z0·s)),
                // y12 = +j/(Z0·s) — built through `T::imag` so the generic
                // field needs no division.
                let y11 = T::lift(c) * T::imag(-1.0 / (line.z0 * s));
                let y12 = T::imag(1.0 / (line.z0 * s));
                stamp_admittance(&mut sys, line.n1, line.n2, y11);
                stamp_admittance(&mut sys, line.n3, line.n4, y11);
                for (a, b, value) in [
                    (line.n1, line.n3, y12),
                    (line.n2, line.n4, y12),
                    (line.n1, line.n4, -y12),
                    (line.n2, line.n3, -y12),
                ] {
                    if let (Some(ia), Some(ib)) = (node_index(a), node_index(b)) {
                        sys.stamp_add(ia, ib, value);
                        sys.stamp_add(ib, ia, value);
                    }
                }
            }

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
            // Jacobians; time/history terms behave as constants there. ABM
            // dynamic forms ride the frequency response H(jω).
            stamp_behavioral(circuit, &mut sys, bias, false, 0.0, 0.0, &[], freq)?;
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
pub(crate) fn stamp_controlled_sources<T: Field>(circuit: &Circuit, sys: &mut MnaSystem<T>) {
    let layout = sys.branch;

    // VCVS (E): one branch unknown, output voltage equals the polynomial.
    for (i, c) in circuit.vcvs.iter().enumerate() {
        let row = layout.vcvs_branch(i);
        stamp_branch_nodes(sys, row, c.out_p, c.out_n);
        for (k, control) in c.controls.iter().enumerate() {
            let coeff = c.coeff(k);
            if let ControlVar::Voltage(p, n) = control {
                if let Some(col) = node_index(*p) {
                    sys.stamp_sub(row, col, T::lift(coeff));
                }
                if let Some(col) = node_index(*n) {
                    sys.stamp_add(row, col, T::lift(coeff));
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
                    sys.stamp_sub(row, col, T::lift(coeff));
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
                        sys.stamp_add(out, col, T::lift(coeff));
                    }
                    if let Some(out) = node_index(c.out_n) {
                        sys.stamp_sub(out, col, T::lift(coeff));
                    }
                }
            }
        }
        let c0 = T::lift(c.constant());
        stamp_current_source(sys, c.out_p, c.out_n, c0);
    }

    // Op-amps (U): one branch row holds the output pair at
    // `gain·(V(in+) − V(in−) + VOS)` — the same shape as a single-pair VCVS,
    // so one stamp serves DC, transient, and AC. The RIN/ROUT slots pull in
    // admittance rungs when finite/non-zero; unknown model names fall back
    // to the documented defaults here (validate() rejects them loudly first).
    for (i, u) in circuit.opamps.iter().enumerate() {
        let model = circuit.opamp_params_at(&u.model);
        let row = layout.opamp_branch(i);
        stamp_branch_nodes(sys, row, u.out_p, u.out_n);
        if let Some(ip) = node_index(u.in_p) {
            sys.stamp_sub(row, ip, T::lift(model.gain));
        }
        if let Some(im) = node_index(u.in_n) {
            sys.stamp_add(row, im, T::lift(model.gain));
        }
        sys.z[row] += T::lift(model.gain * model.vos);
        if model.rin.is_finite() && model.rin > 0.0 {
            stamp_admittance(sys, u.in_p, u.in_n, T::lift(1.0 / model.rin));
        }
        if model.rout > 0.0 {
            stamp_admittance(sys, u.out_p, u.out_n, T::lift(1.0 / model.rout));
        }
    }
}

/// `V(a) − V(b) = 0` row plus the symmetric branch couplings.
fn stamp_branch_nodes<T: Field>(sys: &mut MnaSystem<T>, row: usize, p: NodeId, n: NodeId) {
    if let Some(i) = node_index(p) {
        sys.stamp_add(i, row, T::lift(1.0));
        sys.stamp_add(row, i, T::lift(1.0));
    }
    if let Some(j) = node_index(n) {
        sys.stamp_sub(j, row, T::lift(1.0));
        sys.stamp_sub(row, j, T::lift(1.0));
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
        if let Some(col) = node_index(cp) { sys.stamp_add(row, col, g); }
        if let Some(col) = node_index(cn) { sys.stamp_sub(row, col, g); }
    }
    if let Some(row) = node_index(out_n) {
        if let Some(col) = node_index(cp) { sys.stamp_sub(row, col, g); }
        if let Some(col) = node_index(cn) { sys.stamp_add(row, col, g); }
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
pub(crate) struct BehavioralLin {
    /// `f(x0)` at the snapshot.
    f0: f64,
    /// `∂f/∂probe_k` by central differences on the probe values.
    pub(crate) dk: Vec<f64>,
}

pub(crate) fn behavioral_probe_lin(
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
pub(crate) fn behavioral_probe_snapshot(
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
/// * `freq` — the AC test frequency in Hz (0 elsewhere). ABM dynamic sources
///   (LAPLACE/FREQ/CHEBYSHEV, TODO §5) multiply their linearized coupling by
///   the complex response `H(jω)`; outside AC they carry the DC gain H(0)
///   instead (a strictly s-domain source is silent there — the parse warns).
pub(crate) fn stamp_behavioral<T: Field>(
    circuit: &Circuit,
    sys: &mut MnaSystem<T>,
    values: &DVector<f64>,
    with_companion: bool,
    t: f64,
    dt: f64,
    runtimes: &[BehavioralRuntime],
    freq: f64,
) -> Result<()> {
    if circuit.behavioral.is_empty() {
        return Ok(());
    }
    // (real, imag) factors the couplings multiply by. Outside AC a dynamic
    // form's factor is its DC gain on the real axis; instant forms are 1.
    let source_factor = |b: &crate::circuit::BehavioralSource| -> (f64, f64) {
        match &b.abm {
            Some(abm) if abm.is_dynamic() => {
                if with_companion {
                    (abm.dc_gain(), 0.0)
                } else {
                    let g = abm.ac_gain(freq);
                    (g.re, g.im)
                }
            }
            _ => (1.0, 0.0),
        }
    };
    let lift_factor = |g: (f64, f64)| -> T {
        T::lift(g.0) + T::imag(g.1)
    };
    let branch = sys.branch;
    // Voltage-flavour sources own branch rows in card order, independent of
    // the current-flavour ones interleaved between them.
    let mut voltage_row = branch.behavioral_base;
    for (i, b) in circuit.behavioral.iter().enumerate() {
        let empty = BehavioralRuntime::default();
        let runtime = runtimes.get(i).unwrap_or(&empty);
        let fact: (f64, f64) = source_factor(b);
        let snapshot = behavioral_probe_snapshot(circuit, b, values)?;
        let lin = behavioral_probe_lin(b, &snapshot, t, dt, runtime)?;
        // The companion constant is real by construction (only the DC/transient
        // stamp writes `z`, which never carries an imaginary part).
        let constant = (lin.f0
            - lin
                .dk
                .iter()
                .zip(&snapshot)
                .map(|(d, x)| d * x)
                .sum::<f64>())
            * fact.0;
        // One Jacobian coefficient scaled into this source's (complex) gain.
        let dk_factor = |dk: f64| lift_factor((dk * fact.0, dk * fact.1));

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
                    let g = dk_factor(dk);
                    match probe {
                        crate::behavior::ProbeBinding::Voltage(p, m) => {
                            if let Some(col) = node_index(*p) {
                                sys.stamp_sub(row, col, g);
                            }
                            if let Some(col) = node_index(*m) {
                                sys.stamp_add(row, col, g);
                            }
                        }
                        crate::behavior::ProbeBinding::SourceCurrent(idx) => {
                            let col = branch.src_branch(*idx);
                            sys.stamp_sub(row, col, g);
                        }
                        crate::behavior::ProbeBinding::InductorCurrent(idx) => {
                            let col = branch.ind_branch(*idx);
                            sys.stamp_sub(row, col, g);
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
                        let g = dk_factor(dk);
                        match probe {
                            crate::behavior::ProbeBinding::Voltage(p, m) => {
                                if let Some(col) = node_index(*p) {
                                    sys.stamp_add(row, col, g);
                                }
                                if let Some(col) = node_index(*m) {
                                    sys.stamp_sub(row, col, g);
                                }
                            }
                            crate::behavior::ProbeBinding::SourceCurrent(idx) => {
                                sys.stamp_add(row, branch.src_branch(*idx), g);
                            }
                            crate::behavior::ProbeBinding::InductorCurrent(idx) => {
                                sys.stamp_add(row, branch.ind_branch(*idx), g);
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
                        let g = dk_factor(dk);
                        match probe {
                            crate::behavior::ProbeBinding::Voltage(p, m) => {
                                if let Some(col) = node_index(*p) {
                                    sys.stamp_sub(row, col, g);
                                }
                                if let Some(col) = node_index(*m) {
                                    sys.stamp_add(row, col, g);
                                }
                            }
                            crate::behavior::ProbeBinding::SourceCurrent(idx) => {
                                sys.stamp_sub(row, branch.src_branch(*idx), g);
                            }
                            crate::behavior::ProbeBinding::InductorCurrent(idx) => {
                                sys.stamp_sub(row, branch.ind_branch(*idx), g);
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
    let model = device_model(&circuit.switch_models, "switch", &s.name, &s.model)?;
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
pub(crate) fn stamp_switches<T: Field>(
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
        let model = device_model(&circuit.diode_models, "diode", &d.name, &d.model)?;
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
    let companion = Companion::for_step(method, dt, state.last_dt);
    for (i, d) in circuit.diodes.iter().enumerate() {
        let model = device_model(&circuit.diode_models, "diode", &d.name, &d.model)?;
        if model.cjo == 0.0 && model.tt == 0.0 {
            continue;
        }
        let vd = node_voltage(guess, d.anode) - node_voltage(guess, d.cathode);
        let q = model.total_charge_at(vd, d.area, circuit.temperature);
        let dqdv = model.dcharge_dv_at(vd, d.area, circuit.temperature);
        let q_prev = state.diode_charges.get(i).copied().unwrap_or(q);
        let q_older = state.diode_charges_older.get(i).copied().unwrap_or(q_prev);
        let i_prev = state.diode_cap_currents.get(i).copied().unwrap_or(0.0);
        let (g_eq, i_eq) =
            companion.charge_capacitor(dqdv, vd, q, q_prev, q_older, i_prev);
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
        let model = device_model(&circuit.diode_models, "diode", &d.name, &d.model)?;
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
        let model = device_model(&circuit.bjt_models, "BJT", &q.name, &q.model)?;
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
                if let Some(col) = node_index(q.nb) { sys.stamp_add(row, col, T::lift(vb)); }
                if let Some(col) = node_index(q.ne) { sys.stamp_add(row, col, T::lift(ve)); }
                if let Some(col) = node_index(q.nc) { sys.stamp_add(row, col, T::lift(vc)); }
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
    let companion = Companion::for_step(method, dt, state.last_dt);
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
            let (g_eq, i_eq) = companion.capacitor(cap, v_prev, v_older, i_prev);
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
        let model = device_model(&circuit.jfet_models, "JFET", &j.name, &j.model)?;
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
                if let Some(col) = node_index(j.ng) { sys.stamp_add(row, col, T::lift(vg)); }
                if let Some(col) = node_index(j.ns) { sys.stamp_add(row, col, T::lift(vs)); }
                if let Some(col) = node_index(j.nd) { sys.stamp_add(row, col, T::lift(vd)); }
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
    let companion = Companion::for_step(method, dt, state.last_dt);
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
            let (g_eq, i_eq) = companion.capacitor(cap, v_prev, v_older, i_prev);
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
        let model = device_model(&circuit.mosfet_models, "MOSFET", &m.name, &m.model)?;

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
                sys.stamp_add(id_idx, ig_idx, T::lift(s * gm));
            }
            if let Some(is_idx) = node_index(source) {
                sys.stamp_sub(id_idx, is_idx, T::lift(s * gm));
            }
        }
        if let Some(is_idx) = node_index(source) {
            if let Some(ig_idx) = node_index(gate) {
                sys.stamp_sub(is_idx, ig_idx, T::lift(s * gm));
            }
            if let Some(is2_idx) = node_index(source) {
                sys.stamp_add(is_idx, is2_idx, T::lift(s * gm));
            }
        }

        // -- gbs: bulk transconductance controlled by Vbs --
        //   Current gbs*Vbs flows from drain to source.
        if let Some(id_idx) = node_index(drain) {
            if let Some(ib_idx) = node_index(bulk) {
                sys.stamp_add(id_idx, ib_idx, T::lift(s * gbs));
            }
            if let Some(is_idx) = node_index(source) {
                sys.stamp_sub(id_idx, is_idx, T::lift(s * gbs));
            }
        }
        if let Some(is_idx) = node_index(source) {
            if let Some(ib_idx) = node_index(bulk) {
                sys.stamp_sub(is_idx, ib_idx, T::lift(s * gbs));
            }
            if let Some(is2_idx) = node_index(source) {
                sys.stamp_add(is_idx, is2_idx, T::lift(s * gbs));
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
    let companion = Companion::for_step(method, dt, state.last_dt);
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
            let (g_eq, i_eq) = companion.capacitor(cap, v_prev, v_older, i_prev);
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
    let companion = Companion::for_step(method, dt, state.last_dt);
    for (i, c) in circuit.capacitors.iter().enumerate() {
        let cap = circuit.capacitance_at(c);
        let v_prev = state.capacitor_voltages.get(i).copied().unwrap_or(0.0);
        let i_prev = state.capacitor_currents.get(i).copied().unwrap_or(0.0);
        let v_older = state.capacitor_voltages_older.get(i).copied().unwrap_or(v_prev);
        let (g_eq, i_eq) = companion.capacitor(cap, v_prev, v_older, i_prev);
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
    let companion = Companion::for_step(method, dt, state.last_dt);
    for (i, l) in circuit.inductors.iter().enumerate() {
        let ind = circuit.inductance_at(l);
        let i_prev = state.inductor_currents.get(i).copied().unwrap_or(0.0);
        let i_older = state.inductor_currents_older.get(i).copied().unwrap_or(i_prev);
        let v_prev = state.inductor_voltages.get(i).copied().unwrap_or(0.0);
        let (r_eq, v_eq) = companion.dual(ind, i_prev, i_older, v_prev);
        let branch = sys.branch.ind_branch(i);

        if let Some(i1) = node_index(l.n1) {
            sys.stamp_add(i1, branch, T::lift(1.0));
            sys.stamp_add(branch, i1, T::lift(1.0));
        }
        if let Some(i2) = node_index(l.n2) {
            sys.stamp_sub(i2, branch, T::lift(1.0));
            sys.stamp_sub(branch, i2, T::lift(1.0));
        }
        sys.stamp_sub(branch, branch, T::lift(r_eq));
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
    let companion = Companion::for_step(method, dt, state.last_dt);
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
        let (coef, eq_i) = companion.dual(m, prev_j, older_j, v_mutual);
        let eq_j = companion.dual(m, prev_i, older_i, v_mutual).1;
        let bi = sys.branch.ind_branch(i);
        let bj = sys.branch.ind_branch(j);
        sys.stamp_sub(bi, bj, T::lift(coef));
        sys.stamp_sub(bj, bi, T::lift(coef));
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
        sys.stamp_sub(bi, bj, zm);
        sys.stamp_sub(bj, bi, zm);
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

pub(crate) fn stamp_voltage_sources<T, F>(circuit: &Circuit, sys: &mut MnaSystem<T>, value_at: F)
where
    T: Field,
    F: Fn(&VoltageSource) -> f64,
{
    for (i, v) in circuit.voltage_sources.iter().enumerate() {
        let branch = sys.branch.src_branch(i);
        if let Some(i1) = node_index(v.n1) {
            sys.stamp_add(i1, branch, T::lift(1.0));
            sys.stamp_add(branch, i1, T::lift(1.0));
        }
        if let Some(i2) = node_index(v.n2) {
            sys.stamp_sub(i2, branch, T::lift(1.0));
            sys.stamp_sub(branch, i2, T::lift(1.0));
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

pub(crate) fn stamp_admittance<T: Field>(sys: &mut MnaSystem<T>, n1: NodeId, n2: NodeId, y: T) {
    if let Some(i) = node_index(n1) {
        sys.stamp_add(i, i, y);
    }
    if let Some(j) = node_index(n2) {
        sys.stamp_add(j, j, y);
    }
    if let (Some(i), Some(j)) = (node_index(n1), node_index(n2)) {
        sys.stamp_sub(i, j, y);
        sys.stamp_sub(j, i, y);
    }
}

pub(crate) fn stamp_current_source<T: Field>(
    sys: &mut MnaSystem<T>,
    n1: NodeId,
    n2: NodeId,
    current: T,
) {
    if let Some(i) = node_index(n1) {
        sys.z[i] -= current;
    }
    if let Some(j) = node_index(n2) {
        sys.z[j] += current;
    }
}

/// Return the index into the MNA unknown vector for a node.
/// Ground (node 0) has no row — its voltage is identically zero.
pub(crate) fn node_index(node: NodeId) -> Option<usize> {
    if node.0 == 0 { None } else { Some(node.0 - 1) }
}

pub fn node_voltage<T: Field>(solution: &DVector<T>, node: NodeId) -> T {
    node_index(node).map(|i| solution[i]).unwrap_or_else(|| T::lift(0.0))
}

/// State implied by a solved operating point / initial vector: voltages and
/// currents known, every history term zero (DC steady state). `noise_seed`
/// is the deck's keyed-draw seed for the behavioral noise terms (`white()`/
/// `flicker()`), threaded from the parsed netlist.
pub fn seed_transient_state(
    circuit: &Circuit,
    solution: &DVector<f64>,
    noise_seed: u64,
) -> TransientState {
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
    // Lossless lines: the rings seed from the steady operating point over
    // [−TD, 0] (the transient's time axis starts at t = 0). The wire-pair
    // DC model: both port voltages track, the through wires carry
    // g_wire·(v_hot difference) with port B's current the negative of
    // port A's — the exact steady-state port relation (i_B = −i_A).
    state.tline_histories = circuit
        .tlines
        .iter()
        .map(|line| {
            let v_a = node_voltage(solution, line.n1) - node_voltage(solution, line.n2);
            let v_b = node_voltage(solution, line.n3) - node_voltage(solution, line.n4);
            let i_a = (node_voltage(solution, line.n1) - node_voltage(solution, line.n3))
                / (1.0e-6 * line.z0);
            TlineHistory {
                a: vec![
                    TlineSample {
                        t: -line.delay,
                        v: v_a,
                        i: i_a,
                    },
                    TlineSample {
                        t: 0.0,
                        v: v_a,
                        i: i_a,
                    },
                ],
                b: vec![
                    TlineSample {
                        t: -line.delay,
                        v: v_b,
                        i: -i_a,
                    },
                    TlineSample {
                        t: 0.0,
                        v: v_b,
                        i: -i_a,
                    },
                ],
            }
        })
        .collect();
    state.switch_states = switch_states_after(circuit, solution, &[]);

    // Behavioral sources: the runtime module seeds its own history from the
    // DC operating point — `idt` previous values and every `delay` queue's
    // t = 0 sample. mna only provides the converged operating-point probes;
    // the keyed noise roll starts from `noise_seed` so the transient's
    // white()/flicker() draws reproduce the deck's seed exactly.
    state.behavioral = circuit
        .behavioral
        .iter()
        .map(|b| {
            let snapshot = behavioral_probe_snapshot(circuit, b, solution)
                .unwrap_or_else(|_| vec![0.0; b.behavior.probes.len()]);
            BehavioralRuntime::seeded(&b.behavior, &snapshot, noise_seed)
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
    let companion = Companion::for_step(method, dt, previous.last_dt);
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
            // One history law: the accepted voltage steps and the previous
            // capacitor current.
            companion.derivative(cap, v_new, v_prev, v_older, i_prev)
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
            companion.derivative(1.0, q, q_prev, q_older, i_prev)
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
                currents[k] =
                    companion.derivative(cap, new_v[k], prev_v[k], older_v[k], prev_i[k]);
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
                currents[k] =
                    companion.derivative(caps[k], new_v[k], prev_v[k], older_v[k], prev_i[k]);
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
                currents[k] =
                    companion.derivative(caps[k], new_v[k], prev_v[k], older_v[k], prev_i[k]);
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
        let v = companion.derivative(m, new_j, prev_j, older_j, previous_mutual);
        mutual_voltages.push(v);
    }

    // Behavioral sources: the runtime module owns the history law — `idt`
    // trapezoids complete on the converged end-of-step probes, `delay` sites
    // record their accepted sample. mna only carries the snapshot across.
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
            let snapshot = behavioral_probe_snapshot(circuit, b, solution)
                .unwrap_or_else(|_| vec![0.0; b.behavior.probes.len()]);
            runtime.advance(&b.behavior, &snapshot, t_end, dt);
            runtime
        })
        .collect();

    // Lossless lines: push the accepted end-of-step port samples — the
    // port current from the MoC relation with the same delayed wave the
    // step's stamp used — then prune what no future query can reach.
    let mut tline_histories = if previous.tline_histories.len() == circuit.tlines.len() {
        previous.tline_histories.clone()
    } else {
        vec![TlineHistory::default(); circuit.tlines.len()]
    };
    for (i, line) in circuit.tlines.iter().enumerate() {
        let y = 1.0 / line.z0;
        let far = tline_port_sample(&tline_histories[i].b, t_end - line.delay);
        let incoming_a = far.v + line.z0 * far.i;
        let near = tline_port_sample(&tline_histories[i].a, t_end - line.delay);
        let incoming_b = near.v + line.z0 * near.i;
        let v_a = node_voltage(solution, line.n1) - node_voltage(solution, line.n2);
        let v_b = node_voltage(solution, line.n3) - node_voltage(solution, line.n4);
        push_tline_sample(
            &mut tline_histories[i].a,
            TlineSample {
                t: t_end,
                v: v_a,
                i: y * (v_a - incoming_a),
            },
            t_end - line.delay,
        );
        push_tline_sample(
            &mut tline_histories[i].b,
            TlineSample {
                t: t_end,
                v: v_b,
                i: y * (v_b - incoming_b),
            },
            t_end - line.delay,
        );
    }

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
        tline_histories,
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

    /// The stamp-seam zoo: one deck containing every element family, built
    /// in all three arms. The structural record must cover every dense
    /// non-zero — the sound superset direction (`record ⊇ non-zeros`; a
    /// visited-but-zero slot is allowed, a missed non-zero is a bug) — and
    /// the record must compile to a Markowitz pivot order on every arm.
    #[test]
    fn zoo_stamp_record_covers_dense_nonzeros_in_every_arm() {
        let circuit = zoo_circuit();

        // DC: junction limiter tables per the dc-stage convention.
        {
            let guess = DVector::zeros(circuit.tran_matrix_size());
            let mut used = DcJunctionLimits::seed(&circuit, &guess);
            let sys = build_system::<f64>(
                &circuit,
                Mode::Dc {
                    guess: &guess,
                    limits: &DcJunctionLimits::zeroed(&circuit),
                    used: &mut used,
                    gmin: 1e-12,
                    src_scale: 1.0,
                },
            )
            .unwrap();
            assert_record_covers(&sys, "dc");
            assert_record_orders(&sys, "dc");
        }

        // Transient: fresh history, a real step, trapezoidal (the default).
        {
            let guess = DVector::zeros(circuit.tran_matrix_size());
            let state = TransientState::default();
            let sys = build_system::<f64>(
                &circuit,
                Mode::Tran {
                    dt: 1e-6,
                    t: 0.0,
                    state: &state,
                    method: IntegrationMethod::Trapezoidal,
                    guess: &guess,
                    startup: false,
                },
            )
            .unwrap();
            assert_record_covers(&sys, "tran");
            assert_record_orders(&sys, "tran");
        }

        // AC: everything linearizes at a zero bias — only the record's
        // completeness matters here, not the values.
        {
            let bias = DVector::zeros(circuit.tran_matrix_size());
            let sys = build_system::<Complex64>(
                &circuit,
                Mode::Ac {
                    freq: 1e3,
                    bias: &bias,
                },
            )
            .unwrap();
            assert_record_covers(&sys, "ac");
            assert_record_orders(&sys, "ac");
        }
    }

    /// The zoo solves through `solve()` and agrees with a dense reference in
    /// all three arms — the wiring test over the exact numerics covered by
    /// `sparse.rs`'s unit tests.
    #[test]
    fn zoo_sparse_solve_matches_dense_reference_in_every_arm() {
        let circuit = zoo_circuit();

        // DC, per the record-covers arm above.
        {
            let guess = DVector::zeros(circuit.tran_matrix_size());
            let mut used = DcJunctionLimits::seed(&circuit, &guess);
            let sys = build_system::<f64>(
                &circuit,
                Mode::Dc {
                    guess: &guess,
                    limits: &DcJunctionLimits::zeroed(&circuit),
                    used: &mut used,
                    gmin: 1e-12,
                    src_scale: 1.0,
                },
            )
            .unwrap();
            assert_solve_matches_dense(&sys, "dc", |v| v.abs());
        }

        // Transient, one trapezoidal step on a default history.
        {
            let guess = DVector::zeros(circuit.tran_matrix_size());
            let state = TransientState::default();
            let sys = build_system::<f64>(
                &circuit,
                Mode::Tran {
                    dt: 1e-6,
                    t: 0.0,
                    state: &state,
                    method: IntegrationMethod::Trapezoidal,
                    guess: &guess,
                    startup: false,
                },
            )
            .unwrap();
            assert_solve_matches_dense(&sys, "tran", |v| v.abs());
        }

        // AC over the complex field at a zero bias.
        {
            let bias = DVector::zeros(circuit.tran_matrix_size());
            let sys = build_system::<Complex64>(
                &circuit,
                Mode::Ac {
                    freq: 1e3,
                    bias: &bias,
                },
            )
            .unwrap();
            assert_solve_matches_dense(&sys, "ac", |v| v.norm());
        }
    }

    /// Every dense non-zero slot sits in the structural record.
    fn assert_record_covers<T: Field>(sys: &MnaSystem<T>, arm: &str) {
        for row in 0..sys.size {
            for col in 0..sys.size {
                if sys.a[(row, col)] != T::lift(0.0) {
                    assert!(
                        sys.touched.is_set(row, col),
                        "{arm}: dense non-zero at ({row},{col}) missed by the stamp record"
                    );
                }
            }
        }
    }

    /// `sys.solve()` — the sparse path where the pattern admits an order,
    /// dense otherwise — answers like a straight dense LU solve of the same
    /// matrix. `mag` measures the field's magnitudes (`.abs()` real, `.norm()`
    /// complex).
    fn assert_solve_matches_dense<T, F>(sys: &MnaSystem<T>, arm: &str, mag: F)
    where
        T: Field,
        F: Fn(T) -> f64,
    {
        let sparse = sys
            .solve()
            .unwrap_or_else(|e| panic!("{arm}: solve failed: {e}"));
        let dense = sys
            .a
            .clone()
            .lu()
            .solve(&sys.z)
            .unwrap_or_else(|| panic!("{arm}: dense reference solve failed"));
        for (i, (s, d)) in sparse.iter().zip(dense.iter()).enumerate() {
            let diff = mag(*s - *d);
            let scale = 1.0 + mag(*s).max(mag(*d));
            assert!(
                diff <= 1e-8 * scale,
                "{arm}: solve entry {i}: sparse {} vs dense {}",
                mag(*s),
                mag(*d)
            );
        }
    }

    /// The record compiles: a Markowitz order exists for the arm's pattern,
    /// one pivot per index.
    fn assert_record_orders<T: Field>(sys: &MnaSystem<T>, arm: &str) {
        let pattern = sys.touched.compress();
        let order = crate::sparse::markowitz_ordering(&pattern)
            .unwrap_or_else(|| panic!("{arm}: structural pattern has no Markowitz order"));
        assert_eq!(order.order.len(), pattern.n(), "{arm}: one pivot per index");
    }

    /// One of every element family: R, C, L, mutual `(K)`, V, I, both `E`
    /// forms (linear and `POLY`), `G`, `F`, `H`, `S`, both `B` flavours, `U`
    /// (model form), `T`, `D`, `Q`, `J`, `M`. The three arm tests build this
    /// deck directly (no analysis run): the record's completeness is a
    /// stamping property, not a numerics one.
    fn zoo_circuit() -> Circuit {
        crate::netlist::parse(
            "* structural zoo: every element family\n\
             .model dm D(IS=1e-14)\n\
             .model qm NPN(IS=1e-14 BF=100)\n\
             .model jm NJF(VTO=-2 BETA=2e-4 IS=1e-14)\n\
             .model mm NMOS(LEVEL=1 KP=110u VTO=1)\n\
             .model sw SW(RON=1 ROFF=1meg VT=0.5)\n\
             .model um U(GAIN=1e5)\n\
             V1 in 0 DC 1\n\
             I1 ib 0 DC 1m\n\
             Rib ib 0 1meg\n\
             R1 in mid 1k\n\
             C1 mid cb 1n\n\
             L1 la 0 1m\n\
             L2 lb 0 1m\n\
             K1 L1 L2 0.9\n\
             Vc ctrl 0 DC 1\n\
             S1 s1 s2 ctrl 0 sw\n\
             Rs1 s1 0 1k\n\
             Rs2 s2 0 1k\n\
             Ep out 0 POLY(2) in 0 mid 0 1 2 3\n\
             El out2 0 in 0 10\n\
             G1 gi in mid 0 1m\n\
             Rgi gi 0 1k\n\
             Rf fi 0 1k\n\
             F1 fi fin V1 2\n\
             Rfin fin 0 1k\n\
             Rh hi 0 1k\n\
             H1 hi hin V1 2000\n\
             Rhin hin 0 1k\n\
             Bv bv 0 V=V(in)*2\n\
             Rb bi 0 1meg\n\
             Bi bi 0 I=I(V1)*10\n\
             Ub uo 0 0 in um\n\
             T1 ta 0 tb 0 Z0=50 TD=1n\n\
             Rta ta 0 1k\n\
             Rtb tb 0 1k\n\
             D1 da 0 dm\n\
             Q1 qc qb 0 qm\n\
             J1 jg 0 js jm\n\
             Rg mg 0 1meg\n\
             M1 md mg ms ms mm W=10u L=1u\n\
             Rms ms 0 1k\n\
             Rd md 0 1k\n\
             Rload out 0 1k\n\
             .op\n\
             .tran 1u 10u\n\
             .ac dec 5 1k 100meg\n\
             .end\n",
        )
        .unwrap()
        .circuit
    }
}
