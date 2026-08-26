//! Launch-at-login via the per-user registry Run key — the standard Windows
//! mechanism, no admin rights, uninstaller-independent.

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
    };

    const VALUE: &str = "fastrans";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn open(access: u32) -> Option<HKEY> {
        let path = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        let mut key: HKEY = std::ptr::null_mut();
        let rc = unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, access, &mut key)
        };
        (rc == 0).then_some(key)
    }

    fn exe_command() -> Option<String> {
        let exe = std::env::current_exe().ok()?;
        Some(format!("\"{}\"", exe.display()))
    }

    pub fn is_enabled() -> bool {
        let Some(key) = open(KEY_QUERY_VALUE) else {
            return false;
        };
        let name = wide(VALUE);
        let mut buf = [0u8; 2048];
        let mut len = buf.len() as u32;
        let mut ty = 0u32;
        let rc = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut ty,
                buf.as_mut_ptr(),
                &mut len,
            )
        };
        unsafe { RegCloseKey(key) };
        if rc != 0 || ty != REG_SZ {
            return false;
        }
        let stored: Vec<u16> = buf[..len as usize]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        let stored = String::from_utf16_lossy(&stored);
        exe_command().is_some_and(|cmd| stored.eq_ignore_ascii_case(&cmd))
    }

    pub fn set(enabled: bool) -> bool {
        let Some(key) = open(KEY_SET_VALUE) else {
            return false;
        };
        let name = wide(VALUE);
        let rc = if enabled {
            let Some(cmd) = exe_command() else {
                unsafe { RegCloseKey(key) };
                return false;
            };
            let data = wide(&cmd);
            unsafe {
                RegSetValueExW(
                    key,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr() as *const u8,
                    (data.len() * 2) as u32,
                )
            }
        } else {
            let rc = unsafe { RegDeleteValueW(key, name.as_ptr()) };
            // Deleting a value that isn't there is success for our purposes.
            if rc == 2 {
                0
            } else {
                rc
            }
        };
        unsafe { RegCloseKey(key) };
        rc == 0
    }
}

#[cfg(windows)]
pub use imp::{is_enabled, set};

#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set(_enabled: bool) -> bool {
    false
}
