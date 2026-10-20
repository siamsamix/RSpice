use approx::assert_relative_eq;
use circuitsim::{ParseOptions, parse, parse_with_options, run, simulate};

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// Unique-per-test scratch directory (tempfile is not a dev-dependency).
fn temp_deck_dir(label: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let id = format!(
        "rspice_it_{}_{}_{}",
        std::process::id(),
        label,
        NEXT.fetch_add(1, Ordering::SeqCst)
    );
    let dir = std::env::temp_dir().join(id);
    std::fs::create_dir_all(&dir).expect("scratch dir created");
    dir
}

#[test]
fn voltage_divider_dc() {
    let netlist = r#"
* Voltage divider
V1 1 0 DC 10
R1 1 2 1k
R2 2 0 1k
.op
.end
"#;
    let result = simulate(netlist).unwrap();
    let dc = result.dc.unwrap();
    assert_relative_eq!(dc.node_voltages[1], 10.0, epsilon = 1e-6);
    assert_relative_eq!(dc.node_voltages[2], 5.0, epsilon = 1e-6);
}

#[test]
fn rc_charging_transient() {
    let netlist = r#"
V1 1 0 DC 5
R1 1 2 1k
C1 2 0 1u
.tran 10u 5m
.end
"#;
    let result = simulate(netlist).unwrap();
    let tran = result.tran.unwrap();
    let final_v = tran.points.last().unwrap().node_voltages[2];
    // RC time constant 1ms; after 5ms capacitor is essentially fully charged.
    assert_relative_eq!(final_v, 5.0, epsilon = 0.05);
}

#[test]
fn rl_circuit_dc() {
    let netlist = r#"
V1 1 0 DC 12
R1 1 2 100
L1 2 0 1m
.op
.end
"#;
    let result = simulate(netlist).unwrap();
    let dc = result.dc.unwrap();
    // At DC the inductor is a short, so node 2 is at ground.
    assert_relative_eq!(dc.node_voltages[2], 0.0, epsilon = 1e-6);
    assert_relative_eq!(dc.source_currents[0].abs(), 12.0 / 100.0, epsilon = 1e-4);
}

#[test]
fn rc_lowpass_ac_response() {
    // Guards the AC fold into MnaSystem: small-signal stamps, the phasor
    // solve, and the complex branch extraction. The last sweep point sits at
    // the corner frequency f0 = 1/(2π·R·C) ≈ 159.1549431 Hz.
    let netlist = r#"
V1 1 0 DC 5 AC 1
R1 1 2 1k
C1 2 0 1u
.op
.ac LIN 2 1 159.1549431
.end
"#;
    let result = simulate(netlist).unwrap();
    let ac = result.ac.expect("AC result");

    // Last point sits at the pole: |H(f0)| = 1/√2, phase −45°.
    let last = ac.points.last().unwrap();
    let h = last.node_voltages[2] / last.node_voltages[1];
    assert_relative_eq!(h.norm(), std::f64::consts::FRAC_1_SQRT_2, epsilon = 1e-3);
    assert_relative_eq!(h.arg().to_degrees(), -45.0, epsilon = 0.5);

    // First point (1 Hz << f0) passes nearly everything.
    let first = ac.points.first().unwrap();
    let h0 = first.node_voltages[2] / first.node_voltages[1];
    assert_relative_eq!(h0.norm(), 0.99935, epsilon = 1e-3);
}

// ── P0 coverage: I sources, .dc sweeps, parameters, node names, grammar ──────

#[test]
fn current_source_drives_a_resistor() {
    // `I1 0 1` pushes 1 mA into the R/ground node → exactly one volt.
    let netlist = r#"
I1 0 1 DC 1m
R1 1 0 1k
.op
.end
"#;
    let result = simulate(netlist).unwrap();
    let dc = result.dc.unwrap();
    assert_relative_eq!(dc.node_voltages[1], 1.0, epsilon = 1e-6);
    // Branch currents are recorded for the source family too.
    assert_relative_eq!(dc.branch_currents.resistors[0], 1e-3, epsilon = 1e-9);
    assert_relative_eq!(dc.branch_currents.current_sources[0], 1e-3, epsilon = 1e-9);
}

#[test]
fn dc_sweep_of_a_voltage_source() {
    let netlist = r#"
V1 1 0 DC 5
R1 1 2 1k
R2 2 0 1k
.dc V1 0 10 2.5
.end
"#;
    let result = simulate(netlist).unwrap();
    let sweep = result.dc_sweep.expect("a real .dc sweep");
    assert!(result.dc.is_none(), "the sweep replaces the plain op point");
    assert_eq!(sweep.targets, vec!["V1"]);
    assert_eq!(sweep.points.len(), 5); // 0, 2.5, 5, 7.5, 10
    for (i, pt) in sweep.points.iter().enumerate() {
        let drive = 2.5 * i as f64;
        assert_relative_eq!(pt.values[0], drive, epsilon = 1e-9);
        // The divider tracks the drive exactly on every point.
        assert_relative_eq!(pt.node_voltages[2], drive / 2.0, epsilon = 1e-9);
    }
}

#[test]
fn dc_sweep_of_a_parameter() {
    // Sweeping a .param re-reads the deck once per point with the value
    // pinned, so each `{rl}` element evaluates afresh.
    let netlist = r#"
.param rl=2k
V1 1 0 DC 5
R1 1 2 {rl}
R2 2 0 1k
.dc rl 1k 3k 1k
.end
"#;
    let result = simulate(netlist).unwrap();
    let sweep = result.dc_sweep.expect("param sweep");
    assert_eq!(sweep.targets, vec!["rl"]);
    assert_eq!(sweep.points.len(), 3);
    for (i, rl) in [1e3, 2e3, 3e3].iter().enumerate() {
        let pt = &sweep.points[i];
        assert_relative_eq!(pt.values[0], *rl, epsilon = 1e-9);
        assert_relative_eq!(pt.node_voltages[2], 5.0 * 1e3 / (rl + 1e3), epsilon = 1e-6);
    }
}

#[test]
fn nested_dc_sweep_runs_the_inner_loop_inside() {
    let netlist = r#"
V1 1 0 DC 5
V2 2 0 DC 1
R1 1 2 1k
.dc V1 0 5 5 V2 0 1 1
.end
"#;
    let result = simulate(netlist).unwrap();
    let sweep = result.dc_sweep.expect("nested sweep");
    assert_eq!(sweep.targets, vec!["V1", "V2"]);
    assert_eq!(sweep.points.len(), 4);
    // Outer sweep advances slowly: (0,0), (0,1), (5,0), (5,1).
    let drives: Vec<f64> = sweep.points.iter().flat_map(|p| p.values.clone()).collect();
    assert_eq!(drives, vec![0.0, 0.0, 0.0, 1.0, 5.0, 0.0, 5.0, 1.0]);
    for pt in &sweep.points {
        assert_relative_eq!(pt.node_voltages[1], pt.values[0], epsilon = 1e-9);
        assert_relative_eq!(pt.node_voltages[2], pt.values[1], epsilon = 1e-9);
    }
}

#[test]
fn named_nodes_are_case_insensitive() {
    let netlist = r#"
V1 IN 0 DC 10
R1 in Out 1k
R2 OUT 0 1k
.op
.end
"#;
    let parsed = parse(netlist).unwrap();
    let circuit = &parsed.circuit;
    // "IN"/"in", "Out"/"OUT" unite; only the first spelling is kept.
    assert_eq!(circuit.resistors[0].n1.0, 1);
    assert_eq!(circuit.resistors[0].n2.0, 2);
    assert_eq!(circuit.resistors[1].n1.0, 2);
    assert_eq!(circuit.node_names[1], "IN");

    let result = run(&parsed).unwrap();
    let dc = result.dc.unwrap();
    assert_relative_eq!(dc.node_voltages[2], 5.0, epsilon = 1e-6);
}

#[test]
fn comments_inline_comments_and_continuations() {
    let netlist = "* full-line comment is dropped\n\
     ; semicolon lines drop too\n\
     V1 1 0 DC 5 ; trailing comment\n\
     R1 1 2\n\
     + 2k\n\
     .op\n\
     .end\n";
    let parsed = parse(netlist).unwrap();
    assert_eq!(parsed.circuit.voltage_sources[0].voltage, 5.0);
    assert_relative_eq!(parsed.circuit.resistors[0].resistance, 2000.0, epsilon = 1e-9);
    // One element line, one continuation line: exactly two circuit lines.
    assert_eq!(parsed.circuit.resistors.len(), 1);
}

#[test]
fn dc_sweep_of_a_current_source() {
    // Sweeping `I1` flips its injected current; a 1 k resistor turns each
    // LIST value straight into a voltage.
    let netlist = r#"
I1 0 1 DC 1m
R1 1 0 1k
.dc I1 LIST 0 2m 4m
.end
"#;
    let result = simulate(netlist).unwrap();
    let sweep = result.dc_sweep.expect("current-source sweep");
    assert_eq!(sweep.targets, vec!["I1"]);
    assert_eq!(sweep.points.len(), 3);
    for (i, drive) in [0.0, 2e-3, 4e-3].iter().enumerate() {
        let pt = &sweep.points[i];
        assert_relative_eq!(pt.values[0], *drive, epsilon = 1e-12);
        assert_relative_eq!(pt.node_voltages[1], drive * 1e3, epsilon = 1e-9);
    }
}

#[test]
fn meg_and_mil_suffixes() {
    let netlist = r#"
V1 1 0 DC 12
R1 1 2 4.7Meg
R2 2 0 1Mil
.op
.end
"#;
    let parsed = parse(netlist).unwrap();
    assert_relative_eq!(parsed.circuit.resistors[0].resistance, 4.7e6, epsilon = 1e-3);
    assert_relative_eq!(parsed.circuit.resistors[1].resistance, 25.4e-6, epsilon = 1e-12);
}

