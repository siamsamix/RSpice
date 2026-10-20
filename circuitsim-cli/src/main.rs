use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use circuitsim::{
    run, Circuit, DcSweepPoint, MeasureResult, Netlist, ParseOptions, SimError, SimulationResult,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum AnalysisKind {
    Op,
    Tran,
    Ac,
    Dc,
    Tf,
    Noise,
    Four,
}

#[derive(Parser)]
#[command(
    name = "circuitsim",
    about = "Analog circuit simulator (SPICE-style netlists)"
)]
struct Args {
    /// Netlist file to simulate
    netlist: PathBuf,

    /// Print transient waveform or DC sweep as CSV to stdout
    #[arg(long)]
    csv: bool,

    /// Write the text report (or --csv output) to this file instead of stdout
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Export every transient/AC trace as CSV to this file
    #[arg(long)]
    waves: Option<PathBuf>,

    /// Batch mode: run to completion with only the analysis output
    #[arg(short, long)]
    batch: bool,

    /// Print only one analysis's section
    #[arg(short, long, value_enum)]
    analysis: Option<AnalysisKind>,

    /// Extra model/library search directory (repeatable, before RSPICE_LIB)
    #[arg(short = 'I', long = "include", value_name = "DIR")]
    include: Vec<PathBuf>,

    /// Suppress import warnings
    #[arg(long)]
    no_warnings: bool,
}

fn main() {
    match run_cli() {
        Ok(()) => {}
        Err(CliError::Sim(e)) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        Err(CliError::Io(e)) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

enum CliError {
    Sim(SimError),
    Io(std::io::Error),
}

impl From<SimError> for CliError {
    fn from(e: SimError) -> Self {
        CliError::Sim(e)
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        CliError::Io(e)
    }
}

fn run_cli() -> Result<(), CliError> {
    let args = Args::parse();
    let source = fs::read_to_string(&args.netlist)
        .map_err(|e| SimError::Parse(format!("{}: {e}", args.netlist.display())))?;

    // Library resolution order: the deck's own directory, explicit -I paths,
    // `RSPICE_LIB` (PATH-style), the bundled `libs/` next to the checkout (or
    // the installed executable), and the process directory.
    let mut search_paths: Vec<PathBuf> = args.include.clone();
    if let Ok(paths) = std::env::var("RSPICE_LIB") {
        search_paths.extend(std::env::split_paths(&paths));
    }
    for bundled in bundled_library_dirs() {
        if !search_paths.contains(&bundled) {
            search_paths.push(bundled);
        }
    }
    let options = ParseOptions {
        deck_dir: args.netlist.parent().map(std::path::Path::to_path_buf),
        search_paths,
    };

    let netlist = circuitsim::parse_with_options(&source, &options)?;
    if !args.no_warnings {
        for warning in &netlist.warnings {
            eprintln!("warning: {warning}");
        }
    }

    let result = run(&netlist)?;

    let mut report = String::new();
    if !args.batch {
        print_summary(&mut report, &netlist);
    }
    if let Some(kind) = args.analysis {
        print_selected(&mut report, &netlist, &result, kind)?;
    } else {
        print_all(&mut report, &netlist, &result)?;
    }

    if let Some(path) = &args.waves {
        fs::write(path, waveform_csv(&netlist, &result))?;
    }

    let body = if args.csv {
        csv_body(&netlist, &result)
    } else {
        report
    };
    match &args.output {
        Some(path) => fs::write(path, body)?,
        None => print!("{body}"),
    }
    Ok(())
}

/// Candidate directories for the app's bundled model libraries.
fn bundled_library_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    // A checkout / installed tree: <exe dir>/../libs, <exe dir>/libs.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("libs"));
            if let Some(parent) = dir.parent() {
                out.push(parent.join("libs"));
            }
        }
    }
    // The source checkout this binary was built from.
    out.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libs"));
    out.retain(|p| p.is_dir());
    out
}

