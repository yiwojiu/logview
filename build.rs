//! 构建脚本：把 `assets/icon.rc` 编译成 Windows 资源并交给链接器。
//!
//! 为什么非做不可：`egui::ViewportBuilder::with_icon` 设置的是**运行时**的窗口与
//! 任务栏图标（走 CreateWindow），而资源管理器读的是 PE 文件里的 RT_GROUP_ICON
//! 资源。两者互不替代——没有资源节的 exe，在资源管理器里只能是系统默认图标。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let rc_file = manifest_dir.join("assets").join("icon.rc");
    let ico_file = manifest_dir.join("assets").join("icon.ico");
    println!("cargo:rerun-if-changed={}", rc_file.display());
    println!("cargo:rerun-if-changed={}", ico_file.display());
    println!("cargo:rerun-if-env-changed=RC");

    // 图标只对 Windows 的 PE 资源有意义，其他平台连 rc 都不用找
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // `.res` 是 MSVC 链接器认识的格式；mingw 那条路要改用 windres 产出 COFF 目标文件，
    // 这里不做适配，说清楚而不是悄悄少一个图标。
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        println!("cargo:warning=非 MSVC 工具链暂不嵌入 exe 图标，产物将是系统默认图标");
        return;
    }

    let Some(rc_exe) = find_resource_compiler() else {
        println!(
            "cargo:warning=找不到 rc.exe，本次构建的 exe 不会带自定义图标（可用 RC 环境变量指定路径）"
        );
        return;
    };

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default());
    let res = out_dir.join("logview.res");
    let status = Command::new(&rc_exe)
        // .rc 里引用图标用的是相对路径，把工作目录锚到包根，免得随调用目录漂移
        .current_dir(&manifest_dir)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(&rc_file)
        .status();

    match status {
        // 只作用于可执行文件本身，测试与基准测试不需要重复带一份图标
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", res.display()),
        Ok(s) => println!(
            "cargo:warning={} 编译 {} 失败（{s}），exe 将不带自定义图标",
            rc_exe.display(),
            rc_file.display()
        ),
        Err(e) => println!(
            "cargo:warning=无法执行 {}（{e}），exe 将不带自定义图标",
            rc_exe.display()
        ),
    }
}

/// 定位资源编译器 `rc.exe`。
///
/// Windows SDK 的 `bin` 目录**默认不在** PATH 里——那是 VS 开发者命令行才有的待遇。
/// 只按 PATH 找，普通终端里构建出的 exe 就会悄悄少一个图标，所以这里自己去翻
/// SDK 的安装目录。优先级：`RC` 环境变量 → SDK 目录里最新的一版 → PATH。
fn find_resource_compiler() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RC") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }

    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86") => "x86",
        _ => "x64",
    };
    for root in [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ] {
        let Some(versions) = version_dirs(Path::new(root)) else {
            continue;
        };
        // 最新的一版排在最后：各段等宽时字典序即版本序
        for dir in versions.iter().rev() {
            let cand = dir.join(arch).join("rc.exe");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }

    // 兜底：PATH 里可能已经有了
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join("rc.exe"))
        .find(|p| p.is_file())
}

/// 列出 SDK 目录下的版本号子目录（形如 `10.0.26100.0`），按名字升序
fn version_dirs(root: &Path) -> Option<Vec<PathBuf>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit()))
        })
        .collect();
    dirs.sort();
    Some(dirs)
}
