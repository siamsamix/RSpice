use egui::{Color32, FontFamily, FontId, Rounding, Stroke, TextStyle, Visuals};

// A crisp, engineering blue for selections and active elements
pub const ACCENT: Color32 = Color32::from_rgb(0, 122, 204);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0, 89, 153);

// Neutral, utilitarian dark greys (removes the web-app slate/blue tint)
pub const SURFACE: Color32 = Color32::from_rgb(37, 37, 38);
pub const SURFACE_ELEVATED: Color32 = Color32::from_rgb(45, 45, 48);
pub const BORDER: Color32 = Color32::from_rgb(64, 64, 64);
pub const TEXT: Color32 = Color32::from_rgb(230, 230, 230);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(153, 153, 153);

// Standardized status colors
pub const SUCCESS: Color32 = Color32::from_rgb(76, 175, 80);
pub const ERROR: Color32 = Color32::from_rgb(244, 67, 54);

// High-contrast, "Oscilloscope" style trace colors for waveforms and schematics
pub const PLOT_COLORS: [Color32; 8] = [
    Color32::from_rgb(57, 255, 20),   // Trace 1: Neon Green
    Color32::from_rgb(255, 250, 0),   // Trace 2: Bright Yellow
    Color32::from_rgb(0, 255, 255),   // Trace 3: Cyan
    Color32::from_rgb(255, 80, 255),  // Trace 4: Magenta
    Color32::from_rgb(255, 80, 80),   // Trace 5: Bright Red
    Color32::from_rgb(100, 180, 255), // Trace 6: Light Blue
    Color32::from_rgb(255, 165, 0),   // Trace 7: Orange
    Color32::from_rgb(240, 240, 240), // Trace 8: White
];

pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = SURFACE;
    visuals.window_fill = SURFACE_ELEVATED;
    visuals.extreme_bg_color = Color32::from_rgb(18, 18, 18); // Darker for high contrast plots
    visuals.faint_bg_color = Color32::from_rgb(45, 45, 48);
    visuals.code_bg_color = Color32::from_rgb(30, 30, 30);

    // Sharper borders for a technical feel
    visuals.window_stroke = Stroke::new(1.0, BORDER);

    // Widget states
    visuals.widgets.noninteractive.bg_fill = SURFACE_ELEVATED;
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(60, 60, 60);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(80, 80, 80);
    visuals.widgets.active.bg_fill = ACCENT_DIM;
    visuals.widgets.open.bg_fill = ACCENT_DIM;

    // Selections and highlights
    visuals.selection.bg_fill = Color32::from_rgba_premultiplied(0, 122, 204, 80);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = Color32::from_rgb(255, 193, 7);
    visuals.error_fg_color = ERROR;
    visuals.override_text_color = Some(TEXT);

    // Crisp, small rounding instead of bubbly web-app rounding
    visuals.window_rounding = Rounding::same(2.0);

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();

    // Tighter spacing to maximize schematic/graph real estate
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 4.0);
    style.spacing.window_margin = egui::Margin::same(8.0);
    style.spacing.indent = 16.0;

    // Slightly smaller fonts to pack more data into property inspectors/netlists
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(18.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.0, FontFamily::Monospace),
    );
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(13.0, FontFamily::Proportional),
    );

    ctx.set_style(style);
}

pub fn card_frame() -> egui::Frame {
    egui::Frame::none()
    .fill(SURFACE_ELEVATED)
    .stroke(Stroke::new(1.0, BORDER))
    .rounding(Rounding::same(2.0))
    .inner_margin(egui::Margin::same(10.0))
}

pub fn panel_frame(fill: Color32) -> egui::Frame {
    egui::Frame::none()
    .fill(fill)
    .stroke(Stroke::new(1.0, BORDER))
    .inner_margin(egui::Margin::symmetric(12.0, 8.0))
}

pub fn editor_frame() -> egui::Frame {
    // Near-black background to act as the schematic or waveform canvas
    egui::Frame::none()
    .fill(Color32::from_rgb(18, 18, 18))
    .stroke(Stroke::new(1.0, Color32::from_rgb(80, 80, 80)))
    .rounding(Rounding::same(0.0)) // Perfectly sharp corners for the main canvas
    .inner_margin(egui::Margin::same(8.0))
}

pub fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
        .color(TEXT) // White instead of blue for headings to look more like desktop software
        .strong()
        .size(14.0),
    );
    ui.add_space(4.0);
}

pub fn status_chip(ui: &mut egui::Ui, label: &str, color: Color32) {
    // A more structural tag, useful for components status or DRC checks
    egui::Frame::none()
    .fill(Color32::TRANSPARENT)
    .stroke(Stroke::new(1.0, color))
    .rounding(Rounding::same(2.0))
    .inner_margin(egui::Margin::symmetric(6.0, 2.0))
    .show(ui, |ui| {
        ui.label(egui::RichText::new(label).color(color).size(11.0));
    });
}