#[test]
fn oversize_dc_sweep_is_rejected_loudly() {
    // 1e7 points exceeds the 1e6 cap — the arguments are never ignored.
    let netlist = "V1 1 0 DC 1\nR1 1 2 1k\n.dc V1 0 1 1e-7\n.end\n";
    let err = simulate(netlist).unwrap_err();
    assert!(err.to_string().contains("1 000 000") || err.to_string().contains("1,000,000") || err.to_string().contains("000"),
        "error mentions the cap: {err}");
}

// ── P1 parser & language: subckts, UIC, diode AREA, .include ─────────────────

fn node_index(names: &[String], wanted: &str) -> usize {
    names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(wanted))
        .unwrap_or_else(|| panic!("net '{wanted}' is not a circuit node"))
}

/// The full .subckt/.ends/X pipeline binds ports, mangles internal nets and
/// honours an instance line's parameter override over a body `.param` card.
/// The numbers mirror a hand-checked divider: X1 (rr=2k) chains 2k+2k into
/// `out`, where 1k∥2k (Rload∥X2) presents 2/3 k, giving V(out) = 10/14 and
/// V(X1.mid) = 20/7.
#[test]
fn subckt_hierarchy_resolves_dc_divider() {
    let netlist = r#"
.subckt rdiv a b
.param rr=1k
R1 a mid {rr}
R2 mid b {rr}
.ends
V1 in 0 DC 5
Rload out 0 1k
X1 in out rdiv rr=2k
X2 out 0 rdiv
.op
.end
"#;
    let parsed = parse(netlist).unwrap();
    assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);

    let result = run(&parsed).unwrap();
    let dc = result.dc.unwrap();
    let v = |name: &str| {
        let idx = node_index(&parsed.circuit.node_names, name);
        dc.node_voltages[idx]
    };
    assert_relative_eq!(v("out"), 10.0 / 14.0, epsilon = 1e-6);
    assert_relative_eq!(v("X1.mid"), 20.0 / 7.0, epsilon = 1e-6);
    // X2 keeps the body default: its mid node halves its own 1k+1k chain.
    assert_relative_eq!(v("X2.mid"), v("out") / 2.0, epsilon = 1e-6);
}

/// `.tran … UIC` starts the transient from the `.ic` pins instead of the DC
/// operating point: the t=0 sample carries the requested node voltages and
/// every inductor branch row starts collapsed at zero current.
#[test]
fn uic_transient_honors_ic_entries() {
    let netlist = r#"
V1 in 0 PULSE(0 5 0 1u 1u 5u 10u)
R1 in mid 1k
D1 mid out demod
C1 out 0 100n
L1 out 0 1m
.model demod D(IS=1e-14)
.tran 5u 25u 0 UIC
.ic V(out)=2 V(mid) = 3
.end
"#;
    let parsed = parse(netlist).unwrap();
    let result = run(&parsed).unwrap();
    let tran = result.tran.expect("UIC runs a transient");
    let first = tran.points.first().expect("the t=0 point exists");
    let names = &parsed.circuit.node_names;
    assert_relative_eq!(
        first.node_voltages[node_index(names, "out")],
        2.0,
        epsilon = 1e-12
    );
    assert_relative_eq!(
        first.node_voltages[node_index(names, "mid")],
        3.0,
        epsilon = 1e-12
    );
    // UIC never solves the DC op first, so the inductor starts at 0 A by
    // construction — no hidden seed.
    assert_relative_eq!(first.branch_currents.inductors[0], 0.0, epsilon = 1e-12);
    // And the run proceeds past t=0 instead of stalling there.
    assert_eq!(tran.points.first().unwrap().time, 0.0);
    assert!(tran.points.len() > 2, "the sweep advanced: {} points", tran.points.len());
}

/// Diode `AREA` scales the junction: two ideal-source-clamped diodes each see
/// the same forward voltage, so the AREA=2 device carries exactly twice the
/// current — proof `eval_for` reaches the stamp, not just the summary card.
#[test]
fn diode_area_scales_branch_current() {
    let netlist = r#"
V1 m 0 DC 0.65
D1 m 0 dio
D2 m 0 dio AREA=2
.model dio D(IS=1e-12)
.op
.end
"#;
    let result = simulate(netlist).unwrap();
    let dc = result.dc.unwrap();
    let i1 = dc.branch_currents.diodes[0];
    let i2 = dc.branch_currents.diodes[1];
    assert!(i1 > 0.0, "the clamped junction conducts, got {i1}");
    assert_relative_eq!(i2 / 2.0, i1, max_relative = 1e-9);
}

/// `.include` resolves through the deck's directory: a shared file carrying a
/// model card and a subckt definition splices in and drives the analysis.
#[test]
fn include_resolves_from_deck_dir() {
    let dir = temp_deck_dir("include");
    std::fs::write(
        dir.join("lib.inc"),
        "* shared parts\n\
         .model dio D(IS=5e-13)\n\
         .subckt load in out\n\
         R1 in out 3k\n\
         .ends\n",
    )
    .unwrap();
    let deck = "* the include arrives before any element reference\n\
     .include lib.inc\n\
     V1 in 0 DC 6\n\
     X1 in out load\n\
     R2 out 0 3k\n\
     .op\n\
     .end\n";
    let parsed = parse_with_options(
        deck,
        &ParseOptions {
            deck_dir: Some(dir.clone()),
            search_paths: Vec::new(),
        },
    )
    .unwrap();
    assert!(
        parsed
            .warnings
            .iter()
            .all(|w| !w.contains("include")),
        "include resolved cleanly: {:?}",
        parsed.warnings
    );
    // Both the model card and the subckt body arrived.
    assert_relative_eq!(parsed.circuit.diode_models["dio"].is, 5e-13, epsilon = 1e-15);
    assert_relative_eq!(
        parsed
            .circuit
            .resistors
            .iter()
            .find(|r| r.name == "X1.R1")
            .expect("the subckt's R1 spliced in")
            .resistance,
        3000.0,
        epsilon = 1e-9
    );

    let result = run(&parsed).unwrap();
    let dc = result.dc.unwrap();
    let names = &parsed.circuit.node_names;
    assert_relative_eq!(dc.node_voltages[node_index(names, "in")], 6.0, epsilon = 1e-9);
    // The included subckt's 3k halves against the deck's own 3k.
    assert_relative_eq!(dc.node_voltages[node_index(names, "out")], 3.0, epsilon = 1e-9);

    drop(std::fs::remove_dir_all(dir));
}

// ─── P1: controlled sources, switches, .tf, waveform family ────────────────

#[test]
fn vcvs_scales_the_control_voltage() {
    let netlist = r#"
V1 in 0 DC 1
E1 out 0 in 0 10
R1 out 0 1k
.op
.end
"#;
    let n = parse(netlist).unwrap();
    let r = run(&n).unwrap();
    let out = node_index(&n.circuit.node_names, "out");
    assert_relative_eq!(r.dc.unwrap().node_voltages[out], 10.0, epsilon = 1e-9);
}

#[test]
fn vcvs_poly_form_sums_control_terms() {
    let netlist = r#"
V1 a 0 DC 1
V2 b 0 DC 2
E1 out 0 POLY(2) a 0 b 0 1 2 3
R1 out 0 1k
.op
.end
"#;
    // out = 1 + 2·V(a) + 3·V(b) = 1 + 2 + 6 = 9.
    let n = parse(netlist).unwrap();
    let r = run(&n).unwrap();
    let out = node_index(&n.circuit.node_names, "out");
    assert_relative_eq!(r.dc.unwrap().node_voltages[out], 9.0, epsilon = 1e-9);
}

#[test]
fn vccs_injects_a_transconductance_current() {
    let netlist = r#"
V1 in 0 DC 1
G1 0 out in 0 1m
R1 out 0 1k
.op
.end
"#;
    // 1 mA into `out` through 1 kΩ → 1 V.
    let n = parse(netlist).unwrap();
    let r = run(&n).unwrap();
    let out = node_index(&n.circuit.node_names, "out");
    assert_relative_eq!(r.dc.unwrap().node_voltages[out], 1.0, epsilon = 1e-9);
}

#[test]
fn cccs_reads_the_controlling_source_current() {
    let netlist = r#"
V1 in 0 DC 1
R0 in 0 1k
F1 out 0 V1 2
R1 out 0 1k
.op
.end
"#;
    // I(V1) is −1 mA in the element convention; ×2 drives 2 V across R1.
    let n = parse(netlist).unwrap();
    let r = run(&n).unwrap();
    let out = node_index(&n.circuit.node_names, "out");
    assert_relative_eq!(r.dc.unwrap().node_voltages[out], 2.0, epsilon = 1e-9);
}

#[test]
fn ccvs_output_voltage_tracks_the_control_current() {
    let netlist = r#"
V1 in 0 DC 1
R0 in 0 1k
H1 out 0 V1 2000
R1 out 0 1k
.op
.end
"#;
    // V(out) = 2000·I(V1) = 2000·(−1 mA) = −2 V.
    let n = parse(netlist).unwrap();
    let r = run(&n).unwrap();
    let out = node_index(&n.circuit.node_names, "out");
    assert_relative_eq!(r.dc.unwrap().node_voltages[out], -2.0, epsilon = 1e-9);
}

#[test]
fn voltage_controlled_switch_opens_and_closes() {
    let on = r#"
.model sw SW(RON=1 ROFF=1meg VT=0.5)
Vc ctrl 0 DC 1
S1 in out ctrl 0 sw
V1 in 0 DC 1
R1 out 0 1k
.op
.end
"#;
    let n = parse(on).unwrap();
    let r = run(&n).unwrap();
    let out = node_index(&n.circuit.node_names, "out");
    // Closed: the 1 Ω switch leaves almost all of the volt across R1.
    assert_relative_eq!(r.dc.unwrap().node_voltages[out], 1.0, epsilon = 1e-3);

    let off = on.replace("Vc ctrl 0 DC 1", "Vc ctrl 0 DC 0");
    let n = parse(&off).unwrap();
    let r = run(&n).unwrap();
    let out = node_index(&n.circuit.node_names, "out");
    // Open: the 1 MΩ off resistance forms a divider with 1 kΩ.
    assert_relative_eq!(r.dc.unwrap().node_voltages[out], 1.0 / 1001.0, epsilon = 1e-9);
}

