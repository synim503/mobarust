// MobaRust — кроссплатформенный терминальный / SSH клиент.
// Точка входа: поднимает окно eframe и отдаёт управление MobaApp.

mod app;
mod config;
mod terminal;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([760.0, 480.0])
            .with_title("MobaRust"),
        ..Default::default()
    };

    eframe::run_native(
        "MobaRust",
        native_options,
        Box::new(|cc| Ok(Box::new(app::MobaApp::new(cc)))),
    )
}
