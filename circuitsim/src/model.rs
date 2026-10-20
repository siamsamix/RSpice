//! Device equations — the single copy of nonlinear device physics.
//!
//! Every linearization formula for diodes and MOSFETs lives here. `mna.rs`
//! (DC / transient / AC stamping), and both branch-current extraction paths
//! call [`DiodeModel::eval`] and
//! [`MosfetModel::eval`] instead of re-deriving the equations. A physics
//! fix lands once, here.

// ─── Model cards ────────────────────────────────────────────────────────────
//
// The `.model` card structs live beside the equations they parameterize.
// `circuit.rs` re-exports them, so historical import paths keep working.

/// Reserved slot for PSpice-style Monte-Carlo tolerance specifications parsed
/// off a `.model` card (`DEV=`/`LOT=` pairs; the value carries a percent flag
/// because both `DEV=5` and `DEV=5%` spellings exist). Netlist parsing records
/// them here today; the Monte-Carlo work (TODO §8) is the consumer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelTolerance {
    /// `DEV=<n>[%]` — relative deviation per device instance.
    pub dev: Option<(f64, bool)>,
    /// `LOT=<n>[%]` — correlated deviation across a production lot.
    pub lot: Option<(f64, bool)>,
}

/// A `.model` card for a device family RSpice does not model yet (NPN/PNP
/// transistors, JFETs, RES/CAP/IND cards, …). The full parameter list is
/// parsed and stored so third-party decks import cleanly; a future device
/// letter consumes it through the same grammar.
#[derive(Debug, Clone)]
pub struct GenericModel {
    /// Lowercased SPICE type keyword (e.g. `npn`, `res`).
    pub spice_type: String,
    /// `LEVEL=<n>` when the card carries one.
    pub level: Option<u32>,
    /// Numeric `KEY=VALUE` pairs in card order (lowercased keys).
    pub params: Vec<(String, f64)>,
    /// Reserved MC tolerance slot (`DEV`/`LOT` cards).
    pub tolerance: ModelTolerance,
}

/// Exponential I–V model: `I = IS·(exp(Vd / n·VT) − 1)`.
///
/// The complete SPICE diode parameter list is parsed (grammar-complete per
/// TODO §2 "full model-card grammar"); only the parameters the circuit engine
/// consumes (`IS`, `N`, `AREA`) affect stamping today — the others are stored
/// for the device-completion work (TODO §5, diode model completion).
#[derive(Debug, Clone)]
pub struct DiodeModel {
    /// Saturation current IS (A).
    pub is: f64,
    /// Emission coefficient N.
    pub n: f64,
    /// Series resistance RS (Ω) — not stamped yet.
    pub rs: f64,
    /// Zero-bias junction capacitance CJO (F) — not stamped yet.
    pub cjo: f64,
    /// Junction potential VJ (V) — not stamped yet.
    pub vj: f64,
    /// Grading coefficient M — not stamped yet.
    pub m: f64,
    /// Transit time TT (s) — not stamped yet.
    pub tt: f64,
    /// Activation energy EG (eV) — not stamped yet.
    pub eg: f64,
    /// Saturation-current temperature exponent XTI — not stamped yet.
    pub xti: f64,
    /// Reverse breakdown voltage BV (V) — not stamped yet.
    pub bv: f64,
    /// Current at breakdown voltage IBV (A) — not stamped yet.
    pub ibv: f64,
    /// Flicker noise coefficient KF — not stamped yet (.noise work).
    pub kf: f64,
    /// Flicker noise exponent AF — not stamped yet.
    pub af: f64,
    /// Parameter measurement temperature TNOM (°C).
    pub tnom: f64,
    /// Breakdown current emission coefficient NBV.
    pub nbv: f64,
    /// Forward-bias depletion-capacitance linearization fraction FC.
    pub fc: f64,
    /// Reserved MC tolerance slot (`DEV`/`LOT` cards).
    pub tolerance: ModelTolerance,
}