#[test]
fn dc_transfer_function_reports_gain_and_impedances() {
    let netlist = r#"
V1 in 0 DC 1
R1 in out 1k
R2 out 0 1k
.tf V(out) V1
.end
"#;
    let r = simulate(netlist).unwrap();
    let tf = r.tf.expect(".tf produced a result");
    assert_relative_eq!(tf.transfer, 0.5, epsilon = 1e-12);
    assert_relative_eq!(tf.input_impedance, 2000.0, epsilon = 1e-9);
    // With V1 zeroed (a short), R1 ∥ R2 = 500 Ω.
    assert_relative_eq!(tf.output_impedance, 500.0, epsilon = 1e-9);
}

#[test]
fn pwl_repeat_blocks_cycle_the_value() {
    use circuitsim::circuit::{Pwl, Waveform};
    let w = Waveform::Pwl(Pwl {
        points: vec![(0.0, 0.0), (1.0, 10.0)],
        repeat_from: Some(0.0),
        repeat_count: Some(Some(2)),
    });
    assert_relative_eq!(w.value_at(0.5), 5.0, epsilon = 1e-12);
    assert_relative_eq!(w.value_at(1.5), 5.0, epsilon = 1e-12);
    assert_relative_eq!(w.value_at(2.5), 5.0, epsilon = 1e-12);
    // Two additional repeats, then hold the last repeated value.
    assert_relative_eq!(w.value_at(3.5), 10.0, epsilon = 1e-12);
}

#[test]
fn exp_and_am_sources_parse_and_evaluate() {
    let netlist = r#"
V1 a 0 DC 0 EXP(0 1 1u 2u 3u 4u)
V2 b 0 AM(1 0 1k 10k 0)
R1 a 0 1k
R2 b 0 1k
.tran 1u 10u
.end
"#;
    let n = parse(netlist).unwrap();
    let r = run(&n).unwrap();
    let tran = r.tran.unwrap();
    // EXP rises toward V2 = 1 V, then decays back toward V1 = 0 from TD2:
    // v(10 µs) = (1 − e^(−9µ/2µ)) − (1 − e^(−7µ/4µ)) = 0.162665…
    let a = node_index(&n.circuit.node_names, "a");
    let last = tran.points.last().unwrap();
    assert_relative_eq!(last.node_voltages[a], 0.162665, epsilon = 2e-3);
    // AM(va=1 vo=0 fm=1k fc=10k) is bounded by ±2 V.
    let b = node_index(&n.circuit.node_names, "b");
    for p in &tran.points {
        assert!(p.node_voltages[b].abs() <= 2.0 + 1e-9);
    }
}

#[test]
fn step_runs_the_analysis_once_per_parameter_value() {
    let netlist = r#"
.param r=1k
V1 in 0 DC 1
R1 in out {r}
R2 out 0 1k
.step param r 1k 2k 1k
.op
.end
"#;
    let n = parse(netlist).unwrap();
    let r = run(&n).unwrap();
    assert_eq!(r.step_targets, vec!["r".to_string()]);
    assert_eq!(r.runs.len(), 2);
    let out = node_index(&n.circuit.node_names, "out");
    assert_relative_eq!(r.runs[0].values[0], 1000.0, epsilon = 1e-12);
    assert_relative_eq!(r.runs[0].dc.as_ref().unwrap().node_voltages[out], 0.5, epsilon = 1e-9);
    assert_relative_eq!(r.runs[1].values[0], 2000.0, epsilon = 1e-12);
    // 1 k / (2 k + 1 k) = 1/3.
    assert_relative_eq!(
        r.runs[1].dc.as_ref().unwrap().node_voltages[out],
        1.0 / 3.0,
        epsilon = 1e-9
    );
    // Top-level results mirror the first step run.
    assert_relative_eq!(r.dc.unwrap().node_voltages[out], 0.5, epsilon = 1e-9);
}

#[test]
fn nested_steps_form_the_cartesian_product() {
    let netlist = r#"
.param a=1
.param b=0
V1 in 0 DC 1
R1 in out {a}
R2 out 0 1k
.step param a list 1 2
.step param b list 10 20 30
.op
.end
"#;
    let n = parse(netlist).unwrap();
    let result = run(&n).unwrap();
    assert_eq!(result.step_targets, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(result.runs.len(), 6);
    // First target is the outer loop.
    assert_eq!(result.runs[0].values, vec![1.0, 10.0]);
    assert_eq!(result.runs[2].values, vec![1.0, 30.0]);
    assert_eq!(result.runs[3].values, vec![2.0, 10.0]);
    assert_eq!(result.runs[5].values, vec![2.0, 30.0]);
}

#[test]
fn measurements_read_transient_traces() {
    let netlist = r#"
V1 in 0 PULSE(0 5 0 1n 1n 1 2)
R1 in out 1k
C1 out 0 1u
.tran 10u 5m
.meas tran vmax max V(out)
.meas tran vavg avg V(out)
.meas tran tcross cross V(out) val=2.5
.meas tran trise trig V(out) val=0.5 rise=1 targ V(out) val=4.5 rise=1
.meas tran vfind find V(out) when V(out)=2.5
.end
"#;
    let n = parse(netlist).unwrap();
    let r = run(&n).unwrap();
    let get = |name: &str| {
        r.measurements
            .iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("measurement {name} missing"))
            .clone()
    };
    assert_relative_eq!(get("vmax").value.unwrap(), 4.9663, epsilon = 2e-3);
    // avg = 5·(1 − τ/T·(1 − e^(−T/τ))), τ = 1 ms, T = 5 ms.
    assert_relative_eq!(get("vavg").value.unwrap(), 4.00667, epsilon = 5e-3);
    // 2.5 V is the half-way point: τ·ln 2 = 0.693147 ms.
    assert_relative_eq!(get("tcross").value.unwrap(), 0.693147e-3, epsilon = 5e-6);
    // 0.5 V → 4.5 V: τ·(ln 0.9 − ln 0.1)? times: −τ ln(0.9) to −τ ln(0.1).
    assert_relative_eq!(get("trise").value.unwrap(), 2.197225e-3, epsilon = 5e-5);
    assert_relative_eq!(get("vfind").value.unwrap(), 2.5, epsilon = 1e-3);
}

#[test]
fn four_decomposes_a_sine_wave() {
    let netlist = r#"
V1 in 0 DC 0 SIN(0 1 1k)
R1 in out 1k
.tran 20u 2m
.four 1k V(out)
.end
"#;
    let n = parse(netlist).unwrap();
    let r = run(&n).unwrap();
    let four = r.four.expect(".four produced a result");
    assert_relative_eq!(four.fundamental, 1e3, epsilon = 1e-12);
    let out = &four.outputs[0];
    assert_eq!(out.harmonics.len(), 9);
    let h1 = &out.harmonics[0];
    assert_eq!(h1.number, 1);
    // A 1 V sine through a 1 kΩ resistor: the fundamental is ~1 V.
    assert!((h1.magnitude - 1.0).abs() < 0.03, "fundamental {}", h1.magnitude);
    assert_relative_eq!(h1.normalized_magnitude, 1.0, epsilon = 1e-12);
    // No meaningful higher harmonics on an ideal sine.
    for h in &out.harmonics[1..] {
        assert!(h.magnitude < 0.03, "harmonic {} = {}", h.number, h.magnitude);
    }
    assert!(out.dc.abs() < 0.02, "dc {}", out.dc);
}

#[test]
fn noise_sweep_reports_thermal_density_and_input_referral() {
    let netlist = r#"
V1 in 0 DC 1
R1 in out 1k
R2 out 0 1k
.noise V(out) V1 DEC 3 1k 10k
.end
"#;
    let n = parse(netlist).unwrap();
    let r = run(&n).unwrap();
    let noise = r.noise.expect(".noise produced a result");
    assert_eq!(noise.points.len(), 4);
    // R1 ∥ R2 = 500 Ω: √(4kTR) = 2.8789 nV/√Hz at every frequency.
    for p in &noise.points {
        assert_relative_eq!(p.output_density, 2.878895e-9, epsilon = 1e-13);
        assert_relative_eq!(p.input_density, 2.0 * 2.878895e-9, epsilon = 1e-12);
    }
    // Integrated over 1–10 kHz: density·√(9 kHz).
    assert_relative_eq!(
        noise.total_output_rms,
        2.878895e-9 * (9000.0f64).sqrt(),
        max_relative = 1e-3
    );
}

#[test]
fn passive_model_card_supplies_value_and_temperature_coefficient() {
    let netlist = r#"
.model rm R(R=1k TC1=1m)
V1 in 0 DC 1
R1 in out rm
R2 out 0 1k
.temp 127
.op
.end
"#;
    // R1 at 127 °C = 1k·(1 + 1e-3·100) = 1.1k; divider R2/(R1+R2) = 1/2.1.
    let n = parse(netlist).unwrap();
    assert_relative_eq!(n.circuit.temperature, 127.0, epsilon = 1e-12);
    let r = run(&n).unwrap();
    let out = node_index(&n.circuit.node_names, "out");
    assert_relative_eq!(r.dc.unwrap().node_voltages[out], 1.0 / 2.1, epsilon = 1e-9);
}

#[test]
fn stepped_temperature_runs_report_their_values() {
    let netlist = r#"
.model dmod D(IS=1e-14)
I1 0 a DC 1m
D1 a 0 dmod
.step temp list 27 127
.op
.end
"#;
    let n = parse(netlist).unwrap();
    let r = run(&n).unwrap();
    assert_eq!(r.step_targets, vec!["temp".to_string()]);
    assert_eq!(r.runs.len(), 2);
    let a = node_index(&n.circuit.node_names, "a");
    let v_cold = r.runs[0].dc.as_ref().unwrap().node_voltages[a];
    let v_hot = r.runs[1].dc.as_ref().unwrap().node_voltages[a];
    // A forward diode drops roughly 2 mV per °C: 100 °C is about 0.2 V.
    assert!(v_cold > v_hot, "cold {v_cold} should exceed hot {v_hot}");
    assert!(
        (0.1..0.4).contains(&(v_cold - v_hot)),
        "ΔVf over 100 °C = {}",
        v_cold - v_hot
    );
}

