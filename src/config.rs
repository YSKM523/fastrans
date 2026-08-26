//! Tiny persisted settings: %APPDATA%\fastrans\config.txt, one `key=value`
//! per line. No config crate — two fields don't warrant a dependency.

use std::path::PathBuf;

#[derive(Clone)]
pub struct Settings {
    /// Built-in pinyin fallback on/off (Ctrl+P).
    pub pinyin: bool,
    /// Silent self-update check at launch.
    pub autoupdate: bool,
    /// North-American business-casual polish of the English output.
    pub style: bool,
    /// User-chosen hotkey spec (settings page); falls back to env/defaults.
    pub hotkey: Option<String>,
    /// Last bar position in logical points (drag to move, remembered).
    pub pos: Option<(f32, f32)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            pinyin: true,
            autoupdate: true,
            style: true,
            hotkey: None,
            pos: None,
        }
    }
}

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("fastrans").join("config.txt"))
}

pub fn load() -> Settings {
    let mut s = Settings::default();
    let Some(p) = path() else { return s };
    let Ok(text) = std::fs::read_to_string(p) else {
        return s;
    };
    for line in text.lines() {
        match line.trim().split_once('=') {
            Some(("pinyin", v)) => s.pinyin = v.trim() != "0",
            Some(("autoupdate", v)) => s.autoupdate = v.trim() != "0",
            Some(("style", v)) => s.style = v.trim() != "0",
            Some(("hotkey", v)) => {
                let v = v.trim();
                if !v.is_empty() {
                    s.hotkey = Some(v.to_string());
                }
            }
            Some(("pos", v)) => {
                if let Some((x, y)) = v.split_once(',') {
                    if let (Ok(x), Ok(y)) = (x.trim().parse(), y.trim().parse()) {
                        s.pos = Some((x, y));
                    }
                }
            }
            _ => {}
        }
    }
    s
}

pub fn save(s: &Settings) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut out = format!(
        "pinyin={}\nautoupdate={}\nstyle={}\n",
        if s.pinyin { 1 } else { 0 },
        if s.autoupdate { 1 } else { 0 },
        if s.style { 1 } else { 0 }
    );
    if let Some(hk) = &s.hotkey {
        out.push_str(&format!("hotkey={hk}\n"));
    }
    if let Some((x, y)) = s.pos {
        out.push_str(&format!("pos={x:.0},{y:.0}\n"));
    }
    let _ = std::fs::write(p, out);
}
