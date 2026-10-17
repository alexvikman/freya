# Freya – architecture plan (revision 2)

A cross-platform (macOS, Windows, Linux) music player written in Rust, with its own skin format. Readability and efficiency come first: everything is drawn at the screen's physical resolution and stays crisp at any size.

## Why revision 2

Milestones M0–M5 built a Winamp 2.x clone: an audio engine, Winamp `.wsz` skins, a playlist editor and an equalizer. It worked, but Winamp skins are BMP pixel art drawn for CRT screens around the year 2000. A play button is 23×18 pixels and a letter is 5×6 pixels. On a modern HiDPI screen they can at best be enlarged in whole steps. They can never be sharper or more detailed than they were drawn, and text in them never becomes readable modern text. Readability is the most important requirement, so we pivot:

- **No Winamp skins.** The `.wsz` code is removed. It remains in git history (up to commit `c7de778`).
- **One window with a modern layout** instead of Winamp's separate docked windows. This is simpler on every platform and avoids Wayland's ban on positioning windows.
- **Our own skin format** where a skin controls both layout and style, with vector graphics and real fonts.

## Decisions

| Area | Decision |
|---|---|
| Platforms | macOS, Windows, Linux |
| UI toolkit | **Slint**: declarative UI, crisp text, HiDPI, built-in text input, scrolling and accessibility |
| Skins | `.slint` files loaded at runtime with `slint-interpreter`. A skin controls the whole layout and style |
| Skin package | A folder or `.ampskin` (zip) containing `skin.slint`, plus SVG icons and any fonts |
| Built-in skin | `freya`: our own design, embedded in the binary, loaded through the same interpreter as other skins |
| Layout | One window with panels (now playing, playlist, equalizer, later library) |
| Formats | MP3, FLAC, WAV, OGG Vorbis, Opus, AAC/M4A, ALAC, HTTP streams (unchanged) |
| Licence | Slint's royalty-free licence, which requires crediting Slint, e.g. in the About box. Freya's own code stays MIT OR Apache-2.0 |

### Why Slint

| | Slint | iced | Own renderer (tiny-skia + cosmic-text) |
|---|---|---|---|
| Skin decides the layout | Yes, `.slint` loaded at runtime | No, themes only (colors, fonts, sizes) | Yes |
| Text input, IME, scrolling, accessibility | Built in | Built in | Must be built ourselves |
| Rendering | GPU (FemtoVG/Skia) or software with partial redraw | wgpu or tiny-skia | CPU, our own optimizations |
| Licence | GPL-3 / royalty-free with attribution / commercial | MIT | Ours |

Slint is the only option where a skin can truly redefine the player without us building and maintaining a UI toolkit.

## What we keep and what goes

| Kept as is | Removed |
|---|---|
| `freya-audio`: decoding, resampling, gapless playback, EQ, output | `ampwin-skin`: `.wsz` loading, sprite tables |
| `freya-library`: playlist model, M3U/PLS, tags, EQ presets and `.eqf` | `ampwin-ui`: pixel canvas, winit/softbuffer windows, Winamp window layouts |
| Terminal player (`freya play`) | `tools/skingen`: the BMP skin generator |
| Settings and playlist persistence (adapted) | The multi-window and docking logic |

## Architecture

```
freya/
├── crates/
│   ├── freya-audio/      # unchanged (+ visualization tap in a later milestone)
│   ├── freya-library/    # unchanged (+ SQLite media library later)
│   └── freya-skin/       # NEW: skin loading (folder/.freyaskin), skin API, binding to the app
├── freya/                # binary: Slint event loop, app state, settings, CLI
└── skins/freya/          # the built-in skin: skin.slint + SVG icons
```

### Skin API

The contract between the app and skins is a set of Slint **globals** defined in `api.slint`, which every skin imports:

```slint
export global Player {
    in property <string> title;
    in property <string> artist;
    in property <duration> position;
    in property <duration> length;
    in property <PlaybackState> state;
    in-out property <float> volume;
    callback play(); callback pause(); callback stop();
    callback next(); callback previous();
    callback seek(float);           // 0.0 to 1.0
}
export global Playlist { in property <[TrackRow]> rows; callback play-row(int); /* ... */ }
export global Equalizer { /* bands, preamp, presets */ }
export global Spectrum  { in property <[float]> bars; } // later
```

- The app writes state into the globals and reacts to callbacks. A skin is pure presentation and has no access to files or the network.
- The API carries a version number. Skins declare which version they target, and the app reports a clear error for incompatible skins.
- Skins can be loaded and switched at runtime. For skin authors there is a reload-on-save mode.

### Thread model

The audio side is unchanged: decoder thread, then a lock-free ring buffer, then the audio callback. The UI runs on Slint's event loop. A Slint `Timer` at about 30 Hz moves position and status from `Player::status()` into the globals, and runs at 60 Hz only while visualization is shown. Nothing is redrawn while nothing changes.

### Readability rules for the built-in skin

