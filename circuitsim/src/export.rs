//! The CSV export surface (TODO §6): a sectioned text rendition of the
//! [`rawwave`] builder — one section per `.raw` plot in file order plus a
//! `.meas` table, so one builder feeds the binary writer, the ASCII writer,
//! and this exporter alike.
//!
//! Sections open with a `# <Plotname>` comment line and a blank line between
//! sections. Complex waveforms ride as two cells (`<name>_mag`,
//! `<name>_phase` degrees) like the AC-\[`--waves`\] CSV did; the axis
//! column prints its real part. Numeric cells spill `{:.9e}`; text cells
//! (measurement names and errors) take RFC 4180 quoting.
//!
//! The SVG surface draws the same plots as one panel per `.raw` plot
//! shape ([`panel_groups`]), stacked in first-occurrence order, so one
//! builder also feeds the image exporter — a stepped deck's same-shape
//! runs overlay onto one panel with per-run identification.

use crate::analysis::MeasureResult;
use crate::rawwave::{RawDataset, RawFile};
use num_complex::Complex64;

/// The CSV document for one built [`RawFile`] — every analysis, every step
/// run — plus the `.meas` results of the first run. An empty `traces` list
/// exports every column; a populated one keeps, per plot, the axis plus the
/// named waveforms matched case-insensitively by column name (step columns
/// ride along, so the rows keep their run provenance); requests no plot
/// carries are simply silent.
pub fn csv_document(raw: &RawFile, measurements: &[MeasureResult], traces: &[String]) -> String {
    let mut out = String::new();
    for ds in &raw.datasets {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("# {}\n", ds.plotname));
        let keep = selected_variables(ds, traces);
        let complex = is_complex(ds);
        out.push_str(&header_cells(ds, &keep, complex).join(","));
        out.push('\n');
        for row in &ds.values {
            out.push_str(&row_cells(row, &keep, complex).join(","));
            out.push('\n');
        }
    }
    if !measurements.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&measurements_section(measurements));
    }
    out
}

/// Column indexes to export: the whole row when `traces` is empty, else the
/// axis (0) plus the matching columns (case-insensitive full-name match).
fn selected_variables(ds: &RawDataset, traces: &[String]) -> Vec<usize> {
    if traces.is_empty() {
        return (0..ds.variables.len()).collect();
    }
    let mut keep = vec![0usize];
    for i in 1..ds.variables.len() {
        let name = ds.variables[i].name.as_str();
        let step_column = ds.steps.iter().any(|target| target == name);
        if step_column || traces.iter().any(|t| name.eq_ignore_ascii_case(t)) {
            keep.push(i);
        }
    }
    keep
}

/// One dataset's complex cells: any `complex` flag on the flags row.
fn is_complex(ds: &RawDataset) -> bool {
    ds.flags.iter().any(|f| f.eq_ignore_ascii_case("complex"))
}

/// The header row's cells: `<name>` or the `<name>_mag`/`<name>_phase` pair
/// for complex waveforms; the axis (slot 0) always bare.
fn header_cells(ds: &RawDataset, keep: &[usize], complex: bool) -> Vec<String> {
    let mut cells = Vec::new();
    for (slot, &i) in keep.iter().enumerate() {
        let name = ds.variables[i].name.as_str();
        if complex && slot > 0 {
            cells.push(format!("{name}_mag"));
            cells.push(format!("{name}_phase"));
        } else {
            cells.push(name.to_string());
        }
    }
    cells
}

/// One data row's cells, paired the same way the header spells them.
fn row_cells(row: &[Complex64], keep: &[usize], complex: bool) -> Vec<String> {
    let mut cells = Vec::new();
    for (slot, &i) in keep.iter().enumerate() {
        let value = row[i];
        if complex && slot > 0 {
            cells.push(format!("{:.9e}", value.norm()));
            cells.push(format!("{:.9e}", value.arg().to_degrees()));
        } else {
            cells.push(format!("{:.9e}", value.re));
        }
    }
    cells
}

/// The `.meas` table: `name,value,error` — the failed ones leave the value
/// cell empty and carry their message.
fn measurements_section(measurements: &[MeasureResult]) -> String {
    let mut out = String::from("# Measurements\nname,value,error\n");
    for m in measurements {
        let value = match &m.value {
            Some(v) => format!("{v:.9e}"),
            None => String::new(),
        };
        let error = match &m.error {
            Some(text) => csv_cell(text),
            None => String::new(),
        };
        out.push_str(&format!("{},{},{}\n", csv_cell(&m.name), value, error));
    }
    out
}

