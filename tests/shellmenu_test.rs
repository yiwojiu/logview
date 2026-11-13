//! 资源管理器右键菜单集成的冒烟测试。
//!
//! 会**真改** HKCU 下的注册表，所以默认 `#[ignore]`：CI 的 `cargo test` 不会碰它。
//! 手动验证时这样跑：
//!
//! ```text
//! cargo test --release --test shellmenu_test -- --ignored --nocapture
//! ```
//!
//! 跑完会把注册表恢复成测试之前的样子（本来就是启用的话还会启用回去），
//! 但过程中菜单项会短暂消失——别在这期间去点右键。
#![cfg(windows)]

use logview::shellmenu;

#[test]
#[ignore = "会写入真实注册表（HKCU），只在手动验证时跑"]
fn install_then_uninstall_roundtrip() {
    let before = shellmenu::status().expect("读取初始状态");
    println!(
        "测试前：installed={} registered={:?}",
        before.installed, before.registered_exe
    );

    shellmenu::uninstall().expect("卸载");
    assert!(
        !shellmenu::status().unwrap().installed,
        "卸载之后不该还查得到菜单项"
    );

    shellmenu::install().expect("安装");
    let after = shellmenu::status().expect("读取安装后的状态");
    assert!(after.installed, "安装之后应该能查到菜单项");
    let exe = after
        .registered_exe
        .clone()
        .expect("command 里应该有 exe 路径");
    println!("注册的 exe：{exe}");
    assert!(
        !after.points_elsewhere(),
        "刚装完就指向别的路径，说明写进去的不是当前程序：{exe}"
    );

    shellmenu::uninstall().expect("再次卸载");
    assert!(!shellmenu::status().unwrap().installed, "卸载不干净");

    if before.installed {
        shellmenu::install().expect("恢复测试前的状态");
    }
}
