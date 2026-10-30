//! Frequency spectra of transient traces (TODO §6 — the LTspice FFT view).
//!
//! [`spectra`] takes one [`rawwave`] file and turns every transient plot's
//! matched trace columns into one spectrum plot each: the (possibly
//! non-uniform) time axis is linearly resampled onto a uniform grid of the
//! next power-of-two length, the selected window taper is applied, a
//! hand-rolled radix-2 FFT runs, and the single-sided amplitude comes out
//! (`|X[0]|/N` at DC and Nyquist, `2|X[k]|/N` elsewhere, so a sine reading
//! on an integer bin reports its true peak amplitude). Bin spacing is
//! exactly `1/span` — the span of the transient's own time axis — so a
//! `10ms` transient puts every harmonic on an integer bin.
//!
//! Windows are the 13 parameter-free functions of LTspice's FFT-view
//! dialog, in the dialog's own spelling (+ common aliases in
//! [`Window::from_label`]); the six parameterized dialogs —
//! Dolph-Chebyshev, Gaussian, Hann-Poisson, Kaiser-Bessel, Poisson, Tukey —
//! are a later arc. The shape functions are the published coefficient
//! tables on `t = i/(n−1)`, symmetric, and the taper is normalized to unit
//! mean (Σw = n), so a DC level's first bin reads its true amplitude.

use crate::rawwave::{RawDataset, RawFile, RawVariable};
use num_complex::Complex64;
use std::f64::consts::{PI, TAU};

/// One of the FFT-view dialog's parameter-free window functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
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

impl Window {
    /// The dialog's spelling (note `Blackman-Nuttal`, one l — the
    /// dialog's own string).
    pub fn label(self) -> &'static str {
        match self {
            Window::Bartlett => "Bartlett",
            Window::BartlettHann => "Bartlett-Hann",
            Window::Blackman => "Blackman",
            Window::BlackmanHarris => "Blackman-Harris",
            Window::BlackmanNuttal => "Blackman-Nuttal",
            Window::Bohman => "Bohman",
            Window::Cosine => "Cosine",
            Window::FlatTop => "Flat Top",
            Window::Hamming => "Hamming",
            Window::Hann => "Hann",
            Window::Lanczos => "Lanczos",
            Window::Parzen => "Parzen",
            Window::Welch => "Welch",
        }
    }

    /// Match a user-spelled window name, ignoring case, with the dialog's
    /// words accepted in any spacing plus the common aliases (`hanning`,
    /// `sine`, the two-l Nuttall spelling); unknown names are `None`.
    pub fn from_label(label: &str) -> Option<Window> {
        let norm = label.trim().to_ascii_lowercase().replace(' ', "-");
        let window = match norm.as_str() {
            "bartlett" => Window::Bartlett,
            "bartlett-hann" => Window::BartlettHann,
            "blackman" => Window::Blackman,
            "blackman-harris" => Window::BlackmanHarris,
            "blackman-nuttal" | "blackman-nuttall" => Window::BlackmanNuttal,
            "bohman" => Window::Bohman,
            "cosine" | "sine" => Window::Cosine,
            "flat-top" => Window::FlatTop,
            "hamming" => Window::Hamming,
            "hann" | "hanning" => Window::Hann,
            "lanczos" => Window::Lanczos,
            "parzen" => Window::Parzen,
            "welch" => Window::Welch,
            _ => return None,
        };
        Some(window)
    }

    /// The window's raw weights at `t = i/(n−1)` — the published
    /// coefficient tables, unnormalized.
    pub fn shape(self, n: usize) -> Vec<f64> {
        if n <= 1 {
            return vec![1.0];
        }
        let m1 = (n - 1) as f64;
        (0..n)
            .map(|i| {
                let t = i as f64 / m1;
                let b = 2.0 * t - 1.0;
                match self {
                    Window::Bartlett => 1.0 - b.abs(),
                    Window::BartlettHann => 0.62 - 0.48 * (t - 0.5).abs() - 0.38 * (TAU * t).cos(),
                    Window::Blackman => 0.42 - 0.5 * (TAU * t).cos() + 0.08 * (2.0 * TAU * t).cos(),
                    Window::BlackmanHarris => {
                        0.35875 - 0.48829 * (TAU * t).cos() + 0.14128 * (2.0 * TAU * t).cos()
                            - 0.01168 * (3.0 * TAU * t).cos()
                    }
                    Window::BlackmanNuttal => {
                        0.3635819 - 0.4891775 * (TAU * t).cos() + 0.1365995 * (2.0 * TAU * t).cos()
                            - 0.0106411 * (3.0 * TAU * t).cos()
                    }
                    Window::Bohman => {
                        let u = b.abs();
                        (1.0 - u) * (PI * u).cos() + (PI * u).sin() / PI
                    }
                    Window::Cosine => (PI * t).sin(),
                    Window::FlatTop => {
                        0.21557895 - 0.41663158 * (TAU * t).cos()
                            + 0.277263158 * (2.0 * TAU * t).cos()
                            - 0.083578947 * (3.0 * TAU * t).cos()
                            + 0.006947368 * (4.0 * TAU * t).cos()
                    }
                    Window::Hamming => 0.54 - 0.46 * (TAU * t).cos(),
                    Window::Hann => 0.5 - 0.5 * (TAU * t).cos(),
                    Window::Lanczos if b == 0.0 => 1.0,
                    Window::Lanczos => (PI * b).sin() / (PI * b),
                    Window::Parzen => {
                        let u = b.abs();
                        if u <= 0.5 {
                            1.0 - 6.0 * u * u + 6.0 * u * u * u
                        } else {
                            2.0 * (1.0 - u).powi(3)
                        }
                    }
                    Window::Welch => 1.0 - b * b,
                }
            })
            .collect()
    }

    /// The window's tap weights over `n` points, normalized to unit mean
    /// (Σw = n). Degenerate sizes where the raw form sums to zero — the
    /// endpoint-zero windows at `n = 2` and friends — collapse to the
    /// boxcar instead of dividing by nothing.
    pub fn taper(self, n: usize) -> Vec<f64> {
        let raw = self.shape(n);
        let sum: f64 = raw.iter().sum();
        if n <= 1 || !sum.is_finite() || sum <= 0.0 {
            return vec![1.0; n];
        }
        raw.into_iter().map(|w| w * n as f64 / sum).collect()
    }
}