/// RFC 4180 quoting: cell text holding a comma, a quote, or a newline
/// wraps fully quoted, quotes doubled inside.
fn csv_cell(text: &str) -> String {
    if text.contains(',') || text.contains('"') || text.contains('\n') {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

const PANEL_W: u32 = 760;
const PANEL_H: u32 = 420;
const MARGIN_L: u32 = 78;
const MARGIN_R: u32 = 18;
const MARGIN_T: u32 = 34;
const MARGIN_B: u32 = 46;
const PANEL_GAP: u32 = 26;
const DIVISIONS: u32 = 5;

const TRACE_COLORS: [&str; 8] = [
    "#008000", "#0000d0", "#d00000", "#0090d0", "#d000d0", "#808000", "#206060", "#000000",
];

/// The SVG document for one built [`RawFile`]: one panel per `.raw` plot
/// shape in first-occurrence order ([`panel_groups`]), stacked vertically,
/// driven by the same trace selection as [`csv_document`] (empty list =
/// every column, the axis and the step columns always stay). Same-shape
/// plots overlay: a stepped deck draws one curve per run on shared scales,
/// each chip naming the run's steps; single-run groups keep exactly the
/// per-trace output. Complex columns draw their magnitude — the mag/phase
/// pairing is CSV material; the measurements stay tabular there too and
/// are not drawn. Axes are linear, the y scale anchored at zero, both
/// padded 5%; anything fancier (log axes) is per-plot work with its own
/// TODO entries.
pub fn svg_document(raw: &RawFile, traces: &[String]) -> String {
    svg_document_with_cursors(raw, traces, &[])
}

/// [`svg_document`] with attached cursors: each request within a panel's
/// x span (up to two, like LTspice) snaps to the nearest finite sample of
/// the panel's first curve (run 0's first kept trace) and draws as a
/// crosshair with a dot on that trace plus a coordinate readout, and two
/// accepted cursors also read out their deltas (a time Δ additionally as
/// the frequency 1/Δt). Quiet on cursorless, traceless, and off-range
/// requests.
pub fn svg_document_with_cursors(raw: &RawFile, traces: &[String], cursors: &[f64]) -> String {
    let groups = panel_groups(&raw.datasets);
    let panels = groups.len() as u32;
    let height = panels * PANEL_H + panels.saturating_sub(1) * PANEL_GAP;
    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{PANEL_W}\" height=\"{height}\" viewBox=\"0 0 {PANEL_W} {height}\" font-family=\"sans-serif\">\n"
    ));
    for (i, runs) in groups.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let top = i as u32 * (PANEL_H + PANEL_GAP);
        let keep = selected_variables(runs[0], traces);
        svg_panel(&mut out, runs, top, &keep, is_complex(runs[0]), cursors);
    }
    out.push_str("</svg>\n");
    out
}

/// Partition a file's datasets into overlay groups in first-occurrence
/// order: each dataset joins the first earlier group whose shape matches
/// ([`same_shape`]), so a `build()` that interleaves analyses across step
/// runs (Trans, AC, Trans, AC, …) still lands each analysis in one panel.
fn panel_groups(datasets: &[RawDataset]) -> Vec<Vec<&RawDataset>> {
    let mut groups: Vec<Vec<&RawDataset>> = Vec::new();
    for ds in datasets {
        match groups.iter_mut().find(|group| same_shape(group[0], ds)) {
            Some(group) => group.push(ds),
            None => groups.push(vec![ds]),
        }
    }
    groups
}

/// Same overlay shape: the plot name, the flag row, and every variable's
/// name and kind line up pairwise.
fn same_shape(a: &RawDataset, b: &RawDataset) -> bool {
    a.plotname == b.plotname
        && a.flags == b.flags
        && a.variables.len() == b.variables.len()
        && a.variables
            .iter()
            .zip(&b.variables)
            .all(|(x, y)| x.name == y.name && x.kind == y.kind)
}

