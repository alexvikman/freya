# Developing Freya

Freya is written in Rust; its interface is built with [Slint](https://slint.dev). See [PLAN.md](PLAN.md) for the architecture and milestones.

## Building

Requires Rust ([rustup](https://rustup.rs) installs the version in `rust-toolchain.toml` by itself) and [CMake](https://cmake.org), which builds libopus: the Opus decoder, and Freya Server's encoder (`brew install cmake` on macOS; build Freya without Opus with `--no-default-features` on `freya-audio`). On Linux, install the ALSA, fontconfig and xkbcommon development headers (`libasound2-dev libfontconfig1-dev libxkbcommon-dev` on Debian/Ubuntu).

```sh
cargo build --release
```

Run the server and the player from the checkout:

```sh
cargo run --release -p freya-server &   # the library, in the background
cargo run --release -p freya
```

Options of `freya`: `--skin path` loads a skin (a `.slint` file, a skin folder or a `.freyaskin` package) and remembers it; `--renderer femtovg|software` picks the renderer (the default is software on macOS, where it keeps the window steady while it is resized, and FemtoVG on the GPU elsewhere). `SLINT_BACKEND` overrides both.

The built-in skin's spectrum is drawn by Freya in its own layer on top of the window; `FREYA_NATIVE_SPECTRUM=off` has the skin draw it instead. The visualizer's frame rate is `visualizer_fps` in `settings.toml`.

## Checks

`ci/check.sh` runs the checks for Linux and cross-compiles for Windows in Docker, so other systems can be checked from a Mac: `ci/check.sh linux`, `ci/check.sh windows`, `ci/check.sh deps` (dependency checks), `ci/check.sh secrets` (credentials in any commit), `ci/check.sh workflows` (the GitHub workflows, with actionlint), or all five without an argument. `ci/check.sh gui` runs Freya on virtual X11 and Wayland screens and saves screenshots and CPU use in `target/ci-gui/`; `ci/check.sh licenses` writes `licenses/THIRD-PARTY.html` anew.

Commits are checked for credentials before they are made: after cloning, turn the hook on with `git config core.hooksPath .githooks` (it needs [gitleaks](https://github.com/gitleaks/gitleaks), e.g. `brew install gitleaks`).

Dependencies are pinned, checked against the RustSec database and recorded as audited or knowingly exempted; see [DEPENDENCIES.md](DEPENDENCIES.md).

## Packages and releases

Installers of Freya and of Freya Server for macOS (`.dmg`), Windows (`.msi`) and Linux (`.AppImage`, `.deb`) are made with cargo-packager; see [PACKAGING.md](PACKAGING.md) and [RELEASING.md](RELEASING.md). [SECURITY.md](SECURITY.md) describes how client and server trust each other.

## Skins

[skins.md](skins.md) is the guide for skin authors. The API is defined in [`crates/freya-skin/api/v1.slint`](../crates/freya-skin/api/v1.slint); [`skins/freya/`](../skins/freya/skin.slint) is the built-in skin and [`skins/mini/`](../skins/mini/skin.slint) a small example.

## Layout

| Path | Contents |
|---|---|
| `freya/` | Freya, the player: its window, Up Next, the equalizer's presets and `.eqf` files, and the terminal player |
| `crates/freya-audio/` | Audio engine: 10-band equalizer, output, gapless playback, files from a server through a slow network |
| `crates/freya-dirs/` | Where Freya and its server keep their files on each system: settings, data and cache folders, and the user's home |
| `crates/freya-decode/` | Reading music files as samples, for both: the formats and decoders (Symphonia's, Opus from libopus, Monkey's Audio), and changing channels and rates |
| `crates/freya-client/` | The client's side of the library: a Freya server's library, called over HTTP |
| `crates/freya-http/` | A small HTTP/1.1 server, over TLS or not: Freya Server's, and Freya's login proxy |
| `crates/freya-lastfm/` | Last.fm's web service: what it says of artists and genres, signing in and scrobbles |
| `crates/freya-library/` | Freya Server's media library: the tracks, albums, artists, plays and playlists in SQLite, the scans, what is fetched online for it, and organizing the music's files: writing their tags and identifying their releases |
| `crates/freya-media/` | Music files, for both: finding them, tags, CUE sheets, playlist entries and M3U/PLS files, covers, pictures of artists |
| `crates/freya-protocol/` | What client and server say to each other: the library's tracks, artists, albums, genres and playlists, and how scans go |
| `crates/freya-server/` | `freya-server`: the library as a service over HTTP, with the scans that keep it in line with the music's folders, the folders' watch and the schedule; backups and exports |
| `crates/freya-transcode/` | Freya Server's tracks made smaller for a slow network: decoded and encoded again as Opus in Ogg, at a constant bitrate, its length known before it is made |
| `crates/freya-skin/` | Skin API, skin folders and packages, the skin picker, headless rendering for previews and tests |
| `crates/freya-update/` | Finding, verifying and installing releases, for Freya and Freya Server |
| `skins/` | The built-in skin (`freya/`) and an example skin (`mini/`) |
| `docs/` | Plan, design notes, the skin authoring guide, packaging and dependency policy |
| `deny.toml`, `supply-chain/` | Dependency checks: cargo-deny rules and cargo-vet audits |
| `assets/` | The app icon (the artwork and the sizes made from it by `ci/make-icons.swift`), and the logo and illustration in the README |
| `ci/` | Docker image and script that check Linux and Windows builds locally |

## History

Freya started out as *ampwin*, a Winamp 2.x clone with support for classic `.wsz` skins (milestones M0–M5, up to commit `c7de778`). That approach was dropped because Winamp skins are pixel art made for CRT screens and cannot become crisp on modern displays.
