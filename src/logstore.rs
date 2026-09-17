use anyhow::{Context as _, Result};
use encoding_rs::{Encoding, GB18030, UTF_8};
use memmap2::{Mmap, MmapOptions};
use std::fs::File;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// 索引线程每次回传的行数
const INDEX_CHUNK: usize = 20_000;
/// 最多保留多少个**命中行**的位置（可跳转的上限）。
///
/// 单位是"行"而不是"命中次数"：跳转的单位本来就是行，一行里同一个词出现三次
/// 记三份位置没有一点用，却会让内存随出现次数膨胀——正是这一点让常见词只覆盖到
/// 文件前一小段。按行记之后每命中行只要 8 字节（暂存）+ 4 字节（常驻）：
/// 49 MB / 29.7 万行的日志，最狠的单字母检索也只占 2.3 MB + 1.1 MB。
///
/// 超出的部分照常统计总数，只是无法跳转（`search_truncated` 会报出来）。
pub const MAX_NAVIGABLE_LINES: usize = 1_000_000;

pub enum IndexMsg {
    /// 一批行起始偏移
    Lines(Vec<usize>),
    /// 本轮扫描结束，参数为「下一条待确认行的起始偏移」
    Done(usize),
}

/// 一次检索的原始结果。
///
/// `positions` 每组命中行只保留第一处的位置（用于跳转），最多 [`MAX_NAVIGABLE_LINES`] 行；
/// `total` 是全文件的命中总数（子串检索按出现次数算，正则检索按命中行数算），用于如实报数。
///
/// 注意 `total` 与 `positions.len()` 不是一回事：一行里出现三次，总数算三、位置只留一份。
/// 所以"有没有被截断"不能靠两者比大小，要单独记（`dropped`）。
#[derive(Default)]
pub struct SearchHits {
    positions: Vec<usize>,
    total: usize,
    dropped: bool,
}

impl SearchHits {
    /// 记一个命中行：总数累加，位置在未达上限时保留
    fn push(&mut self, pos: usize) {
        self.total += 1;
        if self.positions.len() < MAX_NAVIGABLE_LINES {
            self.positions.push(pos);
        } else {
            self.dropped = true;
        }
    }

    /// 同一行里除第一处之外的命中：只计数，不留位置
    fn count_only(&mut self) {
        self.total += 1;
    }

    /// 是否有命中行因为上限而没能保留
    fn truncated(&self) -> bool {
        self.dropped
    }
}

pub enum SearchMsg {
    /// 命中位置（升序，可能被上限截断）与全文件命中总数
    Hits(SearchHits),
    /// 正则表达式编译失败
    RegexError(String),
}

/// 一次检索的请求参数
#[derive(Clone, Debug)]
pub struct SearchRequest {
    pub query: String,
    pub case_sensitive: bool,
    pub use_regex: bool,
}

impl SearchRequest {
    pub fn new(query: impl Into<String>, case_sensitive: bool, use_regex: bool) -> Self {
        Self {
            query: query.into(),
            case_sensitive,
            use_regex,
        }
    }
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
    /// 命中数是否撞上保留上限（此时可跳转的命中不完整，但总数是准的）
    search_truncated: bool,
    /// 上一次检索的全文件命中总数，不受保留上限影响
    search_total: usize,
    /// 索引尚未建完时挂起的检索条件，索引完成后自动执行
    pending_search: Option<SearchRequest>,
    /// 上一次正则检索的编译错误
    regex_error: Option<String>,
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
            search_total: 0,
            pending_search: None,
            regex_error: None,
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

    /// 第 idx 行的起始字节偏移。索引还没覆盖到这一行时返回 None。
    ///
    /// 标记（书签）用它把"行"翻译成"字节偏移"——偏移才是能跨会话存下来的锚点。
    pub fn line_offset(&self, idx: usize) -> Option<usize> {
        self.line_starts
            .get(idx)
            .copied()
            .filter(|&o| o < self.real_len)
    }

    /// 某个字节偏移落在第几行（0 基）。索引尚未覆盖到那个位置时返回 None ——
    /// 调用方据此知道"现在还算不出来"，而不是拿到一个错的行号。
    pub fn line_index_at(&self, offset: usize) -> Option<usize> {
        if offset >= self.scanned {
            return None;
        }
        self.line_starts
            .partition_point(|&s| s <= offset)
            .checked_sub(1)
    }

