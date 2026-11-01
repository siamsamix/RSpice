use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Rounding, Stroke, TextStyle, Visuals};

// ---------------------------------------------------------------------------
// Color Palette — LTspice look.
// The `LT_*` colors were sampled pixel-for-pixel from the reference
// screenshots `docs/ltspice-window.png` and
// `docs/ltspice-window_with_circuit_and_graph.png`; the surface/text/accent
// constants below are derived from them, so every card, grid and button in the
// app follows the same light chrome.
// ---------------------------------------------------------------------------

/// Menu bar, toolbar and the selected tab — pure white.
pub const LT_WHITE: Color32 = Color32::from_rgb(255, 255, 255);
/// Tab strip and status bar.
pub const LT_STRIP: Color32 = Color32::from_rgb(245, 245, 245);
/// MDI workspace and the schematic canvas.
pub const LT_WORKSPACE: Color32 = Color32::from_rgb(192, 192, 192);
/// Workspace / child-window border.
pub const LT_FRAME: Color32 = Color32::from_rgb(106, 106, 106);
/// Title bar of the active child window.
pub const LT_TITLE_ACTIVE: Color32 = Color32::from_rgb(50, 150, 250);
/// Title bar of an inactive child window.
pub const LT_TITLE_INACTIVE: Color32 = Color32::from_rgb(128, 128, 128);
/// Title text of an inactive child window.
pub const LT_TITLE_INACTIVE_TEXT: Color32 = Color32::from_rgb(192, 192, 192);
/// Waveform viewer background.
pub const LT_PLOT_BG: Color32 = Color32::BLACK;
/// Waveform viewer frame, tick marks and axis text.
pub const LT_PLOT_FRAME: Color32 = Color32::from_rgb(175, 175, 175);
/// Schematic wires and symbols.
pub const LT_WIRE: Color32 = Color32::from_rgb(0, 0, 255);
/// Schematic text (names, values, directives).
pub const LT_SCHEM_TEXT: Color32 = Color32::BLACK;

/// Primary interactive accent — the active-title blue.
pub const ACCENT: Color32 = LT_TITLE_ACTIVE;
pub const ACCENT_DIM: Color32 = Color32::from_rgb(30, 110, 200);

/// Surface hierarchy.
pub const BG_BASE: Color32 = LT_WHITE;                            // Text editors, input fields
pub const BG_PANEL: Color32 = LT_STRIP;                           // Bars, panels
pub const BG_SURFACE: Color32 = LT_WHITE;                         // Cards
pub const BG_CONTROL: Color32 = Color32::from_rgb(225, 225, 225); // Buttons
pub const BG_HOVER: Color32 = Color32::from_rgb(214, 232, 251);   // Hovered controls

/// Borders — structural and subtle.
pub const BORDER: Color32 = LT_FRAME;
pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(200, 200, 200);

/// Text — black primary, two muted levels.
pub const TEXT_PRIMARY: Color32 = Color32::BLACK;
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(80, 80, 80);
pub const TEXT_DISABLED: Color32 = Color32::from_rgb(144, 144, 144);

/// Status — readable on the light chrome.
pub const STATUS_OK: Color32 = Color32::from_rgb(20, 130, 60);
pub const STATUS_WARN: Color32 = Color32::from_rgb(180, 110, 0);
pub const STATUS_ERROR: Color32 = Color32::from_rgb(190, 30, 30);
pub const STATUS_INFO: Color32 = Color32::from_rgb(30, 110, 200);

// ---------------------------------------------------------------------------
// Waveform Trace Colors — bright on the black viewer. The first trace is
// LTspice's green (sampled from the reference); the rest are distinct,
// high-luminance colors in a fixed order.
// ---------------------------------------------------------------------------
pub const PLOT_COLORS: [Color32; 8] = [
    Color32::from_rgb(0, 255, 0),     // 1 — LTspice green (reference)
    Color32::from_rgb(255, 60, 60),   // 2 — red
    Color32::from_rgb(70, 130, 255),  // 3 — blue
    Color32::from_rgb(255, 230, 0),   // 4 — yellow
    Color32::from_rgb(0, 230, 230),   // 5 — cyan
    Color32::from_rgb(255, 80, 255),  // 6 — magenta
    Color32::from_rgb(255, 255, 255), // 7 — white
    Color32::from_rgb(255, 150, 40),  // 8 — orange
];

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

/// Name of the bold font family (schematic labels and plot text are bold in
/// the reference; egui's `.strong()` only brightens the color).
pub const BOLD_FAMILY: &str = "bold";

/// A bold `FontId`. Requires [`install_fonts`] to have run on the context.
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD_FAMILY.into()))
}

/// Install the app fonts: DejaVu Sans first (it carries the technical
/// symbols the default font lacks), plus the bold family.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "technical_symbols".to_owned(),
        FontData::from_static(include_bytes!("../fonts/DejaVuSans.ttf")),
    );
    fonts.font_data.insert(
        "technical_bold".to_owned(),
        FontData::from_static(include_bytes!("../fonts/DejaVuSans-Bold.ttf")),
    );
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        if let Some(stack) = fonts.families.get_mut(&family) {
            stack.insert(0, "technical_symbols".to_owned());
        }
    }
    // Bold first, then the regular stack as glyph fallback.
    let mut bold_stack = vec!["technical_bold".to_owned()];
    if let Some(stack) = fonts.families.get(&FontFamily::Proportional) {
        bold_stack.extend(stack.iter().cloned());
    }
    fonts
        .families
        .insert(FontFamily::Name(BOLD_FAMILY.into()), bold_stack);
    ctx.set_fonts(fonts);
}

// ---------------------------------------------------------------------------
// Theme Application
// ---------------------------------------------------------------------------

pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::light();

    // Base fills
    visuals.panel_fill = BG_PANEL;
    visuals.window_fill = LT_WHITE;
    visuals.extreme_bg_color = BG_BASE;          // Text editors, input fields
    visuals.faint_bg_color = Color32::from_rgb(238, 238, 238);
    visuals.code_bg_color = Color32::from_rgb(235, 235, 235);

    // Structural chrome — flat, light shadows.
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    let shadow = egui::epaint::Shadow {
        offset: egui::vec2(1.0, 2.0),
        blur: 6.0,
        spread: 0.0,
        color: Color32::from_black_alpha(60),
    };
    visuals.window_shadow = shadow;
    visuals.popup_shadow = shadow;

    // Widget state machine.
    // noninteractive: labels, read-only fields
    visuals.widgets.noninteractive.bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.weak_bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.noninteractive.rounding = Rounding::same(0.0);

    // inactive: default clickable controls
    visuals.widgets.inactive.bg_fill = BG_CONTROL;
    visuals.widgets.inactive.weak_bg_fill = BG_CONTROL;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(150, 150, 150));
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.inactive.rounding = Rounding::same(1.0);

    // hovered
    visuals.widgets.hovered.bg_fill = BG_HOVER;
    visuals.widgets.hovered.weak_bg_fill = BG_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.hovered.rounding = Rounding::same(1.0);

    // active (pressed / toggled on)
    visuals.widgets.active.bg_fill = Color32::from_rgb(184, 217, 252);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(184, 217, 252);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.active.rounding = Rounding::same(1.0);

    // open (combo boxes, drop-downs)
    visuals.widgets.open.bg_fill = Color32::from_rgb(184, 217, 252);
    visuals.widgets.open.weak_bg_fill = Color32::from_rgb(184, 217, 252);
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.open.rounding = Rounding::same(1.0);

    // Selection / highlight
    visuals.selection.bg_fill = Color32::from_rgb(184, 217, 252);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT_DIM);

    // Semantic colors
    visuals.hyperlink_color = ACCENT_DIM;
    visuals.warn_fg_color = STATUS_WARN;
    visuals.error_fg_color = STATUS_ERROR;
    visuals.override_text_color = Some(TEXT_PRIMARY);

    // Geometry — square, like the reference's native widgets.
    visuals.window_rounding = Rounding::same(0.0);
    visuals.menu_rounding = Rounding::same(0.0);

    ctx.set_visuals(visuals);

    // -----------------------------------------------------------------------
    // Spacing & Typography
    // -----------------------------------------------------------------------
    let mut style = (*ctx.style()).clone();

    style.spacing.scroll.bar_width = 8.0;
    style.spacing.scroll.handle_min_length = 24.0;

    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.window_margin = egui::Margin::same(8.0);
    style.spacing.menu_margin = egui::Margin::same(2.0);
    style.spacing.indent = 14.0;
    style.spacing.interact_size.y = 20.0;

    // Font scale: small and data-dense like the reference.
    style.text_styles.insert(TextStyle::Heading, FontId::new(13.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Body, FontId::new(12.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace));
    style.text_styles.insert(TextStyle::Button, FontId::new(12.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Small, FontId::new(10.5, FontFamily::Proportional));

    ctx.set_style(style);
}

// ---------------------------------------------------------------------------
// Frame Constructors
// ---------------------------------------------------------------------------

/// Standard card — result tables and property panels (white on the gray workspace).
pub fn card_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(BG_SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .rounding(Rounding::same(0.0))
        .inner_margin(egui::Margin::same(10.0))
}

/// Flat bar (menu bar, toolbar, tab strip, status bar): fill + margin, no outline.
pub fn bar_frame(fill: Color32, margin: egui::Margin) -> egui::Frame {
    egui::Frame::none().fill(fill).inner_margin(margin)
}

/// Waveform viewer surface — black, edge to edge, like the reference.
pub fn code_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(LT_PLOT_BG)
        .stroke(Stroke::new(1.0, LT_PLOT_FRAME))
        .rounding(Rounding::same(0.0))
        .inner_margin(egui::Margin::same(0.0))
}

// ---------------------------------------------------------------------------
// Reusable UI Primitives
// ---------------------------------------------------------------------------

/// Section heading inside sidebars / inspector panels.
/// Renders as ALL-CAPS small label with a subtle separator line — matches
/// the visual language of instrument software (Keysight, NI LabVIEW panels).
pub fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .color(TEXT_SECONDARY)
            .strong()
            .size(10.0),
    );
    ui.add(egui::Separator::default().spacing(6.0));
}

/// Compact status chip — DRC pass/fail, simulation state, net health.
/// Uses a filled background (not just a border) for legibility at small sizes.
pub fn status_chip(ui: &mut egui::Ui, label: &str, color: Color32) {
    let bg = Color32::from_rgba_premultiplied(
        (color.r() as f32 * 0.15) as u8,
        (color.g() as f32 * 0.15) as u8,
        (color.b() as f32 * 0.15) as u8,
        220,
    );
    egui::Frame::none()
        .fill(bg)
        .stroke(Stroke::new(1.0, color))
        .rounding(Rounding::same(2.0))
        .inner_margin(egui::Margin::symmetric(6.0, 2.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label).color(color).size(10.5).strong());
        });
}

/// Monospaced label for node names, SPICE expressions, or coordinate readouts.
pub fn mono_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .color(TEXT_PRIMARY)
            .font(FontId::new(11.5, FontFamily::Monospace)),
    );
}

/// Muted annotation label — units, descriptions, helper text.
pub fn annotation(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .color(TEXT_SECONDARY)
            .size(11.0),
    );
}