#[test]
fn tran_output_starts_at_tstart_and_keeps_the_print_grid() {
    let netlist = r#"
V1 in 0 PULSE(0 1 0 1p 1p 1 2)
R1 in out 1k
C1 out 0 1u
.tran 10u 100u 50u
.end
"#;
    let n = parse(netlist).unwrap();
    let r = run(&n).unwrap();
    let tran = r.tran.unwrap();
    assert!(tran.points.iter().all(|p| p.time >= 50e-6 - 1e-18));
    // The print grid is the first .tran argument, not the internal step.
    for target in [50e-6, 60e-6, 70e-6, 80e-6, 90e-6, 100e-6] {
        assert!(
            tran.points.iter().any(|p| (p.time - target).abs() < 1e-12),
            "missing print point at {target}"
        );
    }
}

#[test]
fn tran_lands_on_source_edges() {
    let netlist = r#"
V1 in 0 PULSE(0 1 1u 1n 1n 2u 1m)
R1 in 0 1k
.tran 10u 100u
.end
"#;
    let n = parse(netlist).unwrap();
    let tran = run(&n).unwrap().tran.unwrap();
    // The rise edge, plateau edge, and fall edge are all hit exactly enough
    // that a 1 ns feature is not sampled away.
    for edge in [1e-6, 1e-6 + 1e-9, 3e-6 + 1e-9, 3e-6 + 2e-9] {
        assert!(
            tran.points.iter().any(|p| (p.time - edge).abs() < 1e-12),
            "missing breakpoint {edge}: first times {:?}",
            tran.points.iter().take(8).map(|p| p.time).collect::<Vec<_>>()
        );
    }
}

#[test]
fn gear_integration_methods_run_and_agree() {
    let base = r#"
V1 in 0 PULSE(0 1 0 1p 1p 1 2)
R1 in out 1k
C1 out 0 1u
.options method=METHOD
.tran 10u 2m
.end
"#;
    let mut finals = Vec::new();
    for method in ["trap", "gear1", "gear2"] {
        let deck = base.replace("METHOD", method);
        let n = parse(&deck).unwrap();
        let tran = run(&n).unwrap().tran.unwrap();
        let out = node_index(&n.circuit.node_names, "out");
        finals.push(*tran.points.last().unwrap().node_voltages.get(out).unwrap());
    }
    // All three integrate the same RC to the same state: 1 − e^(−2 ms/1 ms).
    let analytic = 1.0 - (-2.0f64).exp();
    for v in &finals {
        assert_relative_eq!(*v, finals[0], epsilon = 5e-3);
        assert!((*v - analytic).abs() < 2e-3, "final {v} vs {analytic}");
    }
}

#[test]
fn diode_series_resistance_drops_voltage_at_the_junction() {
    let deck = r#"
.model dmod D(IS=1e-14 N=1 RS=100)
I1 0 a DC 1m
D1 a 0 dmod
.op
.end
"#;
    let n = parse(deck).unwrap();
    let r = run(&n).unwrap();
    let a = node_index(&n.circuit.node_names, "a");
    let va = r.dc.unwrap().node_voltages[a];
    // The junction sits near 0.6 V; the 100 Ω series resistor adds ~0.1 V.
    assert!(va > 0.65 && va < 0.78, "V(a)={va}");
    // The split created an internal junction node and one extra resistor.
    assert!(n.circuit.node_names.iter().any(|s| s.ends_with(".int")));
    assert!(n.circuit.resistors.iter().any(|x| x.name.ends_with("_RS")));
}

#[test]
fn diode_reverse_breakdown_clamps_at_bv() {
    let deck = r#"
.model dmod D(IS=1e-14 N=1 BV=5.6 IBV=1m)
I1 0 a DC -1m
R1 a b 1k
D1 b 0 dmod
.op
.end
"#;
    let n = parse(deck).unwrap();
    let r = run(&n).unwrap();
    let b = node_index(&n.circuit.node_names, "b");
    let vb = r.dc.unwrap().node_voltages[b];
    // 1 mA through a 5.6 V breakdown diode lands just below −BV.
    assert!(vb < -5.0 && vb > -6.5, "V(b)={vb}");
}

#[test]
fn diode_junction_capacitance_loads_an_ac_sweep() {
    let deck = r#"
.model dmod D(IS=1e-14 N=1 CJO=100n VJ=0.7 M=0.5)
V1 in 0 DC -1 AC 1
R1 in out 1k
D1 out 0 dmod
.ac dec 5 100 100k
.end
"#;
    let n = parse(deck).unwrap();
    let r = run(&n).unwrap();
    let out = node_index(&n.circuit.node_names, "out");
    let ac = r.ac.unwrap();
    // Reverse biased: the junction capacitance forms a pole with 1 kΩ.
    let first = ac.points.first().unwrap().node_voltages[out].norm();
    let last = ac.points.last().unwrap().node_voltages[out].norm();
    assert!(first > last * 5.0, "first {first} last {last}");
}

#[test]
fn mosfet_terminal_capacitances_load_the_drain() {
    let deck = |cg: &str| {
        format!(
            r#"
.model nm NMOS(LEVEL=1 KP=110u VTO=1 LAMBDA=0 {cg})
Vg g 0 DC 0
Vd s 0 DC 1 AC 1
R2 s d 1k
M1 d g 0 0 nm W=10u L=1u
R1 d 0 1meg
.ac dec 5 1k 100meg 1
.end
"#
        )
    };
    let with_caps = parse(&deck("CGSO=10u CGDO=10u")).unwrap();
    let without = parse(&deck("")).unwrap();
    let a = run(&with_caps).unwrap().ac.unwrap();
    let b = run(&without).unwrap().ac.unwrap();
    let d = node_index(&with_caps.circuit.node_names, "d");
    let lo = a.points.first().unwrap().node_voltages[d].norm();
    let hi = a.points.last().unwrap().node_voltages[d].norm();
    let hi_bare = b.points.last().unwrap().node_voltages[d].norm();
    assert!(lo > 0.9, "low-frequency gain {lo}");
    assert!(hi < 0.2, "high-frequency gain {hi}");
    assert!(hi_bare > 10.0 * hi, "capacitance must dominate at 100 MHz");
}

#[test]
fn mosfet_gate_capacitance_slows_the_gate_rise() {
    let deck = |cg: &str| {
        format!(
            r#"
.model nm NMOS(LEVEL=1 KP=110u VTO=1 {cg})
Vd d 0 DC 1
V1 in 0 PULSE(0 2 0 1n 1n 1 2)
R1 in g 1meg
M1 d g 0 0 nm W=1000u L=1u
.tran 100n 5u
.end
"#
        )
    };
    let with_caps = parse(&deck("CGSO=10u CGDO=10u")).unwrap();
    let without = parse(&deck("")).unwrap();
    let a = run(&with_caps).unwrap().tran.unwrap();
    let b = run(&without).unwrap().tran.unwrap();
    let g = node_index(&with_caps.circuit.node_names, "g");
    let at_1us = |t: &circuitsim::TranResult| {
        t.points
            .iter()
            .min_by(|x, y| {
                (x.time - 1e-6).abs().partial_cmp(&(y.time - 1e-6).abs()).unwrap()
            })
            .unwrap()
            .node_voltages[g]
    };
    assert!(at_1us(&a) < at_1us(&b) - 0.05, "gate should rise more slowly with Cgs");
}

/// Monte Carlo goes through the LTspice idiom: `random()`/`gauss()` draw
/// deterministically per parse and `.step` re-parses per point with that
/// point's overrides mixed into the draw seed — each stepped run gets a
/// rolled component value, reproducible across executions.
#[test]
fn step_re_rolls_monte_carlo_draws() {
    let deck = r#"
.param rm = { 1k * abs(1 + gauss(0.2)) }
V1 in 0 DC 5
R1 in out {rm}
R2 out 0 1k
.step param runn 1 3 1
.op
.end
"#;
    let netlist = parse(deck).unwrap();
    let out = node_index(&netlist.circuit.node_names, "out");
    let first = run(&netlist).unwrap();
    let spreads: Vec<f64> = first
        .runs
        .iter()
        .map(|run| run.dc.as_ref().unwrap().node_voltages[out])
        .collect();
    // |z| is capped by Box–Muller at ~7.4σ → rm' ∈ [0, 2.5k] → V(out) ∈ (1.43, 5).
    for (i, v) in spreads.iter().enumerate() {
        assert!((1.4..=5.0).contains(v), "run {i} out of the draw range: {v}");
    }
    assert!(spreads[0].to_bits() != spreads[1].to_bits(), "runs re-roll: {spreads:?}");

    // Reproducible: a second suite of the same text repeats every draw.
    let netlist2 = parse(deck).unwrap();
    let out2 = node_index(&netlist2.circuit.node_names, "out");
    let second = run(&netlist2).unwrap();
    let repeat: Vec<f64> = second
        .runs
        .iter()
        .map(|run| run.dc.as_ref().unwrap().node_voltages[out2])
        .collect();
    for i in 0..3 {
        assert_eq!(
            repeat[i].to_bits(),
            spreads[i].to_bits(),
            "draw {i} repeats across parses"
        );
    }
}

/// The sample whose time is closest to `tv` — transient output is adaptive,
/// so exact timestamps are not stored.
fn nearest_sample(t: &circuitsim::TranResult, tv: f64) -> &circuitsim::TranPoint {
    t.points
        .iter()
        .min_by(|x, y| {
            (x.time - tv).abs().partial_cmp(&(y.time - tv).abs()).unwrap()
        })
        .unwrap()
}

