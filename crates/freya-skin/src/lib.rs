//! Loading Freya skins: `.slint` files compiled at runtime against the skin API.
//!
//! A skin imports the API with `import { Player } from "@freya/v1.slint";`. The API files are
//! embedded in the binary and served to the compiler from a virtual folder, so skins never
//! need a copy of them on disk. Each API version is a separate file; a skin that asks for a
//! version this build does not know fails with a clear error.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use slint_interpreter::{
    CompilationResult, Compiler, ComponentDefinition, ComponentInstance, DiagnosticLevel,
};

pub mod paths;
pub mod render;
pub mod sample;
mod source;

pub use source::{
    MAIN_FILE, MANIFEST_FILE, Manifest, PACKAGE_EXTENSION, SkinInfo, SkinSource, discover, pack,
};

/// The newest skin API version this build provides.
pub const API_VERSION: u32 = 1;

const API_FILES: [(&str, &str); 1] = [("v1.slint", include_str!("../api/v1.slint"))];

/// The built-in skin.
const BUILTIN_SKIN: &str = include_str!("../../../skins/freya/skin.slint");
/// Where the built-in skin is in the source tree. It is compiled as if read from there, so its
/// images next to it are found where the tree is at hand, as in tests and `freya skin check`;
/// Freya itself runs the skin compiled with its images (see freya/build.rs).
const BUILTIN_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../skins/freya/skin.slint");

/// Freya's About window. Built from the API's `AboutFreya`, independent of the skin.
const ABOUT_WINDOW: &str = r#"
import { AboutFreya } from "@freya/v1.slint";
import { Palette } from "std-widgets.slint";

export component AboutWindow inherits Window {
    in property <string> version;
    title: "About Freya";
    preferred-width: 420px;
    preferred-height: 360px;
    background: Palette.background;

    AboutFreya {
        version: root.version;
        text-color: Palette.foreground;
    }
}
"#;

/// The skin picker: Freya's own window for switching skins. It does not depend on the current
/// skin, so it always works, even from a skin without a menu.
const SKIN_PICKER: &str = include_str!("../builtin/skin-picker.slint");
const TRAY: &str = include_str!("../builtin/tray.slint");

/// Globals every skin must export for the app to drive it.
const REQUIRED_GLOBALS: [&str; 1] = ["Player"];

/// Name of the virtual folder that `@freya/…` imports resolve to.
const API_DIR: &str = "__freya_skin_api__";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("skin does not compile:\n{0}")]
    Compile(String),
    #[error("skin defines no window component")]
    NoComponent,
    #[error("skin exports several components ({0}); export only its window")]
    SeveralComponents(String),
    #[error(
        "skin does not export the `{0}` global; add `export {{ {0} }} from \"@freya/v{API_VERSION}.slint\";`"
    )]
    MissingGlobal(&'static str),
    #[error("could not create skin window: {0}")]
    Platform(#[from] slint_interpreter::PlatformError),
    #[error("{0}")]
    Io(String),
    #[error("invalid skin package: {0}")]
    Package(String),
    #[error("invalid skin.toml: {0}")]
    Manifest(String),
    #[error("this skin needs skin API v{0}; this version of Freya provides up to v{API_VERSION}")]
    NeedsNewerFreya(u32),
}

fn compiler() -> Compiler {
    let mut compiler = Compiler::default();
    compiler.set_library_paths(HashMap::from([("freya".to_owned(), api_root())]));
    compiler.set_file_loader(|path: &Path| {
        let served = serve_api_file(path);
        Box::pin(async move { served })
    });
    compiler
}

/// Where `@freya` seems to live. Nothing is there: its files are served from memory. But Slint
/// looks for the API's own imports (`std-widgets.slint`) in that folder on disk first, so it
/// must be one only this user can write to: in the shared temporary folder on Linux, another
/// user could plant one there and make every skin fail to compile.
fn api_root() -> PathBuf {
    directories::ProjectDirs::from("", "", "Freya")
        .map(|dirs| dirs.cache_dir().join(API_DIR))
        .unwrap_or_else(|| std::env::temp_dir().join(API_DIR))
}

/// Serves `@freya/vN.slint` from the binary. Returns `None` for every other file, so the
/// compiler reads it from disk as usual.
fn serve_api_file(path: &Path) -> Option<std::io::Result<String>> {
    let in_api_dir = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == API_DIR);
    if !in_api_dir {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    Some(
        API_FILES
            .iter()
            .find(|(file, _)| *file == name)
            .map(|(_, source)| (*source).to_owned())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "this version of Freya provides skin API files up to v{API_VERSION}.slint, not {name}"
                    ),
                )
            }),
    )
}

