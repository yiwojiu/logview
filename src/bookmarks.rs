//! 标记（书签）：把"这一行值得回看"记下来，并保证下次打开仍能找到它。
//!
//! 两个决定值得先说清楚：
//!
//! 1. **锚点不是行号，是字节偏移 + 内容指纹。** 行号会变——日志被就地截断、被轮转
//!    重写之后，同一个行号指向的是别的内容，而界面上看不出来。所以每条标记同时记下
//!    "那个字节位置上原本是什么"，跳转前先核对：对不上就明说失效，绝不静默跳到错行。
//! 2. **不写日志本身，只写侧车文件** `<日志名>.logview.jsonl`，一条标记一行 JSON。
//!    格式手写读写，不引 serde——为几条标记拉两个依赖不划算，而这个项目为省 2 MB
//!    连 PNG 解码器都不要。
//!
//! 时间戳存 Unix 秒、显示成相对时间（"3 天前"）：格式化成本地时间需要时区数据，
//! 而这里要回答的问题只有一个——"这条是这次标的还是上次留下的"。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 指纹取该行开头的多少个字符。太短容易撞（同一段重复日志），太长则会跟着日志格式
/// 一起变长；48 个字符足以区分同一文件里的不同行，也短到能塞进侧车文件。
pub const HEAD_CHARS: usize = 48;

/// 侧车文件的扩展名，落在日志旁边
const SIDECAR_SUFFIX: &str = ".logview.jsonl";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    /// 行起始的字节偏移——定位的第一依据
    pub offset: u64,
    /// 记录时的行号（0 基）。只用于显示与排序：它可能已经漂移
    pub line: u32,
    /// 该行开头的若干字符，用作指纹
    pub head: String,
    /// 备注，可为空
    pub note: String,
    /// 记录时间（Unix 秒）
    pub at: u64,
}

impl Bookmark {
    /// 依据行内容造一条标记；`head` 由内容算出，不另外传
    pub fn new(offset: usize, line: usize, line_text: &str, at: u64) -> Self {
        Self {
            offset: offset as u64,
            line: line as u32,
            head: head_of(line_text),
            note: String::new(),
            at,
        }
    }

    /// 指纹是否还对得上。
    ///
    /// 判据是"当前这一行以记下的开头**开始**"，而不是两边完全相等：
    /// - 指纹只截前 [`HEAD_CHARS`] 个字符，长行后面本来就会变（也没打算管）；
    /// - 日志最后一行常有"先写半行、稍后补齐换行"的追加方式，那时整行确实变长了，
    ///   但开头没变，不该判失效。
    ///
    /// 空行没有指纹可言（`head` 为空），这时只认偏移——总比认定它失效好。
    pub fn matches_head(&self, current_line: &str) -> bool {
        self.head.is_empty() || current_line.trim().starts_with(&self.head)
    }

    /// "3 天前" 这类相对时间。算不出来（时钟回拨）就说"更早"。
    pub fn relative_time(&self, now: u64) -> String {
        relative_time(self.at, now)
    }
}

/// 取一行文字开头的若干字符作为指纹：去掉首尾空白，按**字符**截断（不是字节），
/// 否则中文会被切成半个字。
pub fn head_of(line_text: &str) -> String {
    line_text.trim().chars().take(HEAD_CHARS).collect()
}

pub fn relative_time(at: u64, now: u64) -> String {
    match now.checked_sub(at) {
        None => "更早".to_string(),
        Some(0..=59) => "刚刚".to_string(),
        Some(s) if s < 3600 => format!("{} 分钟前", s / 60),
        Some(s) if s < 86_400 => format!("{} 小时前", s / 3600),
        Some(s) if s < 86_400 * 30 => format!("{} 天前", s / 86_400),
        Some(_) => "更早".to_string(),
    }
}

