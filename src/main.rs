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

/// 覆盖渲染后端的环境变量，取值 `wgpu` 或 `glow`（不区分大小写）
const ENV_RENDERER: &str = "LOGVIEW_RENDERER";

/// wgpu 自己的环境变量，用来限定它用哪个子后端（`gl` / `vulkan` / `dx12`…）。
/// 我们只是把它回显到启动日志里，见 `main`。
const ENV_WGPU_BACKEND: &str = "WGPU_BACKEND";

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
/// （最典型的是找不到可用的图形适配器——远程桌面、虚拟机或没装显卡驱动的机器上
/// 可能只有 OpenGL 1.1）时，双击运行的表现就是「什么都没发生」，连原因都拿不到。
/// panic 有钩子兜着，`Result::Err` 之前没有，这里补上。
fn report_startup_failure(err: &eframe::Error, backend: &RendererChoice) {
    let detail = format!(
        "logview 无法创建窗口，已退出。\n\n\
         原因：{err}\n\n\
         渲染后端：{}\n\n\
         运行环境：{}\n\n\
         若提示与图形有关，可以试：\n{}",
        backend.describe(),
        session_kind(),
        startup_advice(backend.name),
    );

    eprintln!("{detail}");

    #[cfg(not(debug_assertions))]
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("logview 启动失败")
        .set_description(&detail)
        .show();
}

/// 启动失败的排查建议。
///
/// 「换个后端」这条只在同平台确实编了两个后端时才给：Windows 只编 wgpu、macOS 只编 glow，
/// 说出另一个名字等于把人引到一条不存在的路上。取名而非取整个 `RendererChoice`，
/// 是因为建议只与"当前是哪个后端"有关，这样也更好直接断言。
fn startup_advice(backend_name: &str) -> String {
    if cfg!(target_os = "linux") {
        let other = if backend_name == "glow" {
            "wgpu"
        } else {
            "glow"
        };
        let mut lines = vec![
            format!("· 换渲染后端：LOGVIEW_RENDERER={other} ./logview"),
            "· 换一条图形路径：在 Wayland 会话里运行，或加 LIBGL_ALWAYS_SOFTWARE=1".to_string(),
        ];
        if backend_name == "wgpu" {
            lines.push("· 指定 wgpu 的子后端：WGPU_BACKEND=gl ./logview（或 vulkan）".to_string());
        }
        lines.push("· 在本机登录（而不是远程桌面 / 远程 X 会话）后运行".to_string());
        lines.push("· 安装或更新显卡驱动".to_string());
        lines.join("\n")
    } else {
        "· 在本机登录（而不是远程桌面）后运行\n· 安装或更新显卡驱动".to_string()
    }
}

/// 选定的渲染后端。
///
/// 除了枚举本身还带着名字与来源，因为启动时要把它打到 stderr：出问题的那份日志必须
/// 能自己说清走的是哪条图形路径。glow 与 wgpu 在 X11 上分别走 GLX 与 EGL，失败时的
/// 报错却都从 winit 里冒出来、形态几乎一样（都带 `Failed to call XMapRaised`），
/// 光看报错分不出是谁干的——排查时就差这一行。
struct RendererChoice {
    name: &'static str,
    renderer: eframe::Renderer,
    from_env: bool,
}

impl RendererChoice {
    /// 启动日志与报错里用的一句话描述
    fn describe(&self) -> String {
        if self.from_env {
            format!("{}（来自 {ENV_RENDERER}）", self.name)
        } else {
            format!("{}（平台默认）", self.name)
        }
    }
}

/// 平台默认后端：Windows 与 Linux 走 wgpu，macOS 走 glow。理由见 `Cargo.toml`。
///
/// 名字跟着枚举一起返回，而不是另写一个"把枚举映射回名字"的函数——`Renderer` 的变体
/// 按 feature 存在与否，映射函数里一旦引用没编进来的变体就连编译都过不了。
#[cfg(any(windows, target_os = "linux"))]
fn default_renderer() -> (&'static str, eframe::Renderer) {
    ("wgpu", eframe::Renderer::Wgpu)
}