/// One panel: a `<g>` translated to its stack slot, holding the plotname,
/// the legend chips, a rectangular frame, gray gridlines, and one polyline
/// per curve. A stepped group (`runs` long, same shapes) draws one curve
/// per (run, selected trace) on shared scales, chips naming each run's
/// steps; a single-run group keeps one curve per trace with the plain
/// chips. Step columns ride along as identification metadata, not curves.
/// Single-row plots (`.op`, `.tf`, `.noise` synthetic axes) draw their
/// sample as a small circle, since a one-point polyline would render
/// nothing. Attached cursors in-range snap to the first curve (run 0's
/// first kept trace) and crosshair it.
fn svg_panel(
    out: &mut String,
    runs: &[&RawDataset],
    top: u32,
    keep: &[usize],
    complex: bool,
    cursors: &[f64],
) {
    let head = runs[0];
    out.push_str(&format!("<g transform=\"translate(0,{top})\">\n"));
    out.push_str(&format!(
        "<text x=\"{MARGIN_L}\" y=\"20\" font-size=\"13\">{}</text>\n",
        xml_escape(&head.plotname)
    ));
    let pw = PANEL_W - MARGIN_L - MARGIN_R;
    let ph = PANEL_H - MARGIN_T - MARGIN_B;
    let plot_w = pw as f64;
    let plot_h = ph as f64;
    let left = MARGIN_L as f64;
    let right = (MARGIN_L + pw) as f64;
    let bottom = (MARGIN_T + ph) as f64;

    // One curve per (run, kept trace): step columns are the runs'
    // identification metadata and draw nothing themselves. The y scale
    // spans every curve of the overlay (zero anchors it, both ends pad 5%,
    // flat data widens ±1 first); the x scale spans every run's axis
    // samples, both ends padded 5% (a degenerate axis — one sample, as in
    // synthetic `.op` rows — widens to [a, a+1] to stay degenerate-safe).
    let mut layers: Vec<(usize, usize)> = Vec::new();
    for (run, ds) in runs.iter().enumerate() {
        for &i in keep.iter().skip(1) {
            let name = &ds.variables[i].name;
            if ds.steps.iter().any(|target| target == name) {
                continue;
            }
            layers.push((run, i));
        }
    }
    let columns: Vec<Vec<f64>> = layers
        .iter()
        .map(|&(run, i)| {
            runs[run]
                .values
                .iter()
                .filter_map(|row| {
                    let cell = row[i];
                    let value = if complex { cell.norm() } else { cell.re };
                    if value.is_finite() {
                        Some(value)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .collect();
    let (min_y, max_y) = columns
        .iter()
        .flatten()
        .copied()
        .chain([0.0])
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v), hi.max(v))
        });
    let (min_y, max_y) = if min_y == max_y {
        (min_y - 1.0, max_y + 1.0)
    } else {
        let pad = 0.05 * (max_y - min_y);
        (min_y - pad, max_y + pad)
    };
    let axis: Vec<f64> = runs
        .iter()
        .flat_map(|ds| ds.values.iter().map(|row| row[0].re))
        .collect();
    let (min_x, max_x) = axis
        .iter()
        .copied()
        .filter(|x| x.is_finite())
        .chain([0.0])
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v), hi.max(v))
        });
    let (min_x, max_x) = if min_x == max_x {
        (min_x, max_x + 1.0)
    } else {
        let pad = 0.05 * (max_x - min_x);
        (min_x - pad, max_x + pad)
    };
    let x_at = |value: f64| left + (value - min_x) / (max_x - min_x) * plot_w;
    let y_at = |value: f64| bottom - (value - min_y) / (max_y - min_y) * plot_h;

    for i in 1..DIVISIONS {
        let x = left + i as f64 / DIVISIONS as f64 * plot_w;
        let y = MARGIN_T as f64 + i as f64 / DIVISIONS as f64 * plot_h;
        out.push_str(&format!(
            "<line x1=\"{x:.2}\" y1=\"{}\" x2=\"{x:.2}\" y2=\"{bottom:.2}\" stroke=\"#999999\"/>\n",
            MARGIN_T
        ));
        out.push_str(&format!(
            "<line x1=\"{left:.2}\" y1=\"{y:.2}\" x2=\"{right:.2}\" y2=\"{y:.2}\" stroke=\"#999999\"/>\n"
        ));
    }
    out.push_str(&format!(
        "<rect x=\"{MARGIN_L}\" y=\"{MARGIN_T}\" width=\"{pw}\" height=\"{ph}\" fill=\"none\" stroke=\"#000000\"/>\n"
    ));
    for i in 0..=DIVISIONS {
        let x = x_at(min_x + (max_x - min_x) * i as f64 / DIVISIONS as f64);
        let y = bottom - i as f64 / DIVISIONS as f64 * plot_h;
        out.push_str(&format!(
            "<line x1=\"{x:.2}\" y1=\"{bottom:.2}\" x2=\"{x:.2}\" y2=\"{:.2}\"/>\n",
            bottom + 4.0
        ));
        out.push_str(&format!(
            "<text x=\"{x:.2}\" y=\"{:.2}\" text-anchor=\"middle\" font-size=\"11\">{}</text>\n",
            bottom + 18.0,
            tick_label(min_x + (max_x - min_x) * i as f64 / DIVISIONS as f64)
        ));
        out.push_str(&format!(
            "<line x1=\"{:.2}\" y1=\"{y:.2}\" x2=\"{left:.2}\" y2=\"{y:.2}\"/>\n",
            left - 4.0
        ));
        out.push_str(&format!(
            "<text x=\"{:.2}\" y=\"{:.2}\" text-anchor=\"end\" font-size=\"11\">{}</text>\n",
            left - 8.0,
            y + 4.0,
            tick_label(min_y + (max_y - min_y) * i as f64 / DIVISIONS as f64)
        ));
    }

    for (slot, &(run, i)) in layers.iter().enumerate() {
        let ds = runs[run];
        let name = &ds.variables[i].name;
        let column = &columns[slot];
        let color = TRACE_COLORS[slot % TRACE_COLORS.len()];
        let chip = if runs.len() == 1 {
            name.clone()
        } else {
            run_tag(ds, run, name)
        };
        out.push_str(&format!(
            "<text x=\"{right:.2}\" y=\"{}\" text-anchor=\"end\" fill=\"{color}\">{}</text>\n",
            14.0 + 14.0 * slot as f64,
            xml_escape(&chip)
        ));
        let pairs: Vec<(f64, f64)> = ds
            .values
            .iter()
            .map(|row| row[0].re)
            .zip(column.iter().copied())
            .filter(|(x, y)| x.is_finite() && y.is_finite())
            .map(|(x, y)| (x_at(x), y_at(y)))
            .collect();
        let points = pairs
            .iter()
            .map(|(px, py)| format!("{px:.2},{py:.2}"))
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!(
            "<polyline points=\"{points}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"2\"/>\n"
        ));
        if pairs.len() == 1 {
            out.push_str(&format!(
                "<circle cx=\"{:.2}\" cy=\"{:.2}\" r=\"3\" fill=\"{color}\"/>\n",
                pairs[0].0, pairs[0].1
            ));
        }
    }

    // Attached cursors (LTspice): up to two requests inside the padded x
    // span snap to the nearest finite sample of the first curve — run 0's
    // first kept trace — and crosshair it in magenta with a dot at the
    // snapped point; the readouts list each cursor's coordinates, and with
    // two accepted cursors also the deltas (a time Δ additionally as 1/Δt
    // in Hz).
    if !layers.is_empty() && !cursors.is_empty() {
        let (first_run, first) = layers[0];
        let first_pairs: Vec<(f64, f64)> = runs[first_run]
            .values
            .iter()
            .filter_map(|row| {
                let cell = row[first];
                let y = if complex { cell.norm() } else { cell.re };
                (row[0].re.is_finite() && y.is_finite()).then_some((row[0].re, y))
            })
            .collect();
        if !first_pairs.is_empty() {
            let snaps: Vec<(f64, f64)> = cursors
                .iter()
                .filter(|x| (min_x..=max_x).contains(x))
                .filter_map(|&x| nearest_sample(&first_pairs, x))
                .take(2)
                .collect();
            for (sx, sy) in &snaps {
                let (cx, cy) = (x_at(*sx), y_at(*sy));
                out.push_str(&format!(
                    "<line x1=\"{cx:.2}\" y1=\"{MARGIN_T}\" x2=\"{cx:.2}\" y2=\"{bottom:.2}\" stroke=\"#aa00aa\"/>\n"
                ));
                out.push_str(&format!(
                    "<circle cx=\"{cx:.2}\" cy=\"{cy:.2}\" r=\"3\" fill=\"#aa00aa\"/>\n"
                ));
            }
            let axis_name = &runs[first_run].variables[0].name;
            let trace_name = &runs[first_run].variables[first].name;
            let mut ry = MARGIN_T as f64 + 16.0;
            for (n, (sx, sy)) in snaps.iter().enumerate() {
                out.push_str(&format!(
                    "<text x=\"{:.2}\" y=\"{ry:.2}\" font-size=\"11\" fill=\"#aa00aa\">{}: {}={}, {}={}</text>\n",
                    left + 8.0,
                    n + 1,
                    xml_escape(axis_name),
                    tick_label(*sx),
                    xml_escape(trace_name),
                    tick_label(*sy)
                ));
                ry += 14.0;
            }
            if let [sa, sb] = snaps[..] {
                let (dx, dy) = (sb.0 - sa.0, sb.1 - sa.1);
                out.push_str(&format!(
                    "<text x=\"{:.2}\" y=\"{ry:.2}\" font-size=\"11\" fill=\"#aa00aa\">Δ{}={}, Δ{}={}</text>\n",
                    left + 8.0,
                    xml_escape(axis_name),
                    tick_label(dx),
                    xml_escape(trace_name),
                    tick_label(dy)
                ));
                ry += 14.0;
                if axis_name.eq_ignore_ascii_case("time") && dx != 0.0 {
                    out.push_str(&format!(
                        "<text x=\"{:.2}\" y=\"{ry:.2}\" font-size=\"11\" fill=\"#aa00aa\">f = 1/Δ{} = {}</text>\n",
                        left + 8.0,
                        xml_escape(axis_name),
                        tick_label((1.0 / dx).abs())
                    ));
                }
            }
        }
    }
    out.push_str("</g>\n");
}

