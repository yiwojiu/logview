use crate::logstore::{encoding_name, LogStore, SearchRequest};
use eframe::egui;
use egui::{Color32, FontId, RichText, ScrollArea, TextEdit, TextStyle};
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

    last_refresh: Instant,
    buf: String,
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
        let cur = self.match_cursor as isize + delta;
        let cur = cur.clamp(0, n as isize - 1) as usize;
        self.match_cursor = cur;
        let line = s.matches()[cur] as usize;
        if self.only_matched {
            self.pending_jump = Some(cur);
        } else {
            self.pending_jump = Some(line);
        }
    }

    /// 跳到指定位置（同时退出跟随，否则会被 tail 拉回底部）
    fn jump_to_line(&mut self, idx: usize) {
        self.follow = false;
        self.pending_jump = Some(idx);
    }

    /// 全局快捷键。
    ///
    /// 裸字母键只在搜索框没有焦点时才作为快捷键，否则会抢走正常输入。
    /// 修饰键组合（⌘F / ⌘O）任何时候都生效。
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};

        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            if let Some(p) = rfd::FileDialog::new().pick_file() {
                self.open_path(p);
            }
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
            } else {
                self.jump_to_line(0);
            }
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
        egui::CentralPanel::default().show(ctx, |ui| self.log_area(ui));

        // 快捷键放在各面板之后：此时 search_has_focus 已是本帧的最新状态
        self.handle_shortcuts(ctx);

        // 定时重绘：索引进度、文件变化检测与跟随刷新都依赖它
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