/// 侧车文件路径：`app.log` → `app.log.logview.jsonl`（与日志同目录）。
///
/// 放在日志旁边的理由：标注跟着日志走。把日志打包给同事、或换台机器复盘时，
/// 结论还在原来那行上；删日志时顺手把同名侧车一起删掉就行。
pub fn sidecar_path(log: &Path) -> PathBuf {
    let mut name = log.file_name().unwrap_or_default().to_os_string();
    name.push(SIDECAR_SUFFIX);
    log.with_file_name(name)
}

/// 读侧车文件。文件不存在、读不动、单行格式不对——都只跳过，不报错：
/// 标记是附加信息，不该因为一个坏行就打不开日志。
pub fn load(path: &Path) -> Vec<Bookmark> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out: Vec<Bookmark> = text.lines().filter_map(from_json).collect();
    out.sort_by_key(|b| b.offset);
    out.dedup_by_key(|b| b.offset);
    out
}

/// 写侧车文件。整份重写：几条到几百条标记的量级，重写的代价可以忽略，
/// 换来的是"文件内容永远等于内存里的状态"，不会出现半途损坏的增量文件。
pub fn save(path: &Path, items: &[Bookmark]) -> std::io::Result<()> {
    let mut text = String::with_capacity(items.len() * 160);
    for b in items {
        text.push_str(&to_json(b));
        text.push('\n');
    }
    std::fs::write(path, text)
}

/// 导出成 Markdown，贴进工单/复盘文档用
pub fn export_markdown(
    path: &Path,
    log_name: &str,
    items: &[Bookmark],
    now: u64,
) -> std::io::Result<()> {
    let mut text = format!("# 标记 · {log_name}\n\n共 {} 条。\n\n", items.len());
    for b in items {
        text.push_str(&format!("- **第 {} 行**　`{}`\n", b.line + 1, b.head));
        if !b.note.is_empty() {
            text.push_str(&format!("  - 备注：{}\n", b.note));
        }
        text.push_str(&format!("  - 标记于 {}\n", b.relative_time(now)));
    }
    std::fs::write(path, text)
}

pub fn to_json(b: &Bookmark) -> String {
    format!(
        "{{\"offset\":{},\"line\":{},\"at\":{},\"head\":\"{}\",\"note\":\"{}\"}}",
        b.offset,
        b.line,
        b.at,
        escape_json(&b.head),
        escape_json(&b.note)
    )
}

/// 解析一行。只认自己写出去的形状：键名固定，字段顺序随意，未知字段忽略——
/// 这样以后加字段时旧版本也能读下去。
pub fn from_json(line: &str) -> Option<Bookmark> {
    let offset = number_field(line, "offset")?;
    let line_no = number_field(line, "line").unwrap_or(0);
    Some(Bookmark {
        offset,
        line: line_no as u32,
        head: string_field(line, "head").unwrap_or_default(),
        note: string_field(line, "note").unwrap_or_default(),
        at: number_field(line, "at").unwrap_or(0),
    })
}

