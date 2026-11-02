//! The plot layer: typed axis state, the one bounds-application routine, and
//! the adapters that turn simulation results into named line series.
//!
//! Previously every plot carried its own copy of the bounds/apply logic (five
//! near-identical blocks guarded by string-parsing of twelve always-empty
//! fields) and every tab re-wrote the same element walk with its own copy of
//! the dB/phase math (twenty loops). One owner each here; the tabs only draw.
//! The branch-current walk now goes through the simulator's named family
//! interface (`ElementFamily`), so series are labelled from the deck
//! (`I(RLOAD)`) instead of a positional guess.

use circuitsim::analysis::{AcResult, DcSweepResult, TranResult};
use circuitsim::arith::TraceExpr;
use circuitsim::circuit::{Circuit, ElementFamily, CURRENT_FAMILIES};
use circuitsim::params::ParamEnv;
use circuitsim::rawwave::RawDataset;
use egui::{Stroke, TextStyle};
use egui_plot::{GridMark, Legend, Line, Plot, PlotPoints, PlotUi};

use crate::theme::{bold, LT_PLOT_BG, LT_PLOT_FRAME, PLOT_COLORS};

/// One named polyline with its palette color and data in axis units.
pub struct PlotSeries {
    pub name: String,
    pub color: egui::Color32,
    pub points: Vec<[f64; 2]>,
}

/// Axis-window request for one plot, typed instead of a quartet of always
/// empty strings. `None` on every axis means "auto-fit".
#[derive(Default, Clone, Copy, Debug)]
pub struct Bounds {
    pub x_min: Option<f64>,
    pub x_max: Option<f64>,
    pub y_min: Option<f64>,
    pub y_max: Option<f64>,
}
impl Bounds {
    /// Clear to auto-fit (the "Auto Fit" button).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// X data is plotted as `log10(freq)` when the magnitude is in dB, so a
    /// frequency bound is supplied in Hz and mapped once, here.
    pub fn x_through_log10(&self) -> Bounds {
        Bounds {
            x_min: self.x_min.map(|v| if v > 0.0 { v.log10() } else { v }),
            x_max: self.x_max.map(|v| if v > 0.0 { v.log10() } else { v }),
            ..*self
        }
    }
}

/// Axis scales, bounds and the apply-flags of every plot the editor shows.
/// The old twelve string scale fields are typed now; they stay accessible for
/// future axis-unit widgets, each defaulting to raw SI units.
pub struct PlotState {
    pub tran_x: f64,
    pub tran_y: f64,
    pub ac_x: f64,
    pub ac_mag_y: f64,
    pub ac_phase_y: f64,

    pub tran_bounds: Bounds,
    pub tran_apply: bool,

    pub ac_mag_bounds: Bounds,
    pub ac_phase_bounds: Bounds,
    pub ac_apply: bool,

    pub dc_bounds: Bounds,
    pub dc_apply: bool,
}

impl Default for PlotState {
    fn default() -> Self {
        Self {
            tran_x: 1.0,
            tran_y: 1.0,
            ac_x: 1.0,
            ac_mag_y: 1.0,
            ac_phase_y: 1.0,
            tran_bounds: Bounds::default(),
            tran_apply: false,
            ac_mag_bounds: Bounds::default(),
            ac_phase_bounds: Bounds::default(),
            ac_apply: false,
            dc_bounds: Bounds::default(),
            dc_apply: false,
        }
    }
}

/// Apply a bounds request to one plot — the single implementation shared by
/// every plot: all-axis auto-fit, or a window with the degenerate ranges
/// guarded (equal min/max would give an empty axis, so bump it by a sliver).
pub fn apply_bounds(plot_ui: &mut PlotUi, bounds: &Bounds) {
    if bounds.x_min.is_none()
        && bounds.x_max.is_none()
        && bounds.y_min.is_none()
        && bounds.y_max.is_none()
    {
        plot_ui.set_auto_bounds(egui::Vec2b::new(true, true));
        return;
    }

    plot_ui.set_auto_bounds(egui::Vec2b::new(false, false));
    let cur = plot_ui.plot_bounds();
    let mut x_min = cur.min()[0];
    let mut x_max = cur.max()[0];
    let mut y_min = cur.min()[1];
    let mut y_max = cur.max()[1];
    if let Some(v) = bounds.x_min { x_min = v; }
    if let Some(v) = bounds.x_max { x_max = v; }
    if let Some(v) = bounds.y_min { y_min = v; }
    if let Some(v) = bounds.y_max { y_max = v; }

    let safe_x_min = x_min.min(x_max);
    let safe_x_max = if x_min == x_max { x_max + 1e-6 } else { x_min.max(x_max) };
    let safe_y_min = y_min.min(y_max);
    let safe_y_max = if y_min == y_max { y_max + 1e-6 } else { y_min.max(y_max) };
    plot_ui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
        [safe_x_min, safe_y_min],
        [safe_x_max, safe_y_max],
    ));
}