impl LogViewApp {
    fn toolbar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("打开文件").clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_file() {
                    self.open_path(p);
                }
            }

            if let Some(s) = &self.store {
                ui.label(
                    RichText::new(s.path_str())
                        .monospace()
                        .color(ui.visuals().weak_text_color()),
                );
            } else {
                ui.label(
                    RichText::new("未打开文件 — 点按钮选择，或把日志文件拖进窗口")
                        .color(ui.visuals().weak_text_color()),
                );
            }

            ui.separator();
            let dark_label = match self.dark {
                Some(true) => "浅色",
                Some(false) => "深色",
                None => "主题",
            };
            if ui.button(dark_label).clicked() {
                let now_dark = self.dark.unwrap_or_else(|| ui.visuals().dark_mode);
                self.dark = Some(!now_dark);
            }
        });

        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            let resp = ui.add(
                TextEdit::singleline(&mut self.query)
                    .id(egui::Id::new(SEARCH_ID))
                    .hint_text(format!("搜索（{CMD}F 或 / 聚焦，n / N 跳转）"))
                    .desired_width(320.0),
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
            let om = ui.checkbox(&mut self.only_matched, "只显示匹配行");
            if om.changed() {
                self.scroll_offset = 0.0;
                self.pending_jump = Some(0);
            }
            ui.checkbox(&mut self.follow, "跟随尾部")
                .on_hover_text("类似 tail -f，自动滚到最新一行");
            ui.checkbox(&mut self.wrap, "自动换行");

            let matches = self.store.as_ref().map(|s| s.matches().len()).unwrap_or(0);
            ui.separator();
            if ui.button("◀").clicked() {
                self.goto_match(-1);
            }
            if ui.button("▶").clicked() {
                self.goto_match(1);
            }
            if self.query.is_empty() {
                ui.label(RichText::new("").weak());
            } else {
                ui.label(format!("{}/{}", self.match_cursor + 1, matches.max(1)));
            }
            if ui.button("清空").clicked() {
                self.query.clear();
                if let Some(s) = &mut self.store {
                    s.clear_search();
                }
            }

            let _ = ui.button("?").on_hover_text(format!(
                "快捷键\n\
                 {CMD}F 或 / — 聚焦搜索框\n\
                 n / N — 下 / 上一个命中\n\
                 g / G — 跳到开头 / 末尾\n\
                 Esc — 清空检索\n\
                 {CMD}O — 打开文件\n\
                 双击行内文字 — 就地搜索该词，视图不移动"
            ));
        });
        ui.add_space(4.0);
        let _ = ctx;
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
                    ui.label(
                        RichText::new(format!("{n}+ 匹配（已达上限，结果不完整）"))
                            .color(ui.visuals().warn_fg_color),
                    );
                } else {
                    ui.label(format!("{n} 匹配"));
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
        let regex = self.highlight_regex.as_ref();
        let buf = &mut self.buf;
        let query = self.query.clone();
        let cs = self.case_sensitive;
        let only = self.only_matched;
        let wrap = self.wrap;
        let store_ref = &self.store;
        // 双击行内文字要发起的检索；闭包内拿不到 &mut self，先收集、结束后再处理
        let mut search_word: Option<String> = None;

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
                        if current_line == Some(line_idx) {
                            // 与命中词的黄色高亮区分开，这里用冷色整行铺底
                            let bg = if ui.visuals().dark_mode {
                                egui::Color32::from_rgb(30, 44, 62)
                            } else {
                                egui::Color32::from_rgb(226, 240, 253)
                            };
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    ui.cursor().min,
                                    egui::vec2(ui.available_width(), row_h),
                                ),
                                0.0,
                                bg,
                            );
                        }
                        ui.allocate_ui(egui::vec2(ui.available_width(), row_h), |ui| {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [gutter_w, row_h],
                                    egui::Label::new(
                                        RichText::new(format!("{}", line_idx + 1))
                                            .monospace()
                                            .color(ui.visuals().weak_text_color()),
                                    ),
                                );
                                if let Some(word) = render_content(ui, buf, &query, cs, wrap, regex)
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

        if let Some(word) = search_word {
            self.search_for(word);
        }

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
    }
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
    query: &str,
    case_sensitive: bool,
    wrap: bool,
    regex: Option<&regex::Regex>,
) -> Option<String> {
    let mut shown = text;
    if shown.len() > MAX_RENDER_CHARS {
        shown = floor_char_boundary(shown, MAX_RENDER_CHARS);
    }

    let base_color = level_color(ui, shown).unwrap_or_else(|| ui.visuals().text_color());
    let font_id: FontId = TextStyle::Monospace.resolve(ui.style());

    let mut job = if query.is_empty() {
        egui::text::LayoutJob::single_section(
            shown.to_string(),
            egui::TextFormat::simple(font_id.clone(), base_color),
        )
    } else {
        highlighted_job(
            shown,
            query,
            case_sensitive,
            base_color,
            &font_id,
            ui.visuals().dark_mode,
            regex,
        )
    };
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

fn highlighted_job(
    text: &str,
    query: &str,
    case_sensitive: bool,
    base_color: Color32,
    font_id: &FontId,
    dark: bool,
    regex: Option<&regex::Regex>,
) -> egui::text::LayoutJob {
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
    let mut job = egui::text::LayoutJob::default();
    let push = |job: &mut egui::text::LayoutJob, s: &str, hl: bool| {
        let mut f = egui::TextFormat::simple(font_id.clone(), if hl { hit_fg } else { base_color });
        if hl {
            f.background = hit_bg;
        }
        job.append(s, 0.0, f);
    };

    // 正则模式：高亮范围由表达式自己给出，不必再做大小写归一化
    if let Some(re) = regex {
        let mut last = 0usize;
        for m in re.find_iter(text) {
            if m.start() > last {
                push(&mut job, &text[last..m.start()], false);
            }
            if m.end() > m.start() {
                push(&mut job, &text[m.start()..m.end()], true);
            }
            last = last.max(m.end());
        }
        if last < text.len() {
            push(&mut job, &text[last..], false);
        }
        return job;
    }

    // to_ascii_lowercase 不改变字节长度，可安全用于偏移换算
    let (hay, needle) = if case_sensitive {
        (text.to_string(), query.to_string())
    } else {
        (text.to_ascii_lowercase(), query.to_ascii_lowercase())
    };

    if needle.is_empty() {
        push(&mut job, text, false);
        return job;
    }

    let mut last = 0usize;
    for (idx, _) in hay.match_indices(&needle) {
        if idx > last {
            push(&mut job, &text[last..idx], false);
        }
        push(&mut job, &text[idx..idx + needle.len()], true);
        last = idx + needle.len();
    }
    if last < text.len() {
        push(&mut job, &text[last..], false);
    }
    job
}

/// 按日志级别给整行上色
fn level_color(ui: &egui::Ui, line: &str) -> Option<Color32> {
    // 只取行首一小段做级别判定即可。截断必须落在字符边界上：
    // 中文日志里第 200 个字节经常正好落在一个汉字的中间，直接切片会 panic。
    let head = floor_char_boundary(line, 200);
    let v = ui.visuals();
    if head.contains("FATAL") || head.contains("ERROR") || head.contains("SEVERE") {
        Some(v.error_fg_color)
    } else if head.contains("WARN") {
        Some(v.warn_fg_color)
    } else if head.contains("INFO") {
        Some(if v.dark_mode {
            Color32::from_rgb(120, 190, 255)
        } else {
            Color32::from_rgb(20, 90, 190)
        })
    } else if head.contains("DEBUG") || head.contains("TRACE") {
        Some(v.weak_text_color())
    } else {
        None
    }
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

    /// 级别判定只取行首一小段，中文日志里第 200 个字节常常正落在汉字中间。
    /// 按字节直接切片会 panic——打开文件即闪退，正是这个原因。
    #[test]
    fn level_color_truncates_on_char_boundary() {
        let ctx = setup_ctx();
        // 前 198 字节为 ASCII，紧随其后的汉字占据 198..201，第 200 字节在其内部
        let line = format!("ERROR {}{}", "a".repeat(192), "查找");
        assert!(line.len() > 200);
        assert!(!line.is_char_boundary(200), "前提：构造的行应跨越字符边界");

        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert!(
                    level_color(ui, &line).is_some(),
                    "行首含 ERROR，应当照常着色而不是崩溃"
                );
            });
        });
    }
}