    /// 取 offset 所在那一行的解码文本。
    ///
    /// **不依赖行索引**：直接往前找最近的换行定行首、往后找换行定行尾。标记的校验
    /// 必须走这条路——校验可能发生在索引还没建完的时候（大日志刚打开那一瞬），
    /// 也可能发生在偏移已经不在行首的位置上（文件被改过）。
    pub fn line_text_at(&self, offset: usize) -> Option<String> {
        let bytes = self.mmap.get(..self.real_len)?;
        if offset >= bytes.len() {
            return None;
        }
        let start = memchr::memrchr(b'\n', &bytes[..offset])
            .map(|i| i + 1)
            .unwrap_or(0);
        let end = memchr::memchr(b'\n', &bytes[offset..])
            .map(|i| offset + i)
            .unwrap_or(bytes.len());
        let mut out = String::new();
        decode_into(trim_eol(&bytes[start..end]), self.encoding, &mut out);
        Some(out)
    }

    /// 在整个文件里按**本文件的编码**找一段文本，返回起始字节偏移。
    ///
    /// 给"按原文回找"用：标记失效（文件被轮转/重写）之后，拿当初记下的那行文字
    /// 在现在的文件里再找一次，找到就把锚点挪过去——比让人自己重搜一遍省事。
    pub fn find_offset_of(&self, needle: &str) -> Option<usize> {
        if needle.is_empty() {
            return None;
        }
        let bytes = self.mmap.get(..self.real_len)?;
        let (encoded, _, _) = self.encoding.encode(needle);
        memchr::memmem::find(bytes, &encoded)
    }