#[test]
fn capacitor_rser_charges_through_the_model_resistance() {
    let deck = |rser: &str| {
        format!(
            r#"
.model cm CAP(C=1n {rser})
V1 in 0 PULSE(0 1 0 1n 1n 1 2)
C1 in 0 cm
.tran 50n 5u
.end
"#
        )
    };
    let rser_deck = parse(&deck("RSER=1k")).unwrap();
    let plain_deck = parse(&deck("")).unwrap();
    let rser = run(&rser_deck).unwrap().tran.unwrap();
    let plain = run(&plain_deck).unwrap().tran.unwrap();
    // The arm/core junction node (expanded internal node) carries the RC curve.
    let cap_top = node_index(&rser_deck.circuit.node_names, "C1.int");
    // Every sample in the first 3 µs tracks V = 1 − exp(−t/RSER·C).
    for sample in rser.points.iter().skip(1) {
        if sample.time > 3e-6 {
            break;
        }
        if sample.time < 1e-7 {
            continue; // the 1 ns source edge region
        }
        let want = 1.0 - (-sample.time / 1e-6).exp();
        assert_relative_eq!(
            sample.node_voltages[cap_top],
            want,
            epsilon = 5e-3,
            max_relative = 0.02
        );
    }
    // Discriminator: at t ≈ 1 µs the plain capacitor is already full, the
    // RSER capacitor sits at 1 − e^−1.
    let mid = nearest_sample(&rser, 1e-6).node_voltages[cap_top];
    assert_relative_eq!(mid, 1.0 - (-1.0f64).exp(), epsilon = 5e-3, max_relative = 0.01);
    let plain_top = node_index(&plain_deck.circuit.node_names, "in");
    assert!(
        nearest_sample(&plain, 1e-6).node_voltages[plain_top] > 0.99,
        "plain cap charges instantly"
    );
}

/// Resistor model `RPAR` rides the card terminals as a real parallel arm
/// (TODO §5): the DC divider reads the combined 500 Ω drive impedance.
#[test]
fn resistor_rpar_lowers_the_dc_resistance() {
    let with = parse(
        r#"
.model rm R(RES=1k RPAR=1k)
V1 in 0 DC 5
R1 in out rm
R2 out 0 1k
.op
.end
"#,
    )
    .unwrap();
    let r = run(&with).unwrap().dc.unwrap();
    let out = node_index(&with.circuit.node_names, "out");
    assert_relative_eq!(r.node_voltages[out], 5.0 * 1000.0 / 1500.0, epsilon = 1e-6);
}

/// Inductor model `LSER` is a short at DC and adds jωL in AC (TODO §5):
/// at ωL = 1 kΩ the divider splits the drive between two resistors plus the
/// arm reactance.
#[test]
fn inductor_lser_is_short_at_dc_and_reactive_in_ac() {
    // DC view: the LSER arm is a plain short branch, so the divider reads
    // 1k against 1k.
    let dc_deck = parse(
        r#"
.model rm R(RES=1k LSER=1H)
V1 in 0 DC 5
R1 in out rm
RL out 0 1k
.op
.end
"#,
    )
    .unwrap();
    let dc = run(&dc_deck).unwrap().dc.unwrap();
    let out = node_index(&dc_deck.circuit.node_names, "out");
    assert_relative_eq!(dc.node_voltages[out], 2.5, epsilon = 1e-9);

    // AC view at ω = 1000 rad/s the arm reads j·1k: 1k / (2k + j·1k).
    let ac_deck = parse(
        r#"
.model rm R(RES=1k LSER=1H)
V1 in 0 DC 0 AC 1
R1 in out rm
RL out 0 1k
.ac lin 1 159.154943091895 159.154943091895
.end
"#,
    )
    .unwrap();
    let r = run(&ac_deck).unwrap();
    let out = node_index(&ac_deck.circuit.node_names, "out");
    let h = r.ac.unwrap().points[0].node_voltages[out];
    assert_relative_eq!(h.norm(), 1000.0 / (2000.0f64.hypot(1000.0)), epsilon = 1e-9);
    assert_relative_eq!(h.arg().to_degrees(), -26.5650512, epsilon = 1e-4);
}

// ── B sources: behavioral expressions (TODO §5) ──────────────────────────────

/// `I=…` flavour realizes a plain conductance: the B source drains
/// `V(out)/10k` from `out`, so 1k∥10k sees the divider 5·10/11 and the
/// reported device current matches the same fraction.
#[test]
fn behavioral_current_flavour_realizes_conductance() {
    let netlist = parse(
        r#"
* B-I conductance
V1 1 0 DC 5
R1 1 out 1k
B1 out 0 I=V(out)/10k
.op
.end
"#,
    )
    .unwrap();
    let dc = run(&netlist).unwrap().dc.unwrap();
    let out = node_index(&netlist.circuit.node_names, "out");
    // Divider of 1k against (B1 as) 10k.
    assert_relative_eq!(dc.node_voltages[out], 50.0 / 11.0, epsilon = 1e-9);
    assert_relative_eq!(
        dc.branch_currents.behavioral[0],
        (50.0 / 11.0) / 10_000.0,
        epsilon = 1e-15
    );
}

/// `V=…` flavour holds `V(out) = 2·V(1)` against the 1k load; the branch row
/// reports the current the source must source for it (−5 mA in the element
/// convention: a device pushing current into its own `out` node).
#[test]
fn behavioral_voltage_flavour_holds_and_sinks() {
    let netlist = parse(
        r#"
* B-V gain
V1 1 0 DC 5
R1 1 out 1k
B1 out 0 V=V(1)*2
.op
.end
"#,
    )
    .unwrap();
    let dc = run(&netlist).unwrap().dc.unwrap();
    let out = node_index(&netlist.circuit.node_names, "out");
    assert_relative_eq!(dc.node_voltages[out], 10.0, epsilon = 1e-9);
    // 10 V at `out` against the 5 V drive through 1 k raises `out` above
    // `V1`; the source sources the 5 mA that flows out through the load.
    assert_relative_eq!(dc.branch_currents.behavioral[0], -5e-3, epsilon = 1e-12);
}

/// `idt()` accumulates one trapezoid per accepted step over a linear argument
/// (`1000·time()`), which is exact on *any* accepted-step partition: the
/// projection reaches the analytic `∫1000t = 500·t²` at 2 ms no matter how
/// the controller chose steps.
#[test]
fn behavioral_idt_integrates_across_accepted_steps() {
    let netlist = parse(
        r#"
* idt of a linear argument
B1 out 0 V=idt(1000*time())
RL out 0 1m
.tran 10u 2m UIC
.end
"#,
    )
    .unwrap();
    let tran = run(&netlist).unwrap().tran.unwrap();
    let out = node_index(&netlist.circuit.node_names, "out");
    let last = tran.points.last().unwrap();
    assert_relative_eq!(last.time, 2e-3, epsilon = 1e-15);
    assert_relative_eq!(last.node_voltages[out], 500.0 * 4e-6, epsilon = 1e-12);
}

/// `delay(V(in), 2us)` replays the input two microseconds back: still 0 at
/// 3 µs (input was 0 at 1 µs), full pulse level at 5 µs (input was high at
/// 3 µs).
#[test]
fn behavioral_delay_replays_a_pulse() {
    let netlist = parse(
        r#"
* delayed pulse
V1 in 0 PULSE(0 1 2u 1n 1n 3u 10u)
B1 out 0 V=delay(V(in),2u)
.tran 100n 6u
.end
"#,
    )
    .unwrap();
    let tran = run(&netlist).unwrap().tran.unwrap();
    let out = node_index(&netlist.circuit.node_names, "out");
    let at = |t: f64| {
        let (point, _) = tran
            .points
            .iter()
            .enumerate()
            .map(|(_, p)| (p, (p.time - t).abs()))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap();
        assert!((point.time - t).abs() < 5e-8, "no sample lands on {t}");
        point.node_voltages[out]
    };
    assert_relative_eq!(at(3.0e-6), 0.0, epsilon = 1e-9);
    assert_relative_eq!(at(5.0e-6), 1.0, epsilon = 1e-9);
}

/// AC: the behavioral gain rides the small-signal solve — `V=V(n1)*2` doubles
/// the divider node phasor.
#[test]
fn behavioral_ac_gain_follows_the_small_signal() {
    let netlist = parse(
        r#"
* AC gain through a B source
V1 in 0 DC 0 AC 1
R1 in n1 1k
R2 n1 0 1k
R3 out 0 1k
B1 out 0 V=V(n1)*2
.ac lin 1 1000 1000
.op
.end
"#,
    )
    .unwrap();
    let result = run(&netlist).unwrap();
    let ac = result.ac.unwrap();
    let n1 = node_index(&netlist.circuit.node_names, "n1");
    let out = node_index(&netlist.circuit.node_names, "out");
    let pt = &ac.points[0];
    assert_relative_eq!(pt.node_voltages[n1].norm(), 0.5, epsilon = 1e-9);
    assert_relative_eq!(pt.node_voltages[out].norm(), 1.0, epsilon = 1e-9);
}

/// `I(V1)` is the element-convention current of a delivering source, so a
/// mirrored B-I output reproduces −10× the load current… with the load's
/// marks: 2.5 mA through the divider reads −2.5 mA at `V1`, mirrored (×10)
/// and pushed into `out` against 10 Ω → 0.25 V.
#[test]
fn behavioral_current_senses_source_current() {
    let netlist = parse(
        r#"
* sense I(V1)
V1 1 0 DC 5
R1 1 mid 1k
R2 mid 0 1k
B1 out 0 I=I(V1)*10
R3 out 0 10
.op
.end
"#,
    )
    .unwrap();
    let dc = run(&netlist).unwrap().dc.unwrap();
    // 5V across 2k is 2.5 mA; a delivering voltage source reports negative.
    assert_relative_eq!(dc.branch_currents.voltage_sources[0], -2.5e-3, epsilon = 1e-9);
    assert_relative_eq!(dc.branch_currents.behavioral[0], -2.5e-2, epsilon = 1e-12);
    let out = node_index(&netlist.circuit.node_names, "out");
    assert_relative_eq!(dc.node_voltages[out], 0.25, epsilon = 1e-9);
}

