//! TODO §3 multithreading: threads inside one run must change wall time only.
//!
//! Every test builds a deck big enough to cross the parallel gates, runs it at
//! several thread counts, and compares the full `Debug` dump of the result —
//! bit-for-bit, not within a tolerance — plus checks that the multi-worker
//! path really ran (so a gate regression cannot make these pass vacuously).

use circuitsim::parallel::parallel_regions;
use circuitsim::{parse, run_with_threads};

/// `count` independent stages of each transistor family on one supply: enough
/// devices per family to clear the 128-per-worker gate with room to spare.
fn device_array_deck(count: usize, analyses: &str) -> String {
    let mut deck = String::new();
    deck.push_str("Vcc vcc 0 12 AC 1\n");
    deck.push_str(".model qn NPN(IS=1e-14 BF=100 VAF=80 CJE=1p CJC=0.5p TF=0.3n)\n");
    deck.push_str(".model nj NJF(VTO=-2 BETA=1m LAMBDA=0.01 CGS=2p CGD=1p)\n");
    deck.push_str(".model nm NMOS(VTO=1 KP=2e-4 LAMBDA=0.02 CGSO=1n CGDO=1n)\n");
    deck.push_str(".model dd D(IS=1e-14 N=1.05 CJO=1p)\n");
    for i in 0..count {
        let vary = 1.0 + (i % 7) as f64 * 0.03;
        // BJT common emitter, base biased from the rail, AC-coupled input.
        deck.push_str(&format!("Rb{i} vcc b{i} {}k\n", 820.0 * vary));
        deck.push_str(&format!("Rc{i} vcc c{i} {}k\n", 4.7 * vary));
        deck.push_str(&format!("Q{i} c{i} b{i} 0 qn\n"));
        deck.push_str(&format!("Cin{i} vcc b{i} 100n\n"));
        // JFET self-biased source follower.
        deck.push_str(&format!("Rj{i} vcc jd{i} {}k\n", 3.3 * vary));
        deck.push_str(&format!("J{i} jd{i} 0 js{i} nj\n"));
        deck.push_str(&format!("Rs{i} js{i} 0 {}k\n", 1.0 * vary));
        // MOSFET with a resistive gate divider and a clamp diode.
        deck.push_str(&format!("Rg1{i} vcc g{i} {}k\n", 100.0 * vary));
        deck.push_str(&format!("Rg2{i} g{i} 0 {}k\n", 47.0 * vary));
        deck.push_str(&format!("Rm{i} vcc d{i} {}k\n", 6.8 * vary));
        deck.push_str(&format!("M{i} d{i} g{i} 0 0 nm W=20u L=2u\n"));
        deck.push_str(&format!("D{i} d{i} vcc dd\n"));
    }
    deck.push_str(analyses);
    deck.push('\n');
    deck
}

fn dump(deck: &str, threads: usize) -> (String, u64) {
    let netlist = parse(deck).expect("deck parses");
    let before = parallel_regions();
    let result = run_with_threads(&netlist, threads).expect("deck runs");
    (format!("{result:?}"), parallel_regions() - before)
}

#[test]
fn device_array_op_is_bit_identical_for_any_thread_count() {
    let deck = device_array_deck(300, ".op");
    let (serial, regions1) = dump(&deck, 1);
    assert_eq!(regions1, 0, "one thread must never start a parallel region");
    for threads in [2, 3, 8] {
        let (parallel, regions) = dump(&deck, threads);
        assert!(regions > 0, "{threads} threads: the device passes never went parallel");
        assert!(parallel == serial, "{threads} threads changed the operating point");
    }
}

#[test]
fn device_array_transient_is_bit_identical_for_any_thread_count() {
    let deck = device_array_deck(260, ".tran 20u 120u");
    let (serial, _) = dump(&deck, 1);
    for threads in [2, 4] {
        let (parallel, regions) = dump(&deck, threads);
        assert!(regions > 0, "{threads} threads: the device passes never went parallel");
        assert!(parallel == serial, "{threads} threads changed the transient");
    }
}

#[test]
fn ac_sweep_is_bit_identical_for_any_thread_count() {
    // A small deck with many points (the tiny-system gate wants 256 per
    // worker) so the AC point loop itself splits.
    let deck = "V1 in 0 0 AC 1\nR1 in a 1k\nC1 a 0 1n\nR2 a out 2k\nC2 out 0 470p\n\
                .ac lin 2400 1k 5Meg\n";
    let (serial, regions1) = dump(deck, 1);
    assert_eq!(regions1, 0);
    for threads in [2, 3, 8] {
        let (parallel, regions) = dump(deck, threads);
        assert!(regions > 0, "{threads} threads: the AC sweep never went parallel");
        assert!(parallel == serial, "{threads} threads changed the AC sweep");
    }
}

#[test]
fn nonlinear_ac_sweep_is_bit_identical_for_any_thread_count() {
    let deck = device_array_deck(40, ".ac dec 6 100 10Meg");
    let (serial, _) = dump(&deck, 1);
    for threads in [2, 5] {
        let (parallel, regions) = dump(&deck, threads);
        assert!(regions > 0, "{threads} threads: the AC sweep never went parallel");
        assert!(parallel == serial, "{threads} threads changed the AC sweep");
    }
}

#[test]
fn noise_sweep_is_bit_identical_for_any_thread_count() {
    let deck = "V1 in 0 0 AC 1\nR1 in out 10k\nR2 out 0 10k\nC1 out 0 1n\n\
                .noise V(out) V1 lin 2400 10 1Meg\n";
    let (serial, regions1) = dump(deck, 1);
    assert_eq!(regions1, 0);
    for threads in [2, 4] {
        let (parallel, regions) = dump(deck, threads);
        assert!(regions > 0, "{threads} threads: the noise sweep never went parallel");
        assert!(parallel == serial, "{threads} threads changed the noise sweep");
    }
}

#[test]
fn a_failing_ac_point_reports_the_same_error_at_any_thread_count() {
    // Start frequency 0 is rejected up front, before any worker starts — the
    // error must not depend on the thread budget.
    let deck = "V1 in 0 0 AC 1\nR1 in 0 1k\n.ac lin 2400 0 1Meg\n";
    let netlist = parse(deck);
    if let Ok(netlist) = netlist {
        let e1 = run_with_threads(&netlist, 1).unwrap_err().to_string();
        let e4 = run_with_threads(&netlist, 4).unwrap_err().to_string();
        assert_eq!(e1, e4);
    }
}