/// Every transient plot's matched trace columns, FFT'd to one spectrum
/// plot each (the same trace, same order). A populated `traces` list
/// matches columns case-insensitively by full name; empty means every
/// trace. The axis and step columns never FFT; non-transient plots and
/// too-short traces (fewer than 2 samples over a positive span) stay
/// silent.
pub fn spectra(raw: &RawFile, traces: &[String], window: Window) -> RawFile {
    let mut out = RawFile::default();
    for ds in &raw.datasets {
        if !ds.plotname.eq_ignore_ascii_case("Transient Analysis") {
            continue;
        }
        for i in matched_columns(ds, traces) {
            let trace = ds.variables[i].name.clone();
            let times = ds.axis();
            let values: Vec<f64> = ds.values.iter().map(|row| row[i].re).collect();
            let Some((freqs, amps)) = spectrum(&times, &values, window) else {
                continue;
            };
            out.datasets.push(RawDataset {
                title: ds.title.clone(),
                plotname: format!("{trace} (FFT)"),
                flags: vec!["real".to_string(), "forward".to_string()],
                variables: vec![
                    RawVariable {
                        name: "frequency".to_string(),
                        kind: "frequency".to_string(),
                    },
                    RawVariable {
                        name: trace,
                        kind: ds.variables[i].kind.clone(),
                    },
                ],
                values: freqs
                    .into_iter()
                    .zip(amps)
                    .map(|(f, a)| vec![Complex64::new(f, 0.0), Complex64::new(a, 0.0)])
                    .collect(),
                steps: Vec::new(),
            });
        }
    }
    out
}

/// Trace-column indexes of one dataset: everything but the axis and the
/// step columns, filtered by `traces` when non-empty (case-insensitive
/// full-name matches; requests no column carries are silent).
fn matched_columns(ds: &RawDataset, traces: &[String]) -> Vec<usize> {
    (1..ds.variables.len())
        .filter(|&i| {
            let name = ds.variables[i].name.as_str();
            let step_column = ds.steps.iter().any(|target| target == name);
            !step_column
                && (traces.is_empty() || traces.iter().any(|t| name.eq_ignore_ascii_case(t)))
        })
        .collect()
}