// ── The waveform viewer's look ───────────────────────────────────────────────

const GRID_KEY: &str = "rspice_plot_grid";

/// Publish the View → Plot Grid switch to every plot drawn this frame.
pub fn set_grid(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(GRID_KEY), on));
}

/// Grid lines are off unless switched on, as in the reference viewer.
pub fn grid_on(ui: &egui::Ui) -> bool {
    ui.ctx()
        .data(|d| d.get_temp::<bool>(egui::Id::new(GRID_KEY)))
        .unwrap_or(false)
}

/// Run `add` with the waveform viewer's look: black surface, light bold axis
/// and legend text. Scoped to the plot, so the rest of the app keeps the
/// light chrome.
pub fn viewer_scope<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope(|ui| {
        let visuals = ui.visuals_mut();
        visuals.extreme_bg_color = LT_PLOT_BG;
        visuals.override_text_color = Some(LT_PLOT_FRAME);
        visuals.window_stroke = Stroke::new(1.0, LT_PLOT_FRAME);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LT_PLOT_FRAME);
        // Legend entries take their text from the widget states.
        for w in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
        ] {
            w.fg_stroke.color = LT_PLOT_FRAME;
        }
        ui.style_mut().text_styles.insert(TextStyle::Body, bold(11.5));
        add(ui)
    })
    .inner
}

/// Decimal places a grid step needs: 1 → 0, 0.5 → 1, 0.01 → 2.
fn decimals_for(step: f64) -> usize {
    if !step.is_finite() || step <= 0.0 {
        return 0;
    }
    (-step.log10() - 1e-9).ceil().max(0.0) as usize
}

/// A tick value at the precision of its grid step, never `-0`.
fn tick_number(value: f64, step: f64) -> String {
    let text = format!("{value:.*}", decimals_for(step));
    match text.strip_prefix('-') {
        Some(rest) if rest.chars().all(|c| c == '0' || c == '.') => rest.to_string(),
        _ => text,
    }
}

/// Magnitude tick in dB: `18dB`, `-6dB`.
pub fn db_tick(mark: GridMark) -> String {
    format!("{}dB", tick_number(mark.value, mark.step_size))
}

/// Phase tick in degrees: `-45°`.
pub fn degree_tick(mark: GridMark) -> String {
    format!("{}°", tick_number(mark.value, mark.step_size))
}

