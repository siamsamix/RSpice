use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use circuitsim::fft::Window;
use circuitsim::netlist::OpMode;
use circuitsim::netlist::{McCommand, McOp};
use circuitsim::{
    run, Circuit, DcResult, DcSweepPoint, MeasureResult, Netlist, ParseOptions, RawFlavor,
    SimError, SimulationResult,
};
use clap::{Parser, ValueEnum};

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

/// `--window` choices: LTspice's FFT-view parameter-free windows (clap's
/// kebab-case spellings match the library's `Window::from_label`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum WindowKind {
    Bartlett,
    BartlettHann,
    Blackman,
    BlackmanHarris,
    BlackmanNuttal,
    Bohman,
    Cosine,
    FlatTop,
    Hamming,
    Hann,
    Lanczos,
    Parzen,
    Welch,
}

impl From<WindowKind> for Window {
    fn from(kind: WindowKind) -> Window {
        match kind {
            WindowKind::Bartlett => Window::Bartlett,
            WindowKind::BartlettHann => Window::BartlettHann,
            WindowKind::Blackman => Window::Blackman,
            WindowKind::BlackmanHarris => Window::BlackmanHarris,
            WindowKind::BlackmanNuttal => Window::BlackmanNuttal,
            WindowKind::Bohman => Window::Bohman,
            WindowKind::Cosine => Window::Cosine,
            WindowKind::FlatTop => Window::FlatTop,
            WindowKind::Hamming => Window::Hamming,
            WindowKind::Hann => Window::Hann,
            WindowKind::Lanczos => Window::Lanczos,
            WindowKind::Parzen => Window::Parzen,
            WindowKind::Welch => Window::Welch,
        }
    }
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

    /// Export every analysis's plot as a sectioned CSV to this file (one
    /// `# <Plotname>` section per raw plot, plus the `.meas` table)
    #[arg(long)]
    waves: Option<PathBuf>,

    /// Render every analysis's plot as a stacked SVG image to this file
    /// (one panel per raw plot, drawn top to bottom)
    #[arg(long)]
    image: Option<PathBuf>,

    /// Comma list of trace names for --waves/--image/--fft (case-insensitive
    /// full-column names); unmatched names are silent, the axis and the
    /// step columns always stay
    #[arg(long, value_name = "TRACES")]
    traces: Option<String>,

    /// Attach a crosshair cursor per --image panel (up to two per panel,
    /// like LTspice): requests within the x span snap to the nearest
    /// sample of the panel's first kept trace and read out the snapped
    /// coordinates, two cursors also read out their deltas, and a time
    /// delta additionally reads out as the frequency 1/Δt; SPICE suffix
    /// scales parse (`1.2us`, `4.7Meg`)
    #[arg(long, requires = "image", value_name = "X")]
    cursor: Vec<String>,

    /// Render the FFT spectrum of every matched transient trace as an SVG
    /// image to this file (one panel per trace, drawn top to bottom)
    #[arg(long)]
    fft: Option<PathBuf>,

    /// Write the PSpice performance analysis as an SVG to this file: one
    /// panel per `.meas` name, the measurement value against the swept
    /// parameter across the `.step` runs
    #[arg(long)]
    perf: Option<PathBuf>,

    /// FFT window function, one of LTspice's FFT-view windows (each taper
    /// normalized to unit mean, so a DC level reads its true amplitude)
    #[arg(long, requires = "fft", value_enum, default_value_t = WindowKind::BlackmanHarris)]
    window: WindowKind,

    /// Write the simulation's plots as an LTspice-style .raw file
    #[arg(long, value_name = "FILE")]
    raw: Option<PathBuf>,

    /// Write the --raw file in the text flavor instead of binary
    #[arg(long, requires = "raw")]
    raw_ascii: bool,

    /// Treat the positional file as a .raw file: read it and dump its plots
    /// instead of simulating
    #[arg(long)]
    read_raw: bool,

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
    if args.read_raw {
        let bytes = fs::read(&args.netlist)?;
        let raw = circuitsim::rawwave::read(&bytes)?;
        let mut report = String::new();
        print_raw(&mut report, &raw);
        print!("{report}");
        return Ok(());
    }
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

