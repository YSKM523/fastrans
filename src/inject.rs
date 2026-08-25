//! Commit text into the previously focused application:
//! save clipboard -> set English text -> synthesize paste -> restore clipboard.

use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};

/// Blocking paste sequence. Call from a background thread after the input bar
/// has been hidden and focus has returned to the target application.
pub fn paste_text(text: &str) -> Result<()> {
    let mut clipboard = Clipboard::new()?;
    let saved = clipboard.get_text().ok();

    clipboard.set_text(text)?;
    // Give the clipboard owner change a moment to settle before pasting.
    thread::sleep(Duration::from_millis(20));

    let mut enigo = Enigo::new(&Settings::default())?;
    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;

    enigo.key(modifier, Direction::Press)?;
    let click = enigo.key(Key::Unicode('v'), Direction::Click);
    // Always release the modifier, even if the V injection failed — a stuck
    // Ctrl/Cmd is far worse than a missed paste.
    let release = enigo.key(modifier, Direction::Release);
    click?;
    release?;

    // Let the target app read the clipboard before restoring it.
    thread::sleep(Duration::from_millis(300));
    if let Some(old) = saved {
        let _ = clipboard.set_text(old);
    }
    Ok(())
}

/// The current foreground window handle (0 when none). Used to detect when
/// focus has actually left the bar instead of sleeping a fixed interval.
#[cfg(windows)]
pub fn foreground_window() -> isize {
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() as isize }
}

#[cfg(not(windows))]
pub fn foreground_window() -> isize {
    0
}

/// Waits (up to `timeout`) until the foreground window differs from `prev`,
/// then a short settle. Falls back to the full timeout if nothing changes.
pub fn wait_focus_leave(prev: isize, timeout: Duration) {
    #[cfg(windows)]
    {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let fg = foreground_window();
            if fg != 0 && fg != prev {
                thread::sleep(Duration::from_millis(15));
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = prev;
        thread::sleep(timeout);
    }
}