- System font (SF Pro / Segoe UI / Cantarell) with a minimum size of 12 logical points for body text.
- Contrast of at least WCAG AA (4.5:1) for all text.
- Follows the system's light and dark mode, and respects the OS display scaling.
- Every control can be reached and operated from the keyboard, and screen readers work through Slint's AccessKit integration.

## Milestones

M0–M5 are done (see git history; the app was called ampwin then). New milestones:

| # | Milestone | Done when |
|---|---|---|
| **M6** | **Rename to Freya**: the app, binary, crates, threads, settings folder and documentation are renamed from ampwin to Freya. The Winamp UI code is removed at the same time, since renaming it first would be wasted work | Everything builds and tests pass under the new name; `freya play` works |
| **M7** | **Spike and foundation**: Slint window that loads the built-in skin via the interpreter; skin API v0 (transport, position, title, volume) wired to `freya-audio` | Plays music from a crisp window; idle CPU under 1 %; choice of renderer (FemtoVG/Skia/software) settled with measurements |
| **M8** | **Built-in skin, part 1**: now playing (cover art from tags), transport, seek, volume; playlist panel with a virtualized list, multi-select, drag and drop, reordering, keyboard control | Everything from the old M4 works, but crisp at any size |
| **M9** | **Equalizer panel** (presets, `.eqf`), settings, light/dark variants | Everything from the old M5 works |
| **M10** | **Visualization**: spectrum and oscilloscope from an audio tap, exposed to skins | Smooth without affecting audio; see results for the CPU cost |
| **M11** | **Skin system**: `.freyaskin` packages, skin picker, API versioning, reload on save, guide for skin authors | Someone else can build a skin from the documentation alone |
| **M12** | **Media library**: SQLite, scanning, fast search, browsing by artist/album | 10 000+ tracks, search under 50 ms |
| **M13** | **Internet radio**: HTTP streams, ICY metadata, .pls/.m3u from radio directories | A SomaFM stream plays with live title updates |
| **M14** | **OS integration, optimization and packaging**: media keys, Now Playing, Opus, visualization CPU (see risk 7), .app/.dmg, .msi, AppImage | Installable on all three platforms; visualization well below today's 9 % CPU |

### The new layout (from 2026-10-16)

The built-in skin moves to the layout of a desktop music library: navigation down the left
side, a toolbar with back and search above the content, and a player bar along the bottom. The
navigation is meant to grow over time.

| # | Milestone | Done when |
|---|---|---|
| **U1** | **Layout** with what exists: navigation (Artists, Up Next, Visualizer, Equalizer), toolbar, player bar; the visualizer and equalizer become views of their own. Skin API: `Visualizer.set-mode` and `Visualizer.showing` | Everything from before works in the new layout; the visualizer costs nothing while its view is hidden |
| **U2** | **Library views**: Songs (all tracks in a table), Albums (a grid with covers), Genres | Browsing 10 000+ tracks stays fast; covers are small cached images |
| **U3** | **Radio**: saved stations to play, add and remove | Stations survive a restart |
| **U4** | **Playlists**: several named playlists in the navigation; Up Next is the queue | Playlists are kept by Freya, not only as files |
| **U5** | **Recents**: recently added, recently played, most played | Play history is stored in the library database |
| **U6** | **macOS window**: the window buttons inside the navigation, without a separate title bar | |
| **U7** | **Advanced visualizer**: new modes drawn by the graphics card (Metal, Direct3D, OpenGL/Vulkan) through a native layer like the spectrum's | Planned with the user when U1–U6 are done |

### U7: the hologram (from 2026-10-17)

The advanced visualizer becomes a hologram in the style of Joi in Blade Runner 2049: a woman
projected in translucent pink, violet and cyan light, with scanlines, flicker, glitches and a
soft glow, over a dark city in the rain with blurred neon. She is Freya's own character, the
README's listener (blonde bob, white headphones, black top, peach choker), photorealistic,
and resembles no real person. She breathes and sways a little, and moves from pose to pose
with a hologram glitch; the bass drives the glitches and the glow. Decided with the user
(2026-10-17), after a first try in the Amiga demoscene style was not what they wanted.

Her poses are photographs the user makes (with Codex): the same woman in five poses, on pure
black, so black is see-through in the hologram. The graphics card does everything else.

It is drawn by the Metal layer of the first try: a thread of its own, in step with the display
(120 Hz measured), asleep while hidden; per frame the CPU only hands over the time, the bass
and which poses to blend. macOS first; the shaders stay simple enough to port.

| # | Milestone | Done when |
|---|---|---|
| **H1** | **The Metal layer** (done in the first try): a CAMetalLayer drawn by its own thread at the display's rate, the bass handed over | 120 frames per second |
| **H2** | **The city**: night, fog, rain and blurred neon, drawn by the shader | Looks like the film's streets |
| **H3** | **The hologram**: the poses as textures, tinted light added over the city, scanlines, flicker, colour fringes, glitch slices and glow; breathing and sway; glitch transitions between poses; the bass in the glitches | She reads as a hologram, and the user likes it |
| **H4** | **In Freya**: the mode named and documented, other systems told it is macOS only, CPU measured | Under 1 % of a core at 120 Hz for the drawing |