/// A frequency as the reference prints it: `10Hz`, `1KHz`, `100KHz`, `2.5MHz`.
pub fn format_hz(f: f64) -> String {
    let (scaled, unit) = if f >= 1e9 {
        (f / 1e9, "GHz")
    } else if f >= 1e6 {
        (f / 1e6, "MHz")
    } else if f >= 1e3 {
        (f / 1e3, "KHz")
    } else {
        (f, "Hz")
    };
    let mut text = format!("{scaled:.3}");
    if text.contains('.') {
        text = text.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    format!("{text}{unit}")
}

/// One standard result-tab plot: axis labels, legend, scroll/drag/zoom
/// gestures, the one-time bounds application and the series draw were the
/// same nine lines chained at every tab — the caller now passes only the
/// plot's identity, height, labels, series and optional bounds.
pub fn plot_frame(
    name: &str,
    height: f32,
    x_label: &str,
    y_label: &str,
    series: &[PlotSeries],
    apply: Option<Bounds>,
    ui: &mut egui::Ui,
) {
    let plot = Plot::new(name)
        .height(height)
        .x_axis_label(x_label)
        .y_axis_label(y_label)
        .y_axis_min_width(56.0)
        .legend(Legend::default().position(egui_plot::Corner::RightTop))
        .show_grid(grid_on(ui))
        .show_background(true)
        .allow_scroll(true)
        .allow_drag(true)
        .allow_boxed_zoom(true);
    viewer_scope(ui, |ui| {
        plot.show(ui, |plot_ui| {
            if let Some(bounds) = apply {
                apply_bounds(plot_ui, &bounds);
            }
            draw(plot_ui, series, 2.0);
        });
    });
}

/// X scale of the AC panels, decided by the dB/Linear toggle.
///
/// (Moved here from app.rs — it is plot-layer vocabulary, and the cursor
/// plumbing below is the only place that needs to know what "dB" means.)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AcMagScale { #[default] Db, Linear }

/// The hover-label text of one AC panel.
pub enum AcCursor {
    /// `{name}\nf = … Hz\n{quantity} = …` with the panel's quantity label.
    Quantity(String),
    /// `{name}\nf = … Hz\nphase = …°`.
    Phase,
}

/// One AC panel: the standard [frame][`plot_frame`] plus the frequency
/// cursor plumbing all four panels share — dB mode draws log-spaced grid
/// lines (and, on quantity plots, engineering-notation Hz ticks), linear
/// mode plain ticks, and the hover label resolves the axis value (a
/// `log10` exponent in dB mode) back to Hz before printing.
pub fn ac_plot_frame(
    name: &str,
    height: f32,
    scale: AcMagScale,
    cursor: AcCursor,
    y_label: &str,
    series: &[PlotSeries],
    apply: Option<Bounds>,
    ui: &mut egui::Ui,
) {
    let mut plot = Plot::new(name)
        .height(height)
        .x_axis_label("frequency (Hz)")
        .y_axis_label(y_label)
        .y_axis_min_width(56.0)
        .legend(Legend::default().position(egui_plot::Corner::RightTop))
        .show_grid(grid_on(ui))
        .show_background(true)
        .allow_scroll(true)
        .allow_drag(true)
        .allow_boxed_zoom(true);

    if scale == AcMagScale::Db {
        plot = plot.x_grid_spacer(egui_plot::log_grid_spacer(10));
    }
    // Frequency ticks read `10Hz … 100KHz` on every AC panel (the dB-mode
    // axis carries log10(f), which must never be printed as a raw exponent).
    let frequency_of = |x: f64| match scale {
        AcMagScale::Db => 10.0_f64.powf(x),
        AcMagScale::Linear => x,
    };
    plot = plot.x_axis_formatter(move |mark, _range| format_hz(frequency_of(mark.value)));
    // Y ticks carry their unit: `18dB` on dB magnitudes, `-45°` on phase.
    match (&cursor, scale) {
        (AcCursor::Phase, _) => {
            plot = plot.y_axis_formatter(|mark, _range| degree_tick(mark));
        }
        (AcCursor::Quantity(_), AcMagScale::Db) => {
            plot = plot.y_axis_formatter(|mark, _range| db_tick(mark));
        }
        _ => {}
    }
    let plot = plot.label_formatter(move |name, val| {
        if name.is_empty() { return String::new(); }
        let f = match scale {
            AcMagScale::Db => 10.0_f64.powf(val.x),
            AcMagScale::Linear => val.x,
        };
        match &cursor {
            AcCursor::Quantity(q) => format!("{name}\nf = {f:.3e} Hz\n{q} = {:.3e}", val.y),
            AcCursor::Phase => format!("{name}\nf = {f:.3e} Hz\nphase = {:.2}°", val.y),
        }
    });
    viewer_scope(ui, |ui| {
        plot.show(ui, |plot_ui| {
            if let Some(bounds) = apply {
                apply_bounds(plot_ui, &bounds);
            }
            draw(plot_ui, series, 2.0);
        });
    });
}

/// Draw a batch of series in legend order.
pub fn draw(plot_ui: &mut PlotUi, series: &[PlotSeries], width: f32) {
    for s in series {
        plot_ui.line(
            Line::new(PlotPoints::new(s.points.clone()))
                .name(&s.name)
                .color(s.color)
                .width(width),
        );
    }
}

// ── Transient series ─────────────────────────────────────────────────────────

/// Deck-spelling label for a node (`in`, `out`); falls back to the node id.
fn node_name(names: &[String], node: usize) -> String {
    match names.get(node) {
        Some(s) if !s.is_empty() => s.clone(),
        _ => node.to_string(),
    }
}

/// (time, V(node)) for every enabled node, ground excluded.
pub fn tran_voltage_series(
    tran: &TranResult,
    enabled: &[bool],
    names: &[String],
    x_mult: f64,
    y_mult: f64,
) -> Vec<PlotSeries> {
    let mut out = Vec::new();
    for (node, &on) in enabled.iter().enumerate() {
        if !on || node == 0 {
            continue;
        }
        out.push(PlotSeries {
            name: format!("V({})", node_name(names, node)),
            // By emitted order, not node number: the first trace on screen is
            // always green, whichever node it is (as in LTspice).
            color: PLOT_COLORS[out.len() % PLOT_COLORS.len()],
            points: tran
                .points
                .iter()
                .map(|p| [p.time * x_mult, *p.node_voltages.get(node).unwrap_or(&0.0) * y_mult])
                .collect(),
        });
    }
    out
}

/// (time, V(a) − V(b)) — the differential trace between two nodes.
pub fn tran_diff_series(
    tran: &TranResult,
    a: usize,
    b: usize,
    names: &[String],
    x_mult: f64,
    y_mult: f64,
) -> Vec<PlotSeries> {
    vec![PlotSeries {
        name: format!("V({})−V({})", node_name(names, a), node_name(names, b)),
        color: PLOT_COLORS[0],
        points: tran
            .points
            .iter()
            .map(|p| {
                let va = p.node_voltages.get(a).copied().unwrap_or(0.0);
                let vb = p.node_voltages.get(b).copied().unwrap_or(0.0);
                [p.time * x_mult, (va - vb) * y_mult]
            })
            .collect(),
    }]
}

/// Labels for one family's report slots: the deck's element names, with the
/// family letter + index as the fallback when a report outgrows the list.
fn family_labels(circuit: &Circuit, family: ElementFamily, count: usize) -> Vec<String> {
    let names = circuit.element_names(family);
    (0..count)
        .map(|i| {
            names
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("{}{}", family.letter(), i + 1))
        })
        .collect()
}

