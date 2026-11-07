//! Library parts in the GUI: drawing a symbol's body, the screen geometry the
//! canvas hit-tests with, and the component picker window (Edit ▸ Component,
//! F2). The data — pins, attached model/subckt, spiceline — lives in
//! `circuitsim::symbols` and `schematic::PartData`; this module only draws and
//! lets the user choose.

use crate::schematic::{CompKind, Component, GridPt};
use circuitsim::symbols::{RefKind, Shape, Symbol, SymbolLibrary};
use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};

/// A symbol-frame point `(x, y)` (grid units, floats) turned `rot` quarter
/// turns with the editor's `(x, y) → (−y, x)` law.
fn turned(x: f32, y: f32, rot: u8) -> (f32, f32) {
    let (mut x, mut y) = (x, y);
    for _ in 0..(rot % 4) {
        (x, y) = (-y, x);
    }
    (x, y)
}

fn at(origin: Pos2, unit: f32, rot: u8, x: f32, y: f32) -> Pos2 {
    let (tx, ty) = turned(x, y, rot);
    origin + Vec2::new(tx, ty) * unit
}

/// Draw `symbol`'s body. `origin` is the first pin's screen position and
/// `unit` the screen length of one grid step.
pub fn paint_body(painter: &Painter, symbol: &Symbol, origin: Pos2, unit: f32, rot: u8, stroke: Stroke) {
    for shape in &symbol.shapes {
        match *shape {
            Shape::Line { x1, y1, x2, y2 } => {
                painter.line_segment(
                    [at(origin, unit, rot, x1, y1), at(origin, unit, rot, x2, y2)],
                    stroke,
                );
            }
            Shape::Rect { x1, y1, x2, y2 } => {
                painter.add(egui::Shape::closed_line(
                    vec![
                        at(origin, unit, rot, x1, y1),
                        at(origin, unit, rot, x2, y1),
                        at(origin, unit, rot, x2, y2),
                        at(origin, unit, rot, x1, y2),
                    ],
                    stroke,
                ));
            }
            Shape::Circle { cx, cy, r } => {
                painter.circle_stroke(at(origin, unit, rot, cx, cy), r * unit, stroke);
            }
        }
    }
}

/// Mark a symbol's pins (the connection points wires snap to).
pub fn paint_pin_dots(painter: &Painter, symbol: &Symbol, origin: Pos2, unit: f32, rot: u8, color: Color32) {
    for pin in &symbol.pins {
        painter.circle_filled(at(origin, unit, rot, pin.x as f32, pin.y as f32), 2.5, color);
    }
}

/// Screen length of one grid step under `to_screen`.
fn grid_unit(to_screen: &impl Fn(GridPt) -> Pos2) -> f32 {
    (to_screen(GridPt(1, 0)) - to_screen(GridPt(0, 0))).x
}

/// Draw a placed part: its symbol's body and pin dots (a plain box around the
/// pins when the library no longer has the symbol, so the part stays visible
/// and selectable instead of vanishing).
pub fn paint_part(
    painter: &Painter,
    c: &Component,
    symbol: Option<&Symbol>,
    to_screen: impl Fn(GridPt) -> Pos2,
    stroke: Stroke,
    pin_color: Color32,
) {
    let Some(part) = c.part.as_ref() else { return };
    match symbol {
        Some(symbol) => {
            let (origin, unit) = (to_screen(c.p1), grid_unit(&to_screen));
            paint_body(painter, symbol, origin, unit, part.rot, stroke);
            paint_pin_dots(painter, symbol, origin, unit, part.rot, pin_color);
        }
        None => {
            let body = pins_bounds(c, &to_screen).expand(6.0);
            painter.rect_stroke(body, 0.0, stroke);
            for pin in c.pin_points() {
                painter.circle_filled(to_screen(pin), 2.5, pin_color);
            }
        }
    }
}

