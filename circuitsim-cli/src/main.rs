use std::fs;
use std::path::PathBuf;

use clap::Parser;
use circuitsim::{parse, run, SimulationResult};

#[derive(Parser)]
#[command(name = "circuitsim", about = "Analog circuit simulator (SPICE-style netlists)")]
struct Args {
    /// Netlist file to simulate
    netlist: PathBuf,

    /// Print transient waveform as CSV to stdout
    #[arg(long)]
    csv: bool,
}

fn main() {
    if let Err(e) = run_cli() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run_cli() -> circuitsim::Result<()> {
    let args = Args::parse();
    let source = fs::read_to_string(&args.netlist)
        .map_err(|e| circuitsim::SimError::Parse(format!("{}: {e}", args.netlist.display())))?;

    let netlist = parse(&source)?;
    println!("Circuit: {}", netlist.circuit.name);
    println!(
        "  {} nodes, {} R, {} C, {} L, {} V",
        netlist.circuit.nodes,
        netlist.circuit.resistors.len(),
        netlist.circuit.capacitors.len(),
        netlist.circuit.inductors.len(),
        netlist.circuit.voltage_sources.len()
    );

    let result = run(&netlist.circuit, &netlist.analysis)?;
    print_results(&result, args.csv);
    Ok(())
}

fn print_results(result: &SimulationResult, csv: bool) {
    if let Some(dc) = &result.dc {
        println!("\n--- DC Operating Point ---");
        for (i, v) in dc.node_voltages.iter().enumerate() {
            println!("  V({i}) = {v:.6e} V");
        }
        for (i, src) in dc.source_currents.iter().enumerate() {
            println!("  I(V{i}) = {src:.6e} A");
        }
    }

    if let Some(tran) = &result.tran {
        if csv {
            let n = tran
                .points
                .first()
                .map(|p| p.node_voltages.len())
                .unwrap_or(0);
            print!("time");
            for i in 0..n {
                print!(",V({i})");
            }
            println!();
            for pt in &tran.points {
                print!("{:.6e}", pt.time);
                for v in &pt.node_voltages {
                    print!(",{v:.6e}");
                }
                println!();
            }
        } else {
            println!("\n--- Transient Analysis ---");
            println!("  {} time points", tran.points.len());
            let show = tran.points.len().min(20);
            for pt in tran.points.iter().take(show) {
                print!("  t = {:.6e}s:", pt.time);
                for (i, v) in pt.node_voltages.iter().enumerate() {
                    print!(" V({i})={v:.4e}");
                }
                println!();
            }
            if tran.points.len() > show {
                println!("  ... ({} more points)", tran.points.len() - show);
            }
            let last = tran.points.last().unwrap();
            println!(
                "\n  Final: t = {:.6e}s, V(1) = {:.6e} V",
                last.time, last.node_voltages.get(1).copied().unwrap_or(0.0)
            );
        }
    }
}
