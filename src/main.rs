// 双击运行的 exe 不该拖着一个控制台窗口。只在 Windows 的 release 构建上切换子系统；
// debug 构建保留控制台，`cargo run` 时还能直接看到 panic 与日志。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

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

/// 把 panic 信息弹成对话框。
///
/// release 构建跑在 Windows 图形子系统下，进程根本没有 stderr——不装这个钩子，
/// 任何 panic 都只会表现为「窗口一闪就没了」，连出错原因都拿不到。
/// 之所以不只在开发时用：用户双击运行的就是 release 版。
#[cfg(not(debug_assertions))]
fn install_panic_dialog() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 保留默认行为：从终端启动时（或输出被重定向时）日志仍能落到 stderr
        previous(info);

        let mut msg = String::from("logview 遇到未预期的错误，已停止运行。\n");
        if let Some(loc) = info.location() {
            msg.push_str(&format!(
                "\n位置：{}:{}:{}\n",
                loc.file(),
                loc.line(),
                loc.column()
            ));
        }
        let payload = info.payload();
        if let Some(s) = payload.downcast_ref::<&str>() {
            msg.push_str(&format!("\n{s}"));
        } else if let Some(s) = payload.downcast_ref::<String>() {
            msg.push_str(&format!("\n{s}"));
        }

        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("logview")
            .set_description(msg)
            .show();
    }));
}

/// debug 构建保留控制台，panic 直接看终端输出即可
#[cfg(debug_assertions)]
fn install_panic_dialog() {}

fn main() -> eframe::Result<()> {
    install_panic_dialog();

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
