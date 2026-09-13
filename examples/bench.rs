//! 大文件压力验证：生成日志 → 打开 → 测首屏/全量索引/搜索/随机读耗时。
//! 运行：cargo run --release --example bench [路径] [大小MB]

use logview::logstore::{LogStore, SearchRequest};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn generate(path: &Path, target_mb: usize) -> std::io::Result<()> {
    let levels = ["INFO", "DEBUG", "WARN", "ERROR", "TRACE"];
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut written = 0usize;
    let target = target_mb * 1024 * 1024;
    let mut i = 0u64;
    while written < target {
        let lvl = levels[(i % levels.len() as u64) as usize];
        let line = format!(
            "2026-09-13 07:{:02}:{:02}.{:03} [{lvl}] com.pspace.OrderService - 处理订单 orderId={} 用户=张三丰 状态=成功 耗时={}ms\n",
            (i / 60) % 60,
            i % 60,
            (i % 1000) as u32,
            i,
            i % 997,
        );
        f.write_all(line.as_bytes())?;
        written += line.len();
        i += 1;
    }
    f.flush()
}

fn main() {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/logview_bench.log"));
    let mb: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    if !path.exists() {
        print!("生成 {mb} MB 测试日志… ");
        std::io::stdout().flush().unwrap();
        let t = Instant::now();
        generate(&path, mb).expect("生成日志失败");
        println!("{:.2}s", t.elapsed().as_secs_f64());
    }
    let size = std::fs::metadata(&path).unwrap().len();
    println!(
        "文件：{}  {:.1} MB",
        path.display(),
        size as f64 / 1048576.0
    );

    let t = Instant::now();
    let mut s = LogStore::open(path.clone()).expect("打开失败");
    println!(
        "打开耗时：{:.3}s（mmap 映射，不读内容）",
        t.elapsed().as_secs_f64()
    );

    // 首屏：等到第一批行可用
    let mut first_paint = None;
    let indexed = loop {
        s.pump();
        if first_paint.is_none() && s.line_count() > 0 {
            first_paint = Some(t.elapsed());
        }
        if !s.indexing {
            break t.elapsed();
        }
        if t.elapsed() > Duration::from_secs(120) {
            panic!("索引超时");
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    println!("行数：{}", s.line_count());
    println!("首屏可见：{:.3}s", first_paint.unwrap().as_secs_f64());
    println!("全量索引：{:.3}s", indexed.as_secs_f64());

    // 随机读 5000 行
    let t2 = Instant::now();
    let n = s.line_count();
    let mut out = String::new();
    let mut total_chars = 0usize;
    for k in 0..5000 {
        let idx = (k * 7919) % n.max(1);
        out.clear();
        if s.read_line_into(idx, &mut out) {
            total_chars += out.len();
        }
    }
    println!(
        "随机读 5000 行：{:.3}s（共 {} 字符）",
        t2.elapsed().as_secs_f64(),
        total_chars
    );

    // 子串检索
    for q in ["ERROR", "张三丰", "orderId=12345"] {
        let t3 = Instant::now();
        s.start_search(SearchRequest::new(q, true, false));
        loop {
            s.pump();
            if !s.searching {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        println!(
            "子串 {q:<16} 命中 {:>7} 行  耗时 {:.3}s",
            s.matches().len(),
            t3.elapsed().as_secs_f64()
        );
    }

    // 正则检索：逐行解码 + 自动机，比字节匹配慢多少，量一下就知道了
    for pat in ["ERROR|WARN", "orderId=\\d+", "耗时=\\d{3}ms"] {
        let t4 = Instant::now();
        s.start_search(SearchRequest::new(pat, true, true));
        loop {
            s.pump();
            if !s.searching {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        println!(
            "正则 {pat:<16} 命中 {:>7} 行  耗时 {:.3}s",
            s.matches().len(),
            t4.elapsed().as_secs_f64()
        );
    }

    // 上面那些模式都撞到命中上限提前退出了，看不出扫完整个文件的代价。
    // 换成几乎不命中的模式，两种方式的真实差距才显示出来。
    for (label, pat, is_regex) in [
        ("子串(零命中)", "zzzznomatch", false),
        ("正则(零命中)", r"zzzz\d{3}|qqqqq", true),
    ] {
        let t5 = Instant::now();
        s.start_search(SearchRequest::new(pat, true, is_regex));
        loop {
            s.pump();
            if !s.searching {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        println!(
            "{label:<16} 命中 {:>7} 行  耗时 {:.3}s",
            s.matches().len(),
            t5.elapsed().as_secs_f64()
        );
    }

    // 常驻内存粗估
    println!("行偏移索引条数：{n}");
}
