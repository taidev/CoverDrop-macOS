fn main() {
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let executable = if target_os == "windows" {
            "bin/pdftoppm.exe"
        } else {
            "pdftoppm"
        };
        let renderer = format!("binaries/{target_os}-{target_arch}/{executable}");
        if !std::path::Path::new(&renderer).is_file() {
            panic!("Release packaging requires a bundled Poppler renderer at {renderer}.");
        }
    }
    tauri_build::build()
}
