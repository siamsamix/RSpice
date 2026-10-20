//! Benchmark harness for RSpice's "advanced and fast" track.
//!
//! Runs a canonical circuit suite and reports wall time plus a throughput
//! figure (node·timesteps per second). Accuracy is checked against analytic
//! references — "fast" must be measured, never asserted. Pass `--json` for a
//! machine-readable line per case.

use circuitsim::{parse, run};
use std::time::Instant;

struct Case {
    name: &'static str,
    deck: String,
}

fn rc_ladder(nodes: usize) -> String {
    let mut deck = String::from("* RC ladder benchmark\nV1 in 0 DC 1\n");
    let mut prev = "in".to_string();
    for i in 0..nodes {
        let node = format!("n{i}");
        deck.push_str(&format!("R{i} {prev} {node} 1k\n"));
        deck.push_str(&format!("C{i} {node} 0 1n\n"));
        prev = node;
    }
    deck.push_str(&format!(".tran 1u {}u\n.end\n", 200));
    deck
}

fn rc_step() -> String {
    "* RC step benchmark\n\
     V1 in 0 PULSE(0 1 0 1n 1n 1 2)\n\
     R1 in out 1k\n\
     C1 out 0 1u\n\
     .tran 10u 5m\n\
     .end\n"
        .to_string()
}

fn diode_rectifier() -> String {
    "* Half-wave rectifier benchmark\n\
     V1 in 0 SIN(0 5 1k)\n\
     D1 in out DMOD\n\
     C1 out 0 10u\n\
     R1 out 0 1k\n\
     .model DMOD D(IS=2.68e-9 N=1.84 CJO=4p VJ=0.75 M=0.33)\n\
     .tran 5u 3m\n\
     .end\n"
        .to_string()
}

fn main() {
    let json = std::env::args().any(|a| a == "--json");
    let cases = vec![
        Case { name: "rc-step", deck: rc_step() },
        Case { name: "rc-ladder-50", deck: rc_ladder(50) },
        Case { name: "rc-ladder-200", deck: rc_ladder(200) },
        Case { name: "diode-rectifier", deck: diode_rectifier() },
    ];

    if !json {
        println!(
            "{:<16} {:>8} {:>8} {:>12} {:>14} {:>12}",
            "case", "nodes", "steps", "wall ms", "node·steps/s", "accuracy"
        );
    }
    for case in cases {
        let parse_start = Instant::now();
        let netlist = match parse(&case.deck) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("{}: parse failed: {e}", case.name);
                std::process::exit(1);
            }
        };
        let parse_ms = parse_start.elapsed().as_secs_f64() * 1e3;

        let start = Instant::now();
        let result = match run(&netlist) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{}: simulation failed: {e}", case.name);
                std::process::exit(1);
            }
        };
        let wall = start.elapsed().as_secs_f64();
        let steps = result.tran.as_ref().map(|t| t.points.len()).unwrap_or(0);
        let nodes = netlist.circuit.nodes;
        let throughput = if wall > 0.0 {
            nodes as f64 * steps as f64 / wall
        } else {
            f64::INFINITY
        };

        let accuracy = match case.name {
            "rc-step" => {
                let out = netlist
                    .circuit
                    .node_names
                    .iter()
                    .position(|n| n == "out")
                    .unwrap();
                let last = result.tran.as_ref().unwrap().points.last().unwrap();
                let analytic = 1.0 - (-5.0f64).exp();
                format!("{:.2e}", (last.node_voltages[out] - analytic).abs())
            }
            _ => "—".to_string(),
        };

        if json {
            println!(
                "{{\"case\":\"{}\",\"nodes\":{},\"steps\":{},\"parse_ms\":{:.3},\"wall_ms\":{:.3},\"throughput\":{:.1},\"abs_error\":\"{}\"}}",
                case.name,
                nodes,
                steps,
                parse_ms,
                wall * 1e3,
                throughput,
                accuracy
            );
        } else {
            println!(
                "{:<16} {:>8} {:>8} {:>12.2} {:>14.0} {:>12}",
                case.name,
                nodes,
                steps,
                wall * 1e3,
                throughput,
                accuracy
            );
        }
    }
}