/// Inside a subckt the card name and every probe argument carry the instance
/// path; a top-level B card may also reach an internal net by its flattened
/// dotted name (`X1.mid`).
#[test]
fn behavioral_probes_resolve_inside_subckts() {
    let netlist = parse(
        r#"
* subckt-scoped probes
.subckt amp in out
R1 in mid 1k
R2 mid 0 1k
B1 out 0 V=V(mid)*11
.ends
X1 a o amp
B2 s 0 V=V(X1.mid)*2
V1 a 0 DC 5
.op
.end
"#,
    )
    .unwrap();
    assert_eq!(netlist.circuit.behavioral.len(), 2);
    assert_eq!(netlist.circuit.behavioral[0].name, "X1.B1");
    assert_eq!(netlist.circuit.behavioral[1].name, "B2");
    // Both probes resolved as node pairs (no lingering name forms).
    assert!(netlist
        .circuit
        .behavioral
        .iter()
        .all(|b| b.behavior.probes.iter().all(|p| p.is_unresolved() == false)));
    let dc = run(&netlist).unwrap().dc.unwrap();
    let o = node_index(&netlist.circuit.node_names, "o");
    let s = node_index(&netlist.circuit.node_names, "s");
    // mid = 5·1k/(1k+1k) = 2.5 V in the body; out = 11·mid.
    assert_relative_eq!(dc.node_voltages[o], 27.5, epsilon = 1e-9);
    assert_relative_eq!(dc.node_voltages[s], 5.0, epsilon = 1e-9);
}


/// A grounded-gate NJF sink drains the pull-up resistor: at vgs = 0 the
/// quadratic gives Id = β/2·VTO² = 4e-4 A, which pulls v(d) to 12 − 20k·4e-4
/// = 4.0 V. `branch_currents.jfets` reports the element current positive
/// *into* the drain terminal.
///
/// `.options gmin` (default 1e-12 S) also floors the gate junctions: the
/// reverse-biased G–D junction leaks ~gmin·|v_gd| = 4e-12 A through the
/// 1 MΩ gate arm, lifting v(g) ≈ 4e-6 V; the channel's dId/dvgs = β·(vgs+
/// VTO) ≈ 4e-4 A/V turns that into a 1.6e-9 A sink change, i.e. a
/// 20k·1.6e-9 ≈ 3.2e-5 V pull-down on d. (Pre-gmin the deck sat exactly at
/// 4.0 V — the pin absorbs only this junction-floor shift.)
#[test]
fn jfet_njf_core_saturates_like_a_current_sink() {
    let netlist = parse(
        r#"
* NJF quadratic current sink
.model jmod NJF(VTO=-2 BETA=2e-4 IS=1e-14)
V1 dd 0 DC 12
R1 dd d 20k
Rg g 0 1meg
J1 d g 0 jmod
.op
.end
"#,
    )
    .unwrap();
    assert_eq!(netlist.circuit.jfets.len(), 1);
    let dc = run(&netlist).unwrap().dc.unwrap();
    let d = node_index(&netlist.circuit.node_names, "d");
    assert_relative_eq!(dc.node_voltages[d], 4.0, epsilon = 1e-4);
    assert_relative_eq!(dc.branch_currents.jfets[0], 4e-4, max_relative = 1e-5);
}

/// PJF runs the same quadratic mirrored: with source held at 12 V and the
/// gate pulled up to it, the channel drains the load resistor, so the drain
/// sits at 20k·4e-4 = 8.0 V and the element current into the drain flips
/// negative (current exits the drain terminal into R1). The gate junction
/// gmin floor mirrors the NJF picture the same way (12 − v(g) ≈ 4e-6 across
/// the leak, so d drifts up by the mirrored ≈ 3.2e-5 V).
#[test]
fn jfet_pjf_mirror_drains_the_load() {
    let netlist = parse(
        r#"
* PJF mirror deck
.model pmod PJF(VTO=-2 BETA=2e-4 IS=1e-14)
V1 s 0 DC 12
Rg s g 1meg
R1 d 0 20k
J1 d g s pmod
.op
.end
"#,
    )
    .unwrap();
    let dc = run(&netlist).unwrap().dc.unwrap();
    let d = node_index(&netlist.circuit.node_names, "d");
    assert_relative_eq!(dc.node_voltages[d], 8.0, epsilon = 1e-4);
    assert_relative_eq!(dc.branch_currents.jfets[0], -4e-4, max_relative = 1e-5);
}

/// Gate junctions act like the diode model: forward gate conduction collars
/// the gate near the diode drop even from a 3 V rail through 1 MΩ (both
/// junctions in parallel, IS = 1e-14), while a reverse-driven gate sees no
/// residual current through the same resistor.
#[test]
fn jfet_gate_junction_conducts_forward_and_blocks_reverse() {
    let forward = parse(
        r#"
.model jmod NJF(VTO=-2 BETA=2e-4 IS=1e-14)
Vg rail 0 DC 3
Rg rail g 1meg
Vs s 0 DC 0
Vd d 0 DC 0
J1 d g s jmod
.op
.end
"#,
    )
    .unwrap();
    let fw = run(&forward).unwrap().dc.unwrap();
    let g = node_index(&forward.circuit.node_names, "g");
    // (3 − v)/1e6 = 2·IS·exp(v/Vt) balances at v ≈ 0.48 V.
    assert!((fw.node_voltages[g] - 0.48).abs() < 0.02,
            "forward gate {} not near the diode drop", fw.node_voltages[g]);

    let reverse = parse(
        r#"
.model jmod NJF(VTO=-2 BETA=2e-4 IS=1e-14)
Vg rail 0 DC -5
Rg rail g 1meg
Vs s 0 DC 0
Vd d 0 DC 0
J1 d g s jmod
.op
.end
"#,
    )
    .unwrap();
    let rv = run(&reverse).unwrap().dc.unwrap();
    // Both gate junctions sit at vg = −5 (reverse), so the junction gmin
    // floor leaks 2·gmin·|vg| = 1e-11 A up the 1 MΩ arm: v(g) ≈ −5 + 1e-5.
    assert_relative_eq!(rv.node_voltages[g], -5.0, epsilon = 2e-5);
}

/// AREA on the element card scales the channel (β·AREA) and the junctions
/// (IS·AREA); doubling AREA doubles the sink current and halves the pull-up
/// drop, exactly as the model-level unit tests pin.
#[test]
fn jfet_area_scales_the_channel() {
    let netlist = parse(
        r#"
* area-scaled NJF sink
.model jmod NJF(VTO=-2 BETA=2e-4 IS=1e-14)
V1 dd 0 DC 12
R1 dd d 20k
Rg g 0 1meg
J1 d g 0 jmod AREA=2
.op
.end
"#,
    )
    .unwrap();
    let dc = run(&netlist).unwrap().dc.unwrap();
    let d = node_index(&netlist.circuit.node_names, "d");
    // 2× AREA lifts β_A = 4e-4; the pull-up can no longer hold saturation
    // (β_A/2·VTO² = 8e-4 needs a 16 V drop), so the balance is the triode
    // quadratic v = 12 − 20k·β_A·(2v − v²/2): v = (17 − √97)/8 ≈ 0.893893.
    assert_relative_eq!(dc.node_voltages[d], (17.0 - 97f64.sqrt()) / 8.0,
                        epsilon = 1e-6);
    assert!(dc.branch_currents.jfets[0] > 4e-4, "area doubles the sink");
}

/// RD/RS materialize as real resistor arms inside the device: the arms carry
/// the element names `J1_RD`/`J1_RS`, the internal nets are `J1.dint`/`J1.sint`,
/// and the degenerated sink balances at I = β/2·(VTO−RS·I)² … giving the
/// analytic root v(J1.sint) = 12 − √140 ≈ 0.1678404 V.
#[test]
fn jfet_series_resistance_degenerates_the_sink() {
    let netlist = parse(
        r#"
* RD/RS degenerated deck
.model jmod NJF(VTO=-2 BETA=2e-4 IS=1e-14 RD=1k RS=500)
V1 dd 0 DC 12
R1 dd d 20k
Rg g 0 1meg
J1 d g 0 jmod
.op
.end
"#,
    )
    .unwrap();
    assert!(netlist
        .circuit
        .resistors
        .iter()
        .any(|r| r.name == "J1_RD" && (r.resistance - 1e3).abs() < 1e-12));
    assert!(netlist
        .circuit
        .resistors
        .iter()
        .any(|r| r.name == "J1_RS" && (r.resistance - 5e2).abs() < 1e-12));
    let dc = run(&netlist).unwrap().dc.unwrap();
    let sint = node_index(&netlist.circuit.node_names, "J1.sint");
    // Root sanity: the quadratic (2 − 500i)²·1e-4 = i → v(sint) = 500·i =
    // 12 − √140 V. The G–D junction gmin floor leaks ~gmin·|v_gd|·1 MΩ ≈
    // 5.1e-6 V onto the gate, which moves the sink by ≈ 1.6e-9 A (~4.7e-6
    // relative); v(sint) = 500·I inherits ≈ 8e-7 V of that.
    assert_relative_eq!(dc.node_voltages[sint], 12.0 - 140f64.sqrt(),
                        epsilon = 2e-6);
    assert_relative_eq!(dc.branch_currents.jfets[0], 3.356824589854364e-4,
                        max_relative = 1e-6);
}

/// TODO §3 gmin floor: at DC the engine keeps a small `gmin` conductance
/// (default 1e-12 S, classic SPICE) across every pn junction *and* every
/// capacitor, so a node reachable only through capacitors is not floating —
/// it sits at the gmin divider midpoint. `.options gmin=0` restores the
/// literal open circuit and the solve goes singular, which the whole
/// gmin/source-stepping ladder reports as the generic
/// "DC operating point failed to converge".
#[test]
fn gmin_floor_already_solves_the_capacitor_island() {
    let deck = |options: &str| {
        format!(
            r#"
* island: node b is reachable only through capacitors (plus their gmin)
V1 a 0 DC 1
C1 a b 1n
C2 b 0 1n
{options}
.op
.end
"#
        )
    };
    let netlist = parse(&deck("")).unwrap();
    let dc = run(&netlist).unwrap().dc.unwrap();
    let b = node_index(&netlist.circuit.node_names, "b");
    // Two equal gmin arms → exactly the halfway point (the value is set by
    // the divider, not by the knob: a bigger gmin gives the same 0.5).
    assert_relative_eq!(dc.node_voltages[b], 0.5, epsilon = 1e-9);

    // With the floor disabled the island is truly disconnected and the DC
    // solve cannot come back — singular matrix at every build.
    let floored = parse(&deck(".options gmin=0")).unwrap();
    let err = run(&floored).unwrap_err();
    assert!(
        err.to_string().contains("failed to converge"),
        "expected the ladder exhaust error, got {err}"
    );
}

