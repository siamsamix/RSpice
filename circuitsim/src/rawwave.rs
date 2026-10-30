//! LTspice-style `.raw` waveform file codec (TODO §6): one builder from
//! [`SimulationResult`] plus a reader/writer for both flavors of the format.
//!
//! The format facts themselves are verified against public readers
//! (spicelib/pyltspice documentation and the mcltspice parser): a text
//! preamble of `Title:/Date:/Plotname:/Flags:/No. Variables:/No. Points:`
//! lines, a tab-separated `Variables:` list (`index<tab>name<tab>kind`) with
//! kinds spelled `time`/`frequency`/`voltage`/`device_current`, and either a
//! `Binary:` payload (real: a point-major 8-byte double axis followed by
//! 4-byte values; complex: 8-byte (re, im) pairs throughout) or an ASCII
//! `Values:` payload (one value per line, a variable-0 line starts the next
//! point, complex values comma-paired). Stepped runs concatenate self-
//! headered plots (`Flags:` gains `stepped`); a `FastAccess` flag switches
//! the binary payload to variable-major blocks. Binary-flavor headers are
//! UTF-16-LE in LTspice; ASCII-flavor headers are plain bytes. The reader
//! also understands the all-double real layout ngspice-family writers emit,
//! taken when the byte count rules the mixed one out (when both divisibilities
//! hold the mixed reading wins, matching LTspice). The writer follows the
//! reference layout throughout. `.tf`/`.noise` plots are TODO §6 work.
//!
//! Two writer choices are RSpice interpretations because no public reader
//! pins them: step values travel as extra constant variables named after
//! their step target (a self-describing file, instead of inventing a `.log`
//! sidecar for the step↔plot correspondence), and an operating point keeps a
//! synthetic `time = 0` axis so every real plot keeps the axis-first binary
//! layout. Bit-for-bit LTspice parity is not the goal — reading its files and
//! writing files its readers accept is.

use crate::analysis::{AcResult, DcResult, DcSweepResult, SimulationResult, TranResult};
use crate::circuit::{Circuit, CURRENT_FAMILIES};
use crate::error::{Result, SimError};
use crate::mna::BranchCurrents;
use crate::netlist::{AcScale, Netlist};
use num_complex::Complex64;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

/// One trace of a raw plot: its spelled name (`V(a)`, `I(T1)`, `time`) and
/// the reference files' kind word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawVariable {
    pub name: String,
    pub kind: String,
}

/// One plot: the analysis dataset a `.raw` reader would show, values as
/// `points × variables` rows (complex always; real plots keep the imaginary
/// part at zero). Variable 0 is the axis the plot runs along.
#[derive(Debug, Clone)]
pub struct RawDataset {
    /// The `Title:` text as the writer emitted it.
    pub title: String,
    /// The `Plotname:` spelling (`Transient Analysis`, `AC Analysis`, …).
    pub plotname: String,
    /// Normalised `Flags:` words (`real`, `forward`, `stepped`, …).
    pub flags: Vec<String>,
    pub variables: Vec<RawVariable>,
    pub values: Vec<Vec<Complex64>>,
}

impl RawDataset {
    /// The axis column (variable 0, by format contract).
    pub fn axis(&self) -> Vec<f64> {
        self.values.iter().map(|row| row[0].re).collect()
    }

    /// Row of one variable by name (case-insensitive, like every reader).
    pub fn trace(&self, name: &str) -> Option<Vec<Complex64>> {
        let index = self
            .variables
            .iter()
            .position(|v| v.name.eq_ignore_ascii_case(name))?;
        Some(self.values.iter().map(|row| row[index]).collect())
    }
}

/// A whole `.raw` file: every plot it stores, in file order.
#[derive(Debug, Clone, Default)]
pub struct RawFile {
    pub datasets: Vec<RawDataset>,
}

/// Output flavor of a `.raw` write: LTspice-style binary (UTF-16-LE header,
/// f64 axis + f32 values for real plots, f64 pairs for complex plots) or the
/// ngspice-compatible ASCII listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawFlavor {
    Binary,
    Ascii,
}

/// Build the `.raw` plots for a simulation: one plot per analysis, per step
/// run when the deck stepped. The title travels into every `Title:` line.
pub fn build(title: &str, netlist: &Netlist, result: &SimulationResult) -> RawFile {
    let net = &netlist.circuit;
    let ac_log = netlist
        .analysis
        .ac
        .as_ref()
        .map(|ac| ac.scale != AcScale::Lin)
        .unwrap_or(false);
    // Steps of the run at hand: unstepped decks carry none even if the
    // run-vector shape repeats the target names.
    let steps = |values: &[f64]| -> Vec<(String, f64)> {
        if result.runs.is_empty() {
            Vec::new()
        } else {
            result
                .step_targets
                .iter()
                .zip(values)
                .map(|(target, v)| (target.clone(), *v))
                .collect()
        }
    };
    let mut datasets = Vec::new();
    let mut sources: Vec<_> = result
        .runs
        .iter()
        .map(|run| {
            (
                steps(&run.values),
                View {
                    dc: run.dc.as_ref(),
                    dc_sweep: run.dc_sweep.as_ref(),
                    tran: run.tran.as_ref(),
                    ac: run.ac.as_ref(),
                },
            )
        })
        .collect();
    if sources.is_empty() {
        sources.push((
            Vec::new(),
            View {
                dc: result.dc.as_ref(),
                dc_sweep: result.dc_sweep.as_ref(),
                tran: result.tran.as_ref(),
                ac: result.ac.as_ref(),
            },
        ));
    }
    for (step_values, view) in &sources {
        let step_values = step_values.as_slice();
        if let Some(dc) = view.dc {
            datasets.push(op_dataset(title, net, dc, step_values));
        }
        if let Some(sweep) = view.dc_sweep {
            datasets.push(sweep_dataset(title, net, sweep, step_values));
        }
        if let Some(tran) = view.tran {
            datasets.push(tran_dataset(title, net, tran, step_values));
        }
        if let Some(ac) = view.ac {
            datasets.push(ac_dataset(title, net, ac, ac_log, step_values));
        }
    }
    RawFile { datasets }
}

