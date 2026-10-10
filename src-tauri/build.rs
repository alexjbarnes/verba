fn main() {
    tauri_build::build();

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    // `generate_context!()` bakes frontendDist into the binary at COMPILE time,
    // yet tauri_build's own rerun-if-changed list covers only tauri.conf.json,
    // the gradle files and capabilities. The `tauri` CLI does force a rebuild
    // on frontend edits, but a plain `cargo build` has no declared dependency
    // on the assets it embeds. Declaring them makes the rebuild correct for
    // every entry point.
    rerun_if_frontend_changed(std::path::Path::new(&manifest_dir).join("../src"));
}

/// Emit `rerun-if-changed` for every file under the frontend directory.
/// Cargo only watches a directory's own mtime, which misses edits to files
/// inside it, so each file is registered individually.
fn rerun_if_frontend_changed(dir: std::path::PathBuf) {
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    println!("cargo:rerun-if-changed={}", dir.display());
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rerun_if_frontend_changed(path);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
