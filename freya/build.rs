//! Compiles the built-in skin into Freya. Other skins are read when Freya runs, by Slint's
//! interpreter; compiled, the built-in one starts faster and uses about a third less memory.
//!
//! For Windows, also gives freya.exe Freya's icon and version, as Explorer and the task bar
//! show them.
//!
//! Builds the hologram's poses (assets/hologram/, JPEG or PNG) into Freya: the hologram
//! visualizer projects them.

use std::collections::HashMap;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    // `@freya/v1.slint` in the skin is the skin API, which the interpreter serves from memory.
    let library = HashMap::from([("freya".to_owned(), root.join("crates/freya-skin/api"))]);
    let config = slint_build::CompilerConfiguration::new().with_library_paths(library);
    slint_build::compile_with_config(root.join("skins/freya/skin.slint"), config)
        .expect("the built-in skin compiles");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_resources(&root);
    }
    hologram_poses(&root);
}

/// `POSES`: the hologram's pose images, in the order of their file names.
fn hologram_poses(root: &std::path::Path) {
    let dir = root.join("assets/hologram");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("png")
            })
        })
        .collect();
    files.sort();
    let entries: String = files
        .iter()
        .map(|path| {
            println!("cargo:rerun-if-changed={}", path.display());
            format!("    include_bytes!({:?}),\n", path.display().to_string())
        })
        .collect();
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(
        out.join("hologram_poses.rs"),
        format!("pub const POSES: &[&[u8]] = &[\n{entries}];\n"),
    )
    .expect("OUT_DIR is writable");
}

/// Icon and version information, compiled with the Windows SDK's resource compiler, or with
/// MinGW's windres when building for Windows elsewhere.
fn windows_resources(root: &std::path::Path) {
    let icon = root.join("assets/icons/icon.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let mut resources = winresource::WindowsResource::new();
    resources
        .set_icon(&icon.to_string_lossy())
        .set("ProductName", "Freya")
        .set("FileDescription", "Freya")
        .set("LegalCopyright", "Freya contributors. MIT OR Apache-2.0.");
    resources.compile().expect("the Windows resources compile");
}
