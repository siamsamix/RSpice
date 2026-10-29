use crate::error::{Result, SimError};
use crate::behavior::{ProbeBinding, ProbeSpec};
use num_complex::Complex64;
use std::collections::HashMap;

// Device model cards live in `model.rs` together with the device equations —
// one module owns the physics. Re-exported here for the historical paths.
pub use crate::model::{BjtModel, BjtType, DiodeModel, GenericModel, JfetModel, JfetType, ModelTolerance, MosfetModel, MosfetType, OpAmpModel};

#[derive(Debug, Clone)]
pub struct Mosfet {
    pub name: String,
    /// Drain node.
    pub nd: NodeId,
    /// Gate node.
    pub ng: NodeId,
    /// Source node.
    pub ns: NodeId,
    /// Bulk / body node.
    pub nb: NodeId,
    /// Name of the referenced .MODEL card.
    pub model: String,
    /// Channel width W (m).
    pub w: f64,
    /// Channel length L (m).
    pub l: f64,
    /// Drain junction area AD (m²) — parsed for the capacitance work
    /// (TODO §5); not stamped yet.
    pub ad: f64,
    /// Source junction area AS (m²) — parsed, not stamped yet.
    pub asr: f64,
    /// Drain junction perimeter PS (m) — parsed, not stamped yet.
    pub ps: f64,
    /// Source junction perimeter PD (m) — parsed, not stamped yet.
    pub pd: f64,
}

/// Time-integration method for `.tran` companion models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IntegrationMethod {
    /// Trapezoidal (second-order, the SPICE default; can ring on stiff
    /// circuits).
    #[default]
    Trapezoidal,
    /// Backward Euler / Gear I (first-order, unconditionally stable).
    Gear1,
    /// Gear II (BDF2, second-order, constant step).
    Gear2,
}

// ─── Passive elements ────────────────────────────────────────────────────────

/// Ground is always node 0 and is excluded from the unknown vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub usize);

/// Passive device model card (`.model … R/C/L`): an optional model-supplied
/// value plus the quadratic temperature coefficients and the LTspice-style
/// parasitic slots (`RSER`/`LSER` in series, `CPAR`/`RPAR` across terminals).
#[derive(Debug, Clone, Default)]
pub struct PassiveModel {
    /// Model-supplied nominal value (`R=`, `C=`, `L=`), used when the element
    /// card carries no value.
    pub value: Option<f64>,
    /// First-order temperature coefficient TC1 (1/°C).
    pub tc1: f64,
    /// Second-order temperature coefficient TC2 (1/°C²).
    pub tc2: f64,
    /// Series resistance RSER (Ω) split into a real resistor arm.
    pub rser: f64,
    /// Series inductance LSER (H) split into a real inductor arm.
    pub lser: f64,
    /// Parallel capacitance CPAR (F) stamped across the terminals.
    pub cpar: f64,
    /// Parallel resistance RPAR (Ω) stamped across the terminals.
    pub rpar: f64,
    /// Reserved Monte-Carlo tolerance slots (TODO §8: the `.mc` driver draws
    /// on the nominal value per run/instance).
    pub tolerance: ModelTolerance,
}

impl PassiveModel {
    /// True when the card carries at least one parasitic slot.
    pub fn has_parasitics(&self) -> bool {
        self.rser != 0.0 || self.lser != 0.0 || self.cpar != 0.0 || self.rpar != 0.0
    }
}

/// Which physical element a generated series arm is (TODO §5 parasitics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParasiticArmKind {
    /// Series resistance arm (`RSER`).
    SeriesResistor,
    /// Series inductance arm (`LSER`).
    SeriesInductor,
}

#[derive(Debug, Clone)]
pub struct Resistor {
    pub name: String,
    pub n1: NodeId,
    pub n2: NodeId,
    /// Element value; 0 means "take the model card's R=".
    pub resistance: f64,
    /// Optional `.model … R(…)` reference.
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Capacitor {
    pub name: String,
    pub n1: NodeId,
    pub n2: NodeId,
    /// Element value; 0 means "take the model card's C=".
    pub capacitance: f64,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Inductor {
    pub name: String,
    pub n1: NodeId,
    pub n2: NodeId,
    /// Element value; 0 means "take the model card's L=".
    pub inductance: f64,
    pub model: Option<String>,
}

/// Coupled inductors, SPICE `Kname L1 L2 <coupling>`; the mutual inductance
/// is `M = k·√(L1·L2)`.
#[derive(Debug, Clone)]
pub struct MutualInductance {
    pub name: String,
    pub inductor1: String,
    pub inductor2: String,
    /// Coupling coefficient k (0 < k < 1).
    pub coupling: f64,
}

#[derive(Debug, Clone)]
pub struct Diode {
    pub name: String,
    pub anode: NodeId,
    pub cathode: NodeId,
    pub model: String,
    /// Junction area multiplier (SPICE `AREA`, default 1) — scales the
    /// saturation current in the stamp.
    pub area: f64,
}

/// Bipolar junction transistor (`Q` card). `ns` is the optional substrate
/// node of a four-terminal device.
#[derive(Debug, Clone)]
pub struct Bjt {
    pub name: String,
    pub nc: NodeId,
    pub nb: NodeId,
    pub ne: NodeId,
    pub ns: Option<NodeId>,
    pub model: String,
    /// Area multiplier AREA (default 1).
    pub area: f64,
}

/// Junction field-effect transistor (`J` card):
/// `Jname nd ng ns model [AREA=…]` — drain, gate, source.
#[derive(Debug, Clone)]
pub struct Jfet {
    pub name: String,
    pub nd: NodeId,
    pub ng: NodeId,
    pub ns: NodeId,
    pub model: String,
    /// Area multiplier AREA (default 1).
    pub area: f64,
}

// ─── Controlled sources (E/F/G/H) ───────────────────────────────────────────

/// What one term of a controlled source reads: a differential node voltage
/// (`V(a,b)`) or the branch current of a named voltage source (`I(Vname)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlVar {
    Voltage(NodeId, NodeId),
    Current(String),
}

/// A linear or `POLY(n)` controlled source.
///
/// The output is polynomial in the control variables:
/// `out = coeffs[0] + coeffs[1]·x1 + coeffs[2]·x2 + …` (SPICE writes the
/// constant first, then one coefficient per control). A plain gain card is
/// just `coeffs = [0, gain]` with a single control, so one stamp covers both
/// spellings.
#[derive(Debug, Clone)]
pub struct ControlledSource {
    pub name: String,
    pub out_p: NodeId,
    pub out_n: NodeId,
    pub controls: Vec<ControlVar>,
    pub coeffs: Vec<f64>,
}

/// Output flavour of a behavioral source (`B`): a voltage between its
/// terminals (one MNA branch row) or a current drawn from `out_p` and
/// delivered into `out_n` (no branch row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BehavioralKind {
    /// `B… V=<expr>` — output voltage `V(out_p) − V(out_n) = f(…)`.
    Voltage,
    /// `B… I=<expr>` — output current `f(…)` flowing `out_p → out_n`.
    Current,
}

/// The frequency-domain gain units of an ABM `FREQ` table (TODO §5): gains
/// plain by default, `MEASURE=DB` makes them dB (20·log₁₀); phases degrees by
/// default, `MEASURE=RADS` makes them radians.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbmMeasure {
    /// Gains in dB (`MEASURE=DB`).
    Db,
    /// Phases in radians (`MEASURE=RADS`).
    Rads,
    /// Phases in degrees (`MEASURE=DEGREES`).
    Degrees,
}

/// The PSpice ABM form one behavioral entry was parsed from (TODO §5).
///
/// ABM `VALUE`/`TABLE` responses are instant transfers, carried entirely in
/// the compiled AST (a `Table` wraps the input expression there); this aux
/// exists so `emit` can re-spell the exact card shape, and so the dynamic
/// forms (LAPLACE/FREQ/CHEBYSHEV) can multiply the linearized coupling by
/// their frequency response. Polynomials are stored **descending** (first
/// entry = highest power), matching the PSpice spelling.
#[derive(Debug, Clone)]
pub enum AbmForm {
    /// `VALUE={expr}` (and its `EQUATION` alias) — a plain instant transfer
    /// in every analysis; gain 1, carried fully by the AST.
    Value,
    /// `TABLE {expr} = (x1,y1) (x2,y2) …` — piecewise-linear lookup; the
    /// evaluation lives in the AST (zero outside the bracketing range).
    Table,
    /// `LAPLACE {expr} = {n1, n2, …} {d1, d2, …}` — s-domain polynomial
    /// ratio, coefficients descending (`{2s+1}` → `2, 1`).
    Laplace { num: Vec<f64>, den: Vec<f64> },
    /// `FREQ {expr} = <f, g, p> …` — frequency table of gains/phases.
    Freq { rows: Vec<(f64, f64, f64)>, measure: Option<AbmMeasure> },
    /// `CHEBYSHEV {expr} c2 c3 c4 …` — magnitude response
    /// `|H(f)| = 1 / (1 + |cᵢ·Tᵢ₊₂(f)|)`, Chebyshev polynomials in the
    /// frequency in Hz, zero phase.
    Chebyshev { coeffs: Vec<f64> },
}