/// All transient branch series in canonical family order — the one element
/// walk every tab shares. Series are labelled from the deck's element names
/// (`I(RLOAD)`, `Ic(Q1)`), not a positional guess.
pub fn tran_current_series(
    tran: &TranResult,
    circuit: &Circuit,
    x_mult: f64,
    y_mult: f64,
) -> Vec<PlotSeries> {
    let points = &tran.points;
    let mut out = Vec::new();
    let mut color_idx = 0usize;
    if let Some(first) = points.first() {
        for family in CURRENT_FAMILIES {
            let count = first.branch_currents.slice(family).unwrap_or(&[]).len();
            if count == 0 {
                continue;
            }
            for (i, label) in family_labels(circuit, family, count).into_iter().enumerate() {
                out.push(PlotSeries {
                    name: format!("{}({label})", family.probe_prefix()),
                    color: PLOT_COLORS[color_idx % PLOT_COLORS.len()],
                    points: points
                        .iter()
                        .map(|p| {
                            let cur = p
                                .branch_currents
                                .slice(family)
                                .and_then(|s| s.get(i).copied())
                                .unwrap_or_default();
                            [p.time * x_mult, cur * y_mult]
                        })
                        .collect(),
                });
                color_idx += 1;
            }
        }
    }
    out
}

// ── AC series ────────────────────────────────────────────────────────────────

/// What the y axis shows for an AC series.
#[derive(Clone, Copy, PartialEq)]
pub enum AcQuantity {
    /// |value| in dB (log-x).
    MagnitudeDb,
    /// |value| in linear units.
    MagnitudeLinear,
    /// Phase in degrees.
    Phase,
}

/// dB magnitude with the historical −200 floor for zero signals.
fn to_db(mag: f64) -> f64 {
    if mag > 0.0 { 20.0 * mag.log10() } else { -200.0 }
}

/// A series' x coordinate: log10 frequency for dB plots, linear otherwise.
fn ac_x(freq: f64, x_mult: f64, log_x: bool) -> f64 {
    let scaled = freq * x_mult;
    if log_x { scaled.log10() } else { scaled }
}