fn print_summary(report: &mut String, netlist: &Netlist) {
    let net = &netlist.circuit;
    let _ = writeln!(report, "Circuit: {}", net.name);
    let _ = writeln!(
        report,
        "  {} nodes, {} R, {} C, {} L, {} V, {} I, {} D, {} M, {} switches ({} °C)",
        net.nodes,
        net.resistors.len(),
        net.capacitors.len(),
        net.inductors.len(),
        net.voltage_sources.len(),
        net.current_sources.len(),
        net.diodes.len(),
        net.mosfets.len(),
        net.switches.len(),
        net.temperature
    );
    if !netlist.analysis.steps.is_empty() {
        let targets: Vec<&str> = netlist
            .analysis
            .steps
            .iter()
            .map(|s| s.target.as_str())
            .collect();
        let _ = writeln!(
            report,
            "  .step: {} ({} runs)",
            targets.join(", "),
            netlist.analysis.steps.len()
        );
    }
}

fn print_all(
    report: &mut String,
    netlist: &Netlist,
    result: &SimulationResult,
) -> Result<(), SimError> {
    print_dc(report, netlist, result);
    print_dc_sweep(report, netlist, result);
    print_tran(report, netlist, result);
    print_ac(report, netlist, result);
    print_tf(report, result);
    print_noise(report, result);
    print_four(report, result);
    print_measures(report, result);
    if !result.runs.is_empty() {
        print_runs(report, netlist, result);
    }
    if result.dc.is_none()
        && result.dc_sweep.is_none()
        && result.tran.is_none()
        && result.ac.is_none()
        && result.tf.is_none()
        && result.noise.is_none()
        && result.four.is_none()
        && result.measurements.is_empty()
    {
        let _ = writeln!(report, "\n  (no analysis directive found)");
    }
    Ok(())
}

fn print_selected(
    report: &mut String,
    netlist: &Netlist,
    result: &SimulationResult,
    kind: AnalysisKind,
) -> Result<(), SimError> {
    match kind {
        AnalysisKind::Op => print_dc(report, netlist, result),
        AnalysisKind::Tran => print_tran(report, netlist, result),
        AnalysisKind::Ac => print_ac(report, netlist, result),
        AnalysisKind::Dc => print_dc_sweep(report, netlist, result),
        AnalysisKind::Tf => print_tf(report, result),
        AnalysisKind::Noise => print_noise(report, result),
        AnalysisKind::Four => print_four(report, result),
    }
    print_measures(report, result);
    Ok(())
}

/// Label for node `i`: the deck's name when it has one (`in`, `out`), else its id.
fn node_label(net: &Circuit, i: usize) -> String {
    let name = net.node_name(i);
    if name.is_empty() {
        format!("{i}")
    } else {
        name.to_string()
    }
}

fn print_dc(report: &mut String, netlist: &Netlist, result: &SimulationResult) {
    let net = &netlist.circuit;
    let Some(dc) = &result.dc else { return };
    let _ = writeln!(report, "\n--- DC Operating Point ---");
    for (i, v) in dc.node_voltages.iter().enumerate() {
        let _ = writeln!(report, "  V({}) = {v:.6e} V", node_label(net, i));
    }
    for (src, cur) in net.voltage_sources.iter().zip(&dc.source_currents) {
        let _ = writeln!(report, "  I({}) = {cur:.6e} A", src.name);
    }
    for (cur, mft) in dc.branch_currents.mosfets.iter().zip(&net.mosfets) {
        let _ = writeln!(report, "  I({}) = {cur:.6e} A", mft.name);
    }
    for (cur, cs) in dc.branch_currents.current_sources.iter().zip(&net.current_sources) {
        let _ = writeln!(report, "  I({}) = {cur:.6e} A", cs.name);
    }
    for (cur, b) in dc.branch_currents.behavioral.iter().zip(&net.behavioral) {
        let _ = writeln!(report, "  I({}) = {cur:.6e} A", b.name);
    }
    for (cur, d) in dc.branch_currents.diodes.iter().zip(&net.diodes) {
        let _ = writeln!(report, "  I({}) = {cur:.6e} A", d.name);
    }
    for (cur, q) in dc.branch_currents.bjts.iter().zip(&net.bjts) {
        let _ = writeln!(report, "  Ic({}) = {cur:.6e} A", q.name);
    }
    for (cur, j) in dc.branch_currents.jfets.iter().zip(&net.jfets) {
        let _ = writeln!(report, "  Id({}) = {cur:.6e} A", j.name);
    }
    for (cur, r) in dc.branch_currents.resistors.iter().zip(&net.resistors) {
        let _ = writeln!(report, "  I({}) = {cur:.6e} A", r.name);
    }
}

