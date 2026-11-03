//! Waveform comparison against a reference `.raw` file (TODO §11 benchmark
//! harness): "fast" is only worth claiming next to "as accurate as LTspice",
//! so this turns two [`RawFile`]s — typically an LTspice run read through
//! [`crate::rawwave::read`] and RSpice's own [`crate::rawwave::build`] of the
//! same deck — into per-trace error figures.
//!
//! Method, element for element:
//!
//! * **Plots** pair by `Plotname:` (case-insensitive), the k-th occurrence of
//!   a name with the k-th occurrence in the other file, so stepped files
//!   (self-headered plot per run) line up run for run.
//! * **Traces** pair by variable name (case-insensitive). Variable 0 is each
//!   plot's axis and is never compared; RSpice's synthetic step columns are
//!   skipped. Traces only the reference has are listed, not failed — LTspice
//!   writes device-internal currents RSpice spells differently or lacks.
//! * **Sampling** — the two tools pick different timesteps, so every
//!   *reference* point is compared against RSpice's trace linearly
//!   interpolated at the same axis value (exact when the grids coincide, as
//!   in `.ac`/`.dc`). Reference points outside RSpice's axis span are counted
//!   as `skipped`, not compared. A reference time axis is read as `|t|`:
//!   LTspice flags some stored points with negative timestamps.
//! * **Single-point plots** (`.op`; LTspice's file has no axis variable at
//!   all, RSpice keeps a synthetic `time = 0`) compare every shared name.
//! * **Error** per trace: `max_abs` and `rms` of `|reference − RSpice|`
//!   (complex magnitude for `.ac`), and `max_norm = max_abs / peak`, where
//!   `peak` is the reference trace's own peak magnitude — a scale-free figure
//!   that stays meaningful through zero crossings. LTspice stores real
//!   values as `f32`, so a faithful match shows `max_norm ≈ 1e-7..1e-6` at
//!   best.
//!
//! Documented interpretation: no pass/fail threshold is specified by either
//! reference tool; callers choose one (the CLI's `--compare-tol`).

use crate::rawwave::{RawDataset, RawFile};
use num_complex::Complex64;
use std::fmt::Write as _;

/// Error figures of one reference trace against RSpice's.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceDelta {
    /// Plot spelling, suffixed ` #k` from the second occurrence on (stepped).
    pub plot: String,
    pub trace: String,
    /// Reference points compared.
    pub points: usize,
    /// Reference points outside RSpice's axis span (not compared).
    pub skipped: usize,
    pub max_abs: f64,
    pub rms: f64,
    /// Peak magnitude of the reference trace over the compared points.
    pub peak: f64,
    /// `max_abs / peak` (`0` when both are zero, `∞` when only the peak is).
    pub max_norm: f64,
}

/// Everything [`compare`] found.
#[derive(Debug, Clone, Default)]
pub struct RawComparison {
    pub traces: Vec<TraceDelta>,
    /// `plot: trace` the reference has and RSpice lacks (informational).
    pub missing_traces: Vec<String>,
    /// Reference plots with no comparable counterpart (fails the match).
    pub missing_plots: Vec<String>,
    /// RSpice plots the reference lacks (informational: RSpice also writes an
    /// operating point next to a `.tran`, LTspice does not).
    pub extra_plots: Vec<String>,
}

impl RawComparison {
    /// The trace with the largest normalised error.
    pub fn worst(&self) -> Option<&TraceDelta> {
        self.traces
            .iter()
            .max_by(|a, b| a.max_norm.total_cmp(&b.max_norm))
    }

    /// Largest normalised error over all compared traces (`0` when none).
    pub fn worst_norm(&self) -> f64 {
        self.worst().map(|t| t.max_norm).unwrap_or(0.0)
    }

    /// True when something was compared, no reference plot went missing, and
    /// no trace exceeds `tolerance` (a NaN error never passes).
    pub fn is_within(&self, tolerance: f64) -> bool {
        !self.traces.is_empty()
            && self.missing_plots.is_empty()
            && self.traces.iter().all(|t| t.max_norm <= tolerance)
    }

