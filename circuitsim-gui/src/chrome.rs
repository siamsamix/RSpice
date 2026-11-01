//! MDI chrome: the document tabs and the child-window frame (title bar, window
//! buttons, body) of the LTspice-style workspace.
//!
//! Layout and state stay in the app; these helpers only draw a window into a
//! rect they are handed and report which title-bar control was used.

use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Response, Sense, Stroke, Ui, UiBuilder};

use crate::icons::{paint_icon, Icon};
use crate::theme::{
    LT_FRAME, LT_TITLE_ACTIVE, LT_TITLE_INACTIVE, LT_TITLE_INACTIVE_TEXT, LT_WHITE, TEXT_PRIMARY,
};

/// Height of a child window's title bar.
pub const TITLE_H: f32 = 20.0;

/// What the user did to a child window this frame.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowEvents {
    /// A press landed anywhere inside the window — it should become active.
    pub activated: bool,
    pub minimize: bool,
    pub maximize: bool,
    pub close: bool,
}

/// Window-button glyphs.
#[derive(Clone, Copy)]
enum Glyph {
    Minimize,
    Maximize,
    Restore,
    Close,
}

fn title_button(ui: &mut Ui, rect: Rect, glyph: Glyph, enabled: bool, id: egui::Id) -> bool {
    let response = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    let painter = ui.painter();
    let fill = if enabled && response.hovered() { Color32::from_rgb(214, 232, 251) } else { LT_WHITE };
    painter.rect(rect, 0.0, fill, Stroke::new(1.0, Color32::BLACK));
    let ink = if enabled { Color32::BLACK } else { Color32::from_gray(150) };
    let s = Stroke::new(1.5, ink);
    let c = rect.center();
    match glyph {
        Glyph::Minimize => {
            painter.line_segment([pos2(c.x - 4.0, c.y + 3.0), pos2(c.x + 4.0, c.y + 3.0)], s);
        }
        Glyph::Maximize => {
            painter.rect_stroke(Rect::from_center_size(c, vec2(8.0, 7.0)), 0.0, s);
        }
        Glyph::Restore => {
            painter.rect_stroke(Rect::from_center_size(c + vec2(1.5, -1.5), vec2(6.0, 5.0)), 0.0, s);
            painter.rect_stroke(Rect::from_center_size(c + vec2(-1.5, 1.5), vec2(6.0, 5.0)), 0.0, s);
        }
        Glyph::Close => {
            painter.line_segment([pos2(c.x - 3.5, c.y - 3.5), pos2(c.x + 3.5, c.y + 3.5)], s);
            painter.line_segment([pos2(c.x + 3.5, c.y - 3.5), pos2(c.x - 3.5, c.y + 3.5)], s);
        }
    }
    response.clicked()
}

/// Draw one child window into `rect`: frame, title bar (blue when `active`,
/// gray otherwise), the three window buttons, and the body, which `body`
/// fills. `id` must be unique per window (it keys the window buttons).
/// `maximized` swaps the maximize glyph for restore; `can_close` greys the
/// close button out.
pub fn child_window(
    ui: &mut Ui,
    id: egui::Id,
    rect: Rect,
    title: &str,
    icon: Icon,
    active: bool,
    maximized: bool,
    can_close: bool,
    body_fill: Color32,
    body: impl FnOnce(&mut Ui),
) -> WindowEvents {
    let mut events = WindowEvents::default();

    let title_rect = Rect::from_min_size(rect.min, vec2(rect.width(), TITLE_H));
    let body_rect = Rect::from_min_max(pos2(rect.min.x, title_rect.max.y), rect.max);

    // Press anywhere inside activates the window (read before the body can
    // consume the click).
    let pressed_inside = ui.input(|i| {
        i.pointer.primary_pressed()
            && i.pointer.interact_pos().map_or(false, |p| rect.contains(p))
    });
    events.activated = pressed_inside;

    {
        let painter = ui.painter();
        painter.rect_filled(body_rect, 0.0, body_fill);
        painter.rect_filled(title_rect, 0.0, if active { LT_TITLE_ACTIVE } else { LT_TITLE_INACTIVE });
        paint_icon(painter, Rect::from_min_size(title_rect.min + vec2(4.0, 2.0), vec2(16.0, 16.0)), icon);
        painter.text(
            pos2(title_rect.min.x + 26.0, title_rect.center().y),
            Align2::LEFT_CENTER,
            title,
            FontId::proportional(12.0),
            if active { TEXT_PRIMARY } else { LT_TITLE_INACTIVE_TEXT },
        );
    }

    // Window buttons, right-aligned: minimize, maximize/restore, close.
    let bw = 20.0;
    let bh = 16.0;
    let top = title_rect.min.y + (TITLE_H - bh) / 2.0;
    let right = title_rect.max.x - 3.0;
    let close_rect = Rect::from_min_size(pos2(right - bw, top), vec2(bw, bh));
    let max_rect = Rect::from_min_size(pos2(right - 2.0 * bw - 2.0, top), vec2(bw, bh));
    let min_rect = Rect::from_min_size(pos2(right - 3.0 * bw - 4.0, top), vec2(bw, bh));
    events.minimize = title_button(ui, min_rect, Glyph::Minimize, true, id.with("min"));
    events.maximize = title_button(
        ui,
        max_rect,
        if maximized { Glyph::Restore } else { Glyph::Maximize },
        true,
        id.with("max"),
    );
    events.close = title_button(ui, close_rect, Glyph::Close, can_close, id.with("close"));

    // Body: a child ui clipped to the window so nothing bleeds over the
    // title bar or the neighbouring window.
    let inner = body_rect.shrink(1.0);
    let mut child = ui.new_child(UiBuilder::new().max_rect(inner));
    child.set_clip_rect(inner.intersect(ui.clip_rect()));
    body(&mut child);

    ui.painter()
        .rect_stroke(rect, 0.0, Stroke::new(1.0, LT_FRAME));
    events
}

/// One document tab: icon + label, white and joined to the workspace when
/// `selected`.
pub fn tab(ui: &mut Ui, label: &str, icon: Icon, selected: bool) -> Response {
    let font = FontId::proportional(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, TEXT_PRIMARY);
    let size = vec2(galley.size().x + 36.0, 24.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = if selected {
            LT_WHITE
        } else if response.hovered() {
            Color32::from_rgb(222, 236, 250)
        } else {
            Color32::from_rgb(232, 232, 232)
        };
        painter.rect_filled(rect, 0.0, fill);
        let border = Stroke::new(1.0, LT_FRAME);
        painter.line_segment([rect.left_top(), rect.right_top()], border);
        painter.line_segment([rect.left_top(), rect.left_bottom()], border);
        painter.line_segment([rect.right_top(), rect.right_bottom()], border);
        if !selected {
            painter.line_segment([rect.left_bottom(), rect.right_bottom()], border);
        }
        paint_icon(painter, Rect::from_min_size(rect.min + vec2(6.0, 4.0), vec2(16.0, 16.0)), icon);
        painter.galley(pos2(rect.min.x + 26.0, rect.center().y - galley.size().y / 2.0), galley, TEXT_PRIMARY);
    }
    response
}
