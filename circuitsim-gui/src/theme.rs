use egui::{Color32, FontFamily, FontId, Rounding, Stroke, TextStyle, Visuals};

pub const ACCENT: Color32 = Color32::from_rgb(56, 189, 248);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(14, 116, 144);
pub const SURFACE: Color32 = Color32::from_rgb(15, 23, 42);
pub const SURFACE_ELEVATED: Color32 = Color32::from_rgb(30, 41, 59);
pub const BORDER: Color32 = Color32::from_rgb(51, 65, 85);
pub const TEXT: Color32 = Color32::from_rgb(226, 232, 240);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(148, 163, 184);
pub const SUCCESS: Color32 = Color32::from_rgb(52, 211, 153);
pub const ERROR: Color32 = Color32::from_rgb(248, 113, 113);

pub const PLOT_COLORS: [Color32; 8] = [
    Color32::from_rgb(56, 189, 248),
    Color32::from_rgb(251, 191, 36),
    Color32::from_rgb(167, 139, 250),
    Color32::from_rgb(52, 211, 153),
    Color32::from_rgb(244, 114, 182),
    Color32::from_rgb(251, 146, 60),
    Color32::from_rgb(94, 234, 212),
    Color32::from_rgb(248, 113, 113),
];

pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = SURFACE;
    visuals.window_fill = SURFACE_ELEVATED;
    visuals.extreme_bg_color = Color32::from_rgb(2, 6, 23);
    visuals.faint_bg_color = Color32::from_rgb(30, 41, 59);
    visuals.code_bg_color = Color32::from_rgb(15, 23, 42);
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.bg_fill = SURFACE_ELEVATED;
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(51, 65, 85);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(71, 85, 105);
    visuals.widgets.active.bg_fill = ACCENT_DIM;
    visuals.widgets.open.bg_fill = ACCENT_DIM;
    visuals.selection.bg_fill = Color32::from_rgba_premultiplied(56, 189, 248, 60);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = Color32::from_rgb(251, 191, 36);
    visuals.error_fg_color = ERROR;
    visuals.override_text_color = Some(TEXT);
    visuals.window_rounding = Rounding::same(10.0);

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.window_margin = egui::Margin::same(14.0);
    style.spacing.indent = 18.0;

    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(22.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(14.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(13.0, FontFamily::Monospace),
    );
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(14.0, FontFamily::Proportional),
    );

    ctx.set_style(style);
}

pub fn card_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(SURFACE_ELEVATED)
        .stroke(Stroke::new(1.0, BORDER))
        .rounding(Rounding::same(10.0))
        .inner_margin(egui::Margin::same(14.0))
}

pub fn panel_frame(fill: Color32) -> egui::Frame {
    egui::Frame::none()
        .fill(fill)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(egui::Margin::symmetric(16.0, 10.0))
}

pub fn editor_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(Color32::from_rgb(2, 6, 23))
        .stroke(Stroke::new(1.0, BORDER))
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::same(10.0))
}

pub fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .color(ACCENT)
            .strong()
            .size(15.0),
    );
    ui.add_space(4.0);
}

pub fn status_chip(ui: &mut egui::Ui, label: &str, color: Color32) {
    egui::Frame::none()
        .fill(color.gamma_multiply(0.15))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.5)))
        .rounding(Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(10.0, 4.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label).color(color).size(12.0));
        });
}