/// Bounding box of a part's pins on screen.
fn pins_bounds(c: &Component, to_screen: &impl Fn(GridPt) -> Pos2) -> Rect {
    let pts = c.pin_points();
    let mut rect = Rect::from_min_max(to_screen(pts[0]), to_screen(pts[0]));
    for pt in &pts[1..] {
        rect.extend_with(to_screen(*pt));
    }
    rect
}

/// The screen rectangle that selects `c` when clicked. A two-terminal part
/// keeps its small box around the midpoint (padded so the hit area is a bit
/// larger than the drawn body); a library part covers its pins and drawn
/// body together, so a click anywhere on the symbol selects it.
pub fn component_hit_rect(
    c: &Component,
    symbol: Option<&Symbol>,
    to_screen: impl Fn(GridPt) -> Pos2,
) -> Rect {
    if c.kind != CompKind::Part {
        let p1 = to_screen(c.p1);
        let p2 = to_screen(c.p2);
        let center = p1 + (p2 - p1) / 2.0;
        let is_horizontal = c.p1.1 == c.p2.1;
        let box_size = if is_horizontal { Vec2::new(24.0, 16.0) } else { Vec2::new(16.0, 24.0) };
        return Rect::from_center_size(center, box_size + Vec2::new(12.0, 12.0));
    }
    part_bounds(c, symbol, &to_screen).expand(6.0)
}

/// Pins and drawing of a part on screen, unpadded.
pub fn part_bounds(c: &Component, symbol: Option<&Symbol>, to_screen: &impl Fn(GridPt) -> Pos2) -> Rect {
    let mut rect = pins_bounds(c, to_screen);
    if let (Some(symbol), Some(part)) = (symbol, c.part.as_ref()) {
        let (origin, unit) = (to_screen(c.p1), grid_unit(to_screen));
        for shape in &symbol.shapes {
            let pts: &[(f32, f32)] = match *shape {
                Shape::Line { x1, y1, x2, y2 } | Shape::Rect { x1, y1, x2, y2 } => &[(x1, y1), (x2, y2)],
                Shape::Circle { cx, cy, r } => &[(cx - r, cy - r), (cx + r, cy + r)],
            };
            for &(x, y) in pts {
                rect.extend_with(at(origin, unit, part.rot, x, y));
            }
        }
    }
    rect
}

// ── The component picker ────────────────────────────────────────────────────

/// What the picker window asks of the app this frame.
pub enum PickerEvent {
    None,
    Cancel,
    /// OK / double click: arm the Part tool with this symbol.
    Place(String),
    /// The Load Library… button (the app owns the file dialog).
    LoadLibrary,
}

/// The Select Component Symbol dialog: a filterable, category-grouped symbol
/// list with a preview and the data placing the symbol would attach.
pub struct PartPicker {
    filter: String,
    selected: Option<String>,
    /// True until the first frame has put the cursor in the filter field.
    fresh: bool,
}

impl PartPicker {
    pub fn new(preselected: Option<&str>) -> Self {
        Self { filter: String::new(), selected: preselected.map(str::to_string), fresh: true }
    }

    fn matches(&self, s: &Symbol) -> bool {
        let needle = self.filter.trim().to_ascii_lowercase();
        needle.is_empty()
            || [&s.name, &s.description, &s.category, &s.reference]
                .iter()
                .any(|hay| hay.to_ascii_lowercase().contains(&needle))
    }