impl AbmForm {
    /// True for the frequency-domain forms whose output is multiplied by a
    /// frequency response: their DC/transient stamps carry the DC gain, and
    /// only AC carries the full complex response.
    pub fn is_dynamic(&self) -> bool {
        matches!(self, AbmForm::Laplace { .. } | AbmForm::Freq { .. } | AbmForm::Chebyshev { .. })
    }

    /// The DC transfer of a dynamic form: the response evaluated at zero
    /// frequency (a LAPLACE ratio at s=0, the FREQ table clamped to its
    /// lowest row, the CHEBYSHEV magnitude at f=0). Instant forms are 1 —
    /// their feedback lives inside the AST.
    pub fn dc_gain(&self) -> f64 {
        match self {
            AbmForm::Value | AbmForm::Table => 1.0,
            AbmForm::Laplace { num, den } => {
                let n0 = num.last().copied().unwrap_or(0.0);
                let d0 = den.last().copied().unwrap_or(1.0);
                if d0 != 0.0 { n0 / d0 } else { 0.0 }
            }
            AbmForm::Freq { rows, measure } => {
                let Some(row) = rows.last().copied() else { return 0.0 };
                freq_row_gain(row, *measure)
            }
            AbmForm::Chebyshev { coeffs } => chebyshev_gain(coeffs, 0.0),
        }
    }

    /// The complex frequency response `H(jω)` of a dynamic form at test
    /// frequency `f` (Hz). Instant forms answer 1 — the AC stamp then uses
    /// the plain real linearization.
    pub fn ac_gain(&self, f: f64) -> Complex64 {
        match self {
            AbmForm::Value | AbmForm::Table => Complex64::new(1.0, 0.0),
            AbmForm::Laplace { num, den } => {
                let s = Complex64::new(0.0, 2.0 * std::f64::consts::PI * f);
                let eval = |coeffs: &[f64]| -> Complex64 {
                    // Horner over the descending coefficients.
                    coeffs
                        .iter()
                        .fold(Complex64::new(0.0, 0.0), |acc, c| acc * s + Complex64::new(*c, 0.0))
                };
                let den = eval(den);
                if den.norm() < f64::EPSILON {
                    Complex64::new(0.0, 0.0)
                } else {
                    eval(num) / den
                }
            }
            AbmForm::Freq { rows, measure } => {
                if rows.is_empty() {
                    return Complex64::new(0.0, 0.0);
                }
                let (gain, phase) = if f >= rows[0].0 && f <= rows[rows.len() - 1].0 {
                    // Linear interpolation in log(f) between the bracketing rows.
                    let mut k = 0usize;
                    while k + 1 < rows.len() && rows[k + 1].0 < f {
                        k += 1;
                    }
                    let (f0, g0, p0) = rows[k];
                    let (f1, g1, p1) = rows[(k + 1).min(rows.len() - 1)];
                    if f1 == f0 {
                        (g1, p1)
                    } else {
                        let frac = (f.log10() - f0.log10()) / (f1.log10() - f0.log10());
                        (g0 + frac * (g1 - g0), p0 + frac * (p1 - p0))
                    }
                } else if f < rows[0].0 {
                    (rows[0].1, rows[0].2)
                } else {
                    let last = rows[rows.len() - 1];
                    (last.1, last.2)
                };
                let g = freq_row_gain((0.0, gain, 0.0), *measure);
                Complex64::new(
                    g * phase.to_radians().cos(),
                    g * phase.to_radians().sin(),
                )
            }
            AbmForm::Chebyshev { coeffs } => Complex64::new(chebyshev_gain(coeffs, f), 0.0),
        }
    }
}

/// Convert one FREQ row's numbers into a linear voltage gain, honoring the
/// card's MEASURE selection (gains plain by default, dB under `MEASURE=DB`).
fn freq_row_gain(row: (f64, f64, f64), measure: Option<AbmMeasure>) -> f64 {
    match measure {
        Some(AbmMeasure::Db) => 10f64.powf(row.1 / 20.0),
        _ => row.1,
    }
}

/// The ABM CHEBYSHEV magnitude response at frequency `f`:
/// `|H(f)| = 1 / (1 + |c₂·T₂(f)| + |c₃·T₃(f)| + …)`, `Tₙ` the Chebyshev
/// polynomials of the first kind evaluated at the frequency in Hz.
fn chebyshev_gain(coeffs: &[f64], f: f64) -> f64 {
    let mut penalty = 0.0;
    for (i, c) in coeffs.iter().enumerate() {
        let order = i + 2;
        penalty += (c * chebyshev_tn(order, f)).abs();
    }
    1.0 / (1.0 + penalty)
}

/// Chebyshev polynomial of the first kind, order `n`, at `x` (recurrence).
fn chebyshev_tn(n: usize, x: f64) -> f64 {
    match n {
        0 => 1.0,
        1 => x,
        _ => {
            let mut t_im1 = 1.0;
            let mut t_i = x;
            for _ in 2..=n {
                let t_next = 2.0 * x * t_i - t_im1;
                t_im1 = t_i;
                t_i = t_next;
            }
            t_i
        }
    }
}


/// Behavioral source, SPICE `B` card: `out = f(probes, time)` for an
/// arbitrary expression of node voltages and branch currents (TODO §5).
///
/// The expression text is kept verbatim (it is what `emit` reprints); its
/// compiled form lives in `behavior`. Probes bind late — [Circuit::
/// resolve_behavioral] replaces the name forms with node ids / branch indices
/// after node compaction, and must never *create* the probed node.
#[derive(Debug, Clone)]
pub struct BehavioralSource {
    pub name: String,
    pub out_p: NodeId,
    pub out_n: NodeId,
    pub kind: BehavioralKind,
    /// Expression text as written (after probe-name flattening).
    pub expr: String,
    /// Compiled AST; probe indices refer to `behavior.probes`.
    pub behavior: crate::behavior::Behavior,
    /// The PSpice ABM form this entry came from (`None` for a `B` card).
    /// Carries the frequency response of the dynamic forms and lets `emit`
    /// re-spell the ABM card shape.
    pub abm: Option<AbmForm>,
}

impl ControlledSource {
    /// The constant term of the polynomial (0 for a plain gain card).
    pub fn constant(&self) -> f64 {
        self.coeffs.first().copied().unwrap_or(0.0)
    }

    /// Coefficient applied to control variable `i` (0 when absent).
    pub fn coeff(&self, i: usize) -> f64 {
        self.coeffs.get(i + 1).copied().unwrap_or(0.0)
    }

    /// Linear shorthand: exactly one control with a pure gain.
    pub fn is_linear(&self) -> bool {
        self.controls.len() == 1 && self.constant() == 0.0 && self.coeffs.len() == 2
    }
}

// ─── Op-amps (U) ─────────────────────────────────────────────────────────────

/// An ideal differential op-amp (`U` card, TODO §5): the output pair is held
/// to `gain·(V(in+) − V(in−) + VOS)` on one branch row, like a VCVS driven
/// from a single-ended VCVS; `RIN`/`ROUT` pull in two admittance rungs when
/// their model slots are finite/non-zero (`'—'` defaults: neither). The bare
/// `U` (no model name) resolves to [`OpAmpModel::default`].
#[derive(Debug, Clone)]
pub struct OpAmp {
    pub name: String,
    pub out_p: NodeId,
    pub out_n: NodeId,
    pub in_p: NodeId,
    pub in_n: NodeId,
    /// `.model` card name; empty for a bare `U` (the documented defaults).
    pub model: String,
}

// ─── Switches (S/W) ─────────────────────────────────────────────────────────

/// What drives a switch: a differential voltage (`S`, nodes) or the branch
/// current of a named voltage source (`W`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchControl {
    Voltage(NodeId, NodeId),
    Current(String),
}

/// Switch model card (`.model m SW(RON=… ROFF=… VT=… VH=…)`).
#[derive(Debug, Clone)]
pub struct SwitchModel {
    /// On resistance RON (Ω).
    pub ron: f64,
    /// Off resistance ROFF (Ω).
    pub roff: f64,
    /// Threshold VT (V or A, per controlling quantity).
    pub vt: f64,
    /// Hysteresis half-width VH (same units as VT; 0 = none).
    pub vh: f64,
}

impl Default for SwitchModel {
    fn default() -> Self {
        Self { ron: 1.0, roff: 1e6, vt: 0.0, vh: 0.0 }
    }
}

impl SwitchModel {
    /// Conductance for a controlling value, honouring hysteresis around the
    /// previous state: with `VH > 0` the switch keeps its last state inside
    /// the band `(VT − VH/2, VT + VH/2)`.
    pub fn conductance(&self, control: f64, was_on: Option<bool>) -> (f64, bool) {
        let on = match was_on {
            Some(on) if self.vh > 0.0 => {
                if control > self.vt + 0.5 * self.vh {
                    true
                } else if control < self.vt - 0.5 * self.vh {
                    false
                } else {
                    on
                }
            }
            _ => control > self.vt,
        };
        if on { (1.0 / self.ron.max(1e-12), on) } else { (1.0 / self.roff.max(1e-12), on) }
    }
}

