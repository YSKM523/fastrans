//! Hotkey spec parsing ("ctrl+alt+space") and registration with fallbacks.
//!
//! The preferred hotkey may already be taken by another app (real-machine
//! finding: Ctrl+Alt+Space was grabbed on the first Windows test box), so
//! registration walks a candidate list instead of panicking.

use anyhow::{anyhow, Result};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::GlobalHotKeyManager;

/// Tried in order when FASTRANS_HOTKEY is unset or cannot be registered.
pub const FALLBACKS: &[&str] = &["ctrl+alt+space", "ctrl+shift+space", "ctrl+alt+e"];

/// Registers the first available hotkey: `prefer` (from FASTRANS_HOTKEY) first,
/// then the fallback list. Returns the hotkey and its human-readable spec.
pub fn register(manager: &GlobalHotKeyManager, prefer: Option<&str>) -> Result<(HotKey, String)> {
    let mut failures = Vec::new();
    let specs = prefer
        .into_iter()
        .chain(FALLBACKS.iter().copied())
        .map(str::to_ascii_lowercase);
    for spec in specs {
        match parse(&spec) {
            Ok(hk) => match manager.register(hk) {
                Ok(()) => return Ok((hk, spec)),
                Err(e) => failures.push(format!("{spec}: {e}")),
            },
            Err(e) => failures.push(format!("{spec}: {e}")),
        }
    }
    Err(anyhow!(
        "no hotkey could be registered:\n  {}",
        failures.join("\n  ")
    ))
}

/// Parses "ctrl+alt+space" style specs. Modifiers: ctrl/alt/shift/win.
/// Keys: a-z, 0-9, f1-f12, space/enter/tab/backquote.
pub fn parse(spec: &str) -> Result<HotKey> {
    let mut mods = Modifiers::empty();
    let mut key = None;
    for part in spec.split('+') {
        match part.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "alt" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            "win" | "super" | "meta" | "cmd" => mods |= Modifiers::META,
            other => {
                if key.replace(key_code(other)?).is_some() {
                    return Err(anyhow!("more than one key in {spec:?}"));
                }
            }
        }
    }
    let key = key.ok_or_else(|| anyhow!("no key in {spec:?}"))?;
    let mods = (!mods.is_empty()).then_some(mods);
    Ok(HotKey::new(mods, key))
}

fn key_code(name: &str) -> Result<Code> {
    use Code::*;
    if let Some(c) = single_char_code(name) {
        return Ok(c);
    }
    Ok(match name {
        "space" => Space,
        "enter" | "return" => Enter,
        "tab" => Tab,
        "backquote" | "grave" => Backquote,
        "f1" => F1,
        "f2" => F2,
        "f3" => F3,
        "f4" => F4,
        "f5" => F5,
        "f6" => F6,
        "f7" => F7,
        "f8" => F8,
        "f9" => F9,
        "f10" => F10,
        "f11" => F11,
        "f12" => F12,
        _ => return Err(anyhow!("unknown key {name:?}")),
    })
}

fn single_char_code(name: &str) -> Option<Code> {
    use Code::*;
    let mut chars = name.chars();
    let (c, None) = (chars.next()?, chars.next()) else {
        return None;
    };
    const LETTERS: [Code; 26] = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO,
        KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    const DIGITS: [Code; 10] = [
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    ];
    match c {
        'a'..='z' => Some(LETTERS[(c as u8 - b'a') as usize]),
        '0'..='9' => Some(DIGITS[(c as u8 - b'0') as usize]),
        '`' => Some(Backquote),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_specs() {
        for spec in FALLBACKS {
            parse(spec).unwrap();
        }
        parse("ctrl+shift+f9").unwrap();
        parse("win+`").unwrap();
        assert!(parse("ctrl+alt").is_err());
        assert!(parse("ctrl+q+w").is_err());
        assert!(parse("ctrl+kp0").is_err());
    }
}
