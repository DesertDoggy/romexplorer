// The icon inside the Windows executables (rommusic.exe and rommusic-gui.exe).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/rommusic.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/rommusic.ico");
        if let Err(err) = res.compile() {
            // No resource compiler on this machine: the tools still work, with the default icon.
            println!("cargo:warning=rommusic icon not embedded: {err}");
        }
    }
}
