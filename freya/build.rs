//! Compiles the built-in skin into Freya. Other skins are read when Freya runs, by Slint's
//! interpreter; compiled, the built-in one starts faster and uses about a third less memory.
//!
//! For Windows, also gives freya.exe Freya's icon and version, as Explorer and the task bar
//! show them.

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
