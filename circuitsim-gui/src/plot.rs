//! The plot layer: typed axis state, the one bounds-application routine, and
//! the adapters that turn simulation results into named line series.
//!
//! Previously every plot carried its own copy of the bounds/apply logic (five
//! near-identical blocks guarded by string-parsing of twelve always-empty
//! fields) and every tab re-wrote the same element walk with its own copy of
//! the dB/phase math (twenty loops). One owner each here; the tabs only draw.

use circuitsim::analysis::{AcPoint, AcResult, TranPoint, TranResult};
use circuitsim::mna::{AcBranchCurrents, BranchCurrents};
use egui_plot::{Line, PlotPoints, PlotUi};

use crate::theme::PLOT_COLORS;

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

/// (time, V(node)) for every enabled node, ground excluded.
pub fn tran_voltage_series(
    tran: &TranResult,
    enabled: &[bool],
    x_mult: f64,
    y_mult: f64,
) -> Vec<PlotSeries> {
    let mut out = Vec::new();
    for (node, &on) in enabled.iter().enumerate() {
        if !on || node == 0 {
            continue;
        }
        out.push(PlotSeries {
            name: format!("V({node})"),
            color: PLOT_COLORS[node % PLOT_COLORS.len()],
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
    x_mult: f64,
    y_mult: f64,
) -> Vec<PlotSeries> {
    vec![PlotSeries {
        name: format!("V({a})−V({b})"),
        color: PLOT_COLORS[1 % PLOT_COLORS.len()],
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

fn branch_count(points: &[TranPoint], pick: fn(&BranchCurrents) -> usize) -> usize {
    points
        .first()
        .map(|p| pick(&p.branch_currents))
        .unwrap_or(0)
}

/// All transient branch series in canonical legend order R, C, L, V, D — the
/// one element walk that replaced a copy per tab.
pub fn tran_current_series(tran: &TranResult, x_mult: f64, y_mult: f64) -> Vec<PlotSeries> {
    let points: &[TranPoint] = &tran.points;
    let mut out = Vec::new();
    let mut color_idx = 0usize;

    let families: [(&str, fn(&BranchCurrents) -> usize); 5] = [
        ("R", |b: &BranchCurrents| b.resistors.len()),
        ("C", |b: &BranchCurrents| b.capacitors.len()),
        ("L", |b: &BranchCurrents| b.inductors.len()),
        ("V", |b: &BranchCurrents| b.voltage_sources.len()),
        ("D", |b: &BranchCurrents| b.diodes.len()),
    ];
    for (letter, len_of) in families {
        for i in 0..branch_count(points, len_of) {
            out.push(PlotSeries {
                name: format!("I({letter}{})", i + 1),
                color: PLOT_COLORS[color_idx % PLOT_COLORS.len()],
                points: points
                    .iter()
                    .map(|p| {
                        let b = &p.branch_currents;
                        let v = match letter {
                            "R" => *b.resistors.get(i).unwrap_or(&0.0),
                            "C" => *b.capacitors.get(i).unwrap_or(&0.0),
                            "L" => *b.inductors.get(i).unwrap_or(&0.0),
                            "V" => *b.voltage_sources.get(i).unwrap_or(&0.0),
                            _ => *b.diodes.get(i).unwrap_or(&0.0),
                        };
                        [p.time * x_mult, v * y_mult]
                    })
                    .collect(),
            });
            color_idx += 1;
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
            name: format!("V({node}){suffix}"),
            color: PLOT_COLORS[node % PLOT_COLORS.len()],
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

fn ac_count(points: &[AcPoint], pick: fn(&AcBranchCurrents) -> usize) -> usize {
    points
        .first()
        .map(|p| pick(&p.branch_currents))
        .unwrap_or(0)
}

/// All AC branch series in canonical legend order R, C, L, V, D — both the dB
/// and the phase plot walk this once instead of five loops each.
pub fn ac_current_series(
    ac: &AcResult,
    qty: AcQuantity,
    x_mult: f64,
    y_mult: f64,
    log_x: bool,
) -> Vec<PlotSeries> {
    let points: &[AcPoint] = &ac.points;
    let mut out = Vec::new();
    let mut color_idx = 0usize;

    let families: [(&str, fn(&AcBranchCurrents) -> usize); 5] = [
        ("R", |b: &AcBranchCurrents| b.resistors.len()),
        ("C", |b: &AcBranchCurrents| b.capacitors.len()),
        ("L", |b: &AcBranchCurrents| b.inductors.len()),
        ("V", |b: &AcBranchCurrents| b.voltage_sources.len()),
        ("D", |b: &AcBranchCurrents| b.diodes.len()),
    ];
    let suffix = match qty {
        AcQuantity::Phase => " phase",
        _ => "",
    };
    for (letter, len_of) in families {
        for i in 0..ac_count(points, len_of) {
            out.push(PlotSeries {
                name: format!("I({letter}{}){suffix}", i + 1),
                color: PLOT_COLORS[color_idx % PLOT_COLORS.len()],
                points: points
                    .iter()
                    .map(|p| {
                        let b = &p.branch_currents;
                        let v = match letter {
                            "R" => b.resistors.get(i).copied().unwrap_or_default(),
                            "C" => b.capacitors.get(i).copied().unwrap_or_default(),
                            "L" => b.inductors.get(i).copied().unwrap_or_default(),
                            "V" => b.voltage_sources.get(i).copied().unwrap_or_default(),
                            _ => b.diodes.get(i).copied().unwrap_or_default(),
                        };
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
    out
}

