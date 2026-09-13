use eframe::egui;
use logview::app::LogViewApp;
use logview::fonts;

/// 内嵌窗口图标：128×128 的原始 RGBA 像素。
///
/// 直接存原始像素而不存 PNG，是为了避免引入 PNG 解码器——实测那会让
/// 二进制多出约 2 MB。128 尺寸足够覆盖标题栏、任务栏与高分屏。
/// 图标由 `assets/make_icon.py` 生成，要看效果打开 `assets/icon-128.png`。
const ICON_SIZE: u32 = 128;
const ICON_RGBA: &[u8] = include_bytes!("../assets/icon-128.rgba");

fn load_icon() -> egui::IconData {
    debug_assert_eq!(
        ICON_RGBA.len(),
        (ICON_SIZE * ICON_SIZE * 4) as usize,
        "图标数据尺寸与 ICON_SIZE 不符，重新生成 assets/icon-128.rgba"
    );
    egui::IconData {
        rgba: ICON_RGBA.to_vec(),
        width: ICON_SIZE,
        height: ICON_SIZE,
    }
}

fn main() -> eframe::Result<()> {
    // 支持 `logview app.log` 直接打开
    let initial: Option<std::path::PathBuf> = std::env::args()
        .nth(1)
        .filter(|s| !s.starts_with('-'))
        .map(std::path::PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(load_icon()),
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