/// Compiles a skin and returns its window component, without creating a window.
pub fn compile(source: &SkinSource) -> Result<ComponentDefinition, Error> {
    compile_with_warnings(source).map(|(component, _)| component)
}

/// Compiles a skin and reports its warnings, for skin authors (`freya skin check`).
pub fn check(source: &SkinSource) -> Result<Vec<String>, Error> {
    compile_with_warnings(source).map(|(_, warnings)| warnings)
}

fn compile_with_warnings(source: &SkinSource) -> Result<(ComponentDefinition, Vec<String>), Error> {
    // Reading the manifest checks its syntax and API version before compiling.
    SkinInfo::read(source)?;
    let compiler = compiler();
    let result = match source.main_file()? {
        None => spin_on::spin_on(
            compiler.build_from_source(BUILTIN_SKIN.to_owned(), PathBuf::from(BUILTIN_PATH)),
        ),
        Some(path) => spin_on::spin_on(compiler.build_from_path(path)),
    };
    let warnings = diagnostics(&result)?;
    // Which of several would be the window is down to a hash map's order.
    let mut names: Vec<&str> = result.component_names().collect();
    if names.len() > 1 {
        names.sort_unstable();
        return Err(Error::SeveralComponents(names.join(", ")));
    }
    let component = result.components().next().ok_or(Error::NoComponent)?;
    for global in REQUIRED_GLOBALS {
        if !component.globals().any(|g| g == global) {
            return Err(Error::MissingGlobal(global));
        }
    }
    Ok((component, warnings))
}

/// Compiles a skin and creates its window.
pub fn load(source: &SkinSource) -> Result<ComponentInstance, Error> {
    Ok(compile(source)?.create()?)
}

/// Creates the About window (not yet shown). Its `version` property should be set.
pub fn about_window() -> Result<ComponentInstance, Error> {
    let result = spin_on::spin_on(compiler().build_from_source(
        ABOUT_WINDOW.to_owned(),
        PathBuf::from("builtin/about.slint"),
    ));
    diagnostics(&result)?;
    Ok(result
        .components()
        .last()
        .ok_or(Error::NoComponent)?
        .create()?)
}

/// Creates the skin picker window (not yet shown).
pub fn skin_picker_window() -> Result<ComponentInstance, Error> {
    let result = spin_on::spin_on(compiler().build_from_source(
        SKIN_PICKER.to_owned(),
        PathBuf::from("builtin/skin-picker.slint"),
    ));
    diagnostics(&result)?;
    Ok(result
        .components()
        .last()
        .ok_or(Error::NoComponent)?
        .create()?)
}

/// Creates Freya's icon in the notification area, not yet shown: its `icon` is to be set, and
/// its `visible` turned on when it is wanted.
pub fn tray_icon() -> Result<ComponentInstance, Error> {
    let result = spin_on::spin_on(
        compiler().build_from_source(TRAY.to_owned(), PathBuf::from("builtin/tray.slint")),
    );
    diagnostics(&result)?;
    Ok(result
        .components()
        .last()
        .ok_or(Error::NoComponent)?
        .create()?)
}