#[derive(Debug, Clone)]
pub struct Switch {
    pub name: String,
    pub n1: NodeId,
    pub n2: NodeId,
    pub control: SwitchControl,
    pub model: String,
    /// `ON`/`OFF` keyword on the card: a forced initial state, before the
    /// controlling quantity has been evaluated.
    pub initial: Option<bool>,
}

#[derive(Debug, Clone, Copy)]
pub struct Ac {
    pub mag: f64,
    pub phase: f64,
}

#[derive(Debug, Clone)]
pub struct Pulse {
    pub v1: f64,
    pub v2: f64,
    pub td: f64,
    pub tr: f64,
    pub tf: f64,
    pub pw: f64,
    pub per: f64,
}

impl Pulse {
    pub fn value_at(&self, t: f64) -> f64 {
        if t < self.td {
            return self.v1;
        }
        let t_rel = if self.per > 0.0 {
            (t - self.td) % self.per
        } else {
            t - self.td
        };
        if t_rel < self.tr {
            if self.tr > 0.0 {
                self.v1 + (self.v2 - self.v1) * (t_rel / self.tr)
            } else {
                self.v2
            }
        } else if t_rel < self.tr + self.pw {
            self.v2
        } else if t_rel < self.tr + self.pw + self.tf {
            if self.tf > 0.0 {
                let fall_time = t_rel - self.tr - self.pw;
                self.v2 - (self.v2 - self.v1) * (fall_time / self.tf)
            } else {
                self.v1
            }
        } else {
            self.v1
        }
    }

    /// Edge instants for a few periods after the delay (enough for the
    /// transient window; repeated callers clip to their own stop time).
    pub fn breakpoints(&self) -> Vec<f64> {
        let mut out = vec![self.td, self.td + self.tr];
        out.push(self.td + self.tr + self.pw);
        out.push(self.td + self.tr + self.pw + self.tf);
        if self.per > 0.0 {
            // A generous cap: 1000 periods of edges is more than any sane
            // simulation window needs, and keeps a runaway PER=1e-12 deck
            // from allocating forever.
            for k in 1..1000 {
                let base = self.td + self.per * k as f64;
                out.push(base);
                out.push(base + self.tr);
                out.push(base + self.tr + self.pw);
                out.push(base + self.tr + self.pw + self.tf);
            }
        }
        out
    }
}

use std::f64::consts::PI;

#[derive(Debug, Clone)]
pub struct Sine {
    pub vo: f64,
    pub va: f64,
    pub freq: f64,
    pub td: f64,
    pub theta: f64,
}

impl Sine {
    pub fn value_at(&self, t: f64) -> f64 {
        if t < self.td {
            return self.vo;
        }
        let t_rel = t - self.td;
        let damping = (-self.theta * t_rel).exp();
        let oscillation = (2.0 * PI * self.freq * t_rel).sin();
        self.vo + self.va * damping * oscillation
    }

    /// The delay is the only kink; the sinusoid itself is smooth, so the
    /// adaptive stepper samples it coarsely instead of at every crossing.
    pub fn breakpoints(&self) -> Vec<f64> {
        vec![self.td]
    }
}

/// Exponential source, SPICE `EXP(V1 V2 TD1 TAU1 TD2 TAU2)`.
#[derive(Debug, Clone)]
pub struct Exp {
    pub v1: f64,
    pub v2: f64,
    pub td1: f64,
    pub tau1: f64,
    pub td2: f64,
    pub tau2: f64,
}

impl Exp {
    pub fn value_at(&self, t: f64) -> f64 {
        if t < self.td1 {
            return self.v1;
        }
        // SPICE EXP: rise toward V2 with TAU1 from TD1, then add a decay
        // back toward V1 with TAU2 from TD2.
        let rise = if self.tau1 > 0.0 {
            1.0 - (-(t - self.td1) / self.tau1).exp()
        } else {
            1.0
        };
        let base = self.v1 + (self.v2 - self.v1) * rise;
        if t < self.td2 {
            base
        } else {
            let decay = if self.tau2 > 0.0 {
                1.0 - (-(t - self.td2) / self.tau2).exp()
            } else {
                1.0
            };
            base + (self.v1 - self.v2) * decay
        }
    }

    /// The corner instants the piecewise form changes slope at.
    pub fn breakpoints(&self) -> Vec<f64> {
        vec![self.td1, self.td2]
    }
}

/// Piecewise-linear source, `PWL(t1 v1 t2 v2 …)`.
///
/// `repeat_from` marks the start of an LTspice `REPEAT` block: points before
/// it play once, points from it to the last point repeat (`repeat_count`
/// `None` = forever, `Some(n)` = n additional cycles). Outside the waveform
/// the source holds its first/last value.
#[derive(Debug, Clone, Default)]
pub struct Pwl {
    pub points: Vec<(f64, f64)>,
    pub repeat_from: Option<f64>,
    pub repeat_count: Option<Option<usize>>,
}

impl Pwl {
    /// Sort by time and reject duplicate/backward times (loudly, at parse).
    pub fn validate(&self) -> std::result::Result<(), String> {
        let mut last = f64::NEG_INFINITY;
        for &(t, _) in &self.points {
            if !t.is_finite() || t < last {
                return Err("PWL times must be finite and non-decreasing".into());
            }
            last = t;
        }
        if self.points.is_empty() {
            return Err("PWL needs at least one (time, value) pair".into());
        }
        Ok(())
    }

    pub fn start_value(&self) -> f64 {
        self.points.first().map(|p| p.1).unwrap_or(0.0)
    }

    fn interpolate(&self, t: f64) -> f64 {
        let Some((&(t0, v0), rest)) = self.points.split_first() else {
            return 0.0;
        };
        if t <= t0 {
            return v0;
        }
        for &(t1, v1) in rest {
            if t <= t1 {
                if t1 <= t0 {
                    return v1;
                }
                let frac = (t - t0) / (t1 - t0);
                return v0 + (v1 - v0) * frac;
            }
        }
        self.points.last().map(|p| p.1).unwrap_or(v0)
    }

    pub fn value_at(&self, t: f64) -> f64 {
        let Some(start) = self.repeat_from else {
            return self.interpolate(t);
        };
        let Some(&(end, _)) = self.points.last() else {
            return 0.0;
        };
        if t <= start || end <= start {
            return self.interpolate(t.min(end));
        }
        let period = end - start;
        let elapsed = t - start;
        let cycle = (elapsed / period).floor();
        if let Some(Some(count)) = self.repeat_count {
            // `count` additional cycles after the block's first pass.
            if cycle >= (count + 1) as f64 {
                return self.interpolate(end);
            }
        }
        let local = start + elapsed % period;
        self.interpolate(local)
    }

    pub fn breakpoints(&self) -> Vec<f64> {
        let mut out: Vec<f64> = self.points.iter().map(|p| p.0).collect();
        if let (Some(start), Some(Some(count)), Some(&(end, _))) =
            (self.repeat_from, self.repeat_count, self.points.last())
        {
            let period = end - start;
            if period > 0.0 {
                for k in 0..=(count + 1) {
                    let base = start + period * k as f64;
                    for p in &self.points {
                        if p.0 >= start {
                            out.push(base + (p.0 - start));
                        }
                    }
                }
            }
        }
        out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        out
    }
}

/// Single-frequency FM source, `SFFM(VO VA FC MDI FS TD)`:
/// `VO + VA·sin(2π·FC·(t−TD) + MDI·sin(2π·FS·(t−TD)))`.
#[derive(Debug, Clone)]
pub struct Sffm {
    pub vo: f64,
    pub va: f64,
    pub fc: f64,
    pub mdi: f64,
    pub fs: f64,
    pub td: f64,
}

impl Sffm {
    pub fn value_at(&self, t: f64) -> f64 {
        if t < self.td {
            return self.vo;
        }
        let tau = t - self.td;
        self.vo
            + self.va
                * (2.0 * PI * self.fc * tau + self.mdi * (2.0 * PI * self.fs * tau).sin()).sin()
    }
}

/// Amplitude-modulated source, `AM(VA VO FM FC TD)`:
/// `VO + VA·(1 + sin(2π·FM·(t−TD)))·sin(2π·FC·(t−TD))`.
#[derive(Debug, Clone)]
pub struct Am {
    pub va: f64,
    pub vo: f64,
    pub fm: f64,
    pub fc: f64,
    pub td: f64,
}

impl Am {
    pub fn value_at(&self, t: f64) -> f64 {
        if t < self.td {
            return self.vo;
        }
        let tau = t - self.td;
        self.vo
            + self.va
                * (1.0 + (2.0 * PI * self.fm * tau).sin())
                * (2.0 * PI * self.fc * tau).sin()
    }
}