    // The cursor requests ride as SPICE-scaled initials; the clap
    // `requires` already pinned --image when any was asked.
    let mut cursor_xs = Vec::new();
    for token in &args.cursor {
        let value = circuitsim::units::parse_value(token)
            .map_err(|e| SimError::Parse(format!("--cursor {token}: {e}")))?;
        cursor_xs.push(value);
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
    // `.mc` decks are their own run driver: whichever table was printed, the
    // statistics of the runs the driver produced go on top.
    if result.step_targets.first().map(String::as_str) == Some("mc") {
        if let Some(card) = &netlist.analysis.mc {
            print_mc(&mut report, &result, card);
        }
    }

    if let Some(path) = &args.raw {
        let flavor = if args.raw_ascii {
            RawFlavor::Ascii
        } else {
            RawFlavor::Binary
        };
        let title = format!("* {}", args.netlist.display());
        fs::write(
            path,
            circuitsim::rawwave::to_bytes(
                &circuitsim::rawwave::build(&title, &netlist, &result),
                flavor,
            ),
        )?;
    }

    if let Some(path) = &args.waves {
        let title = format!("* {}", args.netlist.display());
        let raw = circuitsim::rawwave::build(&title, &netlist, &result);
        let csv = circuitsim::export::csv_document(
            &raw,
            &result.measurements,
            &trace_specs(args.traces.as_deref()),
        );
        fs::write(path, csv)?;
    }

    if let Some(path) = &args.image {
        let title = format!("* {}", args.netlist.display());
        let raw = circuitsim::rawwave::build(&title, &netlist, &result);
        let svg = circuitsim::export::svg_document_with_cursors(
            &raw,
            &trace_specs(args.traces.as_deref()),
            &cursor_xs,
        );
        fs::write(path, svg)?;
    }

    if let Some(path) = &args.perf {
        let svg = circuitsim::export::svg_performance(&result)?;
        fs::write(path, svg)?;
    }

    if let Some(path) = &args.fft {
        let title = format!("* {}", args.netlist.display());
        let raw = circuitsim::rawwave::build(&title, &netlist, &result);
        let spectra = circuitsim::fft::spectra(
            &raw,
            &trace_specs(args.traces.as_deref()),
            args.window.into(),
        );
        fs::write(path, circuitsim::export::svg_document(&spectra, &[]))?;
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
    if let Some(mc) = &netlist.analysis.mc {
        let seed_note = match mc.seed {
            Some(n) => format!("seed {n}"),
            None => "deck seed".to_string(),
        };
        let _ = writeln!(report, "  .mc: {} runs ({seed_note})", mc.runs);
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
    for (cur, cs) in dc
        .branch_currents
        .current_sources
        .iter()
        .zip(&net.current_sources)
    {
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
    for (cur, l) in dc.branch_currents.tlines.iter().zip(&net.tlines) {
        let _ = writeln!(report, "  I({}) = {cur:.6e} A", l.name);
    }
    for (cur, r) in dc.branch_currents.resistors.iter().zip(&net.resistors) {
        let _ = writeln!(report, "  I({}) = {cur:.6e} A", r.name);
    }
    // `.op all` (PSpice variant): the annotated device operating points on
    // top of the node/bias table — `.op pon` stays the plain bias point.
    if netlist.analysis.op_mode == OpMode::All {
        print_device_op_points(report, net, dc);
    }
}

/// The `.op all` device table: card-order operating parameters, one line per
/// element (`name key=value …`).
fn print_device_op_points(report: &mut String, net: &Circuit, dc: &DcResult) {
    let points = circuitsim::analysis::device_op_points(
        net,
        &dc.node_voltages,
        &dc.branch_currents,
        &dc.source_currents,
    );
    if points.is_empty() {
        return;
    }
    let _ = writeln!(report, "\n--- Device Operating Points ---");
    for point in &points {
        let detail: Vec<String> = point
            .entries
            .iter()
            .map(|(k, v)| format!("{k}={v:.6e}"))
            .collect();
        let _ = writeln!(report, "  {:<10} {}", point.name, detail.join("  "));
    }
}

fn print_dc_sweep(report: &mut String, netlist: &Netlist, result: &SimulationResult) {
    let Some(sweep) = &result.dc_sweep else {
        return;
    };
    let net = &netlist.circuit;
    let _ = writeln!(report, "\n--- DC Sweep ---");
    let _ = writeln!(
        report,
        "  {} points, targets: {}",
        sweep.points.len(),
        sweep.targets.join(", ")
    );
    // Node voltages are indexed by node id — the deck's node table is the
    // count (post-compaction the arrays are `circuit.nodes` long).
    let n = net.nodes;
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
        let _ = writeln!(
            report,
            "  input impedance  = {:.6e} Ohm",
            tf.input_impedance
        );
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
    let _ = writeln!(
        report,
        "  {:>14} {:>16} {:>16}",
        "freq", "output V/√Hz", "input V/√Hz"
    );
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
        let _ = writeln!(report, "  {}: dc = {:.6e}", out.name, out.dc);
        let _ = writeln!(
            report,
            "  {:>4} {:>14} {:>14} {:>10} {:>10}",
            "n", "frequency", "magnitude", "norm", "phase°"
        );
        for h in &out.harmonics {
            let _ = writeln!(
                report,
                "  {:>4} {:>14.4e} {:>14.6e} {:>10.4} {:>10.2}",
                h.number, h.frequency, h.magnitude, h.normalized_magnitude, h.phase_deg
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

/// The `.mc` statistics: per-measurement spreads, the optional histogram, and
/// the optional yield criterion — exactly what the card asked for.
fn print_mc(report: &mut String, result: &SimulationResult, card: &McCommand) {
    let rep = circuitsim::mc::summarize(result, card);
    if rep.stats.is_empty() {
        return;
    }
    let _ = writeln!(report, "\n--- Monte Carlo ---");
    for stat in &rep.stats {
        let _ = writeln!(
            report,
            "  {:<16} n{:>4}  min={:.6e}  mean={:.6e}  sigma={:.3e}  max={:.6e}",
            stat.name, stat.n, stat.min, stat.mean, stat.sigma, stat.max
        );
    }
    if let Some(hist) = &rep.histogram {
        let peak = hist.counts.iter().max().copied().unwrap_or(1).max(1);
        let samples: usize = hist.counts.iter().sum();
        let _ = writeln!(
            report,
            "  hist {} on [{:.6e}, {:.6e}], {} samples:",
            hist.name, hist.lo, hist.hi, samples
        );
        let width = (hist.hi - hist.lo) / hist.bins.max(1) as f64;
        for (i, count) in hist.counts.iter().enumerate() {
            let lo = hist.lo + i as f64 * width;
            let bar = "#".repeat((*count as f64 / peak as f64 * 32.0).ceil() as usize);
            let _ = writeln!(
                report,
                "   [{:10.4e},{:10.4e}) {:<32} {count}",
                lo,
                lo + width,
                bar
            );
        }
    }
    if let Some(y) = &rep.criterion {
        if y.total > 0 {
            let compare = match y.op {
                McOp::Below => "<",
                McOp::Above => ">",
            };
            let _ = writeln!(
                report,
                "  yield {}{}{}: {}/{} = {:.1}%",
                y.name,
                compare,
                y.limit,
                y.passes,
                y.total,
                100.0 * y.passes as f64 / y.total as f64
            );
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
        if result.runs.first().is_some_and(|r| r.dc.is_some()) {
            format!(
                "V({})",
                node_label(&netlist.circuit, netlist.circuit.nodes.saturating_sub(1))
            )
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

/// The `--traces` argument: comma names, trimmed; blank items (adjacent
/// commas, a trailing comma) drop out so they never match anything.
fn trace_specs(spec: Option<&str>) -> Vec<String> {
    spec.unwrap_or_default()
        .split(',')
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

/// CSV for `--csv`: transient takes precedence, then the DC sweep.
fn csv_body(netlist: &Netlist, result: &SimulationResult) -> String {
    let mut out = String::new();
    if let Some(tran) = &result.tran {
        let net = &netlist.circuit;
        let n = net.nodes;
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
        let n = net.nodes;
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

/// The `--read-raw` dump: one block per plot in file order — the header
/// words and variables, then every row spelled `axis-cell = value` with the
/// complex plots carrying magnitude@phase (real plots print the bare value).
fn print_raw(report: &mut String, raw: &circuitsim::RawFile) {
    for (pi, ds) in raw.datasets.iter().enumerate() {
        let _ = writeln!(report, "\n--- .raw plot {}: {} ---", pi, ds.plotname);
        let _ = writeln!(
            report,
            "  {} points over {} variables (flags: {})",
            ds.values.len(),
            ds.variables.len(),
            if ds.flags.is_empty() {
                "none".to_string()
            } else {
                ds.flags.join(" ")
            }
        );
        let _ = writeln!(
            report,
            "  variables: {}",
            ds.variables
                .iter()
                .map(|v| format!("{}({})", v.name, v.kind))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for row in &ds.values {
            let cells = ds
                .variables
                .iter()
                .zip(row.iter())
                .map(|(var, v)| {
                    if ds.flags.iter().any(|f| f.eq_ignore_ascii_case("complex")) {
                        format!(
                            "{}={:.6e}@{:.4}deg",
                            var.name,
                            v.norm(),
                            v.arg().to_degrees()
                        )
                    } else {
                        format!("{}={:.6e}", var.name, v.re)
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            let _ = writeln!(report, "  {cells}");
        }
    }
}