    pub fn show(&mut self, ctx: &egui::Context, symbols: &SymbolLibrary) -> PickerEvent {
        let mut event = PickerEvent::None;
        egui::Window::new("Select Component Symbol")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let filter = ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("Filter by name, kind or model…")
                        .desired_width(420.0),
                );
                if self.fresh {
                    filter.request_focus();
                    self.fresh = false;
                }
                ui.add_space(4.0);
                ui.horizontal_top(|ui| {
                    ui.allocate_ui(Vec2::new(190.0, 270.0), |ui| {
                        egui::ScrollArea::vertical().id_salt("part_list").max_height(270.0).show(ui, |ui| {
                            let mut shown = 0;
                            for category in symbols.categories() {
                                let items: Vec<&Symbol> = symbols
                                    .symbols
                                    .iter()
                                    .filter(|s| s.category.eq_ignore_ascii_case(category) && self.matches(s))
                                    .collect();
                                if items.is_empty() {
                                    continue;
                                }
                                shown += items.len();
                                egui::CollapsingHeader::new(category)
                                    .id_salt(("part_category", category))
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        for s in items {
                                            let on = self.selected.as_deref() == Some(s.name.as_str());
                                            let row = ui.selectable_label(on, format!("{}  ({})", s.name, s.prefix));
                                            if row.clicked() {
                                                self.selected = Some(s.name.clone());
                                            }
                                            if row.double_clicked() {
                                                event = PickerEvent::Place(s.name.clone());
                                            }
                                        }
                                    });
                            }
                            if shown == 0 {
                                ui.label("No symbol matches.");
                            }
                        });
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(230.0);
                        match self.selected.as_deref().and_then(|n| symbols.get(n)) {
                            Some(symbol) => symbol_details(ui, symbol),
                            None => {
                                ui.label("Pick a symbol from the list.");
                            }
                        }
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ready = self.selected.as_deref().and_then(|n| symbols.get(n)).is_some();
                    if ui.add_enabled(ready, egui::Button::new("OK")).clicked() {
                        if let Some(name) = &self.selected {
                            event = PickerEvent::Place(name.clone());
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        event = PickerEvent::Cancel;
                    }
                    if ui.button("Load Library…").clicked() {
                        event = PickerEvent::LoadLibrary;
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            event = PickerEvent::Cancel;
        }
        event
    }
}

/// Preview + the SPICE data a placement of `symbol` attaches.
fn symbol_details(ui: &mut egui::Ui, symbol: &Symbol) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(230.0, 130.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_gray(24));
    // Fit the symbol's pins and drawing into the preview box.
    let (mut min, mut max) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
    let mut grow = |x: f32, y: f32| {
        min = (min.0.min(x), min.1.min(y));
        max = (max.0.max(x), max.1.max(y));
    };
    for p in &symbol.pins {
        grow(p.x as f32, p.y as f32);
    }
    for s in &symbol.shapes {
        match *s {
            Shape::Line { x1, y1, x2, y2 } | Shape::Rect { x1, y1, x2, y2 } => {
                grow(x1, y1);
                grow(x2, y2);
            }
            Shape::Circle { cx, cy, r } => {
                grow(cx - r, cy - r);
                grow(cx + r, cy + r);
            }
        }
    }
    let (w, h) = ((max.0 - min.0).max(1.0), (max.1 - min.1).max(1.0));
    let unit = ((rect.width() - 40.0) / w).min((rect.height() - 30.0) / h).min(30.0);
    let center = Pos2::new((min.0 + max.0) / 2.0, (min.1 + max.1) / 2.0);
    let origin = rect.center() - Vec2::new(center.x, center.y) * unit;
    paint_body(&painter, symbol, origin, unit, 0, Stroke::new(1.5, crate::theme::LT_WIRE));
    paint_pin_dots(&painter, symbol, origin, unit, 0, Color32::from_rgb(230, 80, 40));

    ui.add_space(4.0);
    if !symbol.description.is_empty() {
        ui.label(&symbol.description);
    }
    let pins: Vec<&str> = symbol.pins.iter().map(|p| p.name.as_str()).collect();
    ui.label(format!("Card {}…  pins: {}", symbol.prefix, pins.join(" ")));
    let what = match symbol.ref_kind {
        RefKind::Model => "Model",
        RefKind::Subckt => "Subckt",
    };
    if symbol.reference.is_empty() {
        ui.label(format!("{what}: type one after placing"));
    } else {
        ui.label(format!("{what}: {}", symbol.reference));
    }
    if let Some(lib) = &symbol.lib {
        ui.label(format!("Library: {lib}"));
    }
    if !symbol.spiceline.is_empty() {
        ui.label(format!("SpiceLine: {}", symbol.spiceline));
    }
}
