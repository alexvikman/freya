<p align="center">
  <img src="assets/readme/logo.png" alt="Freya" width="460">
</p>

<p align="center">
  <img src="assets/readme/listener.png" alt="A listener with headphones" width="320">
</p>

A cross-platform music player written in Rust, with its own skin format. Readability and efficiency come first: the interface is drawn at the screen's physical resolution and stays crisp at any size.

Status: early, but in use. [Download the latest release](https://github.com/alexvikman/freya/releases/latest) for macOS, Windows or Linux; Freya then finds and installs its updates itself, when you say so. The interface is built with [Slint](https://slint.dev). See [docs/PLAN.md](docs/PLAN.md) for the architecture and milestones.

## Building

Requires Rust ([rustup](https://rustup.rs) installs the version in `rust-toolchain.toml` by itself) and [CMake](https://cmake.org), which builds the Opus decoder (`brew install cmake` on macOS; build without Opus with `--no-default-features` on `freya-audio`). On Linux, install the ALSA, fontconfig and xkbcommon development headers (`libasound2-dev libfontconfig1-dev libxkbcommon-dev` on Debian/Ubuntu).

```sh
cargo build --release
```

`ci/check.sh` runs the checks for Linux and cross-compiles for Windows in Docker, so other systems can be checked from a Mac: `ci/check.sh linux`, `ci/check.sh windows`, `ci/check.sh deps` (dependency checks), `ci/check.sh secrets` (credentials in any commit), `ci/check.sh workflows` (the GitHub workflows, with actionlint), or all five without an argument. `ci/check.sh gui` runs Freya on virtual X11 and Wayland screens and saves screenshots and CPU use in `target/ci-gui/`.

Installers for macOS (`.dmg`), Windows (`.msi`) and Linux (`.AppImage`, `.deb`) are made with cargo-packager; see [docs/PACKAGING.md](docs/PACKAGING.md).

Commits are checked for credentials before they are made: after cloning, turn the hook on with `git config core.hooksPath .githooks` (it needs [gitleaks](https://github.com/gitleaks/gitleaks), e.g. `brew install gitleaks`). See [docs/RELEASING.md](docs/RELEASING.md).

Dependencies are pinned, checked against the RustSec database and recorded as audited or knowingly exempted; see [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md).

## Usage

```sh
cargo run --release -- ~/Music/some-album
```

Drop files, folders or playlists (M3U, M3U8, PLS) on the window to add them. In the playlist, click, Shift-click and Cmd/Ctrl-click to select, double-click or Enter to play, drag rows to reorder, Delete to remove. Cover art is read from the files' tags or from a `cover.jpg`/`folder.jpg` next to them. The equalizer (the sliders button) has ten bands from 60 Hz to 16 kHz and a preamp, ±12 dB each: drag across several bands in one sweep, double-click a band to reset it, pick one of Winamp's presets, or load and save Winamp `.eqf` files. The visualizer shows a spectrum, an oscilloscope or the hologram. Its full-screen button, `F` or a double-click fills the screen with it, for a TV: what plays shows below it in large letters, and moving the mouse brings up the controls, the visualizations and Up Next. It runs at 30 frames per second (`visualizer_fps` in `settings.toml`), at most 15 while another app is in front, and not at all while the window is hidden; turn it off to save the rest of its CPU. The built-in skin's spectrum is drawn by Freya in its own layer on top of the window, so it costs almost nothing (set `FREYA_NATIVE_SPECTRUM=off` to have the skin draw it instead). Appearance follows the system, or can be set to light or dark in the playlist's "more" menu. The playlist, volume, shuffle, repeat, equalizer, appearance, music folders and window size are kept between runs.

The **Library** tab lists the music in the folders you choose ("Add music folder…"), by artist and album, and searches it as you type: every word you type must start a word of the artist, album, title, genre or file name, and accents are ignored. Click an artist or album to open it, double-click a track to play it with the tracks around it, or use **+** to add anything to the playlist. Freya reads new and changed files each time it starts, in the background; files on a drive that is not plugged in stay in the library. A disc ripped as one file with a CUE sheet next to it (`Album.flac` or `Album.ape`, and `Album.cue`) shows as an album of its tracks, here and when its folder or sheet is dropped on the playlist, and its tracks play one after the other without a gap.

**Internet radio:** choose "Play stream URL…" in the playlist's "more" menu and paste the address of a stream (like `https://ice1.somafm.com/groovesalad-128-mp3`) or of a station's `.pls` or `.m3u` file from a radio directory (like `https://somafm.com/groovesalad.pls`). The song title updates as the station announces each song, at the moment it starts playing. MP3, AAC, Ogg Vorbis and Ogg Opus streams over HTTP and HTTPS work; `freya play <url>` plays a stream in the terminal (on Windows, `freya-cli play <url>`).

Freya shows what plays in the system's media controls (Now Playing on macOS, the media overlay on Windows, MPRIS on Linux), with cover art, and the keyboard's media keys control it.

Keys: `space` play/pause, `←`/`→` seek 5 s, `↑`/`↓` volume, `N`/`P` next/previous, `S` stop, `O` open files, `F` full-screen visualizer (`Esc` leaves it), `F1` about, `F2` skins. In the playlist: arrows, Page Up/Down, Home/End, Enter, Delete, `Esc`, `Cmd/Ctrl+A`. In the library: arrows, Page Up/Down, Enter, `Esc` to go back, `Cmd/Ctrl+F` to search.

Options: `--skin path` loads a skin (a `.slint` file, a skin folder or a `.freyaskin` package) and remembers it; `--renderer femtovg|software` picks the renderer (the default is software on macOS, where it keeps the window steady while it is resized, and FemtoVG on the GPU elsewhere).

There is also a terminal player, without a window:

```sh
cargo run --release -- play ~/Music/some-album
```

Installed, that is `freya play ~/Music/some-album`. On Windows, where Freya is a window app the terminal does not wait for, use `freya-cli` instead: the installer puts it next to Freya and adds Freya's folder to the PATH, and it works for every command, `freya-cli skin pack` too.

Supported formats: MP3, FLAC, Monkey's Audio (APE, from version 3.95), WAV, AIFF, Ogg Vorbis, Opus, AAC and ALAC (M4A/MP4), CAF, MKV/WebM audio, and Shoutcast/Icecast radio streams, plus CUE sheets for discs ripped as one file. Playback is gapless.

## Skins

Freya's look comes from a skin: a [Slint](https://slint.dev) file, compiled when Freya starts, that connects to the player through a small API. Press `F2` (or choose "Skins…" in the playlist's "more" menu) to switch skins, install a `.freyaskin` package or open the skins folder. A skin used from a folder reloads as soon as one of its files is saved, so skins can be designed with Freya running. A skin that fails to load never keeps Freya from starting: the built-in skin takes over and the picker shows the error.

```slint
import { Player, Shortcuts } from "@freya/v1.slint";
export { Player } from "@freya/v1.slint";

export component MySkin inherits Window {
    Text { text: Player.title; }
}
```

```sh
freya skin check path/to/my-skin    # compile and list errors and warnings
freya skin pack path/to/my-skin     # make my-skin.freyaskin for sharing
```

[docs/skins.md](docs/skins.md) is the full guide for skin authors. The API is defined in [`crates/freya-skin/api/v1.slint`](crates/freya-skin/api/v1.slint); [`skins/freya/`](skins/freya/skin.slint) is the built-in skin and [`skins/mini/`](skins/mini/skin.slint) a small example.

## Layout

| Path | Contents |
|---|---|
| `freya/` | The application binary |
| `crates/freya-audio/` | Audio engine: decoding, resampling, 10-band equalizer, output, gapless playback |
| `crates/freya-library/` | Playlist model, M3U/PLS files, CUE sheets, tag reading, EQ presets and `.eqf` files, the media library (SQLite) |
| `crates/freya-skin/` | Skin API, skin folders and packages, the skin picker, headless rendering for previews and tests |
| `skins/` | The built-in skin (`freya/`) and an example skin (`mini/`) |
| `docs/` | Plan, design notes, the skin authoring guide, packaging and dependency policy |
| `deny.toml`, `supply-chain/` | Dependency checks: cargo-deny rules and cargo-vet audits |
| `assets/` | The app icon (the artwork and the sizes made from it by `ci/make-icons.swift`), and the logo and illustration in this README |
| `ci/` | Docker image and script that check Linux and Windows builds locally |

## History

Freya started out as *ampwin*, a Winamp 2.x clone with support for classic `.wsz` skins (milestones M0–M5, up to commit `c7de778`). That approach was dropped because Winamp skins are pixel art made for CRT screens and cannot become crisp on modern displays.

## License

MIT OR Apache-2.0. The graphical interface uses Slint under its royalty-free license, which requires crediting Slint in the application.
