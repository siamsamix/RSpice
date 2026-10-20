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
