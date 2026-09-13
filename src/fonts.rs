use std::path::Path;

/// 各平台常见中文字体候选，按优先级排列，取第一个存在的。
/// egui 内置字体只覆盖拉丁字符，不加载 CJK 字体会显示成方框。
fn cjk_candidates() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/Library/Fonts/Arial Unicode.ttf",
            "/System/Library/Fonts/Supplemental/Songti.ttc",
        ]
    } else if cfg!(target_os = "windows") {
        &[
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\msyh.ttf",
            "C:\\Windows\\Fonts\\simhei.ttf",
            "C:\\Windows\\Fonts\\simsun.ttc",
            "C:\\Windows\\Fonts\\Deng.ttf",
        ]
    } else {
        &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/truetype/arphic/uming.ttc",
            "/usr/share/fonts/google-noto/NotoSansCJK-Regular.ttc",
        ]
    }
}

fn load_cjk() -> Option<(String, Vec<u8>)> {
    for p in cjk_candidates() {
        let path = Path::new(p);
        if path.exists() {
            if let Ok(data) = std::fs::read(path) {
                if !data.is_empty() {
                    let name = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "cjk".to_string());
                    return Some((name, data));
                }
            }
        }
    }
    None
}

/// 必须在创建窗口前调用。找不到 CJK 字体时原样使用 egui 默认字体。
pub fn setup(ctx: &egui::Context) {
    let Some((name, data)) = load_cjk() else {
        eprintln!("[logview] 未找到系统中文字体，中文可能显示为方框");
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        name.clone(),
        std::sync::Arc::new(egui::FontData::from_owned(data)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.clone());
    }
    ctx.set_fonts(fonts);
}