fn print_dc_sweep(report: &mut String, netlist: &Netlist, result: &SimulationResult) {
    let Some(sweep) = &result.dc_sweep else { return };
    let net = &netlist.circuit;
    let _ = writeln!(report, "\n--- DC Sweep ---");
    let _ = writeln!(
        report,
        "  {} points, targets: {}",
        sweep.points.len(),
        sweep.targets.join(", ")
    );
    let n = sweep
        .points
        .first()
        .map(|p| p.node_voltages.len())
        .unwrap_or(0);
    let mut header = String::new();
    for t in &sweep.targets {
        let _ = write!(header, "{t:>14}");
    }
    for i in 1..n {
        let _ = write!(header, "{:>14}", format!("V({})", node_label(net, i)));
    }
    let _ = writeln!(report, "{header}");
    let show = sweep.points.len().min(20);
    for pt in sweep.points.iter().take(show) {
        print_dc_sweep_row(report, pt);
    }
    if sweep.points.len() > show {
        let _ = writeln!(report, "  ... ({} more points)", sweep.points.len() - show);
        if let Some(pt) = sweep.points.last() {
            print_dc_sweep_row(report, pt);
        }
    }
}

fn print_dc_sweep_row(report: &mut String, pt: &DcSweepPoint) {
    let mut line = String::new();
    for &v in &pt.values {
        let _ = write!(line, "{v:>14.4e}");
    }
    for v in pt.node_voltages.iter().skip(1) {
        let _ = write!(line, "{v:>14.4e}");
    }
    let _ = writeln!(report, "{line}");
}

fn print_tran(report: &mut String, netlist: &Netlist, result: &SimulationResult) {
    let Some(tran) = &result.tran else { return };
    let net = &netlist.circuit;
    let _ = writeln!(report, "\n--- Transient Analysis ---");
    let _ = writeln!(report, "  {} time points", tran.points.len());
    let show = tran.points.len().min(20);
    for pt in tran.points.iter().take(show) {
        let _ = write!(report, "  t = {:.6e}s:", pt.time);
        for (i, v) in pt.node_voltages.iter().enumerate() {
            let _ = write!(report, " V({})={v:.4e}", node_label(net, i));
        }
        let _ = writeln!(report);
    }
    if tran.points.len() > show {
        let _ = writeln!(report, "  ... ({} more points)", tran.points.len() - show);
    }
    if let Some(last) = tran.points.last() {
        let _ = writeln!(
            report,
            "\n  Final: t = {:.6e}s, V({}) = {:.6e} V",
            last.time,
            node_label(net, 1),
            last.node_voltages.get(1).copied().unwrap_or(0.0)
        );
    }
}

fn print_ac(report: &mut String, netlist: &Netlist, result: &SimulationResult) {
    let Some(ac) = &result.ac else { return };
    let net = &netlist.circuit;
    let _ = writeln!(report, "\n--- AC Analysis ---");
    let _ = writeln!(report, "  {} frequency points", ac.points.len());
    let show = ac.points.len().min(20);
    for pt in ac.points.iter().take(show) {
        let _ = write!(report, "  f = {:.6e} Hz:", pt.freq);
        for (i, v) in pt.node_voltages.iter().enumerate().skip(1) {
            let _ = write!(
                report,
                " V({})={:.4e}∠{:.1}°",
                node_label(net, i),
                v.norm(),
                v.arg().to_degrees()
            );
        }
        let _ = writeln!(report);
    }
    if ac.points.len() > show {
        let _ = writeln!(report, "  ... ({} more points)", ac.points.len() - show);
    }
}

fn print_tf(report: &mut String, result: &SimulationResult) {
    let Some(tf) = &result.tf else { return };
    let _ = writeln!(report, "\n--- DC Transfer Function ---");
    let _ = writeln!(report, "  {} / {}", tf.output, tf.input);
    let _ = writeln!(report, "  transfer = {:.6e}", tf.transfer);
    if tf.input_impedance.is_finite() {
        let _ = writeln!(report, "  input impedance  = {:.6e} Ohm", tf.input_impedance);
    } else {
        let _ = writeln!(report, "  input impedance  = infinite");
    }
    if tf.output_impedance.is_finite() {
        let _ = writeln!(
            report,
            "  output impedance = {:.6e} Ohm",
            tf.output_impedance
        );
    } else {
        let _ = writeln!(report, "  output impedance = n/a for a current output");
    }
}