/// The spectrum of one trace: a uniform power-of-two grid over the span,
/// windowed, FFT'd, single-sided amplitudes. `None` when the trace is too
/// short or the times don't span a positive interval.
fn spectrum(times: &[f64], values: &[f64], window: Window) -> Option<(Vec<f64>, Vec<f64>)> {
    let (span, m) = span_width(times)?;
    let weights = window.taper(m);
    let mut x: Vec<Complex64> = resample(times, values)?
        .into_iter()
        .zip(weights)
        .map(|(s, w)| Complex64::new(s * w, 0.0))
        .collect();
    fft_inplace(&mut x);
    let freqs = (0..=m / 2).map(|k| k as f64 / span).collect::<Vec<_>>();
    let amplitude = |k: usize| {
        let scale = if k == 0 || 2 * k == m { 1.0 } else { 2.0 };
        x[k].norm() * scale / m as f64
    };
    let amps = (0..=m / 2).map(amplitude).collect::<Vec<_>>();
    Some((freqs, amps))
}

/// The uniform resample of one trace onto its grid: `m` samples spaced
/// `span/m` apart starting at `times[0]` (`m` the next power of two at or
/// above the sample count).
fn resample(times: &[f64], values: &[f64]) -> Option<Vec<f64>> {
    let (span, m) = span_width(times)?;
    let dt = span / m as f64;
    Some(
        (0..m)
            .map(|j| interp(times, values, times[0] + j as f64 * dt))
            .collect(),
    )
}

/// The span and next-power-of-two length of a time axis: `None` when the
/// trace holds fewer than 2 samples or the times don't span a positive
/// interval (flat or backward axes).
fn span_width(times: &[f64]) -> Option<(f64, usize)> {
    let n = times.len();
    if n < 2 {
        return None;
    }
    let span = times[n - 1] - times[0];
    if span <= 0.0 || span.is_nan() {
        return None;
    }
    Some((span, n.next_power_of_two()))
}

/// Linear interpolation of `(times, values)` at `t`: points outside the
/// span clamp to the end values. (Equal consecutive times can't divide by
/// zero: both sit on the same side of any sampled `t`.)
fn interp(times: &[f64], values: &[f64], t: f64) -> f64 {
    let hi = times.partition_point(|&x| x < t);
    if hi == 0 {
        return values[0];
    }
    if hi == times.len() {
        return values[times.len() - 1];
    }
    let (t0, t1) = (times[hi - 1], times[hi]);
    values[hi - 1] + (values[hi] - values[hi - 1]) * ((t - t0) / (t1 - t0))
}