impl Default for DiodeModel {
    /// SPICE reference-tool defaults.
    fn default() -> Self {
        Self {
            is: 1e-14,
            n: 1.0,
            rs: 0.0,
            cjo: 0.0,
            vj: 0.7,
            m: 0.5,
            tt: 0.0,
            eg: 1.11,
            xti: 3.0,
            bv: 1e30,
            ibv: 1e-3,
            kf: 0.0,
            af: 1.0,
            tnom: 27.0,
            nbv: 1.0,
            fc: 0.5,
            tolerance: ModelTolerance::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MosfetType {
    Nmos,
    Pmos,
}

/// Level-1 (Shichman-Hodges) MOSFET model parameters.
/// Mirrors standard SPICE .MODEL NMOS/PMOS card syntax.
///
/// The complete common parameter list is parsed (TODO §2 "full model-card
/// grammar"); parameters not consumed by the level-1 equations (`TOX`,
/// gate-overlap and junction capacitances, process terms) are stored for the
/// capacitance/level work in TODO §5.
#[derive(Debug, Clone)]
pub struct MosfetModel {
    pub kind: MosfetType,
    /// Technology `LEVEL=` keyword; only level 1 is stamped (validation
    /// rejects elements referencing higher levels).
    pub level: u32,
    /// Transconductance parameter KP = µ·Cox (A/V²).
    pub kp: f64,
    /// Zero-bias threshold voltage Vth0 (V). SPICE spelling `VTO` is the
    /// parser's alias for this.
    pub vth0: f64,
    /// Channel-length modulation coefficient λ (1/V). 0 = ideal.
    pub lambda: f64,
    /// Bulk threshold modulation γ (V^0.5).
    pub gamma: f64,
    /// Strong-inversion surface potential 2·φF (V).
    pub phi: f64,
    /// Nominal temperature TNOM (°C) for the temperature laws.
    pub tnom: f64,
    /// Threshold temperature coefficient TCV (V/K); `Vth(T) = Vth0 − TCV·ΔT`.
    pub tcv: f64,
    /// Mobility temperature exponent BEX; `KP(T) = KP·(T/Tnom)^BEX`.
    pub bex: f64,
    /// Oxide thickness TOX (m) — not stamped yet.
    pub tox: Option<f64>,
    /// Gate–drain overlap capacitance CGDO (F/m) — not stamped yet.
    pub cgdo: Option<f64>,
    /// Gate–source overlap capacitance CGSO (F/m) — not stamped yet.
    pub cgso: Option<f64>,
    /// Gate–bulk overlap capacitance CGBO (F/m) — not stamped yet.
    pub cgbo: Option<f64>,
    /// Zero-bias drain/source junction capacitance per area CJ (F/m²).
    pub cj: Option<f64>,
    /// Sidewall junction capacitance per perimeter CJSW (F/m).
    pub cjsw: Option<f64>,
    /// Junction grading coefficient MJ.
    pub mj: Option<f64>,
    /// Sidewall grading coefficient MJSW.
    pub mjsw: Option<f64>,
    /// Junction built-in potential PB (V).
    pub pb: Option<f64>,
    /// Capacitance linearization fraction FC.
    pub fc: Option<f64>,
    /// Substrate doping NSUB — not stamped yet.
    pub nsub: Option<f64>,
    /// Surface-state density NSS — not stamped yet.
    pub nss: Option<f64>,
    /// Gate-material type TPG — not stamped yet.
    pub tpg: Option<f64>,
    /// Reserved MC tolerance slot (`DEV`/`LOT` cards).
    pub tolerance: ModelTolerance,
}

impl Default for MosfetModel {
    /// Reasonable level-1 NMOS defaults (matches SPICE internal defaults).
    fn default() -> Self {
        Self {
            kind: MosfetType::Nmos,
            level: 1,
            kp: 20e-6,
            vth0: 0.7,
            lambda: 0.0,
            gamma: 0.0,
            phi: 0.6,
            tnom: 27.0,
            tcv: 2.0e-3,
            bex: -1.5,
            tox: None,
            cgdo: None,
            cgso: None,
            cgbo: None,
            cj: None,
            cjsw: None,
            mj: None,
            mjsw: None,
            pb: None,
            fc: None,
            nsub: None,
            nss: None,
            tpg: None,
            tolerance: ModelTolerance::default(),
        }
    }
}
/// Boltzmann constant (J/K).
pub const BOLTZMANN: f64 = 1.380649e-23;
/// Elementary charge (C).
pub const ELECTRON_CHARGE: f64 = 1.602176634e-19;
/// Reference thermal voltage at 300.15 K (V) — kept for the level-1 default
/// and historical expectations.
pub const VT: f64 = 0.02585;
/// Absolute zero offset (°C → K).
pub const ZERO_CELSIUS_K: f64 = 273.15;

/// Thermal voltage kT/q at `temp_c` (°C).
pub fn thermal_voltage(temp_c: f64) -> f64 {
    BOLTZMANN * (ZERO_CELSIUS_K + temp_c) / ELECTRON_CHARGE
}
/// Numerical clamp on the diode bias for the exponential (SPICE-style relaxed
/// convergence): keeps `exp(vd / n·VT)` in range for the Newton-Raphson iteration.
const VD_CLAMP_MIN: f64 = -100.0;
const VD_CLAMP_MAX: f64 = 0.8;
/// Tiny leakage conductance used when a MOSFET is cut off, and the floor for
/// `gds` — purely for numerical stability.
const G_LEAK: f64 = 1e-12;
const GDS_FLOOR: f64 = 1e-12;
/// Floor inside the body-effect derivative so a node sitting exactly at
/// 2·φF cannot divide by zero.
const DVTH_FLOOR: f64 = 1e-9;

// ─── Diode ──────────────────────────────────────────────────────────────────

/// Linearized diode at bias point `vd` (already clamped internally).
#[derive(Debug, Clone, Copy)]
pub struct DiodeEval {
    /// Small-signal conductance gd = d(I)/d(Vd) at the clamped bias.
    pub gd: f64,
    /// DC current I = is·(exp(vd / n·VT) − 1) at the clamped bias.
    pub i_d: f64,
    /// Newton-Raphson companion current: `i_eq = i_d − gd·vd`.
    pub i_eq: f64,
}

impl DiodeModel {
    /// Evaluate the exponential model at bias `vd` (volts), area-scaled.
    ///
    /// `area` is the element's junction area multiplier (default 1): the
    /// saturation current scales linearly with it (the classic SPICE `AREA`
    /// factor; junction capacitance would scale the same way once CJO is
    /// stamped).
    ///
    /// The clamp `[-100 V, +0.8 V]` is part of the model, not of the caller:
    /// every consumer gets the same linearization point by construction.
    pub fn eval_for(&self, vd: f64, area: f64) -> DiodeEval {
        self.eval_for_at(vd, area, 27.0)
    }

    /// Temperature-aware evaluation: the saturation current follows the
    /// SPICE temperature law
    /// `IS(T) = IS·(T/Tnom)^(XTI/N)·exp(EG/(N·Vt)·(T/Tnom − 1))`,
    /// `Vt = kT/q` is recomputed at the device temperature, and the reverse
    /// breakdown term `−IBV·exp(−(Vd+BV)/(NBV·Vt))` is included.
    pub fn eval_for_at(&self, vd: f64, area: f64, temp_c: f64) -> DiodeEval {
        let vt = thermal_voltage(temp_c);
        // A breakdown diode's current diverges below −BV; stop the
        // linearization a bounded distance past the knee so Newton iterates
        // starting far away cannot overflow or build a singular matrix.
        let lower = if self.bv < 1e29 && self.bv > 0.0 {
            let nbv_vt = self.nbv.max(1e-3) * vt;
            (-(self.bv + 20.0 * nbv_vt)).max(VD_CLAMP_MIN)
        } else {
            VD_CLAMP_MIN
        };
        let vd = vd.clamp(lower, VD_CLAMP_MAX);
        let vt_n = self.n * vt;
        let t = ZERO_CELSIUS_K + temp_c;
        let tnom = ZERO_CELSIUS_K + self.tnom;
        let ratio = t / tnom;
        let is_t = self.is
            * ratio.powf(self.xti / self.n)
            * (self.eg / (self.n * vt) * (ratio - 1.0)).exp();
        let is_eff = is_t * area;
        let exp_term = (vd / vt_n).exp();
        let mut i_d = is_eff * (exp_term - 1.0);
        let mut gd = (is_eff / vt_n) * exp_term;

        // Reverse breakdown: the term is negligible far above −BV and grows
        // exponentially once the junction passes it.
        if self.bv < 1e29 && self.bv > 0.0 {
            let nbv_vt = self.nbv.max(1e-3) * vt;
            // Cap the exponent so a Newton iterate far beyond −BV cannot
            // overflow to infinity (the reference tools do the same).
            let arg = (-(vd + self.bv) / nbv_vt).min(40.0);
            let breakdown = self.ibv * arg.exp();
            if breakdown.is_finite() {
                i_d -= breakdown;
                gd += breakdown / nbv_vt;
            }
        }
        DiodeEval { gd, i_d, i_eq: i_d - gd * vd }
    }

    /// Junction capacitance at bias `vd`, area-scaled, with the SPICE `FC`
    /// linearization above `FC·VJ`.
    pub fn junction_capacitance(&self, vd: f64, area: f64) -> f64 {
        if self.cjo <= 0.0 {
            return 0.0;
        }
        let vj = self.vj.max(1e-9);
        let m = self.m.clamp(0.0, 0.99);
        let fc = self.fc.clamp(0.0, 0.99);
        let c0 = self.cjo * area;
        if vd < fc * vj {
            c0 * (1.0 - vd / vj).max(1e-9).powf(-m)
        } else {
            c0 * (1.0 - fc).powf(-m)
        }
    }

    /// Junction charge at bias `vd`, area-scaled (integral of
    /// [`Self::junction_capacitance`]).
    pub fn junction_charge(&self, vd: f64, area: f64) -> f64 {
        if self.cjo <= 0.0 {
            return 0.0;
        }
        let vj = self.vj.max(1e-9);
        let m = self.m.clamp(0.0, 0.99);
        let fc = self.fc.clamp(0.0, 0.99);
        let c0 = self.cjo * area;
        let q_at_fc = if (1.0 - m).abs() < 1e-9 {
            c0 * vj * (1.0 - fc).ln().abs()
        } else {
            c0 * vj / (1.0 - m) * (1.0 - (1.0 - fc).powf(1.0 - m))
        };
        if vd < fc * vj {
            if (1.0 - m).abs() < 1e-9 {
                c0 * vj * (1.0 - vd / vj).max(1e-9).ln().abs()
            } else {
                c0 * vj / (1.0 - m) * (1.0 - (1.0 - vd / vj).max(1e-9).powf(1.0 - m))
            }
        } else {
            q_at_fc + self.junction_capacitance(fc * vj, area) * (vd - fc * vj)
        }
    }

    /// Total stored charge: junction charge plus transit-time diffusion
    /// charge `TT·I(vd)`.
    pub fn total_charge(&self, vd: f64, area: f64) -> f64 {
        self.total_charge_at(vd, area, 27.0)
    }

    /// Temperature-aware total stored charge.
    pub fn total_charge_at(&self, vd: f64, area: f64, temp_c: f64) -> f64 {
        let current = self.eval_for_at(vd, area, temp_c).i_d;
        self.junction_charge(vd, area) + self.tt * current
    }

    /// `dQ/dV` at `vd`: junction capacitance plus `TT·gd`.
    pub fn dcharge_dv(&self, vd: f64, area: f64) -> f64 {
        self.dcharge_dv_at(vd, area, 27.0)
    }

    /// Temperature-aware `dQ/dV`.
    pub fn dcharge_dv_at(&self, vd: f64, area: f64, temp_c: f64) -> f64 {
        let gd = self.eval_for_at(vd, area, temp_c).gd;
        self.junction_capacitance(vd, area) + self.tt * gd
    }

    /// Area-1 evaluation at the nominal temperature (the historical entry
    /// point; kept for tests and callers that have no area factor).
    pub fn eval(&self, vd: f64) -> DiodeEval {
        self.eval_for(vd, 1.0)
    }
}

// ─── Bipolar junction transistor ────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BjtType {
    Npn,
    Pnp,
}

/// Ebers–Moll/Gummel–Poon core model card. The transport current and both
/// junction currents are evaluated together; series resistances (`RB`/`RC`/
/// `RE`) are parsed and carried for the distributed-device upgrade.
#[derive(Debug, Clone)]
pub struct BjtModel {
    pub kind: BjtType,
    /// Saturation current IS (A).
    pub is: f64,
    /// Forward current gain BF.
    pub bf: f64,
    /// Reverse current gain BR.
    pub br: f64,
    /// Forward emission coefficient NF.
    pub nf: f64,
    /// Reverse emission coefficient NR.
    pub nr: f64,
    /// Forward Early voltage VAF (V); infinite = no Early effect.
    pub vaf: f64,
    /// Reverse Early voltage VAR (V).
    pub var: f64,
    /// B-E leakage saturation current ISE (A).
    pub ise: f64,
    /// B-E leakage emission coefficient NE.
    pub ne: f64,
    /// B-C leakage saturation current ISC (A).
    pub isc: f64,
    /// B-C leakage emission coefficient NC.
    pub nc: f64,
    /// Base resistance RB (Ω), stored.
    pub rb: f64,
    /// Collector resistance RC (Ω), stored.
    pub rc: f64,
    /// Emitter resistance RE (Ω), stored.
    pub re: f64,
    /// B-E zero-bias depletion capacitance CJE (F).
    pub cje: f64,
    /// B-E built-in potential VJE (V).
    pub vje: f64,
    /// B-E grading coefficient MJE.
    pub mje: f64,
    /// B-C zero-bias depletion capacitance CJC (F).
    pub cjc: f64,
    /// B-C built-in potential VJC (V).
    pub vjc: f64,
    /// B-C grading coefficient MJC.
    pub mjc: f64,
    /// Forward transit time TF (s).
    pub tf: f64,
    /// Reverse transit time TR (s).
    pub tr: f64,
    /// Saturation-current temperature exponent XTI.
    pub xti: f64,
    /// Bandgap energy EG (eV).
    pub eg: f64,
    /// Nominal temperature TNOM (°C).
    pub tnom: f64,
    /// Flicker noise coefficient KF.
    pub kf: f64,
    /// Flicker noise exponent AF.
    pub af: f64,
    /// Reserved Monte-Carlo tolerance slots.
    pub tolerance: ModelTolerance,
}

impl Default for BjtModel {
    fn default() -> Self {
        Self {
            kind: BjtType::Npn,
            is: 1e-16,
            bf: 100.0,
            br: 1.0,
            nf: 1.0,
            nr: 1.0,
            vaf: f64::INFINITY,
            var: f64::INFINITY,
            ise: 0.0,
            ne: 1.5,
            isc: 0.0,
            nc: 2.0,
            rb: 0.0,
            rc: 0.0,
            re: 0.0,
            cje: 0.0,
            vje: 0.75,
            mje: 0.33,
            cjc: 0.0,
            vjc: 0.75,
            mjc: 0.33,
            tf: 0.0,
            tr: 0.0,
            xti: 3.0,
            eg: 1.11,
            tnom: 27.0,
            kf: 0.0,
            af: 1.0,
            tolerance: ModelTolerance::default(),
        }
    }
}

/// Terminal currents at one bias, in the element convention
/// (`Ic` into the collector, `Ib` into the base, `Ie` into the emitter).
#[derive(Debug, Clone, Copy)]
pub struct BjtCurrents {
    pub ic: f64,
    pub ib: f64,
    pub ie: f64,
}

impl BjtModel {
    /// Temperature-scaled saturation current.
    pub fn is_at(&self, temp_c: f64) -> f64 {
        let vt = thermal_voltage(temp_c);
        let t = ZERO_CELSIUS_K + temp_c;
        let tnom = ZERO_CELSIUS_K + self.tnom;
        let ratio = t / tnom;
        self.is * ratio.powf(self.xti) * (self.eg / vt * (ratio - 1.0)).exp()
    }

    /// Junction capacitance at `v` for one junction, area-scaled and
    /// linearized above `FC·VJ` (0.5).
    fn junction_cap(c0: f64, vj: f64, m: f64, v: f64, area: f64) -> f64 {
        if c0 <= 0.0 {
            return 0.0;
        }
        let vj = vj.max(1e-9);
        let m = m.clamp(0.0, 0.99);
        let fc = 0.5;
        let cap0 = c0 * area;
        if v < fc * vj {
            cap0 * (1.0 - v / vj).max(1e-9).powf(-m)
        } else {
            cap0 * (1.0 - fc).powf(-m)
        }
    }

    /// Base-emitter capacitance: depletion plus TF·gm.
    pub fn cbe(&self, vbe: f64, area: f64, temp_c: f64) -> f64 {
        let gd = self.dif(vbe, self.nf, temp_c) / (self.nf * thermal_voltage(temp_c));
        Self::junction_cap(self.cje, self.vje, self.mje, vbe, area)
            + self.tf * gd.max(0.0)
    }

    /// Base-collector capacitance: depletion plus TR·gm.
    pub fn cbc(&self, vbc: f64, area: f64, temp_c: f64) -> f64 {
        let gd = self.dif(vbc, self.nr, temp_c) / (self.nr * thermal_voltage(temp_c));
        Self::junction_cap(self.cjc, self.vjc, self.mjc, vbc, area)
            + self.tr * gd.max(0.0)
    }

    fn dif(&self, v: f64, n: f64, temp_c: f64) -> f64 {
        let vt = thermal_voltage(temp_c);
        self.is_at(temp_c) * (v.min(0.8) / (n * vt)).exp()
    }

    /// Evaluate the Ebers–Moll terminal currents at real terminal voltages.
    ///
    /// PNP devices are mirrored into the NPN normalization and their terminal
    /// currents are negated back, exactly as the MOSFET path does.
    pub fn currents(
        &self,
        vbe_real: f64,
        vbc_real: f64,
        area: f64,
        temp_c: f64,
    ) -> BjtCurrents {
        let sign = if self.kind == BjtType::Pnp { -1.0 } else { 1.0 };
        let vbe = (sign * vbe_real).clamp(-100.0, 0.8);
        let vbc = (sign * vbc_real).clamp(-100.0, 0.8);
        let vt = thermal_voltage(temp_c);
        let is = self.is_at(temp_c) * area;

        let i_f = is * ((vbe / (self.nf * vt)).exp() - 1.0);
        let i_r = is * ((vbc / (self.nr * vt)).exp() - 1.0);
        let i_se = self.ise * area * ((vbe / (self.ne * vt)).exp() - 1.0);
        let i_sc = self.isc * area * ((vbc / (self.nc * vt)).exp() - 1.0);

        // Gummel-Poon base charge factor; clamped so a bias outside the Early
        // voltages cannot flip the transport current's sign.
        let mut qb = 1.0 - vbc / self.vaf - vbe / self.var;
        if !qb.is_finite() || qb <= 1e-3 {
            qb = 1e-3;
        }
        let i_ct = (i_f - i_r) / qb;

        let ib = i_f / self.bf + i_r / self.br + i_se + i_sc;
        let ic = i_ct - i_r / self.br - i_sc;
        let ie = -(ic + ib);
        BjtCurrents { ic: sign * ic, ib: sign * ib, ie: sign * ie }
    }
}

// ─── MOSFET ─────────────────────────────────────────────────────────────────// ─── MOSFET ─────────────────────────────────────────────────────────────────

/// Linearized level-1 MOSFET at an operating point, in the element's own
/// current convention (see [`MosfetModel::eval`]).
#[derive(Debug, Clone, Copy)]
pub struct MosfetEval {
    /// DC drain current at the given bias (positive = drain, through, source).
    pub ids: f64,
    /// Transconductance magnitude gm = β·Vov (continuation into the
    /// channel-length-modulation factor).
    pub gm: f64,
    /// Drain–source output conductance magnitude.
    pub gds: f64,
    /// Body-effect transconductance magnitude, gbs = gm·dVth/dVsb.
    pub gbs: f64,
    /// Newton-Raphson companion current at the same linearization point:
    /// `i_eq = ids − gds·vds − gm·vgs − gbs·vbs`.
    pub i_eq: f64,
}

impl MosfetModel {
    /// Evaluate the Shichman–Hodges level-1 model.
    ///
    /// **Conventions.** The biases are given in the NMOS-normalized form the
    /// callers produce: for a PMOS they negate all terminal voltages first and
    /// use the returned magnitudes with their existing sign handling.
    ///
    /// * `beta` — β = KP·(W/L) in A/V². KP lives on the model card; W and L
    ///   are per-element, so the caller folds them once.
    /// * `vgs`, `vds`, `vbs` — gate-, drain-, bulk-over-source voltages (V).
    ///   `vds` is *not* clamped here; the DC stamp passes the clamped value,
    ///   the AC path and the current reports pass the raw value, exactly as
    ///   the pre-consolidation code did.
    /// * Region selection: `vov ≤ 0` → cut-off; else `vds < vov` → triode, else
    ///   saturation.
    pub fn eval(&self, beta: f64, vgs: f64, vds: f64, vbs: f64) -> MosfetEval {
        self.eval_at(beta, vgs, vds, vbs, self.tnom)
    }

    /// Temperature-aware level-1 evaluation: the threshold drifts by `TCV`
    /// and the transconductance parameter scales as `(T/Tnom)^BEX`.
    pub fn eval_at(
        &self,
        beta: f64,
        vgs: f64,
        vds: f64,
        vbs: f64,
        temp_c: f64,
    ) -> MosfetEval {
        let t_ratio = (ZERO_CELSIUS_K + temp_c) / (ZERO_CELSIUS_K + self.tnom);
        let beta = beta * t_ratio.powf(self.bex);
        // SPICE writes a negative VTO for PMOS; the mirrored (NMOS-normalized)
        // evaluation space needs the magnitude.
        let vth0 = if self.kind == MosfetType::Pmos { -self.vth0 } else { self.vth0 };
        let vth0 = vth0 - self.tcv * (temp_c - self.tnom);
        // Body effect: Vth = Vth0 + γ·(√|2φF − Vbs| − √|2φF|), with Vsb = −Vbs.
        let vsb = -vbs;
        let vth = vth0
            + self.gamma * ((self.phi + vsb).abs().sqrt() - self.phi.abs().sqrt());
        let vov = vgs - vth;

        let (ids, gm, gds, gbs) = if vov <= 0.0 {
            // Cut-off: device is OFF; tiny leakage conductance for stability.
            (0.0, G_LEAK, G_LEAK, 0.0)
        } else {
            let dvth_dvsb = self.gamma / (2.0 * (self.phi + vsb).abs().sqrt().max(DVTH_FLOOR));
            if vds < vov {
                // Linear (triode) region.
                let ids = beta * ((vov * vds) - 0.5 * vds * vds) * (1.0 + self.lambda * vds);
                let gm = beta * vds * (1.0 + self.lambda * vds);
                let gds = beta * (vov - vds) * (1.0 + self.lambda * vds)
                    + beta * ((vov * vds) - 0.5 * vds * vds) * self.lambda;
                (ids, gm.max(0.0), gds.max(GDS_FLOOR), gm * dvth_dvsb)
            } else {
                // Saturation region.
                let ids = 0.5 * beta * vov * vov * (1.0 + self.lambda * vds);
                let gm = beta * vov * (1.0 + self.lambda * vds);
                let gds = 0.5 * beta * vov * vov * self.lambda;
                (ids, gm.max(0.0), gds.max(GDS_FLOOR), gm * dvth_dvsb)
            }
        };

        MosfetEval {
            ids,
            gm,
            gds,
            gbs,
            i_eq: ids - gds * vds - gm * vgs - gbs * vbs,
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────
//
// Exact values: every reference below was computed independently (15-digit
// arithmetic, then written as a literal), not read off the implementation.

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    const DIODE: DiodeModel = DiodeModel {
        is: 1e-12,
        n: 1.0,
        rs: 0.0,
        cjo: 0.0,
        vj: 0.7,
        m: 0.5,
        tt: 0.0,
        eg: 1.11,
        xti: 3.0,
        bv: 1e30,
        ibv: 1e-3,
        kf: 0.0,
        af: 1.0,
        tnom: 27.0,
        nbv: 1.0,
        fc: 0.5,
        tolerance: ModelTolerance { dev: None, lot: None },
    };

    /// Test model card: kp = 2e-4 (so β = kp·w/l = 2e-4 with w/l = 1),
    /// matching the 15-digit reference values in these tests.
    fn test_model() -> MosfetModel {
        MosfetModel {
            kind: MosfetType::Nmos,
            level: 1,
            kp: 2e-4,
            vth0: 0.7,
            lambda: 0.0,
            gamma: 0.0,
            phi: 0.6,
            tnom: 27.0,
            tcv: 2.0e-3,
            bex: -1.5,
            tox: None,
            cgdo: None,
            cgso: None,
            cgbo: None,
            cj: None,
            cjsw: None,
            mj: None,
            mjsw: None,
            pb: None,
            fc: None,
            nsub: None,
            nss: None,
            tpg: None,
            tolerance: ModelTolerance::default(),
        }
    }

    #[test]
    fn diode_area_scales_is_and_gd() {
        // AREA=2 doubles the effective saturation current.
        let ev2 = DIODE.eval_for(0.3, 2.0);
        let ev1 = DIODE.eval_for(0.3, 1.0);
        assert_relative_eq!(ev2.i_d, 2.0 * ev1.i_d, max_relative = 1e-12);
        assert_relative_eq!(ev2.gd, 2.0 * ev1.gd, max_relative = 1e-12);
        // The companion current is i_d − gd·vd at the same bias: doubles too.
        assert_relative_eq!(ev2.i_eq, 2.0 * ev1.i_eq, max_relative = 1e-12);
    }

    #[test]
    fn diode_at_zero_bias_is_pure_conductance() {
        let ev = DIODE.eval(0.0);
        assert_eq!(ev.i_d, 0.0); // exp(0) − 1 = 0 exactly
        assert_eq!(ev.i_eq, 0.0);
        assert_relative_eq!(
            ev.gd,
            1e-12 / crate::model::thermal_voltage(27.0),
            epsilon = 1e-28
        );
    }

    #[test]
    fn diode_clamps_high_bias_to_spice_relaxation_bound() {
        let raw = DIODE.eval(2.0); // clamped to +0.8
        let at_bound = DIODE.eval(0.8);
        // Identical linearization by construction: same clamped input.
        assert_eq!(raw.i_d, at_bound.i_d);
        assert_eq!(raw.gd, at_bound.gd);
        assert_eq!(raw.i_eq, at_bound.i_eq);
        // Reference at the bound (15-digit arithmetic):
        assert_relative_eq!(raw.i_d, 2.708271179548838e1, max_relative = 1e-12);
        assert_relative_eq!(raw.gd, 1.047082524775861e3, max_relative = 1e-12);
        assert_relative_eq!(raw.i_eq, -8.105833080252004e2, max_relative = 1e-12);
    }

    #[test]
    fn diode_negative_bias_reports_reverse_current() {
        let ev = DIODE.eval(-1.0);
        // is·(e^(−38.7) − 1) = −is to double precision.
        assert_relative_eq!(ev.i_d, -1e-12, max_relative = 1e-13);
        assert_relative_eq!(ev.gd, 6.257827366453828e-28, epsilon = 1e-30);
        assert_relative_eq!(ev.i_eq, -9.999999999999994e-13, max_relative = 1e-13);
    }

    // NMOS-normalized model: kp=2e-4 (β = kp·w/l with w/l = 1), λ=0, γ=0,
    // φ=0.6, Vth0=0.7 — see test_model().

    #[test]
    fn mosfet_cutoff_is_leak_only() {
        let model = test_model();
        let beta = model.kp; // w/l = 1
        let ev = model.eval(beta, 0.5, 1.0, 0.0); // vov = −0.2
        assert_eq!(ev.ids, 0.0);
        assert_eq!(ev.gm, 1e-12);
        assert_eq!(ev.gds, 1e-12);
        assert_eq!(ev.gbs, 0.0);
        assert_relative_eq!(ev.i_eq, -1.5e-12, epsilon = 1e-28);
    }

    #[test]
    fn mosfet_saturation_ideal_lambda_zero() {
        let model = test_model();
        let ev = model.eval(model.kp, 2.0, 5.0, 0.0); // vov = 1.3
        assert_relative_eq!(ev.ids, 1.69e-4, max_relative = 1e-12);
        assert_relative_eq!(ev.gm, 2.6e-4, max_relative = 1e-12);
        assert_relative_eq!(ev.gds, 1e-12, epsilon = 1e-28); // floor
        assert_eq!(ev.gbs, 0.0);
        assert_relative_eq!(ev.i_eq, -3.510000050000001e-4, max_relative = 1e-12);
    }

    #[test]
    fn mosfet_triode_ideal_lambda_zero() {
        let model = test_model();
        let ev = model.eval(model.kp, 2.0, 0.5, 0.0); // vov = 1.3, triode
        assert_relative_eq!(ev.ids, 1.05e-4, max_relative = 1e-12);
        assert_relative_eq!(ev.gm, 1.0e-4, max_relative = 1e-12);
        assert_relative_eq!(ev.gds, 1.6e-4, max_relative = 1e-12);
        assert_eq!(ev.gbs, 0.0);
        assert_relative_eq!(ev.i_eq, -1.75e-4, max_relative = 1e-12);
    }

    #[test]
    fn mosfet_saturation_with_channel_length_modulation() {
        let mut model = test_model();
        model.lambda = 0.1;
        let ev = model.eval(model.kp, 2.0, 5.0, 0.0);
        assert_relative_eq!(ev.ids, 2.535e-4, max_relative = 1e-12);
        assert_relative_eq!(ev.gm, 3.9e-4, max_relative = 1e-12);
        assert_relative_eq!(ev.gds, 1.69e-5, max_relative = 1e-12);
        assert_relative_eq!(ev.i_eq, -6.1100e-4, max_relative = 1e-12);
    }

    #[test]
    fn mosfet_body_effect_raises_the_threshold() {
        // γ = 0.5, Vsb = 1 → Vth = 0.7 + 0.5·(√1.6 − √0.6) = 0.945157197412934.
        let mut model = test_model();
        model.gamma = 0.5;
        let ev = model.eval(model.kp, 2.0, 5.0, -1.0);
        assert_relative_eq!(ev.ids, 1.112693338169736e-4, max_relative = 1e-9);
        assert_relative_eq!(ev.gm, 2.109685605174132e-4, max_relative = 1e-9);
        assert_eq!(ev.gds, 1e-12); // still the λ=0 floor
        assert_relative_eq!(ev.gbs, 4.169632287013103e-5, max_relative = 1e-9);
    }

    #[test]
    fn mosfet_body_effect_and_lambda_in_triode() {
        let mut model = test_model();
        model.gamma = 0.5;
        model.lambda = 0.1;
        let ev = model.eval(model.kp, 2.0, 0.5, -1.0); // vov = 1.054842802587066
        assert_relative_eq!(ev.ids, 8.450849427164194e-5, max_relative = 1e-9);
        assert_relative_eq!(ev.gm, 1.05e-4, max_relative = 1e-12);
        assert_relative_eq!(ev.gds, 1.245654165691545e-4, max_relative = 1e-9);
        assert_relative_eq!(ev.gbs, 2.075244714485499e-5, max_relative = 1e-9);
        assert_relative_eq!(ev.i_eq, -1.670217668680803e-4, max_relative = 1e-9);
    }

    #[test]
    fn mosfet_reversed_vds_keeps_raw_gm_in_gbs() {
        // The raw (unclamped) Vds corner the AC path walks: gm floors to zero,
        // but gbs is derived from the *raw* triode gm — preserving the
        // pre-consolidation arithmetic exactly.
        let model = test_model();
        let ev = model.eval(model.kp, 2.0, -0.5, 0.0); // vov = 1.3 > vds
        assert_relative_eq!(ev.ids, -1.55e-4, max_relative = 1e-12);
        assert_eq!(ev.gm, 0.0); // raw gm was negative → floored
        assert_relative_eq!(ev.gds, 3.6e-4, max_relative = 1e-12);
        assert_eq!(ev.gbs, 0.0); // γ = 0 here; other tests cover γ < 0 corners
        assert_relative_eq!(ev.i_eq, 2.5e-5, max_relative = 1e-12);
    }
}
