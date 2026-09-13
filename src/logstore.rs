use anyhow::{Context as _, Result};
use encoding_rs::{Encoding, GB18030, UTF_8};
use memmap2::{Mmap, MmapOptions};
use std::fs::File;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// 索引线程每次回传的行数
const INDEX_CHUNK: usize = 20_000;
/// 搜索最多保留的命中数，防止超高频词把内存打爆
pub const MAX_SEARCH_HITS: usize = 50_000;

pub enum IndexMsg {
    /// 一批行起始偏移
    Lines(Vec<usize>),
    /// 本轮扫描结束，参数为「下一条待确认行的起始偏移」
    Done(usize),
}

pub enum SearchMsg {
    /// 命中的字节偏移（升序）
    Hits(Vec<usize>),
}

/// 日志文件的只读视图。
///
/// 设计要点：
/// - 文件通过 mmap 映射，不读入内存，1GB 和 10MB 的常驻占用一样；
/// - 只维护「每行起始偏移」数组，随机访问任意行是 O(1)；
/// - 索引在后台线程增量构建，边建边显示；
/// - 文本解码只发生在渲染可见行的那一刻，一次只有几十行。
pub struct LogStore {
    pub path: PathBuf,
    _file: File,
    mmap: Arc<Mmap>,
    /// 每行起始字节偏移（升序）
    line_starts: Vec<usize>,
    /// 已确认完整扫描到的字节位置
    scanned: usize,
    /// 文件真实长度（空文件会用占位映射，故单独记录）
    real_len: usize,
    pub encoding: &'static Encoding,
    rx: Option<Receiver<IndexMsg>>,
    pub indexing: bool,
    search_rx: Option<Receiver<SearchMsg>>,
    pub searching: bool,
    /// 上一次搜索命中的行号（升序、去重）
    matches: Vec<u32>,
    /// 命中数是否达到上限而被截断
    search_truncated: bool,
}

