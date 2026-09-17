use crate::bookmarks::{self, Bookmark};
use crate::logstore::{encoding_name, LogStore, SearchRequest};
use eframe::egui;
use egui::{Color32, FontId, RichText, ScrollArea, TextEdit, TextStyle};
use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 单行最大渲染字符数，超出截断，避免超长行拖慢渲染
const MAX_RENDER_CHARS: usize = 4000;

/// 搜索框的固定 Id，用于快捷键聚焦与失焦
const SEARCH_ID: &str = "logview_search";

/// 界面提示里修饰键的写法。
///
/// egui 的 `Modifiers::COMMAND` 是逻辑修饰键：macOS 上是 ⌘，其他平台是 Ctrl，
/// 界面文案得跟着平台走，否则 Windows 用户看到 ⌘ 会不知道按什么。
const CMD: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl+"
};

/// 检索结果到位后，视图往哪里去。
///
/// 两种入口的期望不一样：在搜索框里敲词是想"找到它"，跳过去理所当然；
/// 双击行内的词是想"看看这个词还出现在哪"，人正读到半截，跳走等于把人拽走。
#[derive(Clone, Copy, PartialEq, Eq)]
enum OnSearchDone {
    /// 跳到全文件的第一个命中（搜索框输入、切换检索选项）
    GotoFirstMatch,
    /// 视图原地不动，只把游标对齐到当前视口（双击取词）
    StayAtView,
}

pub struct LogViewApp {
    store: Option<LogStore>,
    query: String,
    case_sensitive: bool,
    /// 把检索词当作正则表达式
    use_regex: bool,
    /// 正则模式下用于行内高亮的已编译表达式（编译失败时为 None）
    highlight_regex: Option<regex::Regex>,
    only_matched: bool,
    /// 跟随尾部（等价 tail -f）。默认关闭：打开日志先停在开头，
    /// 需要盯实时输出时再勾上。
    follow: bool,
    wrap: bool,
    dark: Option<bool>,
    error: Option<String>,

    /// 搜索防抖
    search_dirty: Option<Instant>,
    /// 当前跳转目标行
    pending_jump: Option<usize>,
    /// 检索结果到位后的去向；None 表示这一轮已经处理过
    pending_after_search: Option<OnSearchDone>,
    /// 匹配项游标（用于上一个/下一个）
    match_cursor: usize,
    /// 纵向滚动偏移，手动维护以便跳转
    scroll_offset: f32,
    /// 视口顶部对应的文件行号，双击取词后用它把游标对齐到当前位置
    top_line: usize,

    /// 搜索框当前是否持有焦点（决定裸字母键是否作为快捷键）
    search_has_focus: bool,
    /// 请求在下一帧把焦点交给搜索框
    focus_search: bool,
    /// 请求跳到文件末尾（需在得知总行数后处理）
    jump_to_end: bool,

    /// 标记（书签）。锚点是字节偏移 + 内容指纹，见 `bookmarks` 模块
    marks: Vec<Mark>,
    /// 侧车文件的路径。日志目录不可写时为 None——那时标记只存在于本次会话
    marks_path: Option<PathBuf>,
    /// 有改动待写盘（改动即写，不做"保存"按钮）
    marks_dirty: bool,
    /// 写侧车失败的原因，显示在面板脚上；不弹窗，因为标记本身还能用
    marks_warning: Option<String>,
    /// 标记面板是否展开
    show_marks: bool,
    /// 正在编辑备注的那条标记（按偏移定位）；Some 期间快捷键整体让位给输入框
    editing_note: Option<u64>,
    /// 备注编辑框的内容
    note_buf: String,
    /// "当前行"：标记与跳转的目标。取最近一次定位落点，没有就取视口第一行
    cursor_line: Option<usize>,
    /// 最近一次从标记面板或 `F2` 跳到的**文件行号**，在日志区里铺一层标记色底。
    ///
    /// 左缘那个紫点只有 4px 宽，跳过去之后在一屏几十行里并不好找。
    /// 任何别的定位动作（搜索跳转、`g`/`G`、点行号）都会把它清掉：
    /// 它表达的是"你刚点了哪一条标记"，不随时间累积成一片紫底。
    focused_mark: Option<usize>,
    /// 视口能装下多少行（由 log_area 每帧记录）：判断"当前行还看不看得见"要用它
    visible_rows: usize,
    /// 状态栏上的一次性提示（标记了第几行之类），几秒后自己消失
    flash: Option<(String, Instant)>,

    last_refresh: Instant,
    buf: String,
}

/// 一条标记加上它此刻的定位状态。
///
/// 状态是**加载时按指纹现算的**，不存进侧车文件：文件随时可能被轮转、被追加，
/// 存下来的"当时对不对"没有任何意义。
#[derive(Clone, Debug)]
struct Mark {
    bm: Bookmark,
    anchor: AnchorState,
}

/// 锚点此刻是否还对得上
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AnchorState {
    /// 指纹一致，可以直接跳过去
    Fresh,
    /// 偏移处的内容变了：不跳，避免静默落在错的行上
    Stale,
    /// 偏移已经超出文件长度（文件变小了）
    Lost,
}

/// 标记面板上能触发的动作。
///
/// 收集起来等 UI 走完再执行：面板上每个动作都要改 `self`，而 UI 闭包同时还在读
/// `self` 的别的字段，直接写会绕进借用冲突。
#[derive(Clone, Copy)]
enum MarkAct {
    Goto(u64),
    Delete(u64),
    StartNote(u64),
    CommitNote,
    RefindAll,
    DropStale,
    Export,
    Hide,
}

/// 面板上一条标记要显示的内容。先算好成一份只读快照，UI 里就只管画。
struct MarkRow {
    offset: u64,
    /// 现在落在第几行；索引还没覆盖到那个位置时为 None
    line: Option<usize>,
    head: String,
    note: String,
    /// 标记时间，显示成"3 天前"
    time: String,
    state: AnchorState,
}

impl Default for LogViewApp {
    fn default() -> Self {
        Self {
            store: None,
            query: String::new(),
            case_sensitive: false,
            use_regex: false,
            highlight_regex: None,
            only_matched: false,
            follow: false,
            wrap: false,
            dark: None,
            error: None,
            search_dirty: None,
            pending_jump: None,
            pending_after_search: None,
            match_cursor: 0,
            scroll_offset: 0.0,
            top_line: 0,
            search_has_focus: false,
            focus_search: false,
            jump_to_end: false,
            marks: Vec::new(),
            marks_path: None,
            marks_dirty: false,
            marks_warning: None,
            show_marks: false,
            editing_note: None,
            note_buf: String::new(),
            cursor_line: None,
            focused_mark: None,
            visible_rows: 1,
            flash: None,
            last_refresh: Instant::now(),
            buf: String::with_capacity(1024),
        }
    }
}

impl LogViewApp {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open_path(&mut self, path: PathBuf) {
        match LogStore::open(path) {
            Ok(mut s) => {
                if !self.query.is_empty() {
                    s.start_search(self.search_request());
                }
                self.highlight_regex = self.compiled_highlight_regex();
                self.scroll_offset = 0.0;
                self.match_cursor = 0;
                self.store = Some(s);
                self.error = None;
                // store 就位之后才能按指纹核对侧车里的标记
                self.load_marks();
            }
            Err(e) => self.error = Some(format!("{e}")),
        }
    }

    /// 执行检索，并记下结果到位后视图的去向。
    fn run_search(&mut self, on_done: OnSearchDone) {
        // 后台线程会独立编译一次用于扫描整个文件；这里再编译一份供行内高亮使用
        self.highlight_regex = self.compiled_highlight_regex();
        // 先取出请求，避免与 &mut self.store 的借用冲突
        let req = self.search_request();
        if let Some(s) = &mut self.store {
            if s.start_search(req) {
                self.pending_after_search = Some(on_done);
            }
        }
        if on_done == OnSearchDone::GotoFirstMatch {
            self.match_cursor = 0;
        }
    }

    /// 检索结束后决定视图去向。每帧调用一次，没在等结果时什么都不做。
    fn apply_search_outcome(&mut self) {
        if self.store.as_ref().map(|s| s.searching).unwrap_or(false) {
            return;
        }
        match self.pending_after_search.take() {
            Some(OnSearchDone::GotoFirstMatch) => self.goto_match(0),
            Some(OnSearchDone::StayAtView) => self.cursor_from_view(),
            None => {}
        }
    }

    /// 把游标对齐到当前视口：取视口之下的第一个命中作为"当前项"。
    ///
    /// 视图一动不动，但游标落在一个看得到的位置上，于是 `n` 是"往下找下一个命中"，
    /// `N` 是"往上找上一个"，而不是从文件开头重新数。
    fn cursor_from_view(&mut self) {
        let Some(s) = &self.store else { return };
        let matches = s.matches();
        if matches.is_empty() {
            return;
        }
        let top = self.top_line as u32;
        self.match_cursor = matches
            .partition_point(|&line| line < top)
            .min(matches.len() - 1);
    }

    /// 当前检索条件
    fn search_request(&self) -> SearchRequest {
        SearchRequest::new(&self.query, self.case_sensitive, self.use_regex)
    }

    /// 正则模式下编译一次用于高亮；编译失败返回 None，
    /// 具体错误由 logstore 在后台报告给状态栏。
    fn compiled_highlight_regex(&self) -> Option<regex::Regex> {
        if !self.use_regex || self.query.is_empty() {
            return None;
        }
        let pattern = if self.case_sensitive {
            self.query.clone()
        } else {
            format!("(?i){}", self.query)
        };
        regex::Regex::new(&pattern).ok()
    }

    /// 当前匹配项所在的行号（用于高亮整行）
    fn current_match_line(&self) -> Option<usize> {
        let s = self.store.as_ref()?;
        s.matches().get(self.match_cursor).map(|v| *v as usize)
    }

    /// 用给定的词直接发起检索（双击行内文字时调用）。
    ///
    /// 不抢搜索框焦点：连着双击几个词追查线索时，焦点留在日志区更顺手。
    /// 也不移动视图：人正读到半截，跳回全文件第一个命中等于把人拽走。
    fn search_for(&mut self, word: String) {
        if word.is_empty() {
            return;
        }
        self.query = word;
        self.run_search(OnSearchDone::StayAtView);
    }

    fn handle_drop(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(f) = dropped.into_iter().next() {
            if let Some(p) = f.path {
                self.open_path(p);
            }
        }
    }

    fn goto_match(&mut self, delta: isize) {
        let Some(s) = &self.store else { return };
        let n = s.matches().len();
        if n == 0 {
            return;
        }
        // 手动跳转时退出跟随，否则会被 tail 拉回底部
        self.follow = false;
        // 视线已经移到命中上了，标记那层高亮该收掉（留在别处的紫底没有来由）
        self.focused_mark = None;
        let cur = self.match_cursor as isize + delta;
        let cur = cur.clamp(0, n as isize - 1) as usize;
        self.match_cursor = cur;
        let line = s.matches()[cur] as usize;
        // 跳到哪里，"当前行"就在哪里：于是跳完直接按 b 标的就是这一行
        self.cursor_line = Some(line);
        if self.only_matched {
            self.pending_jump = Some(cur);
        } else {
            self.pending_jump = Some(line);
        }
    }

    /// 跳到指定位置（同时退出跟随，否则会被 tail 拉回底部）
    fn jump_to_line(&mut self, idx: usize) {
        self.follow = false;
        // 跳到别处去了（g / G），标记那层高亮收掉
        self.focused_mark = None;
        self.pending_jump = Some(idx);
    }

    // ── 标记（书签） ───────────────────────────────────────────────
    //
    // 锚点怎么存、侧车什么格式、失效怎么判，都在 `bookmarks` 模块里；
    // 这里只管交互与呈现。一条原则贯穿始终：**绝不静默跳到错的行上**——
    // 指纹对不上就标成失效并且不跳，同时给出"按原文回找"这个补救动作。

    /// 标记/跳转的目标行。
    ///
    /// 光标还在屏幕里就用它（"搜索跳转 → 按 b 标下这条"是最常见的用法），
    /// 否则用视口第一行。**不能无条件信任光标**：搜到一处、又往下翻了 20 屏之后
    /// 按 b，如果还标在那个看不见的旧位置上，用户只会觉得"按了没反应"。
    /// 两者都是**文件行号**（过滤视图下 log_area 已把列表位置换算回来了）。
    fn mark_target(&self) -> Option<usize> {
        self.store.as_ref()?;
        let top = self.top_line;
        let bottom = top + self.visible_rows.max(1);
        match self.cursor_line {
            Some(line) if line >= top && line < bottom => Some(line),
            _ => Some(top),
        }
    }

    /// 这条标记现在落在第几行。索引还没覆盖到时退回记录时的行号——可能已经漂了，
    /// 但比"没有行号"强。
    fn mark_line(&self, m: &Mark) -> usize {
        self.store
            .as_ref()
            .and_then(|s| s.line_index_at(m.bm.offset as usize))
            .unwrap_or(m.bm.line as usize)
    }