/// The per-curve chip text naming which run a curve belongs to: the step
/// targets spelled with their column values when the dataset carries step
/// columns (readers recover none of those names, so their chips fall back
/// to the run ordinal), else the run ordinal. Single-run groups never
/// reach this and keep the bare trace name.
fn run_tag(ds: &RawDataset, run: usize, name: &str) -> String {
    if ds.steps.is_empty() {
        return format!("{name} run {}", run + 1);
    }
    let spelled: Vec<String> = ds
        .steps
        .iter()
        .map(|target| {
            let value = ds.values.first().and_then(|row| {
                let index = ds.variables.iter().position(|v| &v.name == target)?;
                Some(row[index].re)
            });
            match value {
                Some(v) => format!("{target}={}", tick_label(v)),
                None => target.clone(),
            }
        })
        .collect();
    format!("{name}: {}", spelled.join(", "))
}

/// The nearest finite `(axis, value)` pair to `x` by axis distance: the
/// snap point an attached cursor rides on.
fn nearest_sample(pairs: &[(f64, f64)], x: f64) -> Option<(f64, f64)> {
    pairs.iter().copied().min_by(|(a, _), (b, _)| {
        let da = (a - x).abs();
        let db = (b - x).abs();
        da.total_cmp(&db)
    })
}