/// The time-domain behaviour of an independent `V`/`I` source. Exactly one
/// card may carry a waveform; a plain `DC` card has none.
#[derive(Debug, Clone)]
pub enum Waveform {
    Pulse(Pulse),
    Sine(Sine),
    Exp(Exp),
    Pwl(Pwl),
    Sffm(Sffm),
    Am(Am),
}

impl Waveform {
    pub fn value_at(&self, t: f64) -> f64 {
        match self {
            Waveform::Pulse(w) => w.value_at(t),
            Waveform::Sine(w) => w.value_at(t),
            Waveform::Exp(w) => w.value_at(t),
            Waveform::Pwl(w) => w.value_at(t),
            Waveform::Sffm(w) => w.value_at(t),
            Waveform::Am(w) => w.value_at(t),
        }
    }

    /// Value the waveform holds at `t = 0`, used as a source's DC/operating
    /// point when the card carries no explicit `DC` value.
    pub fn start_value(&self) -> f64 {
        match self {
            Waveform::Pulse(w) => w.v1,
            Waveform::Sine(w) => w.vo,
            Waveform::Exp(w) => w.v1,
            Waveform::Pwl(w) => w.start_value(),
            Waveform::Sffm(w) => w.vo,
            Waveform::Am(w) => w.vo,
        }
    }

    /// Instants where the waveform's piecewise definition changes slope; the
    /// transient stepper aligns steps to these so narrow events are not
    /// stepped over (TODO §3 discontinuity handling).
    pub fn breakpoints(&self) -> Vec<f64> {
        match self {
            Waveform::Pulse(w) => w.breakpoints(),
            Waveform::Sine(w) => w.breakpoints(),
            Waveform::Exp(w) => w.breakpoints(),
            Waveform::Pwl(w) => w.breakpoints(),
            Waveform::Sffm(_) => Vec::new(),
            Waveform::Am(_) => Vec::new(),
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Waveform::Pulse(_) => "PULSE",
            Waveform::Sine(_) => "SIN",
            Waveform::Exp(_) => "EXP",
            Waveform::Pwl(_) => "PWL",
            Waveform::Sffm(_) => "SFFM",
            Waveform::Am(_) => "AM",
        }
    }
}

#[derive(Debug, Clone)]
pub struct VoltageSource {
    pub name: String,
    pub n1: NodeId,
    pub n2: NodeId,
    /// DC/operating-point value when there is no waveform; ignored by
    /// [`VoltageSource::value_at`] when a waveform is present.
    pub voltage: f64,
    pub waveform: Option<Waveform>,
    pub ac: Option<Ac>,
}

impl VoltageSource {
    pub fn value_at(&self, t: f64) -> f64 {
        match &self.waveform {
            Some(w) => w.value_at(t),
            None => self.voltage,
        }
    }

    pub fn breakpoints(&self) -> Vec<f64> {
        self.waveform.as_ref().map(Waveform::breakpoints).unwrap_or_default()
    }
}

/// Independent current source, SPICE `I` card.
///
/// Positive current flows from `n1` through the source to `n2` (the SPICE
/// "n+ through the source to n−" convention), so the element injects `I`
/// into `n2` and pulls `I` out of `n1`.
#[derive(Debug, Clone)]
pub struct CurrentSource {
    pub name: String,
    pub n1: NodeId,
    pub n2: NodeId,
    pub current: f64,
    pub waveform: Option<Waveform>,
    pub ac: Option<Ac>,
}

impl CurrentSource {
    pub fn value_at(&self, t: f64) -> f64 {
        match &self.waveform {
            Some(w) => w.value_at(t),
            None => self.current,
        }
    }

    pub fn breakpoints(&self) -> Vec<f64> {
        self.waveform.as_ref().map(Waveform::breakpoints).unwrap_or_default()
    }
}

// ─── Element families (the named lookup interface) ──────────────────────────

/// The element kinds that carry a branch-current report, in the canonical
/// legend order R C L V D M Q J I B (see `CONTEXT.md`). One variant here
/// corresponds to one `Vec` in the report structures, and every consumer —
/// plots, CSV, `.meas` probes — walks the same list instead of re-spelling
/// the family walk per file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementFamily {
    Resistors,
    Capacitors,
    Inductors,
    VoltageSources,
    Diodes,
    Mosfets,
    Bjts,
    Jfets,
    CurrentSources,
    Behavioral,
    OpAmps,
}

/// Canonical family order: the glossary's R, C, L, V, D, M, I with the
/// three-terminal, behavioral and op-amp families appended where those
/// devices landed.
pub const CURRENT_FAMILIES: [ElementFamily; 11] = [
    ElementFamily::Resistors,
    ElementFamily::Capacitors,
    ElementFamily::Inductors,
    ElementFamily::VoltageSources,
    ElementFamily::Diodes,
    ElementFamily::Mosfets,
    ElementFamily::Bjts,
    ElementFamily::Jfets,
    ElementFamily::CurrentSources,
    ElementFamily::Behavioral,
    ElementFamily::OpAmps,
];

impl ElementFamily {
    /// The element letter the family is spoken with (label fallbacks).
    pub fn letter(self) -> &'static str {
        match self {
            Self::Resistors => "R",
            Self::Capacitors => "C",
            Self::Inductors => "L",
            Self::VoltageSources => "V",
            Self::Diodes => "D",
            Self::Mosfets => "M",
            Self::Bjts => "Q",
            Self::Jfets => "J",
            Self::CurrentSources => "I",
            Self::Behavioral => "B",
            Self::OpAmps => "U",
        }
    }

    /// The probe symbol printed before the name: `I(name)`, with the
    /// reference tools' `Ic`/`Id` spellings for the three-terminal devices.
    pub fn probe_prefix(self) -> &'static str {
        match self {
            Self::Bjts => "Ic",
            Self::Jfets => "Id",
            _ => "I",
        }
    }

    /// Human family word for error messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Resistors => "resistor",
            Self::Capacitors => "capacitor",
            Self::Inductors => "inductor",
            Self::VoltageSources => "voltage source",
            Self::Diodes => "diode",
            Self::Mosfets => "MOSFET",
            Self::Bjts => "BJT",
            Self::Jfets => "JFET",
            Self::CurrentSources => "current source",
            Self::Behavioral => "behavioral source",
            Self::OpAmps => "op-amp",
        }
    }
}

// ─── Circuit ─────────────────────────────────────────────────────────────────

/// The parsed deck, element by element, ready for matrix stamping.
///
/// Boundary contract: netlist parsing is the *only* production writer of
/// these fields (the GUI's schematic editor pushes R/C/L/V cards for
/// rendering only, and never simulates its hand-built circuit), so
/// [`Circuit::validate`] — run at the end of `parse` and again at the top
/// of every analysis pass — is the single gate for cross-reference and
/// positivity invariants. Matrix stamping trusts it: element lookups
/// re-resolve models only through [`crate::mna::device_model`], never with
/// a silent fallback.
#[derive(Debug, Clone, Default)]
pub struct Circuit {
    pub name: String,
    pub nodes: usize,
    /// Simulation temperature in °C (`.temp`, default 27). Device equations
    /// read it through the circuit so every analysis sees the same value.
    pub temperature: f64,
    /// Canonical displayed name of every node, indexed by `NodeId`
    /// (`"0"` for ground). Numeric nodes carry their decimal id so display
    /// and emitted text are unchanged; alphanumeric nodes keep the first
    /// spelling the parser saw.
    pub node_names: Vec<String>,
    pub resistors: Vec<Resistor>,
    pub(crate) resistor_models: HashMap<String, PassiveModel>,
    pub capacitors: Vec<Capacitor>,
    pub(crate) capacitor_models: HashMap<String, PassiveModel>,
    pub inductors: Vec<Inductor>,
    pub(crate) inductor_models: HashMap<String, PassiveModel>,
    pub(crate) mutual_inductances: Vec<MutualInductance>,
    pub voltage_sources: Vec<VoltageSource>,
    pub current_sources: Vec<CurrentSource>,
    pub diode_models: HashMap<String, DiodeModel>,
    pub diodes: Vec<Diode>,
    pub(crate) mosfet_models: HashMap<String, MosfetModel>,
    pub mosfets: Vec<Mosfet>,
    pub(crate) bjt_models: HashMap<String, BjtModel>,
    pub bjts: Vec<Bjt>,
    pub(crate) jfet_models: HashMap<String, JfetModel>,
    pub jfets: Vec<Jfet>,
    /// Voltage-controlled voltage sources (`E`), one branch row each.
    pub(crate) vcvs: Vec<ControlledSource>,
    /// Voltage-controlled current sources (`G`), no branch row.
    pub(crate) vccs: Vec<ControlledSource>,
    /// Current-controlled voltage sources (`H`), one branch row each.
    pub(crate) ccvs: Vec<ControlledSource>,
    /// Current-controlled current sources (`F`), no branch row.
    pub(crate) cccs: Vec<ControlledSource>,
    pub(crate) switch_models: HashMap<String, SwitchModel>,
    pub switches: Vec<Switch>,
    /// Behavioral sources (`B` cards) with compiled expressions.
    pub behavioral: Vec<BehavioralSource>,
    /// Ideal op-amps (`U`), one branch row each.
    pub opamps: Vec<OpAmp>,
    /// `.model … U(…)` op-amp cards.
    pub(crate) opamp_models: HashMap<String, OpAmpModel>,
    /// `.model` cards of device types RSpice does not model yet (NPN, PNP,
    /// RES, …). Parsed so third-party decks import cleanly; no element kind
    /// consumes them yet.
    pub(crate) generic_models: HashMap<String, GenericModel>,
}