    /// 状态栏上的一次性提示
    fn flash(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), Instant::now()));
    }

    /// `b`：标记 / 取消当前行
    fn toggle_mark(&mut self) {
        let Some(line) = self.mark_target() else {
            return;
        };

        // 先把要用的数据取出来，再改 self——否则 &self.store 与 &mut self.marks 打架
        let prepared = {
            let Some(s) = &self.store else { return };
            match s.line_offset(line) {
                Some(offset) => {
                    let mut buf = String::new();
                    if s.read_line_into(line, &mut buf) {
                        Some((offset, buf))
                    } else {
                        None
                    }
                }
                None => None,
            }
        };
        let Some((offset, text)) = prepared else {
            self.flash("这一行还没进索引，稍后再试");
            return;
        };

        match self
            .marks
            .iter()
            .position(|m| m.bm.offset as usize == offset)
        {
            Some(pos) => {
                self.marks.remove(pos);
                self.marks_dirty = true;
                // 取消掉的正是高亮那一条，收掉紫底
                if self.focused_mark == Some(line) {
                    self.focused_mark = None;
                }
                self.flash(format!("已取消第 {} 行的标记", line + 1));
            }
            None => {
                self.marks.push(Mark {
                    bm: Bookmark::new(offset, line, &text, bookmarks::now_secs()),
                    anchor: AnchorState::Fresh,
                });
                self.marks.sort_by_key(|m| m.bm.offset);
                self.marks_dirty = true;
                self.cursor_line = Some(line);
                // 刚标的这条就是"当前那条"：铺上紫底，一眼看清标在了第几行
                self.focused_mark = Some(line);
                let n = self.marks.len();
                self.flash(format!("已标记第 {} 行（共 {n} 条）", line + 1));
            }
        }
    }

    /// 打开文件时把侧车里的标记读回来，并按当前内容校验每条锚点。
    fn load_marks(&mut self) {
        self.marks.clear();
        self.marks_warning = None;
        self.editing_note = None;
        self.cursor_line = None;
        self.focused_mark = None;
        self.marks_dirty = false;

        let Some(path) = self
            .store
            .as_ref()
            .map(|s| bookmarks::sidecar_path(&s.path))
        else {
            self.marks_path = None;
            return;
        };
        self.marks_path = Some(path.clone());
        self.marks = bookmarks::load(&path)
            .into_iter()
            .map(|bm| Mark {
                bm,
                anchor: AnchorState::Fresh,
            })
            .collect();
        self.validate_marks();

        // 上次留下的标记自动展开面板：藏在按钮后面等于没记住
        self.show_marks = !self.marks.is_empty();
        if !self.marks.is_empty() {
            let n = self.marks.len();
            let stale = self
                .marks
                .iter()
                .filter(|m| m.anchor != AnchorState::Fresh)
                .count();
            if stale == 0 {
                self.flash(format!("读回 {n} 条标记"));
            } else {
                self.flash(format!("读回 {n} 条标记，其中 {stale} 条已失效"));
            }
        }
    }

    /// 按指纹核对每条锚点。
    ///
    /// 走 `line_text_at`（直接按字节找行首行尾）而不是行索引：校验可能发生在索引
    /// 还没建完的时候（大日志刚打开那一瞬），而"标记还对不对"这件事不该等索引。
    fn validate_marks(&mut self) {
        let Some(s) = &self.store else {
            for m in &mut self.marks {
                m.anchor = AnchorState::Lost;
            }
            return;
        };
        for m in &mut self.marks {
            m.anchor = match s.line_text_at(m.bm.offset as usize) {
                None => AnchorState::Lost,
                Some(text) if m.bm.matches_head(&text) => AnchorState::Fresh,
                Some(_) => AnchorState::Stale,
            };
        }
    }

    /// 按原文回找：拿记下的行首文字在当前文件里再找一次，把锚点挪过去。
    ///
    /// 文件被轮转、被重写之后，这是把标记救回来的唯一办法——比让人自己重搜一遍省事。
    /// `only` 为 Some 时只处理那一条。
    fn refind_marks(&mut self, only: Option<u64>) {
        let Some(s) = &self.store else { return };
        let mut moved = 0usize;
        let mut missed = 0usize;
        for m in &mut self.marks {
            if only.is_some_and(|o| o != m.bm.offset) || m.anchor == AnchorState::Fresh {
                continue;
            }
            if m.bm.head.is_empty() {
                // 空行的指纹是空的，回找无从下手
                missed += 1;
                continue;
            }
            match s.find_offset_of(&m.bm.head) {
                Some(offset) => {
                    m.bm.offset = offset as u64;
                    m.anchor = match s.line_text_at(offset) {
                        Some(text) if m.bm.matches_head(&text) => AnchorState::Fresh,
                        _ => AnchorState::Stale,
                    };
                    if let Some(idx) = s.line_index_at(offset) {
                        m.bm.line = idx as u32;
                    }
                    moved += 1;
                }
                None => missed += 1,
            }
        }
        // 回找可能把两条挤到同一行上，去个重
        self.marks.sort_by_key(|m| m.bm.offset);
        self.marks.dedup_by_key(|m| m.bm.offset);
        // 行号刚被改写，之前记下的那个已经指不准了：收掉高亮，让用户重新点一条
        self.focused_mark = None;
        self.marks_dirty = true;
        self.flash(format!("回找完成：重定位 {moved} 条，未找到 {missed} 条"));
    }

    fn drop_stale_marks(&mut self) {
        let before = self.marks.len();
        self.marks.retain(|m| m.anchor == AnchorState::Fresh);
        let dropped = before - self.marks.len();
        if dropped > 0 {
            // 清掉的可能正是高亮那一条，收了稳妥
            self.focused_mark = None;
            self.marks_dirty = true;
            self.flash(format!("清掉 {dropped} 条失效标记"));
        }
    }

    /// 跳到某个文件行。
    ///
    /// 过滤视图里"列表位置"与"文件行号"不是一回事：那行不在结果里就先把过滤关掉，
    /// 否则跳过去落在列表里另一个位置，看着就像跳错了。
    fn jump_to_file_line(&mut self, line: usize) {
        let mut target = line;
        if self.only_matched {
            let pos = self
                .store
                .as_ref()
                .and_then(|s| s.matches().binary_search(&(line as u32)).ok());
            match pos {
                Some(i) => target = i,
                None => {
                    self.only_matched = false;
                    self.flash("已关闭「只显示匹配行」以显示这条标记");
                }
            }
        }
        self.follow = false;
        self.cursor_line = Some(line);
        // 这一行铺标记色底：面板里点一条、或 F2 跳一条，都要能在日志区一眼认出落在哪
        self.focused_mark = Some(line);
        self.pending_jump = Some(target);
    }

    /// `F2` / `⇧F2`：在标记之间前后跳。到底了绕回另一端——标记数量少，环绕比
    /// "到头不动"更符合直觉。
    fn goto_mark(&mut self, delta: isize) {
        let mut lines: Vec<usize> = self
            .marks
            .iter()
            .filter(|m| m.anchor == AnchorState::Fresh)
            .map(|m| self.mark_line(m))
            .collect();
        if lines.is_empty() {
            if self.marks.is_empty() {
                self.flash("还没有标记");
            } else {
                self.flash("没有可定位的标记（都失效了，可以试「按原文回找」）");
            }
            return;
        }
        lines.sort_unstable();
        let from = self.cursor_line.unwrap_or(self.top_line);
        let target = if delta >= 0 {
            lines
                .iter()
                .find(|&&l| l > from)
                .copied()
                .unwrap_or(lines[0])
        } else {
            lines
                .iter()
                .rev()
                .find(|&&l| l < from)
                .copied()
                .unwrap_or(*lines.last().unwrap())
        };
        self.jump_to_file_line(target);
    }

    /// 删掉一条标记（面板条目上的 `×`）。
    ///
    /// 删的正好是高亮那一条时，把高亮一起收掉——否则日志区里留下一行没有标记的紫底。
    fn remove_mark(&mut self, offset: u64) {
        let hit = self
            .marks
            .iter()
            .find(|m| m.bm.offset == offset)
            .map(|m| self.mark_line(m));
        if hit == self.focused_mark {
            self.focused_mark = None;
        }
        self.marks.retain(|m| m.bm.offset != offset);
        self.marks_dirty = true;
        self.flash("已删除该标记");
    }

    /// 点行号：把"当前行"设过去，之后按 `b`、写备注都对着它。
    ///
    /// 同时收掉标记那层高亮——这是"选光标"，不是"选标记"，
    /// 留着紫底会让人以为还停在面板里点的那条上。
    fn select_line(&mut self, line: usize) {
        self.cursor_line = Some(line);
        self.focused_mark = None;
    }

    /// 把编辑框里的备注落到那一条标记上。抽出来是为了能直接断言——
    /// 塞在面板的"失去焦点"分支里就只能靠模拟点击去测。
    fn commit_note(&mut self) {
        if let Some(offset) = self.editing_note.take() {
            let text = std::mem::take(&mut self.note_buf);
            if let Some(m) = self.marks.iter_mut().find(|m| m.bm.offset == offset) {
                m.bm.note = text;
            }
            self.marks_dirty = true;
        }
    }

    /// 把标记写回侧车文件。改动即写，没有"保存"按钮。
    fn save_marks(&mut self) {
        if !self.marks_dirty {
            return;
        }
        let Some(path) = self.marks_path.clone() else {
            self.marks_dirty = false;
            return;
        };
        let items: Vec<Bookmark> = self.marks.iter().map(|m| m.bm.clone()).collect();
        match bookmarks::save(&path, &items) {
            Ok(()) => self.marks_dirty = false,
            Err(e) => {
                // 写不进去就退回"只在本次会话有效"。不弹窗——标记本身还能用，
                // 但面板脚上必须说清楚，否则用户会以为已经存下了。
                self.marks_path = None;
                self.marks_warning = Some(format!("日志目录写不进去（{e}），标记只在本次会话有效"));
                self.marks_dirty = false;
            }
        }
    }

    fn export_marks(&mut self) {
        if self.marks.is_empty() {
            self.flash("还没有标记");
            return;
        }
        let name = self
            .store
            .as_ref()
            .and_then(|s| s.path.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "logview".to_string());
        let items: Vec<Bookmark> = self.marks.iter().map(|m| m.bm.clone()).collect();
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{name}.标记.md"))
            .save_file()
        else {
            return;
        };
        match bookmarks::export_markdown(&path, &name, &items, bookmarks::now_secs()) {
            Ok(()) => self.flash(format!("已导出 {} 条标记", items.len())),
            Err(e) => self.flash(format!("导出失败：{e}")),
        }
    }

    /// 右侧标记面板。
    fn marks_panel(&mut self, ui: &mut egui::Ui) {
        // 先把每行要显示的东西算好：面板里要同时读 marks、写 note_buf，
        // 一份只读快照最省事。
        let rows: Vec<MarkRow> = self
            .marks
            .iter()
            .map(|m| MarkRow {
                offset: m.bm.offset,
                line: self
                    .store
                    .as_ref()
                    .and_then(|s| s.line_index_at(m.bm.offset as usize)),
                head: m.bm.head.clone(),
                note: m.bm.note.clone(),
                time: m.bm.relative_time(bookmarks::now_secs()),
                state: m.anchor,
            })
            .collect();
        let stale = rows
            .iter()
            .filter(|r| r.state != AnchorState::Fresh)
            .count();
        let editing = self.editing_note;
        let mut act: Option<MarkAct> = None;

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("标记 {}", rows.len())).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("收起").clicked() {
                    act = Some(MarkAct::Hide);
                }
                if ui.small_button("导出").clicked() {
                    act = Some(MarkAct::Export);
                }
            });
        });

        if stale > 0 {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{stale} 条已失效")).color(ui.visuals().warn_fg_color),
                );
                if ui.small_button("按原文回找").clicked() {
                    act = Some(MarkAct::RefindAll);
                }
                if ui.small_button("清掉").clicked() {
                    act = Some(MarkAct::DropStale);
                }
            });
        }
        if let Some(w) = &self.marks_warning {
            ui.label(RichText::new(w).color(ui.visuals().warn_fg_color));
        }

        ui.separator();

        // 脚注：标记存在哪。写不进去时必须说清楚——"以为存下了"是最坏的结果。
        //
        // 用嵌套的底部面板把它钉住，而不是在滚动区后面直接画一行：滚动区会吃掉所有
        // 剩余高度，跟在它后面的东西会被挤出可视区域（第一版就是这样，脚注整行看不见）。
        let footer = match (&self.marks_path, &self.marks_warning) {
            (Some(p), _) => format!("侧车：{}", p.display()),
            (None, Some(w)) => w.clone(),
            (None, None) => "未打开文件".to_string(),
        };
        egui::TopBottomPanel::bottom("marks_footer").show_inside(ui, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(footer)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                )
                .truncate(),
            );
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("marks_list")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if rows.is_empty() {
                        ui.label(
                            RichText::new("滚到某一行按 b 标记；在搜索命中处按 b 也行")
                                .color(ui.visuals().weak_text_color()),
                        );
                        return;
                    }
                    for row in &rows {
                        let fresh = row.state == AnchorState::Fresh;
                        // 这一条是不是刚跳过去的那条：是的话行号与首行用标记色，
                        // 与日志区那行的紫底呼应——否则从面板点到日志区，视线回来就找不到是第几条
                        let focused = row.line.is_some() && row.line == self.focused_mark;
                        let color = if focused {
                            mark_dot_color(ui.visuals().dark_mode)
                        } else if fresh {
                            ui.visuals().text_color()
                        } else {
                            ui.visuals().weak_text_color()
                        };

                        ui.horizontal(|ui| {
                            let num = match row.line {
                                Some(l) => format!("{}", l + 1),
                                None => "—".to_string(),
                            };
                            ui.label(RichText::new(num).monospace().small().color(color));
                            let resp = ui
                                .add(
                                    egui::Label::new(RichText::new(&row.head).color(color))
                                        .truncate(),
                                )
                                .interact(egui::Sense::click());
                            if resp.clicked() && fresh {
                                act = Some(MarkAct::Goto(row.offset));
                            }
                            if !fresh {
                                let why = match row.state {
                                    AnchorState::Lost => "超出文件尾",
                                    _ => "内容已变",
                                };
                                ui.label(
                                    RichText::new(why).small().color(ui.visuals().warn_fg_color),
                                );
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("×").clicked() {
                                        act = Some(MarkAct::Delete(row.offset));
                                    }
                                },
                            );
                        });

                        ui.horizontal(|ui| {
                            ui.add_space(6.0);
                            if editing == Some(row.offset) {
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.note_buf)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("备注，回车或点到别处即保存"),
                                );
                                resp.request_focus();
                                if resp.lost_focus() {
                                    act = Some(MarkAct::CommitNote);
                                }
                            } else {
                                let empty = row.note.is_empty();
                                let txt = if empty {
                                    "＋备注".to_string()
                                } else {
                                    row.note.clone()
                                };
                                let c = if empty {
                                    ui.visuals().weak_text_color()
                                } else {
                                    color
                                };
                                let resp = ui
                                    .add(
                                        egui::Label::new(RichText::new(txt).small().color(c))
                                            .truncate(),
                                    )
                                    .interact(egui::Sense::click());
                                if resp.clicked() {
                                    act = Some(MarkAct::StartNote(row.offset));
                                }
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(&row.time)
                                            .small()
                                            .color(ui.visuals().weak_text_color()),
                                    );
                                },
                            );
                        });
                        ui.separator();
                    }
                });
        });

        match act {
            Some(MarkAct::Goto(offset)) => {
                if let Some(line) = self
                    .store
                    .as_ref()
                    .and_then(|s| s.line_index_at(offset as usize))
                {
                    self.jump_to_file_line(line);
                }
            }
            Some(MarkAct::Delete(offset)) => self.remove_mark(offset),
            Some(MarkAct::StartNote(offset)) => {
                self.note_buf = self
                    .marks
                    .iter()
                    .find(|m| m.bm.offset == offset)
                    .map(|m| m.bm.note.clone())
                    .unwrap_or_default();
                self.editing_note = Some(offset);
            }
            Some(MarkAct::CommitNote) => {
                self.commit_note();
            }
            Some(MarkAct::RefindAll) => self.refind_marks(None),
            Some(MarkAct::DropStale) => self.drop_stale_marks(),
            Some(MarkAct::Export) => self.export_marks(),
            Some(MarkAct::Hide) => self.show_marks = false,
            None => {}
        }
    }

    /// 全局快捷键。
    ///
    /// 裸字母键只在搜索框没有焦点时才作为快捷键，否则会抢走正常输入。
    /// 修饰键组合（⌘F / ⌘O）任何时候都生效。
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};

        // 正在改备注时，键盘整体让给输入框：这时按 b 是想打字母 b，不是标记。
        // Esc 收回编辑，其余快捷键一律不生效。
        if self.editing_note.is_some() {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.editing_note = None;
                self.note_buf.clear();
            }
            return;
        }

        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            if let Some(p) = rfd::FileDialog::new().pick_file() {
                self.open_path(p);
            }
        }

        // ⌘B 开合标记面板。放在裸字母键之前处理：COMMAND 组合任何时候都该生效。
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::B)) {
            self.show_marks = !self.show_marks;
        }

        let focus_hotkey = ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F))
            || (!self.search_has_focus
                && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Slash)));

        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.query.clear();
            if let Some(s) = &mut self.store {
                s.clear_search();
            }
            self.search_has_focus = false;
            ctx.memory_mut(|m| m.surrender_focus(egui::Id::new(SEARCH_ID)));
            return;
        }

        if focus_hotkey {
            self.focus_search = true;
            return;
        }

        if self.search_has_focus {
            return;
        }

        // 注意：egui 的 consume_key(Modifiers::NONE, ..) 会**忽略**修饰键，
        // 所以不能写成"先匹配 NONE、再匹配 SHIFT"——Shift+N 会被前一条吃掉。
        // 正确做法是用 NONE 消费，再读 shift 状态决定方向。
        let shift = ctx.input(|i| i.modifiers.shift);

        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::N)) {
            if shift {
                self.goto_match(-1);
            } else {
                self.goto_match(1);
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::G)) {
            if shift {
                self.jump_to_end = true;
                self.focused_mark = None;
            } else {
                self.jump_to_line(0);
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::B)) {
            self.toggle_mark();
        }
        // F2 / ⇧F2 在标记之间前后跳：与编辑器里的"下一个书签"同键，习惯能直接搬过来
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F2)) {
            self.goto_mark(if shift { -1 } else { 1 });
        }
    }
}