/// Fails with the compile errors, or returns the warnings.
fn diagnostics(result: &CompilationResult) -> Result<Vec<String>, Error> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for diagnostic in result.diagnostics() {
        match diagnostic.level() {
            DiagnosticLevel::Error => errors.push(diagnostic.to_string()),
            _ => {
                log::warn!("skin: {diagnostic}");
                warnings.push(diagnostic.to_string());
            }
        }
    }
    if errors.is_empty() {
        Ok(warnings)
    } else {
        Err(Error::Compile(errors.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freya-skin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn repo_skins() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skins")
    }

    #[test]
    fn every_skin_in_the_repository_compiles_cleanly() {
        for entry in std::fs::read_dir(repo_skins()).unwrap().flatten() {
            let source = SkinSource::from_path(&entry.path());
            let warnings = check(&source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
            assert!(warnings.is_empty(), "{source:?}: {warnings:#?}");
        }
    }

    #[test]
    fn skin_picker_compiles_cleanly() {
        let result = spin_on::spin_on(compiler().build_from_source(
            SKIN_PICKER.to_owned(),
            PathBuf::from("builtin/skin-picker.slint"),
        ));
        let diagnostics: Vec<String> = result.diagnostics().map(|d| d.to_string()).collect();
        assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    }

    #[test]
    fn tray_icon_compiles_cleanly() {
        let result = spin_on::spin_on(
            compiler().build_from_source(TRAY.to_owned(), PathBuf::from("builtin/tray.slint")),
        );
        let diagnostics: Vec<String> = result.diagnostics().map(|d| d.to_string()).collect();
        assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    }

    #[test]
    fn packages_round_trip_and_are_discovered() {
        let dir = temp_dir("pack");
        let package = dir.join("mini.freyaskin");
        let warnings = pack(&repo_skins().join("mini"), &package).unwrap();
        assert!(warnings.is_empty());

        let source = SkinSource::from_path(&package);
        assert_eq!(source, SkinSource::Package(package.clone()));
        compile(&source).unwrap();
        let unpacked = source.main_file().unwrap().unwrap();
        assert!(unpacked.starts_with(source::package_cache().unwrap()));
        let _ = std::fs::remove_dir_all(unpacked.parent().unwrap());

        let skins = discover(&dir);
        let names: Vec<&str> = skins.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Freya", "Mini"]);
        assert_eq!(skins[1].author, "Freya");
        assert_eq!(skins[1].source, source);
    }

    #[test]
    fn a_manifest_that_is_not_utf8_is_an_error() {
        // In a folder it was taken for no manifest, and its API version went unchecked.
        let dir = temp_dir("latin1-manifest");
        std::fs::write(dir.join("skin.toml"), b"name = \"Caf\xe9\"\napi = 99\n").unwrap();
        let error = SkinInfo::read(&SkinSource::Folder(dir))
            .unwrap_err()
            .to_string();
        assert!(error.contains("UTF-8"), "{error}");
    }

    #[test]
    fn a_skin_exporting_two_windows_is_refused() {
        // Freya opened either of them, as a hash map's order had it.
        let dir = temp_dir("two-windows");
        std::fs::write(dir.join("skin.toml"), "name = \"Two\"\napi = 1\n").unwrap();
        std::fs::write(
            dir.join("skin.slint"),
            "export { Player } from \"@freya/v1.slint\";\n\
             export component A inherits Window { }\n\
             export component B inherits Window { }\n",
        )
        .unwrap();
        let Err(error) = compile(&SkinSource::Folder(dir)) else {
            panic!("compiled");
        };
        let error = error.to_string();
        assert!(error.contains("several components (A, B)"), "{error}");
    }

    #[test]
    fn a_package_written_into_its_folder_leaves_itself_out() {
        // It was listed, half written, among the files to pack.
        let dir = temp_dir("pack-inside");
        let folder = dir.join("mini");
        std::fs::create_dir_all(&folder).unwrap();
        // Mini is one folder of files.
        for file in std::fs::read_dir(repo_skins().join("mini")).unwrap() {
            let file = file.unwrap().path();
            std::fs::copy(&file, folder.join(file.file_name().unwrap())).unwrap();
        }
        let package = folder.join("mini.freyaskin");
        pack(&folder, &package).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&package).unwrap()).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().unwrap().into_owned())
            .collect();
        assert!(
            !names.iter().any(|n| n.ends_with(".freyaskin")),
            "{names:?}"
        );
    }

    #[test]
    fn package_paths_outside_the_folder_are_rejected() {
        use std::io::Write;
        let dir = temp_dir("zipslip");
        // `..`, and on Windows a verbatim path whose `/` only becomes a separator once it is
        // joined onto the folder (zip before 9.0.4 let that one through).
        // Then names that are harmless on macOS and Linux but leave the folder on Windows: a
        // drive after the first part (`x/D:/...` becomes the drive-relative `D:...`, which
        // replaces the folder when joined onto it), and `\` as a separator.
        for name in [
            "../escaped.txt",
            r"\\?\C:\../escaped.txt",
            r"\\?\C:\a/../../escaped.txt",
            "x/D:/Users/Public/escaped.txt",
            "x/C:escaped.txt",
            r"a\..\..\escaped.txt",
            "skin.slint:stream",
        ] {
            let package = dir.join("evil.freyaskin");
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&package).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("skin.slint", options).unwrap();
            zip.write_all(b"export component W inherits Window {}")
                .unwrap();
            zip.start_file(name, options).unwrap();
            zip.write_all(b"nope").unwrap();
            zip.finish().unwrap();

            let Err(err) = compile(&SkinSource::Package(package)) else {
                panic!("a package with the path {name} must be rejected");
            };
            assert!(matches!(err, Error::Package(_)), "{name}: {err}");
            assert!(
                !source::package_cache()
                    .unwrap()
                    .join("escaped.txt")
                    .exists()
            );
        }
    }

    #[test]
    fn manifests_for_newer_apis_are_rejected() {
        let dir = temp_dir("manifest");
        std::fs::write(dir.join(MAIN_FILE), "export component W inherits Window {}").unwrap();
        std::fs::write(dir.join(MANIFEST_FILE), "name = \"Future\"\napi = 99\n").unwrap();
        let Err(err) = compile(&SkinSource::Folder(dir.clone())) else {
            panic!("API 99 must be rejected");
        };
        assert!(matches!(err, Error::NeedsNewerFreya(99)), "{err}");
        std::fs::write(dir.join(MANIFEST_FILE), "colour = \"blue\"\n").unwrap();
        assert!(matches!(
            SkinInfo::read(&SkinSource::Folder(dir)),
            Err(Error::Manifest(_))
        ));
    }

    #[test]
    fn about_window_compiles_cleanly() {
        let result = spin_on::spin_on(compiler().build_from_source(
            ABOUT_WINDOW.to_owned(),
            PathBuf::from("builtin/about.slint"),
        ));
        let diagnostics: Vec<String> = result.diagnostics().map(|d| d.to_string()).collect();
        assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    }

    #[test]
    fn builtin_skin_compiles_cleanly() {
        let compiler = compiler();
        let result = spin_on::spin_on(
            compiler.build_from_source(BUILTIN_SKIN.to_owned(), PathBuf::from(BUILTIN_PATH)),
        );
        let diagnostics: Vec<String> = result.diagnostics().map(|d| d.to_string()).collect();
        assert!(diagnostics.is_empty(), "{diagnostics:#?}");
        let component = result.components().last().expect("a component");
        let globals: Vec<String> = component.globals().collect();
        for global in ["Player", "Playlist", "App"] {
            assert!(
                globals.iter().any(|g| g == global),
                "{global} not in {globals:?}"
            );
        }
    }

    fn write_skin(name: &str, source: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freya-skin-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, source).unwrap();
        path
    }

    #[test]
    fn skin_must_export_the_api_globals() {
        let path = write_skin(
            "no-export.slint",
            "import { Player } from \"@freya/v1.slint\";\nexport component W inherits Window { Text { text: Player.title; } }\n",
        );
        let Err(err) = compile(&SkinSource::File(path)) else {
            panic!("a skin that does not export Player must be rejected");
        };
        assert!(matches!(err, Error::MissingGlobal("Player")), "{err}");
    }

    #[test]
    fn unknown_api_version_is_reported() {
        let path = write_skin(
            "future.slint",
            "import { Player } from \"@freya/v99.slint\";\nexport component W inherits Window { Text { text: Player.title; } }\n",
        );
        let Err(err) = compile(&SkinSource::File(path)) else {
            panic!("a skin asking for an unknown API version must not compile");
        };
        let err = err.to_string();
        assert!(err.contains("v99.slint"), "{err}");
    }
}
