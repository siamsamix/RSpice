//! Branch-current extraction: the read-only family compute side of MNA.
//!
//! Concrete reports at real points ([`compute_branch_currents`]/
//! [`compute_branch_currents_at`]) and phasor reports for `.ac`
//! ([`compute_ac_branch_currents`]) fill a [`BranchCurrents`] report laid out
//! exactly as `Circuit::element_names(family)`. [`initial_guess`] seeds the
//! Newton-Raphson iteration with a linear-only once-solved matrix.

use nalgebra::DVector;
use num_complex::Complex64;

use crate::behavior::BehavioralRuntime;
use crate::circuit::{BehavioralKind, Circuit, ElementFamily, MosfetType};
use crate::error::Result;
use crate::mna::{
    behavioral_probe_lin, behavioral_probe_snapshot, device_model, node_index, node_voltage,
    stamp_admittance, stamp_behavioral, stamp_controlled_sources, stamp_current_source,
    stamp_switches, stamp_voltage_sources, tline_ac_angles, tline_port_sample, BranchLayout,
    MnaSystem, TransientState,
};

// ─── Branch currents ────────────────────────────────────────────────────────

/// Every element family's branch current at one operating point, laid out
/// exactly as `Circuit::element_names(family)`. Generic over the MNA field:
/// `BranchCurrents<f64>` is the real report (`.op`/`.dc`/`.tran`), and the
/// AC sweep instantiates the complex field, where every value is a phasor
/// flowing n1 → n2 through the element, linearised at the DC bias.
#[derive(Debug, Clone)]
pub struct BranchCurrents<T> {
    pub resistors: Vec<T>,
    pub capacitors: Vec<T>,
    pub inductors: Vec<T>,
    pub voltage_sources: Vec<T>,
    pub current_sources: Vec<T>,
    pub diodes: Vec<T>,
    /// Drain current for each MOSFET, drain → source. Small-signal:
    /// Id = gm·Vgs + gds·Vds + gbs·Vbs.
    pub mosfets: Vec<T>,
    /// Collector current (into the collector) for each BJT.
    pub bjts: Vec<T>,
    /// Drain current (into the drain) for each JFET.
    pub jfets: Vec<T>,
    /// Output current of each behavioral source: `n_p → n_n` element
    /// convention, with the voltage flavour reporting the branch unknown.
    /// Small-signal current flavour: Σ dk·(probe phasor) with the
    /// real-difference Jacobian evaluated at the DC bias.
    pub behavioral: Vec<T>,
    /// Output current of each op-amp: the branch unknown, `out+ → out−`
    /// element convention.
    pub opamps: Vec<T>,
    /// Current into each lossless line at its `n1` terminal: the MoC port-A
    /// current in transient, the hot-wire through current in DC/op, and the
    /// y11·V_A + y12·V_B phasor in AC (magnitudes after `collapse_ac`).
    pub tlines: Vec<T>,
}

impl<T> BranchCurrents<T> {
    /// The current values of one element family, positioned exactly as
    /// `Circuit::element_names(family)`. Family identity is resolved in one
    /// place instead of per-consumer `match` towers.
    pub fn slice(&self, family: ElementFamily) -> Option<&[T]> {
        match family {
            ElementFamily::Resistors => Some(&self.resistors),
            ElementFamily::Capacitors => Some(&self.capacitors),
            ElementFamily::Inductors => Some(&self.inductors),
            ElementFamily::VoltageSources => Some(&self.voltage_sources),
            ElementFamily::CurrentSources => Some(&self.current_sources),
            ElementFamily::Diodes => Some(&self.diodes),
            ElementFamily::Mosfets => Some(&self.mosfets),
            ElementFamily::Bjts => Some(&self.bjts),
            ElementFamily::Jfets => Some(&self.jfets),
            ElementFamily::Behavioral => Some(&self.behavioral),
            ElementFamily::OpAmps => Some(&self.opamps),
            ElementFamily::Tlines => Some(&self.tlines),
        }
    }
}

pub fn compute_branch_currents(
    circuit: &Circuit,
    solution: &DVector<f64>,
    dt: Option<f64>,
    prev_cap_voltages: Option<&[f64]>,
    t: Option<f64>,
) -> Result<BranchCurrents<f64>> {
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
        let model = device_model(&circuit.diode_models, "diode", &d.name, &d.model)?;
        let v_a = node_voltage(solution, d.anode);
        let v_c = node_voltage(solution, d.cathode);
        diodes.push(model.eval_for_at(v_a - v_c, d.area, circuit.temperature).i_d);
    }

    // MOSFET: re-evaluate Ids at the converged solution.
    let mut mosfets = Vec::with_capacity(circuit.mosfets.len());
    for m in &circuit.mosfets {
        let model = device_model(&circuit.mosfet_models, "MOSFET", &m.name, &m.model)?;
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
        let model = device_model(&circuit.bjt_models, "BJT", &q.name, &q.model)?;
        let vbe = node_voltage(solution, q.nb) - node_voltage(solution, q.ne);
        let vbc = node_voltage(solution, q.nb) - node_voltage(solution, q.nc);
        bjts.push(model.currents(vbe, vbc, q.area, circuit.temperature).ic);
    }

    let mut jfets = Vec::with_capacity(circuit.jfets.len());
    for j in &circuit.jfets {
        let model = device_model(&circuit.jfet_models, "JFET", &j.name, &j.model)?;
        let vgs = node_voltage(solution, j.ng) - node_voltage(solution, j.ns);
        let vgd = node_voltage(solution, j.ng) - node_voltage(solution, j.nd);
        jfets.push(model.currents(vgs, vgd, j.area, circuit.temperature).id);
    }

    // Lossless lines at DC/op: the current into the element at n1 is the
    // hot-wire through current g_wire·(v_hot difference) — the wire-pair
    // interpretation the DC stamp used (see CONTEXT.md's T entry).
    let tlines = circuit
        .tlines
        .iter()
        .map(|line| {
            let g_wire = 1.0 / (1.0e-6 * line.z0);
            g_wire * (node_voltage(solution, line.n1) - node_voltage(solution, line.n3))
        })
        .collect();

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

    // Op-amps: the branch unknown is the output current in the element
    // convention (out+ → out− through the device), like a V source's.
    let opamps = circuit.opamps.iter().enumerate().map(|(i, _)| {
        solution[branch.opamp_branch(i)]
    }).collect();

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
        opamps,
        tlines,
    })
}