/// AC node-voltage series; magnitudes and phases were computed per plot
/// before — the math lives once here.
pub fn ac_voltage_series(
    ac: &AcResult,
    enabled: &[bool],
    names: &[String],
    qty: AcQuantity,
    x_mult: f64,
    y_mult: f64,
    log_x: bool,
) -> Vec<PlotSeries> {
    let mut out = Vec::new();
    for (node, &on) in enabled.iter().enumerate() {
        if !on || node == 0 {
            continue;
        }
        let suffix = match qty {
            AcQuantity::Phase => " phase",
            _ => "",
        };
        out.push(PlotSeries {
            name: format!("V({}){suffix}", node_name(names, node)),
            // By emitted order, not node number: the first trace on screen is
            // always green, whichever node it is (as in LTspice).
            color: PLOT_COLORS[out.len() % PLOT_COLORS.len()],
            points: ac
                .points
                .iter()
                .map(|p| {
                    let v = p.node_voltages.get(node).copied().unwrap_or_default();
                    let y = match qty {
                        AcQuantity::MagnitudeDb => to_db(v.norm()) * y_mult,
                        AcQuantity::MagnitudeLinear => v.norm() * y_mult,
                        AcQuantity::Phase => v.arg().to_degrees() * y_mult,
                    };
                    [ac_x(p.freq, x_mult, log_x), y]
                })
                .collect(),
        });
    }
    out
}

/// All AC branch series in canonical family order — the dB and the phase plot
/// walk this once. Series are labelled from the deck's element names.
pub fn ac_current_series(
    ac: &AcResult,
    circuit: &Circuit,
    qty: AcQuantity,
    x_mult: f64,
    y_mult: f64,
    log_x: bool,
) -> Vec<PlotSeries> {
    let points = &ac.points;
    let mut out = Vec::new();
    let mut color_idx = 0usize;
    let suffix = match qty {
        AcQuantity::Phase => " phase",
        _ => "",
    };
    if let Some(first) = points.first() {
        for family in CURRENT_FAMILIES {
            let count = first.branch_currents.slice(family).unwrap_or(&[]).len();
            if count == 0 {
                continue;
            }
            for (i, label) in family_labels(circuit, family, count).into_iter().enumerate() {
                out.push(PlotSeries {
                    name: format!("{}({label}){suffix}", family.probe_prefix()),
                    color: PLOT_COLORS[color_idx % PLOT_COLORS.len()],
                    points: points
                        .iter()
                        .map(|p| {
                            let v = p
                                .branch_currents
                                .slice(family)
                                .and_then(|s| s.get(i).copied())
                                .unwrap_or_default();
                            let y = match qty {
                                AcQuantity::MagnitudeDb => to_db(v.norm()) * y_mult,
                                AcQuantity::MagnitudeLinear => v.norm() * y_mult,
                                AcQuantity::Phase => v.arg().to_degrees() * y_mult,
                            };
                            [ac_x(p.freq, x_mult, log_x), y]
                        })
                        .collect(),
                });
                color_idx += 1;
            }
        }
    }
    out
}

// ── DC sweep series ──────────────────────────────────────────────────────────

/// (sweep value, V(node)) series for a flat (single-sweep) `.dc`; ground never
/// gets a trace. A nested sweep is better shown as a table — the DC tab renders
/// one then — so these series carry the single-sweep case only.
pub fn dc_sweep_series(
    sweep: &DcSweepResult,
    enabled: &[bool],
    names: &[String],
    x_mult: f64,
    y_mult: f64,
) -> Vec<PlotSeries> {
    let mut out = Vec::new();
    for (node, &on) in enabled.iter().enumerate() {
        if !on || node == 0 {
            continue;
        }
        out.push(PlotSeries {
            name: format!("V({})", node_name(names, node)),
            // By emitted order, not node number: the first trace on screen is
            // always green, whichever node it is (as in LTspice).
            color: PLOT_COLORS[out.len() % PLOT_COLORS.len()],
            points: sweep
                .points
                .iter()
                .map(|p| [
                    p.values.first().copied().unwrap_or(0.0) * x_mult,
                    p.node_voltages.get(node).copied().unwrap_or(0.0) * y_mult,
                ])
                .collect(),
        });
    }
    out
}

// ── Waveform arithmetic ──────────────────────────────────────────────────────

/// The expression traces of the Waveforms tab (TODO §6, LTspice's "Add
/// Trace"): the first run's transient plot as `.raw` columns plus the
/// expressions typed against it, each evaluated once when added (or when a
/// new run replaces the data) rather than per frame. The expression
/// grammar and evaluator are the core's `arith` module — one law for the
/// CLI exporters and this viewer.
#[derive(Default)]
pub struct ExpressionTraces {
    base: Option<RawDataset>,
    env: ParamEnv,
    traces: Vec<(TraceExpr, Vec<f64>)>,
}

