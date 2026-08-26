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
    fastrans::update::cleanup_old();
    if settings.autoupdate {
        // Silent background check; a newer exe is swapped in for next launch.
        fastrans::update::spawn_check();
    }
    // Window/taskbar icon: raw 64x64 RGBA baked in at build time.
    let icon = egui::IconData {
        rgba: include_bytes!("../assets/icon-64.rgba").to_vec(),
        width: 64,
        height: 64,
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([560.0, 118.0])
        .with_decorations(false)
        .with_always_on_top()
        .with_resizable(false)
        .with_icon(icon)
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

            // Preferred hotkey: settings page choice, then FASTRANS_HOTKEY,
            // then the fallback chain (Ctrl+Alt+Space may be taken).
            let manager = GlobalHotKeyManager::new().expect("hotkey manager");
            let prefer = settings
                .hotkey
                .clone()
                .or_else(|| std::env::var("FASTRANS_HOTKEY").ok());
            let (active_hotkey, hotkey_spec) = hotkey::register(&manager, prefer.as_deref())
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

            // System tray: toggle bar / settings / quit. Menu and icon-click
            // events arrive on crossbeam channels; forwarding threads set
            // flags and wake the UI, same pattern as the global hotkey.
            let open_settings = Arc::new(AtomicBool::new(false));
            let quit = Arc::new(AtomicBool::new(false));
            #[cfg(windows)]
            let _tray = {
                use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
                use tray_icon::{TrayIconBuilder, TrayIconEvent};
                let mi_toggle = MenuItem::new("显示 / 隐藏翻译条", true, None);
                let mi_settings = MenuItem::new("设置…", true, None);
                let mi_quit = MenuItem::new("退出", true, None);
                let menu = Menu::new();
                let _ = menu.append_items(&[
                    &mi_toggle,
                    &mi_settings,
                    &PredefinedMenuItem::separator(),
                    &mi_quit,
                ]);
                let icon_rgba = include_bytes!("../assets/icon-64.rgba").to_vec();
                let tray_icon = tray_icon::Icon::from_rgba(icon_rgba, 64, 64).ok();
                let mut builder = TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_tooltip(concat!("fastrans v", env!("CARGO_PKG_VERSION")));
                if let Some(i) = tray_icon {
                    builder = builder.with_icon(i);
                }
                let tray = builder.build().ok();

                let (id_toggle, id_settings, id_quit) =
                    (mi_toggle.id().clone(), mi_settings.id().clone(), mi_quit.id().clone());
                let (t2, s2, q2) = (toggle.clone(), open_settings.clone(), quit.clone());
                let ctx = cc.egui_ctx.clone();
                thread::spawn(move || {
                    let rx = MenuEvent::receiver();
                    while let Ok(ev) = rx.recv() {
                        if ev.id == id_toggle {
                            t2.store(true, Ordering::SeqCst);
                        } else if ev.id == id_settings {
                            s2.store(true, Ordering::SeqCst);
                        } else if ev.id == id_quit {
                            q2.store(true, Ordering::SeqCst);
                        }
                        ctx.request_repaint();
                    }
                });
                let t3 = toggle.clone();
                let ctx = cc.egui_ctx.clone();
                thread::spawn(move || {
                    let rx = TrayIconEvent::receiver();
                    while let Ok(ev) = rx.recv() {
                        if let TrayIconEvent::DoubleClick { .. } = ev {
                            t3.store(true, Ordering::SeqCst);
                            ctx.request_repaint();
                        }
                    }
                });
                tray
            };

            let ctx = cc.egui_ctx.clone();
            let (job_tx, res_rx) = engine::spawn_worker(model_dir, move || ctx.request_repaint());
            let pinyin = dict_thread.join().expect("pinyin dict load");

            Ok(Box::new(FastransApp::new(
                job_tx,
                res_rx,
                toggle,
                open_settings,
                quit,
                manager,
                active_hotkey,
                hotkey_spec,
                pinyin,
                settings,
                #[cfg(windows)]
                _tray,
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
