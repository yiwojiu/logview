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

/// 当前是不是远程桌面会话。
///
/// 远程会话里的显示驱动通常只提供 OpenGL 1.1，而默认的 glow 后端需要更高版本——
/// 把这条信息带进报错里能省掉一轮猜测。读环境变量即可，不必为此引入 windows 依赖。
fn session_kind() -> &'static str {
    session_kind_from(std::env::var("SESSIONNAME").ok().as_deref())
}

fn session_kind_from(name: Option<&str>) -> &'static str {
    match name {
        Some(s) if s.starts_with("RDP") => "远程桌面会话",
        Some(_) => "本地会话",
        None => "未知",
    }
}

/// 启动失败时把原因摆到用户面前。
///
/// release 跑在 Windows 图形子系统下，进程没有 stderr：`run_native` 返回 Err
/// （最典型的是创建 OpenGL 上下文失败——远程桌面、虚拟机或没装显卡驱动的机器上
/// 可能只有 OpenGL 1.1）时，双击运行的表现就是「什么都没发生」，连原因都拿不到。
/// panic 有钩子兜着，`Result::Err` 之前没有，这里补上。
fn report_startup_failure(err: &eframe::Error) {
    let detail = format!(
        "logview 无法创建窗口，已退出。\n\n\
         原因：{err}\n\n\
         运行环境：{}\n\n\
         若提示与图形加速有关，可以试：\n\
         · 在本机登录（而不是远程桌面）后运行\n\
         · 安装或更新显卡驱动",
        session_kind()
    );

    eprintln!("{detail}");

    #[cfg(not(debug_assertions))]
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("logview 启动失败")
        .set_description(&detail)
        .show();
}

/// 渲染后端：Windows 走 D3D12，其余平台走 OpenGL。理由见 `Cargo.toml`。
///
/// 关键在于远程桌面会话只提供 OpenGL 1.1，glow 起不来窗口——而"在服务器上看日志"
/// 正是这类工具最常见的用法之一。
#[cfg(windows)]
fn preferred_renderer() -> eframe::Renderer {
    eframe::Renderer::Wgpu
}

#[cfg(not(windows))]
fn preferred_renderer() -> eframe::Renderer {
    eframe::Renderer::Glow
}

fn main() -> std::process::ExitCode {
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
        // 走 D3D12 而不是 OpenGL：远程桌面、虚拟机与无显卡驱动的机器上
        // OpenGL 往往只有 1.1，起不来窗口（详见 Cargo.toml 里的说明）。
        renderer: preferred_renderer(),
        ..Default::default()
    };

    let result = eframe::run_native(
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
    );

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            report_startup_failure(&err);
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 报错里要能区分远程会话——那是最常见的"OpenGL 只有 1.1"场景
    #[test]
    fn detects_remote_sessions() {
        assert_eq!(session_kind_from(Some("RDP-Tcp#12")), "远程桌面会话");
        assert_eq!(session_kind_from(Some("Console")), "本地会话");
        assert_eq!(session_kind_from(None), "未知");
    }
}