### Releases (from 2026-10-17, before U7)

Freya gets the release machinery of the user's earlier project Bird: GitHub Releases as the
update feed, with a signed list of checksums as the trust anchor. Decided with the user:

- **macOS signing**: ad hoc for now, as Bird. The pipeline is ready for a Developer ID
  certificate and notarization to be plugged in later.
- **Windows signing**: none for now (SmartScreen warns on first start); the pipeline has a
  place for it.
- **Home**: the GitHub repository `alexvikman/freya`; bundle identifier
  `io.github.alexvikman.freya`. Nothing is pushed until the user publishes.
- **Updates** always ask: Freya looks at launch and once a day (which can be turned off), shows
  the version and its notes, and installs only when the user says so.
- **Secrets**: every commit and every release is scanned, so no credential is pushed by
  mistake. Credentials are only referred to (environment variable names, keychain profiles),
  never written into files.

| # | Milestone | Done when |
|---|---|---|
| **R1** | **Icon**: Freya's icon in the Dock and window also when the program runs outside an app bundle; icon and version resource in `freya.exe`; the real bundle identifier; `CFBundleVersion` from the version | No generic icon anywhere |
| **R2** | **Secret scanning**: gitleaks as a pre-commit hook kept in the repository, and as a step in CI and in the release | A test secret is refused at commit |
| **R3** | **Signing**: ad hoc with the hardened runtime on macOS, scripted so a Developer ID and notarization only need credentials | The packaged app passes `codesign --verify --deep --strict` |
| **R4** | **Updates**: releases read from GitHub; `SHA256SUMS` signed with Ed25519 (SSH signature format, verified with `ring`, which Freya already has) against a public key built into Freya; skin API for "update available", its notes, Install, Later and Skip; the app swapped atomically and restarted | A local test release is found, verified, refused when tampered with, and installed |
| **R5** | **Release pipeline**: a version tag builds every platform, packages and signs them, writes and signs `SHA256SUMS` in an approval-gated environment, checks the signature against the built-in key, attests the build and creates the GitHub Release with notes from the git log; a script bumps the version | The workflow passes `actionlint`, and its steps run locally as far as they can without GitHub |

## Results from M7

Measured on an Apple Silicon MacBook (release build, built-in skin, 560×260 window):

| | FemtoVG (GPU) | Software |
|---|---|---|
| CPU idle | 0.3 % | 0.5 % |
| CPU while playing | 1.5 % | 4.1 % |
| Skin compile time | ~90 ms | ~100 ms |
| Time to first frame | ~320 ms | — |
| Memory (physical footprint) | 58 MB | — |

- The audio engine alone uses 0.1 % CPU; the rest is redrawing. Reporting the position in 250 ms steps instead of every 33 ms cut playback CPU from 5 % to 1.5 % with FemtoVG.
- FemtoVG is the default renderer; Slint picks a fallback if it cannot start. Skia was not tried (large C++ dependency); revisit if FemtoVG causes trouble.
- Time to first frame is slightly above the 300 ms goal. The skin compiles in about 90 ms; most of the rest is creating the window, the GL context and the audio output, which could start in parallel.
- Skins must re-export the API globals (`export { Player } from "@freya/v1.slint";`) because Slint only exposes globals exported from the main document. The loader reports a clear error otherwise.
- Screenshot tests render with Slint's software renderer and the system font, so they run on macOS only.

## Results from M8

- One window: now playing (cover art, title, artist, format, seek, transport, shuffle, repeat, volume) above a playlist with a virtualized list, multi-select, drag-to-reorder, keyboard control and an empty state. Checked in light and dark mode in headless renders.
- Files dropped from the file manager are taken from winit through Slint's `unstable-winit-030` hook, since Slint itself does not deliver them. That hook is tied to winit 0.30 and may change with Slint updates.
- Freya's own About window (`AboutFreya` in the skin API, with Slint's `AboutSlint`) fulfils the attribution requirement regardless of skin. It opens from the skin (F1 or the info button).
- CPU while playing rose to 2.2 % with the larger window (560×680), because FemtoVG redraws the whole window. Position is now reported in 500 ms steps. A renderer with partial redraws could bring this down; not pursued yet.
- Time to first frame: about 315 ms; the larger skin compiles in about 140 ms.

## Results from M9

- Equalizer panel in the built-in skin: on/off switch, preamp and ten band sliders (sweep across bands, double-click to reset, screen-reader increment/decrement), the response curve, Winamp's presets and `.eqf` files.
- The curve is a pure callback, `Equalizer.curve(width, height, bands)`: Slint's `Path` keeps the viewbox's aspect ratio, so a fixed 100 × 100 path could not stretch to the panel. The skin passes its own size and gets path data that fits exactly.
- Appearance can follow the system or be forced light or dark (`App.theme`); saved with the equalizer settings.
- A headless integration test drives the built-in skin's callbacks against the app (`Player::silent` plays into nothing at real-time speed), so the wiring is tested without a window or audio device.
- CPU while playing: 1.9 %.

