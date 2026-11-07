//! LTspice-style waveform viewer (Classic theme).
//!
//! A custom-painted plot that mimics the LTspice waveform window: a black
//! surface with a thin frame, bold tick labels (`10V`, `1KHz`, `-9°`), the
//! trace names centred above the frame, an optional right-hand phase axis for
//! AC results, rubber-band zoom, cursors with the floating readout window,
//! the right-click menu and the axis-limit dialogs.
//!
//! Everything here is theme independent (local colour constants); the app
//! only mounts it when the Classic theme is active.
#![allow(dead_code)]

use egui::{
    pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui,
};

use crate::theme::bold;

// ─── Colours (sampled from the reference screenshots) ───────────────────────

const BG: Color32 = Color32::BLACK;
const FRAME: Color32 = Color32::from_rgb(190, 190, 190);
const TEXT: Color32 = Color32::from_rgb(235, 235, 235);
const GRID: Color32 = Color32::from_rgb(70, 70, 70);
const CURSOR: Color32 = Color32::WHITE;

// ─── Public interface ───────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum XKind {
    Time,
    Frequency,
    Sweep,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// One quantity per trace on the left axis.
    Real,
    /// Magnitude on the left axis, phase (dashed) on the right axis.
    Ac,
}

pub struct Trace {
    pub name: String,
    pub color: Color32,
    /// Ascending x in SI units.
    pub x: Vec<f64>,
    /// Real: the quantity. Ac: magnitude (dB when `mag_db`, else linear).
    pub y: Vec<f64>,
    /// Ac only: phase in degrees, same length as `x`.
    pub phase: Option<Vec<f64>>,
}

pub struct ViewerConfig {
    pub title: String,
    pub x_kind: XKind,
    pub x_unit: String,
    pub y_unit: String,
    pub mode: Mode,
    pub mag_db: bool,
    pub log_x_default: bool,
}

impl ViewerConfig {
    fn left_unit(&self) -> &str {
        if self.mode == Mode::Ac && self.mag_db { "dB" } else { &self.y_unit }
    }

    fn x_unit_str(&self) -> &str {
        match self.x_kind {
            XKind::Time => "s",
            XKind::Frequency => "Hz",
            XKind::Sweep => &self.x_unit,
        }
    }

    fn x_name(&self) -> &'static str {
        match self.x_kind {
            XKind::Time => "time",
            XKind::Frequency => "frequency",
            XKind::Sweep => "sweep",
        }
    }
}

#[derive(Default)]
pub struct ViewerOutput {
    pub hovered_trace: Option<usize>,
    pub request_add_traces: bool,
    pub request_export_csv: bool,
    pub request_settings: bool,
    pub hint: Option<String>,
}

// ─── Formatting and parsing ─────────────────────────────────────────────────

const PREFIXES: [(&str, i32); 9] = [
    ("f", -15), ("p", -12), ("n", -9), ("µ", -6), ("m", -3), ("", 0), ("K", 3), ("M", 6), ("G", 9),
];

fn prefix_for(exp3: i32) -> &'static str {
    PREFIXES.iter().find(|(_, e)| *e == exp3).map_or("", |(p, _)| p)
}

/// Units that never take an SI prefix.
fn unitless_prefix(unit: &str) -> bool {
    matches!(unit, "dB" | "°" | "")
}

/// The multiple-of-three decade exponent of `v`, clamped to the table.
fn exp3_of(v: f64) -> i32 {
    if v == 0.0 || !v.is_finite() {
        return 0;
    }
    let e = (v.abs().log10() + 1e-9).floor() as i32;
    (e.div_euclid(3) * 3).clamp(-15, 9)
}

fn trim_zeros(s: String) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// A tick label the way LTspice prints it: value + SI prefix + unit, no
/// space (`10V`, `-110V`, `0ms`, `1KHz`, `18dB`). `step` is the axis' tick
/// spacing; it fixes the digits shown and the prefix of a zero tick.
pub(crate) fn fmt_si_unit(value: f64, step: f64, unit: &str) -> String {
    if !value.is_finite() {
        return String::new();
    }
    if unitless_prefix(unit) {
        let dec = decimals_for(step).min(6);
        let t = trim_zeros(format!("{value:.dec$}"));
        let t = if t == "-0" { "0".to_string() } else { t };
        return format!("{t}{unit}");
    }
    let reference = if value == 0.0 { step } else { value };
    let e3 = exp3_of(reference);
    let scale = 10f64.powi(e3);
    let dec = decimals_for(step / scale).min(6);
    let mant = value / scale;
    let t = trim_zeros(format!("{mant:.dec$}"));
    let t = if t == "-0" { "0".to_string() } else { t };
    format!("{t}{}{unit}", prefix_for(e3))
}

/// Decimal places a tick step needs: 1 → 0, 0.5 → 1, 0.25 → 2.
fn decimals_for(step: f64) -> usize {
    if !step.is_finite() || step <= 0.0 {
        return 0;
    }
    let mut dec = 0usize;
    let mut s = step;
    while dec < 9 && (s - s.round()).abs() > 1e-6 * s.max(1.0) {
        s *= 10.0;
        dec += 1;
    }
    dec
}

/// Eight significant digits with the SI prefix, as the cursor window shows
/// them (`48.348674ms`, `84.714771V`, `-41.482053°`).
pub(crate) fn fmt_sig(value: f64, unit: &str) -> String {
    if !value.is_finite() {
        return "--".into();
    }
    if value == 0.0 {
        return format!("0{unit}");
    }
    let mut e3 = if unitless_prefix(unit) { 0 } else { exp3_of(value) };
    loop {
        let scale = 10f64.powi(e3);
        let mant = value / scale;
        let int_digits = (mant.abs().log10().floor() as i32 + 1).max(1) as usize;
        let dec = 8usize.saturating_sub(int_digits);
        let text = format!("{mant:.dec$}");
        let rounded: f64 = text.parse().unwrap_or(mant);
        if rounded.abs() >= 1000.0 && e3 < 9 && !unitless_prefix(unit) {
            e3 += 3;
            continue;
        }
        return format!("{text}{}{unit}", prefix_for(e3));
    }
}

/// Parse a limit field: `30V`, `-200`, `100m`, `1k`, `1Meg`, `2.5KHz`,
/// `10ms`, `3e-3`. The unit suffix is optional. SPICE spelling: `m` is
/// milli, `Meg` is mega — except for `Hz`, where a bare `M` is mega.
pub(crate) fn parse_si_unit(text: &str, unit: &str) -> Option<f64> {
    let t = text.trim();
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let digits_start = i;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    let number: f64 = t[..i].parse().ok()?;
    let mut rest = t[i..].trim().to_string();
    if !unit.is_empty() && rest.to_lowercase().ends_with(&unit.to_lowercase()) {
        let cut = rest.len() - unit.len();
        if rest.is_char_boundary(cut) {
            rest.truncate(cut);
        }
    }
    let rest = rest.trim().to_lowercase();
    let mult = if rest.is_empty() {
        1.0
    } else if rest.starts_with("meg") {
        1e6
    } else {
        match rest.chars().next()? {
            't' => 1e12,
            'g' => 1e9,
            'k' => 1e3,
            'm' => if unit == "Hz" && rest == "m" { 1e6 } else { 1e-3 },
            'u' | 'µ' => 1e-6,
            'n' => 1e-9,
            'p' => 1e-12,
            'f' => 1e-15,
            _ => return None,
        }
    };
    let v = number * mult;
    v.is_finite().then_some(v)
}

// ─── Ticks ──────────────────────────────────────────────────────────────────

/// The step candidates (per decade) an axis of this unit prefers.
fn step_candidates(unit: &str) -> &'static [f64] {
    match unit {
        "dB" => &[1.0, 2.0, 3.0, 5.0, 6.0],
        "°" => &[1.0, 1.5, 2.0, 3.0, 4.5, 5.0, 6.0, 9.0],
        _ => &[1.0, 2.0, 5.0],
    }
}

/// Smallest nice step that gives at most about `divs` divisions of `range`.
pub(crate) fn nice_step(range: f64, divs: f64, unit: &str) -> f64 {
    if !range.is_finite() || range <= 0.0 {
        return 1.0;
    }
    let raw = range / divs;
    let mut decade = 10f64.powf(raw.log10().floor());
    let limit = divs * 1.12;
    for _ in 0..3 {
        for &c in step_candidates(unit).iter().chain(std::iter::once(&10.0)) {
            let step = c * decade;
            if range / step <= limit {
                return step;
            }
        }
        decade *= 10.0;
    }
    decade
}

/// Multiples of `step` inside `[lo, hi]` (inclusive, float-tolerant).
pub(crate) fn linear_ticks(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    if !(step > 0.0) || !lo.is_finite() || !hi.is_finite() || hi < lo {
        return Vec::new();
    }
    let first = (lo / step - 1e-9).ceil();
    let last = (hi / step + 1e-9).floor();
    if last - first > 2000.0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut k = first;
    while k <= last {
        let v = k * step;
        out.push(if v.abs() < step * 1e-9 { 0.0 } else { v });
        k += 1.0;
    }
    out
}