impl ExpressionTraces {
    /// A new run's data: expressions already on screen re-evaluate against
    /// it; one the new deck cannot evaluate (a probe it no longer has)
    /// drops out quietly.
    pub fn rebase(&mut self, base: Option<RawDataset>, env: ParamEnv) {
        self.base = base;
        self.env = env;
        let texts: Vec<String> = self.traces.drain(..).map(|(e, _)| e.text().to_string()).collect();
        for text in texts {
            let _ = self.add(&text);
        }
    }

    /// Whether a transient plot is available to compute against.
    pub fn has_data(&self) -> bool {
        self.base.is_some()
    }

    /// Add one expression; a repeat (case-insensitive) is a no-op. Errors
    /// are display text: the syntax problem, or the probe the plot lacks.
    pub fn add(&mut self, text: &str) -> Result<(), String> {
        let Some(base) = &self.base else {
            return Err("no transient plot to compute against — run a .tran first".to_string());
        };
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let expr = TraceExpr::parse(text, &self.env).map_err(|e| e.to_string())?;
        if self
            .traces
            .iter()
            .any(|(t, _)| t.text().eq_ignore_ascii_case(expr.text()))
        {
            return Ok(());
        }
        let values = expr
            .evaluate(base)
            .ok_or_else(|| format!("'{}' names a trace this plot does not have", expr.text()))?;
        self.traces
            .push((expr, values.into_iter().map(|z| z.re).collect()));
        Ok(())
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.traces.len() {
            self.traces.remove(index);
        }
    }

    pub fn clear(&mut self) {
        self.traces.clear();
    }

    /// The expressions as typed, in the order they were added.
    pub fn names(&self) -> Vec<String> {
        self.traces.iter().map(|(e, _)| e.text().to_string()).collect()
    }

    /// One series per expression, colored by order added (first is green).
    pub fn series(&self, x_mult: f64, y_mult: f64) -> Vec<PlotSeries> {
        let Some(base) = &self.base else {
            return Vec::new();
        };
        let axis = base.axis();
        self.traces
            .iter()
            .enumerate()
            .map(|(slot, (expr, values))| PlotSeries {
                name: expr.text().to_string(),
                color: PLOT_COLORS[slot % PLOT_COLORS.len()],
                points: axis
                    .iter()
                    .zip(values)
                    .filter(|(x, y)| x.is_finite() && y.is_finite())
                    .map(|(x, y)| [x * x_mult, y * y_mult])
                    .collect(),
            })
            .collect()
    }
}