#[cfg(all(not(windows), not(target_os = "linux")))]
fn default_renderer() -> (&'static str, eframe::Renderer) {
    ("glow", eframe::Renderer::Glow)
}

fn preferred_renderer() -> RendererChoice {
    let requested = std::env::var(ENV_RENDERER).ok();
    match renderer_override(requested.as_deref()) {
        Some((name, renderer)) => RendererChoice {
            name,
            renderer,
            from_env: true,
        },
        None => {
            // 设了值却没生效时要说一声：可能是拼错了，也可能是这个平台没编那个后端
            // （`Renderer` 的变体按 feature 存在与否，Windows 上就没有 glow）。
            if let Some(v) = requested
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                eprintln!(
                    "{ENV_RENDERER}={v} 未生效（取值有误，或本平台未编入该后端），按默认后端运行"
                );
            }
            let (name, renderer) = default_renderer();
            RendererChoice {
                name,
                renderer,
                from_env: false,
            }
        }
    }
}

/// 解析 `LOGVIEW_RENDERER` 的取值，返回（名字, 后端）。没指定、拼错、或该后端在本平台
/// 没编进来时返回 `None`，即"按平台默认来"——而不是崩掉。
///
/// 抽成纯函数是为了能直接断言——塞在 env 读取里面就只能靠手设环境变量试。
fn renderer_override(value: Option<&str>) -> Option<(&'static str, eframe::Renderer)> {
    let want = value?.trim();

    // 可用性必须与 `Cargo.toml` 的按平台 feature 声明一致：Windows 只编 wgpu、
    // macOS 只编 glow，引用没编进来的变体连编译都过不了，所以这里用同样的 cfg 挡一层。
    #[cfg(not(windows))]
    if want.eq_ignore_ascii_case("glow") {
        return Some(("glow", eframe::Renderer::Glow));
    }
    #[cfg(any(windows, target_os = "linux"))]
    if want.eq_ignore_ascii_case("wgpu") {
        return Some(("wgpu", eframe::Renderer::Wgpu));
    }

    None
}