impl eframe::App for LogViewApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_drop(ctx);

        if let Some(dark) = self.dark {
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
        }

        // 后台数据抽取 + 文件变化检测（节流 250ms）
        if self.last_refresh.elapsed() > Duration::from_millis(250) {
            self.last_refresh = Instant::now();
            if let Some(s) = &mut self.store {
                s.refresh();
            }
        }
        if let Some(s) = &mut self.store {
            s.pump();
        }

        // 搜索结果到位后决定视图去向
        self.apply_search_outcome();

        // 搜索防抖
        if let Some(t) = self.search_dirty {
            if t.elapsed() > Duration::from_millis(250) {
                self.search_dirty = None;
                self.run_search(OnSearchDone::GotoFirstMatch);
            }
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.toolbar(ctx, ui));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.status_bar(ui));
        if self.show_marks {
            egui::SidePanel::right("marks")
                .default_width(272.0)
                .width_range(200.0..=460.0)
                .show(ctx, |ui| self.marks_panel(ui));
        }
        egui::CentralPanel::default().show(ctx, |ui| self.log_area(ui));

        // 快捷键放在各面板之后：此时 search_has_focus 已是本帧的最新状态
        self.handle_shortcuts(ctx);

        // 标记改动即落盘：拖到帧末写，一帧里连改几次也只写一次
        self.save_marks();

        // 定时重绘：索引进度、文件变化检测与跟随刷新都依赖它
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

