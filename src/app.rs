//! The floating input bar: type Chinese, see English, Enter to commit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Frame, Key, Margin, RichText, Stroke};

use crate::config;
use crate::engine::{Job, TransResult};
use crate::inject;
use crate::pinyin::{self, Analysis, PinyinDict};
use crate::style;
use crate::userdict::UserDict;

// With the oneDNN engine at 30-100ms per sentence, a short debounce keeps the
// preview snappy without flooding the worker.
const DEBOUNCE: Duration = Duration::from_millis(120);

/// Candidates shown per page; `-`/`=` (or `,`/`.`, arrow keys) page around.
const PAGE_SIZE: usize = 5;

/// Cached per-run pinyin state: analysis plus the pre-rendered candidate
/// labels (chunked into pages), so cursor-blink repaints allocate nothing.
struct PinyinUi {
    run: String,
    analysis: Analysis,
    pages: Vec<Vec<String>>,
}

pub struct FastransApp {
    job_tx: Sender<Job>,
    res_rx: Receiver<TransResult>,
    toggle: Arc<AtomicBool>,
    // Keeps the OS hotkey registration alive for the app's lifetime.
    _hotkey_manager: global_hotkey::GlobalHotKeyManager,
    hint: String,
    pinyin: PinyinDict,
    pinyin_cache: Option<PinyinUi>,
    /// Per-user pick memory + follow suggestions, persisted.
    user: UserDict,
    /// Current candidate page.
    page: usize,
    /// Last committed word, the seed for follow-up suggestions (联想).
    last_word: Option<String>,
    /// Cached suggestion list for the current seed word.
    sugg_cache: Option<(String, Vec<String>)>,
    /// Built-in pinyin fallback on/off (Ctrl+P, persisted). Machines with a
    /// native IME don't need it — the native IME stays primary.
    pinyin_enabled: bool,
    /// Last known window position, persisted so drags are remembered.
    window_pos: Option<(f32, f32)>,
    autoupdate: bool,
    /// NA business-casual polish of the English output (config `style`).
    style_enabled: bool,
    ime_debug: bool,

    input: String,
    output: String,
    rev: u64,
    shown_rev: u64,
    /// Text of the most recent job, to skip resending identical text
    /// (e.g. Enter after the pinyin preview already translated it).
    last_sent: String,
    last_edit: Instant,
    dirty: bool,
    visible: bool,
    commit_pending: bool,
    /// One-shot: grab keyboard focus on the next frame after showing the bar.
    /// (request_focus() interrupts IME composition, so never call it per-frame.)
    focus_next: bool,
}

impl FastransApp {
    pub fn new(
        job_tx: Sender<Job>,
        res_rx: Receiver<TransResult>,
        toggle: Arc<AtomicBool>,
        hotkey_manager: global_hotkey::GlobalHotKeyManager,
        hotkey_spec: String,
        pinyin: PinyinDict,
        settings: config::Settings,
    ) -> Self {
        Self {
            job_tx,
            res_rx,
            toggle,
            _hotkey_manager: hotkey_manager,
            hint: format!(
                "输入中文,回车上屏英文 · {hotkey_spec} · ^Q退出 ^P拼音 · v{}",
                env!("CARGO_PKG_VERSION")
            ),
            pinyin,
            pinyin_cache: None,
            user: UserDict::load(),
            page: 0,
            last_word: None,
            sugg_cache: None,
            pinyin_enabled: settings.pinyin,
            window_pos: settings.pos,
            autoupdate: settings.autoupdate,
            style_enabled: settings.style,
            ime_debug: std::env::var_os("FASTRANS_IME_DEBUG").is_some(),
            input: String::new(),
            output: String::new(),
            rev: 0,
            shown_rev: 0,
            last_sent: String::new(),
            last_edit: Instant::now(),
            dirty: false,
            visible: false,
            commit_pending: false,
            focus_next: false,
        }
    }