## Results from M10

- The audio callback copies played samples (after the equalizer, before volume, mixed to mono) into a lock-free ring buffer, only while visualization is wanted. A 2048-point FFT with a Hann window gives log-spaced bars from 40 Hz to 16 kHz in dBFS, with instant rise, smooth fall and peak markers; the oscilloscope shows the last 10 ms.
- `Visualizer` global in the skin API: the skin chooses its bar count; clicking cycles spectrum, oscilloscope, off. Frames run on their own timer, only while something plays (and until the bars have fallen), so a paused or stopped player does not redraw.
- **Cost:** every frame redraws the whole window. Measured on an Apple Silicon MacBook while playing: visualization off 1.9–2.2 % CPU, spectrum at 30 fps 8.6–10 %, at 60 fps 11–15 %. Skia (14 %) and the software renderer (38 %) were no better than FemtoVG, and Slint's `cache-rendering-hint` on the static parts did not help. A profile shows about 60 % of render time in walking and drawing the item tree and 40 % in presenting the GL surface. The default is 30 fps (`visualizer_fps`); turning visualization off brings CPU back to about 2 %.
- During the measurements CoreAudio twice reported buffer under/overruns at the moment the process was killed under load; they could not be reproduced in four further runs and did not occur during playback.

## Results from M11

- Skins come as a single `.slint` file, a folder (`skin.slint`, an optional `skin.toml` manifest, images, fonts, imported files) or a `.freyaskin` package (a zip of a folder). Packages are unpacked to a cache folder keyed by path, size and modification time; unpacking rejects paths outside the folder and limits file count and size. A manifest asking for a newer API version is refused with a clear message.
- The skin picker (`F2`, `App.show-skins()`) is Freya's own window, like About, so it works with any skin. It lists the built-in skin and everything in the skins folder, installs packages and opens the folder. Switching skins rebuilds the window in place: playback, playlist, position and cover art are kept.
- A skin used from a file or folder is checked for changes once a second and reloaded on save; compile errors keep the current skin and show in the picker. A saved skin that no longer loads falls back to the built-in skin at startup.
- `freya skin check` and `freya skin pack` support authors; [skins.md](skins.md) documents the API. The Mini skin shows that a skin can use only `Player`.

## Results from M12

