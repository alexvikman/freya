//! Compiles the built-in skin into Freya. Other skins are read when Freya runs, by Slint's
//! interpreter; compiled, the built-in one starts faster and uses about a third less memory.
//!
//! For Windows, also gives freya.exe Freya's icon and version, as Explorer and the task bar
//! show them.
//!
//! Builds the hologram's poses (assets/hologram/, JPEG or PNG) into Freya: the hologram
//! visualizer projects them.
//!
//! Gives Freya its Last.fm API key and shared secret, when the build has them (see
//! docs/RELEASING.md); never from the repository.

use std::collections::HashMap;
use std::path::PathBuf;

fn main() {
    // Where Cargo says the package is as it runs this script: one built into the script would
    // be where it was compiled, an old checkout after the folder moved.
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").expect("run by Cargo");
    let root = PathBuf::from(manifest_dir).join("..");
    // `@freya/v1.slint` in the skin is the skin API, which the interpreter serves from memory.
    let library = HashMap::from([("freya".to_owned(), root.join("crates/freya-skin/api"))]);
    let config = slint_build::CompilerConfiguration::new().with_library_paths(library);
    slint_build::compile_with_config(root.join("skins/freya/skin.slint"), config)
        .expect("the built-in skin compiles");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_resources(&root);
    }
    hologram_poses(&root);
    lastfm_credentials(&root);
}

/// `FREYA_LASTFM_API_KEY` and `FREYA_LASTFM_SECRET` for `option_env!`: from the environment, as
/// release builds set them from the repository's secrets, or else from the checkout's `.env`,
/// which Git ignores. Without them Freya builds, and Last.fm scrobbling is off.
fn lastfm_credentials(root: &std::path::Path) {
    const NAMES: [&str; 2] = ["FREYA_LASTFM_API_KEY", "FREYA_LASTFM_SECRET"];
    let env_file = root.join(".env");
    // A missing file is not watched (Cargo would then run this script on every build): after
    // creating `.env`, touch build.rs once.
    if env_file.exists() {
        println!("cargo:rerun-if-changed={}", env_file.display());
    }
    let file = std::fs::read_to_string(&env_file).unwrap_or_default();
    for name in NAMES {
        println!("cargo:rerun-if-env-changed={name}");
        let from_file = file
            .lines()
            .filter_map(|line| line.split_once('='))
            .find(|(key, _)| key.trim() == name)
            .map(|(_, value)| value.trim().trim_matches('"').to_owned());
        let value = std::env::var(name)
            .ok()
            .filter(|v| !v.is_empty())
            .or(from_file);
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            println!("cargo:rustc-env={name}={value}");
        }
    }
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
