use logview::logstore::{encoding_name, is_utf8, LogStore, SearchRequest};
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

/// 以追加方式写入，模拟真实的日志写入。
///
/// 这里不能用 `std::fs::write`：它会先截断文件，而 Windows 不允许截断一个
/// 正被内存映射的文件（`SetEndOfFile` 返回 `ERROR_USER_MAPPED_FILE`），
/// Unix 上则没有这个限制。追加只会扩展文件，各平台都允许。
fn append_bytes(path: &std::path::Path, data: &[u8]) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("以追加方式打开日志失败");
    f.write_all(data).expect("追加日志失败");
    f.flush().expect("flush 失败");
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

    s.start_search(SearchRequest::new("ERROR", true, false));
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.matches(), &[1], "区分大小写时只应命中第 2 行");

    s.start_search(SearchRequest::new("error", false, false));
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.matches(), &[1, 3], "忽略大小写时两行都应命中");
}

#[test]
fn search_deduplicates_multiple_hits_per_line() {
    let mut s = open(b"error error error\nok\n", "dup.log");
    s.start_search(SearchRequest::new("error", true, false));
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.matches(), &[0], "同一行多次命中只应出现一次");
}

#[test]
fn search_chinese_query() {
    let (bytes, _, _) = encoding_rs::GB18030.encode("第一条\n错误日志\n第三条\n");
    let mut s = open(&bytes, "cn.log");
    s.start_search(SearchRequest::new("错误", true, false));
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.matches(), &[1]);
}

#[test]
fn regex_search_supports_alternation() {
    let mut s = open(
        b"INFO ok\nERROR bad\nFATAL worse\nWARN meh\n",
        "regex_alt.log",
    );
    s.start_search(SearchRequest::new("ERROR|FATAL", true, true));
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.matches(), &[1, 2], "多选分支应同时命中两行");
}

#[test]
fn regex_search_can_anchor_and_ignore_case() {
    let mut s = open(b"error\nERROR\nError happened\n", "regex_anchor.log");
    s.start_search(SearchRequest::new("^error$", false, true));
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(
        s.matches(),
        &[0, 1],
        "忽略大小写时前两行命中，第三行不匹配 ^$"
    );
}

#[test]
fn regex_search_reports_invalid_pattern() {
    let mut s = open(b"anything\n", "regex_bad.log");
    assert!(s.start_search(SearchRequest::new("(unclosed", true, true)));
    settle(&mut s, Duration::from_secs(5));
    assert!(s.regex_error().is_some(), "非法正则应当报错");
    assert!(s.matches().is_empty(), "报错时不应留下命中结果");
}

/// GB18030 日志里的中文正则也要能用——这正是逐行解码而非字节匹配换来的能力
#[test]
fn regex_search_handles_chinese_in_gbk_file() {
    let (bytes, _, _) = encoding_rs::GB18030.encode("错误日志\n一切正常\n错误又来了\n");
    let mut s = open(&bytes, "regex_cn.log");
    s.start_search(SearchRequest::new("^错误", true, true));
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.matches(), &[0, 2]);
}

/// 切换检索方式后不应残留上一次的正则错误
#[test]
fn switching_back_to_plain_search_clears_regex_error() {
    let mut s = open(b"ERROR\n", "regex_clear.log");
    s.start_search(SearchRequest::new("(unclosed", true, true));
    settle(&mut s, Duration::from_secs(5));
    assert!(s.regex_error().is_some());

    s.start_search(SearchRequest::new("ERROR", true, false));
    settle(&mut s, Duration::from_secs(5));
    assert!(s.regex_error().is_none(), "回到子串检索后错误应被清掉");
    assert_eq!(s.matches(), &[0]);
}

#[test]
fn search_before_index_finishes_still_returns_every_hit() {
    // 打开文件后索引一定还在进行（索引线程需要被 pump 才会把 Done 交给 store），
    // 此时发起的检索必须被挂起，而不是拿"已扫描部分"得出一个偏少的命中数。
    let mut data = Vec::new();
    for i in 0..20_000 {
        data.extend_from_slice(format!("line {i} ERROR\n").as_bytes());
    }
    let p = write_tmp("pending.log", &data);

    let mut s = LogStore::open(p).unwrap();
    assert!(s.indexing, "前提：打开后索引应仍在进行中");

    s.start_search(SearchRequest::new("ERROR", true, false));
    assert!(s.search_pending(), "索引尚未完成时检索应当被挂起");

    settle(&mut s, Duration::from_secs(30));
    assert_eq!(s.matches().len(), 20_000, "索引完成后应命中全部行");
}

#[test]
fn clearing_search_cancels_a_pending_one() {
    let mut data = Vec::new();
    for i in 0..20_000 {
        data.extend_from_slice(format!("line {i} ERROR\n").as_bytes());
    }
    let p = write_tmp("pending_cancel.log", &data);

    let mut s = LogStore::open(p).unwrap();
    s.start_search(SearchRequest::new("ERROR", true, false));
    assert!(s.search_pending());

    s.clear_search();
    assert!(!s.search_pending(), "清空检索时应一并撤销挂起的条件");

    settle(&mut s, Duration::from_secs(30));
    assert!(s.matches().is_empty(), "被撤销的检索不应产生结果");
}

#[test]
fn no_match_returns_empty() {
    let mut s = open(b"a\nb\n", "nomatch.log");
    s.start_search(SearchRequest::new("zzzz", true, false));
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

    // 必须在 LogStore 打开之后追加，才能验证运行中检测文件增长的能力
    append_bytes(&p, b"line2\nline3\n");
    s.refresh();
    settle(&mut s, Duration::from_secs(5));
    assert_eq!(s.line_count(), 3);

    let mut out = String::new();
    s.read_line_into(2, &mut out);
    assert_eq!(out, "line3");
}

// Windows 上无法在映射存活期间截断文件，因此这个场景在该平台不可测。
// 对应地，Windows 下日志写入方也无法就地轮转一个正被本程序查看的日志文件，
// 这一点已记入 README 的「已知限制」。
#[cfg_attr(
    windows,
    ignore = "Windows cannot truncate a file that is currently memory-mapped"
)]
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
