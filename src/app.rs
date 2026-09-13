use crate::logstore::{encoding_name, LogStore};
use eframe::egui;
use egui::{Color32, FontId, RichText, ScrollArea, TextEdit, TextStyle};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 单行最大渲染字符数，超出截断，避免超长行拖慢渲染
const MAX_RENDER_CHARS: usize = 4000;

/// 搜索框的固定 Id，用于快捷键聚焦与失焦
const SEARCH_ID: &str = "logview_search";

pub struct LogViewApp {
    store: Option<LogStore>,
    query: String,
    case_sensitive: bool,
    only_matched: bool,
    follow: bool,
    wrap: bool,
    dark: Option<bool>,
    error: Option<String>,

    /// 搜索防抖
    search_dirty: Option<Instant>,
    /// 当前跳转目标行
    pending_jump: Option<usize>,
    /// 搜索完成后是否自动定位到第一个命中
    pending_first_match: bool,
    /// 匹配项游标（用于上一个/下一个）
    match_cursor: usize,
    /// 纵向滚动偏移，手动维护以便跳转
    scroll_offset: f32,

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
            only_matched: false,
            follow: true,
            wrap: false,
            dark: None,
            error: None,
            search_dirty: None,
            pending_jump: None,
            pending_first_match: false,
            match_cursor: 0,
            scroll_offset: 0.0,
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
                    s.start_search(&self.query, self.case_sensitive);
                }
                self.scroll_offset = 0.0;
                self.match_cursor = 0;
                self.store = Some(s);
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e}")),
        }
    }

    fn run_search(&mut self) {
        if let Some(s) = &mut self.store {
            if s.start_search(&self.query, self.case_sensitive) {
                self.pending_first_match = true;
            }
        }
        self.match_cursor = 0;
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

        // 搜索结果到位后自动定位到第一个命中
        if self.pending_first_match && !self.store.as_ref().map(|s| s.searching).unwrap_or(false) {
            self.pending_first_match = false;
            self.goto_match(0);
        }

        // 搜索防抖
        if let Some(t) = self.search_dirty {
            if t.elapsed() > Duration::from_millis(250) {
                self.search_dirty = None;
                self.run_search();
            }
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.toolbar(ctx, ui));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.status_bar(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.log_area(ui));

        // 快捷键放在各面板之后：此时 search_has_focus 已是本帧的最新状态
        self.handle_shortcuts(ctx);

        // 保持 tail 跟随与索引进度的刷新
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
                    .hint_text("搜索（⌘F 或 / 聚焦，n / N 跳转）")
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
                self.run_search();
            }

            ui.checkbox(&mut self.case_sensitive, "区分大小写")
                .on_hover_text("关闭时用 ASCII 小写匹配，速度略慢");
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

            let _ = ui.button("?").on_hover_text(
                "快捷键\n\
                 ⌘F 或 /    聚焦搜索框\n\
                 n / N      下 / 上一个命中\n\
                 g / G      跳到开头 / 末尾\n\
                 Esc        清空检索\n\
                 ⌘O         打开文件",
            );
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
            if !self.query.is_empty() {
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
        let gutter_w = (format!("{total}").len() as f32) * 8.0 + 12.0;

        // 跳转：换算成滚动偏移
        // pending_jump 存的已经是「列表中的行位置」：
        // only_matched 模式下是匹配序号，否则是文件行号，两者都直接乘行高。
        if let Some(target) = self.pending_jump.take() {
            self.scroll_offset = (target as f32 * row_h - 120.0).max(0.0);
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

        let buf = &mut self.buf;
        let query = self.query.clone();
        let cs = self.case_sensitive;
        let only = self.only_matched;
        let wrap = self.wrap;
        let store_ref = &self.store;

        let out = area.show_rows(ui, row_h, total, |ui, rows| {
            let Some(store) = store_ref else { return };
            for r in rows {
                let line_idx = if only {
                    store.matches().get(r).copied().map(|v| v as usize)
                } else {
                    Some(r)
                };
                let Some(line_idx) = line_idx else { continue };
                buf.clear();
                if !store.read_line_into(line_idx, buf) {
                    continue;
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
                        render_content(ui, buf, &query, cs, wrap);
                    });
                });
            }
        });

        if !stick {
            self.scroll_offset = out.state.offset.y;
        }
    }
}

fn render_content(ui: &mut egui::Ui, text: &str, query: &str, case_sensitive: bool, wrap: bool) {
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
        )
    };
    job.wrap.max_width = if wrap {
        ui.available_width()
    } else {
        f32::INFINITY
    };

    let mut label = egui::Label::new(job).selectable(true);
    if !wrap {
        label = label.truncate();
    }
    ui.add(label);
}

fn highlighted_job(
    text: &str,
    query: &str,
    case_sensitive: bool,
    base_color: Color32,
    font_id: &FontId,
    dark: bool,
) -> egui::text::LayoutJob {
    // to_ascii_lowercase 不改变字节长度，可安全用于偏移换算
    let (hay, needle) = if case_sensitive {
        (text.to_string(), query.to_string())
    } else {
        (text.to_ascii_lowercase(), query.to_ascii_lowercase())
    };

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
    let head = &line[..line.len().min(200)];
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

#[cfg(test)]
mod shortcut_tests {
    use super::*;

    /// 造一个带两个命中行的 app。
    ///
    /// `tag` 用于区分文件名：测试是并行执行的，若共用同一个临时文件，
    /// 会互相覆盖内容导致随机失败。
    fn app_with_hits(tag: &str) -> LogViewApp {
        let mut p = std::env::temp_dir();
        p.push(format!("logview_shortcut_{tag}.log"));
        std::fs::write(&p, b"ERROR one\nplain\nERROR two\n").unwrap();

        let mut store = LogStore::open(p).unwrap();
        let wait = |store: &mut LogStore| {
            for _ in 0..2000 {
                store.pump();
                if !store.indexing && !store.searching {
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            panic!("后台任务超时");
        };
        wait(&mut store);
        store.start_search("ERROR", true);
        wait(&mut store);
        assert_eq!(store.matches().len(), 2, "前提：应有 2 个命中行");

        let mut app = LogViewApp::new();
        app.query = "ERROR".to_string();
        app.store = Some(store);
        app
    }

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