impl LogStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path: PathBuf = path.into();
        let file = File::open(&path).with_context(|| format!("无法打开 {}", path.display()))?;
        let mut s = Self::from_file(path, file)?;
        s.spawn_index(0);
        Ok(s)
    }

    fn from_file(path: PathBuf, file: File) -> Result<Self> {
        let real_len = file.metadata()?.len() as usize;
        let mmap = map_file(&file)?;
        let encoding = sniff_encoding(&mmap, real_len);
        Ok(Self {
            path,
            _file: file,
            mmap: Arc::new(mmap),
            line_starts: Vec::new(),
            scanned: 0,
            real_len,
            encoding,
            rx: None,
            indexing: false,
            search_rx: None,
            searching: false,
            matches: Vec::new(),
            search_truncated: false,
        })
    }

    pub fn path_str(&self) -> String {
        self.path.display().to_string()
    }

    pub fn len_bytes(&self) -> usize {
        self.real_len
    }

    /// 可见行数（含最后一行尚未换行的）
    pub fn line_count(&self) -> usize {
        let n = self.line_starts.len();
        if !self.indexing && self.scanned < self.real_len {
            n + 1
        } else {
            n
        }
    }

    fn line_range(&self, idx: usize) -> Option<(usize, usize)> {
        let n = self.line_starts.len();
        if idx < n {
            let s = self.line_starts[idx];
            let e = if idx + 1 < n {
                self.line_starts[idx + 1]
            } else {
                self.scanned.min(self.real_len)
            };
            Some((s, e))
        } else if idx == n && !self.indexing && self.scanned < self.real_len {
            Some((self.scanned, self.real_len))
        } else {
            None
        }
    }

    /// 把第 idx 行解码后追加到 out（复用缓冲，避免每行分配）
    pub fn read_line_into(&self, idx: usize, out: &mut String) -> bool {
        let Some((s, e)) = self.line_range(idx) else {
            return false;
        };
        if s > e || e > self.mmap.len() {
            return false;
        }
        let raw = trim_eol(&self.mmap[s..e]);
        if is_utf8(self.encoding) {
            match std::str::from_utf8(raw) {
                Ok(t) => out.push_str(t),
                Err(_) => out.push_str(&String::from_utf8_lossy(raw)),
            }
        } else {
            let (cow, _, _) = self.encoding.decode(raw);
            out.push_str(&cow);
        }
        true
    }

    /// 抽取后台线程回传的数据。每帧调用一次，返回内容是否有变化。
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.rx {
            let mut done_at = None;
            for msg in rx.try_iter() {
                match msg {
                    IndexMsg::Lines(v) => {
                        self.line_starts.extend_from_slice(&v);
                        changed = true;
                    }
                    IndexMsg::Done(pos) => done_at = Some(pos),
                }
            }
            if let Some(pos) = done_at {
                self.scanned = pos;
                self.indexing = false;
                self.rx = None;
                changed = true;
            }
        }
        if let Some(rx) = &self.search_rx {
            if let Ok(SearchMsg::Hits(hits)) = rx.try_recv() {
                self.apply_hits(hits);
                self.searching = false;
                self.search_rx = None;
                changed = true;
            }
        }
        changed
    }

    /// 检查文件是否被追加/截断（日志轮转）。调用方负责节流。
    pub fn refresh(&mut self) {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return;
        };
        let len = meta.len() as usize;
        if len == self.real_len {
            return;
        }
        let Ok(file) = File::open(&self.path) else {
            return;
        };
        if len < self.real_len {
            // 被截断或轮转，整体重建
            self.line_starts.clear();
            self.scanned = 0;
            self.matches.clear();
            if self.remap(file).is_ok() {
                self.spawn_index(0);
            }
            return;
        }
        if self.remap(file).is_ok() && !self.indexing {
            self.spawn_index(self.scanned);
        }
    }

    fn remap(&mut self, file: File) -> Result<()> {
        let real_len = file.metadata()?.len() as usize;
        let mmap = map_file(&file)?;
        self._file = file;
        self.mmap = Arc::new(mmap);
        self.real_len = real_len;
        Ok(())
    }

    fn spawn_index(&mut self, start: usize) {
        let (tx, rx) = channel();
        let mmap = self.mmap.clone();
        let limit = self.real_len;
        self.rx = Some(rx);
        self.indexing = true;
        std::thread::spawn(move || scan_lines(mmap, start, limit, tx));
    }

    /// 启动一次后台搜索，返回是否真的启动
    pub fn start_search(&mut self, query: &str, case_sensitive: bool) -> bool {
        if query.is_empty() {
            self.clear_search();
            return false;
        }
        let pattern: Vec<u8> = if is_utf8(self.encoding) {
            query.as_bytes().to_vec()
        } else {
            self.encoding.encode(query).0.into_owned()
        };
        if pattern.is_empty() {
            return false;
        }
        let (tx, rx) = channel::<SearchMsg>();
        let mmap = self.mmap.clone();
        let limit = self.scanned.min(self.real_len);
        self.search_rx = Some(rx);
        self.searching = true;
        std::thread::spawn(move || {
            let hits = if case_sensitive {
                search_bytes(&mmap, limit, &pattern)
            } else {
                search_bytes_ci(&mmap, limit, &pattern)
            };
            let _ = tx.send(SearchMsg::Hits(hits));
        });
        true
    }

    pub fn matches(&self) -> &[u32] {
        &self.matches
    }

    pub fn clear_search(&mut self) {
        self.matches.clear();
        self.searching = false;
        self.search_rx = None;
        self.search_truncated = false;
    }

    /// 命中数是否撞上上限（此时显示的数字是不完整的）
    pub fn search_truncated(&self) -> bool {
        self.search_truncated
    }

    fn apply_hits(&mut self, hits: Vec<usize>) {
        self.search_truncated = hits.len() >= MAX_SEARCH_HITS;
        self.matches.clear();
        let mut last: Option<usize> = None;
        for pos in hits {
            let idx = match self.line_starts.binary_search(&pos) {
                Ok(i) => i,
                Err(0) => 0,
                Err(i) => i - 1,
            };
            if Some(idx) != last {
                self.matches.push(idx as u32);
                last = Some(idx);
            }
        }
    }
}

