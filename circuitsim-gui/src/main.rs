mod app;
mod theme;

use app::CircuitSimApp;
#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(target_arch = "wasm32")]
fn main() {
    use eframe::wasm_bindgen::JsCast as _;

    // Redirect tracing/logging to the browser console
    eframe::WebLogger::init(log::LevelFilter::Debug).ok();

    // 1. WebOptions stays clean with default options
    let web_options = eframe::WebOptions::default();

    wasm_bindgen_futures::spawn_local(async {
        let document = eframe::web_sys::window()
        .expect("No window")
        .document()
        .expect("No document");

        let canvas = document
        .get_element_by_id("main_canvas")
        .expect("Failed to find main_canvas ID")
        .dyn_into::<eframe::web_sys::HtmlCanvasElement>()
        .expect("main_canvas was not an HtmlCanvasElement");

        eframe::WebRunner::new()
        .start(
            canvas,
            web_options,
            Box::new(|cc| {
                // 2. Set the theme preference directly in egui's global options
                cc.egui_ctx.options_mut(|options| {
                    // Forcing Dark mode instructs egui to ignore browser system preferences
                    options.theme_preference = eframe::egui::ThemePreference::Dark;

                    // Note: If your custom theme is a Light theme, use this instead:
                    // options.theme_preference = eframe::egui::ThemePreference::Light;
                });

                // 3. Apply your custom theme adjustments
                theme::apply(&cc.egui_ctx);

                Ok(Box::new(CircuitSimApp::new(cc)))
            }),
        )
        .await
        .expect("failed to start eframe");
    });
}