    fn save_settings(&self) {
        config::save(config::Settings {
            pinyin: self.pinyin_enabled,
            autoupdate: self.autoupdate,
            style: self.style_enabled,
            pos: self.window_pos,
        });
    }

    /// Remembers the current window position (call before hiding/quitting).
    fn capture_pos(&mut self, ctx: &egui::Context) {
        if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            self.window_pos = Some((rect.min.x, rect.min.y));
        }
    }

    fn set_visible(&mut self, ctx: &egui::Context, visible: bool) {
        self.visible = visible;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(visible));
        if visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.focus_next = true;
        } else {
            self.capture_pos(ctx);
            self.save_settings();
            self.user.save_if_dirty();
            self.last_word = None;
            self.page = 0;
            self.input.clear();
            self.output.clear();
            self.dirty = false;
            self.commit_pending = false;
            self.last_sent.clear();
            // Invalidate any in-flight translation so it can't repopulate the
            // bar after it is reopened.
            self.rev += 1;
            self.shown_rev = self.rev;
        }
    }

    fn send_job(&mut self) {
        let text = self.effective_input();
        self.dirty = false;
        if text == self.last_sent {
            // Already translated (or in flight) — e.g. Enter right after the
            // pinyin preview submitted the same converted text.
            return;
        }
        self.rev += 1;
        self.last_sent.clone_from(&text);
        let _ = self.job_tx.send(Job {
            rev: self.rev,
            text,
        });
    }

    /// The trailing pinyin run of the input, with its start offset.
    /// Empty when the built-in pinyin fallback is switched off (Ctrl+P) —
    /// that single gate disables candidates, selection keys, and conversion.
    fn pinyin_run(&self) -> (usize, &str) {
        if !self.pinyin_enabled {
            return (self.input.len(), "");
        }
        let start = pinyin::run_start(&self.input);
        (start, &self.input[start..])
    }

    /// Text the engine should translate: any trailing pinyin is replaced by
    /// its best conversion, so the preview stays live while typing pinyin.
    fn effective_input(&mut self) -> String {
        let (start, run) = self.pinyin_run();
        if run.is_empty() {
            return self.input.clone();
        }
        let run = run.to_string();
        let best = self.analysis_for(&run).best_line.clone();
        format!("{}{}", &self.input[..start], best)
    }

    fn analysis_for(&mut self, run: &str) -> &Analysis {
        &self.ensure_pinyin_ui(run).analysis
    }

    fn ensure_pinyin_ui(&mut self, run: &str) -> &PinyinUi {
        if self.pinyin_cache.as_ref().map(|c| c.run.as_str()) != Some(run) {
            // Long labels are clipped so the single-line row can't starve the
            // translation area below it out of the fixed-height window.
            fn clip(s: &str, max: usize) -> String {
                if s.chars().count() > max {
                    let mut t: String = s.chars().take(max).collect();
                    t.push('…');
                    t
                } else {
                    s.to_string()
                }
            }
            let analysis = self.pinyin.analyze(run, &self.user);
            let pages = analysis
                .candidates
                .chunks(PAGE_SIZE)
                .enumerate()
                .map(|(pg, chunk)| {
                    chunk
                        .iter()
                        .enumerate()
                        .map(|(i, c)| {
                            let max = if pg == 0 && i == 0 { 18 } else { 8 };
                            format!("{} {}", i + 1, clip(&c.text, max))
                        })
                        .collect()
                })
                .collect();
            self.pinyin_cache = Some(PinyinUi {
                run: run.to_string(),
                analysis,
                pages,
            });
            self.page = 0;
        }
        self.pinyin_cache.as_ref().unwrap()
    }

    /// Cached follow-up suggestions for `prev` (rebuilt when `prev` changes).
    fn suggestions_for(&mut self, prev: &str) -> &[String] {
        if self.sugg_cache.as_ref().map(|(p, _)| p.as_str()) != Some(prev) {
            self.sugg_cache = Some((prev.to_string(), self.user.suggestions(prev)));
        }
        &self.sugg_cache.as_ref().unwrap().1
    }

    /// Applies pinyin candidate `pick` (absolute index) to the trailing run.
    /// The selection keys are intercepted before TextEdit ever sees them, so
    /// the chosen text renders in the same frame with no digit flicker.
    fn apply_selection(&mut self, pick: usize, ctx: &egui::Context) {
        let (start, run) = self.pinyin_run();
        if run.is_empty() {
            return;
        }
        let run = run.to_string();
        let cand = self.analysis_for(&run).candidates.get(pick).cloned();
        // No such candidate: swallow the key like a real IME would.
        let Some(cand) = cand else { return };
        // Remember the pick (unless it still contains raw letters): the same
        // pinyin ranks it first next time, and the previous word links to it
        // for follow-up suggestions.
        if !cand.text.bytes().any(|b| b.is_ascii_alphanumeric()) {
            self.user.record_pick(
                &run[..cand.consumed_bytes].to_ascii_lowercase(),
                &cand.text,
                self.last_word.as_deref(),
            );
            self.last_word = Some(cand.text.clone());
        }
        let rest = run[cand.consumed_bytes..].trim_start_matches('\'');
        self.input = format!("{}{}{}", &self.input[..start], cand.text, rest);
        // The pick changed the rankings: rebuild on next use.
        self.pinyin_cache = None;
        self.page = 0;
        self.caret_to_end(ctx);
        self.dirty = true;
        self.last_edit = Instant::now();
    }

    /// Appends a follow-up suggestion (联想, mouse-picked) to the input.
    fn apply_suggestion(&mut self, word: &str, ctx: &egui::Context) {
        let Some(prev) = self.last_word.clone() else {
            return;
        };
        self.user.record_follow(&prev, word);
        self.input.push_str(word);
        self.last_word = Some(word.to_string());
        self.caret_to_end(ctx);
        self.dirty = true;
        self.last_edit = Instant::now();
    }

    /// Moves the TextEdit caret to the end after the buffer was rewritten
    /// behind the widget's back (candidate/suggestion insertion).
    fn caret_to_end(&self, ctx: &egui::Context) {
        let id = egui::Id::new("bar_input");
        if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
            let end = egui::text::CCursor::new(self.input.chars().count());
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(end)));
            state.store(ctx, id);
        }
    }

    /// Converts any trailing pinyin in place (used on Enter).
    fn finalize_pinyin(&mut self) -> bool {
        let (start, run) = self.pinyin_run();
        if run.is_empty() {
            return false;
        }
        let run = run.to_string();
        let best = self.analysis_for(&run).best_line.clone();
        self.input = format!("{}{}", &self.input[..start], best);
        true
    }

    fn commit(&mut self, ctx: &egui::Context) {
        let text = self.output.trim().to_string();
        // Our window is foreground right now; once it hides, the foreground
        // handle changes — poll for that instead of sleeping a fixed 150ms.
        let own_window = inject::foreground_window();
        self.set_visible(ctx, false);
        if text.is_empty() {
            return;
        }
        thread::spawn(move || {
            inject::wait_focus_leave(own_window, Duration::from_millis(250));
            if let Err(e) = inject::paste_text(&text) {
                eprintln!("paste failed: {e:#}");
            }
        });
    }
}

