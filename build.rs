// Embed the Faruma font (Dhivehi/Thaana) from assets/fonts/ so Dhivehi renders in Faruma
// even on machines that don't have it installed.
fn main() {
    // Windows: embed the app icon into ficsv.exe (Explorer, taskbar, Alt-Tab).
    println!("cargo::rerun-if-changed=assets/icon/ficsv.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/ficsv.ico");
        res.set("ProductName", "FI CSV");
        res.set("FileDescription", "FI CSV");
        if let Err(e) = res.compile() {
            println!("cargo::warning=could not embed Windows icon: {e}");
        }
    }

    println!("cargo::rustc-check-cfg=cfg(ficsv_bundled_faruma)");
    for name in ["assets/fonts/Faruma.otf", "assets/fonts/Faruma.ttf"] {
        println!("cargo::rerun-if-changed={name}");
        let p = std::path::Path::new(name);
        if p.is_file() {
            let abs = std::fs::canonicalize(p).unwrap();
            println!("cargo::rustc-cfg=ficsv_bundled_faruma");
            println!("cargo::rustc-env=FICSV_FARUMA_PATH={}", abs.display());
            return;
        }
    }
}