/// TODO §3 itl1: `.options itl1=` caps the Newton-Raphson iteration budget of
/// the DC operating point. From the 1e6 seed this deck needs 5 builds to
/// walk the diode bias down from "everything at a megavolt" onto the knee
/// (measured: itl1 ∈ {1,2,3,4} errors, 5 passes), so a budget of 3
/// exhausts with the generic error and a budget of 8 converges to the same
/// gmin-limited fixed point the un-budgeted solve lands on.
///
/// Ladder structure note (documented hunt): no known deck needs the
/// *stepped* retry ladders to converge. An exhaustive empirical hunt —
/// diode chains at itl1 1..20, symmetric and asymmetric MOS/BJT latches,
/// breakdown decks, current-driven diodes at ±srcsteps — never found a deck
/// the plain stage fails but a gmin- or source-stepped rung succeeds on;
/// the plain stage runs with the same per-rung `itl1` budget the stepped
/// rungs get. These tests therefore pin the ladder structurally through the
/// budget-exhaustion path: every rung executes, and the generic error is
/// what a starved solve surfaces.
#[test]
fn itl1_budget_governs_the_dc_operating_point() {
    let deck = |options: &str| {
        format!(
            r#"
* diode-charged node with a tight DC iteration budget
.model dmod D(IS=1e-14)
V1 a 0 DC 3
D1 a k dmod
C1 k 0 1n
{options}
.op
.end
"#
        )
    };
    // Starved: the deck needs at least 5 builds.
    let starved = parse(&deck(".options itl1=3")).unwrap();
    let err = run(&starved).unwrap_err();
    assert!(
        err.to_string().contains("DC operating point failed to converge"),
        "expected the DC exhaust error, got {err}"
    );

    let budgeted = parse(&deck(".options itl1=8")).unwrap();
    let dc = run(&budgeted).unwrap().dc.unwrap();
    let k = node_index(&budgeted.circuit.node_names, "k");
    // The cap isolates k at DC; its only DC coupling is the diode junction
    // plus the gmin floor, so k settles at the gmin-balance point
    // IS·exp(vd/vt) ≈ gmin·(3−2·vd) with vd ≈ 0.145 V.
    assert_relative_eq!(dc.node_voltages[k], 2.855006, epsilon = 1e-5);
    // And it is the same fixed point the default (itl1=100) solve finds.
    let plain = parse(&deck("")).unwrap();
    let plain_dc = run(&plain).unwrap().dc.unwrap();
    let k = node_index(&plain.circuit.node_names, "k");
    assert_relative_eq!(plain_dc.node_voltages[k], dc.node_voltages[k],
                        epsilon = 1e-12);
}

/// TODO §3 itl4: `.options itl4=` caps the Newton-Raphson budget of every
/// *transient* time point (default 100, kept from the pre-options engine).
/// The deck's pulse has a zero-width rise: at the t = 2 ns edge node `a`
/// jumps 3 V in one step and the junction + cap-companion system needs
/// three builds to re-find the knee, so a budget of 1 fails that point and
/// the run aborts with the per-point transient error, while a budget of 5
/// walks through the whole 20 ns.
#[test]
fn itl4_budget_governs_the_transient_time_points() {
    let deck = |options: &str| {
        format!(
            r#"
* sharp-edged diode/transient with a tight per-point budget
.model dmod D(IS=1e-14)
V1 a 0 PULSE(0 3 2n 0 0 8n 20n)
D1 a k dmod
C1 k 0 1u
.tran 100n 20n
{options}
.end
"#
        )
    };
    // Starved: the first edge point needs more than one build.
    let starved = parse(&deck(".options itl4=1")).unwrap();
    let err = run(&starved).unwrap_err();
    assert!(
        err.to_string().contains("Transient failed to converge"),
        "expected the transient point error, got {err}"
    );

    let budgeted = parse(&deck(".options itl4=5")).unwrap();
    let tran = run(&budgeted).unwrap().tran.unwrap();
    let k = node_index(&budgeted.circuit.node_names, "k");
    // The whole run completes at the stop time, and the slow 1 µF/IS-charge
    // pulled node k up through the 0.1 V level during the 8 ns high phase.
    assert_relative_eq!(tran.points.last().unwrap().time, 2e-8, epsilon = 1e-9);
    let peak = tran.points.iter().map(|p| p.node_voltages[k])
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(peak > 0.1, "cap never charged through the edge: {peak}");
}

/// Adding CGS/CGD loads drain→gate: with the mirror stage the 10 pF pair
/// caps the 100 MHz response far below the bare deck (same comparison shape
/// as the MOSFET capacitance test).
#[test]
fn jfet_gate_capacitances_load_the_drain() {
    let deck = |caps: &str| {
        format!(
            r#"
.model jm NJF(VTO=-2 BETA=2e-4 IS=1e-14 {caps})
Vd dd 0 DC 8 AC 1
R1 dd d 10k
Rg g 0 1meg
J1 d g 0 jm
.ac dec 5 1k 100meg
.end
"#
        )
    };
    let with_caps = parse(&deck("CGS=10p CGD=10p PB=1")).unwrap();
    let without = parse(&deck("")).unwrap();
    let a = run(&with_caps).unwrap().ac.unwrap();
    let b = run(&without).unwrap().ac.unwrap();
    let d = node_index(&with_caps.circuit.node_names, "d");
    let hi = a.points.last().unwrap().node_voltages[d].norm();
    let hi_bare = b.points.last().unwrap().node_voltages[d].norm();
    // The drain-side pole of 10 pF against the ~4 V/… gain stage: capacitance
    // must dominate at 100 MHz.
    assert!(hi_bare > 10.0 * hi, "bare {hi_bare} gated {hi}");
}

/// Switching the gate below pinch-off shuts the channel: the drain rises from
/// the R1/J1 divider level back up to the 8 V rail, and the element current
/// through J1 collapses below the junction dribble.
#[test]
fn jfet_gate_step_cuts_off_the_channel() {
    let netlist = parse(
        r#"
* gate-step cutoff transient
.model jmod NJF(VTO=-2 BETA=2e-4 IS=1e-14 CGS=10p CGD=10p PB=1)
V1 dd 0 DC 8
R1 dd d 10k
Vg g 0 PULSE(0 -3 20u 1u 1u 30u 50u)
J1 d g 0 jmod
.tran 1u 60u
.end
"#,
    )
    .unwrap();
    let tran = run(&netlist).unwrap().tran.unwrap();
    let d = node_index(&netlist.circuit.node_names, "d");
    let at = |t: f64| {
        tran.points
            .iter()
            .min_by(|x, y| {
                (x.time - t).abs().partial_cmp(&(y.time - t).abs()).unwrap()
            })
            .unwrap()
    };
    let jfets = |i: usize| tran.points[i].branch_currents.jfets[0];
    // While the gate sits at 0 the channel sinks 4e-4 A → v(d) ≈ 4.0.
    assert_relative_eq!(tran.points.first().unwrap().node_voltages[d], 4.0,
                        epsilon = 1e-6);
    assert_relative_eq!(jfets(0), 4e-4, max_relative = 1e-6);
    // The pulse holds −3 V from 21 µ to 51 µ: the channel is cut, v(d) floats
    // up to the rail and the drain current drops to the reverse-junction
    // dribble below IS-scale currents.
    let cut = at(30e-6);
    assert_relative_eq!(cut.node_voltages[d], 8.0, epsilon = 1e-5);
    let cut_i = jfets(tran.points.iter().position(|p| p.time == cut.time).unwrap());
    assert!(cut_i.abs() < 1e-12, "cutoff current {cut_i}");
    // per = 50 µ: the second period has not started at 60 µ, so the gate is
    // back at 0 and the divider level (and the current) recover.
    let last = tran.points.last().unwrap();
    assert_relative_eq!(last.node_voltages[d], 4.0, epsilon = 1e-5);
    assert_relative_eq!(jfets(tran.points.len() - 1), 4e-4, epsilon = 1e-9);
}

/// A stiff forward-active bias (Vb holds the base, Vc the collector) needs no
/// Newton groundwork: vbe = 0.7 and vbc = 0 exactly, so the Ebers–Moll port
/// equations pin every terminal current analytically — ic/ib = BF.
#[test]
fn bjt_forward_active_matches_the_analytic_ports() {
    let netlist = parse(
        r#"
.model np NPN(IS=1e-14 BF=100)
Vb b 0 DC 0.7
Vc c 0 DC 0.7
Q1 c b 0 np
.op
.end
"#,
    )
    .unwrap();
    assert_eq!(netlist.circuit.bjts.len(), 1);
    let dc = run(&netlist).unwrap().dc.unwrap();
    assert_relative_eq!(dc.branch_currents.bjts[0], 0.005670294683520749,
                        max_relative = 1e-6);
}

/// With IKF = 1 mA the stiff 0.7 V bias runs the quadrature q2 = I_f/IKF =
/// 5.67: qb = 2.9333 and the transport current drops to the same analytic
/// value the model-level pin found.
#[test]
fn bjt_ikf_knee_rolls_the_stiff_op_back() {
    let netlist = parse(
        r#"
.model np NPN(IS=1e-14 BF=100 IKF=1m)
Vb b 0 DC 0.7
Vc c 0 DC 0.7
Q1 c b 0 np
.op
.end
"#,
    )
    .unwrap();
    let dc = run(&netlist).unwrap().dc.unwrap();
    assert_relative_eq!(dc.branch_currents.bjts[0], 0.001933165568456193,
                        max_relative = 1e-6);
}

