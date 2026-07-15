fn main() {
    // Embed the application icon on Windows so the .exe, taskbar, and Start-menu
    // search result show the IronVNC icon. Non-fatal if the resource compiler
    // isn't available — the app still builds, just without an embedded icon.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/ironvnc.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/ironvnc.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon embed skipped: {e}");
        }
    }
}