/// Decade (major) and 2..9 (minor) ticks of a log axis over `[lo, hi]`.
pub(crate) fn log_ticks(lo: f64, hi: f64) -> (Vec<f64>, Vec<f64>) {
    if !(lo > 0.0) || !(hi > lo) {
        return (Vec::new(), Vec::new());
    }
    let first = lo.log10().floor() as i32;
    let last = hi.log10().ceil() as i32;
    let (mut major, mut minor) = (Vec::new(), Vec::new());
    for d in first..=last.min(first + 60) {
        let base = 10f64.powi(d);
        for m in 1..=9 {
            let v = base * m as f64;
            if v < lo * (1.0 - 1e-9) || v > hi * (1.0 + 1e-9) {
                continue;
            }
            if m == 1 { major.push(v) } else { minor.push(v) }
        }
    }
    (major, minor)
}

/// Auto range of a value axis: data span padded out to tick multiples.
pub(crate) fn auto_range(min: f64, max: f64, divs: f64, unit: &str) -> (f64, f64, f64) {
    let (mut lo, mut hi) = (min, max);
    if !lo.is_finite() || !hi.is_finite() {
        return (0.0, 1.0, 0.1);
    }
    if (hi - lo).abs() < f64::EPSILON * lo.abs().max(1.0) * 4.0 {
        let pad = if lo == 0.0 { 1.0 } else { lo.abs() * 0.1 };
        lo -= pad;
        hi += pad;
    }
    let step = nice_step(hi - lo, divs, unit);
    ((lo / step).floor() * step, (hi / step).ceil() * step, step)
}

// ─── Data helpers ───────────────────────────────────────────────────────────

/// Linear interpolation of `y` at `at` (in log x when `log_x`).
pub(crate) fn interp(x: &[f64], y: &[f64], at: f64, log_x: bool) -> Option<f64> {
    let n = x.len().min(y.len());
    if n == 0 || !at.is_finite() {
        return None;
    }
    if n == 1 {
        return (at == x[0]).then_some(y[0]);
    }
    if at < x[0] || at > x[n - 1] {
        return None;
    }
    let k = x[..n].partition_point(|&v| v < at);
    if k == 0 {
        return Some(y[0]);
    }
    let (x0, x1, y0, y1) = (x[k - 1], x[k], y[k - 1], y[k]);
    if x1 == x0 {
        return Some(y1);
    }
    let t = if log_x && x0 > 0.0 && x1 > 0.0 && at > 0.0 {
        (at.ln() - x0.ln()) / (x1.ln() - x0.ln())
    } else {
        (at - x0) / (x1 - x0)
    };
    Some(y0 + (y1 - y0) * t)
}

/// Remove ±360° jumps from a phase series (degrees).
pub(crate) fn unwrap_phase_deg(phase: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(phase.len());
    let mut offset = 0.0;
    for (i, &p) in phase.iter().enumerate() {
        if i > 0 {
            let prev = phase[i - 1];
            let d = p - prev;
            if d > 180.0 {
                offset -= 360.0;
            } else if d < -180.0 {
                offset += 360.0;
            }
        }
        out.push(p + offset);
    }
    out
}

/// Group delay `−dφ/dω` in seconds at `at` Hz, central difference on the
/// unwrapped phase around the nearest sample.
pub(crate) fn group_delay(freq: &[f64], phase_deg: &[f64], at: f64) -> Option<f64> {
    let n = freq.len().min(phase_deg.len());
    if n < 2 || !at.is_finite() {
        return None;
    }
    let ph = unwrap_phase_deg(&phase_deg[..n]);
    let k = freq[..n].partition_point(|&f| f < at).min(n - 1);
    let k = if k > 0 && (at - freq[k - 1]).abs() < (freq[k] - at).abs() { k - 1 } else { k };
    let lo = k.saturating_sub(1);
    let hi = (k + 1).min(n - 1);
    let df = freq[hi] - freq[lo];
    if df == 0.0 {
        return None;
    }
    let dphi_rad = (ph[hi] - ph[lo]).to_radians();
    Some(-(dphi_rad / df) / (2.0 * std::f64::consts::PI))
}

// ─── State ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AcQty {
    Mag,
    Phase,
    GroupDelay,
}

#[derive(Clone, Debug)]
struct Cursor {
    trace: usize,
    x: f64,
    qty: AcQty,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AxisId {
    X,
    Y,
    R,
}

struct AxisDialog {
    which: AxisId,
    unit: String,
    quantity: String,
    lo: String,
    hi: String,
    tick: String,
    log: bool,
    bad: [bool; 3],
}

#[derive(Clone, Default)]
struct Snapshot {
    x: Option<(f64, f64)>,
    y: Option<(f64, f64)>,
    r: Option<(f64, f64)>,
}

enum Drag {
    Zoom { origin: Pos2 },
    Cursor(usize),
}

#[derive(Default)]
pub struct ViewerState {
    x: Option<(f64, f64)>,
    y: Option<(f64, f64)>,
    r: Option<(f64, f64)>,
    x_tick: Option<f64>,
    y_tick: Option<f64>,
    r_tick: Option<f64>,
    x_log: Option<bool>,
    y_log: bool,
    zoom_stack: Vec<Snapshot>,
    grid: bool,
    active: Option<usize>,
    cursors: [Option<Cursor>; 2],
    pending_place: u8,
    cursor_window_hidden: bool,
    drag: Option<Drag>,
    dialog: Option<AxisDialog>,
    dialog_open_request: Option<AxisId>,
}

/// Everything resolved for one frame: windows, ticks, rects.
struct View {
    plot: Rect,
    x: (f64, f64),
    x_log: bool,
    x_ticks: (Vec<f64>, Vec<f64>, f64),
    y: (f64, f64),
    y_log: bool,
    y_ticks: (Vec<f64>, Vec<f64>, f64),
    r: Option<((f64, f64), (Vec<f64>, f64))>,
}

fn to_frac(v: f64, lo: f64, hi: f64, log: bool) -> f64 {
    if log {
        if v <= 0.0 || lo <= 0.0 || hi <= 0.0 {
            return f64::NAN;
        }
        (v.ln() - lo.ln()) / (hi.ln() - lo.ln())
    } else {
        (v - lo) / (hi - lo)
    }
}

fn from_frac(f: f64, lo: f64, hi: f64, log: bool) -> f64 {
    if log && lo > 0.0 && hi > 0.0 {
        (lo.ln() + f * (hi.ln() - lo.ln())).exp()
    } else {
        lo + f * (hi - lo)
    }
}

impl View {
    fn sx(&self, v: f64) -> f32 {
        let f = to_frac(v, self.x.0, self.x.1, self.x_log);
        self.plot.left() + (f as f32) * self.plot.width()
    }
    fn sy(&self, v: f64) -> f32 {
        let f = to_frac(v, self.y.0, self.y.1, self.y_log);
        self.plot.bottom() - (f as f32) * self.plot.height()
    }
    fn sr(&self, v: f64) -> f32 {
        let r = self.r.as_ref().map_or((0.0, 1.0), |r| r.0);
        self.plot.bottom() - (to_frac(v, r.0, r.1, false) as f32) * self.plot.height()
    }
    fn x_at(&self, px: f32) -> f64 {
        from_frac(((px - self.plot.left()) / self.plot.width()) as f64, self.x.0, self.x.1, self.x_log)
    }
}

impl ViewerState {
    pub fn zoom_to_fit(&mut self) {
        self.push_zoom();
        self.x = None;
        self.y = None;
        self.r = None;
        self.x_tick = None;
        self.y_tick = None;
        self.r_tick = None;
    }

    pub fn autorange_y(&mut self) {
        self.push_zoom();
        self.y = None;
        self.r = None;
        self.y_tick = None;
        self.r_tick = None;
    }

    pub fn place_cursor_on_active(&mut self) {
        self.pending_place = self.pending_place.saturating_add(1);
    }

    pub fn clear_cursors(&mut self) {
        self.cursors = [None, None];
        self.pending_place = 0;
    }

    pub fn has_cursors(&self) -> bool {
        self.cursors.iter().any(Option::is_some)
    }

    pub fn set_grid(&mut self, on: bool) {
        self.grid = on;
    }

    pub fn grid(&self) -> bool {
        self.grid
    }

    pub fn set_active_trace(&mut self, idx: Option<usize>) {
        self.active = idx;
    }

    fn push_zoom(&mut self) {
        self.zoom_stack.push(Snapshot { x: self.x, y: self.y, r: self.r });
        if self.zoom_stack.len() > 32 {
            self.zoom_stack.remove(0);
        }
    }

    fn zoom_back(&mut self) {
        if let Some(s) = self.zoom_stack.pop() {
            self.x = s.x;
            self.y = s.y;
            self.r = s.r;
        }
    }

    // ── Frame ───────────────────────────────────────────────────────────────