/// A terse tick spelling: plain decimals where they read well, two-digit
/// scientific beyond them (`1.00e-6`).
fn tick_label(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let magnitude = value.abs();
    if !(1e-3..1e4).contains(&magnitude) {
        return format!("{value:.2e}");
    }
    let spelled = format!("{value:.4}");
    spelled
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

/// SVG text nodes escape the XML specials; the markup this module builds
/// never interpolates text into an attribute value.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::netlist::parse;
    use crate::rawwave::{build, RawVariable};
    use crate::runs::run;
    use approx::assert_relative_eq;

    fn sim(deck: &str) -> (RawFile, Vec<MeasureResult>) {
        let netlist = parse(deck).expect("deck parses");
        let result = run(&netlist).expect("deck runs");
        let raw = build("test title", &netlist, &result);
        let measurements = result.measurements.clone();
        (raw, measurements)
    }

    /// Trace names for a `csv_document` call, lifted to the arguments' type.
    fn tr(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    const TRAN_DECK: &str = "\
V1 in 0 dc 5
R1 in out 1k
R2 out 0 3k
.tran 10u 1m
.meas tran vmax max V(out)
.meas tran vavg avg V(out)
.end
";

    const AC_DECK: &str = "V1 in 0 ac 1\nR1 in out 1k\nC1 out 0 1u\n\
         .ac dec 2 1k 10k\n.end\n";

    const STEPPED: &str = ".param r=1k\n\
         V1 in 0 5\n\
         R1 in out 1k\n\
         R2 out 0 {r}\n\
         .step param r 1k 2k 1k\n\
         .op\n.end\n";

    /// The transient plot exports as one section, and the `.meas` table
    /// appends after a blank line. Numeric cells spill at full precision;
    /// failed values are not exercised here (unit-tested below).
    #[test]
    fn transient_section_and_measurement_table() {
        let (raw, measurements) = sim(TRAN_DECK);
        let doc = csv_document(&raw, &measurements, &tr(&["V(in)", "V(out)"]));
        let parts = doc.split_once("# Measurements\n").expect("meas section");
        assert!(parts.0.ends_with("\n\n"), "blank line between sections");
        let rows: Vec<&str> = parts.0.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(rows[0], "# Transient Analysis");
        assert_eq!(rows[1], "time,V(in),V(out)");
        for row in &rows[2..] {
            assert_eq!(row.split(',').count(), 3, "row {row:?}");
        }
        assert_eq!(rows[2], "0.000000000e0,5.000000000e0,3.750000000e0");
        let mut table = parts.1.lines();
        assert_eq!(table.next(), Some("name,value,error"));
        assert_eq!(table.next(), Some("vmax,3.750000000e0,"));
        assert_eq!(table.next(), Some("vavg,3.750000000e0,"));
        assert_eq!(table.next(), None);
    }

    /// An empty trace list exports every column: the nodes plus every
    /// element family's current trace.
    #[test]
    fn empty_traces_export_every_column() {
        let (raw, measurements) = sim(TRAN_DECK);
        let doc = csv_document(&raw, &measurements, &[]);
        let header = doc.lines().nth(1).expect("header row");
        assert_eq!(header, "time,V(0),V(in),V(out),I(R1),I(R2),I(V1)");
    }

    /// Trace selection keeps the axis and the step columns even when only
    /// the named waveforms were asked for; matches are case-insensitive.
    #[test]
    fn trace_filter_keeps_axis_and_step_columns() {
        let (raw, measurements) = sim(STEPPED);
        let doc = csv_document(&raw, &measurements, &tr(&["v(out)", "I(R1)"]));
        assert_eq!(doc.matches("# Operating Point").count(), 2);
        let rows: Vec<&str> = doc.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(rows.len(), 6, "all rows: {rows:?}");
        assert_eq!(rows[0], "# Operating Point");
        assert_eq!(rows[1], "time,V(out),I(R1),r");
        assert!(rows.iter().all(|row| !row.contains("V(in)")));
        assert_eq!(rows[3], "# Operating Point");
        assert_eq!(rows[4], rows[1]);
        let read = |row: &str| -> Vec<f64> {
            row.split(',')
                .map(|cell| cell.parse().expect("numeric cell"))
                .collect()
        };
        let first = read(rows[2]);
        let second = read(rows[5]);
        assert_relative_eq!(first[0], 0.0, epsilon = 1e-12);
        // R2 = r: the divider ends 2.5 and 5/3, its branch currents follow.
        assert_relative_eq!(first[1], 2.5, epsilon = 1e-9);
        assert_relative_eq!(first[2], 2.5e-3, epsilon = 1e-12);
        assert_relative_eq!(second[1], 5.0 * 2.0 / 3.0, epsilon = 1e-9);
        assert_relative_eq!(second[2], (5.0 - 5.0 * 2.0 / 3.0) / 1e3, epsilon = 1e-12);
        // Run provenance rides as the last cell, spelled at full precision.
        assert_eq!(
            rows[2].rsplit_once(',').map(|(_, cell)| cell),
            Some("1.000000000e3")
        );
        assert_eq!(
            rows[5].rsplit_once(',').map(|(_, cell)| cell),
            Some("2.000000000e3")
        );
    }

    /// Complex plots ride as `_mag`/`_phase` pairs in degrees while the
    /// axis column keeps printing its real part.
    #[test]
    fn ac_complex_traces_pair_mag_and_phase() {
        let (raw, measurements) = sim(AC_DECK);
        let doc = csv_document(&raw, &measurements, &tr(&["V(out)"]));
        let rows: Vec<&str> = doc.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(rows[0], "# AC Analysis");
        assert_eq!(rows[1], "freq,V(out)_mag,V(out)_phase");
        let cells: Vec<f64> = rows[2]
            .split(',')
            .map(|cell| cell.parse().expect("numeric cell"))
            .collect();
        assert_relative_eq!(cells[0], 1000.0, epsilon = 1e-6);
        // V(out) = 1/(1 + jRCω) with RC = 1 ms at 1 kHz.
        let w = 2.0 * std::f64::consts::PI * cells[0];
        assert_relative_eq!(cells[1], 1.0 / f64::hypot(1.0, w * 1e-3), epsilon = 1e-6);
        assert_relative_eq!(cells[2], -(w * 1e-3).atan().to_degrees(), epsilon = 1e-6);
    }

    /// No `.meas` cards → no measurement section at all.
    #[test]
    fn unmeasured_deck_has_no_measurement_section() {
        const PLAIN: &str = "V1 in 0 dc 5\n\
             R1 in out 1k\n\
             R2 out 0 3k\n\
             .tran 10u 1m\n\
             .end\n";
        let (raw, measurements) = sim(PLAIN);
        let doc = csv_document(&raw, &measurements, &[]);
        assert!(doc.starts_with("# Transient Analysis\n"));
        assert!(!doc.contains("# Measurements"));
    }

    /// The table itself: failed measurements leave their value cell empty
    /// and take RFC 4180 quoting for the text cells, quotes doubled inside.
    #[test]
    fn measurement_table_quotes_text_cells() {
        let measures = vec![
            MeasureResult {
                name: "a,b\"c".to_owned(),
                value: Some(std::f64::consts::PI),
                error: None,
            },
            MeasureResult {
                name: "say".to_owned(),
                value: None,
                error: Some("no \"cross\" yet".to_owned()),
            },
        ];
        let section = measurements_section(&measures);
        let mut lines = section.lines();
        assert_eq!(lines.next(), Some("# Measurements"));
        assert_eq!(lines.next(), Some("name,value,error"));
        assert_eq!(lines.next(), Some("\"a,b\"\"c\",3.141592654e0,"));
        assert_eq!(lines.next(), Some("say,,\"no \"\"cross\"\" yet\""));
        assert_eq!(lines.next(), None);
    }

    /// One panel per plot: the head names the canvas, the panel names the
    /// plot, and every selected column draws a polyline; the flat ground
    /// trace is horizontal while the measurements stay CSV material.
    #[test]
    fn svg_document_renders_panels_per_plot() {
        let (raw, _) = sim(TRAN_DECK);
        let doc = svg_document(&raw, &[]);
        assert!(
            doc.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""),
            "opening tag: {doc}"
        );
        assert!(doc.ends_with("</svg>\n"));
        assert!(doc.contains(r#"width="760" height="420""#));
        assert_eq!(doc.matches("<polyline").count(), 6);
        assert!(doc.contains(">Transient Analysis</text>"));
        assert!(doc.contains(">V(in)</text>"));
        let first = doc.split_once("<polyline points=\"").expect("polyline").1;
        let points = &first[..first.find('"').expect("points attribute length")];
        let ys: std::collections::HashSet<&str> = points
            .split(' ')
            .map(|pair| pair.split_once(',').expect("x,y pair").1)
            .collect();
        assert_eq!(ys.len(), 1, "flat ground is horizontal: {points}");
        assert!(!doc.contains("vmax"));
    }

    /// Stepped runs overlay onto one panel: one curve per run on shared
    /// scales, chips naming each run's step value, and the step column
    /// riding as metadata (no `>r</text>` chip of its own).
    #[test]
    fn svg_document_overlays_stepped_runs() {
        let (raw, _) = sim(STEPPED);
        let doc = svg_document(&raw, &tr(&["V(out)"]));
        assert!(doc.contains(r#"width="760" height="420""#), "{doc}");
        assert_eq!(doc.matches("translate(0,").count(), 1);
        assert_eq!(doc.matches("<polyline").count(), 2, "{doc}");
        assert_eq!(doc.matches("<circle").count(), 2, "one-point traces");
        assert!(doc.contains(">V(out): r=1000</text>"), "{doc}");
        assert!(doc.contains(">V(out): r=2000</text>"), "{doc}");
        assert!(!doc.contains(">r</text>"));
    }

    /// Complex (AC) panels draw magnitudes only, labeled by the bare column
    /// name — the mag/phase pairing is CSV material.
    #[test]
    fn svg_complex_columns_draw_magnitude() {
        let (raw, _) = sim(AC_DECK);
        let doc = svg_document(&raw, &tr(&["V(out)"]));
        assert_eq!(doc.matches("<polyline").count(), 1);
        assert!(doc.contains(">V(out)</text>"));
        assert!(!doc.contains("_mag"));
    }

    /// The tick spelling keeps plain decimals terse and goes two-digit
    /// scientific beyond the readable band.
    #[test]
    fn tick_labels_are_terse() {
        assert_eq!(tick_label(0.0), "0");
        assert_eq!(tick_label(2.5), "2.5");
        assert_eq!(tick_label(-0.003), "-0.003");
        assert_eq!(tick_label(1250.0), "1250");
        assert_eq!(tick_label(1e-6), "1.00e-6");
        assert_eq!(tick_label(5e4), "5.00e4");
    }

    /// Text nodes escape the XML specials; plain text passes through.
    #[test]
    fn svg_text_is_xml_escaped() {
        assert_eq!(xml_escape("a&b<c>d"), "a&amp;b&lt;c&gt;d");
        assert_eq!(xml_escape("plain"), "plain");
    }

    /// A tiny real transient plot: time 0..3 s with V(out) at 1, 2, 5, 10
    /// — enough structure for cursor snaps and delta readouts.
    fn ramp_raw() -> RawFile {
        let variable = |name: &str| RawVariable {
            name: name.to_owned(),
            kind: String::new(),
        };
        let row = |(t, v)| vec![Complex64::new(t, 0.0), Complex64::new(v, 0.0)];
        RawFile {
            datasets: vec![RawDataset {
                title: "test".to_owned(),
                plotname: "Transient Analysis".to_owned(),
                flags: vec!["real".to_owned()],
                variables: vec![variable("time"), variable("V(out)")],
                values: vec![
                    row((0.0, 1.0)),
                    row((1.0, 2.0)),
                    row((2.0, 5.0)),
                    row((3.0, 10.0)),
                ],
                steps: Vec::new(),
            }],
        }
    }

    /// A tiny complex (AC) plot: freq 1k/2k against complex V(out).
    fn complex_raw() -> RawFile {
        let variable = |name: &str| RawVariable {
            name: name.to_owned(),
            kind: String::new(),
        };
        RawFile {
            datasets: vec![RawDataset {
                title: "test".to_owned(),
                plotname: "AC Analysis".to_owned(),
                flags: vec![
                    "real".to_owned(),
                    "forward".to_owned(),
                    "complex".to_owned(),
                ],
                variables: vec![variable("freq"), variable("V(out)")],
                values: vec![
                    vec![Complex64::new(1000.0, 0.0), Complex64::new(3.0, 4.0)],
                    vec![Complex64::new(2000.0, 0.0), Complex64::new(0.0, 6.0)],
                ],
                steps: Vec::new(),
            }],
        }
    }

    /// A single in-range cursor snaps to the nearest sample of the first
    /// kept trace, crosshairs it, and reads out the snapped coordinates.
    #[test]
    fn cursor_snaps_and_reads_out_coordinates() {
        let doc = svg_document_with_cursors(&ramp_raw(), &tr(&["V(out)"]), &[0.6]);
        assert_eq!(doc.matches("stroke=\"#aa00aa\"").count(), 1, "{doc}");
        assert_eq!(doc.matches("r=\"3\" fill=\"#aa00aa\"").count(), 1, "{doc}");
        assert!(doc.contains(">1: time=1, V(out)=2</text>"), "{doc}");
    }

    /// Two cursors read out their deltas; on a time axis the Δt also
    /// reads out as the frequency 1/Δt.
    #[test]
    fn two_cursors_read_out_deltas_and_frequency() {
        let doc = svg_document_with_cursors(&ramp_raw(), &tr(&["V(out)"]), &[0.6, 2.4]);
        assert!(doc.contains(">1: time=1, V(out)=2</text>"), "{doc}");
        assert!(doc.contains(">2: time=2, V(out)=5</text>"), "{doc}");
        assert!(doc.contains(">Δtime=1, ΔV(out)=3</text>"), "{doc}");
        assert!(doc.contains(">f = 1/Δtime = 1</text>"), "{doc}");
    }

    /// Three requests still crosshair two per panel (LTspice's limit),
    /// snapped in input order, with the two-cursor delta readout intact.
    #[test]
    fn cursors_cap_at_two_per_panel() {
        let doc = svg_document_with_cursors(&ramp_raw(), &[], &[0.6, 2.4, 2.9]);
        assert_eq!(doc.matches("r=\"3\" fill=\"#aa00aa\"").count(), 2, "{doc}");
        assert!(doc.contains(">Δtime=1, ΔV(out)=3</text>"), "{doc}");
    }

    /// Requests outside the x span, cursorless calls, and a panel with no
    /// kept trace all stay silent: no crosshair color appears.
    #[test]
    fn off_range_and_traceless_calls_stay_silent() {
        let raw = ramp_raw();
        assert!(!svg_document_with_cursors(&raw, &[], &[10.0]).contains("#aa00aa"));
        assert!(!svg_document_with_cursors(&raw, &tr(&["I(V1)"]), &[0.6]).contains("#aa00aa"));
        assert!(!svg_document_with_cursors(&raw, &[], &[0.6, 10.0]).contains('Δ'));
    }

    /// An empty cursor list is exactly the plain [`svg_document`].
    #[test]
    fn empty_cursors_equal_the_plain_document() {
        let raw = ramp_raw();
        assert_eq!(
            svg_document(&raw, &[]),
            svg_document_with_cursors(&raw, &[], &[])
        );
    }

    /// Complex panels snap like real ones and read out the magnitude the
    /// polyline drew; a non-time axis carries no 1/Δt line even with two
    /// cursors.
    #[test]
    fn complex_panel_cursor_reads_the_magnitude() {
        let doc = svg_document_with_cursors(&complex_raw(), &tr(&["V(out)"]), &[1200.0, 1800.0]);
        assert!(doc.contains(">1: freq=1000, V(out)=5</text>"), "{doc}");
        assert!(doc.contains(">2: freq=2000, V(out)=6</text>"), "{doc}");
        assert!(doc.contains(">Δfreq=1000, ΔV(out)=1</text>"), "{doc}");
        assert!(!doc.contains("f = 1/Δ"), "{doc}");
    }

    /// Two same-shape transient runs; `named` toggles between the
    /// `.step`-built form (the `r` step column registered in `steps`) and
    /// the reader-built form (column present, names unrecoverable). Run
    /// two doubles run one everywhere.
    fn two_run_raw(named: bool) -> RawFile {
        let variable = |name: &str| RawVariable {
            name: name.to_owned(),
            kind: String::new(),
        };
        let row = |(t, v, r)| {
            vec![
                Complex64::new(t, 0.0),
                Complex64::new(v, 0.0),
                Complex64::new(r, 0.0),
            ]
        };
        let run = |values: &[(f64, f64, f64)]| RawDataset {
            title: "test".to_owned(),
            plotname: "Transient Analysis".to_owned(),
            flags: vec!["real".to_owned()],
            variables: vec![variable("time"), variable("V(out)"), variable("r")],
            values: values.iter().copied().map(row).collect(),
            steps: if named {
                vec!["r".to_owned()]
            } else {
                Vec::new()
            },
        };
        RawFile {
            datasets: vec![
                run(&[(0.0, 1.0, 1000.0), (1.0, 2.0, 1000.0), (2.0, 3.0, 1000.0)]),
                run(&[(0.0, 2.0, 2000.0), (1.0, 4.0, 2000.0), (2.0, 6.0, 2000.0)]),
            ],
        }
    }

    /// A two-run stepped transient overlays one panel: one curve per run,
    /// step-value chips, per-curve colors, and shared cross-run scaling —
    /// the doubled run stretches the scale, so the first run's own max
    /// lands mid-panel instead of at the top.
    #[test]
    fn stepped_transient_overlays_runs_on_one_panel() {
        let doc = svg_document(&two_run_raw(true), &tr(&["V(out)"]));
        assert!(doc.contains(r#"width="760" height="420""#), "{doc}");
        assert_eq!(doc.matches("translate(0,").count(), 1, "{doc}");
        assert_eq!(doc.matches("<polyline").count(), 2, "{doc}");
        assert!(doc.contains(">V(out): r=1000</text>"), "{doc}");
        assert!(doc.contains(">V(out): r=2000</text>"), "{doc}");
        assert!(doc.contains("stroke=\"#008000\""), "{doc}");
        assert!(doc.contains("stroke=\"#0000d0\""), "{doc}");
        // Shared y scale: values reach 6, both ends pad 5% → the top tick
        // is 6.3; a first-run-only scale would top out at 3.15. And run
        // one's max (3) maps to exact mid-panel y under that shared scale.
        assert!(doc.contains(">6.3</text>"), "{doc}");
        let green = doc
            .lines()
            .find(|line| line.contains("stroke=\"#008000\""))
            .expect("green polyline");
        assert!(green.contains(",204.00"), "{green}");
    }

    /// Real transient axes live in seconds: a sub-1.0 span (here 20 µs)
    /// must spread the full plot width — clamping the x span to [0, 1]
    /// would crush every time-domain trace onto the left edge.
    #[test]
    fn sub_unit_axis_spreads_the_plot() {
        let mut raw = two_run_raw(true);
        for ds in raw.datasets.iter_mut() {
            for row in ds.values.iter_mut() {
                row[0] *= 2e-5 / 2.0;
            }
        }
        let doc = svg_document(&raw, &tr(&["V(out)"]));
        assert!(doc.contains("410.00,255.52"), "{doc}");
        assert!(doc.contains("711.82,49.45"), "{doc}");
    }

    /// Reader-built runs carry no `steps`; the runs still overlay, with
    /// the chips falling back to per-run ordinals.
    #[test]
    fn reader_runs_without_steps_get_ordinal_chips() {
        let doc = svg_document(&two_run_raw(false), &tr(&["V(out)"]));
        assert_eq!(doc.matches("<polyline").count(), 2);
        assert!(doc.contains(">V(out) run 1</text>"), "{doc}");
        assert!(doc.contains(">V(out) run 2</text>"), "{doc}");
        assert!(!doc.contains(">V(out)</text>"));
    }

    /// Overlay grouping is by plot shape: the same shape merges even when
    /// other plots interleave between the runs, and a different variable
    /// row or plot name splits the stack again.
    #[test]
    fn mismatched_shapes_keep_separate_panels() {
        let variable = |name: &str| RawVariable {
            name: name.to_owned(),
            kind: String::new(),
        };
        let ds = |plotname: &str, names: &[&str]| RawDataset {
            title: "test".to_owned(),
            plotname: plotname.to_owned(),
            flags: vec!["real".to_owned()],
            variables: names.iter().map(|&name| variable(name)).collect(),
            values: vec![names.iter().map(|_| Complex64::new(0.0, 0.0)).collect()],
            steps: Vec::new(),
        };
        let raw = RawFile {
            datasets: vec![
                ds("Transient Analysis", &["time", "V(out)"]),
                ds("AC Analysis", &["frequency", "V(out)"]),
                ds("Transient Analysis", &["time", "V(out)", "V(hi)"]),
                ds("Transient Analysis", &["time", "V(out)"]),
            ],
        };
        let doc = svg_document(&raw, &tr(&["V(out)"]));
        assert_eq!(doc.matches("translate(0,").count(), 3, "{doc}");
        assert!(doc.contains("translate(0,446)"), "{doc}");
        assert!(doc.contains("translate(0,892)"), "{doc}");
    }

    /// On a file that carries step columns, the cursor snaps to the first
    /// non-step trace (V(out), not the `r` column), and the step value
    /// never surfaces in this panel.
    #[test]
    fn cursor_on_stepped_snaps_to_first_non_step_trace() {
        let variable = |name: &str| RawVariable {
            name: name.to_owned(),
            kind: String::new(),
        };
        let step_variable = RawVariable {
            name: "r".to_owned(),
            kind: "parameter".to_owned(),
        };
        let row = |(t, v)| {
            vec![
                Complex64::new(t, 0.0),
                Complex64::new(v, 0.0),
                Complex64::new(1000.0, 0.0),
            ]
        };
        let raw = RawFile {
            datasets: vec![RawDataset {
                title: "test".to_owned(),
                plotname: "Transient Analysis".to_owned(),
                flags: vec!["real".to_owned()],
                variables: vec![variable("time"), variable("V(out)"), step_variable],
                values: vec![
                    row((0.0, 1.0)),
                    row((1.0, 2.0)),
                    row((2.0, 5.0)),
                    row((3.0, 10.0)),
                ],
                steps: vec!["r".to_owned()],
            }],
        };
        let doc = svg_document_with_cursors(&raw, &[], &[0.6]);
        assert_eq!(doc.matches("<polyline").count(), 1, "{doc}");
        assert!(doc.contains(">1: time=1, V(out)=2</text>"), "{doc}");
        assert!(!doc.contains("1000"), "{doc}");
    }
}