fn map_file(file: &File) -> Result<Mmap> {
    let len = file.metadata()?.len() as usize;
    if len == 0 {
        // mmap 不允许零长度映射，用一字节匿名映射占位。
        // 真实长度由 real_len 记录，不会因此多出一行。
        let m = MmapOptions::new().len(1).map_anon()?;
        return Ok(m.make_read_only()?);
    }
    Ok(unsafe { Mmap::map(file)? })
}

fn trim_eol(b: &[u8]) -> &[u8] {
    let mut e = b.len();
    while e > 0 && (b[e - 1] == b'\n' || b[e - 1] == b'\r') {
        e -= 1;
    }
    &b[..e]
}

fn scan_lines(mmap: Arc<Mmap>, start: usize, limit: usize, tx: Sender<IndexMsg>) {
    let limit = limit.min(mmap.len());
    let mut i = start.min(limit);
    let mut batch: Vec<usize> = Vec::with_capacity(INDEX_CHUNK);
    while i < limit {
        let Some(rel) = memchr::memchr(b'\n', &mmap[i..limit]) else {
            break;
        };
        batch.push(i);
        i += rel + 1;
        if batch.len() >= INDEX_CHUNK {
            let b = std::mem::replace(&mut batch, Vec::with_capacity(INDEX_CHUNK));
            if tx.send(IndexMsg::Lines(b)).is_err() {
                return;
            }
        }
    }
    if !batch.is_empty() {
        let _ = tx.send(IndexMsg::Lines(batch));
    }
    let _ = tx.send(IndexMsg::Done(i));
}

fn search_bytes(mmap: &[u8], limit: usize, pattern: &[u8]) -> Vec<usize> {
    let finder = memchr::memmem::Finder::new(pattern);
    let mut hits = Vec::new();
    for pos in finder.find_iter(&mmap[..limit]) {
        hits.push(pos);
        if hits.len() >= MAX_SEARCH_HITS {
            break;
        }
    }
    hits
}

fn search_bytes_ci(mmap: &[u8], limit: usize, pattern: &[u8]) -> Vec<usize> {
    let needle = pattern.to_ascii_lowercase();
    let finder = memchr::memmem::Finder::new(&needle);
    let mut hits = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while i < limit {
        let end = match memchr::memchr(b'\n', &mmap[i..limit]) {
            Some(rel) => i + rel,
            None => limit,
        };
        buf.clear();
        buf.extend_from_slice(&mmap[i..end]);
        buf.make_ascii_lowercase();
        for m in finder.find_iter(&buf) {
            hits.push(i + m);
            if hits.len() >= MAX_SEARCH_HITS {
                return hits;
            }
        }
        i = end + 1;
    }
    hits
}

/// 编码嗅探：优先 UTF-8，失败回退 GB18030（覆盖 Windows 中文环境的 GBK 日志）
fn sniff_encoding(data: &[u8], len: usize) -> &'static Encoding {
    let head = &data[..len.min(64 * 1024)];
    if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return UTF_8;
    }
    if std::str::from_utf8(head).is_ok() {
        return UTF_8;
    }
    GB18030
}

pub fn is_utf8(e: &'static Encoding) -> bool {
    std::ptr::eq(e, UTF_8)
}

/// 供 UI 显示的编码名（sniff 只会得出 UTF-8 或 GB18030）
pub fn encoding_name(e: &'static Encoding) -> &'static str {
    if is_utf8(e) {
        "UTF-8"
    } else {
        "GB18030"
    }
}