- The library is an SQLite database (`library.sqlite3` in Freya's data folder, bundled SQLite) holding each track's path, size, modification time and tags, plus an FTS5 table of words for searching. It only holds what can be read back from the files, so a database from an older layout is simply rebuilt by the next scan.
- Scans run on a background thread: unchanged files (same size and modification time) are skipped, new and changed files have their tags read by up to eight threads, and changes are committed every half second so the library fills up visibly. Files under a folder that is missing (an unplugged drive) are kept. Freya scans at startup and when folders change; watching the folders while running is left for later.
- Search: every typed word must start a word of the artist, album artist, album, title, genre or file name (FTS5 prefix queries, diacritics ignored), so "beyonce" finds "Beyoncé". Typed text is quoted, so query syntax cannot cause errors.
- `Library` global in the skin API (an addition to v1): browsing artists → albums → tracks, search, play or enqueue any row, folders and scan status. The built-in skin has Playlist and Library tabs.
- **Measured** on an Apple Silicon MacBook, release build, 10,000 files on the internal SSD: first scan 0.55 s, a scan with nothing changed 63 ms, search 2 ms, list of artists 0.5 ms. In the test suite, with 12,000 tracks: every query under 3 ms against the goal of 50 ms.

## Results from M13

- `Player::play` takes `http://` and `https://` URLs. A network thread (ureq, rustls) reads the stream into a buffer of up to 1 MiB; playback starts once two seconds of audio are buffered, and buffers again if the network falls behind. The decoder never waits on the network directly: playing something else interrupts any wait at once, and a station that sends nothing for 15 seconds is given up on.
- ICY metadata (`icy-metaint`) is taken out of the audio and each `StreamTitle` is kept in place in the buffer, so a new title shows when its song is heard (within a quarter of a second), not when it arrives. The timeline gets a new segment for the same track, so position carries on. Chained Ogg streams, as Ogg stations send, get a new decoder and their new Vorbis comments at each song.
- Station playlists (`.pls`, `.m3u`) are downloaded in the background; their entries keep the station's title whole. HLS playlists are refused with a clear message.
- Skin API additions: `Player.live`, `Player.error` (any track that fails to play), `Playlist.add-url`. The built-in skin has "Play stream URL…", shows "Live" instead of the remaining time, and shows errors in red.
- Tested against a local station in the test suite (titles, interruption of a silent station, station playlists through the skin), and by hand against SomaFM's Groove Salad in MP3 and AAC: playback starts after 0.8 s and titles change with the songs.

## Results from M14

### Visualization CPU

Measured on an Apple Silicon MacBook, release build, built-in skin at 560×680, a test signal that changes the spectrum every frame (CPU time over ten seconds, volume 0):

| | Before | After |
|---|---|---|
| Visualization off | 2.5 % | 2.4 % |
| Spectrum, window in front (30 fps) | 13.4 % | 11.1–11.4 % |
| Spectrum, another app in front (15 fps) | 13.4 % | 7.9 % |
| Spectrum, window hidden or minimized | 13.4 % | as off: frames stop |

- The spectrum is drawn as two `Path` elements whose data Freya computes (`Visualizer.bars-path`, `peaks-path`), instead of 48 rectangles with interpreted bindings: about 10 % less CPU while drawing.
- Frames stop while the window is hidden (winit's `Occluded`) and drop to 15 fps while it does not have focus.
- **Native spectrum (after M14):** on macOS the spectrum is now drawn by Freya in a Core Animation layer on top of the window: one sublayer per bar and peak marker, moved each frame, while Slint only redraws for the position (2 frames a second). The skin places a `NativeSpectrum` component, which reports where it is and draws with Slint where Freya cannot. With the window in front: **3.4–3.5 % CPU, against 11.1–11.4 % before and 3.0 % with visualization off.** Checked in screenshots of Freya's window: light and dark mode, a wider window, and the oscilloscope (layer hidden).
- **Windows:** the same with Windows.UI.Composition: a `DesktopWindowTarget` on top of the window (topmost) holds one solid-color `SpriteVisual` per bar and peak marker, placed in physical pixels with the window's current scale. Cross-compiled and linted in Docker, but **not yet run on Windows**: if composition cannot be set up, Freya logs why and the skin draws the spectrum itself.
- **Linux:** on Wayland, a subsurface of the player's surface (sharing winit's connection), painted into two shared-memory buffers and taking no input; on X11, a child window with an empty input shape, drawn through a pixmap and filled with the skin's `background-color`. Run in Docker by `ci/check.sh gui` on virtual screens (Xvfb with OpenGL in software; headless Sway with Slint's software renderer), with the layer off and on: X11 48.8 % → 8.2 % CPU, Wayland 7.6 % → 2.6 %; screenshots show the bars in place. Not yet run on a real Linux desktop. `FREYA_NATIVE_SPECTRUM=off` turns the layer off.
- Found along the way: Freya refused to start without a sound device (it now plays silently in real time and says so); souvlaki's MPRIS thread panicked without a D-Bus session (the controls are now skipped then); the `.deb` lacked the X11 and Wayland libraries winit loads; Weston 9's headless backend crashes Slint's software renderer even without the layer, so the test uses Sway.
- What remains without the native layer is a fixed cost of redrawing a window with FemtoVG on macOS: an almost empty skin costs 5.5 percentage points at 30 fps, independent of window size; the built-in skin adds about 4. The profile shows little work in Freya or Slint, so most of it is OpenGL (emulated on Metal) presenting frames. Tried and rejected: the WGPU version of FemtoVG (in Slint 1.18 it never draws a frame on macOS), Skia and the software renderer (M10). A renderer with partial redraws would be the real fix.

### Opus

- Opus files and Ogg Opus streams decode with libopus, registered alongside Symphonia's own decoders. A one-second fixture (made with libopus, with Vorbis comments) plays to within one 20 ms frame of its exact length, with tags.

### System media controls

