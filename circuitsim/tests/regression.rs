//! Golden-result regression corpus: analytic references for the canonical
//! circuits, plus a manifest walk that simulates every `examples/*.cir` deck.

use approx::assert_relative_eq;
use circuitsim::{parse_with_options, run, ParseOptions};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn options_for(path: &Path) -> ParseOptions {
    ParseOptions {
        deck_dir: path.parent().map(Path::to_path_buf),
        search_paths: vec![root().join("libs")],
    }
}

fn simulate_example(name: &str) -> circuitsim::Netlist {
    let path = root().join("examples").join(name);
    let source = std::fs::read_to_string(&path).expect("example deck");
    parse_with_options(&source, &options_for(&path)).expect("example parses")
}

fn node(net: &circuitsim::Netlist, name: &str) -> usize {
    net.circuit
        .node_names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(name))
        .unwrap_or_else(|| panic!("node {name} missing"))
}

#[test]
fn rc_step_matches_analytic_exponential() {
    // A zero-to-five-volt edge, so the DC operating point starts at 0 V.
    let deck = r#"
V1 in 0 PULSE(0 5 0 1n 1n 1 2)
R1 in out 1k
C1 out 0 1u
.tran 10u 5m
.end
"#;
    let n = parse_with_options(
        deck,
        &ParseOptions { deck_dir: None, search_paths: Vec::new() },
    )
    .unwrap();
    let r = run(&n).unwrap();
    let tran = r.tran.unwrap();
    let out = node(&n, "out");
    let last = tran.points.last().unwrap();
    // τ = 1 kΩ · 1 µF = 1 ms, tstop = 5 ms.
    let analytic = 5.0 * (1.0 - (-5.0e-3f64 / 1.0e-3).exp());
    assert_relative_eq!(last.node_voltages[out], analytic, epsilon = 1e-3);
}

#[test]
fn rl_step_matches_analytic_current() {
    let n = simulate_example("rl_circuit.cir");
    let r = run(&n).unwrap();
    let tran = r.tran.unwrap();
    let last = tran.points.last().unwrap();
    // The deck is V1 − R1 − L1 to ground with R = 100 Ω, L = 1 mH.
    // I(∞) = V/R; τ = L/R; compare at the final point.
    let v = 12.0;
    let r_ohm = 100.0;
    let tau = 1e-3 / r_ohm;
    let analytic = (v / r_ohm) * (1.0 - (-last.time / tau).exp());
    assert_relative_eq!(
        last.branch_currents.inductors[0],
        analytic,
        epsilon = (v / r_ohm) * 1e-3
    );
}

#[test]
fn series_rlc_peaks_at_the_resonant_frequency() {
    let n = simulate_example("rlc_bandpass.cir");
    let r = run(&n).unwrap();
    let ac = r.ac.unwrap();
    let mid = node(&n, "out");
    let (peak_freq, peak_mag) = ac
        .points
        .iter()
        .map(|p| (p.freq, p.node_voltages[mid].norm()))
        .fold((0.0, 0.0), |best, cand| if cand.1 > best.1 { cand } else { best });
    let f0 = 1.0 / (2.0 * std::f64::consts::PI * (1e-3f64 * 1e-6).sqrt());
    assert_relative_eq!(peak_freq, f0, max_relative = 0.05);
    assert!(peak_mag > 1.5, "peak magnitude {peak_mag}");
}

#[test]
fn diode_dc_curve_matches_shockley() {
    let n = simulate_example("diode_rectifier.cir");
    // Re-purpose the deck's model in a tiny DC sweep rather than a transient.
    let deck = r#"
.model DMOD D(IS=1e-14 N=1)
V1 a 0 DC 0
D1 a 0 DMOD
.dc V1 0 0.6 0.1
.end
"#;
    let parsed = parse_with_options(
        deck,
        &ParseOptions { deck_dir: None, search_paths: vec![root().join("libs")] },
    )
    .unwrap();
    let r = run(&parsed).unwrap();
    let sweep = r.dc_sweep.unwrap();
    let last = sweep.points.last().unwrap();
    let model = &parsed.circuit.diode_models["dmod"];
    let i = model.eval_for(at_last_voltage(last.values[0]), 1.0).i_d;
    assert_relative_eq!(last.branch_currents.diodes[0], i, max_relative = 1e-6);
    let _ = n;
}

fn at_last_voltage(v: f64) -> f64 {
    v
}

#[test]
fn cmos_inverter_output_is_between_the_rails() {
    let n = simulate_example("cmos_inverter.cir");
    let r = run(&n).unwrap();
    let out = node(&n, "out");
    let v = r.dc.unwrap().node_voltages[out];
    assert!((0.0..=1.8).contains(&v), "inverter output {v}");
}

#[test]
fn bundled_opamp_subckt_gives_the_feedback_gain() {
    let n = simulate_example("opamp_gain.cir");
    let r = run(&n).unwrap();
    let tf = r.tf.expect(".tf result");
    // Non-inverting: 1 + 9k/1k = 10 (finite gain drops it slightly).
    assert_relative_eq!(tf.transfer, 10.0, max_relative = 1e-2);
}

/// The bundled two-pole macromodel (`opamp2` in `libs/opamps.lib`) is the
/// exact two-pole map its header documents: |H| = A0/|1+jf/f1|·|1+jf/f2|
/// with f1 = 1/(2π·R1·C1) and f2 = 1/(2π·R2·C2) from the gm-RC chain.
#[test]
fn bundled_two_pole_opamp_follows_the_documented_map() {
    let deck = r#"
.include opamps.lib
V1 in 0 DC 0 AC 1
X1 in 0 out opamp2
.op
.ac DEC 1 100 10k
.end
"#;
    let n = parse_with_options(
        deck,
        &ParseOptions { deck_dir: None, search_paths: vec![root().join("libs")] },
    )
    .unwrap();
    assert!(n.warnings.is_empty(), "{:?}", n.warnings);
    let r = run(&n).unwrap();
    let ac = r.ac.expect("ac result");

    let (f1, f2) = (
        1.0 / (2.0 * std::f64::consts::PI * 1.0 * 15.9e-3),
        1.0 / (2.0 * std::f64::consts::PI * 1.0 * 3.979e-6),
    );
    let a0 = 1e5;
    let out = node(&n, "out");
    for pt in &ac.points {
        let f = pt.freq;
        let expect =
            a0 / ((1.0 + (f / f1) * (f / f1)).sqrt() * (1.0 + (f / f2) * (f / f2)).sqrt());
        assert_relative_eq!(pt.node_voltages[out].norm(), expect, max_relative = 1e-6);
    }
}

#[test]
fn every_example_deck_parses_and_simulates() {
    let mut seen = 0usize;
    for entry in std::fs::read_dir(root().join("examples")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("cir") {
            continue;
        }
        seen += 1;
        let source = std::fs::read_to_string(&path).unwrap();
        let netlist = parse_with_options(&source, &options_for(&path))
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        run(&netlist).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
    assert!(seen >= 10, "expected a non-trivial corpus, saw {seen}");
}
