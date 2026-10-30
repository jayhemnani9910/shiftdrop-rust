//! On Windows (MSVC), embed the app's manifest: per-monitor DPI and current-style dialog controls.
fn main() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/windows/shiftdrop.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    if std::env::var("TARGET").is_ok_and(|t| t.ends_with("windows-msvc")) {
        println!("cargo:rustc-link-arg-bin=shiftdrop=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bin=shiftdrop=/MANIFESTINPUT:{}", manifest.display());
    }
}
