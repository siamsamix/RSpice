//! TODO §3 AVX2 math: `.options fastmath=` is opt-in, parses, round-trips, and
//! changes results only within the kernels' documented few-ulp budget.

use circuitsim::{parse, run, run_with_threads};

fn diode_array(n: usize, options: &str) -> String {
    let mut deck = String::from("V1 in 0 5\n");
    deck.push_str(".model dd D(IS=2e-14 N=1.1 CJO=2p BV=40 IBV=1e-6)\n");
    for i in 0..n {
        let r = 400.0 + 7.0 * i as f64;
        deck.push_str(&format!("R{i} in a{i} {r}\nD{i} a{i} 0 dd\n"));
    }
    deck.push_str(options);
    deck.push_str("\n.op\n");
    deck
}

fn node_voltages(deck: &str) -> Vec<f64> {
    let result = run(&parse(deck).expect("parses")).expect("runs");
    let op = result.dc.expect("op result");
    op.node_voltages.clone()
}

#[test]
fn the_option_parses_defaults_off_and_round_trips() {
    let off = parse(&diode_array(3, "")).unwrap();
    assert!(!off.analysis.config.fast_math && !off.circuit.fast_math);

    let on = parse(&diode_array(3, ".options fastmath=1")).unwrap();
    assert!(on.analysis.config.fast_math && on.circuit.fast_math);

    let emitted = circuitsim::netlist::emit(&on);
    assert!(emitted.contains("fastmath=1"), "emit lost fastmath: {emitted}");
    let again = parse(&emitted).unwrap();
    assert!(again.circuit.fast_math, "round trip dropped fastmath");

    // 0 switches it back off, and the default emits nothing.
    let zero = parse(&diode_array(3, ".options fastmath=1 fastmath=0")).unwrap();
    assert!(!zero.circuit.fast_math);
    assert!(!circuitsim::netlist::emit(&off).contains("fastmath"));
}

#[test]
fn fastmath_agrees_with_libm_to_solver_tolerance() {
    // 300 diodes: well past one vector's worth, with a ragged tail.
    let plain = node_voltages(&diode_array(301, ""));
    let fast = node_voltages(&diode_array(301, ".options fastmath=1"));
    assert_eq!(plain.len(), fast.len());
    for (i, (a, b)) in plain.iter().zip(&fast).enumerate() {
        // The kernels are within 4 ulp of libm per exponential; the Newton
        // solution inherits that to far better than the solver's own 1e-6 V.
        assert!((a - b).abs() <= 1e-9 * a.abs().max(1.0), "node {i}: {a} vs {b}");
    }
}

#[test]
fn the_default_path_is_deterministic_across_thread_counts() {
    // Without the option nothing vectorized runs, so the result is exactly
    // the libm one regardless of the machine's SIMD support or thread budget.
    let netlist = parse(&diode_array(200, "")).unwrap();
    let a = format!("{:?}", run_with_threads(&netlist, 1).unwrap());
    let b = format!("{:?}", run_with_threads(&netlist, 4).unwrap());
    assert!(a == b);
}