    pub fn show(&mut self, ui: &mut Ui, cfg: &ViewerConfig, traces: &[Trace]) -> ViewerOutput {
        let mut out = ViewerOutput::default();
        let size = ui.available_size().max(vec2(120.0, 80.0));
        let (outer, _) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter_at(outer);
        painter.rect_filled(outer, 0.0, BG);

        // Housekeeping against a changed trace set.
        if let Some(a) = self.active {
            if a >= traces.len() {
                self.active = None;
            }
        }
        if self.active.is_none() && !traces.is_empty() {
            self.active = Some(0);
        }
        for c in self.cursors.iter_mut() {
            if c.as_ref().map_or(false, |c| c.trace >= traces.len()) {
                *c = None;
            }
        }

        let ac = cfg.mode == Mode::Ac;
        let plot = Rect::from_min_max(
            pos2(outer.left() + 58.0, outer.top() + 20.0),
            pos2(outer.right() - if ac { 46.0 } else { 14.0 }, outer.bottom() - 22.0),
        );
        if plot.width() < 40.0 || plot.height() < 30.0 {
            return out;
        }
        let view = self.resolve(plot, cfg, traces);

        // Place pending cursors now that the data and window are known.
        while self.pending_place > 0 {
            self.pending_place -= 1;
            self.place_now(&view, cfg, traces);
        }

        self.paint_axes(&painter, &view, cfg);
        self.paint_traces(&painter, &view, cfg, traces);
        self.paint_names(ui, &painter, plot, traces);
        self.paint_cursors(&painter, &view, cfg, traces);

        // ── Interaction ─────────────────────────────────────────────────────
        let id = ui.id().with("lt_viewer");
        let plot_resp = ui.interact(plot, id.with("plot"), Sense::click_and_drag());
        let x_strip = Rect::from_min_max(pos2(plot.left(), plot.bottom()), pos2(plot.right(), outer.bottom()));
        let y_strip = Rect::from_min_max(pos2(outer.left(), plot.top()), pos2(plot.left(), plot.bottom()));
        let r_strip = Rect::from_min_max(pos2(plot.right(), plot.top()), pos2(outer.right(), plot.bottom()));
        let x_resp = ui.interact(x_strip, id.with("xs"), Sense::click());
        let y_resp = ui.interact(y_strip, id.with("ys"), Sense::click());
        let r_resp = ui.interact(r_strip, id.with("rs"), Sense::click());
        if x_resp.secondary_clicked() {
            self.dialog_open_request = Some(AxisId::X);
        }
        if y_resp.secondary_clicked() {
            self.dialog_open_request = Some(AxisId::Y);
        }
        if ac && r_resp.secondary_clicked() {
            self.dialog_open_request = Some(AxisId::R);
        }
        if let Some(which) = self.dialog_open_request.take() {
            self.open_dialog(which, &view, cfg);
        }

        let pointer = ui.input(|i| i.pointer.hover_pos());
        let over_plot = pointer.map_or(false, |p| plot.contains(p));

        // Hover → nearest trace, and the cursor-line hint.
        if let (Some(p), true) = (pointer, over_plot) {
            out.hovered_trace = nearest_trace(&view, cfg, traces, p);
            for (n, c) in self.cursors.iter().enumerate() {
                if let Some(c) = c {
                    if (view.sx(c.x) - p.x).abs() <= 5.0 {
                        out.hint = Some(format!("Left-Click & drag to move Cursor {}", n + 1));
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                }
            }
        }

        // Drag handling: cursor lines first, otherwise a zoom box.
        if plot_resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(p) = plot_resp.interact_pointer_pos() {
                let hit = self.cursors.iter().enumerate().find_map(|(n, c)| {
                    c.as_ref().filter(|c| (view.sx(c.x) - p.x).abs() <= 6.0).map(|_| n)
                });
                self.drag = Some(match hit {
                    Some(n) => Drag::Cursor(n),
                    None => Drag::Zoom { origin: p },
                });
            }
        }
        match (&self.drag, plot_resp.dragged_by(egui::PointerButton::Primary)) {
            (Some(Drag::Cursor(n)), true) => {
                let n = *n;
                if let Some(p) = plot_resp.interact_pointer_pos() {
                    let x = view.x_at(p.x.clamp(plot.left(), plot.right()));
                    if let Some(c) = self.cursors[n].as_mut() {
                        if let Some(t) = traces.get(c.trace) {
                            if let (Some(&lo), Some(&hi)) = (t.x.first(), t.x.last()) {
                                c.x = x.clamp(lo, hi);
                            }
                        }
                        out.hint = Some(format!("Left-Click & drag to move Cursor {}", n + 1));
                    }
                }
            }
            (Some(Drag::Zoom { origin }), true) => {
                if let Some(p) = plot_resp.interact_pointer_pos() {
                    let r = Rect::from_two_pos(*origin, p).intersect(plot);
                    painter.add(Shape::dashed_line(
                        &[r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()],
                        Stroke::new(1.0, CURSOR),
                        4.0,
                        3.0,
                    ));
                }
            }
            _ => {}
        }
        if plot_resp.drag_stopped() {
            if let (Some(Drag::Zoom { origin }), Some(p)) =
                (self.drag.take_if_zoom(), plot_resp.interact_pointer_pos())
            {
                let r = Rect::from_two_pos(origin, p).intersect(plot);
                if r.width() > 6.0 && r.height() > 6.0 {
                    self.zoom_to_rect(&view, r, cfg);
                }
            }
            self.drag = None;
        }
        // Middle-drag pans.
        if plot_resp.dragged_by(egui::PointerButton::Middle) {
            let d = plot_resp.drag_delta();
            self.pan(&view, d, cfg);
        }
        // Wheel zooms about the pointer.
        if let (true, Some(p)) = (over_plot, pointer) {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                let shift = ui.input(|i| i.modifiers.shift);
                let ctrl = ui.input(|i| i.modifiers.ctrl);
                self.wheel_zoom(&view, p, dy, !ctrl, !shift, cfg);
            }
        }