    /// A readable table: worst traces first, then what could not be matched.
    pub fn report(&self, tolerance: f64) -> String {
        let mut out = String::new();
        let mut rows: Vec<&TraceDelta> = self.traces.iter().collect();
        rows.sort_by(|a, b| b.max_norm.total_cmp(&a.max_norm));
        let _ = writeln!(
            out,
            "{:<26} {:<18} {:>7} {:>12} {:>12} {:>12} {:>11}",
            "plot", "trace", "points", "max |err|", "rms err", "peak", "max/peak"
        );
        for t in &rows {
            let _ = writeln!(
                out,
                "{:<26} {:<18} {:>7} {:>12.3e} {:>12.3e} {:>12.3e} {:>11.2e}{}",
                clip(&t.plot, 26),
                clip(&t.trace, 18),
                t.points,
                t.max_abs,
                t.rms,
                t.peak,
                t.max_norm,
                if t.max_norm <= tolerance { "" } else { "  FAIL" },
            );
        }
        let skipped: usize = self.traces.iter().map(|t| t.skipped).sum();
        if skipped > 0 {
            let _ = writeln!(out, "{skipped} reference points lie outside RSpice's axis span");
        }
        for plot in &self.missing_plots {
            let _ = writeln!(out, "MISSING PLOT: {plot}");
        }
        if !self.missing_traces.is_empty() {
            let _ = writeln!(
                out,
                "{} reference traces absent from RSpice: {}",
                self.missing_traces.len(),
                self.missing_traces.join(", ")
            );
        }
        if !self.extra_plots.is_empty() {
            let _ = writeln!(out, "RSpice-only plots: {}", self.extra_plots.join(", "));
        }
        let _ = writeln!(
            out,
            "worst max/peak = {:.3e} over {} traces (tolerance {:.1e}): {}",
            self.worst_norm(),
            self.traces.len(),
            tolerance,
            if self.is_within(tolerance) { "PASS" } else { "FAIL" }
        );
        out
    }
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        let head: String = text.chars().take(width - 1).collect();
        format!("{head}…")
    }
}

/// Compare `ours` (RSpice) against `reference` (usually LTspice).
pub fn compare(reference: &RawFile, ours: &RawFile) -> RawComparison {
    let mut out = RawComparison::default();
    let mut used = vec![false; ours.datasets.len()];
    let mut seen: Vec<(String, usize)> = Vec::new();
    for ref_ds in &reference.datasets {
        let key = ref_ds.plotname.trim().to_ascii_lowercase();
        let nth = bump(&mut seen, &key);
        let label = labelled(&ref_ds.plotname, nth);
        let found = ours
            .datasets
            .iter()
            .enumerate()
            .filter(|(_, d)| d.plotname.trim().eq_ignore_ascii_case(ref_ds.plotname.trim()))
            .nth(nth);
        let Some((index, our_ds)) = found else {
            out.missing_plots.push(label);
            continue;
        };
        used[index] = true;
        if let Err(reason) = compare_plot(ref_ds, our_ds, &label, &mut out) {
            out.missing_plots.push(format!("{label} ({reason})"));
        }
    }
    let mut counted: Vec<(String, usize)> = Vec::new();
    for (i, ds) in ours.datasets.iter().enumerate() {
        let nth = bump(&mut counted, &ds.plotname.trim().to_ascii_lowercase());
        if !used[i] {
            out.extra_plots.push(labelled(&ds.plotname, nth));
        }
    }
    out
}

/// Occurrence index of `key` so far (0-based), then count it.
fn bump(seen: &mut Vec<(String, usize)>, key: &str) -> usize {
    match seen.iter_mut().find(|(k, _)| k == key) {
        Some((_, n)) => {
            *n += 1;
            *n
        }
        None => {
            seen.push((key.to_string(), 0));
            0
        }
    }
}

fn labelled(plotname: &str, nth: usize) -> String {
    if nth == 0 {
        plotname.to_string()
    } else {
        format!("{plotname} #{}", nth + 1)
    }
}

fn is_axis_kind(kind: &str) -> bool {
    matches!(kind.to_ascii_lowercase().as_str(), "time" | "frequency")
}

