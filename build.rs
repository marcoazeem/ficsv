// Embed the Faruma font (Dhivehi/Thaana) from assets/fonts/ so Dhivehi renders in Faruma
// even on machines that don't have it installed.
fn main() {
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
