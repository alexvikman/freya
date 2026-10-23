//! The built-in skin, compiled into Freya (see `build.rs`) instead of read by Slint's
//! interpreter: it starts faster and uses about a third less memory.
//!
//! The rest of the app talks to skins by name, with the interpreter's `Value`s, so that one
//! piece of code serves every skin. This module gives the compiled skin that same face: the
//! table at the bottom lists the skin API once, and maps each property and callback to the
//! typed methods Slint generated for it. A test checks the table against the API itself.

use std::rc::Rc;

use slint::{Brush, Color, ComponentHandle, Image, ModelExt, ModelRc, SharedString};
use slint_interpreter::{Struct, Value};

slint::include_modules!();

/// A skin callback as the app writes it: values in, a value out.
pub type Handler = Rc<dyn Fn(&[Value]) -> Value>;

/// The compiled built-in skin.
pub struct Builtin {
    ui: FreyaSkin,
}

/// Another handle to the same window.
impl Clone for Builtin {
    fn clone(&self) -> Self {
        Self {
            ui: self.ui.clone_strong(),
        }
    }
}

impl Builtin {
    pub fn new() -> Result<Self, slint::PlatformError> {
        Ok(Self {
            ui: FreyaSkin::new()?,
        })
    }

    pub fn window(&self) -> &slint::Window {
        self.ui.window()
    }

    /// A handle that does not keep the window open.
    pub fn as_weak(&self) -> slint::Weak<FreyaSkin> {
        self.ui.as_weak()
    }

    /// The window `as_weak` was made from, while it is open.
    pub fn upgrade(weak: &slint::Weak<FreyaSkin>) -> Option<Self> {
        weak.upgrade().map(|ui| Self { ui })
    }

    pub fn show(&self) -> Result<(), slint::PlatformError> {
        self.ui.show()
    }

    pub fn hide(&self) -> Result<(), slint::PlatformError> {
        self.ui.hide()
    }

    /// The globals the skin exports: all of the API.
    pub fn globals() -> impl Iterator<Item = &'static str> {
        GLOBALS.iter().copied()
    }
}

/// Converts between the interpreter's `Value`s and the compiled skin's Rust types.
trait Convert: Sized {
    fn from_value(value: Value) -> Option<Self>;
    fn to_value(self) -> Value;
}

impl Convert for () {
    fn from_value(_: Value) -> Option<Self> {
        Some(())
    }

    fn to_value(self) -> Value {
        Value::Void
    }
}

impl Convert for bool {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }

    fn to_value(self) -> Value {
        Value::Bool(self)
    }
}

impl Convert for f32 {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Number(n) => Some(n as f32),
            _ => None,
        }
    }

    fn to_value(self) -> Value {
        Value::Number(f64::from(self))
    }
}

impl Convert for i32 {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Number(n) => Some(n.round() as i32),
            _ => None,
        }
    }

    fn to_value(self) -> Value {
        Value::Number(f64::from(self))
    }
}

impl Convert for SharedString {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    fn to_value(self) -> Value {
        Value::String(self)
    }
}

impl Convert for Image {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Image(image) => Some(image),
            _ => None,
        }
    }

    fn to_value(self) -> Value {
        Value::Image(self)
    }
}

impl Convert for Color {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Brush(brush) => Some(brush.color()),
            _ => None,
        }
    }

    fn to_value(self) -> Value {
        Value::Brush(Brush::SolidColor(self))
    }
}

impl Convert for PlaybackState {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::EnumerationValue(_, state) => match state.as_str() {
                "stopped" => Some(Self::Stopped),
                "playing" => Some(Self::Playing),
                "paused" => Some(Self::Paused),
                _ => None,
            },
            _ => None,
        }
    }

    fn to_value(self) -> Value {
        let state = match self {
            Self::Stopped => "stopped",
            Self::Playing => "playing",
            Self::Paused => "paused",
        };
        Value::EnumerationValue("PlaybackState".into(), state.into())
    }
}

/// Lists: the app's list of `Value`s is shown through a mapping, so it can keep changing rows
/// of its own list as before.
impl<T: Convert + Clone + Default + 'static> Convert for ModelRc<T> {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Model(model) => Some(ModelRc::new(
                model.map(|v| T::from_value(v).unwrap_or_default()),
            )),
            _ => None,
        }
    }

    fn to_value(self) -> Value {
        Value::Model(ModelRc::new(self.map(Convert::to_value)))
    }
}

/// A struct field of a `Value::Struct`, or its default when missing or of another type.
fn field<T: Convert + Default>(s: &Struct, name: &str) -> T {
    s.get_field(name)
        .cloned()
        .and_then(T::from_value)
        .unwrap_or_default()
}

fn structure(fields: Vec<(&str, Value)>) -> Value {
    Value::Struct(Struct::from_iter(
        fields
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value)),
    ))
}

impl Convert for TrackRow {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            number: field(&s, "number"),
            title: field(&s, "title"),
            duration: field(&s, "duration"),
            selected: field(&s, "selected"),
            current: field(&s, "current"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("number", self.number.to_value()),
            ("title", self.title.to_value()),
            ("duration", self.duration.to_value()),
            ("selected", self.selected.to_value()),
            ("current", self.current.to_value()),
        ])
    }
}

impl Convert for PlaylistRow {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            name: field(&s, "name"),
            count: field(&s, "count"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("name", self.name.to_value()),
            ("count", self.count.to_value()),
        ])
    }
}

impl Convert for ServerDevice {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            id: field(&s, "id"),
            name: field(&s, "name"),
            connected: field(&s, "connected"),
            used: field(&s, "used"),
            joined: field(&s, "joined"),
            this: field(&s, "this"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("id", self.id.to_value()),
            ("name", self.name.to_value()),
            ("connected", self.connected.to_value()),
            ("used", self.used.to_value()),
            ("joined", self.joined.to_value()),
            ("this", self.this.to_value()),
        ])
    }
}

