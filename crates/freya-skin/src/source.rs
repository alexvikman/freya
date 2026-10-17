//! Where skins come from: the built-in skin, a `.slint` file, a skin folder or a `.freyaskin`
//! package, and their manifests.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{API_VERSION, Error};

/// File name of a skin's main file inside a folder or package.
pub const MAIN_FILE: &str = "skin.slint";
/// File name of a skin's manifest inside a folder or package.
pub const MANIFEST_FILE: &str = "skin.toml";
pub const PACKAGE_EXTENSION: &str = "freyaskin";

/// Largest file a package may contain, and the most it may unpack to in total.
const MAX_FILE_SIZE: u64 = 32 * 1024 * 1024;
const MAX_PACKAGE_SIZE: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkinSource {
    Builtin,
    /// A single `.slint` file; files it imports are resolved relative to it.
    File(PathBuf),
    /// A folder with `skin.slint` and optionally `skin.toml`, images and fonts.
    Folder(PathBuf),
    /// A `.freyaskin` file: a zip of a skin folder.
    Package(PathBuf),
}

impl SkinSource {
    /// Works out the kind of skin at `path`.
    pub fn from_path(path: &Path) -> Self {
        if path.is_dir() {
            Self::Folder(path.to_owned())
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            e.eq_ignore_ascii_case(PACKAGE_EXTENSION) || e.eq_ignore_ascii_case("zip")
        }) {
            Self::Package(path.to_owned())
        } else {
            Self::File(path.to_owned())
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Builtin => None,
            Self::File(p) | Self::Folder(p) | Self::Package(p) => Some(p),
        }
    }

    /// The `.slint` file to compile, unpacking a package first if needed.
    pub(crate) fn main_file(&self) -> Result<Option<PathBuf>, Error> {
        Ok(match self {
            Self::Builtin => None,
            Self::File(p) => Some(p.clone()),
            Self::Folder(dir) => Some(dir.join(MAIN_FILE)),
            Self::Package(p) => Some(unpack(p)?.join(MAIN_FILE)),
        })
    }

    /// Files whose changes should reload the skin while it is in use. Packages are watched as
    /// a whole; the built-in skin never changes.
    pub fn watched_files(&self) -> Vec<PathBuf> {
        match self {
            Self::Builtin => Vec::new(),
            Self::File(p) => {
                // Files next to it may be imported; watching the folder's .slint files is cheap.
                p.parent()
                    .map(|dir| files_in(dir, false))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|f| f.extension().is_some_and(|e| e == "slint"))
                    .chain(std::iter::once(p.clone()))
                    .collect()
            }
            Self::Folder(dir) => files_in(dir, true),
            Self::Package(p) => vec![p.clone()],
        }
    }
}

/// Optional `skin.toml`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Manifest {
    pub name: Option<String>,
    pub author: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    /// Skin API version the skin is written for.
    pub api: Option<u32>,
}

/// What the skin picker shows about a skin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkinInfo {
    pub source: SkinSource,
    pub name: String,
    pub author: String,
    pub version: String,
    pub description: String,
}

impl SkinInfo {
    pub fn builtin() -> Self {
        Self {
            source: SkinSource::Builtin,
            name: "Freya".into(),
            author: "Freya".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "The built-in skin.".into(),
        }
    }

    /// Reads a skin's manifest, falling back to its file name when there is none.
    pub fn read(source: &SkinSource) -> Result<Self, Error> {
        let manifest = match source {
            SkinSource::Builtin => return Ok(Self::builtin()),
            SkinSource::File(_) => Manifest::default(),
            SkinSource::Folder(dir) => match std::fs::read_to_string(dir.join(MANIFEST_FILE)) {
                Ok(text) => parse_manifest(&text)?,
                Err(_) => Manifest::default(),
            },
            SkinSource::Package(p) => match read_package_file(p, MANIFEST_FILE)? {
                Some(bytes) => parse_manifest(&String::from_utf8_lossy(&bytes))?,
                None => Manifest::default(),
            },
        };
        let stem = source
            .path()
            .and_then(Path::file_stem)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Self {
            source: source.clone(),
            name: manifest.name.unwrap_or(stem),
            author: manifest.author.unwrap_or_default(),
            version: manifest.version.unwrap_or_default(),
            description: manifest.description.unwrap_or_default(),
        })
    }
}

fn parse_manifest(text: &str) -> Result<Manifest, Error> {
    let manifest: Manifest = toml::from_str(text).map_err(|e| Error::Manifest(e.to_string()))?;
    if let Some(api) = manifest.api
        && api > API_VERSION
    {
        return Err(Error::NeedsNewerFreya(api));
    }
    Ok(manifest)
}