impl LogViewApp {
    /// 工具栏分两层：第一层管"看什么文件、怎么显示"，第二层管"搜什么、跳到哪"。
    ///
    /// 原先两层是混在一起的：打开文件、主题、六个复选框、导航按钮全铺在同一行，
    /// 每个控件权重一样，找东西得逐个读文字。现在按用途归位，
    /// 并且把「打开文件」做成整屏唯一的主按钮——它是打开软件后第一个要点的东西。
    fn toolbar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            let open = egui::Button::new(RichText::new("打开文件").color(Color32::WHITE))
                .fill(hex(PRIMARY_FILL));
            if ui.add(open).clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_file() {
                    self.open_path(p);
                }
            }

            match &self.store {
                Some(s) => {
                    let name = s.path_str();
                    // 宽度按窗口比例给上限：宽窗口多显示一点路径，窄窗口不把右侧视图开关挤掉。
                    // 用 allocate_ui_with_layout 而不是 add_sized——后者会把 label 居中在格子里，
                    // 文件名就飘到离按钮很远的地方去了。
                    let w = (ui.available_width() * 0.32).clamp(80.0, 360.0);
                    ui.scope(|ui| {
                        ui.set_max_width(w);
                        // 不要在这里加 on_hover_text：`Label` 在文本被 elide 时会**自己**
                        // 挂一个"完整文本"的悬停提示（label.rs 里 `if galley.elided`），
                        // 再加一个就会并排弹出两个内容相同的提示框。
                        // 而且它自带的只在真的被截断时才出现，比无条件提示更合适。
                        let _ = ui.add(egui::Label::new(RichText::new(&name)).truncate());
                    });
                }
                None => {
                    ui.label(
                        RichText::new("未打开文件 — 点按钮选择，或把日志文件拖进窗口")
                            .color(ui.visuals().weak_text_color()),
                    );
                }
            }

            // 视图开关靠右。right_to_left 里先加的排在更右边，所以这里是视觉上的倒序。
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.theme_button(ui);
                ui.checkbox(&mut self.wrap, "自动换行")
                    .on_hover_text("长行折行显示；开启后跳转定位不再精确");
                ui.checkbox(&mut self.follow, "跟随尾部")
                    .on_hover_text("类似 tail -f，自动滚到最新一行");
                if ui
                    .checkbox(&mut self.only_matched, "只显示匹配行")
                    .changed()
                {
                    self.scroll_offset = 0.0;
                    self.pending_jump = Some(0);
                }
            });
        });

        ui.add_space(4.0);

        ui.horizontal(|ui| {
            let search_w = (ui.available_width() * 0.34).clamp(180.0, 460.0);
            let resp = ui.add(
                TextEdit::singleline(&mut self.query)
                    .id(egui::Id::new(SEARCH_ID))
                    .hint_text(format!("搜索（{CMD}F 或 / 聚焦，n / N 跳转）"))
                    .desired_width(search_w),
            );
            if self.focus_search {
                resp.request_focus();
                self.focus_search = false;
            }
            self.search_has_focus = resp.has_focus();
            if resp.changed() {
                self.search_dirty = Some(Instant::now());
            }
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.run_search(OnSearchDone::GotoFirstMatch);
            }

            let cs = ui.checkbox(&mut self.case_sensitive, "区分大小写");
            if cs.changed() {
                // 选项会改变匹配结果，立刻按新条件重查
                self.run_search(OnSearchDone::GotoFirstMatch);
            }
            let rx = ui
                .checkbox(&mut self.use_regex, "正则")
                .on_hover_text("把检索词当作正则表达式，例如 ERROR|FATAL\\d+");
            if rx.changed() {
                self.run_search(OnSearchDone::GotoFirstMatch);
            }

            // 命中导航靠右；没有检索词时不占位置——空着比显示一个 0/0 干净
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let _ = ui.button("?").on_hover_text(format!(
                    "快捷键\n\
                     {CMD}F 或 / — 聚焦搜索框\n\
                     n / N — 下 / 上一个命中\n\
                     g / G — 跳到开头 / 末尾\n\
                     Esc — 清空检索\n\
                     {CMD}O — 打开文件\n\
                     双击行内文字 — 就地搜索该词，视图不移动"
                ));
                if !self.query.is_empty() {
                    if ui.button("清空").clicked() {
                        self.query.clear();
                        if let Some(s) = &mut self.store {
                            s.clear_search();
                        }
                    }
                    let matches = self.store.as_ref().map(|s| s.matches().len()).unwrap_or(0);
                    ui.label(
                        RichText::new(format!("{} / {}", self.match_cursor + 1, matches))
                            .color(ui.visuals().weak_text_color()),
                    );
                    if ui.button("▶").clicked() {
                        self.goto_match(1);
                    }
                    if ui.button("◀").clicked() {
                        self.goto_match(-1);
                    }
                }
            });
        });

        ui.add_space(6.0);
        let _ = ctx;
    }

    /// 主题按钮：图标表示**当前**状态，悬停说明点击后变成什么。
    ///
    /// 原先按钮上写的是"点击后会变成"的状态（当前跟随系统时显示「主题」），
    /// 既不像状态也不像动作，看着不知道是什么意思。
    fn theme_button(&mut self, ui: &mut egui::Ui) {
        let (icon, tip) = match self.dark {
            None => ('◑', "跟随系统（点击改为浅色）"),
            Some(false) => ('☀', "浅色（点击改为深色）"),
            Some(true) => ('🌙', "深色（点击改为跟随系统）"),
        };
        if ui.button(icon.to_string()).on_hover_text(tip).clicked() {
            self.dark = next_theme(self.dark);
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let Some(s) = &self.store else {
                ui.label("就绪");
                return;
            };
            ui.label(format!("{} 行", s.line_count()));
            ui.separator();
            ui.label(human_size(s.len_bytes()));
            ui.separator();
            ui.label(encoding_name(s.encoding));
            ui.separator();
            if s.indexing {
                ui.label(if s.search_pending() {
                    "索引中（完成后自动检索）…"
                } else {
                    "索引中…"
                });
            } else if s.searching {
                ui.label("搜索中…");
            } else {
                ui.label("就绪");
            }
            if let Some(err) = s.regex_error() {
                ui.separator();
                ui.label(
                    RichText::new(format!("正则无效：{err}")).color(ui.visuals().error_fg_color),
                );
            } else if !self.query.is_empty() {
                ui.separator();
                let n = s.matches().len();
                if s.search_truncated() {
                    // 扫描是完整的，总数准确；只是可跳转的命中被保留上限截断了
                    let reach = s
                        .matches()
                        .last()
                        .map(|line| *line as usize + 1)
                        .unwrap_or(0);
                    ui.label(
                        RichText::new(truncated_hits_label(n, reach, s.search_total()))
                            .color(ui.visuals().warn_fg_color),
                    );
                } else {
                    ui.label(format!("{n} 匹配"));
                }
            }
            // 标记数放状态栏而不是工具栏：工具栏在 640 宽度下已经很挤，
            // 而状态栏本来就是一排计数，多一个不挤。
            if !self.marks.is_empty() {
                ui.separator();
                let stale = self
                    .marks
                    .iter()
                    .filter(|m| m.anchor != AnchorState::Fresh)
                    .count();
                let text = if stale == 0 {
                    format!("标记 {}", self.marks.len())
                } else {
                    format!("标记 {}（{stale} 条失效）", self.marks.len())
                };
                ui.label(text);
            }
            // 一次性提示：标记了第几行、读回了几条……几秒后自己消失
            if let Some((msg, at)) = &self.flash {
                if at.elapsed() < Duration::from_secs(4) {
                    ui.separator();
                    ui.label(msg);
                }
            }
            if let Some(e) = &self.error {
                ui.separator();
                ui.label(RichText::new(e).color(ui.visuals().error_fg_color));
            }
        });
    }

    fn log_area(&mut self, ui: &mut egui::Ui) {
        let Some(store) = &self.store else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("把日志文件拖到这里")
                        .size(16.0)
                        .color(ui.visuals().weak_text_color()),
                );
            });
            return;
        };

        let total = if self.only_matched {
            store.matches().len()
        } else {
            store.line_count()
        };
        if total == 0 {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new(if store.indexing {
                        "正在建立索引…"
                    } else {
                        "文件为空"
                    })
                    .color(ui.visuals().weak_text_color()),
                );
            });
            return;
        }

        // G 跳到末尾：需要先知道总行数，所以放到这里处理
        if self.jump_to_end {
            self.jump_to_end = false;
            self.jump_to_line(total.saturating_sub(1));
        }

        let row_h = ui.text_style_height(&TextStyle::Monospace) + 3.0;
        // 一行的实际占位 = 文本高度 + 行间间距。`show_rows` 内部就是这么算行距的，
        // 行号↔像素的换算必须用同一个数——用差了，偏差会随行号累积：
        // 30 万行的日志里差几像素就能累积成几万像素，跳转自然落不到目标行上。
        //
        // 两个数都取自运行时的字体度量与样式，没有硬编码像素值：各平台加载的中文字体
        // 不同、屏幕缩放也不同，行高会跟着变，而"实际行距 == row_pitch"这个关系不变。
        let row_pitch = row_h + ui.spacing().item_spacing.y;
        let gutter_w = (format!("{total}").len() as f32) * 8.0 + 12.0;

        // 跳转：换算成滚动偏移
        // pending_jump 存的已经是「列表中的行位置」：
        // only_matched 模式下是匹配序号，否则是文件行号，两者都直接乘行距。
        if let Some(target) = self.pending_jump.take() {
            self.scroll_offset = (target as f32 * row_pitch - 120.0).max(0.0);
        }

        // pending_jump 已在上面取走并转成偏移量，此处只看跟随开关
        let stick = self.follow && !self.only_matched;
        let mut area = ScrollArea::vertical()
            .id_salt("log_scroll")
            .auto_shrink([false, false])
            .stick_to_bottom(stick);
        if !stick {
            area = area.vertical_scroll_offset(self.scroll_offset);
        }

        // 当前匹配项所在的行：整行加底色，否则按 n 跳转之后看不出落在哪一条
        let current_line = self.current_match_line();
        // 刚从面板或 F2 跳到的标记行：同样铺一层底，用标记色
        let focused_line = self.focused_mark;
        let regex = self.highlight_regex.as_ref();
        let buf = &mut self.buf;
        let query = self.query.clone();
        let cs = self.case_sensitive;
        let only = self.only_matched;
        let wrap = self.wrap;
        let store_ref = &self.store;
        // 双击行内文字要发起的检索；闭包内拿不到 &mut self，先收集、结束后再处理
        let mut search_word: Option<String> = None;
        // 已标记的行（按字节偏移查表）。逐行线扫 marks 也行，但那是每帧 × 每行
        let marked: HashSet<usize> = self.marks.iter().map(|m| m.bm.offset as usize).collect();
        // 点行号 = 把"当前行"设过去。同样先收集，出了闭包再改 self
        let mut clicked_line: Option<usize> = None;

        let out = area.show_rows(ui, row_h, total, |ui, rows| {
            let Some(store) = store_ref else { return };
            for r in rows {
                let row_top = ui.cursor().top();
                let line_idx = if only {
                    store.matches().get(r).copied().map(|v| v as usize)
                } else {
                    Some(r)
                };
                // 空行或越界的行也要占位，否则行距会从这里开始错位
                if let Some(line_idx) = line_idx {
                    buf.clear();
                    if store.read_line_into(line_idx, buf) {
                        // 行首结构只解析一次：色条与行内上色共用同一份结果
                        let head = parse_line_head(buf);
                        // 铺底优先给标记：它是用户刚刚点/跳的那一条，比"当前命中"更该被认出来
                        let row_bg = match row_bg_kind(line_idx, focused_line, current_line) {
                            RowBg::Mark => Some(mark_row_bg(ui.visuals().dark_mode)),
                            // 与命中词的黄色高亮区分开，这里用冷色整行铺底
                            RowBg::Match => Some(if ui.visuals().dark_mode {
                                egui::Color32::from_rgb(30, 44, 62)
                            } else {
                                egui::Color32::from_rgb(226, 240, 253)
                            }),
                            RowBg::None => None,
                        };
                        if let Some(bg) = row_bg {
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    ui.cursor().min,
                                    egui::vec2(ui.available_width(), row_h),
                                ),
                                0.0,
                                bg,
                            );
                        }
                        let rule_color = head
                            .level_of()
                            .and_then(|lv| lv.style(ui.visuals().dark_mode).rule);
                        ui.allocate_ui(egui::vec2(ui.available_width(), row_h), |ui| {
                            ui.horizontal(|ui| {
                                // 级别色条。无论这一行有没有识别出级别，都要占住同样的
                                // 宽度，否则内容列会随首行是否带级别而左右跳动。
                                let (cell, _) = ui.allocate_exact_size(
                                    egui::vec2(LEVEL_RULE_W + LEVEL_RULE_GAP, row_h),
                                    egui::Sense::hover(),
                                );
                                if let Some(c) = rule_color {
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_size(
                                            cell.min,
                                            egui::vec2(LEVEL_RULE_W, row_h),
                                        ),
                                        0.0,
                                        c,
                                    );
                                }
                                // 标记点。同样无论有没有标记都占住这一格，行号才不会左右跳。
                                let (mark_cell, _) = ui.allocate_exact_size(
                                    egui::vec2(MARK_W, row_h),
                                    egui::Sense::hover(),
                                );
                                let is_marked = store
                                    .line_offset(line_idx)
                                    .is_some_and(|o| marked.contains(&o));
                                if is_marked {
                                    let r = egui::Rect::from_center_size(
                                        mark_cell.center(),
                                        egui::vec2(6.0, 6.0),
                                    );
                                    ui.painter().rect_filled(
                                        r,
                                        3.0,
                                        mark_dot_color(ui.visuals().dark_mode),
                                    );
                                }
                                // 行号。点击把"当前行"设到这里——于是 b / 备注定的是
                                // 你点的那一行，而不必先滚到视口顶部。
                                let num = ui
                                    .add_sized(
                                        [gutter_w, row_h],
                                        egui::Label::new(
                                            RichText::new(format!("{}", line_idx + 1))
                                                .monospace()
                                                .color(ui.visuals().weak_text_color()),
                                        ),
                                    )
                                    .interact(egui::Sense::click())
                                    .on_hover_text("点击设为当前行（按 b 标记）");
                                if num.clicked() {
                                    clicked_line = Some(line_idx);
                                }
                                if let Some(word) =
                                    render_content(ui, buf, &head, &query, cs, wrap, regex)
                                {
                                    search_word = Some(word);
                                }
                            });
                        });
                    }
                }
                // 上面的容器实际吃掉的高度会比 row_h 多出约 1px（内部布局取整），
                // 而滚动换算用的是 row_pitch：把光标强制推回 row_top + row_h，
                // 两者才能对齐，跳转才落在正确的行上。
                // 开了自动换行时行高本来就随内容变，按固定高度推反而会重叠，故不干预。
                if !wrap {
                    ui.advance_cursor_after_rect(egui::Rect::from_min_size(
                        egui::pos2(ui.min_rect().left(), row_top),
                        egui::vec2(0.0, row_h),
                    ));
                    // 这一条是整套跳转算法的地基：实际行距必须等于换算用的 row_pitch。
                    // 它一旦不成立（比如 egui 改了光标推进的语义），跳转又会开始偏，
                    // 而且是"每行差几像素、随行号累积"的隐蔽偏差。
                    // 所以宁可让 debug 构建与测试直接炸掉，也不让它悄悄退回去。
                    debug_assert!(
                        (ui.cursor().top() - row_top - row_pitch).abs() < 0.05,
                        "实际行距 {} 与换算用的 {} 不一致",
                        ui.cursor().top() - row_top,
                        row_pitch
                    );
                }
            }
        });

        if !stick {
            self.scroll_offset = out.state.offset.y;
        }

        // 记下视口顶部是哪一行：双击取词后要靠它把游标对齐到当前位置。
        // 「只显示匹配行」模式下滚动的单位是命中序号，先换算回文件行号。
        let top_row = (out.state.offset.y / row_pitch).max(0.0) as usize;
        self.top_line = if only {
            self.store
                .as_ref()
                .and_then(|s| s.matches().get(top_row).copied())
                .map(|line| line as usize)
                .unwrap_or(0)
        } else {
            top_row
        };

        // 点行号 = 把当前行设过去，之后 b / 备注都对着它
        if let Some(line) = clicked_line {
            self.select_line(line);
        }

        // 记下视口能装几行：标记要判断"当前行还看不看得见"（见 mark_target）
        self.visible_rows = (out.inner_rect.height() / row_pitch).max(1.0) as usize;

        // 双击取词：拿词去搜，视图不动
        if let Some(word) = search_word {
            self.search_for(word);
        }
    }
}

/// 「打开文件」按钮的填充色。
///
/// 用固定值而不是 `visuals.selection.bg_fill`：那个值在两个主题下的深浅方向是相反的
/// （浅色主题给浅蓝 `#90D1FF`，白字只有 1.65:1，根本读不出；深色主题给深青蓝 `#005C80`，
/// 白字够亮但相对背景只有 2.33:1）。这跟"不要在主题色上做乘法"是同一类错误——
/// 依赖派生色，就要为每种主题各验一遍；用指定值只需验一个。
/// `#2563EB` 在两种主题下都达标：白字 5.17:1，相对背景浅色 4.87:1 / 深色 3.33:1。
const PRIMARY_FILL: u32 = 0x2563EB;

/// 行左侧级别色条的宽度
const LEVEL_RULE_W: f32 = 3.0;
/// 级别色条与行号之间的间距，色条有无都要占住这段宽度，内容列才会对齐
const LEVEL_RULE_GAP: f32 = 5.0;
/// 标记点那一列的宽度。同样无论有没有标记都要占住：否则标记一出现，
/// 整列行号跟着右移，"同一行在不同时刻对不齐"。
const MARK_W: f32 = 16.0;

/// 标记点的颜色。
///
/// 蓝色的命中底色与黄绿的级别标签都被占用了，标记用紫色，三者在两个主题下都分得开；
/// 只需要 3:1 的非文字对比度（点是个色块，不是文字），所以不必按 WCAG AA 的文本来挑。
fn mark_dot_color(dark: bool) -> Color32 {
    if dark {
        hex(0xAFA9EC)
    } else {
        hex(0x534AB7)
    }
}

/// 从面板/F2 跳到的标记行，整行铺这层底。
///
/// 与 [`mark_dot_color`] 同色系（都取自同一支紫），一眼能看出"紫底就是紫点那一行"。
/// 底色要浅到不夺正文：正文用主题的 text_color，实测两种主题下都在 9:1 以上
/// （浅色 #E7E3F8 对 #1B1B1B 约 13.7:1；深色 #322C52 对 #DCDCDC 约 9.5:1），
/// 高于 WCAG AA 的 4.5:1，也高于 AAA 的 7:1。
fn mark_row_bg(dark: bool) -> Color32 {
    if dark {
        hex(0x322C52)
    } else {
        hex(0xE7E3F8)
    }
}

/// 一行上该铺哪种底。两类高亮互斥，同时命中时**标记优先**——它是用户刚刚点的那一条，
/// 而"当前命中"只是检索的副产品，认错了也不影响判断。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowBg {
    /// 不铺
    None,
    /// 当前搜索命中行（冷色底）
    Match,
    /// 刚从面板 / `F2` 跳到的标记行（标记色底）
    Mark,
}

fn row_bg_kind(line: usize, focused_mark: Option<usize>, current_match: Option<usize>) -> RowBg {
    if focused_mark == Some(line) {
        RowBg::Mark
    } else if current_match == Some(line) {
        RowBg::Match
    } else {
        RowBg::None
    }
}

/// 查找日志级别时最多跳过几个 token。
///
/// 定这个上限是为了修掉一个误判：原先的判据是"整行包含 ERROR 就给整行染色"，
/// 于是正文里提到 ERROR（比如"检测到 3 个 ERROR 已忽略"）也会被当成错误行。
/// 现在只认时间戳之后的头几个 token——`[main]`、`[order-1]` 这类中间字段会各算一个，
/// 3 个够覆盖 `时间 [线程] 级别` 之类排布，同时正文里的 ERROR 再也够不着。
const LEVEL_MAX_TOKENS: usize = 3;

/// 日志级别。只用来给行首那个级别单词上色、以及决定左侧色条。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

