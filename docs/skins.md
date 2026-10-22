# Making skins for Freya

A Freya skin is a [Slint](https://slint.dev) file that draws the player. Freya compiles it when it starts or when you switch skins, and connects it to the music through a small API. A skin decides everything you see: layout, colors, fonts, icons and animations. It cannot touch files or the network; it only shows what Freya tells it and asks Freya to do things.

This guide covers everything needed to build, test and share a skin. For Slint itself, see the [Slint language documentation](https://slint.dev/docs).

## A first skin

Create a folder with a file called `skin.slint`:

```slint
import { Player, PlaybackState, Shortcuts } from "@freya/v1.slint";

// Every skin re-exports the API globals it uses, so Freya can reach them: App too, for the
// shortcuts' F1 (about) and F2 (skins).
export { Player, App } from "@freya/v1.slint";

export component MySkin inherits Window {
    title: "Freya";
    preferred-width: 400px;
    preferred-height: 120px;
    forward-focus: keys;

    keys := Shortcuts {
        VerticalLayout {
            padding: 16px;
            spacing: 8px;

            Text {
                text: Player.title == "" ? "Nothing playing" : Player.title;
                font-size: 18px;
            }
            Text {
                text: Player.position-text + " / " + Player.length-text;
            }
            TouchArea {
                height: 32px;
                clicked => { Player.toggle-pause(); }

                Text {
                    text: Player.state == PlaybackState.playing ? "Pause" : "Play";
                }
            }
        }
    }
}
```

Try it:

```sh
freya --skin path/to/my-skin
```

Keep Freya running and edit `skin.slint`: Freya reloads the skin about a second after you save. If the skin does not compile, the current one stays and the error appears in the skin picker (press `F2`).

The [Mini skin](../skins/mini/skin.slint) is a complete, small example. The [built-in skin](../skins/freya/skin.slint) uses every part of the API. Freya ships the built-in skin compiled into the program, which starts faster and uses less memory; to work on it with reloading on save, run `freya --skin skins/freya`, which reads it like any other skin.

## Files

A skin can be:

| Form | What it is |
|---|---|
| A folder | `skin.slint` (required), `skin.toml` (optional), plus any images, fonts and other `.slint` files it imports |
| A `.freyaskin` file | A zip of such a folder, for sharing. Made with `freya skin pack` |
| A single `.slint` file | Handy for quick experiments |

Paths in the skin (`@image-url("cover.png")`, `import "parts.slint"`) are relative to the file that uses them.

### `skin.toml`

```toml
name = "Midnight"
author = "Your Name"
version = "1.0"
description = "A dark, compact skin."
api = 1
```

All fields are optional. `name` defaults to the folder or file name. `api` is the skin API version the skin is written for; Freya refuses skins that need a newer API than it has, with a clear message, instead of failing in confusing ways.

## Installing and sharing

```sh
freya skin check my-skin            # compile and list errors and warnings
freya skin pack my-skin             # writes my-skin.freyaskin, after checking it
```

To install a skin, open the skin picker (`F2`, or "Skins…" in the built-in skin's menu) and choose **Install skin…**, or copy it into the skins folder shown at the bottom of the picker:

| System | Skins folder |
|---|---|
| macOS | `~/Library/Application Support/Freya/skins` |
| Windows | `%APPDATA%\Freya\data\skins` |
| Linux | `~/.local/share/freya/skins` |

The picker is part of Freya, not of the skin, so it is always available, even from a skin without any menu.

To preview a skin as an image, without opening a window, from a checkout of Freya's source:

```sh
cargo run -p freya-skin --example render_skin -- preview.png path/to/my-skin 2 560 860
```

The arguments after the skin are scale, width and height. The preview is filled with sample data.

## The skin API, version 1

Import from `@freya/v1.slint`. The API is made of **globals**: Freya writes state into their `in` properties and reacts to their callbacks. To change something, a skin calls a callback; Freya then updates the property. For example, a volume slider shows `Player.volume` and calls `Player.set-volume(v)` while dragged.

Only `Player` is required. `Playlist`, `Playlists`, `Library`, `Radio`, `Equalizer`, `Visualizer`, `App` and `Updates` are optional: export the ones you use. Freya does no work for parts a skin leaves out: a skin without `Visualizer` costs no CPU for visualization, without `Library` Freya never reads the music folders, and without `Updates` it never looks for updates.

### `Player`

| Property | Type | |
|---|---|---|
| `title` | string | Title of the current track; empty when nothing is loaded, so the skin can show its own placeholder. For radio, the song the station announces, or the station's name |
| `artist`, `album` | string | Empty when unknown |
| `format-info` | string | For example "FLAC · 912 kbps · 44.1 kHz · Stereo"; for radio the station comes first. Empty when stopped |
| `live` | bool | True for internet radio: no length, and seeking does nothing |
| `error` | string | Why the last track could not be played, until another one starts; empty otherwise |
| `state` | `PlaybackState` | `stopped`, `playing` or `paused` |
| `position`, `length` | float | Seconds; `length` is 0 when unknown. Position updates twice a second |
| `position-text`, `length-text`, `remaining-text` | string | "1:23", "3:45", "-2:22" |
| `volume` | float | 0 to 1 |
| `cover`, `has-cover` | image, bool | Cover art of the current track, if any |

| Callback | |
|---|---|
| `play()`, `pause()`, `toggle-pause()`, `stop()`, `next()`, `previous()` | |
| `seek(fraction)` | 0 to 1 of the track |
| `seek-by(seconds)` | Relative; may be negative |
| `set-volume(value)` | 0 to 1 |
| `open-files()` | Asks the user for files to play |

### `Playlist`

| Property | Type | |
|---|---|---|
| `rows` | `[TrackRow]` | `number` (1-based), `title`, `duration` ("3:45" or empty), `selected`, `current` |
| `summary` | string | "12 tracks · 47:13"; ends with "+" if some lengths are unknown |
| `cursor` | int | Row the keyboard acts on, or -1 |
| `shuffle`, `repeat` | bool | |
| `drop-hover` | bool | True while files are dragged over the window |

| Callback | |
|---|---|
| `activate(row)` | Play the row |
| `select(row, extend, toggle)` | A click: `extend` for Shift (range), `toggle` for Ctrl/Cmd |
| `move-cursor(delta, extend)` | Arrow keys; `extend` grows the selection |
| `select-all()`, `clear-selection()`, `remove-selected()`, `clear()` | |
| `move-selected(row)` | Move the selected rows to before `row`; the row count means the end |
| `add-files()`, `add-folder()`, `save()`, `load()` | Each opens a file dialog |
| `add-url(text)` | Add an internet radio stream, or a station playlist (`.pls`, `.m3u`) from a radio directory, and play it |
| `toggle-shuffle()`, `toggle-repeat()` | |

Use `ListView` (from `std-widgets.slint`) for the rows: it only creates the rows that are visible, so long playlists stay fast.

### `Library`

The user's music, read from the folders they chose, browsed in four sections (artists, albums, songs and genres) or searched. Three more sections, the Recents, show the albums added last and the tracks played last and most. A track counts as played once it has been listened to for half its length or four minutes; skipping ahead does not count.

| Property | Type | |
|---|---|---|
| `rows` | `[LibraryRow]` | `kind` ("artist", "album", "genre" or "track"), `title`, `detail` ("3 albums · 41 tracks", "1997 · 12 tracks", or a track's artist and album), `artist` (of an album or track), `album` (of a track), `cover` (for albums: what to pass to `cover()`), `number` (track number inside an album, else empty), `duration`, `played` (in Recently Played when, "3 days ago"; in Most Played how often, "12 plays"; else empty) |
| `section` | string | "artists", "albums", "songs", "genres", "recently-added", "recently-played" or "most-played": where browsing started |
| `layout` | string | How the rows are best shown: "list" (artists, an album's tracks), "grid" (albums and genres) or "table" (tracks from many albums, with artist and album columns, and `played` in Recently Played and Most Played; also a search's songs), or "search" (a search's page of every kind of match) |
| `title`, `subtitle` | string | Heading for the rows, like "Artists" or an album with "Björk · 1995 · 11 tracks · 46:12" |
| `can-go-back` | bool | True below a section's own list, and while searching |
| `covers-ready` | int | Goes up when new covers are ready; pass it to `cover()` |
| `query` | string | `in-out`: the search text. Bind a text field to it with `<=>` |
| `folders` | `[string]` | The music folders, like "~/Music" |
| `scanning` | bool | True while Freya reads the folders |
| `status` | string | "10,412 tracks · 682 albums", or "Reading 1,234 of 5,000 files" during a scan |

| Callback | |
|---|---|
| `search(text)` | Shows what matches `text`: with `search-filter` "all", the best match of all (`search-top`, a row), the songs (`search-songs`) and the artists, albums, playlists and genres (`search-sections`, each a range of rows); else all of one kind. Names match as wholes first, then at their start, then word by word; songs by the words of their artists, albums, titles, genres and file names. Accents and case are ignored. Empty text goes back to browsing |
| `set-search-filter(kind)` | "all", "songs", "artists", "albums", "playlists" or "genres" |
| `suggest(text)`, `open-suggestion(index)` | A few of the best matches of `text` in `suggestions`, for under a search field, without leaving what is shown; opening one shows an artist, album or genre, plays a song or shows a playlist |
| `show(section)` | Start browsing a section: "artists", "albums", "songs", "genres", or one of the Recents, "recently-added", "recently-played" or "most-played" (200 albums or tracks each) |
| `cover(key, covers-ready) -> image` | Pure: a small album cover (about 300 pixels) for a row's `cover`; empty while it is being made and for albums without one |
| `activate(row)` | Open an artist, album or genre; on a track, the playlist becomes the tracks shown, playing from this one |
| `play(row)`, `enqueue(row)` | Play, or add to the end of the playlist, everything a row stands for: all of an artist's, album's or genre's tracks |
| `play-all()`, `enqueue-all()` | The same for every row shown |
| `back()` | Back the way the user came (an album opened from a genre goes back to the genre), or out of a search |
| `add-folder()`, `remove-folder(index)`, `rescan()` | Freya asks for the folder itself |

A search field:

```slint
TextInput {
    text <=> Library.query;
    single-line: true;
    edited => { Library.search(self.text); }
}
```

Search results stop at 1,000 tracks. Use `ListView` for the rows, as for the playlist.

Album covers are made in the background the first time they are asked for, and kept in Freya's cache folder. Ask only for covers on screen, which a `ListView` does by itself:

```slint
property <image> cover: Library.cover(row.cover, Library.covers-ready);
if cover.width > 0: Image { source: cover; image-fit: cover; }
```

Freya looks for a cover in the track's tags, then for an image next to it (`cover`, `folder`, `front`, … or the folder's only image), then in a `Cover`, `Covers`, `Artwork` or `Scans` folder beside it.

### `Playlists`

Playlists the user keeps in Freya, apart from the queue: `Playlist` is what plays (shown as Up Next in the built-in skin), `Playlists` are the lists kept for later. Each is an M3U file in Freya's data folder, in a `playlists` folder, named after the playlist.

| Property | Type | |
|---|---|---|
| `playlists` | `[PlaylistRow]` | `name`, `count` (tracks, as text), in name order |
| `shown` | int | The playlist chosen with `show`, or -1 |
| `tracks` | `[TrackRow]` | The tracks of the playlist shown; `selected` and `current` are false |
| `summary` | string | "12 tracks · 48:10" for the playlist shown |
| `error` | string | Why the last `create`, `save-queue` or `rename` failed; empty after one that worked. Freya answers at once, so check it right after |
| `page` | `PlaylistPage` | The playlist shown as its own page shows it: `name`, `description`, `picture` (for `Library.cover`: the user's picture, else its tracks' covers), `banner` (its wide picture, once decoded) and `has-banner`, `color`, `summary` ("84 tracks · 6 h 9 min") and `sort` |
| `entries` | `[PlaylistEntryRow]` | Its tracks in the order of `page.sort`: `number`, `title`, `artist`, `album`, `cover` (for `Library.cover`), `added` ("23 Dec 2025") and `duration`. `play`, `remove` and the rest take rows of these |
| `draft` | `PlaylistDraft` | The playlist being changed: `shown`, `name`, `description`, `picture` and `backdrop` as chosen so far, and `error` |

| Callback | |
|---|---|
| `show(index)` | Choose the playlist the other callbacks act on |
| `create(name)` | A new, empty playlist, which is then shown. Names must be new, and cannot start with a dot or hold `/ \ : * ? " < > \|` |
| `save-queue(name)` | Keep the playlist (Up Next) as a new playlist, which is then shown |
| `rename(name)`, `delete()` | The playlist shown; deleting removes its file, never the music |
| `play(row)` | The playlist shown becomes the playlist that plays, from `row` |
| `enqueue()` | Add the playlist shown to the end of the playlist that plays |
| `remove(row)` | Take a track out of the playlist shown |
| `add-queue-selection(index)` | Add the selected tracks of the playlist that plays (all of them when none is selected) to a playlist |
| `add-library-row(index, row)` | Add everything a library row stands for to a playlist |
| `shuffle()` | The playlist shown becomes the playlist that plays, shuffled |
| `set-sort(sort)` | Order the tracks shown: "custom" (as the playlist has them), "title", "artist", "album", "added" or "duration" |
| `edit()`, `save-edit(name, description)`, `cancel-edit()` | Change the playlist shown: its name, description and pictures, kept in Freya beside its file |
| `pick-picture(kind)`, `clear-picture(kind)` | While changing it, ask the user for a picture file or take one away: "picture" or "backdrop" |

### `Radio`

Saved internet radio stations, kept by Freya in `stations.m3u8` next to the playlist. New users start with a few well-known stations.

| Property | Type | |
|---|---|---|
| `stations` | `[StationRow]` | `name`, `address` (without "https://"), `playing` |
| `error` | string | Why the last `add` failed, like "Not a web address: …"; empty after one that worked |

| Callback | |
|---|---|
| `play(row)` | Play a station: its entry in the playlist when it is there, else added to the end |
| `add(name, address)` | Save a station: its stream, or its `.pls` or `.m3u` file. "http://" may be left out; without a name the station is named after the address. Freya answers at once, so check `error` right after |
| `remove(row)` | |

### `Equalizer`

| Property | Type | |
|---|---|---|
| `enabled` | bool | |
| `preamp` | float | dB, -12 to 12 |
| `bands` | `[float]` | Ten gains in dB, from 60 Hz to 16 kHz |
| `band-labels` | `[string]` | "60", "170", … "16k" |
| `presets` | `[string]` | Names of the built-in presets |
| `preset-name` | string | The preset matching the current gains, or "Custom" |

| Callback | |
|---|---|
| `toggle()`, `reset()` | |
| `set-preamp(db)`, `set-band(index, db)` | Freya clamps, rounds to 0.1 dB and snaps values near 0 to 0 |
| `load-preset(index)` | Index into `presets` |
| `load-file()`, `save-file()` | Winamp `.eqf` files |
| `curve(width, height, bands) -> string` | SVG path data of the response curve for a `width` × `height` box |

### `Visualizer`

| Property | Type | |
|---|---|---|
| `mode` | string | "spectrum", "oscilloscope", "hologram" or "off" |
| `bar-count` | int | `in-out`: how many bars the skin wants; set it in `init` |
| `bars`, `peaks` | `[float]` | 0 to 1, low to high frequencies |
| `waveform` | `[float]` | The last 10 ms, -1 to 1 |
| `showing` | bool | `in-out`, true by default: set it to false while no visualization is on screen, for example when it has a view of its own and another view is shown |
| `fps` | int | Frames a second the visualization is drawn at while the window has focus, 30 by default; kept between runs |

| Callback | |
|---|---|
| `cycle-mode()` | Spectrum, oscilloscope, off |
| `set-mode(mode)` | "spectrum", "oscilloscope", "hologram" or "off"; other values are ignored |
| `set-fps(fps)` | Draw at `fps` frames a second, 10 to 120; other values are ignored |
| `bars-path(width, height, bars) -> string` | SVG path data of the spectrum bars for a `width` × `height` box (or use `NativeSpectrum`, below) |
| `peaks-path(width, height, peaks, thickness) -> string` | The same for the peak markers |
| `wave(width, height, waveform) -> string` | SVG path data of the waveform for a `width` × `height` box (or use `NativeOscilloscope`, below) |

Freya only computes visualization while something plays, the mode is not "off", the skin is `showing` it and the window can be seen; while another app has focus it draws at most 15 frames a second. `bars`, `peaks` and `waveform` are new lists on every frame that changes.

#### `NativeSpectrum`: a spectrum that costs almost no CPU

Every frame Slint draws redraws the whole window, which costs far more than the spectrum itself (about 8 percentage points of CPU at 30 frames a second on a MacBook). Put a `NativeSpectrum` where the spectrum should be instead:

```slint
if Visualizer.mode == "spectrum": NativeSpectrum {
    width: 144px;
    height: 60px;
    bar-color: #a35f00;
    peak-color: #8a4f00;
    background-color: #ffffff;
}
```

Freya then draws the bars itself in a layer on top of the window, and the window is not redrawn for them at all: CPU drops to about what it is with visualization off. It does so on macOS (Core Animation), Windows (Windows.UI.Composition) and Linux (a Wayland subsurface, or an X11 child window). Elsewhere, in previews, or with `FREYA_NATIVE_SPECTRUM=off` set, `NativeSpectrum` draws the bars with Slint, like `bars-path` and `peaks-path` would. Properties: `bar-color`, `peak-color`, `peak-thickness` (2 px), `background-color` (the color behind the bars; X11 cannot draw see-through, so set it to the color around the spectrum) and `shown` (set it to false when the spectrum should not show, e.g. when its box is squeezed). Limits: use one per skin, nothing in the skin is drawn on top of it (keep pop-ups away from it), and colors are solid. `Visualizer.native-drawing` says which way it is drawn; `Visualizer.native-area` is how the component tells Freya where it is.

Without `NativeSpectrum`, draw the spectrum with `bars-path` and `peaks-path` (two `Path` elements) rather than one `Rectangle` per bar: Slint then evaluates two bindings per frame instead of several per bar.

#### `NativeOscilloscope`: an oscilloscope that costs almost no CPU

Drawn with `wave` and a `Path`, the oscilloscope redraws its whole box every frame; with the software renderer Freya uses on macOS that costs about two thirds of a CPU core full screen. Put a `NativeOscilloscope` there instead, and keep the `Path` for systems where Freya cannot draw the line:

```slint
if Visualizer.mode == "oscilloscope" && Visualizer.oscilloscope-drawing: NativeOscilloscope {
    width: 100%;
    height: 100%;
    line-color: #ffb22e;
    line-width: 2px;
}
if Visualizer.mode == "oscilloscope" && !Visualizer.oscilloscope-drawing: Path {
    // ... as under "Drawing curves and waveforms"
}
```

Freya then hands the line's points to the system 30 times a second, and the graphics card draws it in a layer on top of the window; `waveform` is not updated meanwhile. The line has the same shape as `wave` draws: the box's full width and 90 % of its height. So far it is drawn on macOS (Core Animation) only. Properties: `line-color`, `line-width` (2 px) and `shown`. The same limits as for `NativeSpectrum` apply: one per skin, and nothing of the skin is drawn on top of it. `Visualizer.oscilloscope-area` is how the component tells Freya where it is.

#### `NativeHologram`: the hologram

The mode "hologram" is a woman projected in translucent light over a city at night in the rain, after Blade Runner 2049. Freya draws it with the graphics card, in a layer of its own on top of the window, at 60 frames a second; the bass drives its glitches and glow, and she moves whether or not something plays. Put a `NativeHologram` where it should be:

```slint
if Visualizer.mode == "hologram": NativeHologram {
    width: parent.width;
    height: parent.height;
    border-radius: 12px;    // the layer's corners follow it
    shown: root.shown;      // false while its view is hidden: Freya then draws nothing
}
```

It costs little: measured in a 1400 × 900 point box on a Retina MacBook, about 0.3 % of a CPU core for the drawing, and about 1.8 ms of the graphics card's time a frame (the city behind her is drawn at a quarter of the resolution and 30 times a second, she and the rain at full resolution every frame). Freya sets it up as soon as the mode is chosen, on a thread of its own, so it shows at once. So far it is drawn on macOS (Metal) only; elsewhere `Visualizer.hologram-drawing` is false and the box shows its background, so say so in it. The same limits as for `NativeSpectrum` apply: one per skin, and nothing of the skin is drawn on top of it.

Her poses are pictures in `assets/hologram/` (JPEG or PNG, built into Freya), shown in the order of their names: the same woman on a pure black background, which is see-through in the hologram, all of the same shape.

### `App`

| Property | Type | |
|---|---|---|
| `version` | string | Freya's version |
| `build-date` | string | When this copy of Freya was built, as "2026-10-17"; empty when unknown |
| `theme` | string | "system", "light" or "dark", as chosen by the user |
| `equalizer-visible` | bool | Whether the user wants the equalizer shown |
| `title-bar-height` | length | `in-out`, set by the skin: see below. 0 by default |
| `window-buttons-width` | length | The room the window buttons take at the left of the skin's top bar; 0 while the system draws its own title bar, and in full screen |

| Callback | |
|---|---|
| `show-about()`, `show-skins()` | Freya's About window and skin picker |
| `set-theme(name)`, `toggle-equalizer()` | |
| `drag-window()` | Call on a mouse press in an empty part of the top bar: the window follows the mouse, and a double click zooms or minimizes it, as the system's settings say |
| `open-link(name)` | Opens one of Freya's own pages in the browser: "github", "releases", "issues" or "license". Other names are ignored: a skin cannot open other addresses |

A settings page in a skin can be built from what the API already offers: `App.set-theme`, `App.show-skins`, the library's `folders`, `add-folder`, `remove-folder(index)` and `rescan`, the visualizer's `set-mode` and `set-fps`, and `Updates`. The built-in skin opens its Settings and About views from the "⋯" menu at the top right (Cmd+, or Ctrl+, opens Settings).

#### Drawing into the title bar

On macOS, a skin can reach the window's top edge, with the window buttons inside its own top bar, as in Apple's own apps. Set `App.title-bar-height` to the height of the top bar, in the window's `init`; Freya then makes the title bar transparent and centres the window buttons in that height, 20 points from the left edge. Keep `App.window-buttons-width` free at the left of whatever is at the top left, and let empty parts of the top bar move the window:

```slint
init => { App.title-bar-height = 56px; }

// In the top bar, behind its controls:
TouchArea {
    pointer-event(event) => {
        if (event.kind == PointerEventKind.down && App.window-buttons-width > 0) {
            App.drag-window();
        }
    }
}
```

On other systems the title bar stays the system's, and `window-buttons-width` stays 0, so the same skin works everywhere.

### `LastFm`

Scrobbling to the user's Last.fm profile. Optional, and off until the user connects an account. Freya then tells Last.fm what plays ("now playing"), and sends each track heard for half its length or four minutes, whichever comes first, if it is tagged with an artist and a title and is longer than 30 seconds; radio never. Plays wait on disk while Last.fm cannot be reached.

| Property | Type | |
|---|---|---|
| `state` | string | "unavailable" (this build has no Last.fm API key), "disconnected", "authorizing" (Freya opened Last.fm's page and waits for the user to allow it; it connects by itself once they have) or "connected" |
| `user` | string | The connected user's name |
| `scrobbling` | bool | Whether plays are scrobbled while connected; kept between runs |
| `waiting` | int | Plays waiting to be sent |
| `error` | string | What went wrong last, until something works again |

| Callback | |
|---|---|
| `connect()` | Opens Last.fm's page in the browser, where the user lets Freya scrobble for them |
| `cancel()` | Stops waiting for the user to allow Freya |
| `disconnect()` | Forgets the account, and the plays waiting to be sent |
| `set-scrobbling(on)` | |

### `Server`

The server that keeps the library: Freya's own on this computer, or one on another computer on the network, which the user finds or types in and logs in to. The first time, Freya takes the other server's certificate, and the user types the connection code Settings shows on that computer: the server proves it knows the code with that certificate, so another computer answering in its place is found out. After, Freya trusts that certificate only. Logging in and setting the password happen in the user's browser, on the server's own pages, which Freya opens at `http://127.0.0.1:47477`. Optional: without it, a skin always shows this computer's library.

| Property | Type | |
|---|---|---|
| `remote`, `name`, `address` | bool, string, string | Whether the library is on another computer, what that server is called and where it is |
| `state` | string | "connecting", "connected", "unreachable" (Freya tries again by itself) or "log-in" (the server no longer takes this computer: log in again) |
| `error` | string | Why it is unreachable, or must be logged in to |
| `fingerprint` | string | The server's certificate's fingerprint |
| `server-name`, `network`, `autostart` | string, bool, bool | For the server on this computer: what others find it as, whether they may connect, and whether it starts when the user logs in |
| `user`, `has-password` | string, bool | The account this computer is connected to, and whether it has a password; only then can other computers log in |
| `devices` | [`ServerDevice`] | The devices connected to the account: `id`, `name`, `connected` and `used` (dates; `used` is "" when not known), `joined` ("paired" on the server's own computer, "logged-in" in the browser, or ""), `this` |
| `password-waiting`, `password-error` | bool, string | While the page the password is set or changed on is open in the browser; why it could not be opened |
| `finding`, `found` | bool, [`FoundServer`] | While looking for servers on the network, and those that answered: `name`, `address`, `fingerprint` |
| `logging-in`, `login-address`, `login-fingerprint`, `login-busy`, `login-waiting`, `login-error` | | While logging in to a server chosen: where it is, its certificate's fingerprint, whether Freya is asking it now, whether its login page is open in the browser, and what went wrong |
| `login-verified` | bool | Whether the server chosen proved it knows the connection code, or is the one this computer used before: only then can it be logged in to |
| `connection-code-error` | string | Why no connection code could be shown; "" otherwise |
| `connection-code`, `connection-code-until` | string | On the server's computer: the connection code shown for a new device ("K7Q2M-9XD4T") and until when it works ("20:31"); "" when none is |

| Callback | |
|---|---|
| `find()` | Looks for servers on the network |
| `choose-found(index)`, `choose-address(text)` | Starts logging in to a server found, or to one typed in ("mini.local", "192.168.1.20:47475"): Freya takes its certificate, and `logging-in` turns on |
| `verify-code(code)` | Checks the connection code typed with the server chosen; `login-verified` turns on when it matches |
| `show-connection-code()` | On the server's computer: shows a new connection code, in place of any before |
| `log-in()`, `cancel-login()` | Opens the server's login page in the browser, where a password manager fills it in; once the user logged in there, the library is that server's |
| `use-this-computer()` | The library is this computer's server's again |
| `change-password()` | Opens the page the password is set or changed on in the browser |
| `revoke(id)` | Takes a device's token back: it has to log in again |
| `log-out()` | Logs this computer out of the server elsewhere: the server takes its token back, and the library is this computer's again |
| `set-network(on)`, `set-server-name(name)`, `set-autostart(on)` | For the server on this computer |
| `browse-start()`, `browse-into(index)`, `browse-up()`, `browse-add()`, `browse-cancel()` | Choosing a music folder among a server's on another computer: `browsing`, `browse-folder` ("" where to start: the home folder and the drives) and `browse-folders` say what is shown; `browse-add` adds the folder shown to `Library.folders-draft` |

### `Updates`

Updates to Freya. Freya looks at launch and once a day while `check-automatically` is on, and installs only when the user asks: it downloads the new version, checks it against the release's signed checksums, puts it in place of the running Freya and restarts. On macOS (the app bundle) and Linux (the AppImage) Freya installs updates itself; elsewhere `install` opens the release's page.

| Property | Type | |
|---|---|---|
| `state` | string | "idle", "checking" (only when the user asked), "up-to-date", "available", "downloading", "installed" (Freya restarts) or "failed" |
| `current-version`, `version` | string | The running version, and the new one while it is available, downloading or installed |
| `notes` | string | The new version's release notes as plain text; list items start with "• " |
| `progress` | float | 0 to 1 while downloading |
| `error` | string | Why looking or installing failed |
| `can-install` | bool | Whether Freya installs the update itself; otherwise `install` opens the release page |
| `check-automatically` | bool | Whether Freya looks on its own; kept between runs |

| Callback | |
|---|---|
| `check()` | Look now; the answer shows in `state` either way, and a skipped version is offered again |
| `install()` | Download, check and install the update, then restart Freya |
| `later()` | Hide the update until Freya looks again |
| `skip()` | Never offer this version again |
| `set-check-automatically(on)` | |

### `Shortcuts`

A `FocusScope` with Freya's standard keys: Space play/pause, ←/→ seek 5 s, ↑/↓ volume, N/P next/previous, S stop, O open files, F1 about, F2 skins. Put your window's content inside it and give it focus with `forward-focus`, as in the first example. Inner focus scopes (like a playlist that uses the arrow keys) get keys first and can let the rest through by returning `reject`.

### `AboutFreya`

The credits Freya shows in its About window. You can show it in your skin too.

## Drawing curves and waveforms

Slint's `Path` keeps the aspect ratio of its viewbox, so a path made for a fixed box would be squashed. `Equalizer.curve`, `Visualizer.bars-path`, `Visualizer.peaks-path` and `Visualizer.wave` therefore take the box size, and return path data that fits it exactly. Use the element's own size as the viewbox:

```slint
Path {
    viewbox-width: self.width / 1px;
    viewbox-height: self.height / 1px;
    commands: Equalizer.curve(self.width / 1px, self.height / 1px, Equalizer.bands);
    stroke: #ffb22e;
    stroke-width: 2px;
}
```

Passing `Equalizer.bands` makes Slint redraw the curve whenever the bands change.

## Advice

- **Readability.** Use at least 12 px for text, and check contrast: at least 4.5:1 between text and its background (WCAG AA). The built-in skin follows `App.theme` and the system's light or dark mode.
- **Icons.** Prefer SVG path data in a `Path`, or SVG files with `@image-url`. Symbols like ▶ are missing from many fonts.
- **Accessibility.** Give buttons and sliders an `accessible-role` and `accessible-label`, so screen readers can use the skin.
- **Tooltips.** Icons alone are hard to guess: put a `Tooltip { text: "..."; }` (built into Slint) in each icon button, ideally with its keyboard shortcut. A tooltip opens next to the pointer inside the window, so one that would land on a `NativeSpectrum` is covered by it.
- **Small windows.** Set `min-width` and `min-height`, and let long titles end with `overflow: elide`.
- **Efficiency.** Avoid animations that run all the time: each frame redraws the whole window.
- **Text that changes often**, like `Player.position-text`, is best kept out of layouts. Inside one, every change lays out the whole window again, and Slint redraws everything whose place depends on it, such as the lists' scroll bars, even when nothing moved. Leave a slot of fixed size in the layout, and draw the text over it as a child of the component's root element: `x: slot.absolute-position.x - root.absolute-position.x`, and the same for `y`, with the slot's width and height. The built-in skin does this for its times; only the text is then redrawn.

## Compatibility

Version 1 of the API only grows: new properties and callbacks may be added, but nothing a skin relies on is removed or changed. A breaking change will come as `@freya/v2.slint`, and Freya will keep serving v1 for existing skins as long as practical.