/// The built-in skin followed by every skin in `dir`: folders with a `skin.slint`,
/// `.freyaskin` packages and loose `.slint` files. Skins whose manifest cannot be read are
/// skipped with a warning.
pub fn discover(dir: &Path) -> Vec<SkinInfo> {
    let mut skins = vec![SkinInfo::builtin()];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return skins;
    };
    let mut found: Vec<SkinInfo> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            if p.is_dir() {
                p.join(MAIN_FILE).is_file()
            } else {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e == PACKAGE_EXTENSION || e == "slint")
            }
        })
        .filter_map(|p| match SkinInfo::read(&SkinSource::from_path(&p)) {
            Ok(info) => Some(info),
            Err(e) => {
                log::warn!("skipping skin {}: {e}", p.display());
                None
            }
        })
        .collect();
    found.sort_by_key(|s| s.name.to_lowercase());
    skins.extend(found);
    skins
}

/// Unpacks a package into a cache folder, reusing it while the package is unchanged.
fn unpack(package: &Path) -> Result<PathBuf, Error> {
    let meta = std::fs::metadata(package).map_err(|e| io(package, e))?;
    let mut hasher = DefaultHasher::new();
    package.hash(&mut hasher);
    meta.len().hash(&mut hasher);
    meta.modified().ok().hash(&mut hasher);
    let stem = package
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cache = package_cache()?;
    let target = cache.join(format!("{stem}-{:016x}", hasher.finish()));
    if target.join(MAIN_FILE).is_file() {
        return Ok(target);
    }

    let staging = cache.join(format!(
        ".unpacking-{}-{:016x}",
        std::process::id(),
        hasher.finish()
    ));
    let _ = std::fs::remove_dir_all(&staging);
    if let Err(e) = extract(package, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&staging, &target).map_err(|e| io(&target, e))?;
    Ok(target)
}

/// Where packages are unpacked: Freya's cache folder, which belongs to the current user.
/// Not the shared temporary folder, where another user of the computer could plant a
/// folder that Freya would then load as the skin.
pub(crate) fn package_cache() -> Result<PathBuf, Error> {
    let dirs = directories::ProjectDirs::from("", "", "Freya")
        .ok_or_else(|| Error::Package("no cache folder for this user".into()))?;
    let cache = dirs.cache_dir().join("skins");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&cache).map_err(|e| io(&cache, e))?;
    Ok(cache)
}

/// Extracts every file of `package` into the new folder `out`, checking paths and sizes.
fn extract(package: &Path, out: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(out).map_err(|e| io(out, e))?;
    let file = std::fs::File::open(package).map_err(|e| io(package, e))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| Error::Package(e.to_string()))?;
    if archive.len() > MAX_ENTRIES {
        return Err(Error::Package(format!("more than {MAX_ENTRIES} files")));
    }
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| Error::Package(e.to_string()))?;
        // Rejects absolute paths and `..`, so nothing is written outside the folder.
        let Some(relative) = entry.enclosed_name() else {
            return Err(Error::Package(format!(
                "unsafe path in package: {:?}",
                entry.name()
            )));
        };
        let path = out.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| io(&path, e))?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        }
        let mut bytes = Vec::new();
        (&mut entry)
            .take(MAX_FILE_SIZE + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::Package(e.to_string()))?;
        total += bytes.len() as u64;
        if bytes.len() as u64 > MAX_FILE_SIZE || total > MAX_PACKAGE_SIZE {
            return Err(Error::Package("package is too large".into()));
        }
        std::fs::write(&path, bytes).map_err(|e| io(&path, e))?;
    }
    if !out.join(MAIN_FILE).is_file() {
        return Err(Error::Package(format!(
            "no {MAIN_FILE} at the top of the package"
        )));
    }
    Ok(())
}

fn read_package_file(package: &Path, name: &str) -> Result<Option<Vec<u8>>, Error> {
    let file = std::fs::File::open(package).map_err(|e| io(package, e))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| Error::Package(e.to_string()))?;
    let Ok(entry) = archive.by_name(name) else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    entry
        .take(MAX_FILE_SIZE)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::Package(e.to_string()))?;
    Ok(Some(bytes))
}

/// Packs a skin folder into a `.freyaskin` file, after checking that it compiles.
pub fn pack(folder: &Path, out: &Path) -> Result<Vec<String>, Error> {
    let warnings = crate::check(&SkinSource::Folder(folder.to_owned()))?;
    let file = std::fs::File::create(out).map_err(|e| io(out, e))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for path in files_in(folder, true) {
        let relative = path
            .strip_prefix(folder)
            .map_err(|e| Error::Package(e.to_string()))?;
        // Zip paths always use forward slashes.
        let name = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let bytes = std::fs::read(&path).map_err(|e| io(&path, e))?;
        zip.start_file(name, options)
            .map_err(|e| Error::Package(e.to_string()))?;
        zip.write_all(&bytes).map_err(|e| io(out, e))?;
    }
    zip.finish().map_err(|e| Error::Package(e.to_string()))?;
    Ok(warnings)
}

/// Files in `dir` (recursively when `deep`), skipping hidden ones, sorted.
fn files_in(dir: &Path, deep: bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        if path.is_dir() {
            if deep {
                files.extend(files_in(&path, true));
            }
        } else {
            files.push(path);
        }
    }
    files.sort();
    files
}

fn io(path: &Path, e: std::io::Error) -> Error {
    Error::Io(format!("{}: {e}", path.display()))
}