/// The transient plot the viewer computes expressions against: the first
/// run's `Transient Analysis` dataset of a built `.raw` file.
pub fn first_transient_plot(raw: circuitsim::rawwave::RawFile) -> Option<RawDataset> {
    raw.datasets
        .into_iter()
        .find(|ds| ds.plotname.eq_ignore_ascii_case("Transient Analysis"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use circuitsim::{parse, run};

    const DIVIDER: &str = "V1 in 0 dc 5\nR1 in out 1k\nR2 out 0 3k\n.tran 10u 100u\n.end\n";

    fn divider_traces() -> ExpressionTraces {
        let netlist = parse(DIVIDER).expect("deck parses");
        let result = run(&netlist).expect("deck runs");
        let raw = circuitsim::rawwave::build("t", &netlist, &result);
        let mut traces = ExpressionTraces::default();
        traces.rebase(first_transient_plot(raw), netlist.param_env());
        traces
    }

    #[test]
    fn expression_series_carry_the_computed_values_in_axis_units() {
        let mut traces = divider_traces();
        traces.add("V(out)*I(V1)").unwrap();
        traces.add("2*V(out)+1").unwrap();
        let series = traces.series(1.0, 1.0);
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].name, "V(out)*I(V1)");
        assert!((series[0].points[3][1].abs() - 4.6875e-3).abs() < 1e-8);
        assert!((series[1].points[3][1] - 8.5).abs() < 1e-9);
        assert_eq!(series[0].color, PLOT_COLORS[0]);
        assert_eq!(series[1].color, PLOT_COLORS[1]);
        let scaled = traces.series(1e6, 1e3);
        assert!((scaled[1].points[3][1] - 8500.0).abs() < 1e-6);
        assert!((scaled[1].points[3][0] - series[1].points[3][0] * 1e6).abs() < 1e-6);
    }

    #[test]
    fn bad_expressions_report_and_leave_the_set_alone() {
        let mut traces = divider_traces();
        traces.add("V(out)*2").unwrap();
        assert!(traces.add("V(out").unwrap_err().contains("missing"));
        assert!(traces.add("V(nope)*2").unwrap_err().contains("V(nope)*2"));
        assert_eq!(traces.names(), vec!["V(out)*2".to_string()]);
        // Repeats and blanks are no-ops.
        traces.add("v(OUT)*2").unwrap();
        traces.add("   ").unwrap();
        assert_eq!(traces.names().len(), 1);
    }

    #[test]
    fn a_new_run_re_evaluates_and_drops_what_it_can_no_longer_compute() {
        let mut traces = divider_traces();
        traces.add("V(out)*2").unwrap();
        traces.add("V(in)*3").unwrap();
        let other = parse("V1 a 0 dc 2\nR1 a out 1k\nR2 out 0 1k\n.tran 10u 100u\n.end\n").unwrap();
        let result = run(&other).unwrap();
        let raw = circuitsim::rawwave::build("t", &other, &result);
        traces.rebase(first_transient_plot(raw), other.param_env());
        assert_eq!(traces.names(), vec!["V(out)*2".to_string()]);
        assert!((traces.series(1.0, 1.0)[0].points[2][1] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn without_a_transient_plot_nothing_adds() {
        let mut traces = ExpressionTraces::default();
        assert!(!traces.has_data());
        assert!(traces.add("V(out)").unwrap_err().contains(".tran"));
        assert!(traces.series(1.0, 1.0).is_empty());
    }

    #[test]
    fn remove_and_clear_drop_traces() {
        let mut traces = divider_traces();
        traces.add("V(out)").unwrap();
        traces.add("V(in)").unwrap();
        traces.remove(0);
        assert_eq!(traces.names(), vec!["V(in)".to_string()]);
        traces.remove(9);
        traces.clear();
        assert!(traces.names().is_empty());
    }

    /// Three-node chain, so a plot can omit node 1 and still have traces.
    const NODES_ONLY_2_AND_3: [bool; 4] = [false, false, true, true];

    fn names(netlist: &circuitsim::Netlist) -> Vec<String> {
        netlist.circuit.node_names.clone()
    }

    /// Whatever node a plot starts at, its first trace is the palette's
    /// first colour (green) and the rest follow in emitted order.
    fn assert_colours_follow_emitted_order(series: &[PlotSeries]) {
        assert_eq!(series.len(), 2, "two nodes enabled");
        assert_eq!(series[0].color, PLOT_COLORS[0], "{}", series[0].name);
        assert_eq!(series[1].color, PLOT_COLORS[1], "{}", series[1].name);
    }

    #[test]
    fn transient_voltage_colours_follow_emitted_order() {
        let n = parse("V1 1 0 DC 5\nR1 1 2 1k\nR2 2 3 1k\nC1 3 0 1u\n.tran 10u 1m UIC\n.end\n")
            .expect("deck parses");
        let tran = run(&n).expect("deck runs").tran.expect("transient result");
        let series = tran_voltage_series(&tran, &NODES_ONLY_2_AND_3, &names(&n), 1.0, 1.0);
        assert_colours_follow_emitted_order(&series);
    }

    #[test]
    fn ac_voltage_colours_follow_emitted_order() {
        let n = parse("V1 1 0 AC 1\nR1 1 2 1k\nR2 2 3 1k\nC1 3 0 100n\n.ac dec 5 10 100k\n.end\n")
            .expect("deck parses");
        let ac = run(&n).expect("deck runs").ac.expect("ac result");
        let series = ac_voltage_series(
            &ac, &NODES_ONLY_2_AND_3, &names(&n), AcQuantity::MagnitudeDb, 1.0, 1.0, false,
        );
        assert_colours_follow_emitted_order(&series);
    }

    #[test]
    fn dc_sweep_voltage_colours_follow_emitted_order() {
        let n = parse("V1 1 0 DC 0\nR1 1 2 1k\nR2 2 3 1k\nR3 3 0 1k\n.dc V1 0 5 1\n.end\n")
            .expect("deck parses");
        let sweep = run(&n).expect("deck runs").dc_sweep.expect("dc sweep result");
        let series = dc_sweep_series(&sweep, &NODES_ONLY_2_AND_3, &names(&n), 1.0, 1.0);
        assert_colours_follow_emitted_order(&series);
    }
}