fn print_noise(report: &mut String, result: &SimulationResult) {
    let Some(noise) = &result.noise else { return };
    let _ = writeln!(report, "\n--- Noise Analysis ---");
    let _ = writeln!(report, "  output {}, input {}", noise.output, noise.input);
    let _ = writeln!(report, "  {:>14} {:>16} {:>16}", "freq", "output V/√Hz", "input V/√Hz");
    for p in &noise.points {
        let _ = writeln!(
            report,
            "  {:>14.4e} {:>16.6e} {:>16.6e}",
            p.freq, p.output_density, p.input_density
        );
    }
    let _ = writeln!(
        report,
        "  integrated output RMS = {:.6e} V, input-referred = {:.6e} V",
        noise.total_output_rms, noise.total_input_rms
    );
}

fn print_four(report: &mut String, result: &SimulationResult) {
    let Some(four) = &result.four else { return };
    let _ = writeln!(
        report,
        "\n--- Fourier Analysis at {:.6e} Hz ---",
        four.fundamental
    );
    for out in &four.outputs {
        let _ = writeln!(
            report,
            "  {}: dc = {:.6e}",
            out.name, out.dc
        );
        let _ = writeln!(
            report,
            "  {:>4} {:>14} {:>14} {:>10} {:>10}",
            "n", "frequency", "magnitude", "norm", "phase°"
        );
        for h in &out.harmonics {
            let _ = writeln!(
                report,
                "  {:>4} {:>14.4e} {:>14.6e} {:>10.4} {:>10.2}",
                h.number,
                h.frequency,
                h.magnitude,
                h.normalized_magnitude,
                h.phase_deg
            );
        }
    }
}

fn print_measures(report: &mut String, result: &SimulationResult) {
    if result.measurements.is_empty() {
        return;
    }
    let _ = writeln!(report, "\n--- Measurements ---");
    for MeasureResult { name, value, error } in &result.measurements {
        match (value, error) {
            (Some(v), _) => {
                let _ = writeln!(report, "  {name} = {v:.9e}");
            }
            (None, Some(e)) => {
                let _ = writeln!(report, "  {name} = failed: {e}");
            }
            (None, None) => {
                let _ = writeln!(report, "  {name} = failed");
            }
        }
    }
}

/// One line per `.step` run with the measurements/vp values that identify it.
fn print_runs(report: &mut String, netlist: &Netlist, result: &SimulationResult) {
    let _ = writeln!(report, "\n--- Stepped Runs ---");
    let _ = writeln!(
        report,
        "  {:>3}  {:<24} {}",
        "#",
        result.step_targets.join(", "),
        if let Some(first) = result.runs.first().and_then(|r| r.dc.as_ref()) {
            format!("V({})", node_label(&netlist.circuit, first.node_voltages.len().saturating_sub(1)))
        } else {
            String::new()
        }
    );
    for (i, run) in result.runs.iter().enumerate() {
        let values: Vec<String> = run.values.iter().map(|v| format!("{v:.6e}")).collect();
        let last_v = run
            .dc
            .as_ref()
            .and_then(|dc| dc.node_voltages.last().copied());
        match last_v {
            Some(v) => {
                let _ = writeln!(report, "  {:>3}  {:<24} {v:.6e}", i + 1, values.join(", "));
            }
            None => {
                let _ = writeln!(report, "  {:>3}  {}", i + 1, values.join(", "));
            }
        }
    }
}

