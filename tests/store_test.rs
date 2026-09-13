use logview::logstore::{encoding_name, is_utf8, LogStore};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn write_tmp(name: &str, data: &[u8]) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("logview_test_{name}"));
    std::fs::write(&p, data).unwrap();
    p
}

/// 等待后台索引/搜索完成
fn settle(s: &mut LogStore, timeout: Duration) {
    let start = Instant::now();
    loop {
        s.pump();
        if !s.indexing && !s.searching {
            return;
        }
        if start.elapsed() > timeout {
            panic!("后台任务超时未完成");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn open(data: &[u8], name: &str) -> LogStore {
    let p = write_tmp(name, data);
    let mut s = LogStore::open(p).unwrap();
    settle(&mut s, Duration::from_secs(5));
    s
}

#[test]
fn counts_lines_utf8() {
    let s = open(b"first\nsecond\nthird\n", "utf8.log");
    assert_eq!(s.line_count(), 3);
    assert!(is_utf8(s.encoding));

    let mut out = String::new();
    s.read_line_into(1, &mut out);
    assert_eq!(out, "second");
}

#[test]
fn last_line_without_newline_is_counted() {
    let s = open(b"a\nb\nno-trailing-newline", "tail.log");
    assert_eq!(s.line_count(), 3);
    let mut out = String::new();
    s.read_line_into(2, &mut out);
    assert_eq!(out, "no-trailing-newline");
}

#[test]
fn empty_file_has_no_lines() {
    let s = open(b"", "empty.log");
    assert_eq!(s.line_count(), 0);
    assert_eq!(s.len_bytes(), 0);
}

#[test]
fn crlf_is_trimmed() {
    let s = open(b"one\r\ntwo\r\n", "crlf.log");
    assert_eq!(s.line_count(), 2);
    let mut out = String::new();
    s.read_line_into(0, &mut out);
    assert_eq!(out, "one");
}

#[test]
fn gbk_log_is_decoded() {
    // "错误日志" 的 GB18030 字节
    let (bytes, _, _) = encoding_rs::GB18030.encode("错误日志\nsecond\n");
    let s = open(&bytes, "gbk.log");
    assert!(!is_utf8(s.encoding), "应识别为非 UTF-8");
    assert_eq!(encoding_name(s.encoding), "GB18030");

    let mut out = String::new();
    s.read_line_into(0, &mut out);
    assert_eq!(out, "错误日志");
}

#[test]
fn search_case_sensitive() {
    let data = b"INFO start\nERROR boom\nwarn maybe\nerror lower\n";
    let mut s = open(data, "search.log");

    s.start_search("ERROR", true);
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.matches(), &[1], "区分大小写时只应命中第 2 行");

    s.start_search("error", false);
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.matches(), &[1, 3], "忽略大小写时两行都应命中");
}

#[test]
fn search_deduplicates_multiple_hits_per_line() {
    let mut s = open(b"error error error\nok\n", "dup.log");
    s.start_search("error", true);
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.matches(), &[0], "同一行多次命中只应出现一次");
}

#[test]
fn search_chinese_query() {
    let (bytes, _, _) = encoding_rs::GB18030.encode("第一条\n错误日志\n第三条\n");
    let mut s = open(&bytes, "cn.log");
    s.start_search("错误", true);
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.matches(), &[1]);
}

#[test]
fn no_match_returns_empty() {
    let mut s = open(b"a\nb\n", "nomatch.log");
    s.start_search("zzzz", true);
    settle(&mut s, Duration::from_secs(5));
    assert!(s.matches().is_empty());
}

#[test]
fn file_growth_is_picked_up() {
    let mut p = std::env::temp_dir();
    p.push("logview_test_grow.log");
    std::fs::write(&p, b"line1\n").unwrap();

    let mut s = LogStore::open(p.clone()).unwrap();
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.line_count(), 1);

    std::fs::write(&p, b"line1\nline2\nline3\n").unwrap();
    s.refresh();
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.line_count(), 3);

    let mut out = String::new();
    s.read_line_into(2, &mut out);
    assert_eq!(out, "line3");
}

#[test]
fn truncation_rebuilds_index() {
    let mut p = std::env::temp_dir();
    p.push("logview_test_truncate.log");
    std::fs::write(&p, b"a\nb\nc\nd\n").unwrap();

    let mut s = LogStore::open(p.clone()).unwrap();
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.line_count(), 4);

    // 模拟日志轮转：文件被重写成更短的内容
    std::fs::write(&p, b"x\ny\n").unwrap();
    s.refresh();
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.line_count(), 2);

    let _ = std::fs::remove_file(&p);
}