fn string_field(line: &str, key: &str) -> Option<String> {
    let start = line.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &line[start..];
    let colon = rest.find(':')? + 1;
    let mut chars = rest[colon..].trim_start().chars();
    if chars.next()? != '"' {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for c in chars {
        if escaped {
            out.push(match c {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

fn number_field(line: &str, key: &str) -> Option<u64> {
    let start = line.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &line[start..];
    let colon = rest.find(':')? + 1;
    let digits: String = rest[colon..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// JSON 字符串转义。控制字符走 `\u00XX`（JSON 规范不接受裸控制字符），
/// 只认我们可能遇到的几个：日志里的引号、反斜杠与换行。
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 当前时间（Unix 秒）。抽出来是为了测试能传固定值。
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_keeps_chinese_intact() {
        let head = head_of("  09:14:03.002 ERROR order 8821 failed: gateway timeout  ");
        assert!(head.starts_with("09:14:03.002 ERROR"));
        assert_eq!(head.len(), HEAD_CHARS, "ASCII 下按字符截断等于按字节");

        // 按字节截断会把中文切成半个字（然后是替换字符），这里必须按字符
        let cn = head_of(&"中".repeat(100));
        assert_eq!(cn.chars().count(), HEAD_CHARS);
        assert!(cn.chars().all(|c| c == '中'));
    }

    #[test]
    fn empty_line_has_no_fingerprint_and_stays_valid() {
        let b = Bookmark::new(0, 0, "   \n", 0);
        assert!(b.head.is_empty());
        assert!(b.matches_head("完全不同的内容"), "空指纹只认偏移");
        assert!(b.matches_head(""));
    }

    #[test]
    fn fingerprint_rejects_a_rewritten_line() {
        let b = Bookmark::new(120, 3, "09:14:03 ERROR order 8821 failed", 0);
        assert!(b.matches_head("09:14:03 ERROR order 8821 failed"));
        assert!(
            b.matches_head("  09:14:03 ERROR order 8821 failed  "),
            "两侧空白不算差异"
        );
        assert!(!b.matches_head("09:14:03 INFO order 8821 ok"));

        // 指纹只取开头：长行后面变了仍算同一行
        let long = "x".repeat(HEAD_CHARS + 20);
        let b = Bookmark::new(0, 0, &long, 0);
        assert!(b.matches_head(&format!("{long} 后面还有内容")));
        assert!(
            !b.matches_head(&"y".repeat(HEAD_CHARS + 20)),
            "开头变了就是另一行"
        );

        // 最后一行可能"先写半行、后补换行"，整行变长但开头没变——不该判失效
        let tail = Bookmark::new(0, 0, "最后一行写到一半", 0);
        assert!(tail.matches_head("最后一行写到一半，补完了"));
        assert!(!tail.matches_head("换了完全不同的内容"), "开头不对就该失效");
    }

    #[test]
    fn json_round_trip_survives_hostile_notes() {
        let mut b = Bookmark::new(98213, 1483, "09:14:03 ERROR order 8821", 1_757_000_000);
        b.note = "引号\" 反斜杠\\ 换行\n制表\t 中文 与 🎯".to_string();
        let text = to_json(&b);
        assert!(!text.contains('\n'), "一条记录必须落在同一行上");
        assert_eq!(from_json(&text), Some(b));
    }

    #[test]
    fn json_reader_tolerates_field_order_and_extra_fields() {
        let line =
            r#"{"v":2,"note":"备注","head":"行首","at":42,"offset":10,"line":2,"future":true}"#;
        let b = from_json(line).expect("应能解析");
        assert_eq!(b.offset, 10);
        assert_eq!(b.line, 2);
        assert_eq!(b.head, "行首");
        assert_eq!(b.note, "备注");
        assert_eq!(b.at, 42);

        assert_eq!(from_json("这不是 JSON"), None);
        assert_eq!(
            from_json(r#"{"line":3}"#),
            None,
            "没有 offset 的记录无法定位"
        );
    }

    #[test]
    fn sidecar_sits_next_to_the_log() {
        let p = sidecar_path(Path::new("/var/log/app.log"));
        assert_eq!(p, PathBuf::from("/var/log/app.log.logview.jsonl"));

        // 没有扩展名的日志也要能用
        let p = sidecar_path(Path::new("C:/logs/102_Ana_0"));
        assert_eq!(p.file_name().unwrap(), "102_Ana_0.logview.jsonl");
    }

    #[test]
    fn relative_time_reads_naturally() {
        let now = 1_757_000_000;
        assert_eq!(relative_time(now, now), "刚刚");
        assert_eq!(relative_time(now - 600, now), "10 分钟前");
        assert_eq!(relative_time(now - 7_200, now), "2 小时前");
        assert_eq!(relative_time(now - 86_400 * 3, now), "3 天前");
        assert_eq!(relative_time(now + 60, now), "更早", "时钟回拨时不装作知道");
    }
}