/// 级别在界面上用到的颜色。
///
/// `bg` / `rule` 是 `Option`：只有 WARN 与 ERROR 才带底色和左侧色条。
/// INFO 与 DEBUG 在日志里占绝大多数，若每行都挂色条，左边会连成一条满屏的线，
/// 色条也就失去了"让异常跳出来"的作用——全都强调等于都不强调。
struct LevelStyle {
    /// 标签底色，None 表示不给底色
    bg: Option<Color32>,
    /// 标签文字色
    fg: Color32,
    /// 行左侧色条，None 表示这一行不画
    rule: Option<Color32>,
}

/// `0xRRGGBB` → Color32，便于把配色写成一张表
fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8)
}

impl Level {
    /// 级别词的候选写法。更严重的排在前面；较长的写法必须排在较短的之前，
    /// 否则 `WARNING` 会被 `WARN` 抢先匹配掉半截。
    const WORDS: [(&'static str, Level); 10] = [
        ("CRITICAL", Level::Error),
        ("FATAL", Level::Error),
        ("SEVERE", Level::Error),
        ("ERROR", Level::Error),
        ("WARNING", Level::Warn),
        ("WARN", Level::Warn),
        ("NOTICE", Level::Info),
        ("INFO", Level::Info),
        ("DEBUG", Level::Debug),
        ("TRACE", Level::Debug),
    ];

    /// WARN / ERROR 用底色加色条突出；INFO / DEBUG 只调文字色，不加任何色块。
    /// 亮色主题用 50 级做底、800 级做字；暗色主题用 900 级做底、100 级做字。
    /// 色条在两个主题里共用同一组中间色——滚动时它才是稳定的视觉锚点。
    fn style(self, dark: bool) -> LevelStyle {
        match (self, dark) {
            (Level::Error, false) => LevelStyle {
                bg: Some(hex(0xFCEBEB)),
                fg: hex(0x791F1F),
                rule: Some(hex(0xE24B4A)),
            },
            (Level::Error, true) => LevelStyle {
                bg: Some(hex(0x501313)),
                fg: hex(0xF7C1C1),
                rule: Some(hex(0xE24B4A)),
            },
            (Level::Warn, false) => LevelStyle {
                bg: Some(hex(0xFAEEDA)),
                fg: hex(0x633806),
                rule: Some(hex(0xEF9F27)),
            },
            (Level::Warn, true) => LevelStyle {
                bg: Some(hex(0x412402)),
                fg: hex(0xFAC775),
                rule: Some(hex(0xEF9F27)),
            },
            (Level::Info, false) => LevelStyle {
                bg: None,
                fg: hex(0x145ABE),
                rule: None,
            },
            (Level::Info, true) => LevelStyle {
                bg: None,
                fg: hex(0x78BEFF),
                rule: None,
            },
            (Level::Debug, false) | (Level::Debug, true) => LevelStyle {
                bg: None,
                fg: muted_text(dark),
                rule: None,
            },
        }
    }
}

/// 时间戳这类"次要但必须读得清"的文字色。
///
/// 这里给**指定值**，而不是拿 `weak_text_color()` 再压暗。
/// 原先的做法是 `weak_text_color().linear_multiply(0.62)`，浅色主题下对比度掉到 3:1
/// 以下——老许直接反馈"看不清"。次要文字可以弱，但不能弱到读不动，
/// 所以两边都取到 WCAG AA（4.5:1）以上：亮色 #5F5E5A ≈ 6.2:1，暗色 #B4B2A9 ≈ 8.0:1。
fn muted_text(dark: bool) -> Color32 {
    if dark {
        hex(0xB4B2A9)
    } else {
        hex(0x5F5E5A)
    }
}

/// 行首能识别出来的结构（都按字节记范围，因为日期/时间/级别全是 ASCII）。
///
/// 日期与时间分开记，是为了把日期淡出：同一天的日志里日期是冗余的，
/// 真正在变的是时间，让它醒目一点更有用。
#[derive(Default, PartialEq, Eq, Debug)]
struct LineHead {
    /// 日期部分，如 `2026-09-11`
    date: Option<Range<usize>>,
    /// 时间部分（含小数秒），如 `14:33:41.561`
    time: Option<Range<usize>>,
    /// 级别单词及其级别
    level: Option<(Range<usize>, Level)>,
}

impl LineHead {
    fn level_of(&self) -> Option<Level> {
        self.level.as_ref().map(|(_, lv)| *lv)
    }
}

/// 扫描 `YYYY-MM-DD HH:MM:SS[.fff]`（也接受 `/` 作日期分隔、`T` 作日期时间分隔、
/// `,` 作小数分隔——Java 的某些 Locale 会用逗号）。
///
/// 返回 `(日期结束位置, 整段结束位置)`。只认 ASCII 数字与固定分隔符，按字节扫描是安全的。
fn scan_timestamp(b: &[u8], start: usize) -> Option<(usize, usize)> {
    if b.len() < start + 10 {
        return None;
    }
    let sep = b[start + 4];
    if sep != b'-' && sep != b'/' {
        return None;
    }
    if b[start + 7] != sep {
        return None;
    }
    if !(0..10).all(|k| b[start + k].is_ascii_digit() || k == 4 || k == 7) {
        return None;
    }
    let date_end = start + 10;

    let mut i = date_end;
    if i >= b.len() || (b[i] != b' ' && b[i] != b'T') {
        return None;
    }
    i += 1;
    if b.len() < i + 8 || b[i + 2] != b':' || b[i + 5] != b':' {
        return None;
    }
    if !(0..8).all(|k| b[i + k].is_ascii_digit() || k == 2 || k == 5) {
        return None;
    }
    i += 8;
    // 小数秒
    if i < b.len() && (b[i] == b'.' || b[i] == b',') {
        let mut k = i + 1;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        if k > i + 1 {
            i = k;
        }
    }
    Some((date_end, i))
}

/// 从 `start` 起数 [`LEVEL_MAX_TOKENS`] 个 token，看有没有哪个正好是级别词。
///
/// 空白与方括号只当分隔符、不占 token，所以 `[main]` 算一个 token、
/// `[order-1]` 也算一个。整个 token 必须与级别词完全相等：
/// `ERRORS`、`ERROR_CODE=5` 都不会被当成级别。
fn find_level(b: &[u8], start: usize) -> Option<(Range<usize>, Level)> {
    let is_sep = |c: u8| c.is_ascii_whitespace() || c == b'[' || c == b']' || c == b'|';

    let mut i = start;
    let mut taken = 0usize;
    while taken < LEVEL_MAX_TOKENS {
        while i < b.len() && is_sep(b[i]) {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        let tok_start = i;
        while i < b.len() && !is_sep(b[i]) {
            i += 1;
        }
        let tok = &b[tok_start..i];
        // 行首的结构字段（时间戳、方括号、线程名、logger 名）都是 ASCII。
        // 一旦取到含非 ASCII 的 token，说明已经读进正文了——中文没有空格分词，
        // 整段中文会算成一个 token，正文里的 ERROR 就会跟着溜进来。到此为止。
        if !tok.is_ascii() {
            return None;
        }
        for (word, lv) in Level::WORDS {
            if tok == word.as_bytes() {
                return Some((tok_start..i, lv));
            }
        }
        taken += 1;
    }
    None
}

/// 从行首解析出日期、时间与日志级别。
fn parse_line_head(line: &str) -> LineHead {
    let b = line.as_bytes();
    let mut head = LineHead::default();

    // 允许前导的 '[' 与空白：`[2026-09-11 14:33:41.561] INFO ...` 是常见排布
    let mut i = 0;
    while i < b.len() && (b[i] == b'[' || b[i] == b' ') {
        i += 1;
    }
    if let Some((date_end, ts_end)) = scan_timestamp(b, i) {
        head.date = Some(i..date_end);
        // 日期与时间之间的分隔符（空格或 'T'）不属于任何一段，留作默认色
        head.time = Some(date_end + 1..ts_end);
        i = ts_end;
    }
    head.level = find_level(b, i);
    head
}

/// 绘制一行内容，返回该行被双击时命中的词。
///
/// 这里没有用 `Label::selectable`，而是自己 layout galley 再交给
/// `LabelSelectionState::label_text_selection`。两者都提供划选与复制，
/// 区别在于后者会把 galley 留在手上，才能反查"双击点到了哪个词"——
/// egui 并未对外暴露 Label 当前选中的文本，这是唯一可行的路子。
fn render_content(
    ui: &mut egui::Ui,
    text: &str,
    head: &LineHead,
    query: &str,
    case_sensitive: bool,
    wrap: bool,
    regex: Option<&regex::Regex>,
) -> Option<String> {
    let mut shown = text;
    if shown.len() > MAX_RENDER_CHARS {
        shown = floor_char_boundary(shown, MAX_RENDER_CHARS);
    }

    let base_color = ui.visuals().text_color();
    let font_id: FontId = TextStyle::Monospace.resolve(ui.style());
    let highlights = match_ranges(shown, query, case_sensitive, regex);
    let mut job = styled_job(
        shown,
        head,
        base_color,
        muted_text(ui.visuals().dark_mode),
        &font_id,
        &highlights,
        ui.visuals().dark_mode,
    );
    job.wrap.max_width = ui.available_width();
    if !wrap {
        // 不换行时压成单行、超出部分用省略号收尾，等价于原先的 truncate
        job.wrap.max_rows = 1;
        job.wrap.overflow_character = Some('…');
    }

    let galley = ui.fonts(|f| f.layout_job(job));
    let (rect, response) = ui.allocate_exact_size(galley.size(), egui::Sense::click_and_drag());

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }

    // 划选、复制与绘制都交给 egui，与 Label 内部的做法一致
    egui::text_selection::LabelSelectionState::label_text_selection(
        ui,
        &response,
        rect.min,
        galley.clone(),
        base_color,
        egui::Stroke::NONE,
    );

    if response.double_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            return word_at(&galley, pos - rect.min);
        }
    }
    None
}