/// XTB = 1 at 127 °C: IS follows its own XTI/EG law while β scales as
/// (T/Tnom) = 1.3332 — the warm operating point matches the analytic currents.
#[test]
fn bjt_xtb_scales_the_beta_in_a_warm_deck() {
    let netlist = parse(
        r#"
.model np NPN(IS=1e-14 BF=100 XTB=1)
.temp 127
Vb b 0 DC 0.7
Vc c 0 DC 0.7
Q1 c b 0 np
.op
.end
"#,
    )
    .unwrap();
    let dc = run(&netlist).unwrap().dc.unwrap();
    assert_relative_eq!(dc.branch_currents.bjts[0], 0.7057933695182553,
                        max_relative = 1e-6);
    assert_relative_eq!(dc.node_voltages[node_index(&netlist.circuit.node_names, "b")],
                        0.7, epsilon = 1e-9);
}

/// Each series slot solves to its own 1-D analytic corner when only that slot
/// is populated: the emitter arm squares the diode-level balance, the base arm
/// subtracts its IR drop, and the collector arm pulls the internal collector
/// down to the resistive drop.
#[test]
fn bjt_each_series_slot_holds_its_own_analytic_corner() {
    let re_deck = parse(
        r#"
.model n2 NPN(IS=1e-14 BF=100 RE=200)
Vb b 0 DC 0.7
Vc c 0 DC 3
Q1 c b 0 n2
.op
.end
"#,
    )
    .unwrap();
    let dc = run(&re_deck).unwrap().dc.unwrap();
    assert!(re_deck.circuit.resistors.iter()
        .any(|r| r.name.eq_ignore_ascii_case("Q1_RE") && (r.resistance - 200.0).abs() < 1e-12));
    assert_relative_eq!(dc.node_voltages[node_index(&re_deck.circuit.node_names, "Q1.eint")],
                        0.07167955298947117, epsilon = 1e-5);
    assert_relative_eq!(dc.branch_currents.bjts[0], 0.0003548492722351888,
                        max_relative = 1e-5);

    let rb_deck = parse(
        r#"
.model n3 NPN(IS=1e-14 BF=100 RB=1k)
Vb b 0 DC 0.7
Vc c 0 DC 3
Q1 c b 0 n3
.op
.end
"#,
    )
    .unwrap();
    let dc = run(&rb_deck).unwrap().dc.unwrap();
    assert!(rb_deck.circuit.resistors.iter()
        .any(|r| r.name.eq_ignore_ascii_case("Q1_RB") && (r.resistance - 1e3).abs() < 1e-12));
    assert_relative_eq!(dc.node_voltages[node_index(&rb_deck.circuit.node_names, "Q1.bint")],
                        0.6768402634366125, epsilon = 1e-5);
    assert_relative_eq!(dc.branch_currents.bjts[0], 0.002315973657358745,
                        max_relative = 1e-5);

    let rc_deck = parse(
        r#"
.model n4 NPN(IS=1e-14 BF=100 RC=1k)
Vb b 0 DC 0.7
Vc c 0 DC 3
Q1 c b 0 n4
.op
.end
"#,
    )
    .unwrap();
    let dc = run(&rc_deck).unwrap().dc.unwrap();
    assert!(rc_deck.circuit.resistors.iter()
        .any(|r| r.name.eq_ignore_ascii_case("Q1_RC") && (r.resistance - 1e3).abs() < 1e-12));
    assert_relative_eq!(dc.node_voltages[node_index(&rc_deck.circuit.node_names, "Q1.cint")],
                        0.03704944069253077, epsilon = 1e-5);
    assert_relative_eq!(dc.branch_currents.bjts[0], 0.002962950559307482,
                        max_relative = 1e-5);
}

/// With all three slots populated the expansion keeps the whole device self
/// consistent: three arms of the card values carry the terminal currents (KCL
/// at each internal node ties the resistor arm current to the element
/// terminal), and the three internal node voltages land on the combined
/// Newton solution.
#[test]
fn bjt_series_slots_materialize_as_arms() {
    let netlist = parse(
        r#"
.model full NPN(IS=1e-14 BF=100 RB=1k RC=500 RE=200)
Vb b 0 DC 0.7
Vc c 0 DC 3
Q1 c b 0 full
.op
.end
"#,
    )
    .unwrap();
    let circuit = &netlist.circuit;
    assert!(circuit.resistors.iter()
        .any(|r| r.name.eq_ignore_ascii_case("Q1_RB") && (r.resistance - 1e3).abs() < 1e-12));
    assert!(circuit.resistors.iter()
        .any(|r| r.name.eq_ignore_ascii_case("Q1_RC") && (r.resistance - 500.0).abs() < 1e-12));
    assert!(circuit.resistors.iter()
        .any(|r| r.name.eq_ignore_ascii_case("Q1_RE") && (r.resistance - 200.0).abs() < 1e-12));
    let dc = run(&netlist).unwrap().dc;
    let dc = dc.unwrap();
    let v = |name: &str| dc.node_voltages[node_index(&circuit.node_names, name)];
    assert_relative_eq!(v("Q1.bint"), 0.6965754964446849, epsilon = 1e-5);
    assert_relative_eq!(v("Q1.cint"), 2.828774821724247, epsilon = 1e-5);
    assert_relative_eq!(v("Q1.eint"), 0.06917497202136423, epsilon = 1e-5);
    assert_relative_eq!(dc.branch_currents.bjts[0], 0.0003424503565514932,
                        max_relative = 1e-5);
    // KCL at the collector internal node: the RC arm current equals the
    // element's collector terminal current, both flowing c → cint.
    let ridx = circuit.resistors.iter()
        .position(|r| r.name.eq_ignore_ascii_case("Q1_RC")).unwrap();
    assert_relative_eq!(dc.branch_currents.resistors[ridx], dc.branch_currents.bjts[0],
                        max_relative = 1e-5);
}

/// `.tran … STARTUP` ramps every independent V/I source from zero over the
/// first 10 µs: the RC charge falls well behind the straight-deck charge at
/// every probe time while the steady behaviour (post-ramp) converges back to
/// the plain exponential, exactly as the analytic ramped-ODE solution pins.
#[test]
fn tran_startup_ramps_the_sources_over_ten_micros() {
    let deck_fn = || {
        parse(
            r#"
V1 in 0 DC 5
R1 in out 1k
C1 out 0 1u
.tran 500n 30u STARTUP
.end
"#,
        )
        .unwrap()
    };
    let without_deck = parse(
        r#"
V1 in 0 DC 5
R1 in out 1k
C1 out 0 1u
.tran 500n 30u
.end
"#,
    )
    .unwrap();
    let on_deck = deck_fn();
    let on = run(&on_deck).unwrap().tran.unwrap();
    let off = run(&without_deck).unwrap().tran.unwrap();
    let line = |t: &circuitsim::TranResult, target: f64| {
        t.points
            .iter()
            .min_by(|x, y| {
                (x.time - target).abs().partial_cmp(&(y.time - target).abs()).unwrap()
            })
            .unwrap()
            .node_voltages[node_index(&on_deck.circuit.node_names, "out")]
    };
    // Analytic ramped ODE (two routes agree to 1e-9) at t = 2/3.5/10/20/30 µs.
    for (target, pin) in [
        (2e-6, 0.0009993336665333776),
        (3.5e-6, 0.003058930207448281),
        (10e-6, 0.02491687458402679),
        (20e-6, 0.07441977879362435),
        (30e-6, 0.12343012087643736),
    ] {
        assert_relative_eq!(line(&on, target), pin, max_relative = 5e-3);
    }
    // The straight card value (no STARTUP) charges faster everywhere.
    assert!(line(&off, 10e-6) > line(&on, 10e-6));
    assert!(line(&off, 30e-6) > line(&on, 30e-6));
    // The zero-source seed: the first point sits at exactly 0 V.
    assert_relative_eq!(
        on.points.first().unwrap().node_voltages[node_index(&on_deck.circuit.node_names, "out")],
        0.0,
        epsilon = 1e-12
    );
}

/// A pure integrator under STARTUP shows the ramp directly: charging a 1 nF
/// cap from a 5 mA source gives v = I/(C·Td)·t²/2 during the ramp (6.25 V at
/// 5 µs, 25 V — twice that — at 10 µ) and then the plain I·t/C thereafter.
#[test]
fn tran_startup_integrator_ramp_is_semicircular() {
    let netlist = parse(
        r#"
I1 0 out DC 5m
R1 out 0 1g
C1 out 0 1n
.tran 250n 20u STARTUP
.end
"#,
    )
    .unwrap();
    let tran = run(&netlist).unwrap().tran.unwrap();
    let out_deck = netlist.clone();
    let out = |target: f64| {
        tran.points
            .iter()
            .min_by(|x, y| {
                (x.time - target).abs().partial_cmp(&(y.time - target).abs()).unwrap()
            })
            .unwrap()
            .node_voltages[node_index(&out_deck.circuit.node_names, "out")]
    };
    // Ramp window: t² law — 6.25 V at the midpoint, 25 V at the end.
    assert_relative_eq!(out(5e-6), 25.0 * 0.25, epsilon = 0.2);
    assert_relative_eq!(out(10e-6), 25.0, epsilon = 0.2);
    // Post-ramp: linear rise at I/C = 5e3·t again — +25 V per 5 µs.
    assert_relative_eq!(out(20e-6), 75.0, epsilon = 0.3);
    assert_relative_eq!(out(0.0), 0.0, epsilon = 1e-12);
}

/// Behavioral sources are part of the model family, not supplies: they hold
/// their expression's value from the start (the ramp multiplies independent
/// V/I cards only).
#[test]
fn tran_startup_leaves_behavioral_sources_alone() {
    let netlist = parse(
        r#"
B1 out 0 V=2
R1 out 0 1k
.tran 1u 15u STARTUP
.end
"#,
    )
    .unwrap();
    let tran = run(&netlist).unwrap().tran.unwrap();
    let out = node_index(&netlist.circuit.node_names, "out");
    assert_relative_eq!(
        tran.points.first().unwrap().node_voltages[out],
        2.0,
        epsilon = 1e-9
    );
    assert_relative_eq!(
        tran.points.last().unwrap().node_voltages[out],
        2.0,
        epsilon = 1e-9
    );
}