/// Append `id` to the used-node list when not present (first-seen order).
fn collect_used_node(used: &mut Vec<usize>, id: NodeId) {
    if !used.contains(&id.0) {
        used.push(id.0);
    }
}

impl Circuit {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            nodes: 1,
            temperature: 27.0,
            node_names: vec!["0".to_string()],
            ..Default::default()
        }
    }

    /// Grow the node space to cover `id`. New slots default to their decimal
    /// id as name; a caller assigning a real name overwrites it afterwards.
    pub fn ensure_node(&mut self, id: usize) {
        if id >= self.nodes {
            let next = self.node_names.len();
            self.node_names.extend((next..=id).map(|n| n.to_string()));
            self.nodes = id + 1;
        }
    }

    /// Give node `id` a display name (no-op if it already has one — first
    /// spelling wins, matching the case-insensitive lookup).
    pub fn set_node_name(&mut self, id: usize, name: &str) {
        self.ensure_node(id);
        if self.node_names[id].is_empty() || self.node_names[id] == id.to_string() {
            self.node_names[id] = name.to_string();
        }
    }

    /// The node's canonical name, falling back to its decimal id.
    pub fn node_name(&self, id: usize) -> &str {
        self.node_names
            .get(id)
            .map(|s| s.as_str())
            .unwrap_or_default()
    }

    /// Nominal value of a passive element: the card's own value, or the one
    /// the referenced model card supplies when the card carried none.
    fn passive_value(element: f64, model: Option<&PassiveModel>) -> f64 {
        if element > 0.0 {
            element
        } else {
            model.and_then(|m| m.value).unwrap_or(element)
        }
    }

    fn passive_temperature(model: Option<&PassiveModel>, value: f64, temp_c: f64) -> f64 {
        let Some(model) = model else { return value };
        let dt = temp_c - 27.0;
        value * (1.0 + model.tc1 * dt + model.tc2 * dt * dt)
    }

    /// Resistor value at the circuit temperature (TC1/TC2 applied).
    pub fn resistance_at(&self, r: &Resistor) -> f64 {
        let model = r
            .model
            .as_ref()
            .and_then(|name| self.resistor_models.get(name));
        let nominal = Self::passive_value(r.resistance, model);
        Self::passive_temperature(model, nominal, self.temperature)
    }

    /// Capacitor value at the circuit temperature.
    pub fn capacitance_at(&self, c: &Capacitor) -> f64 {
        let model = c
            .model
            .as_ref()
            .and_then(|name| self.capacitor_models.get(name));
        let nominal = Self::passive_value(c.capacitance, model);
        Self::passive_temperature(model, nominal, self.temperature)
    }

    /// Inductor value at the circuit temperature.
    pub fn inductance_at(&self, l: &Inductor) -> f64 {
        let model = l
            .model
            .as_ref()
            .and_then(|name| self.inductor_models.get(name));
        let nominal = Self::passive_value(l.inductance, model);
        Self::passive_temperature(model, nominal, self.temperature)
    }

    /// The op-amp model parameters one `U` element resolves to: a named card
    /// when it exists, the documented defaults for a bare `U`. `validate()`
    /// has already rejected unknown model names, so the default returned
    /// for a stale name is only reached through non-validated paths.
    pub fn opamp_params_at(&self, model: &str) -> OpAmpModel {
        if model.is_empty() {
            return OpAmpModel::default();
        }
        self.opamp_models.get(model).cloned().unwrap_or_default()
    }

    /// Split every diode with a nonzero `RS` into a series resistor and an
    /// internal junction node. Model cards may appear after their elements, so
    /// this runs once at the end of parsing rather than in the element walk.
    pub fn expand_diode_series_resistance(&mut self) {
        let mut additions = Vec::new();
        for i in 0..self.diodes.len() {
            let (anode, name, area) = {
                let d = &self.diodes[i];
                (d.anode, d.name.clone(), d.area)
            };
            let Some(model) = self.diode_models.get(&self.diodes[i].model) else {
                continue;
            };
            let rs = model.rs / area.max(1e-12);
            if rs <= 0.0 {
                continue;
            }
            let internal = self.nodes;
            self.ensure_node(internal);
            self.set_node_name(internal, &format!("{name}.int"));
            self.diodes[i].anode = NodeId(internal);
            additions.push(Resistor {
                name: format!("{name}_RS"),
                n1: anode,
                n2: NodeId(internal),
                resistance: rs,
                model: None,
            });
        }
        self.resistors.extend(additions);
    }

    /// Split the JFET `RD`/`RS` model parasitics the same way the diode `RS`
    /// expansion does: each nonzero slot becomes a real resistor between the
    /// external terminal and a fresh internal node, and the carrier's
    /// terminal rewires to the internal side. Runs after the deck walk so the
    /// model tables are final, and before node compaction so the fresh nodes
    /// join the hole-free renumbering. The card value divides by AREA like
    /// the diode slot.
    pub fn expand_jfet_series_resistance(&mut self) {
        let mut additions = Vec::new();
        for i in 0..self.jfets.len() {
            let (nd, ns, name, area) = {
                let j = &self.jfets[i];
                (j.nd, j.ns, j.name.clone(), j.area)
            };
            let Some(model) = self.jfet_models.get(&self.jfets[i].model) else {
                continue;
            };
            let rd = model.rd / area.max(1e-12);
            let rs = model.rs / area.max(1e-12);
            if rd <= 0.0 && rs <= 0.0 {
                continue;
            }
            if rd > 0.0 {
                let internal = self.nodes;
                self.ensure_node(internal);
                self.set_node_name(internal, &format!("{name}.dint"));
                self.jfets[i].nd = NodeId(internal);
                additions.push(Resistor {
                    name: format!("{name}_RD"),
                    n1: nd,
                    n2: NodeId(internal),
                    resistance: rd,
                    model: None,
                });
            }
            if rs > 0.0 {
                let internal = self.nodes;
                self.ensure_node(internal);
                self.set_node_name(internal, &format!("{name}.sint"));
                self.jfets[i].ns = NodeId(internal);
                additions.push(Resistor {
                    name: format!("{name}_RS"),
                    n1: ns,
                    n2: NodeId(internal),
                    resistance: rs,
                    model: None,
                });
            }
        }
        self.resistors.extend(additions);
    }

    /// Split the BJT `RE`/`RC`/`RB` model resistance slots exactly like the
    /// diode `RS` and JFET `RD/RS` expansions: each nonzero slot becomes a
    /// real resistor between the external terminal and a fresh internal node
    /// (`{name}.eint`/`.cint`/`.bint`) and the carrier's terminal rewires to
    /// the internal side. Runs after the deck walk so the model tables are
    /// final, and before node compaction so the fresh nodes join the
    /// hole-free renumbering. The card value divides by AREA; the optional
    /// substrate node (`Q c b e [s] model`) is untouched.
    pub fn expand_bjt_series_resistance(&mut self) {
        let mut additions = Vec::new();
        for i in 0..self.bjts.len() {
            let (nc, nb, ne, name, area) = {
                let q = &self.bjts[i];
                (q.nc, q.nb, q.ne, q.name.clone(), q.area)
            };
            let Some(model) = self.bjt_models.get(&self.bjts[i].model) else {
                continue;
            };
            let re = model.re / area.max(1e-12);
            let rc = model.rc / area.max(1e-12);
            let rb = model.rb / area.max(1e-12);
            if re <= 0.0 && rc <= 0.0 && rb <= 0.0 {
                continue;
            }
            for (series, slot, internal_suffix, terminal, arm_name) in [
                (re, 0, "eint", ne, format!("{name}_RE")),
                (rc, 1, "cint", nc, format!("{name}_RC")),
                (rb, 2, "bint", nb, format!("{name}_RB")),
            ] {
                if series <= 0.0 {
                    continue;
                }
                let internal = self.nodes;
                self.ensure_node(internal);
                self.set_node_name(internal, &format!("{name}.{internal_suffix}"));
                match slot {
                    0 => self.bjts[i].ne = NodeId(internal),
                    1 => self.bjts[i].nc = NodeId(internal),
                    _ => self.bjts[i].nb = NodeId(internal),
                }
                additions.push(Resistor {
                    name: arm_name,
                    n1: terminal,
                    n2: NodeId(internal),
                    resistance: series,
                    model: None,
                });
            }
        }
        self.resistors.extend(additions);
    }

    /// Split passive-model parasitics (TODO §5): a nonzero `RSER`/`LSER` on a
    /// model card becomes a real resistor/inductor arm in series with the
    /// carrier device, and `CPAR`/`RPAR` become parallel arms across the
    /// carrier's original terminals. Like the diode `RS` expansion this runs
    /// after the model tables are final and before node compaction, so the
    /// fresh internal nodes join the hole-free renumbering. Arms are only
    /// generated for nonzero slots, which keeps `validate`'s value checks
    /// (R > 0, C > 0, L > 0) true for every generated element.
    pub fn expand_parasitics(&mut self) {
        self.expand_resistor_parasitics();
        self.expand_capacitor_parasitics();
        self.expand_inductor_parasitics();
    }

    /// The parasitic slots of the carrier's model card, cloned so the caller
    /// can borrow `&mut self` while working from them. `None` when the card
    /// is absent or entirely parasitic-free.
    fn resistor_parasitic_slot(&self, index: usize) -> Option<PassiveModel> {
        let model = self.resistors[index].model.as_deref()?;
        self.resistor_models
            .get(model)
            .cloned()
            .filter(|m| m.has_parasitics())
    }

    fn capacitor_parasitic_slot(&self, index: usize) -> Option<PassiveModel> {
        let model = self.capacitors[index].model.as_deref()?;
        self.capacitor_models
            .get(model)
            .cloned()
            .filter(|m| m.has_parasitics())
    }

    fn inductor_parasitic_slot(&self, index: usize) -> Option<PassiveModel> {
        let model = self.inductors[index].model.as_deref()?;
        self.inductor_models
            .get(model)
            .cloned()
            .filter(|m| m.has_parasitics())
    }

    /// Build the `n`-th internal node of a carrier: the first hangs off
    /// `.int` like the diode `RS` expansion, further stages count up.
    fn new_parasitic_node(&mut self, carrier_name: &str, stage: usize) -> NodeId {
        let internal = self.nodes;
        self.ensure_node(internal);
        let suffix = if stage == 1 {
            "int".to_string()
        } else {
            format!("int{stage}")
        };
        self.set_node_name(internal, &format!("{carrier_name}.{suffix}"));
        NodeId(internal)
    }

    /// Chain the series slots (RSER first, then LSER) left of the carrier:
    /// each nonzero arm sits between the previous node and a fresh internal
    /// node, and the returned node becomes the carrier's left terminal.
    fn apply_series_slots(
        &mut self,
        slots: &PassiveModel,
        carrier_name: &str,
        external_left: NodeId,
    ) -> NodeId {
        let mut left = external_left;
        let mut stage = 1;
        for (value, kind) in [
            (slots.rser, ParasiticArmKind::SeriesResistor),
            (slots.lser, ParasiticArmKind::SeriesInductor),
        ] {
            if value <= 0.0 {
                continue;
            }
            let right = self.new_parasitic_node(carrier_name, stage);
            match kind {
                ParasiticArmKind::SeriesResistor => self.resistors.push(Resistor {
                    name: format!("R{carrier_name}_RSER"),
                    n1: left,
                    n2: right,
                    resistance: value,
                    model: None,
                }),
                ParasiticArmKind::SeriesInductor => self.inductors.push(Inductor {
                    name: format!("L{carrier_name}_LSER"),
                    n1: left,
                    n2: right,
                    inductance: value,
                    model: None,
                }),
            }
            left = right;
            stage += 1;
        }
        left
    }

    /// Stamp the parallel slots (RPAR, then CPAR) across the carrier's
    /// original external terminals.
    fn apply_parallel_slots(
        &mut self,
        slots: &PassiveModel,
        carrier_name: &str,
        n1: NodeId,
        n2: NodeId,
    ) {
        if slots.rpar > 0.0 {
            self.resistors.push(Resistor {
                name: format!("R{carrier_name}_RPAR"),
                n1,
                n2,
                resistance: slots.rpar,
                model: None,
            });
        }
        if slots.cpar > 0.0 {
            self.capacitors.push(Capacitor {
                name: format!("C{carrier_name}_CPAR"),
                n1,
                n2,
                capacitance: slots.cpar,
                model: None,
            });
        }
    }

    fn expand_resistor_parasitics(&mut self) {
        for i in 0..self.resistors.len() {
            let Some(slots) = self.resistor_parasitic_slot(i) else {
                continue;
            };
            let (n1, n2, name) = {
                let r = &self.resistors[i];
                (r.n1, r.n2, r.name.clone())
            };
            let left = self.apply_series_slots(&slots, &name, n1);
            self.resistors[i].n1 = left;
            self.apply_parallel_slots(&slots, &name, n1, n2);
        }
    }

    fn expand_capacitor_parasitics(&mut self) {
        for i in 0..self.capacitors.len() {
            let Some(slots) = self.capacitor_parasitic_slot(i) else {
                continue;
            };
            let (n1, n2, name) = {
                let c = &self.capacitors[i];
                (c.n1, c.n2, c.name.clone())
            };
            let left = self.apply_series_slots(&slots, &name, n1);
            self.capacitors[i].n1 = left;
            self.apply_parallel_slots(&slots, &name, n1, n2);
        }
    }

    fn expand_inductor_parasitics(&mut self) {
        for i in 0..self.inductors.len() {
            let Some(slots) = self.inductor_parasitic_slot(i) else {
                continue;
            };
            let (n1, n2, name) = {
                let l = &self.inductors[i];
                (l.n1, l.n2, l.name.clone())
            };
            let left = self.apply_series_slots(&slots, &name, n1);
            self.inductors[i].n1 = left;
            self.apply_parallel_slots(&slots, &name, n1, n2);
        }
    }

    /// Renumber the used node ids to `0,1,2,…` (ground stays 0, other ids keep
    /// their relative order) and move the names along.
    ///
    /// Node addressing is row-index-based everywhere downstream of parsing, so
    /// a hole — an id with no element attached, which happens whenever a named
    /// node sits above an unused numeric id — would leave an unstamped MNA row
    /// and a singular matrix. Compaction closes the hole; the display names
    /// (`node_names`) preserve every node's original spelling, numeric ones
    /// included.
    pub fn compact_node_ids(&mut self) {
        let mut used: Vec<usize> = Vec::with_capacity(self.node_names.len());
        for r in &self.resistors {
            collect_used_node(&mut used, r.n1);
            collect_used_node(&mut used, r.n2);
        }
        for c in &self.capacitors {
            collect_used_node(&mut used, c.n1);
            collect_used_node(&mut used, c.n2);
        }
        for l in &self.inductors {
            collect_used_node(&mut used, l.n1);
            collect_used_node(&mut used, l.n2);
        }
        for d in &self.diodes {
            collect_used_node(&mut used, d.anode);
            collect_used_node(&mut used, d.cathode);
        }
        for v in &self.voltage_sources {
            collect_used_node(&mut used, v.n1);
            collect_used_node(&mut used, v.n2);
        }
        for i in &self.current_sources {
            collect_used_node(&mut used, i.n1);
            collect_used_node(&mut used, i.n2);
        }
        for m in &self.mosfets {
            collect_used_node(&mut used, m.nd);
            collect_used_node(&mut used, m.ng);
            collect_used_node(&mut used, m.ns);
            collect_used_node(&mut used, m.nb);
        }
        for q in &self.bjts {
            collect_used_node(&mut used, q.nc);
            collect_used_node(&mut used, q.nb);
            collect_used_node(&mut used, q.ne);
            if let Some(ns) = q.ns {
                collect_used_node(&mut used, ns);
            }
        }
        for j in &self.jfets {
            collect_used_node(&mut used, j.nd);
            collect_used_node(&mut used, j.ng);
            collect_used_node(&mut used, j.ns);
        }
        for c in self.vcvs.iter().chain(&self.vccs).chain(&self.ccvs).chain(&self.cccs) {
            collect_used_node(&mut used, c.out_p);
            collect_used_node(&mut used, c.out_n);
            for cv in &c.controls {
                if let ControlVar::Voltage(a, b) = cv {
                    collect_used_node(&mut used, *a);
                    collect_used_node(&mut used, *b);
                }
            }
        }
        for s in &self.switches {
            collect_used_node(&mut used, s.n1);
            collect_used_node(&mut used, s.n2);
            if let SwitchControl::Voltage(a, b) = s.control {
                collect_used_node(&mut used, a);
                collect_used_node(&mut used, b);
            }
        }
        for b in &self.behavioral {
            collect_used_node(&mut used, b.out_p);
            collect_used_node(&mut used, b.out_n);
        }
        for u in &self.opamps {
            collect_used_node(&mut used, u.out_p);
            collect_used_node(&mut used, u.out_n);
            collect_used_node(&mut used, u.in_p);
            collect_used_node(&mut used, u.in_n);
        }
        used.sort_unstable();
        // `used` is sorted and unique, so when the ids are already 0..n-1 in
        // order the map is the identity and there is nothing to compact. A
        // deck with no elements at all keeps its default ground slot.
        if used.is_empty()
            || (used.len() == self.nodes && used.iter().enumerate().all(|(i, &u)| i == u))
        {
            return;
        }
        let map: HashMap<usize, usize> =
            used.iter().enumerate().map(|(new, &old)| (old, new)).collect();
        let old_names = std::mem::take(&mut self.node_names);
        let mut new_names = vec![String::new(); used.len()];
        for (old, name) in old_names.iter().enumerate() {
            if let Some(&new) = map.get(&old) {
                new_names[new] = name.clone();
            }
        }
        let compact = |id: &mut NodeId| id.0 = map[&id.0];
        for r in &mut self.resistors { compact(&mut r.n1); compact(&mut r.n2); }
        for c in &mut self.capacitors { compact(&mut c.n1); compact(&mut c.n2); }
        for l in &mut self.inductors { compact(&mut l.n1); compact(&mut l.n2); }
        for d in &mut self.diodes { compact(&mut d.anode); compact(&mut d.cathode); }
        for v in &mut self.voltage_sources { compact(&mut v.n1); compact(&mut v.n2); }
        for i in &mut self.current_sources { compact(&mut i.n1); compact(&mut i.n2); }
        for m in &mut self.mosfets {
            compact(&mut m.nd); compact(&mut m.ng); compact(&mut m.ns); compact(&mut m.nb);
        }
        for q in &mut self.bjts {
            compact(&mut q.nc);
            compact(&mut q.nb);
            compact(&mut q.ne);
            if let Some(ns) = &mut q.ns {
                compact(ns);
            }
        }
        for j in &mut self.jfets {
            compact(&mut j.nd);
            compact(&mut j.ng);
            compact(&mut j.ns);
        }
        for c in self
            .vcvs
            .iter_mut()
            .chain(&mut self.vccs)
            .chain(&mut self.ccvs)
            .chain(&mut self.cccs)
        {
            compact(&mut c.out_p);
            compact(&mut c.out_n);
            for cv in &mut c.controls {
                if let ControlVar::Voltage(a, b) = cv {
                    compact(a);
                    compact(b);
                }
            }
        }
        for s in &mut self.switches {
            compact(&mut s.n1);
            compact(&mut s.n2);
            if let SwitchControl::Voltage(a, b) = &mut s.control {
                compact(a);
                compact(b);
            }
        }
        for b in &mut self.behavioral {
            compact(&mut b.out_p);
            compact(&mut b.out_n);
        }
        for u in &mut self.opamps {
            compact(&mut u.out_p);
            compact(&mut u.out_n);
            compact(&mut u.in_p);
            compact(&mut u.in_n);
        }
        self.node_names = new_names;
        self.nodes = used.len();
    }

    // ─── Behavioral probe binding ────────────────────────────────────────

    /// Bind the probes of every behavioral source against the final node
    /// space and element tables (called after `compact_node_ids`).
    ///
    /// Bindings never create nodes: a probe names something the deck already
    /// declared, so a missing name is a loud parse error, not a floating net.
    /// Inside `subckt` bodies element/net names carry the instance path; a
    /// probe that names one without its path falls back to the card's own
    /// instance prefix (`B1` inside X1 reading `V(sum)` matches `X1.sum`).
    pub fn resolve_behavioral(&mut self) -> Result<()> {
        if self.behavioral.is_empty() {
            return Ok(());
        }
        for i in 0..self.behavioral.len() {
            let card = self.behavioral[i].name.clone();
            let mut probes = std::mem::take(&mut self.behavioral[i].behavior.probes);
            for probe in probes.iter_mut() {
                match probe {
                    ProbeBinding::Unresolved(ProbeSpec::Voltage(a, b)) => {
                        let plus = self.probe_node_id(a, &card)?;
                        let minus = match b {
                            Some(name) => self.probe_node_id(name, &card)?,
                            None => NodeId(0),
                        };
                        *probe = ProbeBinding::Voltage(plus, minus);
                    }
                    ProbeBinding::Unresolved(ProbeSpec::Current(src)) => {
                        *probe = self.probe_branch(src, &card)?;
                    }
                    _ => {}
                }
            }
            self.behavioral[i].behavior.probes = probes;
        }
        Ok(())
    }

    /// Resolve one probe node name: exact (case-insensitive) first, then the
    /// probe spelled without its instance path.
    fn probe_node_id(&self, token: &str, card: &str) -> Result<NodeId> {
        if let Some(id) = self.node_id(token) {
            return Ok(id);
        }
        if let Some(dot) = card.rfind('.') {
            let candidate = format!("{}.{}", &card[..dot], token);
            if let Some(id) = self.node_id(&candidate) {
                return Ok(id);
            }
        }
        Err(SimError::Parse(format!(
            "behavioral source '{}' probes unknown node '{}'",
            card, token
        )))
    }

    /// Resolve `I(src)`: an independent voltage source or inductor, again
    /// with the card's own instance prefix as the fallback spelling.
    fn probe_branch(&self, src: &str, card: &str) -> Result<ProbeBinding> {
        let mut spellings: Vec<String> = vec![src.to_string()];
        if let Some(dot) = card.rfind('.') {
            spellings.push(format!("{}.{}", &card[..dot], src));
        }
        for cand in &spellings {
            if let Some(idx) = self
                .voltage_sources
                .iter()
                .position(|v| v.name.eq_ignore_ascii_case(cand))
            {
                return Ok(ProbeBinding::SourceCurrent(idx));
            }
            if let Some(idx) = self
                .inductors
                .iter()
                .position(|l| l.name.eq_ignore_ascii_case(cand))
            {
                return Ok(ProbeBinding::InductorCurrent(idx));
            }
        }
        Err(SimError::Parse(format!(
            "behavioral source '{}' reads current through '{}', which is not \
             an independent voltage source or inductor",
            card, src
        )))
    }

    // ─── Element families (the named lookup interface) ──────────────────

    /// Display names of one family, positioned exactly as the family's
    /// report `Vec`. This — not an index guess — is what "which element is
    /// row i" is asked of.
    pub fn element_names(&self, family: ElementFamily) -> Vec<String> {
        match family {
            ElementFamily::Resistors => self.resistors.iter().map(|e| e.name.clone()).collect(),
            ElementFamily::Capacitors => self.capacitors.iter().map(|e| e.name.clone()).collect(),
            ElementFamily::Inductors => self.inductors.iter().map(|e| e.name.clone()).collect(),
            ElementFamily::VoltageSources => {
                self.voltage_sources.iter().map(|e| e.name.clone()).collect()
            }
            ElementFamily::Diodes => self.diodes.iter().map(|e| e.name.clone()).collect(),
            ElementFamily::Mosfets => self.mosfets.iter().map(|e| e.name.clone()).collect(),
            ElementFamily::Bjts => self.bjts.iter().map(|e| e.name.clone()).collect(),
            ElementFamily::Jfets => self.jfets.iter().map(|e| e.name.clone()).collect(),
            ElementFamily::CurrentSources => {
                self.current_sources.iter().map(|e| e.name.clone()).collect()
            }
            ElementFamily::Behavioral => self.behavioral.iter().map(|e| e.name.clone()).collect(),
            ElementFamily::OpAmps => self.opamps.iter().map(|e| e.name.clone()).collect(),
        }
    }

    /// Where `name` lives for an `I(name)` probe: family plus index into the
    /// family's report `Vec`, scanned in canonical order. Every consumer
    /// resolves through here, so a new element family is added in one place
    /// (the per-family chains this replaces forgot Q the moment BJT landed).
    pub fn resolve_current_probe(&self, name: &str) -> Option<(ElementFamily, usize)> {
        CURRENT_FAMILIES.iter().copied().find_map(|family| {
            self.element_names(family)
                .iter()
                .position(|n| n.eq_ignore_ascii_case(name))
                .map(|i| (family, i))
        })
    }

    /// The net a probe name refers to: ground (`0`/`gnd` — the alias lore in
    /// one place), then the case-insensitive first spelling from
    /// `node_names`.
    pub fn node_id(&self, name: &str) -> Option<NodeId> {
        let lower = name.to_ascii_lowercase();
        if lower == "0" || lower == "gnd" {
            return Some(NodeId(0));
        }
        self.node_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
            .map(|i| NodeId(i))
    }

    /// The six branch-row families in stamp order, one count each: voltage
    /// sources, inductors, VCVS `E`, CCVS `H`, the voltage-flavour `B`
    /// cards (the only behavioural flavour with a branch row), and op-amp
    /// `U` cards.
    /// `BranchLayout` — the layout owner — and `tran_matrix_size` both
    /// derive from this one count, so the twin row-counters this replaces
    /// can no longer drift apart.
    pub fn branch_family_counts(&self) -> [usize; 6] {
        [
            self.voltage_sources.len(),
            self.inductors.len(),
            self.vcvs.len(),
            self.ccvs.len(),
            self.behavioral
                .iter()
                .filter(|b| b.kind == BehavioralKind::Voltage)
                .count(),
            self.opamps.len(),
        ]
    }

    /// Branch rows plus node rows: the size of the transient matrix.
    pub fn tran_matrix_size(&self) -> usize {
        self.nodes.saturating_sub(1) + self.branch_family_counts().iter().sum::<usize>()
    }

    pub fn validate(&self) -> Result<()> {
        if self.nodes < 1 {
            return Err(SimError::Circuit("circuit has no nodes".into()));
        }
        for r in &self.resistors {
            if let Some(name) = &r.model {
                if !self.resistor_models.contains_key(name) {
                    return Err(SimError::Circuit(format!(
                        "resistor {} references unknown model '{}'", r.name, name
                    )));
                }
            }
            if self.resistance_at(r) <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "resistor {} must have R > 0 (card or model)", r.name
                )));
            }
        }
        for c in &self.capacitors {
            if let Some(name) = &c.model {
                if !self.capacitor_models.contains_key(name) {
                    return Err(SimError::Circuit(format!(
                        "capacitor {} references unknown model '{}'", c.name, name
                    )));
                }
            }
            if self.capacitance_at(c) <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "capacitor {} must have C > 0 (card or model)", c.name
                )));
            }
        }
        for l in &self.inductors {
            if let Some(name) = &l.model {
                if !self.inductor_models.contains_key(name) {
                    return Err(SimError::Circuit(format!(
                        "inductor {} references unknown model '{}'", l.name, name
                    )));
                }
            }
            if self.inductance_at(l) <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "inductor {} must have L > 0 (card or model)", l.name
                )));
            }
        }
        for d in &self.diodes {
            if d.area <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "diode {} must have AREA > 0", d.name
                )));
            }
            if !self.diode_models.contains_key(&d.model) {
                return Err(SimError::Circuit(format!(
                    "diode {} references unknown model '{}'", d.name, d.model
                )));
            }
        }
        for m in &self.mosfets {
            if m.w <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "MOSFET {} must have W > 0", m.name
                )));
            }
            if m.l <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "MOSFET {} must have L > 0", m.name
                )));
            }
            if m.ad < 0.0 || m.asr < 0.0 || m.ps < 0.0 || m.pd < 0.0 {
                return Err(SimError::Circuit(format!(
                    "MOSFET {} junction sizing (AD/AS/PS/PD) cannot be negative", m.name
                )));
            }
            if !self.mosfet_models.contains_key(&m.model) {
                return Err(SimError::Circuit(format!(
                    "MOSFET {} references unknown model '{}'", m.name, m.model
                )));
            }
            let level = self.mosfet_models[&m.model].level;
            if level != 1 {
                return Err(SimError::Circuit(format!(
                    "MOSFET {} references model '{}' on level {level}; only level 1 is supported",
                    m.name, m.model
                )));
            }
        }
        for v in &self.voltage_sources {
            if let Some(w) = &v.waveform {
                validate_waveform("voltage source", &v.name, w)?;
            }
        }
        for i in &self.current_sources {
            if let Some(w) = &i.waveform {
                validate_waveform("current source", &i.name, w)?;
            }
        }
        for c in self
            .vcvs
            .iter()
            .chain(&self.vccs)
            .chain(&self.ccvs)
            .chain(&self.cccs)
        {
            if c.controls.is_empty() {
                return Err(SimError::Circuit(format!(
                    "controlled source {} needs at least one control variable",
                    c.name
                )));
            }
            if c.coeffs.len() != c.controls.len() + 1 {
                return Err(SimError::Circuit(format!(
                    "controlled source {} has {} control(s) but {} coefficient(s) — \
                     expected a constant plus one per control",
                    c.name,
                    c.controls.len(),
                    c.coeffs.len()
                )));
            }
            for cv in &c.controls {
                if let ControlVar::Current(src) = cv {
                    if !self
                        .voltage_sources
                        .iter()
                        .any(|v| v.name.eq_ignore_ascii_case(src))
                    {
                        return Err(SimError::Circuit(format!(
                            "controlled source {} reads current through '{src}', \
                             which is not an independent voltage source",
                            c.name
                        )));
                    }
                }
            }
        }
        for s in &self.switches {
            let model = self.switch_models.get(&s.model).ok_or_else(|| {
                SimError::Circuit(format!(
                    "switch {} references unknown model '{}'",
                    s.name, s.model
                ))
            })?;
            if model.ron <= 0.0 || model.roff <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "switch {} model '{}' needs RON > 0 and ROFF > 0",
                    s.name, s.model
                )));
            }
            if let SwitchControl::Current(src) = &s.control {
                if !self
                    .voltage_sources
                    .iter()
                    .any(|v| v.name.eq_ignore_ascii_case(src))
                {
                    return Err(SimError::Circuit(format!(
                        "switch {} is controlled by current through '{src}', \
                         which is not an independent voltage source",
                        s.name
                    )));
                }
            }
        }
        for u in &self.opamps {
            let (model_label, params) = if u.model.is_empty() {
                ("(bare U)", self.opamp_params_at(""))
            } else {
                let name = u.model.as_str();
                if !self.opamp_models.contains_key(name) {
                    return Err(SimError::Circuit(format!(
                        "op-amp {} references unknown model '{}'", u.name, name
                    )));
                }
                (name, self.opamp_params_at(name))
            };
            if !params.gain.is_finite() || params.gain == 0.0 {
                return Err(SimError::Circuit(format!(
                    "op-amp {} model '{}' needs a finite, non-zero GAIN",
                    u.name, model_label
                )));
            }
            if params.rin != f64::INFINITY && params.rin <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "op-amp {} model '{}' needs RIN > 0 (or unset for ∞)",
                    u.name, model_label
                )));
            }
            if params.rout < 0.0 {
                return Err(SimError::Circuit(format!(
                    "op-amp {} model '{}' needs ROUT >= 0",
                    u.name, model_label
                )));
            }
        }
        for q in &self.bjts {
            if q.area <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "BJT {} must have AREA > 0", q.name
                )));
            }
            if !self.bjt_models.contains_key(&q.model) {
                return Err(SimError::Circuit(format!(
                    "BJT {} references unknown model '{}'", q.name, q.model
                )));
            }
        }
        for j in &self.jfets {
            if j.area <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "JFET {} must have AREA > 0", j.name
                )));
            }
            if !self.jfet_models.contains_key(&j.model) {
                return Err(SimError::Circuit(format!(
                    "JFET {} references unknown model '{}'", j.name, j.model
                )));
            }
        }
        for b in &self.behavioral {
            if b.out_p == b.out_n {
                return Err(SimError::Circuit(format!(
                    "behavioral source {} has identical output terminals",
                    b.name
                )));
            }
            if b.behavior.probes.iter().any(|p| p.is_unresolved()) {
                return Err(SimError::Circuit(format!(
                    "behavioral source {} has unresolved probes — the parse \
                     pipeline binds them via resolve_behavioral()",
                    b.name
                )));
            }
        }
        for k in &self.mutual_inductances {
            if !(k.coupling > 0.0 && k.coupling <= 1.0) {
                return Err(SimError::Circuit(format!(
                    "mutual inductance {} needs 0 < k <= 1, got {}",
                    k.name, k.coupling
                )));
            }
            for name in [&k.inductor1, &k.inductor2] {
                if !self.inductors.iter().any(|l| l.name.eq_ignore_ascii_case(name)) {
                    return Err(SimError::Circuit(format!(
                        "mutual inductance {} references unknown inductor '{name}'",
                        k.name
                    )));
                }
            }
            if k.inductor1.eq_ignore_ascii_case(&k.inductor2) {
                return Err(SimError::Circuit(format!(
                    "mutual inductance {} couples an inductor to itself",
                    k.name
                )));
            }
        }
        Ok(())
    }
}
fn validate_waveform(kind: &str, name: &str, w: &Waveform) -> Result<()> {
    match w {
        Waveform::Pulse(p) => {
            if p.td < 0.0 || p.tr < 0.0 || p.tf < 0.0 || p.pw < 0.0 || p.per < 0.0 {
                return Err(SimError::Circuit(format!(
                    "{kind} {name} pulse timings cannot be negative"
                )));
            }
            if p.per > 0.0 && p.per < (p.tr + p.pw + p.tf) {
                return Err(SimError::Circuit(format!(
                    "{kind} {name} pulse period must be greater than active duration (tr + pw + tf)"
                )));
            }
        }
        Waveform::Sine(s) => {
            if s.freq < 0.0 || s.td < 0.0 {
                return Err(SimError::Circuit(format!(
                    "{kind} {name} sine frequency and delay cannot be negative"
                )));
            }
        }
        Waveform::Exp(e) => {
            if e.td1 < 0.0 || e.td2 < 0.0 || e.tau1 < 0.0 || e.tau2 < 0.0 {
                return Err(SimError::Circuit(format!(
                    "{kind} {name} EXP delays and time constants cannot be negative"
                )));
            }
        }
        Waveform::Pwl(p) => {
            p.validate().map_err(|e| {
                SimError::Circuit(format!("{kind} {name} PWL: {e}"))
            })?;
        }
        Waveform::Sffm(s) => {
            if s.td < 0.0 {
                return Err(SimError::Circuit(format!(
                    "{kind} {name} SFFM delay cannot be negative"
                )));
            }
        }
        Waveform::Am(a) => {
            if a.td < 0.0 {
                return Err(SimError::Circuit(format!(
                    "{kind} {name} AM delay cannot be negative"
                )));
            }
        }
    }
    Ok(())
}