/// 取出 galley 中指定位置所在的词，按分隔符向两侧扩展。
///
/// 中文没有空格分词，连续的汉字会被整体取出——双击一段中文日志时，
/// 这通常正是想拿去搜的片段。
fn word_at(galley: &egui::Galley, pos: egui::Vec2) -> Option<String> {
    let chars: Vec<char> = galley.text().chars().collect();
    // ccursor.index 是字符偏移（不是字节偏移），恰好匹配 chars 的下标
    let idx = galley.cursor_from_pos(pos).ccursor.index.min(chars.len());

    let is_separator = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '[' | ']'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | '<'
                    | '>'
                    | ','
                    | ';'
                    | ':'
                    | '\''
                    | '"'
                    | '='
                    | '|'
                    | '/'
                    | '\\'
            )
    };

    let mut start = idx;
    while start > 0 && !is_separator(chars[start - 1]) {
        start -= 1;
    }
    let mut end = idx;
    while end < chars.len() && !is_separator(chars[end]) {
        end += 1;
    }
    if start >= end {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

/// 算出这一行里所有被检索命中的字节区间（升序、互不重叠）。
fn match_ranges(
    text: &str,
    query: &str,
    case_sensitive: bool,
    regex: Option<&regex::Regex>,
) -> Vec<Range<usize>> {
    // 正则模式：命中范围由表达式自己给出，不必再做大小写归一化
    if let Some(re) = regex {
        let mut out = Vec::new();
        let mut last = 0usize;
        for m in re.find_iter(text) {
            // 表达式可能写出重叠匹配，这里只取不重叠的部分
            if m.end() > m.start() && m.start() >= last {
                out.push(m.start()..m.end());
                last = m.end();
            }
        }
        return out;
    }

    // to_ascii_lowercase 不改变字节长度，可安全用于偏移换算
    let (hay, needle) = if case_sensitive {
        (text.to_string(), query.to_string())
    } else {
        (text.to_ascii_lowercase(), query.to_ascii_lowercase())
    };
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (idx, _) in hay.match_indices(&needle) {
        out.push(idx..idx + needle.len());
    }
    out
}

/// 把一行的样式拼成 LayoutJob：行首结构决定基础色，检索命中叠加底色。
///
/// 两套区间会重叠（比如要搜的词恰好就是 `INFO`），所以统一按"所有边界点切段"来做：
/// 切开之后每一小段要么整个属于某个结构分段、要么整个落在某个命中区间里，逐段定色即可。
/// 这样只需要一遍 Append，不必为两种高亮各写一套分段逻辑。
fn styled_job(
    text: &str,
    head: &LineHead,
    base: Color32,
    muted: Color32,
    font_id: &FontId,
    highlights: &[Range<usize>],
    dark: bool,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    if text.is_empty() {
        // 空行也要有一次 append：没有 section 的 job 量不出行高
        job.append("", 0.0, egui::TextFormat::simple(font_id.clone(), base));
        return job;
    }

    // 日期与时间连成一整段（含中间那个分隔符），整段用次要灰。
    // 曾经让日期再淡一层，浅色主题下对比度不足 3:1，读不清，已撤掉。
    let ts_range = match (&head.date, &head.time) {
        (Some(d), Some(t)) => Some(d.start..t.end),
        (Some(d), None) => Some(d.clone()),
        (None, Some(t)) => Some(t.clone()),
        (None, None) => None,
    };
    let level_range = head.level.as_ref().map(|(r, _)| r);
    let level_style = head.level_of().map(|lv| lv.style(dark));

    let mut cuts = vec![0usize, text.len()];
    for r in [ts_range.as_ref(), level_range].into_iter().flatten() {
        if r.start <= r.end && r.end <= text.len() {
            cuts.push(r.start);
            cuts.push(r.end);
        }
    }
    for h in highlights {
        if h.start <= h.end && h.end <= text.len() {
            cuts.push(h.start);
            cuts.push(h.end);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();

    let hit_bg = if dark {
        Color32::from_rgb(140, 110, 20)
    } else {
        Color32::from_rgb(255, 226, 130)
    };
    let hit_fg = if dark {
        Color32::from_rgb(255, 245, 220)
    } else {
        Color32::from_rgb(60, 40, 0)
    };

    // 切点与命中区间都是升序，双指针扫一遍就够，不必对每段重查命中列表
    let mut hi = 0usize;
    for w in cuts.windows(2) {
        let (s, e) = (w[0], w[1]);
        if s >= e {
            continue;
        }
        while hi < highlights.len() && highlights[hi].end <= s {
            hi += 1;
        }

        let mut fmt = egui::TextFormat::simple(font_id.clone(), base);
        if let (Some(st), Some(r)) = (&level_style, level_range) {
            if r.contains(&s) {
                fmt.color = st.fg;
                if let Some(bg) = st.bg {
                    fmt.background = bg;
                }
            }
        }
        if let Some(r) = &ts_range {
            if r.contains(&s) {
                fmt.color = muted;
            }
        }
        // 命中最后覆盖：搜的就是级别词时，看到的应该是命中而不是级别标签
        if hi < highlights.len() && highlights[hi].start <= s {
            fmt.color = hit_fg;
            fmt.background = hit_bg;
        }
        job.append(&text[s..e], 0.0, fmt);
    }
    job
}

fn floor_char_boundary(s: &str, max: usize) -> &str {
    if max >= s.len() {
        return s;
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

/// 命中数被保留上限截断时的状态栏文案。
///
/// 三个数字各回答一个问题：能跳转多少行、这些覆盖到第几行、全文一共多少处命中。
/// 关键在「到第几行」——它让用户知道文件后面那段到底有没有命中，
/// 而不是含糊的一句"结果不完整"，看完仍不知道是没命中还是没搜。
fn truncated_hits_label(navigable_lines: usize, reach_line: usize, total_hits: usize) -> String {
    format!("可跳转前 {navigable_lines} 行（到第 {reach_line} 行）；全文共 {total_hits} 处命中")
}

/// 主题三态循环：跟随系统 → 浅色 → 深色 → 跟随系统。
///
/// 抽成纯函数是为了能直接断言——塞在按钮点击回调里就只能靠手点了。
/// 之所以要三态而不是简单取反：取反的话一旦点过就再也回不到"跟随系统"。
fn next_theme(current: Option<bool>) -> Option<bool> {
    match current {
        None => Some(false),
        Some(false) => Some(true),
        Some(true) => None,
    }
}

fn human_size(n: usize) -> String {
    const KB: f64 = 1024.0;
    let n = n as f64;
    if n < KB {
        format!("{n:.0} B")
    } else if n < KB * KB {
        format!("{:.1} KB", n / KB)
    } else if n < KB * KB * KB {
        format!("{:.1} MB", n / KB / KB)
    } else {
        format!("{:.2} GB", n / KB / KB / KB)
    }
}

/// 测试共用的搭架子代码。
#[cfg(test)]
mod test_support {
    use super::*;

    /// 造一个带两个命中行的 app。
    ///
    /// `tag` 用于区分文件名：测试是并行执行的，若共用同一个临时文件，
    /// 会互相覆盖内容导致随机失败。
    pub fn app_with_hits(tag: &str) -> LogViewApp {
        let mut p = std::env::temp_dir();
        p.push(format!("logview_test_{tag}.log"));
        std::fs::write(&p, b"ERROR one\nplain\nERROR two\n").unwrap();

        let mut store = LogStore::open(p).unwrap();
        settle(&mut store);
        store.start_search(SearchRequest::new("ERROR", true, false));
        settle(&mut store);
        assert_eq!(store.matches().len(), 2, "前提：应有 2 个命中行");

        let mut app = LogViewApp::new();
        app.query = "ERROR".to_string();
        app.store = Some(store);
        // 这些测试不渲染帧，视口行数得自己给：mark_target 要求"当前行还在可见范围内"
        // 才认它，不渲染时 visible_rows 会停在默认值 1。760px 高的窗口约 40 行。
        app.visible_rows = 40;
        app
    }

    /// 等后台索引 / 检索结束
    pub fn settle(store: &mut LogStore) {
        for _ in 0..2000 {
            store.pump();
            if !store.indexing && !store.searching {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("后台任务超时");
    }

    /// 等 app 当前这一轮检索结束
    pub fn settle_app(app: &mut LogViewApp) {
        settle(app.store.as_mut().expect("app 应已打开文件"));
    }

    /// 造一个较大的 app：`lines` 行，每 101 行放一处「进度」命中，跨满整个文件。
    ///
    /// 命中要铺开到全文件，跳转类的问题才会暴露——只在前几行有命中是测不出来的。
    pub fn big_app(tag: &str, lines: usize) -> LogViewApp {
        let mut p = std::env::temp_dir();
        p.push(format!("logview_test_{tag}.log"));
        let mut data = String::new();
        for i in 0..lines {
            if i % 101 == 0 {
                data.push_str("进度 MARK\n");
            } else {
                data.push_str("plain line here\n");
            }
        }
        std::fs::write(&p, data).unwrap();

        let mut app = LogViewApp::new();
        app.store = Some(LogStore::open(p).unwrap());
        settle_app(&mut app);
        app.follow = false;
        app.visible_rows = 40;
        app
    }

    /// 带真实窗口尺寸的输入。不给屏幕尺寸的话视口是 0，滚动行为与真实运行不一致。
    pub fn raw_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1180.0, 760.0),
            )),
            ..Default::default()
        }
    }

    /// 渲染一帧日志区，返回本帧结束后视口顶部对应的行号
    pub fn frame(ctx: &egui::Context, app: &mut LogViewApp) -> usize {
        let _ = ctx.run(raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.log_area(ui));
        });
        app.top_line
    }
}

#[cfg(test)]
mod shortcut_tests {
    use super::test_support::app_with_hits;
    use super::*;

    fn press(
        ctx: &egui::Context,
        app: &mut LogViewApp,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) {
        let raw = egui::RawInput {
            // 必须一并设置：egui 会用 RawInput.modifiers 覆盖事件自带的修饰键，
            // 只设事件里那个的话，Shift+N 会被当成普通 n。
            modifiers,
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.handle_shortcuts(ctx));
    }

    /// 关键边界：搜索框有焦点时按字母必须当作普通输入，不能触发跳转，
    /// 否则用户根本没法在搜索框里打出 n 或 g。
    #[test]
    fn letter_keys_are_ignored_while_search_box_is_focused() {
        let ctx = egui::Context::default();
        let mut app = app_with_hits("focused");
        app.search_has_focus = true;
        let before = app.match_cursor;

        press(&ctx, &mut app, egui::Key::N, egui::Modifiers::NONE);
        assert_eq!(app.match_cursor, before, "搜索框有焦点时 n 不应跳转");
        assert!(app.pending_jump.is_none());
    }

    #[test]
    fn letter_keys_work_when_search_box_is_not_focused() {
        let ctx = egui::Context::default();
        let mut app = app_with_hits("nofocus");
        app.search_has_focus = false;

        assert_eq!(app.match_cursor, 0);
        press(&ctx, &mut app, egui::Key::N, egui::Modifiers::NONE);
        assert_eq!(app.match_cursor, 1, "无焦点时 n 应跳到下一个命中");
        assert!(app.pending_jump.is_some(), "跳转请求应已排队");
    }

    #[test]
    fn shift_n_goes_backwards() {
        let ctx = egui::Context::default();
        let mut app = app_with_hits("shiftn");
        app.search_has_focus = false;
        app.match_cursor = 1;

        press(&ctx, &mut app, egui::Key::N, egui::Modifiers::SHIFT);
        assert_eq!(app.match_cursor, 0, "Shift+N 应回到上一个命中");
    }

    #[test]
    fn slash_requests_search_focus() {
        let ctx = egui::Context::default();
        let mut app = app_with_hits("slash");
        app.search_has_focus = false;

        press(&ctx, &mut app, egui::Key::Slash, egui::Modifiers::NONE);
        assert!(app.focus_search, "/ 应请求聚焦搜索框");
    }

    /// `b` 标记的是"当前行"，还没定位过就用视口第一行——这是可预期的，
    /// 而且状态栏会把行号回报出来，所以不用猜自己标了哪一行。
    #[test]
    fn b_marks_the_viewport_top_when_nothing_was_located_yet() {
        let ctx = egui::Context::default();
        let mut app = super::test_support::big_app("markb", 500);
        app.search_has_focus = false;
        app.top_line = 3;

        press(&ctx, &mut app, egui::Key::B, egui::Modifiers::NONE);
        assert_eq!(app.marks.len(), 1, "b 应标记一行");
        assert_eq!(app.marks[0].bm.line, 3, "应标在视口第一行上");
        assert!(app.marks_dirty, "改动了就该落盘");

        press(&ctx, &mut app, egui::Key::B, egui::Modifiers::NONE);
        assert!(app.marks.is_empty(), "再按一次应取消");
    }

    /// 搜索跳转之后，"当前行"就是刚跳到的那一行：跳完直接 b 标它
    #[test]
    fn b_follows_the_last_jump() {
        let ctx = egui::Context::default();
        let mut app = app_with_hits("markjump");
        app.search_has_focus = false;
        app.top_line = 0;

        press(&ctx, &mut app, egui::Key::N, egui::Modifiers::NONE); // 跳到第 2 个命中（第 3 行）
        press(&ctx, &mut app, egui::Key::B, egui::Modifiers::NONE);
        assert_eq!(app.marks.len(), 1);
        assert_eq!(app.marks[0].bm.line, 2, "应标在刚跳到的命中行上");
    }

    /// F2 / ⇧F2 在标记之间前后走，到底了绕回另一端
    #[test]
    fn f2_walks_between_marks_and_wraps() {
        let ctx = egui::Context::default();
        let mut app = super::test_support::big_app("markf2", 500);
        app.search_has_focus = false;

        for line in [10usize, 100, 300] {
            app.top_line = line;
            press(&ctx, &mut app, egui::Key::B, egui::Modifiers::NONE);
        }
        assert_eq!(app.marks.len(), 3);

        app.cursor_line = Some(0);
        press(&ctx, &mut app, egui::Key::F2, egui::Modifiers::NONE);
        assert_eq!(app.cursor_line, Some(10), "F2 应跳到第一条");

        press(&ctx, &mut app, egui::Key::F2, egui::Modifiers::NONE);
        assert_eq!(app.cursor_line, Some(100));

        press(&ctx, &mut app, egui::Key::F2, egui::Modifiers::SHIFT);
        assert_eq!(app.cursor_line, Some(10), "⇧F2 应往回");

        press(&ctx, &mut app, egui::Key::F2, egui::Modifiers::SHIFT);
        assert_eq!(app.cursor_line, Some(300), "到头了绕回最后一条");
    }

    /// 改备注时键盘要整体让给输入框：这时按 b 是想打字母 b
    #[test]
    fn typing_in_the_note_editor_never_triggers_shortcuts() {
        let ctx = egui::Context::default();
        let mut app = app_with_hits("marknote");
        app.search_has_focus = false;
        app.top_line = 0;
        press(&ctx, &mut app, egui::Key::B, egui::Modifiers::NONE);
        assert_eq!(app.marks.len(), 1);

        app.editing_note = Some(app.marks[0].bm.offset);
        press(&ctx, &mut app, egui::Key::B, egui::Modifiers::NONE);
        assert_eq!(app.marks.len(), 1, "编辑备注时按 b 不该再标记一行");

        press(&ctx, &mut app, egui::Key::Escape, egui::Modifiers::NONE);
        assert!(app.editing_note.is_none(), "Esc 应收回编辑");
    }
}

#[cfg(test)]
mod defaults {
    use super::*;

    /// 打开日志先停在开头看起，需要盯实时输出时再手动勾上「跟随尾部」。
    #[test]
    fn follow_is_off_by_default() {
        assert!(
            !LogViewApp::new().follow,
            "跟随尾部默认为关：否则打开大日志会直接跳到末尾"
        );
    }
}

/// 命中数被保留上限截断时，状态栏要把"能跳到哪、覆盖到哪、一共多少"说清楚
#[cfg(test)]
mod status_label_tests {
    use super::*;

    #[test]
    fn truncated_label_reports_reach_and_total() {
        assert_eq!(
            truncated_hits_label(50_000, 50_000, 296_899),
            "可跳转前 50000 行（到第 50000 行）；全文共 296899 处命中"
        );
    }
}

/// 双击取词与在搜索框里输入，对视图的期望不一样：
/// 前者不该把读到一半的人拽走，后者要跳到第一个命中。
#[cfg(test)]
mod search_view_tests {
    use super::test_support::{app_with_hits, big_app, frame, settle_app};
    use super::*;

    /// 文件内容固定为三行，ERROR 出现在第 1、3 行（0 起算的 0 和 2）
    #[test]
    fn word_search_keeps_the_view_and_aligns_the_cursor() {
        let mut app = app_with_hits("word_view");
        app.top_line = 1;

        app.search_for("ERROR".to_string());
        settle_app(&mut app);
        app.apply_search_outcome();

        assert!(app.pending_jump.is_none(), "双击取词不该移动视图");
        assert_eq!(app.match_cursor, 1, "游标应落在视口下方的那个命中上");
    }

