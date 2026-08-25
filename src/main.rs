#![cfg_attr(windows, windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};

use fastrans::app::FastransApp;
use fastrans::engine;
use fastrans::hotkey;

fn main() -> eframe::Result<()> {
    let model_dir = engine::find_model_dir().unwrap_or_else(|| {
        eprintln!(
            "model not found: put the converted model in ./models/opus-mt-zh-en \
             (next to the executable) or set FASTRANS_MODEL"
        );
        std::process::exit(1);
    });
    // The engine worker loads the model itself, so the window and hotkey come
    // up immediately; the pinyin dictionary loads in parallel (~20ms).
    let dict_thread = thread::spawn(fastrans::pinyin::PinyinDict::load);

    let settings = fastrans::config::load();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([560.0, 118.0])
        .with_decorations(false)
        .with_always_on_top()
        .with_resizable(false)
        .with_visible(false);
    // Reopen where the user last dragged the bar.
    if let Some((x, y)) = settings.pos {
        viewport = viewport.with_position([x, y]);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "fastrans",
        options,
        Box::new(move |cc| {
            install_cjk_font(&cc.egui_ctx);

            // Preferred hotkey from FASTRANS_HOTKEY, else the fallback chain
            // (the default Ctrl+Alt+Space may be taken by another app).
            let manager = GlobalHotKeyManager::new().expect("hotkey manager");
            let prefer = std::env::var("FASTRANS_HOTKEY").ok();
            let (_hotkey, hotkey_spec) = hotkey::register(&manager, prefer.as_deref())
                .unwrap_or_else(|e| {
                    eprintln!("{e:#}");
                    std::process::exit(1);
                });
            eprintln!("hotkey: {hotkey_spec}");

            let toggle = Arc::new(AtomicBool::new(false));
            let toggle2 = toggle.clone();
            let ctx = cc.egui_ctx.clone();
            thread::spawn(move || {
                let rx = GlobalHotKeyEvent::receiver();
                while let Ok(ev) = rx.recv() {
                    if ev.state == global_hotkey::HotKeyState::Pressed {
                        toggle2.store(true, Ordering::SeqCst);
                        ctx.request_repaint();
                    }
                }
            });

            let ctx = cc.egui_ctx.clone();
            let (job_tx, res_rx) = engine::spawn_worker(model_dir, move || ctx.request_repaint());
            let pinyin = dict_thread.join().expect("pinyin dict load");

            Ok(Box::new(FastransApp::new(
                job_tx,
                res_rx,
                toggle,
                manager,
                hotkey_spec,
                pinyin,
                settings,
            )))
        }),
    )
}

/// egui's bundled fonts have no CJK glyphs; load one from the OS.
fn install_cjk_font(ctx: &egui::Context) {
    let candidates: &[&str] = &[
        #[cfg(windows)]
        "C:\\Windows\\Fonts\\msyh.ttc",
        #[cfg(windows)]
        "C:\\Windows\\Fonts\\simhei.ttf",
        #[cfg(target_os = "macos")]
        "/System/Library/Fonts/PingFang.ttc",
        #[cfg(target_os = "linux")]
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        #[cfg(target_os = "linux")]
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "cjk".into(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("cjk".into());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
    eprintln!("warning: no CJK font found, Chinese text will not render");
}