impl Convert for ServerUser {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            name: field(&s, "name"),
            role: field(&s, "role"),
            state: field(&s, "state"),
            invited_until: field(&s, "invited-until"),
            devices: field(&s, "devices"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("name", self.name.to_value()),
            ("role", self.role.to_value()),
            ("state", self.state.to_value()),
            ("invited-until", self.invited_until.to_value()),
            ("devices", self.devices.to_value()),
        ])
    }
}

impl Convert for FoundServer {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            name: field(&s, "name"),
            address: field(&s, "address"),
            fingerprint: field(&s, "fingerprint"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("name", self.name.to_value()),
            ("address", self.address.to_value()),
            ("fingerprint", self.fingerprint.to_value()),
        ])
    }
}

impl Convert for StationRow {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            name: field(&s, "name"),
            address: field(&s, "address"),
            playing: field(&s, "playing"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("name", self.name.to_value()),
            ("address", self.address.to_value()),
            ("playing", self.playing.to_value()),
        ])
    }
}

impl Convert for LibraryRow {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            kind: field(&s, "kind"),
            title: field(&s, "title"),
            detail: field(&s, "detail"),
            artist: field(&s, "artist"),
            album: field(&s, "album"),
            cover: field(&s, "cover"),
            number: field(&s, "number"),
            duration: field(&s, "duration"),
            played: field(&s, "played"),
            plays: field(&s, "plays"),
            favorite: field(&s, "favorite"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("kind", self.kind.to_value()),
            ("title", self.title.to_value()),
            ("detail", self.detail.to_value()),
            ("artist", self.artist.to_value()),
            ("album", self.album.to_value()),
            ("cover", self.cover.to_value()),
            ("number", self.number.to_value()),
            ("duration", self.duration.to_value()),
            ("played", self.played.to_value()),
            ("plays", self.plays.to_value()),
            ("favorite", self.favorite.to_value()),
        ])
    }
}

impl Convert for Activity {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            title: field(&s, "title"),
            detail: field(&s, "detail"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("title", self.title.to_value()),
            ("detail", self.detail.to_value()),
        ])
    }
}

impl Convert for PlaylistPage {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            name: field(&s, "name"),
            description: field(&s, "description"),
            picture: field(&s, "picture"),
            banner: field(&s, "banner"),
            has_banner: field(&s, "has-banner"),
            color: field(&s, "color"),
            summary: field(&s, "summary"),
            sort: field(&s, "sort"),
            favorite: field(&s, "favorite"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("name", self.name.to_value()),
            ("description", self.description.to_value()),
            ("picture", self.picture.to_value()),
            ("banner", self.banner.to_value()),
            ("has-banner", self.has_banner.to_value()),
            ("color", self.color.to_value()),
            ("summary", self.summary.to_value()),
            ("sort", self.sort.to_value()),
            ("favorite", self.favorite.to_value()),
        ])
    }
}

impl Convert for PlaylistEntryRow {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            number: field(&s, "number"),
            title: field(&s, "title"),
            artist: field(&s, "artist"),
            album: field(&s, "album"),
            cover: field(&s, "cover"),
            added: field(&s, "added"),
            duration: field(&s, "duration"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("number", self.number.to_value()),
            ("title", self.title.to_value()),
            ("artist", self.artist.to_value()),
            ("album", self.album.to_value()),
            ("cover", self.cover.to_value()),
            ("added", self.added.to_value()),
            ("duration", self.duration.to_value()),
        ])
    }
}

impl Convert for PlaylistDraft {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            shown: field(&s, "shown"),
            name: field(&s, "name"),
            description: field(&s, "description"),
            picture: field(&s, "picture"),
            backdrop: field(&s, "backdrop"),
            error: field(&s, "error"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("shown", self.shown.to_value()),
            ("name", self.name.to_value()),
            ("description", self.description.to_value()),
            ("picture", self.picture.to_value()),
            ("backdrop", self.backdrop.to_value()),
            ("error", self.error.to_value()),
        ])
    }
}

impl Convert for SearchSection {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            kind: field(&s, "kind"),
            title: field(&s, "title"),
            first: field(&s, "first"),
            count: field(&s, "count"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("kind", self.kind.to_value()),
            ("title", self.title.to_value()),
            ("first", self.first.to_value()),
            ("count", self.count.to_value()),
        ])
    }
}

impl Convert for GenreDraft {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            shown: field(&s, "shown"),
            name: field(&s, "name"),
            description: field(&s, "description"),
            picture: field(&s, "picture"),
            backdrop: field(&s, "backdrop"),
            error: field(&s, "error"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("shown", self.shown.to_value()),
            ("name", self.name.to_value()),
            ("description", self.description.to_value()),
            ("picture", self.picture.to_value()),
            ("backdrop", self.backdrop.to_value()),
            ("error", self.error.to_value()),
        ])
    }
}

impl Convert for LetterJump {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            letter: field(&s, "letter"),
            row: field(&s, "row"),
            present: field(&s, "present"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("letter", self.letter.to_value()),
            ("row", self.row.to_value()),
            ("present", self.present.to_value()),
        ])
    }
}

impl Convert for LogEntry {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            time: field(&s, "time"),
            level: field(&s, "level"),
            message: field(&s, "message"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("time", self.time.to_value()),
            ("level", self.level.to_value()),
            ("message", self.message.to_value()),
        ])
    }
}

impl Convert for ArtistMatchDraft {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            artist: field(&s, "artist"),
            mbid: field(&s, "mbid"),
            found_mbid: field(&s, "found-mbid"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("artist", self.artist.to_value()),
            ("mbid", self.mbid.to_value()),
            ("found-mbid", self.found_mbid.to_value()),
        ])
    }
}

impl Convert for AlbumMatchDraft {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            title: field(&s, "title"),
            artist: field(&s, "artist"),
            folder: field(&s, "folder"),
            release_group: field(&s, "release-group"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("title", self.title.to_value()),
            ("artist", self.artist.to_value()),
            ("folder", self.folder.to_value()),
            ("release-group", self.release_group.to_value()),
        ])
    }
}