    /// 命中全在视口之上时，游标停在上一个命中，n 不会掉头跳回文件开头
    #[test]
    fn word_search_below_all_matches_stays_on_the_last_one() {
        let mut app = app_with_hits("word_tail");
        app.top_line = 100_000;

        app.search_for("ERROR".to_string());
        settle_app(&mut app);
        app.apply_search_outcome();

        assert!(app.pending_jump.is_none());
        assert_eq!(app.match_cursor, 1, "应停在最后一个命中，而不是回到第一个");
    }

    /// 在搜索框里输入仍然要跳到第一个命中，这是原有的行为
    #[test]
    fn typed_search_still_jumps_to_the_first_match() {
        let mut app = app_with_hits("typed");
        app.match_cursor = 1;

        app.run_search(OnSearchDone::GotoFirstMatch);
        settle_app(&mut app);
        app.apply_search_outcome();

        assert_eq!(app.match_cursor, 0);
        assert_eq!(
            app.pending_jump,
            Some(0),
            "搜索框输入应跳到第一个命中所在行"
        );
    }

    /// 视口顶部的行号必须真的跟着滚动走——上面那条"对齐到当前位置"
    /// 全靠它；它要是一直是 0，双击取词就还是会跳回文件开头。
    #[test]
    fn top_line_follows_the_scroll_offset() {
        let mut app = big_app("topline", 10_000);

        let ctx = egui::Context::default();
        let at = |app: &mut LogViewApp, offset: f32| {
            app.scroll_offset = offset;
            frame(&ctx, app)
        };

        let a = at(&mut app, 1_000.0);
        let b = at(&mut app, 4_000.0);
        assert!(a > 0, "视口顶部行号应随滚动变化，实际为 {a}");
        assert!(
            (b as f32) > 3.5 * a as f32 && (b as f32) < 4.5 * a as f32,
            "偏移翻四倍，行号也应接近翻四倍：a={a} b={b}"
        );
    }

    /// 跳到某个命中行之后，那一行必须真的落在可视范围里。
    ///
    /// 行号↔像素的换算一旦用错行距，误差会随行号累积：二十万行的日志里
    /// 一处命中就能差出几万像素。用户看到的就是"按 n 跳过去了，屏幕上却没有那个命中"。
    #[test]
    fn jump_lands_the_target_line_inside_the_viewport() {
        let gap = jump_gap(1.0, "jump_far", true);
        assert!(gap <= 20, "目标行应落在可视范围内，实际相差 {gap} 行");
    }

    /// 跳转精度不能依赖屏幕缩放：macOS 的 2x 屏、Linux 的 1.25 / 1.5 分数缩放
    /// 都在这条里。缩放只改变物理像素，逻辑坐标下的行距关系必须保持一致。
    #[test]
    fn jump_lands_the_target_line_at_hidpi_scales() {
        for ppp in [1.25f32, 1.5, 2.0] {
            let gap = jump_gap(ppp, &format!("jump_ppp{ppp}"), false);
            assert!(
                gap <= 20,
                "ppp={ppp} 时目标行应落在可视范围内，实际相差 {gap} 行"
            );
        }
    }

    /// 造一个 2 万行的日志，搜到命中后跳到其中一处，返回目标行离视口顶部的行数。
    ///
    /// `near_end` 为真时取倒数第三个命中：它贴着文件末尾，会撞上"滚到底"的截断，
    /// 目标被截在视口偏下的位置——那是正常现象，与行距换算无关。
    /// 为假时取文件中部的命中；缩放大时测试脚手架的视口高度会虚高，
    /// 更容易触发截断，所以 HiDPI 用例取中部。
    fn jump_gap(ppp: f32, tag: &str, near_end: bool) -> usize {
        let mut app = big_app(tag, 20_000);
        app.query = "进度".to_string();
        app.run_search(OnSearchDone::GotoFirstMatch);
        settle_app(&mut app);
        app.apply_search_outcome();

        let target = {
            let s = app.store.as_ref().unwrap();
            let matches = s.matches();
            assert!(matches.len() > 100, "前提：应有一批散布全文件的命中");
            let idx = if near_end {
                matches.len() - 3
            } else {
                matches.len() / 2
            };
            matches[idx] as usize
        };

        app.jump_to_line(target);
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        let top = frame(&ctx, &mut app);

        assert!(top <= target, "ppp={ppp} 时跳转不该越过目标行");
        target - top
    }
}

#[cfg(test)]
mod word_tests {
    use super::*;
    use std::sync::Arc;

    fn setup_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        // egui 的字体要等首次 run 之后才可用
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn layout(ctx: &egui::Context, text: &str) -> Arc<egui::Galley> {
        let font_id = egui::FontId::monospace(14.0);
        ctx.fonts(|f| {
            f.layout_job(egui::text::LayoutJob::single_section(
                text.to_string(),
                egui::TextFormat::simple(font_id, egui::Color32::WHITE),
            ))
        })
    }

    /// ASCII 字符宽度；等宽字体下汉字占两倍宽度
    fn cw(ctx: &egui::Context) -> f32 {
        ctx.fonts(|f| f.glyph_width(&egui::FontId::monospace(14.0), 'a'))
    }

    #[test]
    fn picks_identifier_around_click() {
        let ctx = setup_ctx();
        let galley = layout(&ctx, "user=zhangsan status=OK");
        // 落在 zhangsan 中间
        let pos = egui::vec2(cw(&ctx) * 7.0, 5.0);
        assert_eq!(word_at(&galley, pos).as_deref(), Some("zhangsan"));
    }

    /// 汉字各占两个 ASCII 宽度，若把字符索引当字节索引算，这里会错位
    #[test]
    fn picks_chinese_run_as_one_word() {
        let ctx = setup_ctx();
        let galley = layout(&ctx, "处理订单失败 orderId=123");
        let pos = egui::vec2(cw(&ctx) * 1.0, 5.0);
        assert_eq!(word_at(&galley, pos).as_deref(), Some("处理订单失败"));
    }

    #[test]
    fn picks_value_after_equals_sign() {
        let ctx = setup_ctx();
        let galley = layout(&ctx, "orderId=12345 done");
        let pos = egui::vec2(cw(&ctx) * 9.0, 5.0);
        assert_eq!(word_at(&galley, pos).as_deref(), Some("12345"));
    }

    #[test]
    fn returns_none_when_nothing_but_separators() {
        let ctx = setup_ctx();
        let galley = layout(&ctx, "     ");
        assert_eq!(word_at(&galley, egui::vec2(1.0, 5.0)), None);
    }
}

/// 行首结构的识别，以及一条曾经的误判：
/// 原先"整行包含 ERROR 就给整行染色"，正文里提到 ERROR 也会被当成错误行。
#[cfg(test)]
mod line_head_tests {
    use super::*;

    fn job_for(line: &str, query: &str) -> egui::text::LayoutJob {
        styled_job(
            line,
            &parse_line_head(line),
            Color32::BLACK,
            muted_text(false),
            &FontId::monospace(12.0),
            &match_ranges(line, query, true, None),
            false,
        )
    }

    /// 把 LayoutJob 的各段拼回来
    fn sections_text(job: &egui::text::LayoutJob) -> String {
        job.sections
            .iter()
            .map(|s| &job.text[s.byte_range.clone()])
            .collect()
    }

    fn slice<'a>(line: &'a str, r: Option<&Range<usize>>) -> Option<&'a str> {
        r.map(|r| &line[r.clone()])
    }

    #[test]
    fn parses_timestamp_and_level() {
        let line = "2026-09-11 14:33:41.561 INFO c.s.pspace.X - hello";
        let head = parse_line_head(line);
        assert_eq!(slice(line, head.date.as_ref()), Some("2026-09-11"));
        assert_eq!(slice(line, head.time.as_ref()), Some("14:33:41.561"));
        let (r, lv) = head.level.clone().expect("应识别出级别");
        assert_eq!(&line[r], "INFO");
        assert_eq!(lv, Level::Info);
    }

    /// 核心回归：正文里的 ERROR 不是级别。
    /// 旧实现用 `head.contains("ERROR")` 判断，这一行会被整行染成错误色。
    #[test]
    fn level_word_inside_the_message_is_not_the_level() {
        let line = "2026-09-11 14:33:41.561 INFO c.s.X - 检测到 3 个 ERROR 已忽略";
        assert_eq!(parse_line_head(line).level_of(), Some(Level::Info));

        // 行首结构里根本没有级别词时，正文提到多少级别词都不认
        let no_level = "2026-09-11 14:33:41.561 c.s.X - 检测到 3 个 ERROR 已忽略";
        assert_eq!(parse_line_head(no_level).level_of(), None);
    }

    /// 中文开头的行：整段中文会算成一个 token，正文里的 ERROR 不能因此溜进来
    #[test]
    fn chinese_prefix_stops_the_level_lookup() {
        assert_eq!(parse_line_head("正在检查 ERROR 日志").level_of(), None);
        assert_eq!(
            parse_line_head("2026-09-11 14:33:41.561 检测到 ERROR 已忽略").level_of(),
            None
        );
    }

    /// 必须整个 token 相等：ERRORS / ERROR_CODE=5 都不是级别
    #[test]
    fn level_must_be_a_whole_token() {
        assert_eq!(
            parse_line_head("ERROR 已处理").level_of(),
            Some(Level::Error)
        );
        assert_eq!(parse_line_head("ERRORS 已处理").level_of(), None);
        assert_eq!(parse_line_head("ERROR_CODE=5").level_of(), None);
        // 只数头几个 token，正文里的词够不着
        assert_eq!(
            parse_line_head("c.s.X - 检测到 ERROR 已忽略").level_of(),
            None
        );
    }

    /// 常见排布都要认：无时间戳、带方括号、线程名在中间、逗号小数秒
    #[test]
    fn tolerates_common_layouts() {
        assert_eq!(
            parse_line_head("INFO 服务已启动").level_of(),
            Some(Level::Info)
        );
        assert_eq!(
            parse_line_head("WARN 磁盘剩余 5%").level_of(),
            Some(Level::Warn)
        );

        let bracketed = "[2026-09-11 14:33:41,561] [main] ERROR c.s.X - 慢查询";
        let head = parse_line_head(bracketed);
        assert_eq!(
            slice(bracketed, head.date.as_ref()),
            Some("2026-09-11"),
            "带方括号与逗号小数秒也要认出日期"
        );
        assert_eq!(head.level_of(), Some(Level::Error));
    }

    /// 时间戳之后出现的第一个级别词才算数，别被正文里更靠后的别的级别盖过去
    #[test]
    fn picks_the_level_right_after_the_timestamp() {
        let line = "2026-09-11 14:33:41.561 WARN c.s.X - 上一行报的是 ERROR，这里是 WARN";
        assert_eq!(parse_line_head(line).level_of(), Some(Level::Warn));
    }

    /// 分段必须完整覆盖原行、不丢字不重复 —— 这同时守住了"不切在汉字中间"。
    /// 老实现是"取行首 200 字节"，中文日志里第 200 字节常常正落在汉字内部，
    /// 直接切片 panic（打开文件即闪退就是这个原因）；现在不再对整行做字节切片。
    #[test]
    fn sections_cover_the_line_exactly_once() {
        let line = format!("ERROR {}{}", "a".repeat(192), "查找");
        assert!(!line.is_char_boundary(200), "前提：构造的行应跨越字符边界");
        assert_eq!(sections_text(&job_for(&line, "查找")), line);
    }

    /// 命中区间压在时间戳或级别上时（两套区间重叠），分段仍然完整
    #[test]
    fn overlapping_highlight_and_structure_stay_consistent() {
        for line in [
            "2026-09-11 14:33:41.561 INFO c.s.X - INFO 又出现一次",
            "2026-09-11 14:33:41.561 ERROR 时间戳里也有 14:33",
            "没有可识别结构的一行中文日志",
        ] {
            for query in ["INFO", "ERROR", "14:33", "2026-09-11", "不存在"] {
                let job = job_for(line, query);
                assert_eq!(sections_text(&job), line, "query={query} line={line}");
            }
        }
    }

    /// WCAG 相对亮度
    fn luminance(c: Color32) -> f32 {
        let ch = |v: u8| {
            let s = v as f32 / 255.0;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }

    /// WCAG 对比度，1:1 到 21:1
    fn contrast(a: Color32, b: Color32) -> f32 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// 次要文字可以弱，但不能弱到读不动。
    ///
    /// 这条是老许用出来的：早先日期取 `weak_text_color().linear_multiply(0.62)`、
    /// 时间取 `weak_text_color()`，浅色主题下对比度只有 3:1 上下，他直接说"看不清"。
    /// 靠眼睛发现这类问题太晚，所以把下限钉成断言。
    #[test]
    fn text_meets_wcag_aa_on_both_themes() {
        for dark in [false, true] {
            let visuals = if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            };
            let bg = visuals.panel_fill;
            let name = if dark { "深色" } else { "浅色" };

            let ratio = contrast(muted_text(dark), bg);
            assert!(
                ratio >= 4.5,
                "{name}主题下时间戳对比度只有 {ratio:.2}:1，低于 WCAG AA 的 4.5:1"
            );

            let body = contrast(visuals.text_color(), visuals.extreme_bg_color);
            assert!(body >= 4.5, "{name}主题下正文对比度只有 {body:.2}:1");

            // 主按钮：白字要读得出（4.5:1），按钮本身相对背景要看得出来
            // （非文本 UI 元素按 WCAG 只要 3:1，但它是整屏唯一的主操作，别让它糊进背景）
            let primary = hex(PRIMARY_FILL);
            let on_fill = contrast(Color32::WHITE, primary);
            assert!(
                on_fill >= 4.5,
                "{name}主题下主按钮的白字对比度只有 {on_fill:.2}:1"
            );
            let vs_bg = contrast(primary, bg);
            assert!(
                vs_bg >= 3.0,
                "{name}主题下主按钮相对背景只有 {vs_bg:.2}:1，作为 UI 元素应 ≥ 3:1"
            );

            for lv in [Level::Error, Level::Warn, Level::Info, Level::Debug] {
                let st = lv.style(dark);
                let r = contrast(st.fg, st.bg.unwrap_or(bg));
                assert!(
                    r >= 4.5,
                    "{name}主题下 {lv:?} 级别的文字对比度只有 {r:.2}:1"
                );
            }
        }
    }

    /// 时间/日期/级别三段各自着色，且互不重叠
    #[test]
    fn styles_are_applied_to_the_expected_ranges() {
        let line = "2026-09-11 14:33:41.561 ERROR c.s.X - boom";
        let job = job_for(line, "");
        let head = parse_line_head(line);

        let color_at = |at: usize| {
            job.sections
                .iter()
                .find(|s| s.byte_range.contains(&at))
                .map(|s| s.format.color)
        };
        let date = color_at(head.date.as_ref().unwrap().start).unwrap();
        let time = color_at(head.time.as_ref().unwrap().start).unwrap();
        let level = head.level.as_ref().unwrap().0.start;
        let lv_sec = job
            .sections
            .iter()
            .find(|s| s.byte_range.contains(&level))
            .unwrap();
        assert_eq!(lv_sec.format.color, Level::Error.style(false).fg);
        assert_eq!(
            lv_sec.format.background,
            Level::Error.style(false).bg.unwrap()
        );
        assert_eq!(date, time, "日期与时间应同为可读的次要灰");
        assert_ne!(time, Color32::BLACK, "时间不该用正文色");
    }
}