fn compare_plot(
    reference: &RawDataset,
    ours: &RawDataset,
    label: &str,
    out: &mut RawComparison,
) -> Result<(), String> {
    if reference.values.is_empty() || ours.values.is_empty() {
        return Err("a plot has no points".into());
    }
    let single = reference.values.len() == 1 || ours.values.len() == 1;
    let skip_name = |name: &str| ours.steps.iter().any(|s| s.eq_ignore_ascii_case(name));

    // Pair variable columns: (name, reference column, our column).
    let mut pairs = Vec::new();
    for (ri, rv) in reference.variables.iter().enumerate() {
        let axis_like = if single { is_axis_kind(&rv.kind) } else { ri == 0 };
        if axis_like || skip_name(&rv.name) {
            continue;
        }
        let ci = ours.variables.iter().enumerate().position(|(ci, ov)| {
            ov.name.eq_ignore_ascii_case(&rv.name) && (single || ci != 0)
        });
        match ci {
            Some(ci) => pairs.push((rv.name.clone(), ri, ci)),
            None => out.missing_traces.push(format!("{label}: {}", rv.name)),
        }
    }

    if single {
        if reference.values.len() != ours.values.len() {
            return Err(format!(
                "{} vs {} points",
                reference.values.len(),
                ours.values.len()
            ));
        }
        for (name, ri, ci) in pairs {
            let delta = sample_delta(
                label,
                &name,
                [(reference.values[0][ri], ours.values[0][ci])].into_iter(),
                0,
            );
            out.traces.push(delta);
        }
        return Ok(());
    }

    // A reference time axis is |t| (LTspice flags stored points with
    // negative timestamps).
    let ref_time = reference
        .variables
        .first()
        .map(|v| v.kind.eq_ignore_ascii_case("time"))
        .unwrap_or(false);
    let ref_axis: Vec<f64> = reference
        .values
        .iter()
        .map(|row| if ref_time { row[0].re.abs() } else { row[0].re })
        .collect();
    let our_axis: Vec<f64> = ours.values.iter().map(|row| row[0].re).collect();

    let monotone = |axis: &[f64]| axis.windows(2).all(|w| w[1] >= w[0]);
    if !monotone(&our_axis) || !monotone(&ref_axis) {
        // Descending or nested sweeps: no interpolation, row-by-row only.
        if ref_axis.len() != our_axis.len() {
            return Err(format!(
                "axis is not monotone and the plots differ in length ({} vs {} points)",
                ref_axis.len(),
                our_axis.len()
            ));
        }
        for (name, ri, ci) in pairs {
            let rows = reference
                .values
                .iter()
                .zip(&ours.values)
                .map(|(r, o)| (r[ri], o[ci]));
            out.traces.push(sample_delta(label, &name, rows, 0));
        }
        return Ok(());
    }

    for (name, ri, ci) in pairs {
        let mut skipped = 0usize;
        let mut rows = Vec::with_capacity(ref_axis.len());
        for (k, &x) in ref_axis.iter().enumerate() {
            match interpolate(&our_axis, &ours.values, ci, x) {
                Some(value) => rows.push((reference.values[k][ri], value)),
                None => skipped += 1,
            }
        }
        out.traces
            .push(sample_delta(label, &name, rows.into_iter(), skipped));
    }
    Ok(())
}

/// Linear interpolation of column `col` at axis value `x`, `None` outside the
/// axis span (a hair of relative slack at the ends absorbs `f32` axis
/// rounding in the reference file).
fn interpolate(axis: &[f64], rows: &[Vec<Complex64>], col: usize, x: f64) -> Option<Complex64> {
    let (first, last) = (*axis.first()?, *axis.last()?);
    let slack = 1e-9 * (last - first).abs().max(f64::MIN_POSITIVE);
    if x < first - slack || x > last + slack {
        return None;
    }
    // Number of axis points ≤ x.
    let upper = axis.partition_point(|&a| a <= x);
    if upper == 0 {
        return Some(rows[0][col]);
    }
    if upper == axis.len() {
        return Some(rows[axis.len() - 1][col]);
    }
    let (x0, x1) = (axis[upper - 1], axis[upper]);
    let (y0, y1) = (rows[upper - 1][col], rows[upper][col]);
    // x0 ≤ x < x1, so x1 > x0 and the fraction is well defined.
    let t = (x - x0) / (x1 - x0);
    Some(y0 + (y1 - y0) * t)
}