- Freya connects to Now Playing on macOS, the media overlay on Windows and MPRIS on Linux: title, artist, album or station, length, cover art (written as a PNG to Freya's cache folder) and play state. The system moves the position along by itself; Freya only reports it when playback changes or the position jumps. Media keys and the system's controls play, pause, skip, seek and, through MPRIS, set the volume.
- First built on `souvlaki`; replaced after M14 by Freya's own thin integration (`gui/system_media/`), which cut about 60 crates, among them an old zbus 3, `windows 0.44` and the unmaintained `derivative` and `block 0.1.6` (risk 9). One small `Controls` per system sits behind the same interface: the MediaPlayer framework through `objc2-media-player` on macOS, SystemMediaTransportControls through the `windows` crate Freya already used, and MPRIS over the zbus 5 that Slint already brings. MPRIS now also reports Freya's real volume.
- Updating is cheap: a track that plays sends three updates in total, and CPU is unchanged. Media keys cannot be pressed from automated tests. On macOS the keys, the Now Playing widget and seeking in it were tried by hand (2026-10-16); on Linux the MPRIS interface was driven with playerctl in Docker (status, metadata, position, pause, seek, volume, next, previous, stop, quit, and the change signals); the Windows overlay is built and linted but not yet tried on a Windows machine.

### Packaging

- cargo-packager makes `Freya.app` and a `.dmg` on macOS (15 MB), an `.msi` on Windows and an AppImage and `.deb` on Linux, with the app icon (rendered to PNGs and an `.ico` from `assets/icon-art.png` by `ci/make-icons.swift`). See [PACKAGING.md](PACKAGING.md).
- Built and started from the bundle on macOS. Linux and Windows are checked locally in Docker with `ci/check.sh` (no GitHub until release): on Linux all tests pass and the `.deb` and AppImage install and run on a clean Ubuntu 22.04; for Windows, a MinGW cross-build passes clippy and links. The MSVC build, the `.msi` and x86-64 Linux packages still need a run on those systems.
- Found by the local checks: the `.deb` declared no dependencies (it now needs ALSA, fontconfig, xkbcommon, EGL and GL), the Windows-only console code broke the `unsafe` lint, and libopus needs `libssp` when cross-built with MinGW.
- Found while testing the bundle: quitting from the system (Cmd+Q, the Dock) ended the process without returning from the event loop, so settings and the playlist were not saved. Freya now also saves in winit's `exiting` hook.
- Windows release builds are window apps (no console window); started from a terminal, they attach to it so `freya play` and `freya skin` still print.
- Not done: signing and notarization (they need the owner's certificates), and the bundle identifier `app.freya.player` is a placeholder.

## Results after M14: CPU and memory

Measured on an Apple Silicon MacBook, release build, 560×680 window in front, volume 0:

| | Before | After |
|---|---|---|
| Idle (nothing plays) | ~1.1 % | 0.20 % |
| Playing, no visualization | 2.5 % | 2.2 % |
| Playing, spectrum | 3.0 % | 2.5 % |
| Binary size | 39.4 MB | 29.2 MB |

- The audio device is stopped after five seconds with nothing playing, so it and the system's audio service can rest; playing, seeking or stopping starts it again (in about 20 ms) before the engine asks it to drop its buffer.
- The app's timer runs 30 times a second only while something plays or the library is read, otherwise 5 times.
- The position is shown in whole seconds, so the window redraws once a second while playing instead of twice.
- Release builds use whole-program optimization (`lto = "fat"`, one codegen unit): 26 % smaller, at about three minutes per release build.
- **The built-in skin is compiled into Freya** (`freya/build.rs`, `slint-build`) instead of read by the interpreter. Slint's interpreter keeps the compiler's data for its own tools, about 30 MB for this skin. The app still talks to every skin by name: `SkinWindow` is either the compiled skin or an interpreted one, and `builtin_skin.rs` maps the skin API, listed once in a table, to the generated Rust methods; a test checks the table against the API. Other skins, and the built-in one started with `--skin skins/freya`, are interpreted as before.

| | Before | After |
|---|---|---|
| Memory (footprint) | 101 MB | 63 MB |
| of which heap | 48 MB | 16 MB |
| CPU playing, spectrum | 2.5 % | 2.1 % |
| CPU playing, no visualization | 2.2 % | 1.6 % |
| CPU idle | 0.20 % | 0.10 % |
| First frame | ~360 ms | 319 ms |

- What remains of memory: about 23 MB of window buffers (GPU; they grow with the window), 16 MB of heap, and code and mapped files.

## Results: CUE sheets

A disc ripped as one audio file with a CUE sheet next to it (`Album.flac` + `Album.cue`) is
listed and played as its tracks, in the library and in the playlist.

- **Reading sheets** (`freya-library/src/cue/`): a small parser of our own, no new crate. It
  reads FILE, TRACK, INDEX 01, TITLE, PERFORMER and REM GENRE, DATE and DISCNUMBER, and skips
  the rest. Text is UTF-8 (with or without BOM), UTF-16 with BOM, or else Windows-1251 or
  Windows-1252: Cyrillic words are runs of bytes from 0xC0 up, while Western European text has
  its accented letters one at a time. The gap before a track (INDEX 00) is played at the end
  of the track before, as most players do. Sheets often name the file wrongly (a `.wav` that
  became a `.flac`); the file is then found by its stem, by the sheet's own name, or as the
  folder's only audio file. "Artist - Title" given as both title and performer is split. A
  sheet without a disc number takes one from its name (`[CD2]`), unless the album title has it.
- **Which file is what:** a file that a sheet cuts into tracks is replaced by them. A FILE
  listed as one track from the start is left as it is, with its own tags (some sheets list a
  folder of files that are tracks already). When two sheets cut the same file, the one named
  after it wins.
- **Library:** a row says what it was read from (`source`: the audio file, or the sheet) and,
  for a track of a sheet, which part of the file it is (`start_ms`, `end_ms`). A sheet's stamp
  covers the sheet and its audio files, so editing either reads the disc again. Tracks take the
  sheet's tags, and the album, year and genre of the file where the sheet says nothing.
  Layout version 2.
- **Playing:** `Player::play` takes a `Media`: a path, optionally with where to start and stop
  and tags to show instead of the file's. The source seeks to the start and stops at the exact
  end frame, keeping what it decoded past it. When the next track is the part of the same file
  that follows, decoding simply goes on, so the tracks of a disc play without a gap and without
  reopening the file. Position, length and seeking count from the track's start.
- **Playlist:** entries carry their part. M3U files keep it in a `#FREYA-PART:start,end` line
  before the path, which other players ignore (they play the whole file). PLS cannot say it,
  so a track of a sheet is saved there as its whole file.
- **Tested:** the parser, encodings, finding files and sheets with generated files; a scan of a
  generated WAV with a sheet (tracks, editing the sheet, removing it); the engine with
  generated WAV and the FLAC fixture: exact lengths, gapless parts at the output's rate and
  resampled, parts across FLAC packets, seeking within a part. On the user's collection (read
  only, Freya in an isolated home folder, volume 0): 22 discs became 435 tracks with their
  real artists, and a sheet cutting a FLAC disc into 4-second tracks played them in turn with
  the right titles in Now Playing. The scan of the folder took 4.2 s, most of it walking a
  slow external disk.
- **Not done:** a CUE sheet embedded in a FLAC file's tags is not read. (Monkey's Audio, left
  out here at first, came next; see below.)

