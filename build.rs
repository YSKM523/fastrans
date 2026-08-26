fn main() {
    // Embed the app icon into the Windows executable (Explorer, taskbar).
    #[cfg(target_os = "windows")]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon resource embedding failed: {e}");
        }
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
}
