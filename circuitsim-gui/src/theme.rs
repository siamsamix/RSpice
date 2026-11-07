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
pub const LT_STRIP: Color32 = Color32::from_rgb(246, 247, 249);
/// MDI workspace and the schematic canvas.
pub const LT_WORKSPACE: Color32 = Color32::from_rgb(232, 235, 240);
/// Workspace / child-window border.
pub const LT_FRAME: Color32 = Color32::from_rgb(196, 202, 212);
/// Title bar of the active child window.
pub const LT_TITLE_ACTIVE: Color32 = Color32::from_rgb(37, 99, 235);
/// Title bar of an inactive child window.
pub const LT_TITLE_INACTIVE: Color32 = Color32::from_rgb(238, 240, 244);
/// Title text of an inactive child window.
pub const LT_TITLE_INACTIVE_TEXT: Color32 = Color32::from_rgb(120, 128, 142);
/// Title bar fill of the active child window (soft blue tint; the strong
/// blue is drawn as an underline).
pub const TITLE_ACTIVE_BG: Color32 = Color32::from_rgb(229, 238, 255);
/// Waveform viewer background.
pub const LT_PLOT_BG: Color32 = Color32::from_rgb(13, 15, 19);
/// Waveform viewer frame, tick marks and axis text.
pub const LT_PLOT_FRAME: Color32 = Color32::from_rgb(150, 158, 172);
/// Schematic wires and symbols.
pub const LT_WIRE: Color32 = Color32::from_rgb(29, 78, 216);
/// Schematic text (names, values, directives).
pub const LT_SCHEM_TEXT: Color32 = Color32::from_rgb(24, 28, 36);

/// Primary interactive accent — the active-title blue.
pub const ACCENT: Color32 = LT_TITLE_ACTIVE;
/// Corner radius of buttons and inputs.
pub const RADIUS: f32 = 5.0;
/// Selection / pressed fill.
pub const SELECT_FILL: Color32 = Color32::from_rgb(191, 214, 253);

pub const ACCENT_DIM: Color32 = Color32::from_rgb(29, 78, 216);

/// Surface hierarchy.
pub const BG_BASE: Color32 = LT_WHITE;                            // Text editors, input fields
pub const BG_PANEL: Color32 = LT_STRIP;                           // Bars, panels
pub const BG_SURFACE: Color32 = LT_WHITE;                         // Cards
pub const BG_CONTROL: Color32 = Color32::from_rgb(238, 240, 244); // Buttons
pub const BG_HOVER: Color32 = Color32::from_rgb(219, 230, 252);   // Hovered controls

/// Borders — structural and subtle.
pub const BORDER: Color32 = LT_FRAME;
pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(220, 224, 231);

/// Text — black primary, two muted levels.
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(24, 28, 36);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(88, 96, 110);
pub const TEXT_DISABLED: Color32 = Color32::from_rgb(158, 165, 177);

/// Status — readable on the light chrome.
pub const STATUS_OK: Color32 = Color32::from_rgb(22, 140, 72);
pub const STATUS_WARN: Color32 = Color32::from_rgb(180, 110, 0);
pub const STATUS_ERROR: Color32 = Color32::from_rgb(190, 30, 30);
pub const STATUS_INFO: Color32 = Color32::from_rgb(37, 99, 235);

// ---------------------------------------------------------------------------
// Waveform Trace Colors — bright on the black viewer. The first trace is
// LTspice's green (sampled from the reference); the rest are distinct,
// high-luminance colors in a fixed order.
// ---------------------------------------------------------------------------
pub const PLOT_COLORS: [Color32; 8] = [
    Color32::from_rgb(52, 232, 112),  // 1 — green (LTspice-style first trace)
    Color32::from_rgb(255, 99, 99),   // 2 — red
    Color32::from_rgb(96, 150, 255),  // 3 — blue
    Color32::from_rgb(255, 214, 64),  // 4 — yellow
    Color32::from_rgb(45, 212, 224),  // 5 — cyan
    Color32::from_rgb(240, 110, 240), // 6 — magenta
    Color32::from_rgb(240, 242, 245), // 7 — white
    Color32::from_rgb(255, 159, 67),  // 8 — orange
];

