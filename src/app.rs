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

// With the oneDNN engine at 30-100ms per sentence, a short debounce keeps the
// preview snappy without flooding the worker.
const DEBOUNCE: Duration = Duration::from_millis(120);

/// Cached per-run pinyin state: analysis plus the pre-rendered candidate
/// labels, so cursor-blink repaints allocate nothing.
struct PinyinUi {
    run: String,
    analysis: Analysis,
    labels: Vec<String>,
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
    /// Built-in pinyin fallback on/off (Ctrl+P, persisted). Machines with a
    /// native IME don't need it — the native IME stays primary.
    pinyin_enabled: bool,
    /// Last known window position, persisted so drags are remembered.
    window_pos: Option<(f32, f32)>,
    autoupdate: bool,
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
            pinyin_enabled: settings.pinyin,
            window_pos: settings.pos,
            autoupdate: settings.autoupdate,
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
            let analysis = self.pinyin.analyze(run);
            let labels = analysis
                .candidates
                .iter()
                .take(6)
                .enumerate()
                .map(|(i, c)| format!("{} {}", i + 1, c.text))
                .collect();
            self.pinyin_cache = Some(PinyinUi {
                run: run.to_string(),
                analysis,
                labels,
            });
        }
        self.pinyin_cache.as_ref().unwrap()
    }

    /// Applies pinyin candidate `pick` to the trailing run. The selection keys
    /// (digits/space) are intercepted before TextEdit ever sees them, so the
    /// chosen text renders in the same frame with no digit flicker.
    fn apply_selection(&mut self, pick: usize) {
        let (start, run) = self.pinyin_run();
        if run.is_empty() {
            return;
        }
        let run = run.to_string();
        let cand = self.analysis_for(&run).candidates.get(pick).cloned();
        // No such candidate: swallow the key like a real IME would.
        let Some(cand) = cand else { return };
        let rest = run[cand.consumed_bytes..].trim_start_matches('\'');
        self.input = format!("{}{}{}", &self.input[..start], cand.text, rest);
        self.dirty = true;
        self.last_edit = Instant::now();
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
                self.output = res.text;
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

            // Intercept pinyin selection keys (digits 1-9, space) BEFORE the
            // TextEdit consumes them: the chosen candidate then renders this
            // same frame, with no digit flashing in the buffer first.
            if !self.pinyin_run().1.is_empty() {
                let mut picked: Option<usize> = None;
                ui.input_mut(|i| {
                    i.events.retain(|e| {
                        if picked.is_some() {
                            return true;
                        }
                        if let egui::Event::Text(t) = e {
                            let b = t.as_bytes();
                            if b == b" " {
                                picked = Some(0);
                                return false;
                            }
                            if b.len() == 1 && (b'1'..=b'9').contains(&b[0]) {
                                picked = Some((b[0] - b'1') as usize);
                                return false;
                            }
                        }
                        true
                    });
                });
                if let Some(pick) = picked {
                    self.apply_selection(pick);
                }
            }

            let edit = egui::TextEdit::singleline(&mut self.input)
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
            if response.changed() {
                self.dirty = true;
                self.last_edit = Instant::now();
                ctx.request_repaint_after(DEBOUNCE);
            }

            // Built-in pinyin candidates (for machines without a Chinese IME):
            // digits pick a candidate, space picks the first. The label cache
            // makes cursor-blink repaints allocation-free.
            if !self.pinyin_run().1.is_empty() {
                let stale = self.pinyin_cache.as_ref().map(|c| c.run.as_str())
                    != Some(self.pinyin_run().1);
                if stale {
                    let run = self.pinyin_run().1.to_string();
                    self.ensure_pinyin_ui(&run);
                }
                let labels = &self.pinyin_cache.as_ref().unwrap().labels;
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    for text in labels {
                        ui.label(
                            RichText::new(text)
                                .size(14.0)
                                .color(Color32::from_rgb(200, 180, 120)),
                        );
                    }
                });
            }

            ui.add_space(8.0);
            let shown = if self.output.is_empty() && !self.input.is_empty() {
                RichText::new("…").size(16.0).color(Color32::from_gray(110))
            } else {
                RichText::new(&self.output)
                    .size(16.0)
                    .color(Color32::from_rgb(140, 200, 255))
            };
            ui.label(shown);

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
                // config is flushed right here.
                self.capture_pos(&ctx);
                self.save_settings();
                std::process::exit(0);
            }
            if toggle_pinyin {
                self.pinyin_enabled = !self.pinyin_enabled;
                self.save_settings();
                self.pinyin_cache = None;
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
