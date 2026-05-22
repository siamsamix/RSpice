mod app;
mod theme;

use app::CircuitSimApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("RSpice"),
        ..Default::default()
    };

    eframe::run_native(
        "RSpice",
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(CircuitSimApp::new(cc)))
        }),
    )
}