// ---------------------------------------------------------------------------
// Theme selection.
//
// Two looks share one app: `Modern` (the constants above, unchanged) and
// `Classic` (pixel-sampled from the LTspice reference screenshots in `docs/`).
// Code that must differ per theme asks [`pal`] instead of naming a constant;
// the Modern palette is built from the very same constants, so nothing drawn
// in Modern changes.
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeKind {
    /// Soft, rounded light theme.
    Modern,
    /// Flat gray LTspice look: square corners, gray workspace, black viewer.
    Classic,
}

static THEME_KIND: AtomicU8 = AtomicU8::new(1);

pub fn kind() -> ThemeKind {
    if THEME_KIND.load(Ordering::Relaxed) == 0 { ThemeKind::Modern } else { ThemeKind::Classic }
}

pub fn set_kind(kind: ThemeKind) {
    THEME_KIND.store(matches!(kind, ThemeKind::Classic) as u8, Ordering::Relaxed);
}

pub fn is_classic() -> bool {
    kind() == ThemeKind::Classic
}

/// The theme named by the `RSPICE_THEME` environment variable
/// (`modern` / `classic`), if any.
pub fn kind_from_env() -> Option<ThemeKind> {
    match std::env::var("RSPICE_THEME").ok()?.to_ascii_lowercase().as_str() {
        "modern" => Some(ThemeKind::Modern),
        "classic" | "ltspice" => Some(ThemeKind::Classic),
        _ => None,
    }
}

/// Every colour whose value differs between the two themes.
#[derive(Clone, Copy)]
pub struct Palette {
    pub white: Color32,
    pub strip: Color32,
    pub workspace: Color32,
    pub frame: Color32,
    pub title_active: Color32,
    pub title_active_bg: Color32,
    pub title_active_text: Color32,
    pub title_inactive: Color32,
    pub title_inactive_text: Color32,
    pub plot_bg: Color32,
    pub plot_frame: Color32,
    pub wire: Color32,
    pub schem_text: Color32,
    pub accent: Color32,
    pub accent_dim: Color32,
    pub bg_hover: Color32,
    pub border_subtle: Color32,
    pub select_fill: Color32,
    pub tab_idle: Color32,
}

impl Palette {
    pub const MODERN: Palette = Palette {
        white: LT_WHITE,
        strip: LT_STRIP,
        workspace: LT_WORKSPACE,
        frame: LT_FRAME,
        title_active: LT_TITLE_ACTIVE,
        title_active_bg: TITLE_ACTIVE_BG,
        title_active_text: TEXT_PRIMARY,
        title_inactive: LT_TITLE_INACTIVE,
        title_inactive_text: LT_TITLE_INACTIVE_TEXT,
        plot_bg: LT_PLOT_BG,
        plot_frame: LT_PLOT_FRAME,
        wire: LT_WIRE,
        schem_text: LT_SCHEM_TEXT,
        accent: ACCENT,
        accent_dim: ACCENT_DIM,
        bg_hover: BG_HOVER,
        border_subtle: BORDER_SUBTLE,
        select_fill: SELECT_FILL,
        tab_idle: Color32::from_rgb(236, 239, 244),
    };

    /// Sampled from `docs/ltspice-window*.png`.
    pub const CLASSIC: Palette = Palette {
        white: Color32::from_rgb(255, 255, 255),
        strip: Color32::from_rgb(245, 245, 245),
        workspace: Color32::from_rgb(192, 192, 192),
        frame: Color32::from_rgb(106, 106, 106),
        title_active: Color32::from_rgb(50, 150, 250),
        title_active_bg: Color32::from_rgb(50, 150, 250),
        title_active_text: Color32::BLACK,
        title_inactive: Color32::from_rgb(128, 128, 128),
        title_inactive_text: Color32::from_rgb(200, 200, 200),
        plot_bg: Color32::BLACK,
        plot_frame: Color32::from_rgb(192, 192, 192),
        wire: Color32::from_rgb(0, 0, 255),
        schem_text: Color32::BLACK,
        accent: Color32::from_rgb(50, 150, 250),
        accent_dim: Color32::from_rgb(0, 0, 200),
        bg_hover: Color32::from_rgb(227, 227, 227),
        border_subtle: Color32::from_rgb(174, 174, 174),
        select_fill: Color32::from_rgb(166, 202, 240),
        tab_idle: Color32::from_rgb(236, 236, 236),
    };
}