    /// 把第 idx 行解码后追加到 out（复用缓冲，避免每行分配）
    pub fn read_line_into(&self, idx: usize, out: &mut String) -> bool {
        let Some((s, e)) = self.line_range(idx) else {
            return false;
        };
        if s > e || e > self.mmap.len() {
            return false;
        }
        decode_into(trim_eol(&self.mmap[s..e]), self.encoding, out);
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
                // 索引刚建完，补跑此前挂起的检索
                if let Some(req) = self.pending_search.take() {
                    self.spawn_search(req);
                }
                changed = true;
            }
        }
        if let Some(rx) = &self.search_rx {
            if let Ok(msg) = rx.try_recv() {
                match msg {
                    SearchMsg::Hits(hits) => self.apply_hits(hits),
                    SearchMsg::RegexError(e) => self.regex_error = Some(e),
                }
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
            // 被截断或轮转，整体重建（旧结果与挂起条件都随之失效）
            self.line_starts.clear();
            self.scanned = 0;
            self.matches.clear();
            self.search_truncated = false;
            self.search_total = 0;
            self.pending_search = None;
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

    /// 启动一次后台检索，返回是否真的启动了。
    ///
    /// 若行索引尚未建完，检索会先挂起，待索引完成后自动执行：
    /// 此时文件只有一部分被扫描过，直接检索会给出偏少的命中数，
    /// 而界面上看不出任何异常——那比"慢一点出结果"危险得多。
    pub fn start_search(&mut self, req: SearchRequest) -> bool {
        if req.query.is_empty() {
            self.clear_search();
            return false;
        }
        if self.indexing {
            self.pending_search = Some(req);
            self.searching = true;
            return true;
        }
        self.spawn_search(req)
    }

    /// 是否有检索正在等待索引完成
    pub fn search_pending(&self) -> bool {
        self.pending_search.is_some()
    }

    /// 上一次正则检索的编译错误，供界面提示
    pub fn regex_error(&self) -> Option<&str> {
        self.regex_error.as_deref()
    }

    fn spawn_search(&mut self, req: SearchRequest) -> bool {
        let (tx, rx) = channel::<SearchMsg>();
        let mmap = self.mmap.clone();
        // 索引已完成，此时可覆盖整个文件（含末尾无换行符的最后一行）
        let limit = self.real_len;
        let encoding = self.encoding;
        self.search_rx = Some(rx);
        self.searching = true;
        self.pending_search = None;
        self.regex_error = None;
        std::thread::spawn(move || {
            let msg = if req.use_regex {
                // 忽略大小写交给正则自己的标志位处理
                let pattern = if req.case_sensitive {
                    req.query.clone()
                } else {
                    format!("(?i){}", req.query)
                };
                match regex::Regex::new(&pattern) {
                    Ok(re) => SearchMsg::Hits(search_regex(&mmap, limit, &re, encoding)),
                    Err(e) => SearchMsg::RegexError(e.to_string()),
                }
            } else {
                let pattern: Vec<u8> = if is_utf8(encoding) {
                    req.query.as_bytes().to_vec()
                } else {
                    encoding.encode(&req.query).0.into_owned()
                };
                let hits = if req.case_sensitive {
                    search_bytes(&mmap, limit, &pattern)
                } else {
                    search_bytes_ci(&mmap, limit, &pattern)
                };
                SearchMsg::Hits(hits)
            };
            let _ = tx.send(msg);
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
        self.search_total = 0;
        self.pending_search = None;
        self.regex_error = None;
    }

    /// 是否有命中因为保留上限而无法跳转（总数仍然准确）
    pub fn search_truncated(&self) -> bool {
        self.search_truncated
    }

    /// 上一次检索的全文件命中总数。子串检索按出现次数算，正则检索按命中行数算。
    pub fn search_total(&self) -> usize {
        self.search_total
    }

    fn apply_hits(&mut self, hits: SearchHits) {
        self.search_truncated = hits.truncated();
        self.search_total = hits.total;
        self.matches.clear();
        // 位置与行起始都是升序，一次线性归并就够，不必对每个位置二分：
        // 上限提到百万行之后，逐位二分会成为检索完成后的主要开销。
        let mut line = 0usize;
        for pos in hits.positions {
            while line + 1 < self.line_starts.len() && self.line_starts[line + 1] <= pos {
                line += 1;
            }
            if self.matches.last() != Some(&(line as u32)) {
                self.matches.push(line as u32);
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

fn search_bytes(mmap: &[u8], limit: usize, pattern: &[u8]) -> SearchHits {
    let finder = memchr::memmem::Finder::new(pattern);
    let mut hits = SearchHits::default();
    let mut previous: Option<usize> = None;
    for pos in finder.find_iter(&mmap[..limit]) {
        // 与上一处命中之间没有换行，说明还在同一行：只计数，不再留位置
        let same_line = previous.is_some_and(|p| memchr::memchr(b'\n', &mmap[p..pos]).is_none());
        if same_line {
            hits.count_only();
        } else {
            hits.push(pos);
        }
        previous = Some(pos);
    }
    hits
}

fn search_bytes_ci(mmap: &[u8], limit: usize, pattern: &[u8]) -> SearchHits {
    let needle = pattern.to_ascii_lowercase();
    let finder = memchr::memmem::Finder::new(&needle);
    let mut hits = SearchHits::default();
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
        let mut first_in_line = true;
        for m in finder.find_iter(&buf) {
            if first_in_line {
                hits.push(i + m);
                first_in_line = false;
            } else {
                hits.count_only();
            }
        }
        i = end + 1;
    }
    hits
}

/// 正则检索：逐行解码之后再匹配，返回命中的行起始偏移。
///
/// 没有像普通检索那样直接匹配原始字节，原因是正则表达式本身是 UTF-8 文本，
/// 而 GB18030 日志里的汉字是多字节的，在字节流上匹配无法正确对应。
/// 逐行解码慢一些，但换来编码无关——中文正则也能用。
fn search_regex(
    mmap: &[u8],
    limit: usize,
    re: &regex::Regex,
    encoding: &'static Encoding,
) -> SearchHits {
    let mut hits = SearchHits::default();
    let mut line = String::with_capacity(256);
    let mut i = 0usize;
    while i < limit {
        let end = match memchr::memchr(b'\n', &mmap[i..limit]) {
            Some(rel) => i + rel,
            None => limit,
        };
        line.clear();
        decode_into(trim_eol(&mmap[i..end]), encoding, &mut line);
        if re.is_match(&line) {
            hits.push(i);
        }
        i = end + 1;
    }
    hits
}

/// 按文件编码把一段原始字节解码后追加到 out
fn decode_into(raw: &[u8], encoding: &'static Encoding, out: &mut String) {
    if is_utf8(encoding) {
        match std::str::from_utf8(raw) {
            Ok(t) => out.push_str(t),
            Err(_) => out.push_str(&String::from_utf8_lossy(raw)),
        }
    } else {
        let (cow, _, _) = encoding.decode(raw);
        out.push_str(&cow);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 上限的算术：位置留到上限为止，总数继续累加；正好到上限不算截断。
    #[test]
    fn hits_beyond_the_cap_keep_counting() {
        let mut hits = SearchHits::default();
        for i in 0..MAX_NAVIGABLE_LINES {
            hits.push(i);
        }
        assert_eq!(hits.positions.len(), MAX_NAVIGABLE_LINES);
        assert!(!hits.truncated(), "正好到上限时不该算截断");

        hits.push(MAX_NAVIGABLE_LINES);
        hits.count_only();
        assert_eq!(hits.positions.len(), MAX_NAVIGABLE_LINES, "位置不再增长");
        assert_eq!(hits.total, MAX_NAVIGABLE_LINES + 2, "总数照常累加");
        assert!(hits.truncated(), "超出上限后应报截断");
    }
}