        // Keys (only with the pointer over the viewer, and no text focus).
        if over_plot && !ui.ctx().wants_keyboard_input() {
            let (space, ctrl_y, key_a, esc, shift_c) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::Space),
                    i.modifiers.ctrl && i.key_pressed(egui::Key::Y),
                    !i.modifiers.any() && i.key_pressed(egui::Key::A),
                    i.key_pressed(egui::Key::Escape),
                    i.modifiers.shift && i.key_pressed(egui::Key::C),
                )
            });
            if space {
                self.zoom_to_fit();
            }
            if ctrl_y {
                self.autorange_y();
            }
            if key_a {
                out.request_add_traces = true;
            }
            if esc || shift_c {
                self.clear_cursors();
            }
        }

        // Context menu.
        let mut action: Vec<Act> = Vec::new();
        let have_traces = !traces.is_empty();
        let has_cursors = self.has_cursors();
        let mut grid = self.grid;
        let mut cursor_window_hidden = self.cursor_window_hidden;
        plot_resp.context_menu(|ui| {
            ui.set_min_width(300.0);
            if menu_item(ui, "Zoom to Fit", "Space", true) { action.push(Act::Fit); ui.close_menu(); }
            if menu_item(ui, "Autorange Y-axis", "Ctrl+Y", true) { action.push(Act::AutoY); ui.close_menu(); }
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut grid, "Grid");
                if ui.button("Zoom Back").clicked() { action.push(Act::Back); ui.close_menu(); }
                let mut show = !cursor_window_hidden;
                if ui.checkbox(&mut show, "Cursor Window").changed() {
                    cursor_window_hidden = !show;
                }
            });
            if menu_item(ui, "Add Traces", "A", true) { action.push(Act::AddTraces); ui.close_menu(); }
            if menu_item(ui, "Place Cursor on Active Trace", "", have_traces) { action.push(Act::Place); ui.close_menu(); }
            if menu_item(ui, "Clear All Cursors", "Shift+C or Esc", has_cursors) { action.push(Act::Clear); ui.close_menu(); }
            if menu_item(ui, "Redraw", "F5", true) { ui.ctx().request_repaint(); ui.close_menu(); }
            for label in [
                "Add Plot Pane Above", "Add Plot Pane Below", "Move Plot Pane Up",
                "Move Plot Pane Down", "Delete this Pane", "Sync. Horiz. Axes",
            ] {
                menu_item(ui, label, "", false);
            }
            ui.add_enabled_ui(false, |ui| {
                ui.menu_button("Notes & Annotations", |_| {});
                ui.menu_button("Edit", |_| {});
            });
            menu_item(ui, "Copy Image to Clipboard", "", false);
            ui.separator();
            ui.add_enabled_ui(false, |ui| { ui.menu_button("Marching Waveforms", |_| {}); });
            ui.separator();
            ui.menu_button("File", |ui| {
                if ui.button("Export data as CSV…").clicked() { action.push(Act::Csv); ui.close_menu(); }
            });
            menu_item(ui, "Float Window", "", false);
            if menu_item(ui, "Waveform Settings", "", true) { action.push(Act::Settings); ui.close_menu(); }
        });
        self.grid = grid;
        self.cursor_window_hidden = cursor_window_hidden;
        for a in action {
            match a {
                Act::Fit => self.zoom_to_fit(),
                Act::AutoY => self.autorange_y(),
                Act::Back => self.zoom_back(),
                Act::AddTraces => out.request_add_traces = true,
                Act::Place => self.place_cursor_on_active(),
                Act::Clear => self.clear_cursors(),
                Act::Csv => out.request_export_csv = true,
                Act::Settings => out.request_settings = true,
            }
        }

        self.axis_dialog(ui.ctx(), plot, cfg);
        self.cursor_window(ui.ctx(), outer, cfg, traces);
        out
    }

    // ── View resolution ────────────────────────────────────────────────────

    fn resolve(&self, plot: Rect, cfg: &ViewerConfig, traces: &[Trace]) -> View {
        let x_log = self.x_log.unwrap_or(cfg.log_x_default);
        // x window
        let x = match self.x {
            Some(w) => w,
            None => {
                let mut lo = f64::INFINITY;
                let mut hi = f64::NEG_INFINITY;
                for t in traces {
                    for &v in &t.x {
                        if v.is_finite() && (!x_log || v > 0.0) {
                            lo = lo.min(v);
                            hi = hi.max(v);
                        }
                    }
                }
                if !(lo.is_finite() && hi.is_finite()) {
                    if x_log { (1.0, 10.0) } else { (0.0, 1.0) }
                } else if lo == hi {
                    if x_log { (lo / 2.0, lo * 2.0) } else { (lo - 0.5, lo + 0.5) }
                } else {
                    (lo, hi)
                }
            }
        };
        let x_unit = cfg.x_unit_str();
        let x_ticks = if x_log {
            let (maj, min) = log_ticks(x.0, x.1);
            (maj, min, 0.0)
        } else {
            let step = self.x_tick.unwrap_or_else(|| nice_step(x.1 - x.0, 8.0, x_unit));
            (linear_ticks(x.0, x.1, step), Vec::new(), step)
        };

        // y window — auto ranges look only at samples inside the x window.
        let in_x = |xv: f64| xv >= x.0 && xv <= x.1;
        let mut ymin = f64::INFINITY;
        let mut ymax = f64::NEG_INFINITY;
        let mut rmin = f64::INFINITY;
        let mut rmax = f64::NEG_INFINITY;
        for t in traces {
            for (k, &xv) in t.x.iter().enumerate() {
                if !in_x(xv) {
                    continue;
                }
                if let Some(&yv) = t.y.get(k) {
                    if yv.is_finite() && (!self.y_log || yv > 0.0) {
                        ymin = ymin.min(yv);
                        ymax = ymax.max(yv);
                    }
                }
                if let Some(&pv) = t.phase.as_ref().and_then(|p| p.get(k)) {
                    if pv.is_finite() {
                        rmin = rmin.min(pv);
                        rmax = rmax.max(pv);
                    }
                }
            }
        }
        let left_unit = cfg.left_unit();
        let y_log = self.y_log;
        let (y, y_ticks) = match self.y {
            Some(w) => {
                let step = self.y_tick.unwrap_or_else(|| nice_step(w.1 - w.0, 10.0, left_unit));
                (w, y_ticks_for(w, step, y_log))
            }
            None if ymin.is_finite() && ymax.is_finite() => {
                if y_log {
                    let w = (10f64.powf(ymin.log10().floor()), 10f64.powf(ymax.log10().ceil()));
                    (w, y_ticks_for(w, 0.0, true))
                } else {
                    let (lo, hi, step) = auto_range(ymin, ymax, 10.0, left_unit);
                    let step = self.y_tick.unwrap_or(step);
                    ((lo, hi), y_ticks_for((lo, hi), step, false))
                }
            }
            None => {
                let w = if y_log { (1.0, 10.0) } else { (-1.0, 1.0) };
                (w, y_ticks_for(w, if y_log { 0.0 } else { 0.5 }, y_log))
            }
        };

        let r = if cfg.mode == Mode::Ac {
            let (w, step) = match self.r {
                Some(w) => (w, self.r_tick.unwrap_or_else(|| nice_step(w.1 - w.0, 10.0, "°"))),
                None if rmin.is_finite() && rmax.is_finite() => {
                    let (lo, hi, step) = auto_range(rmin, rmax, 10.0, "°");
                    ((lo, hi), self.r_tick.unwrap_or(step))
                }
                None => ((-180.0, 180.0), 36.0),
            };
            Some((w, (linear_ticks(w.0, w.1, step), step)))
        } else {
            None
        };

        View { plot, x, x_log, x_ticks, y, y_log, y_ticks, r }
    }

    fn open_dialog(&mut self, which: AxisId, view: &View, cfg: &ViewerConfig) {
        let (unit, lo, hi, tick, log, quantity) = match which {
            AxisId::X => (
                cfg.x_unit_str().to_string(),
                view.x.0,
                view.x.1,
                view.x_ticks.2,
                view.x_log,
                cfg.x_name().to_string(),
            ),
            AxisId::Y => (cfg.left_unit().to_string(), view.y.0, view.y.1, view.y_ticks.2, view.y_log, String::new()),
            AxisId::R => {
                let r = view.r.as_ref();
                (
                    "°".to_string(),
                    r.map_or(0.0, |r| r.0 .0),
                    r.map_or(1.0, |r| r.0 .1),
                    r.map_or(1.0, |r| r.1 .1),
                    false,
                    String::new(),
                )
            }
        };
        let fmt = |v: f64, step: f64| fmt_si_unit(v, if step > 0.0 { step } else { (hi - lo).abs() / 10.0 }, &unit);
        let (top, bottom) = if which == AxisId::X { (lo, hi) } else { (hi, lo) };
        self.dialog = Some(AxisDialog {
            which,
            unit: unit.clone(),
            quantity,
            lo: fmt(top, tick),
            hi: fmt(bottom, tick),
            tick: if tick > 0.0 { fmt(tick, tick) } else { String::new() },
            log,
            bad: [false; 3],
        });
    }

    // ── Painting ────────────────────────────────────────────────────────────

    fn paint_axes(&self, painter: &egui::Painter, v: &View, cfg: &ViewerConfig) {
        let font = bold(11.5);
        let p = v.plot;
        // Grid first, under the frame.
        if self.grid {
            let dot = Stroke::new(1.0, GRID);
            for &t in &v.x_ticks.0 {
                let x = v.sx(t);
                painter.add(Shape::dashed_line(&[pos2(x, p.top()), pos2(x, p.bottom())], dot, 1.0, 3.0));
            }
            for &t in &v.y_ticks.0 {
                let y = v.sy(t);
                painter.add(Shape::dashed_line(&[pos2(p.left(), y), pos2(p.right(), y)], dot, 1.0, 3.0));
            }
        }
        painter.rect_stroke(p, 0.0, Stroke::new(1.0, FRAME));
        let tick = Stroke::new(1.0, FRAME);

        // Bottom axis.
        let xu = cfg.x_unit_str();
        for &t in &v.x_ticks.0 {
            let x = v.sx(t);
            if !x.is_finite() {
                continue;
            }
            painter.line_segment([pos2(x, p.bottom()), pos2(x, p.bottom() - 4.0)], tick);
            let label = fmt_si_unit(t, if v.x_log { t.abs().max(1e-30) } else { v.x_ticks.2 }, xu);
            painter.text(pos2(x, p.bottom() + 3.0), Align2::CENTER_TOP, label, font.clone(), TEXT);
        }
        for &t in &v.x_ticks.1 {
            let x = v.sx(t);
            if x.is_finite() {
                painter.line_segment([pos2(x, p.bottom()), pos2(x, p.bottom() - 2.0)], tick);
            }
        }
        // Left axis.
        let yu = cfg.left_unit();
        for &t in &v.y_ticks.0 {
            let y = v.sy(t);
            if !y.is_finite() {
                continue;
            }
            painter.line_segment([pos2(p.left(), y), pos2(p.left() - 4.0, y)], tick);
            let label = fmt_si_unit(t, if v.y_log { t.abs().max(1e-30) } else { v.y_ticks.2 }, yu);
            painter.text(pos2(p.left() - 5.0, y), Align2::RIGHT_CENTER, label, font.clone(), TEXT);
        }
        // Right axis (phase).
        if let Some((_, (ticks, step))) = &v.r {
            for &t in ticks {
                let y = v.sr(t);
                painter.line_segment([pos2(p.right(), y), pos2(p.right() + 4.0, y)], tick);
                painter.text(
                    pos2(p.right() + 5.0, y),
                    Align2::LEFT_CENTER,
                    fmt_si_unit(t, *step, "°"),
                    font.clone(),
                    TEXT,
                );
            }
        }
    }

    fn paint_traces(&self, painter: &egui::Painter, v: &View, cfg: &ViewerConfig, traces: &[Trace]) {
        let clip = painter.with_clip_rect(v.plot.expand(1.0));
        for t in traces {
            let mut mag: Vec<Pos2> = Vec::with_capacity(t.x.len());
            for (k, &xv) in t.x.iter().enumerate() {
                let Some(&yv) = t.y.get(k) else { break };
                let (sx, sy) = (v.sx(xv), v.sy(yv));
                if sx.is_finite() && sy.is_finite() && sy.abs() < 1e7 {
                    mag.push(pos2(sx, sy));
                } else if mag.len() > 1 {
                    clip.add(Shape::line(std::mem::take(&mut mag), Stroke::new(1.0, t.color)));
                } else {
                    mag.clear();
                }
            }
            if mag.len() > 1 {
                clip.add(Shape::line(mag, Stroke::new(1.0, t.color)));
            }
            if cfg.mode == Mode::Ac {
                if let Some(ph) = &t.phase {
                    let pts: Vec<Pos2> = t
                        .x
                        .iter()
                        .zip(ph)
                        .map(|(&xv, &pv)| pos2(v.sx(xv), v.sr(pv)))
                        .filter(|p| p.x.is_finite() && p.y.is_finite())
                        .collect();
                    if pts.len() > 1 {
                        clip.extend(Shape::dashed_line(&pts, Stroke::new(1.0, t.color), 4.0, 2.0));
                    }
                }
            }
        }
    }

    fn paint_names(&mut self, ui: &mut Ui, painter: &egui::Painter, plot: Rect, traces: &[Trace]) {
        let font = bold(11.5);
        let galleys: Vec<_> = traces
            .iter()
            .map(|t| painter.layout_no_wrap(t.name.clone(), font.clone(), t.color))
            .collect();
        let gap = 14.0;
        let total: f32 = galleys.iter().map(|g| g.size().x).sum::<f32>()
            + gap * galleys.len().saturating_sub(1) as f32;
        let mut x = plot.center().x - total / 2.0;
        let y = plot.top() - 4.0 - galleys.first().map_or(12.0, |g| g.size().y);
        for (i, g) in galleys.into_iter().enumerate() {
            let rect = Rect::from_min_size(pos2(x, y), g.size());
            let resp = ui.interact(rect.expand(2.0), ui.id().with(("lt_name", i)), Sense::click());
            if resp.clicked() {
                self.active = Some(i);
            }
            if self.active == Some(i) {
                painter.rect_stroke(rect.expand2(vec2(2.0, 0.0)), 0.0, Stroke::new(1.0, CURSOR));
            }
            x += rect.width() + gap;
            painter.galley(rect.min, g, TEXT);
        }
    }

    fn paint_cursors(&self, painter: &egui::Painter, v: &View, cfg: &ViewerConfig, traces: &[Trace]) {
        let clip = painter.with_clip_rect(v.plot.expand(1.0));
        let stroke = Stroke::new(1.0, CURSOR);
        for c in self.cursors.iter().flatten() {
            let x = v.sx(c.x);
            if !x.is_finite() {
                continue;
            }
            clip.add(Shape::dashed_line(&[pos2(x, v.plot.top()), pos2(x, v.plot.bottom())], stroke, 3.0, 3.0));
            let Some(t) = traces.get(c.trace) else { continue };
            let reading = read_at(t, c.x, cfg);
            let y = match (cfg.mode, c.qty) {
                (Mode::Real, _) | (Mode::Ac, AcQty::Mag) => reading.y.map(|y| v.sy(y)),
                (Mode::Ac, AcQty::Phase) => reading.phase.map(|p| v.sr(p)),
                (Mode::Ac, AcQty::GroupDelay) => None,
            };
            if let Some(y) = y.filter(|y| y.is_finite()) {
                clip.add(Shape::dashed_line(&[pos2(v.plot.left(), y), pos2(v.plot.right(), y)], stroke, 3.0, 3.0));
            }
        }
    }

    // ── Cursors ─────────────────────────────────────────────────────────────

    fn place_now(&mut self, view: &View, cfg: &ViewerConfig, traces: &[Trace]) {
        let Some(active) = self.active.filter(|&a| a < traces.len()) else { return };
        let slot = if self.cursors[0].is_none() {
            0
        } else if self.cursors[1].is_none() {
            1
        } else {
            return;
        };
        let frac = if slot == 0 { 0.5 } else { 0.75 };
        let mut x = from_frac(frac, view.x.0, view.x.1, view.x_log);
        let t = &traces[active];
        if let (Some(&lo), Some(&hi)) = (t.x.first(), t.x.last()) {
            x = x.clamp(lo, hi);
        }
        let _ = cfg;
        self.cursors[slot] = Some(Cursor { trace: active, x, qty: AcQty::Mag });
    }

    fn cursor_window(&mut self, ctx: &egui::Context, outer: Rect, cfg: &ViewerConfig, traces: &[Trace]) {
        if !self.has_cursors() || self.cursor_window_hidden {
            return;
        }
        let mut open = true;
        let ac = cfg.mode == Mode::Ac;
        let readings: [Option<(String, Reading)>; 2] = [0, 1].map(|n| {
            self.cursors[n].as_ref().and_then(|c| {
                traces.get(c.trace).map(|t| (t.name.clone(), read_at(t, c.x, cfg)))
            })
        });
        let qtys = [
            self.cursors[0].as_ref().map(|c| c.qty),
            self.cursors[1].as_ref().map(|c| c.qty),
        ];
        let mut new_qty = qtys;
        let win_w = if ac { 296.0 } else { 296.0 };
        egui::Window::new(cfg.title.clone())
            .id(egui::Id::new("lt_cursor_window"))
            .open(&mut open)
            .collapsible(true)
            .resizable(false)
            .default_pos(pos2(outer.right() - win_w - 60.0, outer.top() + 60.0))
            .show(ctx, |ui| {
                ui.set_min_width(win_w - 16.0);
                for n in 0..2 {
                    cursor_group(ui, n, ac, cfg, readings[n].as_ref(), &mut new_qty[n]);
                }
                diff_group(ui, ac, cfg, &readings);
            });
        for n in 0..2 {
            if let (Some(c), Some(q)) = (self.cursors[n].as_mut(), new_qty[n]) {
                c.qty = q;
            }
        }
        if !open {
            self.cursor_window_hidden = true;
        }
    }

    // ── View changes ────────────────────────────────────────────────────────

    fn zoom_to_rect(&mut self, v: &View, r: Rect, cfg: &ViewerConfig) {
        self.push_zoom();
        let fx0 = ((r.left() - v.plot.left()) / v.plot.width()) as f64;
        let fx1 = ((r.right() - v.plot.left()) / v.plot.width()) as f64;
        let fy0 = ((v.plot.bottom() - r.bottom()) / v.plot.height()) as f64;
        let fy1 = ((v.plot.bottom() - r.top()) / v.plot.height()) as f64;
        self.x = Some((from_frac(fx0, v.x.0, v.x.1, v.x_log), from_frac(fx1, v.x.0, v.x.1, v.x_log)));
        self.y = Some((from_frac(fy0, v.y.0, v.y.1, v.y_log), from_frac(fy1, v.y.0, v.y.1, v.y_log)));
        if let (Mode::Ac, Some((w, _))) = (cfg.mode, v.r.as_ref()) {
            self.r = Some((from_frac(fy0, w.0, w.1, false), from_frac(fy1, w.0, w.1, false)));
        }
        self.x_tick = None;
        self.y_tick = None;
        self.r_tick = None;
        self.x_log = Some(v.x_log);
    }

    fn pan(&mut self, v: &View, d: egui::Vec2, cfg: &ViewerConfig) {
        let fx = -(d.x / v.plot.width()) as f64;
        let fy = (d.y / v.plot.height()) as f64;
        let shift = |w: (f64, f64), f: f64, log: bool| {
            (from_frac(f, w.0, w.1, log), from_frac(1.0 + f, w.0, w.1, log))
        };
        self.x = Some(shift(v.x, fx, v.x_log));
        self.y = Some(shift(v.y, fy, v.y_log));
        if let (Mode::Ac, Some((w, _))) = (cfg.mode, v.r.as_ref()) {
            self.r = Some(shift(*w, fy, false));
        }
        self.x_log = Some(v.x_log);
    }

    fn wheel_zoom(&mut self, v: &View, at: Pos2, dy: f32, zoom_x: bool, zoom_y: bool, cfg: &ViewerConfig) {
        let k = (-(dy as f64) * 0.0015).exp();
        let fx = ((at.x - v.plot.left()) / v.plot.width()) as f64;
        let fy = ((v.plot.bottom() - at.y) / v.plot.height()) as f64;
        let scale = |w: (f64, f64), f: f64, log: bool| {
            // New window keeps the pointer's fraction fixed.
            (from_frac(f - f * k, w.0, w.1, log), from_frac(f + (1.0 - f) * k, w.0, w.1, log))
        };
        if zoom_x {
            self.x = Some(scale(v.x, fx, v.x_log));
            self.x_tick = None;
            self.x_log = Some(v.x_log);
        }
        if zoom_y {
            self.y = Some(scale(v.y, fy, v.y_log));
            self.y_tick = None;
            if let (Mode::Ac, Some((w, _))) = (cfg.mode, v.r.as_ref()) {
                self.r = Some(scale(*w, fy, false));
                self.r_tick = None;
            }
        }
    }

    // ── Axis dialog ─────────────────────────────────────────────────────────

    fn axis_dialog(&mut self, ctx: &egui::Context, plot: Rect, cfg: &ViewerConfig) {
        let Some(mut d) = self.dialog.take() else { return };
        let (title, a_label, b_label, t_label) = match d.which {
            AxisId::X => ("Horizontal Axis", "Left:", "Right:", "tick:"),
            AxisId::Y => ("Vertical Axis", "Top:", "Bottom:", "Tick:"),
            AxisId::R => ("Right Vertical Axis", "Top:", "Bottom:", "Tick:"),
        };
        let (mut ok, mut cancel) = (false, false);
        egui::Window::new(title)
            .id(egui::Id::new("lt_axis_dialog"))
            .collapsible(false)
            .resizable(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(plot.center())
            .show(ctx, |ui| {
                ui.set_min_width(300.0);
                if d.which == AxisId::X {
                    ui.horizontal(|ui| {
                        ui.label("Quantity Plotted:");
                        let mut q = d.quantity.clone();
                        ui.add(egui::TextEdit::singleline(&mut q).interactive(false).desired_width(110.0));
                        ui.add_enabled(false, egui::Button::new("Eye Diagram"));
                    });
                    ui.add_space(4.0);
                }
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Axis Limits").strong());
                    egui::Grid::new("lt_axis_grid").num_columns(2).show(ui, |ui| {
                        for (label, text, i) in [
                            (a_label, &mut d.lo, 0usize),
                            (t_label, &mut d.tick, 1),
                            (b_label, &mut d.hi, 2),
                        ] {
                            ui.label(label);
                            let edit = egui::TextEdit::singleline(text).desired_width(110.0).text_color(
                                if d.bad[i] { Color32::from_rgb(200, 0, 0) } else { Color32::BLACK },
                            );
                            ui.add(edit);
                            ui.end_row();
                        }
                    });
                });
                ui.checkbox(&mut d.log, "Logarithmic");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ok = ui.button("OK").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
                let (enter, esc) =
                    ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
                ok |= enter;
                cancel |= esc;
            });
        if cancel {
            return;
        }
        if ok {
            if self.apply_dialog(&mut d, cfg) {
                return;
            }
        }
        self.dialog = Some(d);
    }

    /// Validate and apply; `false` keeps the dialog open with bad fields red.
    fn apply_dialog(&mut self, d: &mut AxisDialog, _cfg: &ViewerConfig) -> bool {
        let first = parse_si_unit(&d.lo, &d.unit);
        let second = parse_si_unit(&d.hi, &d.unit);
        let tick = if d.tick.trim().is_empty() { Some(None) } else { parse_si_unit(&d.tick, &d.unit).map(Some) };
        d.bad = [first.is_none(), tick.is_none(), second.is_none()];
        let (Some(first), Some(second), Some(tick)) = (first, second, tick) else { return false };
        let (lo, hi) = if d.which == AxisId::X { (first, second) } else { (second, first) };
        if !(lo < hi) || (d.log && lo <= 0.0) || tick.map_or(false, |t| t <= 0.0) {
            d.bad = [true, tick.map_or(false, |t| t <= 0.0), true];
            return false;
        }
        self.push_zoom();
        match d.which {
            AxisId::X => {
                self.x = Some((lo, hi));
                self.x_tick = tick;
                self.x_log = Some(d.log);
            }
            AxisId::Y => {
                self.y = Some((lo, hi));
                self.y_tick = tick;
                self.y_log = d.log;
            }
            AxisId::R => {
                self.r = Some((lo, hi));
                self.r_tick = tick;
            }
        }
        true
    }
}

