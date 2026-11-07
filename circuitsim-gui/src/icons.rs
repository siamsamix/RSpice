//! Toolbar, tab and title-bar icons, drawn with the egui painter.
//!
//! The reference toolbar is a row of bold black / blue / green glyphs on
//! white. Each icon here is described in a unit square (`0..1` on both axes)
//! and scaled to whatever rect it is painted into, so the same drawing serves
//! the 28 px toolbar buttons and the 16 px tab and title-bar icons.

use egui::epaint::{PathShape, Shape};
use egui::{vec2, Color32, Painter, Pos2, Rect, Response, Sense, Stroke, Ui};

use crate::theme::{pal, ACCENT_DIM, BG_HOVER, BORDER_SUBTLE};

/// Every glyph the chrome draws.
#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    // File / run
    Open,
    Save,
    Run,
    Stop,
    /// Gear showing the active analysis command (`.tran`, `.ac`).
    Analysis(&'static str),
    // Window / view
    TileVertical,
    TileHorizontal,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    // Schematic tools
    Select,
    Wire,
    Ground,
    Resistor,
    Capacitor,
    Inductor,
    Voltage,
    Rotate,
    Delete,
    // Document kinds (tab strip / title bars)
    Schematic,
    Netlist,
    Waveform,
}

const BLACK: Color32 = Color32::BLACK;
const GREEN: Color32 = Color32::from_rgb(0, 200, 0);
const GRAY: Color32 = Color32::from_rgb(160, 160, 160);

/// Desaturate a color for a disabled button.
fn dim(c: Color32, enabled: bool) -> Color32 {
    if enabled {
        return c;
    }
    let luma = (c.r() as u32 * 30 + c.g() as u32 * 59 + c.b() as u32 * 11) / 100;
    Color32::from_gray((luma / 2 + 120).min(200) as u8)
}

/// A toolbar button: flat icon, light-blue bevel when hovered or `selected`.
pub fn icon_button(ui: &mut Ui, icon: Icon, tip: &str, selected: bool, enabled: bool) -> Response {
    let size = vec2(32.0, 32.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let hovered = enabled && response.hovered();
        if selected || (hovered && response.is_pointer_button_down_on()) {
            painter.rect(rect, 1.0, Color32::from_rgb(184, 217, 252), Stroke::new(1.0, ACCENT_DIM));
        } else if hovered {
            painter.rect(rect, 1.0, BG_HOVER, Stroke::new(1.0, BORDER_SUBTLE));
        }
        paint(painter, rect.shrink(4.0), icon, enabled);
    }
    response.on_hover_text(tip)
}

/// Paint `icon` into `rect` (enabled colors).
pub fn paint_icon(painter: &Painter, rect: Rect, icon: Icon) {
    paint(painter, rect, icon, true);
}