## Results: Monkey's Audio

Many CUE rips are `Album.ape`, which Symphonia cannot decode: 12 discs in the collection above.

- **Choice:** a decoder of Freya's own (`freya-audio/src/ape/`, about 1270 lines plus 230 of
  tests) over a crate. `ape-decoder` 0.3.2 decoded 9 of the 12 discs exactly but failed every
  frame of the 3 made with version 3.97 (it only has the entropy coder of 3.99 on, yet accepts
  older files), and comes from one publisher who released it in a day and not since.
  `oxideav-ape` 0.0.4 decoded them all but wants the whole file in memory (300-400 MB a disc).
  Freya's own needs no new dependency, so nothing to audit.
- **What it decodes:** files from version 3.95 (2002) on, both header layouts, all five
  compression levels, mono and stereo, 8, 16 and 24 bits. Older files, more channels, 32-bit
  and floating-point samples are refused with a message. It plugs into Symphonia as a format
  reader and a decoder, so tags (APEv2, through Symphonia's probe), seeking and the gapless
  parts of CUE sheets work as for FLAC; the library reads tags and length through lofty.
- **Frames:** a frame holds up to 294912 samples per channel, almost seven seconds. The reader
  hands each out in packets of 4096, which the decoder works through one at a time, so the
  work per packet stays small even in a debug build (about 6x real time there, against a
  quarter-second buffer). Each frame's CRC is checked at its end; a damaged frame is reported
  once and the rest of it is silence, so the position stays right. Seeking starts at the frame
  that holds the position and decodes up to it.
- **Tested:** 13 small fixtures, made once with `oxideav-ape`'s encoder from the WAV fixtures
  and from a synthetic signal, and checked with both crates (see
  `tests/fixtures/ape/README.md`): exact samples at every level and bit depth, silent and
  identical-channel frames, seeking, a damaged frame, tags, and parts of a file. On the
  collection (read only): all 12 discs, 3.97 and 3.99, Fast to Extra High, decode with every
  frame's CRC right and the exact length, at 130-150x real time including reading from the
  external disk (`decodes_real_files`, ignored by default); two of them played through the
  engine as 22 consecutive parts each came out sample-exact, and a part on its own as well
  (`plays_real_files_in_parts_without_a_gap`). In the app, in an isolated home folder at
  volume 0: the library scan listed all 12 discs as albums of their tracks, and a disc's sheet
  given on the command line played without errors.
- **Not done:** no file older than 3.97 or at level Insane was at hand from a real encoder;
  Insane is covered by the fixture only.

## Results: the hologram (U7)

Measured on an M-series MacBook, in a 1400 × 900 point box on its Retina screen (2800 × 1800
pixels), with `cargo test -p freya frame_cost -- --ignored --nocapture`:

| | CPU a frame | Graphics card a frame | At 120 frames a second |
|---|---|---|---|
| Everything at full resolution | 38 µs | 16 ms | 0.45 % of a core; 192 % of the graphics card, so not possible |
| The city at a quarter of the resolution | 34 µs | 3.0 ms | |
| Reading only the pose shown | 35 µs | 2.8 ms | |
| The city drawn 30 times a second | 27 µs | 1.8 ms | 0.3 % of a core, 22 % of the graphics card |

The city behind her was the work; she costs about 0.3 ms a frame. In Freya the drawing thread
logs its own CPU use with `RUST_LOG=freya::gui::hologram=debug`; it ran at the display's
120 Hz, and now at 60 (see below). It used to take several seconds to appear: scaling the poses on the CPU took four.
Uploaded at their own size, the five poses take 50 ms; with Metal and the shaders, all is
ready about 55 ms after Freya starts (240 ms when the shaders are not yet cached), on its own
thread and before the view is shown.

## Results: the full-screen visualizer and CPU, round two

The visualizer can fill the screen for a TV (`F`, the full-screen buttons, or a double-click):
what plays below it in large letters, and the controls, the visualizations and Up Next while
the mouse moves. Measured with it, on an M-series MacBook at full screen, playing a FLAC file,
release build, CPU as a share of one core:

| | Before | After |
|---|---|---|
| Queue view, no visualization | about 5 % | 1.1 % |
| Nothing playing | | 0.2 % |
| Full screen, spectrum | 6.7 % | about 3 % |
| Full screen, oscilloscope | 65 % | 2.9 % |
| Full screen, hologram | 17 % CPU, 17 % of the graphics card | 9.4 % CPU, 10 % of the graphics card |

The queue view and idle are measured with the built-in skin as compiled into Freya; the
full-screen rows with the skin read from its folder, which costs a little more.

- **The oscilloscope** was drawn by Slint, which on macOS draws on the CPU: its whole box, 30
  times a second. It now has a layer of its own like the spectrum (`NativeOscilloscope`, Core
  Animation on macOS), and the graphics card draws the line.
- **The hologram** shows each frame at least 1/60 s instead of at the display's 120 Hz. What it
  costs now is mostly the system handing frames to the screen (Metal and Core Animation
  threads, half of it in the kernel), about 0.1 ms of CPU a frame.
- **The audio engine** woke every 10 ms while its buffer was full, to top it up a little, and
  its channel spun a while before sleeping each time: half of Freya's CPU while playing. It
  now sleeps until the buffer is half empty, at most 50 ms; commands still wake it at once.
- **Redrawing the window** cost about 26 ms for any change, as softbuffer handed every frame to
  Core Animation as a new image to copy and color-convert on the CPU. Freya now builds with a
  patched softbuffer (see `vendor/README.md`) that keeps the pixels between frames, so Slint
  redraws only what changed, and shows them through IOSurfaces.
- **The time** shown while playing changes every second. Inside the player bar's layout, each
  change laid out the whole window again, and Slint redrew both lists' scroll bars, about 180,000
  pixels, though nothing moved: Slint marks everything that depends on a property as changed
  when it is set, without comparing values. The times are now drawn over fixed slots, outside the
  layout; a second now redraws a few thousand pixels.
- **Waking less:** the app's tick ran 30 times a second while playing, though the position
  shown moves once a second; it now runs five times a second, playing or not, and media keys
  wake it at once. The audio callback asks for 1024 frames (21 ms) where the device allows,
  rather than the usual 512, halving how often the audio thread wakes.
- **The spectrum** on macOS was one Core Animation layer per bar and per peak marker, moved
  every frame; it is now two shape layers, one path each. Freya spends a little more (0.5
  points) building the paths, the window server about 2 points less, and the graphics card
  draws the shapes (about 1.5 points more of it).
- **Tried and dropped:** pacing the hologram by sleeping and presenting plainly, instead of
  `presentDrawable:afterMinimumDuration:`, cost the same.
- **Assembler** was considered, and would not help: decoding FLAC takes about 0.03 % of a core,
  the audio output about 0.05 %, and the resampler's FFTs, which use rustfft's hand-written
  NEON (SIMD) code already, about 0.2 %. What
  cost was how often things woke and how much was copied, not how fast code ran; the
  hologram's shader runs on the graphics card, whose instruction set Apple does not publish.

## Risks and open questions

1. **Slint's licence:** the royalty-free licence requires visible attribution. Accepted on 2026-10-10; fulfilled in M8 by Freya's About window, which credits Slint independent of the skin.
2. **Interpreter performance:** loading `.slint` at runtime adds startup time. To be measured in M7; the goal is under 300 ms to the first frame.
3. **API stability:** the skin API must be versioned from day one, otherwise every change breaks existing skins.
4. **macOS and OpenGL:** FemtoVG uses OpenGL, which is deprecated on macOS. The alternatives are Skia (large C++ dependency) or Slint's software renderer. Decided in M7 with measurements.
5. **Opus** pulls in libopus (C, built with CMake) through `symphonia-adapter-libopus`, behind the default `opus` feature of `freya-audio`. Chosen in M14 over the young pure-Rust decoders; revisit if Symphonia gains Opus.
6. **Name clash:** an unrelated Rust GUI library is also called Freya (`freya` on crates.io). Our crates are never published (`publish = false`), so this only matters if we ever publish.
7. **Visualization CPU:** solved with the native spectrum layer: measured on macOS (3.5 % in front, as low as visualization off) and on virtual Linux screens; Windows is built but has not run on a Windows machine yet.
8. **Radio limitations:** servers that answer with Shoutcast 1's `ICY 200 OK` status line instead of `HTTP/1.x` are not understood by the HTTP library; a dropped connection is not reconnected (the next playlist entry plays instead, which for a station playlist is the next mirror); HLS streams are not supported. To revisit if they turn out to matter.
9. **Old macOS dependency:** `souvlaki` (system media controls) pulled in `block 0.1.6`, which Rust warns will be rejected by a future compiler version. Solved: souvlaki was replaced by Freya's own integration on `objc2`, and `block 0.1.6` is gone.
10. **Monkey's Audio (APE):** many older CUE rips are `.ape`, which Symphonia cannot decode. Solved with a decoder of Freya's own (see "Results: Monkey's Audio"). Files before version 3.95 (2002) remain unsupported.

## Test strategy

- Unit tests for audio, playlist, formats and presets (the existing 33 tests in `freya-audio` and `freya-library` are kept).
- Every skin in the repository is loaded in tests and must compile without errors against the current skin API.
- Screenshot tests: the built-in skin is rendered with Slint's software renderer and compared against reference images.
- The audio engine is still tested with a null output.