fn y_ticks_for(w: (f64, f64), step: f64, log: bool) -> (Vec<f64>, Vec<f64>, f64) {
    if log {
        let (maj, min) = log_ticks(w.0, w.1);
        (maj, min, 0.0)
    } else {
        (linear_ticks(w.0, w.1, step), Vec::new(), step)
    }
}

trait TakeZoom {
    fn take_if_zoom(&mut self) -> Option<Drag>;
}

impl TakeZoom for Option<Drag> {
    fn take_if_zoom(&mut self) -> Option<Drag> {
        if matches!(self, Some(Drag::Zoom { .. })) { self.take() } else { None }
    }
}

enum Act {
    Fit,
    AutoY,
    Back,
    AddTraces,
    Place,
    Clear,
    Csv,
    Settings,
}

fn menu_item(ui: &mut Ui, label: &str, shortcut: &str, enabled: bool) -> bool {
    let mut b = egui::Button::new(label).frame(false).min_size(vec2(ui.available_width(), 0.0));
    if !shortcut.is_empty() {
        b = b.shortcut_text(shortcut);
    }
    ui.add_enabled(enabled, b).clicked()
}

fn nearest_trace(v: &View, cfg: &ViewerConfig, traces: &[Trace], p: Pos2) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, t) in traces.iter().enumerate() {
        let step = (t.x.len() / 2000).max(1);
        for k in (0..t.x.len()).step_by(step) {
            let Some(&yv) = t.y.get(k) else { break };
            let d = (pos2(v.sx(t.x[k]), v.sy(yv)) - p).length();
            let mut d = d;
            if cfg.mode == Mode::Ac {
                if let Some(&pv) = t.phase.as_ref().and_then(|ph| ph.get(k)) {
                    d = d.min((pos2(v.sx(t.x[k]), v.sr(pv)) - p).length());
                }
            }
            if d.is_finite() && d <= 8.0 && best.map_or(true, |(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
    }
    best.map(|(i, _)| i)
}

// ─── Cursor readings ────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Reading {
    pub x: f64,
    pub y: Option<f64>,
    pub phase: Option<f64>,
    pub gd: Option<f64>,
}

fn read_at(t: &Trace, x: f64, cfg: &ViewerConfig) -> Reading {
    let log = cfg.x_kind == XKind::Frequency;
    let y = interp(&t.x, &t.y, x, log);
    let (phase, gd) = match (&t.phase, cfg.mode) {
        (Some(ph), Mode::Ac) => {
            let unwrapped = unwrap_phase_deg(ph);
            (
                interp(&t.x, &unwrapped, x, log).map(|p| {
                    // Show the phase as the engine reported it (wrapped).
                    let mut q = p % 360.0;
                    if q > 180.0 { q -= 360.0 } else if q <= -180.0 { q += 360.0 }
                    q
                }),
                group_delay(&t.x, ph, x),
            )
        }
        _ => (None, None),
    };
    Reading { x, y, phase, gd }
}

fn readout(ui: &mut Ui, text: &str, width: f32, enabled: bool) {
    let mut s = text.to_string();
    ui.add_enabled(
        enabled,
        egui::TextEdit::singleline(&mut s)
            .interactive(false)
            .desired_width(width)
            .horizontal_align(egui::Align::RIGHT)
            .font(FontId::proportional(11.0)),
    );
}

fn cursor_group(
    ui: &mut Ui,
    n: usize,
    ac: bool,
    cfg: &ViewerConfig,
    reading: Option<&(String, Reading)>,
    qty: &mut Option<AcQty>,
) {
    egui::Frame::none()
        .stroke(Stroke::new(1.0, Color32::from_rgb(170, 170, 170)))
        .inner_margin(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(format!("Cursor {}", n + 1)).size(10.5));
            ui.vertical_centered(|ui| {
                ui.label(reading.map_or("", |r| r.0.as_str()));
            });
            let na = "-- N/A--";
            let enabled = reading.is_some();
            let r = reading.map(|r| r.1);
            let xu = cfg.x_unit_str();
            if ac {
                egui::Grid::new(("lt_cur_ac", n)).num_columns(4).show(ui, |ui| {
                    ui.label("Freq:");
                    readout(ui, &r.map_or(na.into(), |r| fmt_sig(r.x, "Hz")), 92.0, enabled);
                    ui.label("Mag:");
                    readout(ui, &r.and_then(|r| r.y).map_or(na.into(), |y| fmt_sig(y, if cfg.mag_db { "dB" } else { &cfg.y_unit })), 92.0, enabled);
                    radio(ui, qty, AcQty::Mag);
                    ui.end_row();
                    ui.label("");
                    ui.label("");
                    ui.label("Phase:");
                    readout(ui, &r.and_then(|r| r.phase).map_or(na.into(), |p| fmt_sig(p, "°")), 92.0, enabled);
                    radio(ui, qty, AcQty::Phase);
                    ui.end_row();
                    ui.label("");
                    ui.label("");
                    ui.label("Group Delay:");
                    readout(ui, &r.and_then(|r| r.gd).map_or(na.into(), |g| fmt_sig(g, "s")), 92.0, enabled);
                    radio(ui, qty, AcQty::GroupDelay);
                    ui.end_row();
                });
            } else {
                egui::Grid::new(("lt_cur", n)).num_columns(4).show(ui, |ui| {
                    ui.label("Horz:");
                    readout(ui, &r.map_or(na.into(), |r| fmt_sig(r.x, xu)), 96.0, enabled);
                    ui.label("Vert:");
                    readout(ui, &r.and_then(|r| r.y).map_or(na.into(), |y| fmt_sig(y, &cfg.y_unit)), 96.0, enabled);
                    ui.end_row();
                });
            }
        });
}

