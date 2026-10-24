//! For Windows, gives freya-server.exe Freya's icon and its name, as Explorer and the
//! notification area show them.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").expect("run by Cargo");
    let icon = std::path::PathBuf::from(manifest_dir).join("../../assets/icons/icon.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let mut resources = winresource::WindowsResource::new();
    resources
        .set_icon(&icon.to_string_lossy())
        .set("ProductName", "Freya Server")
        .set("FileDescription", "Freya Server")
        .set("LegalCopyright", "Freya contributors. MIT OR Apache-2.0.");
    resources.compile().expect("the Windows resources compile");
}