impl Convert for AlbumMatchChoice {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            release_group: field(&s, "release-group"),
            title: field(&s, "title"),
            facts: field(&s, "facts"),
            artist: field(&s, "artist"),
            detail: field(&s, "detail"),
            picture: field(&s, "picture"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("release-group", self.release_group.to_value()),
            ("title", self.title.to_value()),
            ("facts", self.facts.to_value()),
            ("artist", self.artist.to_value()),
            ("detail", self.detail.to_value()),
            ("picture", self.picture.to_value()),
        ])
    }
}

impl Convert for MatchChoice {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            mbid: field(&s, "mbid"),
            name: field(&s, "name"),
            detail: field(&s, "detail"),
            facts: field(&s, "facts"),
            picture: field(&s, "picture"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("mbid", self.mbid.to_value()),
            ("name", self.name.to_value()),
            ("detail", self.detail.to_value()),
            ("facts", self.facts.to_value()),
            ("picture", self.picture.to_value()),
        ])
    }
}

impl Convert for PictureChoice {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            picture: field(&s, "picture"),
            source: field(&s, "source"),
            size: field(&s, "size"),
            chosen: field(&s, "chosen"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("picture", self.picture.to_value()),
            ("source", self.source.to_value()),
            ("size", self.size.to_value()),
            ("chosen", self.chosen.to_value()),
        ])
    }
}

impl Convert for ArtistDraft {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            artist: field(&s, "artist"),
            name: field(&s, "name"),
            sort_name: field(&s, "sort-name"),
            mbid: field(&s, "mbid"),
            found_mbid: field(&s, "found-mbid"),
            biography: field(&s, "biography"),
            biography_from: field(&s, "biography-from"),
            header: field(&s, "header"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("artist", self.artist.to_value()),
            ("name", self.name.to_value()),
            ("sort-name", self.sort_name.to_value()),
            ("mbid", self.mbid.to_value()),
            ("found-mbid", self.found_mbid.to_value()),
            ("biography", self.biography.to_value()),
            ("biography-from", self.biography_from.to_value()),
            ("header", self.header.to_value()),
        ])
    }
}

impl Convert for ArtistPage {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            shown: field(&s, "shown"),
            kind: field(&s, "kind"),
            favorite: field(&s, "favorite"),
            name: field(&s, "name"),
            detail: field(&s, "detail"),
            genres: field(&s, "genres"),
            picture: field(&s, "picture"),
            banner: field(&s, "banner"),
            has_banner: field(&s, "has-banner"),
            portrait: field(&s, "portrait"),
            picture_full: field(&s, "picture-full"),
            color: field(&s, "color"),
            summary: field(&s, "summary"),
            biography: field(&s, "biography"),
            tags: field(&s, "tags"),
            own_biography: field(&s, "own-biography"),
            biography_from: field(&s, "biography-from"),
            note: field(&s, "note"),
            more: field(&s, "more"),
            popular_count: field(&s, "popular-count"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("shown", self.shown.to_value()),
            ("kind", self.kind.to_value()),
            ("favorite", self.favorite.to_value()),
            ("name", self.name.to_value()),
            ("detail", self.detail.to_value()),
            ("genres", self.genres.to_value()),
            ("picture", self.picture.to_value()),
            ("banner", self.banner.to_value()),
            ("has-banner", self.has_banner.to_value()),
            ("portrait", self.portrait.to_value()),
            ("picture-full", self.picture_full.to_value()),
            ("color", self.color.to_value()),
            ("summary", self.summary.to_value()),
            ("biography", self.biography.to_value()),
            ("tags", self.tags.to_value()),
            ("own-biography", self.own_biography.to_value()),
            ("biography-from", self.biography_from.to_value()),
            ("note", self.note.to_value()),
            ("more", self.more.to_value()),
            ("popular-count", self.popular_count.to_value()),
        ])
    }
}

impl Convert for AlbumPage {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            shown: field(&s, "shown"),
            title: field(&s, "title"),
            artist: field(&s, "artist"),
            detail: field(&s, "detail"),
            cover: field(&s, "cover"),
            color: field(&s, "color"),
            favorite: field(&s, "favorite"),
            picture_full: field(&s, "picture-full"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("shown", self.shown.to_value()),
            ("title", self.title.to_value()),
            ("artist", self.artist.to_value()),
            ("detail", self.detail.to_value()),
            ("cover", self.cover.to_value()),
            ("color", self.color.to_value()),
            ("favorite", self.favorite.to_value()),
            ("picture-full", self.picture_full.to_value()),
        ])
    }
}

impl Convert for SpectrumArea {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            x: field(&s, "x"),
            y: field(&s, "y"),
            width: field(&s, "width"),
            height: field(&s, "height"),
            shown: field(&s, "shown"),
            bar_color: field(&s, "bar-color"),
            peak_color: field(&s, "peak-color"),
            peak_thickness: field(&s, "peak-thickness"),
            background: field(&s, "background"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("x", self.x.to_value()),
            ("y", self.y.to_value()),
            ("width", self.width.to_value()),
            ("height", self.height.to_value()),
            ("shown", self.shown.to_value()),
            ("bar-color", self.bar_color.to_value()),
            ("peak-color", self.peak_color.to_value()),
            ("peak-thickness", self.peak_thickness.to_value()),
            ("background", self.background.to_value()),
        ])
    }
}

impl Convert for OscilloscopeArea {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            x: field(&s, "x"),
            y: field(&s, "y"),
            width: field(&s, "width"),
            height: field(&s, "height"),
            shown: field(&s, "shown"),
            line_color: field(&s, "line-color"),
            line_width: field(&s, "line-width"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("x", self.x.to_value()),
            ("y", self.y.to_value()),
            ("width", self.width.to_value()),
            ("height", self.height.to_value()),
            ("shown", self.shown.to_value()),
            ("line-color", self.line_color.to_value()),
            ("line-width", self.line_width.to_value()),
        ])
    }
}