fn radio(ui: &mut Ui, qty: &mut Option<AcQty>, value: AcQty) {
    let mut cur = qty.unwrap_or(AcQty::Mag);
    if ui.add_enabled(qty.is_some(), egui::RadioButton::new(cur == value, "")).clicked() {
        cur = value;
        *qty = Some(cur);
    }
}

fn diff_group(ui: &mut Ui, ac: bool, cfg: &ViewerConfig, readings: &[Option<(String, Reading)>; 2]) {
    let both = match (&readings[0], &readings[1]) {
        (Some(a), Some(b)) => Some((a.1, b.1)),
        _ => None,
    };
    egui::Frame::none()
        .stroke(Stroke::new(1.0, Color32::from_rgb(170, 170, 170)))
        .inner_margin(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.label(if ac { "Ratio (Cursor2 / Cursor1)" } else { "Diff (Cursor2 - Cursor1)" });
            });
            let na = "-- N/A--";
            let enabled = both.is_some();
            let xu = cfg.x_unit_str();
            let dx = both.map(|(a, b)| b.x - a.x);
            if ac {
                // As LTspice does: the frequency difference and the dB /
                // degree / delay differences between the two cursors.
                egui::Grid::new("lt_ratio").num_columns(4).show(ui, |ui| {
                    ui.label("Freq:");
                    readout(ui, &dx.map_or(na.into(), |d| fmt_sig(d, "Hz")), 92.0, enabled);
                    ui.label("Mag:");
                    readout(ui, &both.and_then(|(a, b)| Some(b.y? - a.y?)).map_or(na.into(), |d| fmt_sig(d, if cfg.mag_db { "dB" } else { &cfg.y_unit })), 92.0, enabled);
                    ui.end_row();
                    ui.label("");
                    ui.label("");
                    ui.label("Phase:");
                    readout(ui, &both.and_then(|(a, b)| Some(b.phase? - a.phase?)).map_or(na.into(), |d| fmt_sig(d, "°")), 92.0, enabled);
                    ui.end_row();
                    ui.label("");
                    ui.label("");
                    ui.label("Group Delay:");
                    readout(ui, &both.and_then(|(a, b)| Some(b.gd? - a.gd?)).map_or(na.into(), |d| fmt_sig(d, "s")), 92.0, enabled);
                    ui.end_row();
                });
            } else {
                let dy = both.and_then(|(a, b)| Some(b.y? - a.y?));
                egui::Grid::new("lt_diff").num_columns(4).show(ui, |ui| {
                    ui.label("Horz:");
                    readout(ui, &dx.map_or(na.into(), |d| fmt_sig(d, xu)), 96.0, enabled);
                    ui.label("Vert:");
                    readout(ui, &dy.map_or(na.into(), |d| fmt_sig(d, &cfg.y_unit)), 96.0, enabled);
                    ui.end_row();
                    ui.label("Freq:");
                    let freq = (cfg.x_kind == XKind::Time).then(|| dx.filter(|d| *d != 0.0).map(|d| 1.0 / d.abs())).flatten();
                    readout(ui, &freq.map_or(na.into(), |f| fmt_sig(f, "Hz")), 96.0, enabled && freq.is_some());
                    ui.label("Slope:");
                    let slope = dx.filter(|d| *d != 0.0).zip(dy).map(|(dx, dy)| dy / dx);
                    readout(ui, &slope.map_or(na.into(), |s| fmt_sig(s, &format!("{}/{}", cfg.y_unit, xu))), 96.0, enabled && slope.is_some());
                    ui.end_row();
                });
            }
        });
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_labels_follow_ltspice_spelling() {
        assert_eq!(fmt_si_unit(10.0, 10.0, "V"), "10V");
        assert_eq!(fmt_si_unit(-110.0, 30.0, "V"), "-110V");
        assert_eq!(fmt_si_unit(1000.0, 1000.0, "Hz"), "1KHz");
        assert_eq!(fmt_si_unit(100_000.0, 1.0, "Hz"), "100KHz");
        assert_eq!(fmt_si_unit(2.5e6, 0.5e6, "Hz"), "2.5MHz");
        assert_eq!(fmt_si_unit(0.01, 0.01, "s"), "10ms");
        assert_eq!(fmt_si_unit(0.0, 0.01, "s"), "0ms");
        assert_eq!(fmt_si_unit(0.0, 10.0, "V"), "0V");
        assert_eq!(fmt_si_unit(18.0, 6.0, "dB"), "18dB");
        assert_eq!(fmt_si_unit(-9.0, 9.0, "°"), "-9°");
        assert_eq!(fmt_si_unit(0.5, 0.5, "V"), "500mV");
        assert_eq!(fmt_si_unit(20e-6, 1e-6, "s"), "20µs");
    }

    #[test]
    fn cursor_values_print_eight_significant_digits() {
        assert_eq!(fmt_sig(84.714771, "V"), "84.714771V");
        assert_eq!(fmt_sig(0.048348674, "s"), "48.348674ms");
        assert_eq!(fmt_sig(-4.5304609, "dB"), "-4.5304609dB");
        assert_eq!(fmt_sig(1244.0748, "Hz"), "1.2440748KHz");
        assert_eq!(fmt_sig(-585.45298e-6, "s"), "-585.45298µs");
        assert_eq!(fmt_sig(0.0, "V"), "0V");
        // Rounding up across a prefix boundary.
        assert_eq!(fmt_sig(999.999999, "Hz"), "1.0000000KHz");
    }

    #[test]
    fn limits_parse_with_spice_spelling() {
        assert_eq!(parse_si_unit("30V", "V"), Some(30.0));
        assert_eq!(parse_si_unit("-200", "V"), Some(-200.0));
        assert_eq!(parse_si_unit("100m", "V"), Some(0.1));
        assert_eq!(parse_si_unit("1k", "Hz"), Some(1000.0));
        assert_eq!(parse_si_unit("1Meg", "Hz"), Some(1e6));
        assert_eq!(parse_si_unit("2.5KHz", "Hz"), Some(2500.0));
        assert_eq!(parse_si_unit("10ms", "s"), Some(0.01));
        assert_eq!(parse_si_unit("1u", "s"), Some(1e-6));
        assert_eq!(parse_si_unit("3e-3", "V"), Some(0.003));
        assert_eq!(parse_si_unit("2M", "Hz"), Some(2e6));
        assert_eq!(parse_si_unit("2M", "V"), Some(2e-3));
        assert_eq!(parse_si_unit("abc", "V"), None);
        assert_eq!(parse_si_unit("", "V"), None);
        assert_eq!(parse_si_unit("5x", "V"), None);
    }

    #[test]
    fn db_axis_gets_six_db_steps() {
        let (lo, hi, step) = auto_range(-47.5, 17.6, 10.0, "dB");
        assert_eq!((lo, hi, step), (-48.0, 18.0, 6.0));
        assert_eq!(linear_ticks(lo, hi, step).len(), 12);
    }

    #[test]
    fn phase_axis_gets_nine_degree_steps() {
        let (lo, hi, step) = auto_range(-89.9, -0.1, 10.0, "°");
        assert_eq!((lo, hi, step), (-90.0, 0.0, 9.0));
    }

    #[test]
    fn voltage_axis_uses_one_two_five() {
        assert_eq!(nice_step(100.0, 10.0, "V"), 10.0);
        assert_eq!(nice_step(7.0, 10.0, "V"), 1.0);
        assert_eq!(nice_step(0.3, 10.0, "V"), 0.05);
        assert_eq!(nice_step(0.0, 10.0, "V"), 1.0);
    }

    #[test]
    fn flat_data_still_gets_a_range() {
        let (lo, hi, step) = auto_range(5.0, 5.0, 10.0, "V");
        assert!(lo < 5.0 && hi > 5.0 && step > 0.0);
        let (lo, hi, _) = auto_range(0.0, 0.0, 10.0, "V");
        assert!(lo < 0.0 && hi > 0.0);
    }

    #[test]
    fn log_ticks_split_decades_and_minors() {
        let (maj, min) = log_ticks(1.0, 100_000.0);
        assert_eq!(maj, vec![1.0, 10.0, 100.0, 1000.0, 10_000.0, 100_000.0]);
        assert_eq!(min.len(), 5 * 8);
        assert!(log_ticks(0.0, 10.0).0.is_empty());
    }

    #[test]
    fn linear_ticks_are_clean_multiples() {
        assert_eq!(linear_ticks(-200.0, 100.0, 30.0).len(), 10);
        let t = linear_ticks(0.0, 1.0, 0.1);
        assert_eq!(t.len(), 11);
        assert_eq!(t[0], 0.0);
        assert!(linear_ticks(1.0, 0.0, 0.1).is_empty());
        assert!(linear_ticks(0.0, 1.0, 0.0).is_empty());
    }

    #[test]
    fn interpolation_is_linear_and_log_aware() {
        let x = [1.0, 10.0, 100.0];
        let y = [0.0, 10.0, 20.0];
        assert_eq!(interp(&x, &y, 10.0, true), Some(10.0));
        let mid = interp(&x, &y, 10f64.sqrt(), true).unwrap();
        assert!((mid - 5.0).abs() < 1e-9);
        let lin = interp(&x, &y, 5.5, false).unwrap();
        assert!((lin - 5.0).abs() < 1e-9);
        assert_eq!(interp(&x, &y, 0.5, true), None);
        assert_eq!(interp(&x, &y, 200.0, true), None);
        assert_eq!(interp(&[], &[], 1.0, false), None);
    }

    #[test]
    fn phase_unwraps_through_the_branch_cut() {
        let p = [170.0, 179.0, -178.0, -170.0];
        assert_eq!(unwrap_phase_deg(&p), vec![170.0, 179.0, 182.0, 190.0]);
        let q = [-170.0, -179.0, 178.0];
        assert_eq!(unwrap_phase_deg(&q), vec![-170.0, -179.0, -182.0]);
    }

    #[test]
    fn group_delay_of_a_single_pole_is_half_tau_at_the_corner() {
        // R = 10.7k, C = 100n  →  τ = 1.07 ms, fc = 148.7 Hz.
        let tau = 10.7e3 * 100e-9;
        let fc = 1.0 / (2.0 * std::f64::consts::PI * tau);
        let freq: Vec<f64> = (0..500).map(|k| 10f64.powf(k as f64 / 100.0)).collect();
        let phase: Vec<f64> = freq.iter().map(|&f| -(f / fc).atan().to_degrees()).collect();
        let gd = group_delay(&freq, &phase, fc).unwrap();
        assert!((gd - tau / 2.0).abs() / (tau / 2.0) < 0.01, "gd = {gd}");
        // Far above the corner the delay collapses.
        assert!(group_delay(&freq, &phase, 1e4).unwrap() < tau / 100.0);
        assert!(group_delay(&[1.0], &[0.0], 1.0).is_none());
    }

    fn rc_trace() -> Trace {
        let freq: Vec<f64> = (0..400).map(|k| 10f64.powf(k as f64 / 100.0)).collect();
        let fc = 148.7;
        Trace {
            name: "V(n002)".into(),
            color: Color32::GREEN,
            y: freq.iter().map(|&f| 20.0 * (1.0 / (1.0 + (f / fc).powi(2)).sqrt()).log10()).collect(),
            phase: Some(freq.iter().map(|&f| -(f / fc).atan().to_degrees()).collect()),
            x: freq,
        }
    }

    fn ac_cfg() -> ViewerConfig {
        ViewerConfig {
            title: "Draft1.raw".into(),
            x_kind: XKind::Frequency,
            x_unit: "Hz".into(),
            y_unit: "V".into(),
            mode: Mode::Ac,
            mag_db: true,
            log_x_default: true,
        }
    }

    fn tran_cfg() -> ViewerConfig {
        ViewerConfig {
            title: "full_bridge.raw".into(),
            x_kind: XKind::Time,
            x_unit: "s".into(),
            y_unit: "V".into(),
            mode: Mode::Real,
            mag_db: false,
            log_x_default: false,
        }
    }

    fn sine_trace() -> Trace {
        let x: Vec<f64> = (0..=1000).map(|k| k as f64 * 1e-4).collect();
        Trace {
            name: "V(out)".into(),
            color: Color32::GREEN,
            y: x.iter().map(|&t| 5.0 * (2.0 * std::f64::consts::PI * 50.0 * t).sin()).collect(),
            phase: None,
            x,
        }
    }

    #[test]
    fn ac_reading_matches_the_analytic_response() {
        let t = rc_trace();
        let r = read_at(&t, 148.7, &ac_cfg());
        assert!((r.y.unwrap() + 3.0103).abs() < 0.01);
        assert!((r.phase.unwrap() + 45.0).abs() < 0.2);
        assert!(r.gd.unwrap() > 0.0);
    }

    #[test]
    fn transient_reading_interpolates_between_samples() {
        let t = sine_trace();
        let r = read_at(&t, 0.005, &tran_cfg());
        assert!((r.y.unwrap() - 5.0).abs() < 1e-3);
        assert!(r.phase.is_none() && r.gd.is_none());
    }

    #[test]
    fn view_maps_log_and_linear_axes() {
        let v = View {
            plot: Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0)),
            x: (1.0, 100.0),
            x_log: true,
            x_ticks: (vec![], vec![], 0.0),
            y: (0.0, 10.0),
            y_log: false,
            y_ticks: (vec![], vec![], 1.0),
            r: None,
        };
        assert!((v.sx(10.0) - 50.0).abs() < 1e-4);
        assert!((v.sy(5.0) - 50.0).abs() < 1e-4);
        assert!((v.x_at(50.0) - 10.0).abs() < 1e-6);
        assert!(v.sx(-1.0).is_nan());
    }

    // ── Headless render smoke tests ─────────────────────────────────────────

    fn run_frames(state: &mut ViewerState, cfg: &ViewerConfig, traces: &[Trace], frames: usize) -> ViewerOutput {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut last = ViewerOutput::default();
        for _ in 0..frames {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    last = state.show(ui, cfg, traces);
                });
            });
        }
        last
    }

    #[test]
    fn renders_with_no_traces_and_with_degenerate_data() {
        let mut s = ViewerState::default();
        run_frames(&mut s, &tran_cfg(), &[], 2);
        run_frames(&mut s, &ac_cfg(), &[], 2);
        let flat = Trace { name: "k".into(), color: Color32::RED, x: vec![0.0, 1.0], y: vec![3.0, 3.0], phase: None };
        run_frames(&mut s, &tran_cfg(), &[flat], 2);
        let one = Trace { name: "p".into(), color: Color32::RED, x: vec![1.0], y: vec![f64::NAN], phase: None };
        run_frames(&mut s, &tran_cfg(), &[one], 2);
    }

    #[test]
    fn renders_real_and_ac_traces_with_cursors() {
        let mut s = ViewerState::default();
        let traces = [sine_trace()];
        run_frames(&mut s, &tran_cfg(), &traces, 2);
        s.place_cursor_on_active();
        s.place_cursor_on_active();
        s.place_cursor_on_active(); // third is a no-op
        run_frames(&mut s, &tran_cfg(), &traces, 2);
        assert!(s.has_cursors());
        assert!(s.cursors[0].is_some() && s.cursors[1].is_some());
        s.clear_cursors();
        assert!(!s.has_cursors());

        let mut a = ViewerState::default();
        let ac = [rc_trace()];
        run_frames(&mut a, &ac_cfg(), &ac, 2);
        a.place_cursor_on_active();
        a.place_cursor_on_active();
        run_frames(&mut a, &ac_cfg(), &ac, 2);
        assert!(a.cursors[1].is_some());
        a.cursors[0].as_mut().unwrap().qty = AcQty::Phase;
        run_frames(&mut a, &ac_cfg(), &ac, 2);
    }

    #[test]
    fn dropped_trace_removes_its_cursor_and_active_marker() {
        let mut s = ViewerState::default();
        let two = [sine_trace(), sine_trace()];
        run_frames(&mut s, &tran_cfg(), &two, 1);
        s.set_active_trace(Some(1));
        s.place_cursor_on_active();
        run_frames(&mut s, &tran_cfg(), &two, 1);
        assert!(s.has_cursors());
        run_frames(&mut s, &tran_cfg(), &two[..1], 1);
        assert!(!s.has_cursors());
        assert_eq!(s.active, Some(0));
    }

    #[test]
    fn axis_dialog_round_trip_sets_explicit_limits() {
        let mut s = ViewerState::default();
        let traces = [sine_trace()];
        run_frames(&mut s, &tran_cfg(), &traces, 1);
        s.dialog_open_request = Some(AxisId::X);
        run_frames(&mut s, &tran_cfg(), &traces, 2);
        let d = s.dialog.as_mut().expect("dialog opened");
        d.lo = "10ms".into();
        d.hi = "50ms".into();
        d.tick = "10ms".into();
        let mut d = s.dialog.take().unwrap();
        assert!(s.apply_dialog(&mut d, &tran_cfg()));
        assert_eq!(s.x, Some((0.01, 0.05)));
        assert_eq!(s.x_tick, Some(0.01));
        run_frames(&mut s, &tran_cfg(), &traces, 1);
        // Reversed limits are rejected and flagged.
        let mut bad = AxisDialog {
            which: AxisId::Y, unit: "V".into(), quantity: String::new(),
            lo: "-10V".into(), hi: "10V".into(), tick: String::new(), log: false, bad: [false; 3],
        };
        // Vertical: Top = lo field. Top(-10) < Bottom(10) means reversed.
        assert!(!s.apply_dialog(&mut bad, &tran_cfg()));
        assert!(bad.bad[0] && bad.bad[2]);
        let mut garbage = AxisDialog {
            which: AxisId::Y, unit: "V".into(), quantity: String::new(),
            lo: "oops".into(), hi: "0".into(), tick: String::new(), log: false, bad: [false; 3],
        };
        assert!(!s.apply_dialog(&mut garbage, &tran_cfg()));
        assert!(garbage.bad[0]);
    }

    #[test]
    fn zoom_to_fit_and_zoom_back_restore_the_view() {
        let mut s = ViewerState::default();
        s.x = Some((1.0, 2.0));
        s.y = Some((0.0, 1.0));
        s.zoom_to_fit();
        assert!(s.x.is_none() && s.y.is_none());
        s.zoom_back();
        assert_eq!(s.x, Some((1.0, 2.0)));
        s.autorange_y();
        assert!(s.y.is_none() && s.x == Some((1.0, 2.0)));
    }
}