fn sample_delta(
    plot: &str,
    trace: &str,
    samples: impl Iterator<Item = (Complex64, Complex64)>,
    skipped: usize,
) -> TraceDelta {
    let (mut n, mut max_abs, mut sum_sq, mut peak) = (0usize, 0.0f64, 0.0f64, 0.0f64);
    for (r, o) in samples {
        let err = (r - o).norm();
        // A NaN anywhere poisons the max: it must never read as a pass.
        max_abs = if err.is_nan() || max_abs.is_nan() { f64::NAN } else { max_abs.max(err) };
        sum_sq += err * err;
        peak = peak.max(r.norm());
        n += 1;
    }
    let rms = if n > 0 { (sum_sq / n as f64).sqrt() } else { 0.0 };
    let max_norm = if max_abs.is_nan() {
        f64::NAN
    } else if peak > 0.0 {
        max_abs / peak
    } else if max_abs > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };
    TraceDelta {
        plot: plot.to_string(),
        trace: trace.to_string(),
        points: n,
        skipped,
        max_abs,
        rms,
        peak,
        max_norm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rawwave::{self, RawFlavor, RawVariable};
    use crate::{parse, run};

    fn var(name: &str, kind: &str) -> RawVariable {
        RawVariable { name: name.into(), kind: kind.into() }
    }

    fn ds(plotname: &str, vars: &[(&str, &str)], rows: &[&[f64]]) -> RawDataset {
        RawDataset {
            title: "t".into(),
            plotname: plotname.into(),
            flags: vec!["real".into()],
            variables: vars.iter().map(|(n, k)| var(n, k)).collect(),
            values: rows
                .iter()
                .map(|r| r.iter().map(|&v| Complex64::new(v, 0.0)).collect())
                .collect(),
            steps: Vec::new(),
        }
    }

    fn file(datasets: Vec<RawDataset>) -> RawFile {
        RawFile { datasets }
    }

    fn rc_deck() -> &'static str {
        "* rc\nV1 in 0 PULSE(0 5 0 1n 1n 1 2)\nR1 in out 1k\nC1 out 0 1u\n.tran 10u 5m\n.end\n"
    }

    #[test]
    fn a_file_matches_itself_exactly() {
        let n = parse(rc_deck()).unwrap();
        let raw = rawwave::build("rc", &n, &run(&n).unwrap());
        let cmp = compare(&raw, &raw);
        assert!(!cmp.traces.is_empty());
        assert_eq!(cmp.worst_norm(), 0.0);
        assert!(cmp.is_within(0.0));
        assert!(cmp.missing_plots.is_empty() && cmp.extra_plots.is_empty());
    }

    /// The reference path an LTspice file takes: f32 values off a binary
    /// payload. Quantisation shows up at ~1e-7 and nowhere near 1e-5.
    #[test]
    fn f32_quantisation_is_the_only_difference_through_a_binary_file() {
        let n = parse(rc_deck()).unwrap();
        let ours = rawwave::build("rc", &n, &run(&n).unwrap());
        let bytes = rawwave::to_bytes(&ours, RawFlavor::Binary);
        let reference = rawwave::read(&bytes).unwrap();
        let cmp = compare(&reference, &ours);
        let worst = cmp.worst().expect("something compared");
        assert!(worst.max_norm > 0.0 && worst.max_norm < 1e-6, "{worst:?}");
        assert!(cmp.is_within(1e-6));
        // The out trace was matched point for point.
        let out = cmp.traces.iter().find(|t| t.trace == "V(out)").unwrap();
        assert_eq!(out.points, ours.datasets[0].values.len());
        assert_eq!(out.skipped, 0);
    }

    /// Different timestep grids: the reference is the analytic RC step on a
    /// coarse grid, RSpice's trace is interpolated onto it.
    #[test]
    fn a_coarse_reference_grid_compares_through_interpolation() {
        let n = parse(rc_deck()).unwrap();
        let ours = rawwave::build("rc", &n, &run(&n).unwrap());
        let tau = 1e-3;
        let coarse: Vec<Vec<f64>> = (0..=50)
            .map(|k| {
                let t = k as f64 * 100e-6;
                vec![t, 5.0 * (1.0 - (-t / tau).exp())]
            })
            .collect();
        let rows: Vec<&[f64]> = coarse.iter().map(|r| r.as_slice()).collect();
        let reference = file(vec![ds(
            "Transient Analysis",
            &[("time", "time"), ("V(out)", "voltage")],
            &rows,
        )]);
        let cmp = compare(&reference, &ours);
        let out = &cmp.traces[0];
        assert_eq!(out.trace, "V(out)");
        assert_eq!(out.points, 51);
        assert!(out.max_norm < 5e-3, "analytic RC vs RSpice: {out:?}");
        assert!(cmp.missing_plots.is_empty() && cmp.extra_plots.is_empty());
    }

    /// Plots only RSpice has (it writes an operating point next to a `.tran`,
    /// LTspice does not) are informational, never a failure.
    #[test]
    fn rspice_only_plots_are_informational() {
        let tran = ds(
            "Transient Analysis",
            &[("time", "time"), ("V(a)", "voltage")],
            &[&[0.0, 1.0], &[1.0, 1.0]],
        );
        let op = ds("Operating Point", &[("time", "time"), ("V(a)", "voltage")], &[&[0.0, 1.0]]);
        let cmp = compare(&file(vec![tran.clone()]), &file(vec![op, tran]));
        assert_eq!(cmp.extra_plots, vec!["Operating Point".to_string()]);
        assert!(cmp.is_within(0.0));
    }

    #[test]
    fn an_injected_error_is_measured_and_fails_the_tolerance() {
        let ours = file(vec![ds(
            "Transient Analysis",
            &[("time", "time"), ("V(a)", "voltage")],
            &[&[0.0, 0.0], &[1.0, 2.0], &[2.0, 4.0]],
        )]);
        let reference = file(vec![ds(
            "Transient Analysis",
            &[("time", "time"), ("V(a)", "voltage")],
            &[&[0.0, 0.0], &[1.0, 2.02], &[2.0, 4.04]],
        )]);
        let cmp = compare(&reference, &ours);
        let t = &cmp.traces[0];
        assert!((t.max_abs - 0.04).abs() < 1e-12);
        assert!((t.peak - 4.04).abs() < 1e-12);
        assert!((t.max_norm - 0.04 / 4.04).abs() < 1e-12);
        assert!(cmp.is_within(1e-2));
        assert!(!cmp.is_within(5e-3));
        assert!(cmp.report(5e-3).contains("FAIL"));
        assert!(cmp.report(1e-2).contains("PASS"));
    }

    /// LTspice stamps some stored points with negative time; the reference
    /// axis is read as |t|. Points outside our span are counted, not scored.
    #[test]
    fn reference_negative_timestamps_and_out_of_span_points() {
        let ours = file(vec![ds(
            "Transient Analysis",
            &[("time", "time"), ("V(a)", "voltage")],
            &[&[0.0, 0.0], &[1.0, 1.0], &[2.0, 2.0]],
        )]);
        let reference = file(vec![ds(
            "Transient Analysis",
            &[("time", "time"), ("V(a)", "voltage")],
            &[&[0.0, 0.0], &[-0.5, 0.5], &[1.5, 1.5], &[3.0, 3.0]],
        )]);
        let cmp = compare(&reference, &ours);
        let t = &cmp.traces[0];
        // |−0.5| = 0.5 is interior; 3.0 is past our end.
        assert_eq!((t.points, t.skipped), (3, 1));
        assert!(t.max_norm < 1e-12);
    }

    /// `.op`: LTspice's file has no axis variable, RSpice keeps a synthetic
    /// `time = 0`; every shared net still compares.
    #[test]
    fn operating_points_compare_by_name_without_an_axis() {
        let ours = file(vec![ds(
            "Operating Point",
            &[("time", "time"), ("V(out)", "voltage"), ("I(R1)", "device_current")],
            &[&[0.0, 2.5, 1e-3]],
        )]);
        let reference = file(vec![ds(
            "Operating Point",
            &[("V(out)", "voltage"), ("I(R1)", "device_current"), ("Ix(u1:1)", "device_current")],
            &[&[2.5000001, 1e-3, 7.0]],
        )]);
        let cmp = compare(&reference, &ours);
        assert_eq!(cmp.traces.len(), 2);
        assert!(cmp.worst_norm() < 1e-6);
        assert_eq!(cmp.missing_traces, vec!["Operating Point: Ix(u1:1)".to_string()]);
        assert!(cmp.is_within(1e-6));
    }

    #[test]
    fn complex_ac_traces_compare_by_phasor_distance() {
        let mut ours = ds(
            "AC Analysis",
            &[("frequency", "frequency"), ("V(out)", "voltage")],
            &[&[1.0, 0.0], &[10.0, 0.0], &[100.0, 0.0]],
        );
        ours.flags = vec!["complex".into()];
        let mut reference = ours.clone();
        for (k, row) in ours.values.iter_mut().enumerate() {
            row[1] = Complex64::new(1.0 / (1.0 + k as f64), -0.1 * k as f64);
        }
        for (k, row) in reference.values.iter_mut().enumerate() {
            row[1] = Complex64::new(1.0 / (1.0 + k as f64), -0.1 * k as f64 + 0.001);
        }
        let cmp = compare(&file(vec![reference]), &file(vec![ours]));
        let t = &cmp.traces[0];
        assert!((t.max_abs - 0.001).abs() < 1e-12, "{t:?}");
        assert_eq!(t.points, 3);
    }

    #[test]
    fn missing_plots_fail_and_stepped_plots_pair_by_occurrence() {
        let step = |v: f64| {
            ds(
                "Transient Analysis",
                &[("time", "time"), ("V(a)", "voltage")],
                &[&[0.0, v], &[1.0, v]],
            )
        };
        let reference = file(vec![step(1.0), step(2.0), step(3.0)]);
        let ours = file(vec![step(1.0), step(2.0)]);
        let cmp = compare(&reference, &ours);
        assert_eq!(cmp.traces.len(), 2);
        assert_eq!(cmp.worst_norm(), 0.0);
        assert_eq!(cmp.missing_plots, vec!["Transient Analysis #3".to_string()]);
        assert!(!cmp.is_within(1.0), "a missing reference plot is a failure");
        // Swapped step values must show up as an error, not pair by name only.
        let swapped = compare(&file(vec![step(1.0), step(2.0)]), &file(vec![step(2.0), step(1.0)]));
        assert!(swapped.worst_norm() >= 0.5);
    }

    #[test]
    fn nan_never_passes_and_empty_never_passes() {
        let a = file(vec![ds(
            "Transient Analysis",
            &[("time", "time"), ("V(a)", "voltage")],
            &[&[0.0, 1.0], &[1.0, 1.0]],
        )]);
        let b = file(vec![ds(
            "Transient Analysis",
            &[("time", "time"), ("V(a)", "voltage")],
            &[&[0.0, 1.0], &[1.0, f64::NAN]],
        )]);
        assert!(!compare(&a, &b).is_within(1e9));
        assert!(!compare(&RawFile::default(), &RawFile::default()).is_within(1.0));
    }

    #[test]
    fn descending_sweeps_fall_back_to_row_comparison() {
        let mk = |scale: f64| {
            ds(
                "DC transfer characteristic",
                &[("v1", "voltage"), ("V(out)", "voltage")],
                &[&[2.0, 2.0 * scale], &[1.0, 1.0 * scale], &[0.0, 0.0]],
            )
        };
        let cmp = compare(&file(vec![mk(1.0)]), &file(vec![mk(1.0)]));
        assert_eq!(cmp.worst_norm(), 0.0);
        assert_eq!(cmp.traces[0].points, 3);
        let bad = compare(&file(vec![mk(1.0)]), &file(vec![mk(1.1)]));
        assert!((bad.worst_norm() - 0.1).abs() < 1e-12);
    }
}
