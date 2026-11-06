//! MDI chrome: the document tabs and the child-window frame (title bar, window
//! buttons, body) of the LTspice-style workspace.
//!
//! Layout and state stay in the app; these helpers only draw a window into a
//! rect they are handed and report which title-bar control was used.

use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Response, Sense, Stroke, Ui, UiBuilder};

use crate::icons::{paint_icon, Icon};
use crate::theme::{
    BG_HOVER, LT_FRAME, LT_TITLE_ACTIVE, LT_TITLE_INACTIVE, LT_TITLE_INACTIVE_TEXT, LT_WHITE,
    TEXT_PRIMARY, TEXT_SECONDARY, TITLE_ACTIVE_BG,
};

/// Height of a child window's title bar.
pub const TITLE_H: f32 = 24.0;

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
    let hot = enabled && response.hovered();
    let close_hot = hot && matches!(glyph, Glyph::Close);
    if hot {
        let fill = if close_hot { Color32::from_rgb(232, 69, 69) } else { BG_HOVER };
        painter.rect_filled(rect, 4.0, fill);
    }
    let ink = if !enabled {
        Color32::from_gray(190)
    } else if close_hot {
        LT_WHITE
    } else {
        TEXT_SECONDARY
    };
    let s = Stroke::new(1.4, ink);
    let c = rect.center();
    match glyph {
        Glyph::Minimize => {
            painter.line_segment([pos2(c.x - 4.0, c.y + 3.0), pos2(c.x + 4.0, c.y + 3.0)], s);
        }
        Glyph::Maximize => {
            painter.rect_stroke(Rect::from_center_size(c, vec2(8.0, 7.0)), 1.5, s);
        }
        Glyph::Restore => {
            painter.rect_stroke(Rect::from_center_size(c + vec2(1.5, -1.5), vec2(6.0, 5.0)), 1.0, s);
            painter.rect_stroke(Rect::from_center_size(c + vec2(-1.5, 1.5), vec2(6.0, 5.0)), 1.0, s);
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
        painter.rect_filled(title_rect, 0.0, if active { TITLE_ACTIVE_BG } else { LT_TITLE_INACTIVE });
        // Accent underline marks the active window.
        let underline = Rect::from_min_max(pos2(title_rect.min.x, title_rect.max.y - 2.0), title_rect.max);
        painter.rect_filled(underline, 0.0, if active { LT_TITLE_ACTIVE } else { LT_FRAME });
        paint_icon(painter, Rect::from_min_size(title_rect.min + vec2(6.0, 4.0), vec2(16.0, 16.0)), icon);
        painter.text(
            pos2(title_rect.min.x + 28.0, title_rect.center().y - 1.0),
            Align2::LEFT_CENTER,
            title,
            FontId::proportional(12.0),
            if active { TEXT_PRIMARY } else { LT_TITLE_INACTIVE_TEXT },
        );
    }

    // Window buttons, right-aligned: minimize, maximize/restore, close.
    let bw = 22.0;
    let bh = 18.0;
    let top = title_rect.min.y + (TITLE_H - 2.0 - bh) / 2.0;
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
    let size = vec2(galley.size().x + 40.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = if selected {
            LT_WHITE
        } else if response.hovered() {
            BG_HOVER
        } else {
            Color32::from_rgb(236, 239, 244)
        };
        let round = egui::Rounding { nw: 7.0, ne: 7.0, sw: 0.0, se: 0.0 };
        painter.rect(rect, round, fill, Stroke::new(1.0, LT_FRAME));
        if selected {
            // Join to the workspace and mark the tab with the accent.
            painter.line_segment(
                [pos2(rect.left() + 1.0, rect.bottom()), pos2(rect.right() - 1.0, rect.bottom())],
                Stroke::new(1.5, LT_WHITE),
            );
            painter.line_segment(
                [pos2(rect.left() + 6.0, rect.top() + 1.0), pos2(rect.right() - 6.0, rect.top() + 1.0)],
                Stroke::new(2.0, LT_TITLE_ACTIVE),
            );
        }
        paint_icon(painter, Rect::from_min_size(rect.min + vec2(8.0, 5.0), vec2(16.0, 16.0)), icon);
        painter.galley(pos2(rect.min.x + 30.0, rect.center().y - galley.size().y / 2.0), galley, TEXT_PRIMARY);
    }
    response
}