/// 主题按钮的三态循环。抽成纯函数就是为了这条断言能直接跑。
#[cfg(test)]
mod theme_tests {
    use super::*;

    /// 跟随系统 → 浅色 → 深色 → 跟随系统，一圈要能回到原点。
    /// 若写成简单取反，点过一次就再也回不到"跟随系统"。
    #[test]
    fn theme_cycles_through_all_three_states() {
        assert_eq!(next_theme(None), Some(false), "跟随系统之后应到浅色");
        assert_eq!(next_theme(Some(false)), Some(true), "浅色之后应到深色");
        assert_eq!(next_theme(Some(true)), None, "深色之后应回到跟随系统");

        let back = next_theme(next_theme(next_theme(None)));
        assert_eq!(back, None, "三次点击应回到起点");
    }
}

#[cfg(test)]
mod mark_tests {
    use super::test_support::settle_app;
    use super::*;
    use std::path::Path;

    /// 造一个临时日志，并清掉可能残留的侧车文件——上一次跑剩下的侧车会让断言莫名其妙。
    fn temp_log(tag: &str, content: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("logview_marks_{tag}.log"));
        let _ = std::fs::remove_file(bookmarks::sidecar_path(&p));
        std::fs::write(&p, content).unwrap();
        p
    }

    /// 走真实打开路径（`open_path`），于是侧车的读取与逐条校验都真的跑了一遍
    fn open(path: &Path) -> LogViewApp {
        let mut app = LogViewApp::new();
        app.open_path(path.to_path_buf());
        settle_app(&mut app);
        // 这些测试不渲染帧，视口行数得自己给：mark_target 只在"当前行仍然可见"时才认它
        app.visible_rows = 40;
        app
    }

    fn drop_sidecar(path: &Path) {
        let _ = std::fs::remove_file(bookmarks::sidecar_path(path));
    }

    /// 标记要落到日志旁边的侧车文件里，重开同一个文件时读回来，并且仍然定位得上
    #[test]
    fn marks_round_trip_through_the_sidecar() {
        let p = temp_log("roundtrip", "one\ntwo\nthree\nfour\n");
        let mut app = open(&p);
        app.cursor_line = Some(2);
        app.toggle_mark();
        assert_eq!(app.marks.len(), 1);
        assert_eq!(app.marks[0].bm.line, 2);
        assert_eq!(app.marks[0].bm.head, "three", "指纹取的是那一行的内容");
        app.save_marks();

        let sidecar = bookmarks::sidecar_path(&p);
        assert!(sidecar.exists(), "标记应写在日志旁边的侧车文件里");
        let text = std::fs::read_to_string(&sidecar).unwrap();
        assert!(text.contains("\"head\":\"three\""), "侧车内容：{text}");

        let again = open(&p);
        assert_eq!(again.marks.len(), 1, "重开应读回上次的标记");
        assert_eq!(again.marks[0].anchor, AnchorState::Fresh);
        assert_eq!(again.marks[0].bm.head, "three");
        assert!(again.show_marks, "有上次留下的标记时面板应自动展开");

        drop_sidecar(&p);
    }

    /// 那一行被改写之后，锚点必须判失效、并且**不跳**——静默跳到错的行上是最坏的结果
    #[test]
    fn a_rewritten_line_is_stale_and_never_jumped_to() {
        let p = temp_log("stale", "alpha line\nbeta line\n");
        let sidecar = bookmarks::sidecar_path(&p);
        let stale = Bookmark {
            offset: 0,
            line: 0,
            head: "这里原本是别的内容".to_string(),
            note: String::new(),
            at: 0,
        };
        bookmarks::save(&sidecar, std::slice::from_ref(&stale)).unwrap();

        let mut app = open(&p);
        assert_eq!(app.marks.len(), 1);
        assert_eq!(
            app.marks[0].anchor,
            AnchorState::Stale,
            "偏移处的内容对不上就该判失效"
        );

        app.cursor_line = None;
        app.top_line = 0;
        app.goto_mark(1);
        assert!(app.pending_jump.is_none(), "失效的标记不该跳");

        drop_sidecar(&p);
    }

    /// 日志被整体重写（轮转那种）之后，按原文回找能把锚点挪到新位置
    #[test]
    fn refind_moves_anchors_after_the_file_changed() {
        let p = temp_log("refind", "first\nsecond\nthird\n");
        let mut app = open(&p);
        app.cursor_line = Some(1); // second
        app.toggle_mark();
        app.save_marks();
        // Windows 下映射还活着时，写这个文件会被拒（README 已知限制里那条），
        // 所以先把 app 放掉再重写文件
        drop(app);

        // 前面插进来两行：second 从第 2 行挪到了第 4 行
        std::fs::write(&p, "x\ny\nfirst\nsecond\nthird\n").unwrap();

        let mut again = open(&p);
        assert_eq!(again.marks.len(), 1);
        assert_eq!(
            again.marks[0].anchor,
            AnchorState::Stale,
            "偏移已经漂了，先判失效而不是硬跳"
        );

        again.refind_marks(None);
        assert_eq!(
            again.marks[0].anchor,
            AnchorState::Fresh,
            "按原文应能找回来"
        );
        assert_eq!(again.marks[0].bm.line, 3, "行号也要更新成现在的");

        drop_sidecar(&p);
    }

    /// 过滤视图里，标记所在的行可能根本不在结果里：这时要先把过滤关掉，
    /// 否则 pending_jump 会被当成"列表里的第 N 行"，跳过去落在别的行上。
    #[test]
    fn jumping_to_a_mark_outside_the_filter_turns_it_off() {
        let p = temp_log("filter", "ERROR one\nplain\nstill plain\nERROR two\n");
        let mut app = open(&p);
        app.cursor_line = Some(1);
        app.toggle_mark();

        app.query = "ERROR".to_string();
        app.run_search(OnSearchDone::GotoFirstMatch);
        settle_app(&mut app);
        app.only_matched = true;
        app.top_line = 0;
        app.cursor_line = None;

        app.jump_to_file_line(1);
        assert!(!app.only_matched, "那行不在命中里，应自动关掉过滤");
        assert_eq!(app.pending_jump, Some(1), "关掉过滤后可以直接用文件行号");

        // 反过来：命中里的行仍然走"列表位置"
        app.only_matched = true;
        app.jump_to_file_line(3);
        assert!(app.only_matched, "命中行不该把过滤关掉");
        assert_eq!(app.pending_jump, Some(1), "第 4 行是第 2 个命中");

        drop_sidecar(&p);
    }

    /// 备注落到那一条标记上，然后跟着侧车一起保存
    #[test]
    fn a_note_is_committed_and_saved_with_the_mark() {
        let p = temp_log("note", "one\ntwo\n");
        let mut app = open(&p);
        app.cursor_line = Some(0);
        app.toggle_mark();

        app.note_buf = "OOM 前最后一条 ERROR".to_string();
        app.editing_note = Some(app.marks[0].bm.offset);
        app.commit_note();
        assert_eq!(app.marks[0].bm.note, "OOM 前最后一条 ERROR");
        assert!(app.editing_note.is_none(), "提交后应退出编辑");
        assert!(app.marks_dirty);

        app.save_marks();
        let again = open(&p);
        assert_eq!(again.marks[0].bm.note, "OOM 前最后一条 ERROR");

        drop_sidecar(&p);
    }

    /// 面板点击与 F2 都要把目标行记下来（日志区靠它铺紫底），
    /// 而"选光标"类动作（搜索跳转、g/G、点行号）该把它收掉。
    #[test]
    fn the_mark_highlight_tracks_the_last_mark_jump() {
        let p = temp_log("focus", "one\ntwo\nthree\nfour\n");
        let mut app = open(&p);
        app.cursor_line = Some(2);
        app.toggle_mark();
        assert_eq!(app.focused_mark, Some(2), "刚按 b 标下的那条就是当前那条");

        // ① 面板点一条、F2 跳一条 → 高亮跟过去
        app.focused_mark = None; // 先清掉，模拟"之前被别的定位动作收走过"
        app.jump_to_file_line(2);
        assert_eq!(app.focused_mark, Some(2), "面板点击要聚焦那一行");
        app.focused_mark = None;
        app.goto_mark(1);
        assert_eq!(app.focused_mark, Some(2), "F2 跳到的那条也要聚焦");

        // ② 搜索跳转 → 收掉（视线已经移到命中上了）
        app.jump_to_file_line(2);
        app.query = "one".to_string();
        app.run_search(OnSearchDone::GotoFirstMatch);
        settle_app(&mut app);
        // 检索完成后由每帧的 apply_search_outcome 决定去向（测试不渲染帧，得手动走这一步）
        app.apply_search_outcome();
        assert_eq!(app.focused_mark, None, "搜索跳转之后不该留着标记高亮");

        // ③ g 跳开头 → 收掉
        app.jump_to_file_line(2);
        app.jump_to_line(0);
        assert_eq!(app.focused_mark, None, "g 跳开头之后不该留着标记高亮");

        // ④ 点行号 → 收掉（那是选光标，不是选标记）
        app.jump_to_file_line(2);
        app.select_line(1);
        assert_eq!(app.focused_mark, None, "点行号之后不该留着标记高亮");

        drop_sidecar(&p);
    }

    /// 取消、删除、换文件之后都不能留下高亮——否则日志区里有一行没有标记的紫底，
    /// 看着就像标记还在。
    #[test]
    fn removing_a_mark_clears_its_highlight() {
        let p = temp_log("focus_remove", "one\ntwo\nthree\n");
        let mut app = open(&p);

        // 再按一次 b 取消
        app.cursor_line = Some(1);
        app.toggle_mark();
        assert_eq!(app.focused_mark, Some(1));
        app.toggle_mark();
        assert!(app.marks.is_empty());
        assert_eq!(app.focused_mark, None, "取消标记后高亮要一起收掉");

        // 面板条目上的 ×
        app.cursor_line = Some(2);
        app.toggle_mark();
        let offset = app.marks[0].bm.offset;
        app.remove_mark(offset);
        assert!(app.marks.is_empty());
        assert_eq!(app.focused_mark, None, "删除标记后高亮要一起收掉");

        // 换文件
        app.jump_to_file_line(0);
        let other = temp_log("focus_other", "alpha\nbeta\n");
        app.open_path(other.clone());
        settle_app(&mut app);
        assert_eq!(app.focused_mark, None, "换文件后不该留着上一份文件的高亮");

        drop_sidecar(&p);
        drop_sidecar(&other);
    }

    /// 铺底的优先级：同一行既是命中又是刚跳到的标记时显示成标记——
    /// 那才是用户刚点的动作，而"当前命中"只是检索的副产品。
    #[test]
    fn the_mark_wins_over_the_current_hit() {
        assert_eq!(row_bg_kind(5, Some(5), Some(5)), RowBg::Mark);
        assert_eq!(row_bg_kind(5, Some(5), None), RowBg::Mark);
        assert_eq!(row_bg_kind(5, None, Some(5)), RowBg::Match);
        assert_eq!(
            row_bg_kind(5, Some(4), Some(5)),
            RowBg::Match,
            "标记在别的行上时不该抢这一行"
        );
        assert_eq!(row_bg_kind(5, Some(4), Some(6)), RowBg::None);
    }
}
