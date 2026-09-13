use eframe::egui;
use logview::app::LogViewApp;
use logview::fonts;

fn main() -> eframe::Result<()> {
    // 支持 `logview app.log` 直接打开
    let initial: Option<std::path::PathBuf> = std::env::args()
        .nth(1)
        .filter(|s| !s.starts_with('-'))
        .map(std::path::PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "logview",
        options,
        Box::new(move |cc| {
            fonts::setup(&cc.egui_ctx);
            let mut app = LogViewApp::new();
            if let Some(p) = initial {
                app.open_path(p);
            }
            Ok(Box::new(app))
        }),
    )
}