/// Branch currents at one transient point: the real computation plus the
/// integrator-consistent capacitor currents from the accepted state.
pub fn compute_branch_currents_at(
    circuit: &Circuit,
    solution: &DVector<f64>,
    t: f64,
    state: &TransientState,
) -> Result<BranchCurrents<f64>> {
    let mut currents = compute_branch_currents(circuit, solution, None, None, Some(t))?;
    currents.capacitors = state.capacitor_currents.clone();
    currents.inductors = state.inductor_currents.clone();
    // Lossless lines report the method-of-characteristics port-A current:
    // I_A = (V_A − E_A(t))/Z0 with port B's accepted history at t − TD —
    // exactly the wave the step's stamp injected.
    if currents.tlines.len() == state.tline_histories.len() {
        for (i, line) in circuit.tlines.iter().enumerate() {
            let y = 1.0 / line.z0;
            let far = tline_port_sample(&state.tline_histories[i].b, t - line.delay);
            let incoming = far.v + line.z0 * far.i;
            let v_a = node_voltage(solution, line.n1) - node_voltage(solution, line.n2);
            currents.tlines[i] = y * (v_a - incoming);
        }
    }
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

// ─── Branch currents (AC, complex) ──────────────────────────────────────────

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
) -> Result<BranchCurrents<Complex64>> {
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
        let model = device_model(&circuit.diode_models, "diode", &d.name, &d.model)?;
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
        let model = device_model(&circuit.mosfet_models, "MOSFET", &m.name, &m.model)?;

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

    // ── Lossless lines: exact small-signal two-port ─────────────────────
    // I_A = y11·V_A + y12·V_B with y = 1/(Z0·j·sinθ)·[[cosθ,−1],[−1,cosθ]]
    // — the same clamped coefficients the AC arm stamped.
    let tlines = circuit
        .tlines
        .iter()
        .map(|line| {
            let (s, c) = tline_ac_angles(omega * line.delay);
            let y11 = c * Complex64::new(0.0, -1.0 / (line.z0 * s));
            let y12 = Complex64::new(0.0, 1.0 / (line.z0 * s));
            let v_a = node_voltage(solution, line.n1) - node_voltage(solution, line.n2);
            let v_b = node_voltage(solution, line.n3) - node_voltage(solution, line.n4);
            y11 * v_a + y12 * v_b
        })
        .collect();

    let mut bjts = Vec::with_capacity(circuit.bjts.len());
    for q in &circuit.bjts {
        let model = device_model(&circuit.bjt_models, "BJT", &q.name, &q.model)?;
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
        let model = device_model(&circuit.jfet_models, "JFET", &j.name, &j.model)?;
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
                    let dv = crate::behavior::resolve_probe(
                        probe,
                        |p, m| node_voltage(solution, p) - node_voltage(solution, m),
                        |i| solution[branch.src_branch(i)],
                        |i| solution[branch.ind_branch(i)],
                    )
                    .unwrap_or_else(|| Complex64::new(0.0, 0.0));
                    phasor += Complex64::new(dk, 0.0) * dv;
                }
                behavioral.push(phasor);
            }
        }
    }

    // Op-amps: the output-current phasor is the branch unknown, as for a
    // VCVS.
    let opamps = circuit.opamps.iter().enumerate().map(|(i, _)| {
        solution[branch.opamp_branch(i)]
    }).collect();

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
        opamps,
        tlines,
    })
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

    // Voltage sources at their DC value — the same ±1 coupling stamp the
    // solve arms use.
    stamp_voltage_sources(circuit, &mut sys, |v| v.voltage);

    // Inductors as 0 V shorts (same as the DC mode).
    for (i, l) in circuit.inductors.iter().enumerate() {
        let branch = sys.branch.ind_branch(i);
        if let Some(i1) = node_index(l.n1) {
            sys.stamp_add(i1, branch, 1.0);
            sys.stamp_add(branch, i1, 1.0);
        }
        if let Some(i2) = node_index(l.n2) {
            sys.stamp_sub(i2, branch, 1.0);
            sys.stamp_sub(branch, i2, 1.0);
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
    let _ = stamp_behavioral(circuit, &mut sys, &zero_probes, true, 0.0, 0.0, &[], 0.0);

    match sys.solve() {
        Ok(x0) => x0,
        Err(_) => DVector::zeros(sys.size),
    }
}