/// The analysis plots of one run, borrowing both the top-level result and the
/// per-run step copies through one accessor shape.
struct View<'a> {
    dc: Option<&'a DcResult>,
    dc_sweep: Option<&'a DcSweepResult>,
    tran: Option<&'a TranResult>,
    ac: Option<&'a AcResult>,
}

/// Serialise a raw file in the given flavor, ready for `fs::write`.
pub fn to_bytes(file: &RawFile, flavor: RawFlavor) -> Vec<u8> {
    match flavor {
        RawFlavor::Binary => to_binary(file),
        RawFlavor::Ascii => to_ascii(file).into_bytes(),
    }
}

// ─── Variable rows (the shared naming surface) ──────────────────────────────

/// Every net as a voltage trace, ground net included: RSpice's ground-less
/// decks compact their lowest net onto id 0, where its reading can be the
/// deck's meaningful level, so it carries like any other net.
fn node_variables(net: &Circuit) -> Vec<RawVariable> {
    (0..net.nodes)
        .map(|i| RawVariable {
            name: format!("V({})", node_label(net, i)),
            kind: "voltage".into(),
        })
        .collect()
}

/// Every element family's current trace in the canonical walk, spelled like
/// the report probes (`I(R1)`, `Ic(Q1)`, `I(T1)`).
fn current_variables(net: &Circuit) -> Vec<RawVariable> {
    CURRENT_FAMILIES
        .iter()
        .flat_map(|family| {
            // `element_names` builds an owned Vec, so the map runs inside
            // the closure — trace rows per element, closure-local.
            net.element_names(*family)
                .iter()
                .map(|name| RawVariable {
                    name: format!("{}({})", family.probe_prefix(), name),
                    kind: "device_current".into(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The step values of one run as constant columns named after their targets
/// (see the module comment: self-describing steps, no `.log` sidecar).
fn step_variables(step_values: &[(String, f64)]) -> Vec<RawVariable> {
    step_values
        .iter()
        .map(|(target, _)| RawVariable {
            name: target.clone(),
            kind: target_kind(target).into(),
        })
        .collect()
}

/// A step column's kind: a swept source reads as its own flavour, anything
/// else (a `.param` key, `temp`) is metadata the reference kind list has no
/// word for.
fn target_kind(target: &str) -> &'static str {
    match target.chars().find(|c| c.is_ascii_alphabetic()) {
        Some('v') | Some('V') => "voltage",
        Some('i') | Some('I') => "device_current",
        _ => "parameter",
    }
}

/// Deck-spelled net label (falls back to the numeric id like the CLI).
fn node_label(net: &Circuit, i: usize) -> String {
    let name = net.node_name(i);
    if name.is_empty() {
        format!("{i}")
    } else {
        name.to_string()
    }
}

// ─── Plot builders ──────────────────────────────────────────────────────────

/// Assemble a `RawDataset`: flags grow the `stepped` mark when columns carry
/// step values.
fn dataset_from(
    title: &str,
    plotname: &str,
    base_flags: &[&str],
    variables: Vec<RawVariable>,
    rows: Vec<Vec<Complex64>>,
    step_values: &[(String, f64)],
) -> RawDataset {
    let mut variables = variables;
    variables.extend(step_variables(step_values));
    let mut flags: Vec<String> = base_flags.iter().map(|f| f.to_string()).collect();
    if !step_values.is_empty() {
        flags.push("stepped".into());
    }
    RawDataset {
        title: title.to_string(),
        plotname: plotname.to_string(),
        flags,
        variables,
        values: rows,
    }
}

/// The operating point as a one-point real plot; its axis is the synthetic
/// `time = 0` marker (see the module comment) and `forward` stays off.
fn op_dataset(
    title: &str,
    net: &Circuit,
    dc: &DcResult,
    step_values: &[(String, f64)],
) -> RawDataset {
    let mut variables = vec![RawVariable {
        name: "time".into(),
        kind: "time".into(),
    }];
    variables.extend(node_variables(net));
    variables.extend(current_variables(net));
    let mut row = vec![Complex64::new(0.0, 0.0)];
    extend_real(&mut row, &dc.node_voltages);
    extend_real_currents(&mut row, &dc.branch_currents);
    extend_constants(&mut row, step_values);
    dataset_from(
        title,
        "Operating Point",
        &["real"],
        variables,
        vec![row],
        step_values,
    )
}

/// A `.dc` sweep: the primary swept target is the axis, a nested second
/// target joins as its own variable column, values follow.
fn sweep_dataset(
    title: &str,
    net: &Circuit,
    sweep: &DcSweepResult,
    step_values: &[(String, f64)],
) -> RawDataset {
    let mut variables: Vec<RawVariable> = sweep
        .targets
        .iter()
        .map(|target| RawVariable {
            name: target.clone(),
            kind: target_kind(target).into(),
        })
        .collect();
    variables.extend(node_variables(net));
    variables.extend(current_variables(net));
    let rows = sweep
        .points
        .iter()
        .map(|point| {
            let mut row: Vec<Complex64> = Vec::with_capacity(variables.len());
            extend_real(&mut row, &point.values);
            extend_real(&mut row, &point.node_voltages);
            extend_real_currents(&mut row, &point.branch_currents);
            extend_constants(&mut row, step_values);
            row
        })
        .collect();
    let mut flags = vec!["real"];
    if sweep.points.len() > 1 {
        flags.push("forward");
    }
    dataset_from(
        title,
        "DC transfer characteristic",
        &flags,
        variables,
        rows,
        step_values,
    )
}

/// A transient run: real values over `time`.
fn tran_dataset(
    title: &str,
    net: &Circuit,
    tran: &TranResult,
    step_values: &[(String, f64)],
) -> RawDataset {
    let mut variables = vec![RawVariable {
        name: "time".into(),
        kind: "time".into(),
    }];
    variables.extend(node_variables(net));
    variables.extend(current_variables(net));
    let rows = tran
        .points
        .iter()
        .map(|point| {
            let mut row: Vec<Complex64> = Vec::with_capacity(variables.len());
            row.push(Complex64::new(point.time, 0.0));
            extend_real(&mut row, &point.node_voltages);
            extend_real_currents(&mut row, &point.branch_currents);
            extend_constants(&mut row, step_values);
            row
        })
        .collect();
    let mut flags = vec!["real"];
    if tran.points.len() > 1 {
        flags.push("forward");
    }
    dataset_from(
        title,
        "Transient Analysis",
        &flags,
        variables,
        rows,
        step_values,
    )
}

/// An AC sweep: complex phasors over `frequency`, with the log/linear plot
/// hint from the command's scale.
fn ac_dataset(
    title: &str,
    net: &Circuit,
    ac: &AcResult,
    ac_log: bool,
    step_values: &[(String, f64)],
) -> RawDataset {
    let mut variables = vec![RawVariable {
        name: "freq".into(),
        kind: "frequency".into(),
    }];
    variables.extend(node_variables(net));
    variables.extend(current_variables(net));
    let rows = ac
        .points
        .iter()
        .map(|point| {
            let mut row: Vec<Complex64> = Vec::with_capacity(variables.len());
            row.push(Complex64::new(point.freq, 0.0));
            extend_phasors(&mut row, &point.node_voltages);
            extend_complex_currents(&mut row, &point.branch_currents);
            extend_constants(&mut row, step_values);
            row
        })
        .collect();
    let mut flags = vec!["complex"];
    if ac.points.len() > 1 {
        flags.push("forward");
    }
    flags.push(if ac_log { "log" } else { "linear" });
    dataset_from(title, "AC Analysis", &flags, variables, rows, step_values)
}

/// Real voltages/sweep values into the row.
fn extend_real(row: &mut Vec<Complex64>, values: &[f64]) {
    row.extend(values.iter().map(|v| Complex64::new(*v, 0.0)));
}

/// Phasors into the row.
fn extend_phasors(row: &mut Vec<Complex64>, values: &[Complex64]) {
    row.extend_from_slice(values);
}

/// Real current families and step-value constants into the row.
fn extend_real_currents(row: &mut Vec<Complex64>, currents: &BranchCurrents<f64>) {
    for family in &CURRENT_FAMILIES {
        row.extend(
            currents
                .slice(*family)
                .unwrap_or(&[])
                .iter()
                .map(|i| Complex64::new(*i, 0.0)),
        );
    }
}

fn extend_complex_currents(row: &mut Vec<Complex64>, currents: &BranchCurrents<Complex64>) {
    for family in &CURRENT_FAMILIES {
        row.extend(currents.slice(*family).unwrap_or(&[]).iter().copied());
    }
}

/// Constant step columns close every row (their values never vary in-run).
fn extend_constants(row: &mut Vec<Complex64>, step_values: &[(String, f64)]) {
    row.extend(step_values.iter().map(|(_, v)| Complex64::new(*v, 0.0)));
}

// ─── Header text (shared by both flavors) ───────────────────────────────────

/// The preamble text of one plot, LF-terminated lines (the caller encodes).
fn header_text(ds: &RawDataset, date: &str) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "Title: {}", ds.title);
    let _ = writeln!(text, "Date: {date}");
    let _ = writeln!(text, "Plotname: {}", ds.plotname);
    let _ = writeln!(text, "Flags: {}", ds.flags.join(" "));
    let _ = writeln!(text, "No. Variables: {}", ds.variables.len());
    let _ = writeln!(text, "No. Points: {}", ds.values.len());
    let _ = writeln!(text, "Offset: 0");
    let _ = writeln!(text, "Command: circuitsim");
    text.push_str("Variables:\n");
    for (i, var) in ds.variables.iter().enumerate() {
        let _ = writeln!(text, "\t{i}\t{}\t{}", var.name, var.kind);
    }
    text
}

/// Deterministic-in-shape `Date:` fill: unix seconds — `std` ships no
/// calendar, and every reader treats the field as opaque text.
fn stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| format!("unix-seconds:{}", elapsed.as_secs()))
        .unwrap_or_else(|_| "unix-seconds:0".into())
}

// ─── ASCII writer ───────────────────────────────────────────────────────────

/// The ASCII flavor: plain header bytes, then one value per line — a
/// variable-0 line starts the next point, and a complex value is `re,im`.
pub fn to_ascii(file: &RawFile) -> String {
    let date = stamp();
    let mut text = String::new();
    for ds in &file.datasets {
        text.push_str(&header_text(ds, &date));
        text.push_str("Values:\n");
        let complex = ds.flags.iter().any(|f| f.eq_ignore_ascii_case("complex"));
        for row in &ds.values {
            for (i, value) in row.iter().enumerate() {
                // A not-a-number cell would break the reader's line
                // accounting; the solver never produces one, and 0 keeps
                // the row accounting honest.
                let v = sanitize(*value);
                if complex {
                    let _ = writeln!(text, "\t{i}\t{:.15e},{:.15e}", v.re, v.im);
                } else {
                    let _ = writeln!(text, "\t{i}\t{:.15e}", v.re);
                }
            }
        }
    }
    text
}

// ─── Binary writer ──────────────────────────────────────────────────────────

/// The binary flavor: header encoded UTF-16-LE (LTspice's own spelling), a
/// `Binary:` marker, then the point-major payload — per point, the axis as
/// an 8-byte double followed by 4-byte floats for the real plots; complex
/// plots carry every variable as an 8-byte (re, im) pair.
pub fn to_binary(file: &RawFile) -> Vec<u8> {
    let date = stamp();
    let mut out = Vec::new();
    for ds in &file.datasets {
        let header = format!("{}Binary:\n", header_text(ds, &date));
        out.extend(header.encode_utf16().flat_map(|unit| unit.to_le_bytes()));
        let complex = ds.flags.iter().any(|f| f.eq_ignore_ascii_case("complex"));
        for row in &ds.values {
            for (i, value) in row.iter().enumerate() {
                let v = sanitize(*value);
                if complex {
                    out.extend(v.re.to_le_bytes());
                    out.extend(v.im.to_le_bytes());
                } else if i == 0 {
                    out.extend(v.re.to_le_bytes());
                } else {
                    out.extend((v.re as f32).to_le_bytes());
                }
            }
        }
    }
    out
}

/// A not-a-number cell would break the payload's size accounting and pass
/// NaN-comparisons silently into waveform views; the solver never produces
/// one, so 0 is the honest fallback.
fn sanitize(value: Complex64) -> Complex64 {
    let re = if value.re.is_nan() { 0.0 } else { value.re };
    let im = if value.im.is_nan() { 0.0 } else { value.im };
    Complex64::new(re, im)
}

// ─── Reader ─────────────────────────────────────────────────────────────────

/// Parse a `.raw` byte stream (either flavor, either header encoding, one or
/// many plots) into datasets in file order.
pub fn read(bytes: &[u8]) -> Result<RawFile> {
    let starts = block_starts(bytes);
    if starts.is_empty() {
        return Err(SimError::Parse(
            "raw reader: no 'Title:' header found — not a .raw file".into(),
        ));
    }
    let mut datasets = Vec::with_capacity(starts.len());
    for (i, start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(bytes.len());
        datasets.push(parse_block(&bytes[*start..end])?);
    }
    Ok(RawFile { datasets })
}

/// Positions where a plot begins: `Title:` in the file's own encoding.
fn block_starts(bytes: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let marker: Vec<u8> = "Title:"
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let plain = b"Title:".to_vec();
    let keys: &[Vec<u8>] = if starts_utf16(bytes) {
        &[marker]
    } else {
        &[plain]
    };
    let mut at = 0;
    while let Some(found) = find(bytes, &keys[0], at) {
        starts.push(found);
        at = found + 1;
    }
    starts
}

/// A file/blocks whose second and fourth byte are NUL carries the UTF-16-LE
/// header spelling (the "T\0i\0" signature).
fn starts_utf16(bytes: &[u8]) -> bool {
    bytes.first() == Some(&b'T')
        && bytes.get(1) == Some(&0)
        && bytes.get(2) == Some(&b'i')
        && bytes.get(3) == Some(&0)
}

/// First occurrence of `marker` at or after `from`, byte-naive.
fn find(bytes: &[u8], marker: &[u8], from: usize) -> Option<usize> {
    (from..=bytes.len().saturating_sub(marker.len()))
        .find(|&i| &bytes[i..i + marker.len()] == marker)
}

/// UTF-16-LE or byte-per-char decoding of a preamble (the payload markers
/// are searched in the same encoding).
fn parse_block(block: &[u8]) -> Result<RawDataset> {
    let utf16 = starts_utf16(block);
    let keys = |word: &str| -> Vec<u8> {
        if utf16 {
            word.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
        } else {
            word.as_bytes().to_vec()
        }
    };
    let bin_key = keys("Binary:\n");
    let val_key = keys("Values:\n");
    let bin_at = find(block, &bin_key, 0);
    let ascii_at = find(block, &val_key, 0);
    let (marker_at, is_binary, key_len) = match (bin_at, ascii_at) {
        (Some(bin), Some(val)) if bin < val => (bin, true, bin_key.len()),
        (Some(bin), None) => (bin, true, bin_key.len()),
        (None, Some(val)) => (val, false, val_key.len()),
        (Some(_), Some(val)) => (val, false, val_key.len()),
        (None, None) => {
            return Err(SimError::Parse(
                "raw reader: header has no 'Binary:' or 'Values:' marker".into(),
            ))
        }
    };
    let data_start = marker_at + key_len;
    let header = decode_text(&block[..data_start], utf16)?;

    // Key walk. After the `Variables:` keyword every line belongs to the
    // list until a marker keyword closes it — a name inside the list may
    // carry a colon, so the entries are captured before key matching.
    let mut title = None;
    let mut plotname = None;
    let mut flags_line = None;
    let mut declared_vars = None;
    let mut declared_points = None;
    let mut variable_entries: Vec<String> = Vec::new();
    let mut in_variables = false;
    for line in header.lines() {
        let (key, rest) = match line.find(':') {
            Some(colon) => (line[..colon].trim(), line[colon + 1..].trim()),
            None => (line.trim(), ""),
        };
        if in_variables {
            if key.eq_ignore_ascii_case("binary") || key.eq_ignore_ascii_case("values") {
                in_variables = false;
            } else if !line.trim().is_empty() {
                variable_entries.push(line.trim().to_string());
            }
            continue;
        }
        match key.to_ascii_lowercase().as_str() {
            "title" => title = Some(rest.to_string()),
            "plotname" => plotname = Some(rest.to_string()),
            "flags" => flags_line = Some(rest.to_string()),
            "no. variables" => declared_vars = rest.parse().ok(),
            "no. points" => declared_points = rest.parse().ok(),
            "variables" => in_variables = true,
            // Offset/Command/Backannotation and both trace markers are opaque.
            _ => {}
        }
    }
    let Some(plotname) = plotname else {
        return Err(SimError::Parse(
            "raw reader: header has no 'Plotname:' line".into(),
        ));
    };
    let Some(flags_line) = flags_line else {
        return Err(SimError::Parse(
            "raw reader: header has no 'Flags:' line".into(),
        ));
    };
    let flags: Vec<String> = flags_line.split_whitespace().map(str::to_string).collect();
    let complex = flags.iter().any(|f| f.eq_ignore_ascii_case("complex"));

    let mut variables = Vec::with_capacity(variable_entries.len());
    for entry in &variable_entries {
        let tokens: Vec<&str> = entry.split_whitespace().collect();
        if tokens.len() < 2 {
            return Err(SimError::Parse(format!(
                "raw reader: malformed Variables entry {entry:?}"
            )));
        }
        // `tokens[0]` is the plot-internal ordinal; the name is `tokens[1]`
        // and the kind word trails (net names with spaces would break this,
        // and no reference reader supports them in the file format either).
        variables.push(RawVariable {
            name: tokens[1].to_string(),
            kind: tokens.last().copied().unwrap_or("").to_string(),
        });
    }
    let n_vars = if variables.is_empty() {
        // A declared count without a Variables list synthesises placeholder
        // names so the payload decode still lines up column-for-column.
        let Some(count) = declared_vars else {
            return Err(SimError::Parse(
                "raw reader: header lists no variables".into(),
            ));
        };
        for i in 0..count {
            variables.push(RawVariable {
                name: format!("v{i}"),
                kind: String::new(),
            });
        }
        count
    } else {
        variables.len()
    };

    let payload = &block[data_start..];
    let is_fast = flags.iter().any(|f| f.eq_ignore_ascii_case("fastaccess"));
    let values = if is_binary {
        decode_binary(
            payload,
            declared_points.unwrap_or(0),
            n_vars,
            complex,
            is_fast,
        )?
    } else {
        // Binary payloads are raw words, so only an ascii payload goes
        // through the text decoder.
        decode_ascii(&decode_text(payload, utf16)?, n_vars, complex)?
    };
    Ok(RawDataset {
        title: title.unwrap_or_default(),
        plotname,
        flags,
        variables,
        values,
    })
}

/// UTF-16-LE or byte-per-char decoding of a preamble.
fn decode_text(bytes: &[u8], utf16: bool) -> Result<String> {
    if utf16 {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units)
            .map_err(|e| SimError::Parse(format!("raw reader: bad UTF-16 header: {e}")))
    } else {
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Binary payload decode. Real plots: the point-major layout carries an
/// 8-byte double axis and 4-byte values per point (the shape mcltspice pins
/// for modern LTspice); the all-double layout is read only when the byte
/// count rules the mixed one out, and with 3+ variables both fit — mixed
/// wins, matching LTspice (`n_vars <= 1` is the degenerate identical case).
/// Complex plots: 16-byte (re, im) pairs throughout. A `FastAccess` file
/// stores variable-major blocks of `n_points` values each instead. The
/// decoded point count is min(declared, payload capacity) — a truncated tail
/// just ends there, still-aligned rows only.
fn decode_binary(
    payload: &[u8],
    declared_points: usize,
    n_vars: usize,
    complex: bool,
    fast: bool,
) -> Result<Vec<Vec<Complex64>>> {
    let mut cursor = 0usize;
    let next_f64 = |cursor: &mut usize| -> Result<f64> {
        let bytes: [u8; 8] = payload
            .get(*cursor..*cursor + 8)
            .and_then(|window| window.try_into().ok())
            .ok_or_else(|| SimError::Parse("raw reader: binary payload truncated".into()))?;
        *cursor += 8;
        Ok(f64::from_le_bytes(bytes))
    };
    let next_f32 = |cursor: &mut usize| -> Result<f32> {
        let bytes: [u8; 4] = payload
            .get(*cursor..*cursor + 4)
            .and_then(|window| window.try_into().ok())
            .ok_or_else(|| SimError::Parse("raw reader: binary payload truncated".into()))?;
        *cursor += 4;
        Ok(f32::from_le_bytes(bytes))
    };

    if complex {
        let capacity = payload.len() / 16 / n_vars.max(1);
        let count = point_count(declared_points, capacity);
        if !fast {
            let mut rows = Vec::with_capacity(count);
            for _ in 0..count {
                let mut row = Vec::with_capacity(n_vars);
                for _ in 0..n_vars {
                    let re = next_f64(&mut cursor)?;
                    let im = next_f64(&mut cursor)?;
                    row.push(Complex64::new(re, im));
                }
                rows.push(row);
            }
            return Ok(rows);
        }
        // FastAccess: one full column per variable, axis block first.
        let mut columns = Vec::with_capacity(n_vars);
        for _ in 0..n_vars {
            let mut column = Vec::with_capacity(count);
            for _ in 0..count {
                let re = next_f64(&mut cursor)?;
                let im = next_f64(&mut cursor)?;
                column.push(Complex64::new(re, im));
            }
            columns.push(column);
        }
        return Ok(rows_from_columns(columns));
    }

    // Real plots: pick the layout by byte count (see the doc comment).
    let per_point_mixed = 8 + 4 * n_vars.saturating_sub(1);
    let per_point_double = 8 * n_vars;
    let all_double = n_vars > 1
        && payload.len().is_multiple_of(per_point_double)
        && !payload.len().is_multiple_of(per_point_mixed);
    let width = if all_double {
        per_point_double
    } else {
        per_point_mixed
    };
    let capacity = payload.len() / width.max(1);
    let count = point_count(declared_points, capacity);
    if !fast {
        let mut rows = Vec::with_capacity(count);
        for _ in 0..count {
            let mut row = Vec::with_capacity(n_vars);
            for var in 0..n_vars {
                // Axis or all-double: 8 bytes; otherwise 4.
                let re = if var == 0 || all_double {
                    next_f64(&mut cursor)?
                } else {
                    next_f32(&mut cursor)? as f64
                };
                row.push(Complex64::new(re, 0.0));
            }
            rows.push(row);
        }
        return Ok(rows);
    }
    // FastAccess real: one full block per variable, axis block first.
    let mut columns: Vec<Vec<Complex64>> = Vec::with_capacity(n_vars);
    for var in 0..n_vars {
        let mut column = Vec::with_capacity(count);
        for _ in 0..count {
            let re = if var == 0 || all_double {
                next_f64(&mut cursor)?
            } else {
                next_f32(&mut cursor)? as f64
            };
            column.push(Complex64::new(re, 0.0));
        }
        columns.push(column);
    }
    Ok(rows_from_columns(columns))
}

/// Declared vs payload-capacity point count: the declared count is trusted
/// when the payload has room for it, the payload otherwise sets the count
/// (a `0` declaration means the header did not carry one at all).
fn point_count(declared: usize, capacity: usize) -> usize {
    if declared == 0 {
        capacity
    } else {
        declared.min(capacity)
    }
}

/// Column-major blocks re-rowed point-major (FastAccess payloads decode one
/// variable block at a time and assemble here).
fn rows_from_columns(columns: Vec<Vec<Complex64>>) -> Vec<Vec<Complex64>> {
    let points = columns.first().map_or(0, Vec::len);
    (0..points)
        .map(|p| {
            columns
                .iter()
                .map(|column| column.get(p).copied().unwrap_or_default())
                .collect()
        })
        .collect()
}

/// ASCII payload decode: one value per line (`<index><tab><value>`), a
/// variable-0 line closes the previous point and starts the next, complex
/// values spell `re,im` (possibly with spaces — tokens re-join).
fn decode_ascii(text: &str, n_vars: usize, complex: bool) -> Result<Vec<Vec<Complex64>>> {
    let mut rows: Vec<Vec<Complex64>> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let Some(index_text) = tokens.next() else {
            continue;
        };
        let Ok(index) = index_text.parse::<usize>() else {
            return Err(SimError::Parse(format!(
                "raw reader: ascii payload line {line:?} does not open with a variable index"
            )));
        };
        if index == 0 {
            // A new point starts: the row in progress must be complete.
            if let Some(row) = rows.last() {
                if row.len() != n_vars {
                    return Err(SimError::Parse(format!(
                        "raw reader: ascii point has {} values, expected {n_vars}",
                        row.len()
                    )));
                }
            }
            rows.push(Vec::with_capacity(n_vars));
        }
        let Some(row) = rows.last_mut() else {
            return Err(SimError::Parse(
                "raw reader: ascii payload opens with variable 1".into(),
            ));
        };
        if index != row.len() {
            return Err(SimError::Parse(format!(
                "raw reader: ascii value at variable {index} of a {len}-long row",
                len = row.len()
            )));
        }
        row.push(parse_ascii_value(
            &tokens.collect::<Vec<_>>().join(" "),
            complex,
        )?);
    }
    if let Some(row) = rows.last() {
        if row.len() != n_vars {
            return Err(SimError::Parse(format!(
                "raw reader: final ascii point has {} values, expected {n_vars}",
                row.len()
            )));
        }
    }
    Ok(rows)
}

/// One ascii value: `re` for real plots, `re`,`im` (comma-paired, the
/// tokens may carry spaces around it) for complex.
fn parse_ascii_value(text: &str, complex: bool) -> Result<Complex64> {
    let broken = |text: &str| SimError::Parse(format!("raw reader: broken ascii value {text:?}"));
    if complex {
        let (re, im) = text.split_once(',').ok_or_else(|| broken(text))?;
        let re: f64 = re.trim().parse().map_err(|_| broken(text))?;
        let im: f64 = im.trim().parse().map_err(|_| broken(text))?;
        Ok(Complex64::new(re, im))
    } else {
        let re: f64 = text.trim().parse().map_err(|_| broken(text))?;
        Ok(Complex64::new(re, 0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::parse;
    use crate::runs::run;
    use approx::assert_relative_eq;

    fn sim(deck: &str) -> RawFile {
        let netlist = parse(deck).expect("deck parses");
        let result = run(&netlist).expect("deck runs");
        build("test title", &netlist, &result)
    }

    /// The binary tran flavor: the file reads back with the same plot
    /// naming, exact axis (f64), and f32-tolerant values.
    #[test]
    fn tran_binary_round_trip() {
        let raw = sim("R1 in a 1k\nR2 a 0 1k\nV1 in 0 5\n.tran 100n 1u\n.end\n");
        assert_eq!(raw.datasets.len(), 1);
        let ds = &raw.datasets[0];
        assert_eq!(ds.plotname, "Transient Analysis");
        assert_eq!(ds.flags, ["real", "forward"]);
        assert_eq!(ds.variables[0].name, "time");
        assert_eq!(ds.variables[0].kind, "time");
        assert_eq!(ds.variables.len(), 7); // time + 3 nets + 3 currents
        let header = header_text(ds, "date");
        assert!(header.contains("Title: test title"));
        assert!(header.contains("Plotname: Transient Analysis"));
        assert!(header.contains("No. Points: 13"));

        let bytes = to_bytes(&raw, RawFlavor::Binary);
        let read_back = read(&bytes).expect("our own binary flavor re-reads");
        assert_eq!(read_back.datasets.len(), 1);
        let back = &read_back.datasets[0];
        assert_eq!(back.plotname, "Transient Analysis");
        assert_eq!(
            back.variables.iter().map(|v| &v.name).collect::<Vec<_>>(),
            ds.variables.iter().map(|v| &v.name).collect::<Vec<_>>()
        );
        // The axis is f64 in the binary flavor — exact.
        assert_eq!(back.axis(), ds.axis());
        // Values ride on f32: tolerant equality.
        let a = ds.trace("V(a)").unwrap();
        let b = back.trace("V(a)").unwrap();
        for (x, y) in a.iter().zip(&b) {
            assert!(
                (x.re - y.re).abs() < 1e-5,
                "f32 loss overstepped: {x} vs {y}"
            );
        }
        let src = ds.trace("I(R1)").unwrap();
        assert_relative_eq!(src[3].re, 2.5e-3, epsilon = 1e-7);
        // The V1 source current is the drawn current with the report's
        // (negative) polarity, round-tripped through f32.
        let v1 = ds.trace("I(V1)").unwrap();
        assert_relative_eq!(v1[3].re, -2.5e-3, epsilon = 1e-7);
    }

    /// The AC flavor keeps every phasor bit-exact (f64 pairs round-trip).
    #[test]
    fn ac_complex_round_trip_exact() {
        let raw = sim("V1 a 0 ac 1\nR1 a b 1k\nR2 b 0 1k\n.ac dec 4 1 100\n.end\n");
        assert_eq!(raw.datasets.len(), 1);
        let ds = &raw.datasets[0];
        assert_eq!(ds.plotname, "AC Analysis");
        assert_eq!(ds.flags, ["complex", "forward", "log"]);
        assert_eq!(ds.variables[0].name, "freq");
        assert_eq!(ds.variables[0].kind, "frequency");
        // The matched pair divides by exactly 2 at every frequency.
        let mid = ds.trace("V(b)").unwrap();
        assert_relative_eq!(mid[0].re, 0.5, epsilon = 1e-12);
        assert_relative_eq!(mid[0].im, 0.0, epsilon = 1e-12);

        let bytes = to_bytes(&raw, RawFlavor::Binary);
        let back = read(&bytes).unwrap();
        let back = &back.datasets[0];
        assert_eq!(back.plotname, "AC Analysis");
        assert_eq!(back.flags, ["complex", "forward", "log"]);
        let a = ds.trace("V(b)").unwrap();
        let b = back.trace("V(b)").unwrap();
        assert_eq!(a, b, "complex values must round-trip bit-exact");
        // The frequency axis is real-valued in the complex encoding.
        assert_eq!(back.axis(), ds.axis());
    }

    /// Plotname spellings and flag sets for the single-point analyses.
    #[test]
    fn op_and_dc_plotnames_and_axis() {
        let raw = sim("R1 in a 1k\nV1 in 0 5\n.op\n.end\n");
        assert_eq!(raw.datasets.len(), 1);
        let op = &raw.datasets[0];
        assert_eq!(op.plotname, "Operating Point");
        assert_eq!(op.flags, ["real"]);
        assert_eq!(op.values.len(), 1);
        // Synthetic axis at 0, and the ground net listed first with it.
        assert_eq!(op.values[0][0].re, 0.0);
        assert_eq!(op.variables[0].name, "time");
        assert_eq!(op.variables[1].name, "V(0)");
        let v_in = op.trace("V(in)").unwrap();
        assert_relative_eq!(v_in[0].re, 5.0, epsilon = 1e-12);

        let raw = sim("V1 a 0 3\nR1 a 0 1k\n.dc V1 0 2 1\n.end\n");
        let dc = &raw.datasets[0];
        assert_eq!(dc.plotname, "DC transfer characteristic");
        assert_eq!(dc.flags, ["real", "forward"]);
        // The sweep target is the axis variable.
        assert_eq!(dc.variables[0].name, "V1");
        assert_eq!(dc.variables[0].kind, "voltage");
        assert_eq!(dc.axis(), [0.0, 1.0, 2.0]);
        let across = dc.trace("V(a)").unwrap();
        assert_relative_eq!(across[2].re, 2.0, epsilon = 1e-12);
    }

    /// Stepped decks concatenate self-headered plots, each carrying the
    /// step value as a constant column named after the target.
    #[test]
    fn stepped_runs_become_plot_per_run_with_step_columns() {
        let raw = sim(".param r=1k\nV1 in 0 DC 1\nR1 in out {r}\nR2 out 0 1k\n\
             .step param r 1k 3k 1k\n.op\n.end\n");
        assert_eq!(raw.datasets.len(), 3);
        for (i, ds) in raw.datasets.iter().enumerate() {
            assert_eq!(ds.plotname, "Operating Point");
            assert_eq!(ds.flags, ["real", "stepped"]);
            // The constant step column: always 1000·(i+1) Ω.
            let names = ds
                .variables
                .iter()
                .map(|v| v.name.clone())
                .collect::<Vec<_>>();
            assert!(
                names.iter().any(|n| n == "r"),
                "step column 'r' present: {names:?}"
            );
            let step_run = ds.trace("r").unwrap();
            assert_eq!(step_run.len(), 1);
            assert_relative_eq!(step_run[0].re, 1000.0 * (i + 1) as f64, epsilon = 1e-9);
            let out = ds.trace("V(out)").unwrap();
            // V(out) = 1 V across R2 with R1 = r: 0.5, 1/3, 1/4.
            assert_relative_eq!(
                out[0].re,
                1_000.0 / (1_000.0 * (i + 1) as f64 + 1_000.0),
                epsilon = 1e-9
            );
        }
        // Reading the file back keeps all three plots with their own values.
        let bytes = to_bytes(&raw, RawFlavor::Binary);
        let back = read(&bytes).unwrap();
        assert_eq!(back.datasets.len(), 3);
        let steps: Vec<f64> = back
            .datasets
            .iter()
            .map(|ds| ds.trace("r").unwrap()[0].re)
            .collect();
        assert_eq!(steps, [1000.0, 2000.0, 3000.0]);
    }

    /// The ASCII flavor: plain header bytes and a `Values:` listing that the
    /// reader decodes back to the same table.
    #[test]
    fn ascii_round_trip() {
        let raw = sim("R1 in a 1k\nR2 a 0 1k\nV1 in 0 5\n.tran 100n 1u\n.end\n");
        let text = to_ascii(&raw);
        assert!(text.starts_with("Title:"));
        assert!(text.contains("\nPlotname: Transient Analysis\n"));
        assert!(text.contains("\nValues:\n"));
        let back = read(text.as_bytes()).unwrap();
        assert_eq!(back.datasets.len(), 1);
        let back = &back.datasets[0];
        assert_eq!(back.plotname, "Transient Analysis");
        let a = raw.datasets[0].trace("V(a)").unwrap();
        let b = back.trace("V(a)").unwrap();
        assert_eq!(a, b, "ascii values are printed at 15 digits — exact");
        // Complex ascii: comma-paired, round-trips too.
        let raw = sim("V1 a 0 ac 1\nR1 a b 1k\nR2 b 0 1k\n.ac dec 2 1 10\n.end\n");
        let back = read(to_ascii(&raw).as_bytes()).unwrap();
        let a = raw.datasets[0].trace("V(b)").unwrap();
        let b = back.datasets[0].trace("V(b)").unwrap();
        assert_eq!(a, b);
    }

    /// Reader tolerance: a `FastAccess` complex block stores column-major
    /// 16-byte pairs and re-rows into point-major datasets.
    #[test]
    fn reads_hand_crafted_fast_access_complex_block() {
        let header = "Title: t\nDate: d\nPlotname: FastAccess\nFlags: complex FastAccess\n\
                      No. Variables: 3\nNo. Points: 2\nOffset: 0\nCommand: circuitsim\n\
                      Variables:\n\t0\tfreq\tfrequency\n\t1\tV(a)\tvoltage\n\t2\tV(b)\tvoltage\n\
                      Binary:\n";
        let mut block: Vec<u8> = header.as_bytes().to_vec();
        // Column-major: one full variable block after the other, each value
        // a 16-byte (re, im) pair — the axis column first.
        let pairs: [(f64, f64); 6] = [
            (1.0, 0.0),
            (2.0, 0.0), // freq column
            (3.0, 1.0),
            (4.0, 1.0), // V(a) column
            (5.0, -1.0),
            (6.0, -1.0), // V(b) column
        ];
        for &(re, im) in &pairs {
            block.extend(re.to_le_bytes());
            block.extend(im.to_le_bytes());
        }
        let file = read(&block).expect("the FastAccess complex block decodes");
        assert_eq!(file.datasets.len(), 1);
        let back = &file.datasets[0];
        assert_eq!(back.plotname, "FastAccess");
        assert_eq!(back.flags, ["complex", "FastAccess"]);
        // The declared 2 points of 3 variables re-row point-major.
        let row0 = &back.values[0];
        let row1 = &back.values[1];
        assert_eq!(
            row0,
            &[
                Complex64::new(1.0, 0.0),
                Complex64::new(3.0, 1.0),
                Complex64::new(5.0, -1.0)
            ]
        );
        assert_eq!(
            row1,
            &[
                Complex64::new(2.0, 0.0),
                Complex64::new(4.0, 1.0),
                Complex64::new(6.0, -1.0)
            ]
        );
    }

    /// The all-double real layout: with 2 variables the step-8 payload
    /// (32 bytes) divides by 16 but not by 12, so the mixed reading loses
    /// and every value is an 8-byte double.
    #[test]
    fn reads_all_double_real_layout_when_mixed_ruled_out() {
        let header = "Title: t\nDate: d\nPlotname: P\nFlags: real\n\
                      No. Variables: 2\nNo. Points: 2\nOffset: 0\nCommand: circuitsim\n\
                      Variables:\n\t0\ttime\ttime\n\t1\tV(a)\tvoltage\nBinary:\n";
        let mut block: Vec<u8> = header.as_bytes().to_vec();
        for value in [1.0f64, 2.0, 3.0, 4.0] {
            block.extend(value.to_le_bytes());
        }
        let file = read(&block).expect("the all-double block decodes");
        let back = &file.datasets[0];
        assert_eq!(back.values.len(), 2);
        assert_eq!(back.values[1][0].re, 3.0);
        assert_eq!(back.values[1][1].re, 4.0);
    }

    /// Loud parse errors: a non-`.raw` file is rejected, and a malformed
    /// ascii value names itself.
    #[test]
    fn reader_rejects_non_raw_and_broken_values() {
        let err = read(b"not a raw file").expect_err("not a .raw");
        assert!(err.to_string().contains("not a .raw file"));
        let header = "Title: t\nPlotname: P\nFlags: real\nNo. Variables: 2\n\
                      No. Points: 1\nOffset: 0\nCommand: circuitsim\n\
                      Variables:\n\t0\ttime\ttime\n\t1\tV(a)\tvoltage\nValues:\n";
        let bytes = format!("{header}\t0\t1.0\n\t1\tbroken\n");
        let err = read(bytes.as_bytes()).expect_err("broken value");
        assert!(err.to_string().contains("broken"), "loud: {err}");
        // Point accounting too: a short row is a loud error.
        let bytes = format!("{header}\t0\t1.0\n");
        let err = read(bytes.as_bytes()).expect_err("short row");
        assert!(
            err.to_string().contains("1 values, expected 2"),
            "loud: {err}"
        );
    }
}