/// The active theme's palette.
pub fn pal() -> Palette {
    match kind() {
        ThemeKind::Modern => Palette::MODERN,
        ThemeKind::Classic => Palette::CLASSIC,
    }
}

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
    visuals.faint_bg_color = Color32::from_rgb(241, 243, 246);
    visuals.code_bg_color = Color32::from_rgb(236, 239, 244);

    // Structural chrome — flat, light shadows.
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    let shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 16.0,
        spread: 0.0,
        color: Color32::from_black_alpha(40),
    };
    visuals.window_shadow = shadow;
    visuals.popup_shadow = shadow;

    // Widget state machine.
    // noninteractive: labels, read-only fields
    visuals.widgets.noninteractive.bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.weak_bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.noninteractive.rounding = Rounding::same(RADIUS);

    // inactive: default clickable controls
    visuals.widgets.inactive.bg_fill = BG_CONTROL;
    visuals.widgets.inactive.weak_bg_fill = BG_CONTROL;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(205, 210, 219));
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.inactive.rounding = Rounding::same(RADIUS);

    // hovered
    visuals.widgets.hovered.bg_fill = BG_HOVER;
    visuals.widgets.hovered.weak_bg_fill = BG_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.hovered.rounding = Rounding::same(RADIUS);

    // active (pressed / toggled on)
    visuals.widgets.active.bg_fill = SELECT_FILL;
    visuals.widgets.active.weak_bg_fill = SELECT_FILL;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.active.rounding = Rounding::same(RADIUS);

    // open (combo boxes, drop-downs)
    visuals.widgets.open.bg_fill = SELECT_FILL;
    visuals.widgets.open.weak_bg_fill = SELECT_FILL;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.open.rounding = Rounding::same(RADIUS);

    // Selection / highlight
    visuals.selection.bg_fill = SELECT_FILL;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT_DIM);

    // Semantic colors
    visuals.hyperlink_color = ACCENT_DIM;
    visuals.warn_fg_color = STATUS_WARN;
    visuals.error_fg_color = STATUS_ERROR;
    visuals.override_text_color = Some(TEXT_PRIMARY);

    // Geometry — softly rounded controls and floating surfaces; the MDI
    // child windows stay square so their bodies clip cleanly.
    visuals.window_rounding = Rounding::same(10.0);
    visuals.menu_rounding = Rounding::same(8.0);

    ctx.set_visuals(visuals);

    // -----------------------------------------------------------------------
    // Spacing & Typography
    // -----------------------------------------------------------------------
    let mut style = (*ctx.style()).clone();

    style.spacing.scroll.bar_width = 9.0;
    style.spacing.scroll.handle_min_length = 24.0;

    style.spacing.item_spacing = egui::vec2(7.0, 4.0);
    style.spacing.button_padding = egui::vec2(10.0, 3.0);
    style.spacing.window_margin = egui::Margin::same(10.0);
    style.spacing.menu_margin = egui::Margin::same(4.0);
    style.spacing.indent = 14.0;
    style.spacing.interact_size.y = 22.0;

    // Font scale: small and data-dense like the reference.
    style.text_styles.insert(TextStyle::Heading, FontId::new(13.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Body, FontId::new(12.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace));
    style.text_styles.insert(TextStyle::Button, FontId::new(12.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Small, FontId::new(10.5, FontFamily::Proportional));

    ctx.set_style(style);
}

/// Select and install a theme on `ctx`. Modern goes through [`apply`]
/// untouched.
pub fn apply_kind(ctx: &egui::Context, kind: ThemeKind) {
    set_kind(kind);
    match kind {
        ThemeKind::Modern => apply(ctx),
        ThemeKind::Classic => apply_classic(ctx),
    }
}

/// The Classic (LTspice / classic Windows) look: flat, square, no shadows,
/// gray dialogs, black text.
pub fn apply_classic(ctx: &egui::Context) {
    let p = Palette::CLASSIC;
    let face = Color32::from_rgb(240, 240, 240);
    let edge = Color32::from_rgb(173, 173, 173);
    let mut visuals = Visuals::light();

    visuals.panel_fill = face;
    visuals.window_fill = face;
    visuals.extreme_bg_color = Color32::WHITE;
    visuals.faint_bg_color = Color32::from_rgb(245, 245, 245);
    visuals.code_bg_color = Color32::from_rgb(235, 235, 235);
    visuals.window_stroke = Stroke::new(1.0, Color32::from_rgb(100, 100, 100));
    visuals.window_shadow = egui::epaint::Shadow::NONE;
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: egui::vec2(2.0, 2.0),
        blur: 0.0,
        spread: 0.0,
        color: Color32::from_black_alpha(60),
    };

    let flat = Rounding::ZERO;
    visuals.widgets.noninteractive.bg_fill = face;
    visuals.widgets.noninteractive.weak_bg_fill = face;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, edge);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::BLACK);
    visuals.widgets.noninteractive.rounding = flat;

    visuals.widgets.inactive.bg_fill = Color32::from_rgb(225, 225, 225);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(225, 225, 225);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, edge);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::BLACK);
    visuals.widgets.inactive.rounding = flat;

    visuals.widgets.hovered.bg_fill = Color32::from_rgb(229, 241, 251);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(229, 241, 251);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0, 120, 215));
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::BLACK);
    visuals.widgets.hovered.rounding = flat;

    visuals.widgets.active.bg_fill = Color32::from_rgb(204, 228, 247);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(204, 228, 247);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0, 84, 153));
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::BLACK);
    visuals.widgets.active.rounding = flat;

    visuals.widgets.open.bg_fill = Color32::from_rgb(204, 228, 247);
    visuals.widgets.open.weak_bg_fill = Color32::from_rgb(204, 228, 247);
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0, 84, 153));
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, Color32::BLACK);
    visuals.widgets.open.rounding = flat;

    visuals.selection.bg_fill = p.select_fill;
    visuals.selection.stroke = Stroke::new(1.0, Color32::from_rgb(0, 84, 153));
    visuals.hyperlink_color = p.accent_dim;
    visuals.warn_fg_color = STATUS_WARN;
    visuals.error_fg_color = Color32::from_rgb(200, 0, 0);
    visuals.override_text_color = Some(Color32::BLACK);
    visuals.window_rounding = flat;
    visuals.menu_rounding = flat;

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.scroll.bar_width = 12.0;
    style.spacing.item_spacing = egui::vec2(6.0, 3.0);
    style.spacing.button_padding = egui::vec2(8.0, 2.0);
    style.spacing.window_margin = egui::Margin::same(8.0);
    style.spacing.menu_margin = egui::Margin::same(2.0);
    style.spacing.indent = 14.0;
    style.spacing.interact_size.y = 20.0;
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
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::same(12.0))
}

/// Flat bar (menu bar, toolbar, tab strip, status bar): fill + margin, no outline.
pub fn bar_frame(fill: Color32, margin: egui::Margin) -> egui::Frame {
    egui::Frame::none().fill(fill).inner_margin(margin)
}

/// Waveform viewer surface — black, edge to edge, like the reference.
pub fn code_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(pal().plot_bg)
        .stroke(Stroke::new(1.0, pal().plot_frame))
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
        .rounding(Rounding::same(10.0))
        .inner_margin(egui::Margin::symmetric(8.0, 2.0))
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
