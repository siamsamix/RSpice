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

/// Exponential I–V model: `I = IS·(exp(Vd / n·VT) − 1)`.
#[derive(Debug, Clone)]
pub struct DiodeModel {
    pub is: f64,
    pub n: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MosfetType {
    Nmos,
    Pmos,
}

/// Level-1 (Shichman-Hodges) MOSFET model parameters.
/// Mirrors standard SPICE .MODEL NMOS/PMOS card syntax.
#[derive(Debug, Clone)]
pub struct MosfetModel {
    pub kind: MosfetType,
    /// Transconductance parameter KP = µ·Cox (A/V²).
    pub kp: f64,
    /// Zero-bias threshold voltage Vth0 (V).
    pub vth0: f64,
    /// Channel-length modulation coefficient λ (1/V). 0 = ideal.
    pub lambda: f64,
    /// Bulk threshold modulation γ (V^0.5).
    pub gamma: f64,
    /// Strong-inversion surface potential 2·φF (V).
    pub phi: f64,
}

impl Default for MosfetModel {
    /// Reasonable level-1 NMOS defaults (matches SPICE internal defaults).
    fn default() -> Self {
        Self {
            kind: MosfetType::Nmos,
            kp: 20e-6,
            vth0: 0.7,
            lambda: 0.0,
            gamma: 0.0,
            phi: 0.6,
        }
    }
}
/// Thermal voltage at 300 K (V).
const VT: f64 = 0.02585;
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
    /// Evaluate the exponential model at bias `vd` (volts).
    ///
    /// The clamp `[-100 V, +0.8 V]` is part of the model, not of the caller:
    /// every consumer gets the same linearization point by construction.
    pub fn eval(&self, vd: f64) -> DiodeEval {
        let vd = vd.clamp(VD_CLAMP_MIN, VD_CLAMP_MAX);
        let vt_n = self.n * VT;
        let exp_term = (vd / vt_n).exp();
        let i_d = self.is * (exp_term - 1.0);
        let gd = (self.is / vt_n) * exp_term;
        DiodeEval { gd, i_d, i_eq: i_d - gd * vd }
    }
}

// ─── MOSFET ─────────────────────────────────────────────────────────────────

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
        // Body effect: Vth = Vth0 + γ·(√|2φF − Vbs| − √|2φF|), with Vsb = −Vbs.
        let vsb = -vbs;
        let vth = self.vth0
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

    const DIODE: DiodeModel = DiodeModel { is: 1e-12, n: 1.0 };

    /// Test model card: kp = 2e-4 (so β = kp·w/l = 2e-4 with w/l = 1),
    /// matching the 15-digit reference values in these tests.
    fn test_model() -> MosfetModel {
        MosfetModel {
            kind: MosfetType::Nmos,
            kp: 2e-4,
            vth0: 0.7,
            lambda: 0.0,
            gamma: 0.0,
            phi: 0.6,
        }
    }

    #[test]
    fn diode_at_zero_bias_is_pure_conductance() {
        let ev = DIODE.eval(0.0);
        assert_eq!(ev.i_d, 0.0); // exp(0) − 1 = 0 exactly
        assert_eq!(ev.i_eq, 0.0);
        assert_relative_eq!(ev.gd, 1e-12 / 0.02585, epsilon = 1e-28);
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
        assert_relative_eq!(raw.i_d, 2.757072476037856e1, max_relative = 1e-12);
        assert_relative_eq!(raw.gd, 1.066565754753561e3, max_relative = 1e-12);
        assert_relative_eq!(raw.i_eq, -8.256818790424706e2, max_relative = 1e-12);
    }

    #[test]
    fn diode_negative_bias_reports_reverse_current() {
        let ev = DIODE.eval(-1.0);
        // is·(e^(−38.7) − 1) = −is to double precision.
        assert_relative_eq!(ev.i_d, -1e-12, max_relative = 1e-13);
        assert_relative_eq!(ev.gd, 6.123210987845883e-28, epsilon = 1e-30);
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