/// In-place iterative radix-2 Cooley-Tukey FFT; `x.len()` must be a power
/// of two.
fn fft_inplace(x: &mut [Complex64]) {
    let n = x.len();
    if n <= 1 {
        return;
    }
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            x.swap(i, j);
        }
    }
    let twiddles: Vec<Complex64> = (0..n / 2)
        .map(|k| Complex64::from_polar(1.0, -TAU * k as f64 / n as f64))
        .collect();
    let mut len = 2;
    while len <= n {
        let step = n / len;
        let half = len / 2;
        for start in (0..n).step_by(len) {
            for k in 0..half {
                let u = x[start + k];
                let v = x[start + k + half] * twiddles[k * step];
                x[start + k] = u + v;
                x[start + k + half] = u - v;
            }
        }
        len *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    /// One fabricated plot: the columns named as given (a step column
    /// carries whatever values the test hands in), the axis over
    /// `j/(n−1)`.
    fn plot(cols: &[(&str, Vec<f64>)], steps: &[&str], plotname: &str) -> RawDataset {
        let n = cols[0].1.len();
        let times: Vec<f64> = (0..n).map(|j| j as f64 / (n - 1) as f64).collect();
        let mut variables = vec![RawVariable {
            name: "time".to_string(),
            kind: "time".to_string(),
        }];
        for (name, _) in cols {
            variables.push(RawVariable {
                name: (*name).to_string(),
                kind: "voltage".to_string(),
            });
        }
        let mut values = Vec::with_capacity(n);
        for (r, t) in times.iter().enumerate() {
            let mut row = vec![Complex64::new(*t, 0.0)];
            for (_, v) in cols {
                row.push(Complex64::new(v[r], 0.0));
            }
            values.push(row);
        }
        RawDataset {
            title: "test title".to_string(),
            plotname: plotname.to_string(),
            flags: vec!["real".to_string()],
            variables,
            values,
            steps: steps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn window_labels_all_variants_and_aliases() {
        for window in [
            Window::Bartlett,
            Window::BartlettHann,
            Window::Blackman,
            Window::BlackmanHarris,
            Window::BlackmanNuttal,
            Window::Bohman,
            Window::Cosine,
            Window::FlatTop,
            Window::Hamming,
            Window::Hann,
            Window::Lanczos,
            Window::Parzen,
            Window::Welch,
        ] {
            assert_eq!(
                Window::from_label(window.label()),
                Some(window),
                "{}",
                window.label()
            );
        }
        // Aliases: the usual alternate spellings match the same dialog word.
        assert_eq!(Window::from_label("hanning"), Some(Window::Hann));
        assert_eq!(Window::from_label("sine"), Some(Window::Cosine));
        assert_eq!(
            Window::from_label("blackman-nuttall"),
            Some(Window::BlackmanNuttal)
        );
        assert_eq!(Window::from_label("flat top"), Some(Window::FlatTop));
        assert_eq!(
            Window::from_label(" BLACKMAN-HARRIS "),
            Some(Window::BlackmanHarris)
        );
        assert_eq!(Window::from_label("kaiser"), None);
        assert_eq!(Window::from_label(""), None);
    }

    #[test]
    fn shape_table_blackman_harris_n5() {
        // The published BH3 coefficients at n=5: near zero at the edges,
        // one at the center.
        let shape = Window::BlackmanHarris.shape(5);
        for (w, want) in shape.iter().zip([6e-5, 0.21747, 1.0, 0.21747, 6e-5]) {
            assert_relative_eq!(*w, want, epsilon = 1e-12);
        }
    }

    #[test]
    fn taper_unit_mean_and_symmetric() {
        for window in [
            Window::Bartlett,
            Window::BartlettHann,
            Window::Blackman,
            Window::BlackmanHarris,
            Window::BlackmanNuttal,
            Window::Bohman,
            Window::Cosine,
            Window::FlatTop,
            Window::Hamming,
            Window::Hann,
            Window::Lanczos,
            Window::Parzen,
            Window::Welch,
        ] {
            let taps = window.taper(64);
            let sum: f64 = taps.iter().sum();
            assert_relative_eq!(sum, 64.0, epsilon = 1e-9, max_relative = 1e-9);
            assert_relative_eq!(taps[0], taps[63], epsilon = 1e-12);
            assert_relative_eq!(taps[31], taps[32], epsilon = 1e-12);
        }
    }

    #[test]
    fn taper_degenerate_guards() {
        assert_eq!(Window::Hann.taper(1), vec![1.0]);
        // Hann's raw pair sums to zero: the boxcar takes over.
        assert_eq!(Window::Hann.taper(2), vec![1.0, 1.0]);
        // Flat Top's raw pair sums negative there too.
        assert_eq!(Window::FlatTop.taper(2), vec![1.0, 1.0]);
    }

    #[test]
    fn fft_matches_direct_dft() {
        let src = [
            Complex64::new(3.1, -0.2),
            Complex64::new(0.4, 2.5),
            Complex64::new(-1.7, 0.9),
            Complex64::new(0.02, -3.3),
            Complex64::new(2.05, 0.14),
            Complex64::new(-0.6, -0.05),
            Complex64::new(1.11, 0.77),
            Complex64::new(0.32, 1.4),
        ];
        let mut x = src.to_vec();
        fft_inplace(&mut x);
        let n = src.len();
        let direct: Vec<Complex64> = (0..n)
            .map(|k| {
                (0..n)
                    .map(|j| {
                        src[j] * Complex64::from_polar(1.0, -TAU * j as f64 * k as f64 / n as f64)
                    })
                    .sum::<Complex64>()
            })
            .collect();
        for k in 0..n {
            assert_relative_eq!(x[k].re, direct[k].re, epsilon = 1e-12);
            assert_relative_eq!(x[k].im, direct[k].im, epsilon = 1e-12);
        }
    }

    #[test]
    fn windowed_on_bin_sine_amplitudes() {
        // Stages applied directly: 64 samples already on the FFT grid (no
        // resampling), bin 8 sine through the Blackman-Harris taper. The
        // peak reads its true amplitude; skirts read at bins ±1..±3; bins
        // outside that stay far below (the −92 dB filter window).
        let m = 64;
        let taps = Window::BlackmanHarris.taper(m);
        let mut x: Vec<Complex64> = (0..m)
            .map(|j| Complex64::new(taps[j] * (TAU * 8.0 * j as f64 / m as f64).sin(), 0.0))
            .collect();
        fft_inplace(&mut x);
        let dc = x[0].norm() / m as f64;
        assert!(dc < 1e-4, "DC {dc}");
        let amp = |k: usize| x[k].norm() * 2.0 / m as f64;
        assert_relative_eq!(amp(8), 1.0, epsilon = 1e-5, max_relative = 1e-5);
        for k in 0..=m / 2 {
            if (5..=11).contains(&k) {
                continue; // the tone's own main-lobe skirt
            }
            assert!(amp(k) < 1e-4, "bin {k}: {}", amp(k));
        }
    }

    #[test]
    fn constant_trace_dc_exact_through_pipeline() {
        // A DC level resamples to itself on any grid, so the pipeline
        // reads it exactly: unit-mean taper puts Σw·2.5 at bin 0 and
        // nothing above it but the (n−1)-shape tap bins.
        let times: Vec<f64> = (0..64).map(|j| j as f64 / 63.0).collect();
        let values = vec![2.5; 64];
        let (freqs, amps) = spectrum(&times, &values, Window::BlackmanHarris).unwrap();
        assert_relative_eq!(amps[0], 2.5, epsilon = 1e-9, max_relative = 1e-9);
        for (k, &a) in amps.iter().enumerate().skip(1) {
            assert!(a < 3.6, "bin {k}: {a}");
        }
        for (k, &a) in amps.iter().enumerate().skip(4) {
            assert!(a < 1e-3, "bin {k}: {a}");
        }
        // Bin spacing is exactly 1/span (span = 1 here, so bin k sits at k).
        for (k, &f) in freqs.iter().enumerate() {
            assert_relative_eq!(f, k as f64, epsilon = 0.0, max_relative = 0.0);
        }
    }

    #[test]
    fn spectrum_rejects_short_and_flat_traces() {
        assert!(spectrum(&[], &[], Window::Hann).is_none());
        assert!(spectrum(&[1.0], &[1.0], Window::Hann).is_none());
        assert!(spectrum(&[2.0, 2.0], &[1.0, 3.0], Window::Hann).is_none());
    }

    #[test]
    fn resampled_ramp_lands_on_the_uniform_grid() {
        // A linear ramp interpolates exactly between any two knots, so
        // whatever the grid offset, the resample reproduces it.
        let times: Vec<f64> = (0..64).map(|j| j as f64 / 64.0).collect();
        let values = times.clone();
        let y = resample(&times, &values).unwrap();
        let span = times[63] - times[0];
        let dt = span / 64.0;
        for (j, &v) in y.iter().enumerate() {
            assert_relative_eq!(
                v,
                times[0] + j as f64 * dt,
                epsilon = 1e-13,
                max_relative = 1e-13
            );
        }
        assert_eq!(y.len(), 64);
    }

    #[test]
    fn resample_clamps_outside_the_span() {
        let y = resample(&[0.0, 0.5], &[5.0, 7.0]).unwrap();
        assert_eq!(y, vec![5.0, 6.0]);
        // Backward/flat axes are refused by the span check.
        assert!(resample(&[1.0, 1.0], &[1.0, 2.0]).is_none());
    }

    #[test]
    fn jittered_times_still_read_the_tone() {
        // 128 solver-jittered samples of a sine pitched to land exactly on
        // bin 3 of the resampled grid (f = 3/span); the resample and window
        // put the peak at the right bin near its true amplitude (the linear
        // interpolation through the jittered knots shaves a fraction of a
        // percent), and every bin sits exactly on k/span.
        let n = 128;
        let times: Vec<f64> = (0..n)
            .map(|j| j as f64 * 1e-3 + 0.3e-3 * if j % 3 == 1 { 1.0 } else { 0.0 })
            .collect();
        let span = times[n - 1] - times[0];
        let tone = 3.0 / span;
        let values: Vec<f64> = times.iter().map(|t| (TAU * tone * t).sin()).collect();
        let (freqs, amps) = spectrum(&times, &values, Window::BlackmanHarris).unwrap();
        assert_eq!(freqs.len(), 65);
        assert_eq!(amps.len(), 65);
        assert_relative_eq!(freqs[0], 0.0, epsilon = 0.0, max_relative = 0.0);
        assert_relative_eq!(freqs[3], 3.0 / span, epsilon = 0.0, max_relative = 0.0);
        assert_relative_eq!(amps[3], 1.0, epsilon = 5e-3, max_relative = 5e-3);
        // DC reads nearly zero.
        assert!(amps[0] < 5e-3, "DC {}", amps[0]);
    }

    #[test]
    fn spectral_plots_shape_selection_and_skips() {
        let out_col: Vec<f64> = (0..8).map(|j| j as f64).collect();
        let em_col = vec![1.0; 8];
        let step_col = vec![2.0; 8];
        let raw = crate::rawwave::RawFile {
            datasets: vec![
                plot(
                    &[("V(out)", out_col), ("V(em)", em_col), ("r", step_col)],
                    &["r"],
                    "Transient Analysis",
                ),
                plot(&[("V(out)", vec![1.0; 3])], &[], "AC Analysis"),
            ],
        };

        // Named trace: one spectrum, plot shape per the module contract.
        let spec = spectra(&raw, &["v(out)".to_string()], Window::BlackmanHarris);
        assert_eq!(spec.datasets.len(), 1);
        let ds = &spec.datasets[0];
        assert_eq!(ds.plotname, "V(out) (FFT)");
        assert_eq!(ds.title, "test title");
        assert_eq!(ds.flags, ["real", "forward"]);
        assert!(ds.steps.is_empty());
        assert_eq!(ds.variables.len(), 2);
        assert_eq!(
            (ds.variables[0].name.as_str(), ds.variables[0].kind.as_str()),
            ("frequency", "frequency")
        );
        assert_eq!(
            (ds.variables[1].name.as_str(), ds.variables[1].kind.as_str()),
            ("V(out)", "voltage")
        );
        // 8 samples → m = 8 → 5 bins; each row is [frequency, amplitude].
        assert_eq!(ds.values.len(), 5);
        assert_eq!(ds.values[0].len(), 2);
        // The plot's axis runs j/7 over span 1.0, so bin spacing is exactly
        // 1/span = 1 and bin 3 sits at 3.0.
        assert_relative_eq!(ds.values[3][0].re, 3.0, epsilon = 0.0, max_relative = 0.0);

        // No names: every non-step trace of every transient plot, the AC
        // plot untouched.
        let all = spectra(&raw, &[], Window::BlackmanHarris);
        assert_eq!(all.datasets.len(), 2);
        assert_eq!(all.datasets[0].plotname, "V(out) (FFT)");
        assert_eq!(all.datasets[1].plotname, "V(em) (FFT)");

        // Unmatched names stay silent.
        assert!(
            spectra(&raw, &["V(nope)".to_string()], Window::BlackmanHarris)
                .datasets
                .is_empty()
        );
    }

    #[test]
    fn stepped_runs_fft_each_dataset() {
        // Each step run's transient plot carries its own trace columns
        // (plus the step column, which never FFTs): every run FFTs.
        let col = vec![2.5; 8];
        let raw = crate::rawwave::RawFile {
            datasets: vec![
                plot(
                    &[("V(out)", col.clone()), ("r", col.clone())],
                    &["r"],
                    "Transient Analysis",
                ),
                plot(
                    &[("V(out)", col.clone()), ("r", col.clone())],
                    &["r"],
                    "Transient Analysis",
                ),
            ],
        };
        let spec = spectra(&raw, &[], Window::BlackmanHarris);
        assert_eq!(spec.datasets.len(), 2);
        for ds in &spec.datasets {
            assert_eq!(ds.plotname, "V(out) (FFT)");
            // A DC-constant trace through the pipeline still reads 2.5...
            assert!(
                (ds.values[0][1].re - 2.5).abs() < 1e-9,
                "DC {}",
                ds.values[0][1].re
            );
        }
    }
}