/// CSV for `--csv`: transient takes precedence, then the DC sweep.
fn csv_body(netlist: &Netlist, result: &SimulationResult) -> String {
    let mut out = String::new();
    if let Some(tran) = &result.tran {
        let net = &netlist.circuit;
        let n = tran
            .points
            .first()
            .map(|p| p.node_voltages.len())
            .unwrap_or(0);
        out.push_str("time");
        for i in 0..n {
            let _ = write!(out, ",V({})", node_label(net, i));
        }
        out.push('\n');
        for pt in &tran.points {
            let _ = write!(out, "{:.6e}", pt.time);
            for v in &pt.node_voltages {
                let _ = write!(out, ",{v:.6e}");
            }
            out.push('\n');
        }
        return out;
    }
    if let Some(sweep) = &result.dc_sweep {
        let net = &netlist.circuit;
        let n = sweep
            .points
            .first()
            .map(|p| p.node_voltages.len())
            .unwrap_or(0);
        let mut header = sweep.targets.clone();
        for i in 1..n {
            header.push(format!("V({})", node_label(net, i)));
        }
        out.push_str(&header.join(","));
        out.push('\n');
        for pt in &sweep.points {
            let mut cells: Vec<String> = pt.values.iter().map(|v| format!("{v:.6e}")).collect();
            for v in pt.node_voltages.iter().skip(1) {
                cells.push(format!("{v:.6e}"));
            }
            out.push_str(&cells.join(","));
            out.push('\n');
        }
        return out;
    }
    // No waveform result: fall back to a comment so the file is still valid.
    out.push_str("# no .tran or .dc sweep result\n");
    out
}

/// Full export surface: every node voltage and element current, with AC
/// magnitude/phase when that is the only result.
fn waveform_csv(netlist: &Netlist, result: &SimulationResult) -> String {
    let net = &netlist.circuit;
    let mut out = String::new();
    if let Some(tran) = &result.tran {
        out.push_str("time");
        for i in 0..net.nodes {
            let _ = write!(out, ",V({})", node_label(net, i));
        }
        for r in &net.resistors {
            let _ = write!(out, ",I({})", r.name);
        }
        for c in &net.capacitors {
            let _ = write!(out, ",I({})", c.name);
        }
        for l in &net.inductors {
            let _ = write!(out, ",I({})", l.name);
        }
        for v in &net.voltage_sources {
            let _ = write!(out, ",I({})", v.name);
        }
        for i in &net.current_sources {
            let _ = write!(out, ",I({})", i.name);
        }
        for b in &net.behavioral {
            let _ = write!(out, ",I({})", b.name);
        }
        for d in &net.diodes {
            let _ = write!(out, ",I({})", d.name);
        }
        for m in &net.mosfets {
            let _ = write!(out, ",I({})", m.name);
        }
        for j in &net.jfets {
            let _ = write!(out, ",Id({})", j.name);
        }
        for q in &net.bjts {
            let _ = write!(out, ",Ic({})", q.name);
        }
        out.push('\n');
        for pt in &tran.points {
            let _ = write!(out, "{:.9e}", pt.time);
            for v in &pt.node_voltages {
                let _ = write!(out, ",{v:.9e}");
            }
            for v in &pt.branch_currents.resistors {
                let _ = write!(out, ",{v:.9e}");
            }
            for v in &pt.branch_currents.capacitors {
                let _ = write!(out, ",{v:.9e}");
            }
            for v in &pt.branch_currents.inductors {
                let _ = write!(out, ",{v:.9e}");
            }
            for v in &pt.branch_currents.voltage_sources {
                let _ = write!(out, ",{v:.9e}");
            }
            for v in &pt.branch_currents.current_sources {
                let _ = write!(out, ",{v:.9e}");
            }
            for v in &pt.branch_currents.behavioral {
                let _ = write!(out, ",{v:.9e}");
            }
            for v in &pt.branch_currents.diodes {
                let _ = write!(out, ",{v:.9e}");
            }
            for v in &pt.branch_currents.mosfets {
                let _ = write!(out, ",{v:.9e}");
            }
            for v in &pt.branch_currents.jfets {
                let _ = write!(out, ",{v:.9e}");
            }
            for v in &pt.branch_currents.bjts {
                let _ = write!(out, ",{v:.9e}");
            }
            out.push('\n');
        }
        return out;
    }
    if let Some(ac) = &result.ac {
        out.push_str("freq");
        for i in 0..net.nodes {
            let _ = write!(out, ",V({})_mag,V({})_phase", node_label(net, i), node_label(net, i));
        }
        out.push('\n');
        for pt in &ac.points {
            let _ = write!(out, "{:.9e}", pt.freq);
            for v in &pt.node_voltages {
                let _ = write!(out, ",{:.9e},{:.9e}", v.norm(), v.arg().to_degrees());
            }
            out.push('\n');
        }
        return out;
    }
    out.push_str("# no .tran or .ac result\n");
    out
}