impl Convert for HologramArea {
    fn from_value(value: Value) -> Option<Self> {
        let Value::Struct(s) = value else { return None };
        Some(Self {
            x: field(&s, "x"),
            y: field(&s, "y"),
            width: field(&s, "width"),
            height: field(&s, "height"),
            corner_radius: field(&s, "corner-radius"),
            shown: field(&s, "shown"),
        })
    }

    fn to_value(self) -> Value {
        structure(vec![
            ("x", self.x.to_value()),
            ("y", self.y.to_value()),
            ("width", self.width.to_value()),
            ("height", self.height.to_value()),
            ("corner-radius", self.corner_radius.to_value()),
            ("shown", self.shown.to_value()),
        ])
    }
}

fn convert<T: Convert>(value: Value, what: &str) -> Result<T, String> {
    T::from_value(value).ok_or_else(|| format!("wrong type for {what}"))
}

/// What a callback hands back to the skin: its declared return type, or nothing.
macro_rules! returns {
    () => {
        ()
    };
    ($ty:ty) => {
        $ty
    };
}

/// Builds the name-based access to the compiled skin from one table of the skin API: each
/// property with its generated setter and getter, each callback with its generated `on_` and
/// `invoke_` methods and argument types.
macro_rules! skin_api {
    (
        properties {
            $( $pg:ident . $pn:literal => $set:ident, $get:ident; )*
        }
        callbacks {
            $( $cg:ident . $cn:literal => $on:ident, $invoke:ident ( $( $arg:ident : $ty:ty ),* ) $( -> $ret:ty )?; )*
        }
    ) => {
        /// Every property and callback of the table, as (global, name), for the test.
        #[cfg(test)]
        pub const PROPERTIES: &[(&str, &str)] = &[$( (stringify!($pg), $pn) ),*];
        #[cfg(test)]
        pub const CALLBACKS: &[(&str, &str)] = &[$( (stringify!($cg), $cn) ),*];

        impl Builtin {
            pub fn set_global_property(&self, global: &str, name: &str, value: Value) -> Result<(), String> {
                match (global, name) {
                    $( (stringify!($pg), $pn) => self.ui.global::<$pg>().$set(convert(value, $pn)?), )*
                    _ => return Err(format!("no property {global}.{name}")),
                }
                Ok(())
            }

            pub fn get_global_property(&self, global: &str, name: &str) -> Result<Value, String> {
                match (global, name) {
                    $( (stringify!($pg), $pn) => Ok(self.ui.global::<$pg>().$get().to_value()), )*
                    _ => Err(format!("no property {global}.{name}")),
                }
            }

            pub fn set_global_callback(&self, global: &str, name: &str, handler: Handler) -> Result<(), String> {
                match (global, name) {
                    $( (stringify!($cg), $cn) => {
                        self.ui.global::<$cg>().$on(move |$( $arg: $ty ),*| {
                            let result = handler(&[$( $arg.to_value() ),*]);
                            <returns!($( $ret )?)>::from_value(result).unwrap_or_default()
                        });
                    } )*
                    _ => return Err(format!("no callback {global}.{name}")),
                }
                Ok(())
            }

            #[cfg(test)]
            pub fn invoke_global(&self, global: &str, name: &str, args: &[Value]) -> Result<Value, String> {
                match (global, name) {
                    $( (stringify!($cg), $cn) => {
                        #[allow(unused_mut, unused_variables)]
                        let mut args = args.iter().cloned();
                        let result = self.ui.global::<$cg>().$invoke(
                            $( convert::<$ty>(args.next().unwrap_or_default(), stringify!($arg))? ),*
                        );
                        Ok(result.to_value())
                    } )*
                    _ => Err(format!("no callback {global}.{name}")),
                }
            }
        }
    };
}

const GLOBALS: [&str; 11] = [
    "Player",
    "Playlist",
    "Playlists",
    "Library",
    "Radio",
    "Equalizer",
    "Visualizer",
    "App",
    "Updates",
    "LastFm",
    "Server",
];

