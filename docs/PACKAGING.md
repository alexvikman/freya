# Packaging Freya

Installers are made with [cargo-packager](https://github.com/crabnebula-dev/cargo-packager), configured under `[package.metadata.packager]` in [`freya/Cargo.toml`](../freya/Cargo.toml). Each system builds its own:

| System | Formats | Command |
|---|---|---|
| macOS | `Freya.app`, `.dmg`, `.app.zip` (signed) | `ci/package-macos.sh` (see [RELEASING.md](RELEASING.md)); unsigned: `cargo packager -p freya --release --formats app,dmg` |
| Windows | `.msi` (WiX) | `cargo packager -p freya --release --formats wix` |
| Linux | `.AppImage`, `.deb` | `cargo packager -p freya --release --formats appimage,deb` |

Install the tool once with `cargo install cargo-packager --locked`. Packages land in `target/packages/`. The [Release workflow](../.github/workflows/release.yml) builds and signs all of them on GitHub: run by hand it keeps them as downloadable artifacts, and for a version tag it publishes them as a release (see [RELEASING.md](RELEASING.md)).

Building needs CMake (for the Opus decoder) besides Rust; on Linux also the ALSA, fontconfig and xkbcommon headers, and FUSE to build the AppImage (see the workflow).

Without GitHub, `ci/check.sh package` builds the Linux AppImage and `.deb` in Docker, for the machine's architecture (arm64 on Apple Silicon; x86-64 needs Docker's emulation and is slow). Packages land in the `freya-ci-target` Docker volume, under `/target/packages`.

## Icons

[`assets/icon-art.png`](../assets/icon-art.png) is the artwork (1024 pixels square). `swift ci/make-icons.swift` (on a Mac) puts it on a rounded square following the macOS icon grid, with a soft shadow and transparent corners, and writes the PNG sizes and `icon.ico` (16–256 pixels) into `assets/icons/`; cargo-packager makes the macOS `.icns` from the PNGs. Run it again after changing the artwork.

Freya also carries its icon itself, for where the system takes it from the running program rather than from a package:

- `freya.exe` has `icon.ico` and its version as Windows resources, compiled in by `freya/build.rs` (with the Windows SDK's resource compiler, or MinGW's windres when cross-compiling). `ci/check.sh windows` checks that the resources are there.
- `128x128.png` is built into Freya: it is the windows' icon on Windows and X11, and the Dock icon on macOS when Freya runs outside its app bundle, as during development (see `freya/src/gui/app_icon.rs`). Wayland desktops take the icon from the installed `.desktop` file.

## Before publishing

- **Identifier and maintainer.** The identifier is `io.github.alexvikman.freya`, after the GitHub repository Freya is released from; Freya's settings and library are found by its name, so changing it moves nothing. The author "Freya contributors" is still a placeholder for a maintainer name and address.
- **macOS signing.** `ci/package-macos.sh` signs the app ad hoc with the hardened runtime, so macOS warns the first time a downloaded copy is opened. With an Apple Developer ID it also notarizes; see [RELEASING.md](RELEASING.md).
- **Windows signing.** The `.msi` and `freya.exe` are unsigned, so SmartScreen warns; see [RELEASING.md](RELEASING.md).
- **Licenses.** Freya is MIT OR Apache-2.0. The packages include libopus (BSD) and Slint under its royalty-free license, which is credited in Freya's About window.

## What was tested

- **macOS:** `.app` and `.dmg` built; the app starts from the bundle and saves its settings when quit from the system.
- **Linux (arm64, in Docker):** all checks and tests pass on Ubuntu 22.04. The `.deb` installs on a clean Ubuntu 22.04 with its dependencies, finds all libraries, and `freya --version` and `freya skin check` work; the AppImage runs. The window itself was not opened (no display in the container). x86-64 packages were not built.
- **Windows:** cross-compiled with MinGW: clippy passes and `freya.exe` links as a window app. Not run, and the MSVC build and `.msi` need a Windows machine.
