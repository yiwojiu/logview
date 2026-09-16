use std::path::{Path, PathBuf};

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

/// 文件名里带这些片段的就算 CJK 字体，按优先级排列（先命中先赢）。
///
/// 用文件名片段而不是完整路径，是因为各发行版把**同一套**字体装在不同层级：
/// Debian/Ubuntu 是 `/usr/share/fonts/truetype/wqy/`，RHEL/CentOS 是
/// `/usr/share/fonts/wqy-zenhei/`，openSUSE 又是另一处。写死路径必然漏掉一半机器，
/// 于是标准路径没命中时按文件名认（见 `scan_font_dirs`）。
const CJK_FONT_KEYS: &[&str] = &[
    "wqy-zenhei",
    "wqy-microhei",
    "notosanscjk",
    "notosanssc",
    "sourcehansans",
    "droidsansfallback",
    "wenquanyi",
    "uming",
    "ukai",
];

/// 扫描字体目录时的上限：防止在异常目录结构里迷路
/// （正常机器上 `/usr/share/fonts` 只有几百个文件，几毫秒就扫完）
const SCAN_MAX_DEPTH: usize = 4;
const SCAN_MAX_ENTRIES: usize = 20_000;

/// 找不到系统 CJK 字体时的提示。
///
/// 带一条能直接照做的安装命令——只说"可能显示为方框"等于把问题丢回给用户，
/// 而这一步在各平台上都是同一条命令。
const FONT_HINT: &str =
    "装一个即可：yum/dnf install wqy-zenhei-fonts ｜ apt install fonts-wqy-zenhei";

/// 字体可能被装在哪儿。这几个位置只对 Linux 有意义——Windows 与 macOS 的字体位置是固定的，
/// 标准路径已经覆盖，`scan_font_dirs` 在那两个平台上会直接落空（无害）。
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
    }
    dirs
}

/// 是不是一个 CJK 字体文件——按文件名判断。纯函数，便于直接断言。
fn is_cjk_font_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    let is_font = matches!(
        Path::new(&lower).extension().and_then(|e| e.to_str()),
        Some("ttf" | "ttc" | "otf" | "otc")
    );
    is_font && CJK_FONT_KEYS.iter().any(|k| lower.contains(k))
}

/// 按 `CJK_FONT_KEYS` 的优先级从候选文件名里挑一个。
///
/// 同一优先级里还要再排一下：字体目录的遍历顺序是文件系统给的，不排的话同一台机器
/// 两次运行可能挑到不同字体；而 Bold 当界面字体太重，所以 Regular 优先。
/// 纯函数，便于直接断言。
fn pick_cjk_font<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let names: Vec<&str> = names.into_iter().collect();
    for key in CJK_FONT_KEYS {
        let mut hits: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| n.to_lowercase().contains(key))
            .collect();
        hits.sort_by_key(|n| font_sort_key(n));
        if let Some(name) = hits.first() {
            return Some((*name).to_string());
        }
    }
    None
}

fn font_sort_key(name: &str) -> (bool, usize, String) {
    let lower = name.to_lowercase();
    (!lower.contains("regular"), name.len(), lower)
}

/// 在字体目录里找一个 CJK 字体。只在标准路径全部落空时才走这条路。
fn scan_font_dirs() -> Option<PathBuf> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    let mut visited = 0usize;

    'roots: for root in font_dirs() {
        let mut stack = vec![(root, 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            if depth > SCAN_MAX_DEPTH {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                visited += 1;
                if visited > SCAN_MAX_ENTRIES {
                    // 目录结构反常：拿已经找到的就走，别一直扫
                    break 'roots;
                }
                let path = entry.path();
                if path.is_dir() {
                    stack.push((path, depth + 1));
                    continue;
                }
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
                    continue;
                };
                if is_cjk_font_file(&name) {
                    found.push((name, path));
                }
            }
        }
    }

    let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
    let chosen = pick_cjk_font(names)?;
    found
        .iter()
        .find(|(n, _)| n == &chosen)
        .map(|(_, path)| path.clone())
}

/// 读一个字体文件，返回（名字, 内容）；读不出来或空文件就当作没有。
fn read_font(path: &Path) -> Option<(String, Vec<u8>)> {
    let data = std::fs::read(path).ok()?;
    if data.is_empty() {
        return None;
    }
    let name = path.file_stem()?.to_string_lossy().to_string();
    Some((name, data))
}

fn load_cjk() -> Option<(String, Vec<u8>)> {
    // ① 先查标准路径：常见发行版一次就命中，不必扫目录
    for p in cjk_candidates() {
        if let Some(font) = read_font(Path::new(p)) {
            return Some(font);
        }
    }
    // ② 再按文件名在字体目录里找：同一套字体各发行版装的位置不一样
    read_font(&scan_font_dirs()?)
}

/// 必须在创建窗口前调用。找不到 CJK 字体时原样使用 egui 默认字体。
pub fn setup(ctx: &egui::Context) {
    let Some((name, data)) = load_cjk() else {
        eprintln!("[logview] 未找到系统中文字体，中文可能显示为方框");
        eprintln!("[logview] {FONT_HINT}");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 认得出各发行版给同一套字体起的文件名，也别把别的文件当字体
    #[test]
    fn recognizes_cjk_font_files() {
        assert!(is_cjk_font_file("wqy-zenhei.ttc"));
        assert!(is_cjk_font_file("wqy-microhei.ttc"));
        assert!(is_cjk_font_file("NotoSansCJK-Regular.ttc"));
        assert!(is_cjk_font_file("NotoSansSC-Regular.otf"));
        assert!(is_cjk_font_file("DroidSansFallbackFull.ttf"));

        assert!(
            !is_cjk_font_file("DejaVuSans.ttf"),
            "拉丁字体不该被当成 CJK"
        );
        assert!(!is_cjk_font_file("fonts.dir"), "不是字体扩展名");
        assert!(
            !is_cjk_font_file("wqy-zenhei.ttc.bak"),
            "扩展名不对就不算，别把备份文件喂给字体解析器"
        );
    }

    /// 优先级要生效，且同一优先级里结果稳定（不随目录遍历顺序变）
    #[test]
    fn picks_by_priority_then_name() {
        let picked = pick_cjk_font([
            "uming.ttc",
            "wqy-zenhei.ttc",
            "NotoSansCJK-Regular.ttc",
            "NotoSansCJK-Bold.ttc",
        ]);
        assert_eq!(
            picked.as_deref(),
            Some("wqy-zenhei.ttc"),
            "wqy-zenhei 排在 noto 之前"
        );

        let picked = pick_cjk_font([
            "uming.ttc",
            "NotoSansCJK-Bold.ttc",
            "NotoSansCJK-Regular.ttc",
        ]);
        assert_eq!(
            picked.as_deref(),
            Some("NotoSansCJK-Regular.ttc"),
            "同一套字体里 Regular 优先，Bold 当界面字体太重"
        );

        assert_eq!(pick_cjk_font(["DejaVuSans.ttf", "fonts.dir"]), None);
        assert_eq!(pick_cjk_font(Vec::<&str>::new()), None);
    }
}