skin_api! {
    properties {
        Player."title" => set_title, get_title;
        Player."artist" => set_artist, get_artist;
        Player."album" => set_album, get_album;
        Player."format-info" => set_format_info, get_format_info;
        Player."live" => set_live, get_live;
        Player."error" => set_error, get_error;
        Player."state" => set_state, get_state;
        Player."position" => set_position, get_position;
        Player."length" => set_length, get_length;
        Player."position-text" => set_position_text, get_position_text;
        Player."length-text" => set_length_text, get_length_text;
        Player."remaining-text" => set_remaining_text, get_remaining_text;
        Player."volume" => set_volume, get_volume;
        Player."cover" => set_cover, get_cover;
        Player."has-cover" => set_has_cover, get_has_cover;

        Playlist."rows" => set_rows, get_rows;
        Playlist."summary" => set_summary, get_summary;
        Playlist."cursor" => set_cursor, get_cursor;
        Playlist."shuffle" => set_shuffle, get_shuffle;
        Playlist."repeat" => set_repeat, get_repeat;
        Playlist."drop-hover" => set_drop_hover, get_drop_hover;

        Library."rows" => set_rows, get_rows;
        Library."section" => set_section, get_section;
        Library."layout" => set_layout, get_layout;
        Library."covers-ready" => set_covers_ready, get_covers_ready;

        Playlists."playlists" => set_playlists, get_playlists;
        Playlists."shown" => set_shown, get_shown;
        Playlists."tracks" => set_tracks, get_tracks;
        Playlists."summary" => set_summary, get_summary;
        Playlists."error" => set_error, get_error;
        Playlists."page" => set_page, get_page;
        Playlists."entries" => set_entries, get_entries;
        Playlists."draft" => set_draft, get_draft;

        Radio."stations" => set_stations, get_stations;
        Radio."error" => set_error, get_error;
        Library."title" => set_title, get_title;
        Library."subtitle" => set_subtitle, get_subtitle;
        Library."can-go-back" => set_can_go_back, get_can_go_back;
        Library."query" => set_query, get_query;
        Library."folders" => set_folders, get_folders;
        Library."folders-draft" => set_folders_draft, get_folders_draft;
        Library."album-options" => set_album_options, get_album_options;
        Library."playing-row" => set_playing_row, get_playing_row;
        Library."playing-known" => set_playing_known, get_playing_known;
        Library."reveal-row" => set_reveal_row, get_reveal_row;
        Library."reveals" => set_reveals, get_reveals;
        Library."album-page" => set_album_page, get_album_page;
        Library."artist-options" => set_artist_options, get_artist_options;
        Library."artist-page" => set_artist_page, get_artist_page;
        Library."popular" => set_popular, get_popular;
        Library."artist-bios-online" => set_artist_bios_online, get_artist_bios_online;
        Library."artist-wikipedia-online" => set_artist_wikipedia_online, get_artist_wikipedia_online;
        Library."fanart-key" => set_fanart_key, get_fanart_key;
        Library."artist-draft" => set_artist_draft, get_artist_draft;
        Library."genre-draft" => set_genre_draft, get_genre_draft;
        Library."portrait-choices" => set_portrait_choices, get_portrait_choices;
        Library."backdrop-choices" => set_backdrop_choices, get_backdrop_choices;
        Library."finding-pictures" => set_finding_pictures, get_finding_pictures;
        Library."picture-error" => set_picture_error, get_picture_error;
        Library."artist-match" => set_artist_match, get_artist_match;
        Library."artist-matches" => set_artist_matches, get_artist_matches;
        Library."finding-matches" => set_finding_matches, get_finding_matches;
        Library."match-error" => set_match_error, get_match_error;
        Library."album-draft" => set_album_draft, get_album_draft;
        Library."album-matches" => set_album_matches, get_album_matches;
        Library."finding-album-matches" => set_finding_album_matches, get_finding_album_matches;
        Library."album-match-error" => set_album_match_error, get_album_match_error;
        Library."artist-sort" => set_artist_sort, get_artist_sort;
        Library."artist-view" => set_artist_view, get_artist_view;
        Library."artist-pictures-online" => set_artist_pictures_online, get_artist_pictures_online;
        Library."album-sort" => set_album_sort, get_album_sort;
        Library."album-view" => set_album_view, get_album_view;
        Library."scanning" => set_scanning, get_scanning;
        Library."letters" => set_letters, get_letters;
        Library."albums-options" => set_albums_options, get_albums_options;
        Library."albums-sort" => set_albums_sort, get_albums_sort;
        Library."playlists-options" => set_playlists_options, get_playlists_options;
        Library."playlists-sort" => set_playlists_sort, get_playlists_sort;
        Library."search-filter" => set_search_filter, get_search_filter;
        Library."search-shown" => set_search_shown, get_search_shown;
        Library."search-songs" => set_search_songs, get_search_songs;
        Library."search-sections" => set_search_sections, get_search_sections;
        Library."search-top" => set_search_top, get_search_top;
        Library."suggestions" => set_suggestions, get_suggestions;
        Library."watch-folders" => set_watch_folders, get_watch_folders;
        Library."scan-every-hours" => set_scan_every_hours, get_scan_every_hours;
        Library."scan-at-start" => set_scan_at_start, get_scan_at_start;
        Library."status" => set_status, get_status;
        Library."scan-folder" => set_scan_folder, get_scan_folder;

        Equalizer."enabled" => set_enabled, get_enabled;
        Equalizer."preamp" => set_preamp, get_preamp;
        Equalizer."bands" => set_bands, get_bands;
        Equalizer."band-labels" => set_band_labels, get_band_labels;
        Equalizer."presets" => set_presets, get_presets;
        Equalizer."preset-name" => set_preset_name, get_preset_name;

        Visualizer."mode" => set_mode, get_mode;
        Visualizer."bar-count" => set_bar_count, get_bar_count;
        Visualizer."bars" => set_bars, get_bars;
        Visualizer."peaks" => set_peaks, get_peaks;
        Visualizer."waveform" => set_waveform, get_waveform;
        Visualizer."native-drawing" => set_native_drawing, get_native_drawing;
        Visualizer."native-area" => set_native_area, get_native_area;
        Visualizer."oscilloscope-drawing" => set_oscilloscope_drawing, get_oscilloscope_drawing;
        Visualizer."oscilloscope-area" => set_oscilloscope_area, get_oscilloscope_area;
        Visualizer."hologram-drawing" => set_hologram_drawing, get_hologram_drawing;
        Visualizer."hologram-area" => set_hologram_area, get_hologram_area;
        Visualizer."showing" => set_showing, get_showing;
        Visualizer."fps" => set_fps, get_fps;

        App."version" => set_version, get_version;
        App."build-date" => set_build_date, get_build_date;
        App."theme" => set_theme, get_theme;
        App."activities" => set_activities, get_activities;
        App."log" => set_log, get_log;
        App."log-level" => set_log_level, get_log_level;
        App."equalizer-visible" => set_equalizer_visible, get_equalizer_visible;
        App."title-bar-height" => set_title_bar_height, get_title_bar_height;
        App."window-buttons-width" => set_window_buttons_width, get_window_buttons_width;
        App."view" => set_view, get_view;
        App."scroll" => set_scroll, get_scroll;
        App."restore-view" => set_restore_view, get_restore_view;
        App."restore-scroll" => set_restore_scroll, get_restore_scroll;
        App."restores" => set_restores, get_restores;
        App."can-go-back" => set_can_go_back, get_can_go_back;
        App."can-go-forward" => set_can_go_forward, get_can_go_forward;
        Updates."state" => set_state, get_state;
        Updates."current-version" => set_current_version, get_current_version;
        Updates."version" => set_version, get_version;
        Updates."notes" => set_notes, get_notes;
        Updates."progress" => set_progress, get_progress;
        Updates."error" => set_error, get_error;
        Updates."can-install" => set_can_install, get_can_install;
        LastFm."state" => set_state, get_state;
        LastFm."user" => set_user, get_user;
        LastFm."scrobbling" => set_scrobbling, get_scrobbling;
        LastFm."waiting" => set_waiting, get_waiting;
        LastFm."error" => set_error, get_error;
        Updates."check-automatically" => set_check_automatically, get_check_automatically;
        Server."remote" => set_remote, get_remote;
        Server."name" => set_name, get_name;
        Server."address" => set_address, get_address;
        Server."state" => set_state, get_state;
        Server."error" => set_error, get_error;
        Server."fingerprint" => set_fingerprint, get_fingerprint;
        Server."server-name" => set_server_name, get_server_name;
        Server."network" => set_network, get_network;
        Server."autostart" => set_autostart, get_autostart;
        Server."user" => set_user, get_user;
        Server."has-password" => set_has_password, get_has_password;
        Server."devices" => set_devices, get_devices;
        Server."password-error" => set_password_error, get_password_error;
        Server."password-waiting" => set_password_waiting, get_password_waiting;
        Server."finding" => set_finding, get_finding;
        Server."found" => set_found, get_found;
        Server."logging-in" => set_logging_in, get_logging_in;
        Server."login-address" => set_login_address, get_login_address;
        Server."login-fingerprint" => set_login_fingerprint, get_login_fingerprint;
        Server."login-busy" => set_login_busy, get_login_busy;
        Server."login-waiting" => set_login_waiting, get_login_waiting;
        Server."login-verified" => set_login_verified, get_login_verified;
        Server."connection-code" => set_connection_code, get_connection_code;
        Server."connection-code-until" => set_connection_code_until, get_connection_code_until;
        Server."connection-code-error" => set_connection_code_error, get_connection_code_error;
        Server."login-error" => set_login_error, get_login_error;
        Server."login-kind" => set_login_kind, get_login_kind;
        Server."login-user" => set_login_user, get_login_user;
        Server."admin" => set_admin, get_admin;
        Server."users" => set_users, get_users;
        Server."invite-code" => set_invite_code, get_invite_code;
        Server."invite-for" => set_invite_for, get_invite_for;
        Server."invite-until" => set_invite_until, get_invite_until;
        Server."invite-error" => set_invite_error, get_invite_error;
        Server."browsing" => set_browsing, get_browsing;
        Server."browse-folder" => set_browse_folder, get_browse_folder;
        Server."browse-folders" => set_browse_folders, get_browse_folders;
    }
    callbacks {
        Player."play" => on_play, invoke_play();
        Player."pause" => on_pause, invoke_pause();
        Player."toggle-pause" => on_toggle_pause, invoke_toggle_pause();
        Player."stop" => on_stop, invoke_stop();
        Player."next" => on_next, invoke_next();
        Player."previous" => on_previous, invoke_previous();
        Player."seek" => on_seek, invoke_seek(fraction: f32);
        Player."seek-by" => on_seek_by, invoke_seek_by(seconds: f32);
        Player."set-volume" => on_set_volume, invoke_set_volume(volume: f32);
        Player."open-files" => on_open_files, invoke_open_files();

        Playlist."activate" => on_activate, invoke_activate(row: i32);
        Playlist."select" => on_select, invoke_select(row: i32, extend: bool, toggle: bool);
        Playlist."move-cursor" => on_move_cursor, invoke_move_cursor(delta: i32, extend: bool);
        Playlist."select-all" => on_select_all, invoke_select_all();
        Playlist."clear-selection" => on_clear_selection, invoke_clear_selection();
        Playlist."remove-selected" => on_remove_selected, invoke_remove_selected();
        Playlist."move-selected" => on_move_selected, invoke_move_selected(to: i32);
        Playlist."clear" => on_clear, invoke_clear();
        Playlist."add-files" => on_add_files, invoke_add_files();
        Playlist."add-folder" => on_add_folder, invoke_add_folder();
        Playlist."add-url" => on_add_url, invoke_add_url(url: SharedString);
        Playlist."save" => on_save, invoke_save();
        Playlist."load" => on_load, invoke_load();
        Playlist."toggle-shuffle" => on_toggle_shuffle, invoke_toggle_shuffle();
        Playlist."toggle-repeat" => on_toggle_repeat, invoke_toggle_repeat();

        Library."search" => on_search, invoke_search(text: SharedString);
        Library."activate" => on_activate, invoke_activate(row: i32);
        Library."play" => on_play, invoke_play(row: i32);
        Library."enqueue" => on_enqueue, invoke_enqueue(row: i32);
        Library."play-all" => on_play_all, invoke_play_all();
        Library."enqueue-all" => on_enqueue_all, invoke_enqueue_all();
        Library."show" => on_show, invoke_show(section: SharedString);
        Library."cover" => on_cover, invoke_cover(key: SharedString, version: i32) -> Image;

        Playlists."show" => on_show, invoke_show(index: i32);
        Playlists."create" => on_create, invoke_create(name: SharedString);
        Playlists."save-queue" => on_save_queue, invoke_save_queue(name: SharedString);
        Playlists."rename" => on_rename, invoke_rename(name: SharedString);
        Playlists."delete" => on_delete, invoke_delete();
        Playlists."play" => on_play, invoke_play(row: i32);
        Playlists."enqueue" => on_enqueue, invoke_enqueue();
        Playlists."remove" => on_remove, invoke_remove(row: i32);
        Playlists."add-queue-selection" => on_add_queue_selection, invoke_add_queue_selection(index: i32);
        Playlists."add-library-row" => on_add_library_row, invoke_add_library_row(index: i32, row: i32);
        Playlists."shuffle" => on_shuffle, invoke_shuffle();
        Playlists."set-sort" => on_set_sort, invoke_set_sort(sort: SharedString);
        Playlists."edit" => on_edit, invoke_edit();
        Playlists."pick-picture" => on_pick_picture, invoke_pick_picture(kind: SharedString);
        Playlists."clear-picture" => on_clear_picture, invoke_clear_picture(kind: SharedString);
        Playlists."save-edit" => on_save_edit, invoke_save_edit(name: SharedString, description: SharedString);
        Playlists."cancel-edit" => on_cancel_edit, invoke_cancel_edit();
        Playlists."toggle-favorite" => on_toggle_favorite, invoke_toggle_favorite();

        Radio."play" => on_play, invoke_play(row: i32);
        Radio."add" => on_add, invoke_add(name: SharedString, url: SharedString);
        Radio."remove" => on_remove, invoke_remove(row: i32);
        Library."back" => on_back, invoke_back();
        Library."set-album-sort" => on_set_album_sort, invoke_set_album_sort(sort: SharedString);
        Library."shuffle-all" => on_shuffle_all, invoke_shuffle_all();
        Library."play-popular" => on_play_popular, invoke_play_popular(index: i32);
        Library."more-popular" => on_more_popular, invoke_more_popular();
        Library."set-artist-bios-online" => on_set_artist_bios_online, invoke_set_artist_bios_online(online: bool);
        Library."set-artist-wikipedia-online" => on_set_artist_wikipedia_online, invoke_set_artist_wikipedia_online(online: bool);
        Library."set-fanart-key" => on_set_fanart_key, invoke_set_fanart_key(key: SharedString);
        Library."edit-artist" => on_edit_artist, invoke_edit_artist();
        Library."save-artist" => on_save_artist, invoke_save_artist(name: SharedString, sort_name: SharedString, mbid: SharedString, biography: SharedString, header: SharedString);
        Library."cancel-artist" => on_cancel_artist, invoke_cancel_artist();
        Library."edit-genre" => on_edit_genre, invoke_edit_genre();
        Library."pick-genre-picture" => on_pick_genre_picture, invoke_pick_genre_picture(kind: SharedString);
        Library."clear-genre-picture" => on_clear_genre_picture, invoke_clear_genre_picture(kind: SharedString);
        Library."save-genre" => on_save_genre, invoke_save_genre(description: SharedString);
        Library."cancel-genre" => on_cancel_genre, invoke_cancel_genre();
        Library."choose-picture" => on_choose_picture, invoke_choose_picture(kind: SharedString, index: i32);
        Library."pick-picture-file" => on_pick_picture_file, invoke_pick_picture_file(kind: SharedString);
        Library."picture-from-url" => on_picture_from_url, invoke_picture_from_url(kind: SharedString, url: SharedString);
        Library."match-artist" => on_match_artist, invoke_match_artist();
        Library."find-matches" => on_find_matches, invoke_find_matches(query: SharedString);
        Library."save-artist-match" => on_save_artist_match, invoke_save_artist_match(mbid: SharedString);
        Library."cancel-artist-match" => on_cancel_artist_match, invoke_cancel_artist_match();
        Library."match-album" => on_match_album, invoke_match_album();
        Library."find-album-matches" => on_find_album_matches, invoke_find_album_matches(title: SharedString, artist: SharedString);
        Library."save-album-match" => on_save_album_match, invoke_save_album_match(release_group: SharedString);
        Library."cancel-album-match" => on_cancel_album_match, invoke_cancel_album_match();
        Library."set-artist-sort" => on_set_artist_sort, invoke_set_artist_sort(sort: SharedString);
        Library."set-artist-view" => on_set_artist_view, invoke_set_artist_view(view: SharedString);
        Library."set-artist-pictures-online" => on_set_artist_pictures_online, invoke_set_artist_pictures_online(online: bool);
        Library."set-watch-folders" => on_set_watch_folders, invoke_set_watch_folders(watch: bool);
        Library."set-albums-sort" => on_set_albums_sort, invoke_set_albums_sort(sort: SharedString);
        Library."set-playlists-sort" => on_set_playlists_sort, invoke_set_playlists_sort(sort: SharedString);
        Library."toggle-favorite" => on_toggle_favorite, invoke_toggle_favorite(row: i32);
        Library."toggle-album-favorite" => on_toggle_album_favorite, invoke_toggle_album_favorite();
        Library."show-album-cover" => on_show_album_cover, invoke_show_album_cover();
        Library."set-search-filter" => on_set_search_filter, invoke_set_search_filter(filter: SharedString);
        Library."suggest" => on_suggest, invoke_suggest(text: SharedString);
        Library."open-suggestion" => on_open_suggestion, invoke_open_suggestion(index: i32);
        Library."set-scan-every-hours" => on_set_scan_every_hours, invoke_set_scan_every_hours(hours: i32);
        Library."set-scan-at-start" => on_set_scan_at_start, invoke_set_scan_at_start(scan: bool);
        Library."open-album-artist" => on_open_album_artist, invoke_open_album_artist();
        Library."open-playing-artist" => on_open_playing_artist, invoke_open_playing_artist();
        Library."open-playing-album" => on_open_playing_album, invoke_open_playing_album();
        Library."set-album-view" => on_set_album_view, invoke_set_album_view(view: SharedString);
        Library."add-folder" => on_add_folder, invoke_add_folder();
        Library."remove-folder" => on_remove_folder, invoke_remove_folder(index: i32);
        Library."edit-folders" => on_edit_folders, invoke_edit_folders();
        Library."add-draft-folder" => on_add_draft_folder, invoke_add_draft_folder();
        Library."remove-draft-folder" => on_remove_draft_folder, invoke_remove_draft_folder(index: i32);
        Library."save-folders" => on_save_folders, invoke_save_folders();
        Library."rescan" => on_rescan, invoke_rescan();
        Library."rescan-files" => on_rescan_files, invoke_rescan_files();
        Library."scan-one-folder" => on_scan_one_folder, invoke_scan_one_folder(index: i32);
        Library."stop-scan" => on_stop_scan, invoke_stop_scan();

        Equalizer."curve" => on_curve, invoke_curve(width: f32, height: f32, bands: ModelRc<f32>) -> SharedString;
        Equalizer."toggle" => on_toggle, invoke_toggle();
        Equalizer."set-preamp" => on_set_preamp, invoke_set_preamp(db: f32);
        Equalizer."set-band" => on_set_band, invoke_set_band(band: i32, db: f32);
        Equalizer."load-preset" => on_load_preset, invoke_load_preset(index: i32);
        Equalizer."reset" => on_reset, invoke_reset();
        Equalizer."load-file" => on_load_file, invoke_load_file();
        Equalizer."save-file" => on_save_file, invoke_save_file();

        Visualizer."cycle-mode" => on_cycle_mode, invoke_cycle_mode();
        Visualizer."set-mode" => on_set_mode, invoke_set_mode(mode: SharedString);
        Visualizer."set-fps" => on_set_fps, invoke_set_fps(fps: i32);
        Visualizer."wave" => on_wave, invoke_wave(width: f32, height: f32, samples: ModelRc<f32>) -> SharedString;
        Visualizer."bars-path" => on_bars_path, invoke_bars_path(width: f32, height: f32, bars: ModelRc<f32>) -> SharedString;
        Visualizer."peaks-path" => on_peaks_path, invoke_peaks_path(width: f32, height: f32, peaks: ModelRc<f32>, thickness: f32) -> SharedString;

        App."show-about" => on_show_about, invoke_show_about();
        App."set-log-level" => on_set_log_level, invoke_set_log_level(level: SharedString);
        App."show-skins" => on_show_skins, invoke_show_skins();
        App."set-theme" => on_set_theme, invoke_set_theme(theme: SharedString);
        App."toggle-equalizer" => on_toggle_equalizer, invoke_toggle_equalizer();
        App."drag-window" => on_drag_window, invoke_drag_window();
        App."open-link" => on_open_link, invoke_open_link(name: SharedString);
        App."navigated" => on_navigated, invoke_navigated();
        App."go-back" => on_go_back, invoke_go_back();
        App."go-forward" => on_go_forward, invoke_go_forward();
        Updates."check" => on_check, invoke_check();
        Updates."install" => on_install, invoke_install();
        Updates."later" => on_later, invoke_later();
        Updates."skip" => on_skip, invoke_skip();
        Updates."set-check-automatically" => on_set_check_automatically, invoke_set_check_automatically(on: bool);
        LastFm."connect" => on_connect, invoke_connect();
        LastFm."cancel" => on_cancel, invoke_cancel();
        LastFm."disconnect" => on_disconnect, invoke_disconnect();
        LastFm."set-scrobbling" => on_set_scrobbling, invoke_set_scrobbling(on: bool);
        Server."find" => on_find, invoke_find();
        Server."choose-found" => on_choose_found, invoke_choose_found(index: i32);
        Server."choose-address" => on_choose_address, invoke_choose_address(address: SharedString);
        Server."log-in" => on_log_in, invoke_log_in();
        Server."cancel-login" => on_cancel_login, invoke_cancel_login();
        Server."use-this-computer" => on_use_this_computer, invoke_use_this_computer();
        Server."change-password" => on_change_password, invoke_change_password();
        Server."revoke" => on_revoke, invoke_revoke(id: SharedString);
        Server."log-out" => on_log_out, invoke_log_out();
        Server."invite-user" => on_invite_user, invoke_invite_user(name: SharedString, admin: bool);
        Server."invite-again" => on_invite_again, invoke_invite_again(name: SharedString);
        Server."remove-user" => on_remove_user, invoke_remove_user(name: SharedString);
        Server."hide-invite-code" => on_hide_invite_code, invoke_hide_invite_code();
        Server."verify-code" => on_verify_code, invoke_verify_code(code: SharedString);
        Server."show-connection-code" => on_show_connection_code, invoke_show_connection_code();
        Server."set-network" => on_set_network, invoke_set_network(on: bool);
        Server."set-server-name" => on_set_server_name, invoke_set_server_name(name: SharedString);
        Server."set-autostart" => on_set_autostart, invoke_set_autostart(on: bool);
        Server."browse-start" => on_browse_start, invoke_browse_start();
        Server."browse-into" => on_browse_into, invoke_browse_into(index: i32);
        Server."browse-up" => on_browse_up, invoke_browse_up();
        Server."browse-add" => on_browse_add, invoke_browse_add();
        Server."browse-cancel" => on_browse_cancel, invoke_browse_cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table above must list exactly what the skin API has, or the compiled skin would
    /// behave differently from the same skin read by the interpreter.
    #[test]
    fn the_table_matches_the_skin_api() {
        let definition =
            freya_skin::compile(&freya_skin::SkinSource::Builtin).expect("built-in skin");
        let mut api_properties = Vec::new();
        let mut api_callbacks = Vec::new();
        for global in definition.globals() {
            for (name, _) in definition.global_properties(&global).into_iter().flatten() {
                api_properties.push((global.clone(), name));
            }
            for name in definition.global_callbacks(&global).into_iter().flatten() {
                api_callbacks.push((global.clone(), name));
            }
        }
        let sorted = |mut v: Vec<(String, String)>| {
            v.sort();
            v
        };
        let table = |entries: &[(&str, &str)]| {
            entries
                .iter()
                .map(|(g, n)| (g.to_string(), n.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(sorted(table(PROPERTIES)), sorted(api_properties));
        assert_eq!(sorted(table(CALLBACKS)), sorted(api_callbacks));
        let mut globals: Vec<String> = definition.globals().collect();
        globals.sort();
        let mut ours: Vec<String> = GLOBALS.iter().map(|g| g.to_string()).collect();
        ours.sort();
        assert_eq!(ours, globals);
    }
}