impl eframe::App for FastransApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Same as the bar fill: window transparency is unreliable on Windows
        // (glow backend), so the bar is opaque and fills the whole window.
        // Win11's DWM rounds the borderless window corners for us.
        Color32::from_rgb(24, 26, 30).to_normalized_gamma_f32()
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.ime_debug {
            ctx.input(|i| {
                for e in &i.events {
                    if matches!(e, egui::Event::Ime(_)) {
                        eprintln!("ime event: {e:?}");
                    }
                }
            });
        }
        // Global hotkey toggles the bar.
        if self.toggle.swap(false, Ordering::SeqCst) {
            let show = !self.visible;
            self.set_visible(ctx, show);
        }

        // Translation results (only accept the newest revision).
        while let Ok(res) = self.res_rx.try_recv() {
            if let Some(err) = res.error {
                // Engine failure: show it, never commit it.
                self.output = format!("⚠ 翻译引擎错误: {err}");
                self.commit_pending = false;
                continue;
            }
            if res.rev >= self.shown_rev {
                self.shown_rev = res.rev;
                self.output = if self.style_enabled {
                    style::polish(&res.text)
                } else {
                    res.text
                };
                // Restart the fade-in for the fresh translation.
                ctx.animate_value_with_time(egui::Id::new("out_fade"), 0.0, 0.0);
            }
        }
        // A commit was requested while a translation was still in flight.
        if self.commit_pending && !self.dirty && self.shown_rev == self.rev {
            self.commit_pending = false;
            self.commit(ctx);
            return;
        }

        // Debounced translate-as-you-type. (Single elapsed() read: a second
        // read could cross the deadline and make the subtraction panic.)
        if self.dirty {
            let remaining = DEBOUNCE.saturating_sub(self.last_edit.elapsed());
            if remaining.is_zero() {
                self.send_job();
            } else {
                ctx.request_repaint_after(remaining);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let bar = Frame::new()
            .fill(Color32::from_rgb(24, 26, 30))
            .stroke(Stroke::new(1.0, Color32::from_rgb(58, 62, 70)))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(16, 14));

        bar.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(ui.available_height());

            // Registered before the widgets, so it sits underneath them:
            // dragging empty bar space moves the window (native OS drag),
            // while text selection inside the TextEdit keeps working.
            // The OS drag starts on the press itself — waiting for egui's
            // drag threshold adds a dead zone that makes the pickup feel
            // sticky. A grab cursor advertises the affordance.
            let drag_zone = ui.interact(
                ui.max_rect(),
                egui::Id::new("bar_drag"),
                egui::Sense::drag(),
            );
            if drag_zone.is_pointer_button_down_on() {
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                if ctx.input(|i| i.pointer.primary_pressed()) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
            } else if drag_zone.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::Grab);
            }

            // Intercept IME keys BEFORE the TextEdit consumes them, so the
            // result renders in this same frame with no key flashing in the
            // buffer. Interception happens ONLY while real pinyin is being
            // composed (the trailing run yields candidates) — plain English
            // like "GPT-4" or "Wi-Fi" must keep its digits and punctuation.
            // Suggestions (联想) are mouse-only and never steal keys.
            // At most one selection/paging event is honoured per frame (a
            // batched later keystroke would otherwise act on stale state).
            let composing = {
                let run = self.pinyin_run().1;
                if run.is_empty() {
                    false
                } else {
                    let run = run.to_string();
                    !self.ensure_pinyin_ui(&run).analysis.candidates.is_empty()
                }
            };
            if composing {
                #[derive(Clone, Copy)]
                enum Act {
                    Pick(usize),
                    Swallow,
                    Page(i32),
                }
                let mut act: Option<Act> = None;
                ui.input_mut(|i| {
                    i.events.retain(|e| {
                        if act.is_some() {
                            return true;
                        }
                        match e {
                            egui::Event::Text(t) => {
                                let b = t.as_bytes();
                                if b.len() != 1 {
                                    return true;
                                }
                                match b[0] {
                                    b' ' => {
                                        act = Some(Act::Pick(0));
                                        false
                                    }
                                    d @ b'1'..=b'9' => {
                                        let d = (d - b'1') as usize;
                                        // Digits past the visible page pick
                                        // nothing but are swallowed like a
                                        // real IME would.
                                        act = Some(if d < PAGE_SIZE {
                                            Act::Pick(d)
                                        } else {
                                            Act::Swallow
                                        });
                                        false
                                    }
                                    b'-' | b',' => {
                                        act = Some(Act::Page(-1));
                                        false
                                    }
                                    b'=' | b'.' => {
                                        act = Some(Act::Page(1));
                                        false
                                    }
                                    _ => true,
                                }
                            }
                            egui::Event::Key {
                                key,
                                pressed: true,
                                modifiers,
                                ..
                            } if modifiers.is_none() => match key {
                                Key::ArrowUp | Key::PageUp => {
                                    act = Some(Act::Page(-1));
                                    false
                                }
                                Key::ArrowDown | Key::PageDown => {
                                    act = Some(Act::Page(1));
                                    false
                                }
                                _ => true,
                            },
                            _ => true,
                        }
                    });
                });
                let n_pages = self
                    .pinyin_cache
                    .as_ref()
                    .map(|c| c.pages.len().max(1))
                    .unwrap_or(1);
                self.page = self.page.min(n_pages - 1);
                match act {
                    Some(Act::Pick(d)) => {
                        let pick = self.page * PAGE_SIZE + d;
                        self.apply_selection(pick, &ctx);
                    }
                    Some(Act::Page(delta)) => {
                        self.page = (self.page as i64 + delta as i64)
                            .clamp(0, n_pages as i64 - 1)
                            as usize;
                    }
                    Some(Act::Swallow) | None => {}
                }
            }

            let edit = egui::TextEdit::singleline(&mut self.input)
                .id(egui::Id::new("bar_input"))
                .hint_text(self.hint.as_str())
                .font(egui::FontId::proportional(19.0))
                .desired_width(f32::INFINITY)
                .frame(Frame::new());
            let response = ui.add(edit);
            // One-shot focus on show: request_focus() interrupts IME
            // composition, so it must never run while the user is typing.
            if std::mem::take(&mut self.focus_next) {
                response.request_focus();
            }
            // Note: the 联想 seed is NOT cleared on edits — the suggestion
            // row's own gate (input must still end with the seed word) hides
            // it naturally and lets it return after a backspace.
            if response.changed() {
                self.dirty = true;
                self.last_edit = Instant::now();
                ctx.request_repaint_after(DEBOUNCE);
            }

            // Candidate row (paged, single line, clickable), or the mouse-only
            // follow-up suggestion row (联想) when nothing is being composed.
            // Labels are cached per run so cursor-blink repaints stay cheap.
            // Re-evaluated here (not via `composing`): a selection above may
            // have rewritten the buffer and invalidated the cache this frame.
            let run_after = self.pinyin_run().1;
            if !run_after.is_empty() {
                let run = run_after.to_string();
                self.ensure_pinyin_ui(&run);
                let cache = self.pinyin_cache.as_ref().unwrap();
                let n_pages = cache.pages.len().max(1);
                let page = self.page.min(n_pages - 1);
                let mut clicked: Option<usize> = None;
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if n_pages > 1 {
                        ui.label(
                            RichText::new(format!("{}/{}", page + 1, n_pages))
                                .size(12.0)
                                .color(Color32::from_gray(120)),
                        );
                    }
                    if let Some(labels) = cache.pages.get(page) {
                        for (i, text) in labels.iter().enumerate() {
                            let r = ui.add(
                                egui::Label::new(
                                    RichText::new(text)
                                        .size(14.0)
                                        .color(Color32::from_rgb(200, 180, 120)),
                                )
                                .sense(egui::Sense::click()),
                            );
                            if r.clicked() {
                                clicked = Some(page * PAGE_SIZE + i);
                            }
                        }
                    }
                });
                if let Some(pick) = clicked {
                    self.apply_selection(pick, &ctx);
                }
            } else if self.pinyin_enabled && self.pinyin_run().1.is_empty() {
                // Suggestion row: only while the input still ends with the
                // seed word (so it disappears on other edits and comes back
                // after a backspace), and never touches the keyboard.
                let seed = self
                    .last_word
                    .clone()
                    .filter(|w| self.input.ends_with(w.as_str()));
                if let Some(seed) = seed {
                    let words: Vec<String> = self.suggestions_for(&seed).to_vec();
                    if !words.is_empty() {
                        let mut clicked: Option<String> = None;
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("联想")
                                    .size(12.0)
                                    .color(Color32::from_gray(120)),
                            );
                            for w in &words {
                                let r = ui.add(
                                    egui::Label::new(
                                        RichText::new(w)
                                            .size(14.0)
                                            .color(Color32::from_rgb(150, 190, 160)),
                                    )
                                    .sense(egui::Sense::click()),
                                );
                                if r.clicked() {
                                    clicked = Some(w.clone());
                                }
                            }
                        });
                        if let Some(word) = clicked {
                            self.apply_suggestion(&word, &ctx);
                        }
                    }
                }
            }

            ui.add_space(8.0);
            let pending = self.dirty || self.shown_rev < self.rev;
            if self.commit_pending {
                // Enter was pressed while translating: spinner until it lands.
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(
                        RichText::new("正在上屏…")
                            .size(14.0)
                            .color(Color32::from_gray(150)),
                    );
                });
            } else if self.output.is_empty() && !self.input.is_empty() {
                // First translation on its way: spinner + walking dots.
                let dots = 1 + (ui.input(|i| i.time) * 3.0) as usize % 3;
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(13.0));
                    ui.label(
                        RichText::new("· ".repeat(dots))
                            .size(16.0)
                            .color(Color32::from_gray(120)),
                    );
                });
            } else {
                // Fresh translations fade in; while a newer one is being
                // computed the current text dims slightly. Long output wraps
                // and scrolls (mouse wheel) inside the fixed-height bar.
                let fade = ctx.animate_value_with_time(egui::Id::new("out_fade"), 1.0, 0.18);
                let color = if pending {
                    Color32::from_rgb(105, 145, 185)
                } else {
                    Color32::from_rgb(140, 200, 255)
                }
                .gamma_multiply(0.35 + 0.65 * fade);
                egui::ScrollArea::vertical()
                    .max_height(ui.available_height().max(22.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.label(RichText::new(&self.output).size(16.0).color(color));
                    });
            }

            let (enter, esc, quit, toggle_pinyin) = ui.input(|i| {
                (
                    i.key_pressed(Key::Enter),
                    i.key_pressed(Key::Escape),
                    i.modifiers.ctrl && i.key_pressed(Key::Q),
                    i.modifiers.ctrl && i.key_pressed(Key::P),
                )
            });
            if quit {
                // Ctrl+Q: quit for real. ViewportCommand::Close hangs in some
                // component's drop (observed: window gone, process stuck), and
                // everything is flushed right here.
                self.capture_pos(&ctx);
                self.save_settings();
                self.user.save_if_dirty();
                std::process::exit(0);
            }
            if toggle_pinyin {
                self.pinyin_enabled = !self.pinyin_enabled;
                self.save_settings();
                self.pinyin_cache = None;
                self.page = 0;
                self.last_word = None;
                self.sugg_cache = None;
                self.output = if self.pinyin_enabled {
                    "内置拼音:开(无系统输入法时的兜底)".into()
                } else {
                    "内置拼音:关(只用系统输入法)".into()
                };
                self.dirty = true;
                self.last_edit = Instant::now();
                return;
            }
            if esc {
                self.set_visible(&ctx, false);
            } else if enter && !self.input.trim().is_empty() {
                if self.finalize_pinyin() {
                    self.dirty = true;
                }
                if self.dirty || self.shown_rev < self.rev {
                    // Latest text not translated yet: push it now, commit on arrival.
                    if self.dirty {
                        self.send_job();
                    }
                    self.commit_pending = true;
                } else {
                    self.commit(&ctx);
                }
            } else if enter {
                self.set_visible(&ctx, false);
            }
        });
    }
}