fn paint(painter: &Painter, rect: Rect, icon: Icon, enabled: bool) {
    let w = rect.width();
    // Unit-square → screen mapping and a stroke scaled to the icon size.
    let p = |x: f32, y: f32| -> Pos2 { rect.min + vec2(x * w, y * rect.height()) };
    let lw = |units: f32| -> f32 { (units * w / 14.0).max(1.0) };
    let stroke = |c: Color32, units: f32| Stroke::new(lw(units), dim(c, enabled));
    let line = |pts: &[(f32, f32)], s: Stroke| {
        painter.add(Shape::line(pts.iter().map(|&(x, y)| p(x, y)).collect(), s));
    };
    let poly_fill = |pts: &[(f32, f32)], fill: Color32, s: Stroke| {
        painter.add(Shape::convex_polygon(
            pts.iter().map(|&(x, y)| p(x, y)).collect(),
            dim(fill, enabled),
            s,
        ));
    };
    let closed = |pts: &[(f32, f32)], s: Stroke| {
        painter.add(Shape::Path(PathShape::closed_line(
            pts.iter().map(|&(x, y)| p(x, y)).collect(),
            s,
        )));
    };
    let rect_u = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(p(x0, y0), p(x1, y1));

    match icon {
        Icon::Open => {
            // Black folder with a blue arrow.
            painter.rect_filled(rect_u(0.08, 0.22, 0.42, 0.34), 1.0, dim(BLACK, enabled));
            painter.rect_filled(rect_u(0.08, 0.30, 0.92, 0.84), 1.0, dim(BLACK, enabled));
            poly_fill(&[(0.5, 0.36), (0.72, 0.58), (0.28, 0.58)], pal().wire, Stroke::NONE);
            painter.rect_filled(rect_u(0.44, 0.56, 0.56, 0.74), 0.0, dim(pal().wire, enabled));
        }
        Icon::Save => {
            painter.rect_filled(rect_u(0.1, 0.1, 0.9, 0.9), 2.0, dim(BLACK, enabled));
            painter.rect_filled(rect_u(0.28, 0.1, 0.72, 0.4), 0.0, dim(Color32::WHITE, enabled));
            painter.rect_filled(rect_u(0.24, 0.58, 0.76, 0.9), 0.0, dim(pal().wire, enabled));
        }
        Icon::Run => {
            poly_fill(&[(0.2, 0.08), (0.2, 0.92), (0.86, 0.5)], GREEN, stroke(Color32::from_rgb(0, 120, 0), 1.0));
        }
        Icon::Stop => {
            painter.rect_filled(rect_u(0.14, 0.14, 0.86, 0.86), 2.0, dim(Color32::from_gray(170), enabled));
        }
        Icon::Analysis(label) => {
            // Gear: toothed ring + body, label on top.
            let c = p(0.5, 0.5);
            let r = 0.46 * w;
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::TAU / 8.0;
                painter.circle_filled(c + vec2(a.cos(), a.sin()) * r * 0.92, r * 0.2, dim(GREEN, enabled));
            }
            painter.circle_filled(c, r * 0.86, dim(GREEN, enabled));
            let size = if label.len() <= 3 { w * 0.40 } else { w * 0.32 };
            painter.text(c, egui::Align2::CENTER_CENTER, label, crate::theme::bold(size), dim(BLACK, enabled));
        }
        Icon::TileVertical => {
            let s = stroke(pal().wire, 1.4);
            painter.rect(rect_u(0.06, 0.1, 0.46, 0.9), 2.0, Color32::WHITE, s);
            painter.rect(rect_u(0.54, 0.1, 0.94, 0.9), 2.0, Color32::WHITE, s);
        }
        Icon::TileHorizontal => {
            let s = stroke(pal().wire, 1.4);
            painter.rect(rect_u(0.1, 0.06, 0.9, 0.46), 2.0, Color32::WHITE, s);
            painter.rect(rect_u(0.1, 0.54, 0.9, 0.94), 2.0, Color32::WHITE, s);
        }
        Icon::ZoomIn | Icon::ZoomOut | Icon::ZoomFit => {
            // Black lens with a white glyph, thick handle.
            line(&[(0.62, 0.62), (0.92, 0.92)], stroke(BLACK, 2.6));
            painter.circle_filled(p(0.42, 0.42), 0.36 * w, dim(BLACK, enabled));
            let white = stroke(Color32::WHITE, 1.6);
            match icon {
                Icon::ZoomIn => {
                    line(&[(0.26, 0.42), (0.58, 0.42)], white);
                    line(&[(0.42, 0.26), (0.42, 0.58)], white);
                }
                Icon::ZoomOut => line(&[(0.26, 0.42), (0.58, 0.42)], white),
                _ => {
                    line(&[(0.28, 0.56), (0.56, 0.28)], white);
                    line(&[(0.28, 0.56), (0.28, 0.44)], white);
                    line(&[(0.28, 0.56), (0.40, 0.56)], white);
                    line(&[(0.56, 0.28), (0.44, 0.28)], white);
                    line(&[(0.56, 0.28), (0.56, 0.40)], white);
                }
            }
        }
        Icon::Select => {
            poly_fill(&[(0.24, 0.08), (0.24, 0.84), (0.82, 0.56)], BLACK, Stroke::NONE);
            line(&[(0.5, 0.66), (0.72, 0.94)], stroke(BLACK, 2.4));
        }
        Icon::Wire => {
            let s = stroke(pal().wire, 1.8);
            line(&[(0.2, 0.18), (0.2, 0.5), (0.8, 0.5), (0.8, 0.82)], s);
            for (x, y) in [(0.2, 0.18), (0.8, 0.82)] {
                painter.circle(p(x, y), 0.09 * w, Color32::WHITE, s);
            }
        }
        Icon::Ground => {
            let s = stroke(BLACK, 1.8);
            line(&[(0.5, 0.1), (0.5, 0.48)], s);
            closed(&[(0.14, 0.48), (0.86, 0.48), (0.5, 0.9)], s);
            painter.circle_filled(p(0.5, 0.1), 0.07 * w, dim(pal().wire, enabled));
        }
        Icon::Resistor => {
            let s = stroke(pal().wire, 1.6);
            let mut pts = vec![(0.04, 0.5), (0.22, 0.5)];
            for k in 0..6 {
                let x = 0.22 + (k as f32 + 0.5) * (0.56 / 6.0);
                pts.push((x, if k % 2 == 0 { 0.22 } else { 0.78 }));
            }
            pts.extend([(0.78, 0.5), (0.96, 0.5)]);
            line(&pts, s);
        }
        Icon::Capacitor => {
            let s = stroke(pal().wire, 1.6);
            line(&[(0.04, 0.5), (0.4, 0.5)], s);
            line(&[(0.6, 0.5), (0.96, 0.5)], s);
            let plate = stroke(pal().wire, 2.4);
            line(&[(0.4, 0.18), (0.4, 0.82)], plate);
            line(&[(0.6, 0.18), (0.6, 0.82)], plate);
        }
        Icon::Inductor => {
            let s = stroke(pal().wire, 1.6);
            let mut pts = vec![(0.04, 0.6)];
            for k in 0..3 {
                for step in 0..=8 {
                    let a = std::f32::consts::PI * step as f32 / 8.0;
                    let x = 0.2 + k as f32 * 0.2 + 0.1 - 0.1 * a.cos();
                    let y = 0.6 - 0.34 * a.sin();
                    pts.push((x, y));
                }
            }
            pts.push((0.96, 0.6));
            line(&pts, s);
        }
        Icon::Voltage => {
            let s = stroke(pal().wire, 1.6);
            line(&[(0.5, 0.02), (0.5, 0.18)], s);
            line(&[(0.5, 0.82), (0.5, 0.98)], s);
            painter.circle_stroke(p(0.5, 0.5), 0.32 * w, s);
            let k = stroke(BLACK, 1.2);
            line(&[(0.4, 0.36), (0.6, 0.36)], k);
            line(&[(0.5, 0.26), (0.5, 0.46)], k);
            line(&[(0.4, 0.64), (0.6, 0.64)], k);
        }
        Icon::Rotate => {
            let s = stroke(BLACK, 2.0);
            let c = (0.5_f32, 0.5_f32);
            let pts: Vec<(f32, f32)> = (0..=18)
                .map(|i| {
                    let a = (40.0 + i as f32 * 15.0).to_radians();
                    (c.0 + 0.34 * a.cos(), c.1 - 0.34 * a.sin())
                })
                .collect();
            line(&pts, s);
            let end = *pts.last().unwrap();
            poly_fill(&[(end.0 - 0.16, end.1 - 0.02), (end.0 + 0.12, end.1 - 0.04), (end.0 - 0.02, end.1 + 0.2)], BLACK, Stroke::NONE);
        }
        Icon::Delete => {
            painter.circle_filled(p(0.5, 0.5), 0.42 * w, dim(BLACK, enabled));
            let white = stroke(Color32::WHITE, 2.0);
            line(&[(0.34, 0.34), (0.66, 0.66)], white);
            line(&[(0.66, 0.34), (0.34, 0.66)], white);
        }
        Icon::Schematic => {
            let s = stroke(BLACK, 1.4);
            line(&[(0.06, 0.5), (0.5, 0.5)], s);
            line(&[(0.5, 0.5), (0.9, 0.14)], s);
            line(&[(0.5, 0.5), (0.9, 0.86)], s);
            line(&[(0.5, 0.18), (0.5, 0.82)], s);
        }
        Icon::Netlist => {
            painter.rect(rect_u(0.14, 0.06, 0.86, 0.94), 0.0, Color32::WHITE, stroke(BLACK, 1.0));
            for y in [0.24, 0.42, 0.6, 0.78] {
                line(&[(0.26, y), (0.74, y)], stroke(GRAY, 1.0));
            }
        }
        Icon::Waveform => {
            painter.rect(rect_u(0.04, 0.08, 0.96, 0.92), 0.0, Color32::WHITE, stroke(BLACK, 1.0));
            let red: Vec<(f32, f32)> = (0..=16)
                .map(|i| {
                    let x = 0.1 + 0.8 * i as f32 / 16.0;
                    (x, 0.48 - 0.26 * (i as f32 / 16.0 * std::f32::consts::TAU).sin())
                })
                .collect();
            line(&red, stroke(Color32::from_rgb(220, 0, 0), 1.0));
            line(&[(0.1, 0.74), (0.9, 0.74)], stroke(pal().wire, 1.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paint `icon` alone on a bare layer — no panel, so nothing else in the
    /// frame draws — and return the shapes it produced.
    fn paint_alone(ctx: &egui::Context, rect: Rect, icon: Icon, enabled: bool) -> Vec<egui::epaint::ClippedShape> {
        ctx.run(egui::RawInput::default(), |ctx| {
            let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("icon_test"));
            paint(&ctx.layer_painter(layer), rect, icon, enabled);
        })
        .shapes
    }

    /// Every icon, enabled or disabled, at toolbar and tab sizes, draws
    /// something — and only inside its own square.
    #[test]
    fn all_icons_paint_inside_their_rect() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let icons = [
            Icon::Open, Icon::Save, Icon::Run, Icon::Stop, Icon::Analysis(".tran"),
            Icon::Analysis(".ac"), Icon::TileVertical, Icon::TileHorizontal, Icon::ZoomIn,
            Icon::ZoomOut, Icon::ZoomFit, Icon::Select, Icon::Wire, Icon::Ground,
            Icon::Resistor, Icon::Capacitor, Icon::Inductor, Icon::Voltage, Icon::Rotate,
            Icon::Delete, Icon::Schematic, Icon::Netlist, Icon::Waveform,
        ];
        for (n, icon) in icons.into_iter().enumerate() {
            for size in [16.0, 24.0] {
                for enabled in [true, false] {
                    let rect = Rect::from_min_size(egui::pos2(10.0, 10.0), vec2(size, size));
                    let shapes = paint_alone(&ctx, rect, icon, enabled);
                    assert!(
                        !shapes.is_empty(),
                        "icon #{n} (size {size}, enabled {enabled}) painted nothing"
                    );
                    for clipped in &shapes {
                        let bounds = clipped.shape.visual_bounding_rect();
                        assert!(
                            rect.expand(2.0).contains_rect(bounds),
                            "icon #{n} (size {size}) drew outside its {rect:?}: {bounds:?}"
                        );
                    }
                }
            }
        }
    }

    /// The harness itself must see a no-op: if painting nothing also produced
    /// shapes, the test above could not fail.
    #[test]
    fn an_empty_frame_has_no_shapes() {
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("icon_test"));
            let _unused = ctx.layer_painter(layer);
        });
        assert!(out.shapes.is_empty());
    }
}