/// 打印 wgpu 实际选中的适配器。
///
/// 「选了 wgpu」不等于「跑在硬件上」：wgpu 在 Linux 会先试 Vulkan、再退到 GL(EGL)，
/// 后者在软件渲染（llvmpipe）下也能跑起来。出问题时这一行能立刻区分硬件与软件渲染，
/// 省掉一轮来回。只有编了 wgpu 的平台（Windows / Linux）有这一步。
#[cfg(any(windows, target_os = "linux"))]
fn report_adapter(cc: &eframe::CreationContext<'_>) {
    if let Some(state) = &cc.wgpu_render_state {
        let info = state.adapter.get_info();
        eprintln!(
            "[logview] 图形适配器 = {:?} / {}（{:?}）",
            info.backend, info.name, info.device_type
        );
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn report_adapter(_cc: &eframe::CreationContext<'_>) {}

fn main() -> std::process::ExitCode {
    install_panic_dialog();

    // 支持 `logview app.log` 直接打开
    let initial: Option<std::path::PathBuf> = std::env::args()
        .nth(1)
        .filter(|s| !s.starts_with('-'))
        .map(std::path::PathBuf::from);

    let backend = preferred_renderer();
    // 先把"走的是哪条图形路径"说清楚再开窗口：崩在窗口创建阶段时，这一行往往就是
    // 日志里唯一能定位问题的东西（Windows 的 release 没有 stderr，但那边也不看终端；
    // Linux 上它一直在）。
    eprintln!("[logview] 渲染后端 = {}", backend.describe());

    // wgpu 另有自己的环境变量能限定子后端。它在 wgpu 里是"认不出的名字就悄悄忽略"，
    // 一路忽略到"一个后端都没有"，最后只报成"找不到适配器"——设了就打出来，
    // 省得为这个拼写问题再排查一轮。
    if backend.name == "wgpu" {
        if let Some(v) = std::env::var(ENV_WGPU_BACKEND)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            eprintln!("[logview] {ENV_WGPU_BACKEND}={v}（wgpu 的子后端被限定为这个）");
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(load_icon()),
        // wgpu 而不是 glow：远程桌面 / 远程 X 会话下 OpenGL 那条路太不可靠
        // （详见 Cargo.toml 里的说明）。可用 LOGVIEW_RENDERER 覆盖。
        renderer: backend.renderer,
        ..Default::default()
    };

    let result = eframe::run_native(
        "logview",
        options,
        Box::new(move |cc| {
            fonts::setup(&cc.egui_ctx);
            report_adapter(cc);
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
            report_startup_failure(&err, &backend);
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

    /// 换后端这个出口要认得住常见写法，也要能拒绝乱填的值
    #[test]
    fn parses_renderer_override() {
        // 与平台无关的部分：没设、留空、拼错 —— 都当作"按平台默认来"，而不是崩掉
        assert_eq!(renderer_override(None), None);
        assert_eq!(renderer_override(Some("")), None);
        assert_eq!(renderer_override(Some("   ")), None);
        assert_eq!(renderer_override(Some("vulkan")), None);

        // 可用的后端随平台而变（与 Cargo.toml 的 feature 声明一致），
        // 所以断言也跟着 cfg 走，而不是写一个"哪里都能过"的空断言
        #[cfg(any(windows, target_os = "linux"))]
        {
            assert!(matches!(
                renderer_override(Some("wgpu")),
                Some(("wgpu", eframe::Renderer::Wgpu))
            ));
            assert!(
                matches!(
                    renderer_override(Some("  WGPU ")),
                    Some(("wgpu", eframe::Renderer::Wgpu))
                ),
                "应忽略大小写与空白"
            );
        }
        #[cfg(not(windows))]
        assert!(matches!(
            renderer_override(Some("glow")),
            Some(("glow", eframe::Renderer::Glow))
        ));
        #[cfg(windows)]
        assert_eq!(
            renderer_override(Some("glow")),
            None,
            "Windows 产物里没有编 glow，这个值应被当成不可用"
        );
    }

    /// 默认后端必须与 `Cargo.toml` 的 feature 声明对得上。
    ///
    /// 这里只能断言名字，不能写 `matches!(renderer, Renderer::Wgpu)`：
    /// macOS 上那个变体根本没编进来，写出来是把编译错误引到测试里。
    #[test]
    fn default_backend_matches_platform() {
        let (name, _renderer) = default_renderer();
        if cfg!(any(windows, target_os = "linux")) {
            assert_eq!(name, "wgpu");
        } else {
            assert_eq!(name, "glow");
        }
    }

    /// 建议里要出现"另一个"后端；没有第二个后端的平台上则不该给出换后端的建议
    #[test]
    fn advice_suggests_the_other_backend() {
        let advice = startup_advice("glow");
        if cfg!(target_os = "linux") {
            assert!(
                advice.contains("LOGVIEW_RENDERER=wgpu"),
                "Linux 上 glow 走不通时该建议换 wgpu：{advice}"
            );
        } else {
            assert!(
                !advice.contains("LOGVIEW_RENDERER"),
                "本平台只编了一个后端，不该建议换：{advice}"
            );
        }

        if cfg!(target_os = "linux") {
            let advice = startup_advice("wgpu");
            assert!(
                advice.contains("LOGVIEW_RENDERER=glow"),
                "反过来也一样：{advice}"
            );
            assert!(
                advice.contains("WGPU_BACKEND"),
                "wgpu 还有子后端可挑，值得提一句：{advice}"
            );
        }
    }
}
