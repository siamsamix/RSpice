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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::netlist::parse;
    use crate::rawwave::build;
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
        const STEPPED: &str = ".param r=1k\n\
             V1 in 0 5\n\
             R1 in out 1k\n\
             R2 out 0 {r}\n\
             .step param r 1k 2k 1k\n\
             .op\n.end\n";
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
        const AC_DECK: &str = "V1 in 0 ac 1\nR1 in out 1k\nC1 out 0 1u\n\
             .ac dec 2 1k 10k\n.end\n";
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
}
