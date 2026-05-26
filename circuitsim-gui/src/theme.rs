use egui::{Color32, FontFamily, FontId, Rounding, Stroke, TextStyle, Visuals};

// ---------------------------------------------------------------------------
// Color Palette — "Instrument Panel" aesthetic
// Modeled after high-end test & measurement equipment (Tektronix, Keysight).
// Pure neutral grays, no blue/warm tint. Steel-blue accent only for active
// interactive states. Everything else reads as hardware, not a web app.
// ---------------------------------------------------------------------------

/// Primary interactive accent — muted steel blue, not eye-wateringly bright.
pub const ACCENT: Color32 = Color32::from_rgb(41, 128, 185);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(28, 93, 138);
pub const ACCENT_SUBTLE: Color32 = Color32::from_rgba_premultiplied(41, 128, 185, 60);

/// Surface hierarchy — pure neutral, zero chromatic bias.
pub const BG_BASE: Color32 = Color32::from_rgb(18, 18, 20);       // Main window / canvas
pub const BG_PANEL: Color32 = Color32::from_rgb(28, 28, 30);      // Sidebars, toolbars
pub const BG_SURFACE: Color32 = Color32::from_rgb(38, 38, 42);    // Cards, elevated panels
pub const BG_CONTROL: Color32 = Color32::from_rgb(52, 52, 56);    // Input fields, inactive buttons
pub const BG_HOVER: Color32 = Color32::from_rgb(66, 66, 72);      // Hovered controls

/// Borders — two levels: structural and subtle.
pub const BORDER: Color32 = Color32::from_rgb(60, 60, 66);
pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(44, 44, 48);

/// Text — high contrast primary, two muted levels for hierarchy.
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(220, 220, 224);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(148, 148, 158);
pub const TEXT_DISABLED: Color32 = Color32::from_rgb(88, 88, 96);

/// Status — standard engineering traffic-light palette.
pub const STATUS_OK: Color32 = Color32::from_rgb(39, 174, 96);
pub const STATUS_WARN: Color32 = Color32::from_rgb(230, 152, 0);
pub const STATUS_ERROR: Color32 = Color32::from_rgb(192, 57, 43);
pub const STATUS_INFO: Color32 = Color32::from_rgb(41, 128, 185);

// ---------------------------------------------------------------------------
// Waveform / Schematic Trace Colors
// Ordered to match Tektronix channel conventions (Ch1=Yellow, Ch2=Cyan, …).
// Saturation kept high but luminance capped so they don't blow out on dark BG.
// ---------------------------------------------------------------------------
pub const PLOT_COLORS: [Color32; 8] = [
    Color32::from_rgb(255, 220, 0),   // Ch 1 — Scope Yellow      (V_out, primary node)
    Color32::from_rgb(0, 220, 220),   // Ch 2 — Cyan              (I_source, secondary)
    Color32::from_rgb(255, 100, 50),  // Ch 3 — Burnt Orange      (V_in)
    Color32::from_rgb(160, 110, 255), // Ch 4 — Soft Violet       (V_ref)
    Color32::from_rgb(80, 200, 120),  // Ch 5 — Instrument Green  (V_dd)
    Color32::from_rgb(80, 160, 255),  // Ch 6 — Sky Blue          (V_gnd probe)
    Color32::from_rgb(255, 255, 255), // Ch 7 — White             (math channel / derived)
    Color32::from_rgb(200, 60, 90),   // Ch 8 — Crimson           (error / DRC flag)
];

// ---------------------------------------------------------------------------
// Theme Application
// ---------------------------------------------------------------------------

pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();

    // Base fills
    visuals.panel_fill = BG_PANEL;
    visuals.window_fill = BG_SURFACE;
    visuals.extreme_bg_color = BG_BASE;          // Plot canvas, code editors
    visuals.faint_bg_color = BG_PANEL;
    visuals.code_bg_color = Color32::from_rgb(24, 24, 26);

    // Structural chrome
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 12.0,
        spread: 0.0,
        color: Color32::from_black_alpha(120),
    };

    // Widget state machine — five states, each a deliberate step.
    // noninteractive: labels, read-only fields
    visuals.widgets.noninteractive.bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
    visuals.widgets.noninteractive.rounding = Rounding::same(2.0);

    // inactive: default clickable controls
    visuals.widgets.inactive.bg_fill = BG_CONTROL;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.inactive.rounding = Rounding::same(2.0);

    // hovered
    visuals.widgets.hovered.bg_fill = BG_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(88, 88, 96));
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, TEXT_PRIMARY);
    visuals.widgets.hovered.rounding = Rounding::same(2.0);

    // active (pressed / toggled on)
    visuals.widgets.active.bg_fill = ACCENT_DIM;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    visuals.widgets.active.rounding = Rounding::same(2.0);

    // open (combo boxes, drop-downs)
    visuals.widgets.open.bg_fill = ACCENT_DIM;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.open.rounding = Rounding::same(2.0);

    // Selection / highlight
    visuals.selection.bg_fill = ACCENT_SUBTLE;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);

    // Semantic colors
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = STATUS_WARN;
    visuals.error_fg_color = STATUS_ERROR;
    visuals.override_text_color = Some(TEXT_PRIMARY);

    // Geometry — flat, precise, instrument-like. No bubbly corners anywhere.
    visuals.window_rounding = Rounding::same(3.0);
    visuals.menu_rounding = Rounding::same(3.0);

    ctx.set_visuals(visuals);

    // -----------------------------------------------------------------------
    // Spacing & Typography
    // -----------------------------------------------------------------------
    let mut style = (*ctx.style()).clone();

    // Scrollbars — thin, unobtrusive (field lives on Spacing in egui ≥0.27)
    style.spacing.scroll.bar_width = 6.0;
    style.spacing.scroll.handle_min_length = 24.0;

    // Compact but breathable — maximises schematic/waveform real estate.
    style.spacing.item_spacing = egui::vec2(6.0, 5.0);
    style.spacing.button_padding = egui::vec2(10.0, 4.0);
    style.spacing.window_margin = egui::Margin::same(10.0);
    style.spacing.menu_margin = egui::Margin::same(4.0);
    style.spacing.indent = 14.0;
    style.spacing.interact_size.y = 22.0;   // Tighter row height for property lists

    // Font scale: readable but data-dense.
    // Heading: section labels in sidebars / inspector panels.
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(13.0, FontFamily::Proportional),
    );
    // Body: property values, netlists, log output.
    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(12.0, FontFamily::Proportional),
    );
    // Monospace: SPICE netlists, expression editor, node names.
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(11.0, FontFamily::Monospace),
    );
    // Button: toolbar actions, dialog buttons.
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(12.0, FontFamily::Proportional),
    );
    // Small: status bar, coordinate readout, zoom level.
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(10.5, FontFamily::Proportional),
    );

    ctx.set_style(style);
}

// ---------------------------------------------------------------------------
// Frame Constructors
// ---------------------------------------------------------------------------

/// Standard elevated card — property inspector, component detail panels.
pub fn card_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(BG_SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .rounding(Rounding::same(3.0))
        .inner_margin(egui::Margin::same(10.0))
}

/// Sidebar / toolbar panel. Caller provides the fill from the surface hierarchy.
pub fn panel_frame(fill: Color32) -> egui::Frame {
    egui::Frame::none()
        .fill(fill)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(egui::Margin::symmetric(10.0, 7.0))
}

/// Main schematic / waveform canvas — near-black, zero rounding, sharp boundary.
pub fn editor_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(BG_BASE)
        .stroke(Stroke::new(1.0, BORDER))
        .rounding(Rounding::same(0.0))
        .inner_margin(egui::Margin::same(0.0))
}

/// Inset panel for SPICE netlist / expression editors.
pub fn code_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(Color32::from_rgb(24, 24, 26))
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .rounding(Rounding::same(2.0))
        .inner_margin(egui::Margin::same(8.0))
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
