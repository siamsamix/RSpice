//! Benchmark harness for RSpice's "advanced and fast" track.
//!
//! Runs a canonical circuit suite and reports wall time plus a throughput
//! figure (node·timesteps per second). Accuracy is checked against analytic
//! references — "fast" must be measured, never asserted. Pass `--json` for a
//! machine-readable line per case.
//!
//! LTspice comparison (TODO §11): `--write-decks DIR` writes every case deck
//! as `DIR/<case>.cir`; run each in LTspice (it leaves `<case>.raw` beside
//! the deck), then `--reference-dir DIR` compares RSpice's traces against
//! those files element for element and adds the worst `max|err|/peak` to the
//! report (`--tol X` flags cases past it, default 1e-3; any failure exits 1).
//! Cases without a `.raw` simply report `—`.

use circuitsim::{compare_raw, parse, rawwave, run};
use std::path::PathBuf;
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

/// `--name VALUE` from the command line.
fn flag_value(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let json = std::env::args().any(|a| a == "--json");
    let write_decks = flag_value("--write-decks").map(PathBuf::from);
    let reference_dir = flag_value("--reference-dir").map(PathBuf::from);
    let tol: f64 = flag_value("--tol").and_then(|t| t.parse().ok()).unwrap_or(1e-3);
    let mut failed = false;
    let cases = vec![
        Case { name: "rc-step", deck: rc_step() },
        Case { name: "rc-ladder-50", deck: rc_ladder(50) },
        Case { name: "rc-ladder-200", deck: rc_ladder(200) },
        Case { name: "diode-rectifier", deck: diode_rectifier() },
    ];

    if let Some(dir) = &write_decks {
        std::fs::create_dir_all(dir).expect("create deck directory");
        for case in &cases {
            let path = dir.join(format!("{}.cir", case.name));
            std::fs::write(&path, &case.deck).expect("write deck");
            println!("wrote {}", path.display());
        }
        return;
    }

    if !json {
        println!(
            "{:<16} {:>8} {:>8} {:>12} {:>14} {:>12} {:>12}",
            "case", "nodes", "steps", "wall ms", "node·steps/s", "accuracy", "vs LTspice"
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

        // Worst max|err|/peak against LTspice's own `<case>.raw`, when present.
        let reference = reference_dir
            .as_ref()
            .map(|dir| dir.join(format!("{}.raw", case.name)))
            .filter(|path| path.is_file())
            .map(|path| {
                let bytes = std::fs::read(&path).expect("read reference .raw");
                let reference = rawwave::read(&bytes).unwrap_or_else(|e| {
                    eprintln!("{}: {}: {e}", case.name, path.display());
                    std::process::exit(1);
                });
                let ours = rawwave::build(case.name, &netlist, &result);
                compare_raw(&reference, &ours)
            });
        let vs_ltspice = match &reference {
            Some(cmp) => {
                if !cmp.is_within(tol) {
                    failed = true;
                    eprint!("{} vs LTspice:\n{}", case.name, cmp.report(tol));
                }
                format!("{:.2e}", cmp.worst_norm())
            }
            None => "—".to_string(),
        };

        if json {
            println!(
                "{{\"case\":\"{}\",\"nodes\":{},\"steps\":{},\"parse_ms\":{:.3},\"wall_ms\":{:.3},\"throughput\":{:.1},\"abs_error\":\"{}\",\"ltspice_max_norm\":\"{}\"}}",
                case.name,
                nodes,
                steps,
                parse_ms,
                wall * 1e3,
                throughput,
                accuracy,
                vs_ltspice
            );
        } else {
            println!(
                "{:<16} {:>8} {:>8} {:>12.2} {:>14.0} {:>12} {:>12}",
                case.name,
                nodes,
                steps,
                wall * 1e3,
                throughput,
                accuracy,
                vs_ltspice
            );
        }
    }
    if failed {
        std::process::exit(1);
    }
}
