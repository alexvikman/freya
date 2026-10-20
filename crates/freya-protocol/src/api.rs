//! What a client can ask of the library ([`LibraryApi`]) and of the server that keeps it
//! ([`ServerControl`]).

use std::path::Path;
use std::sync::mpsc::Receiver;

use crate::ServiceEvent;

/// Why the library could not do what it was asked.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ApiError(pub String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ApiError {}

pub type ApiResult<T> = Result<T, ApiError>;

/// Calls `$then!` with what is in the braces and every call of [`LibraryData`]: each as the
/// library's own method of the name takes it. The one list the trait, and what serves and
/// calls it, are made from.
#[macro_export]
macro_rules! with_library_data {
    ($then:ident { $($pre:tt)* }) => {
        $then! {
            $($pre)*
            /// Totals of the whole library.
            fn stats() -> $crate::Stats;
            fn artists() -> Vec<$crate::Artist>;
            fn artists_by(order: $crate::ArtistOrder) -> Vec<$crate::Artist>;
            /// The albums of `artist`, every album when none.
            fn albums(artist: Option<&str>) -> Vec<$crate::Album>;
            fn albums_by(order: $crate::AlbumOrder) -> Vec<$crate::Album>;
            fn all_albums() -> Vec<$crate::Album>;
            /// The track of the file at `path` that starts at `start`.
            fn track_at(path: &::std::path::Path, start: ::std::time::Duration) -> Option<$crate::Track>;
            fn album_tracks(artist: Option<&str>, album: Option<&str>) -> Vec<$crate::Track>;
            fn artist_tracks(artist: Option<&str>) -> Vec<$crate::Track>;
            fn genres() -> Vec<$crate::Genre>;
            fn genre_albums(genre: Option<&str>) -> Vec<$crate::Album>;
            fn genre_tracks(genre: Option<&str>) -> Vec<$crate::Track>;
            fn all_tracks() -> Vec<$crate::Track>;
            fn recently_added(limit: usize) -> Vec<$crate::Album>;
            fn recently_played(limit: usize) -> Vec<$crate::Played>;
            fn most_played(limit: usize) -> Vec<$crate::Played>;
            fn search(text: &str, limit: usize) -> Vec<$crate::Track>;
            /// How often each of an album's tracks was played, by its file and start.
            fn album_plays(
                artist: Option<&str>,
                album: Option<&str>,
            ) -> Vec<((::std::path::PathBuf, Option<::std::time::Duration>), u32)>;
            /// Each album's cover as the last scan made it, by the album's key.
            fn album_covers() -> ::std::collections::HashMap<String, $crate::AlbumCover>;
            fn artist_mbid(artist: Option<&str>) -> Option<String>;
            fn artist_summary(artist: Option<&str>, genres: usize) -> $crate::ArtistSummary;
            fn artist_popular(artist: Option<&str>, limit: usize) -> Vec<$crate::Played>;
            fn genre_popular(genre: &str, limit: usize) -> Vec<$crate::Played>;
            fn artist_edit(artist: &str) -> $crate::ArtistEdit;
            fn artist_edits() -> ::std::collections::HashMap<String, $crate::ArtistEdit>;
            fn set_artist_edit(artist: &str, edit: &$crate::ArtistEdit) -> ();
            fn set_album_favorite(
                artist: Option<&str>,
                album: Option<&str>,
                favorite: bool,
                at: ::std::time::SystemTime,
            ) -> ();
            /// Each favourite album, and when it was made one.
            fn album_favorites() -> Vec<($crate::AlbumKey, ::std::time::SystemTime)>;
            fn set_artist_favorite(artist: &str, favorite: bool, at: ::std::time::SystemTime) -> ();
            fn artist_favorites() -> ::std::collections::HashMap<String, ::std::time::SystemTime>;
            fn set_playlist_favorite(playlist: &str, favorite: bool, at: ::std::time::SystemTime) -> ();
            fn playlist_favorites() -> ::std::collections::HashMap<String, ::std::time::SystemTime>;
            fn genre_infos() -> ::std::collections::HashMap<String, $crate::GenreInfo>;
            fn set_genre_info(genre: &str, info: &$crate::GenreInfo) -> ();
            fn genre_article(genre: &str) -> Option<$crate::Asked<Option<$crate::GenreArticle>>>;
            fn match_album(album: &$crate::AlbumMatch) -> ();
            fn unmatch_album(album_artist: &str, album: &str) -> ();
            fn album_match(artist: &str, album: &str) -> Option<$crate::AlbumMatch>;
            fn artist_picture(artist: &str, kind: $crate::PictureKind) -> Option<$crate::PictureRow>;
            /// Each artist's picture of `kind`, by the artist.
            fn artist_pictures(kind: $crate::PictureKind) -> ::std::collections::HashMap<String, String>;
            fn set_artist_picture(
                artist: &str,
                kind: $crate::PictureKind,
                source: &str,
                picture: Option<&str>,
                asked: ::std::time::SystemTime,
            ) -> ();
            fn forget_artist_picture(artist: &str, kind: $crate::PictureKind) -> ();
            fn searched_artist_id(artist: &str) -> Option<$crate::Asked<Option<String>>>;
            fn artist_text(artist: &str) -> Option<$crate::Asked<$crate::ArtistText>>;
            fn artist_article(artist: &str) -> Option<(String, $crate::Asked<Option<$crate::Article>>)>;
            fn forget_artist_text(artist: &str) -> ();
            fn playlist_info(playlist: &str) -> $crate::PlaylistInfo;
            fn set_playlist_info(playlist: &str, info: &$crate::PlaylistInfo) -> ();
            /// When each of a playlist's tracks was added to it.
            fn playlist_added(playlist: &str) -> Vec<($crate::PlaylistTrack, ::std::time::SystemTime)>;
            fn note_playlist_added(playlist: &str, tracks: &[$crate::PlaylistTrack], at: ::std::time::SystemTime) -> ();
        }
    };
}

/// Every call of [`ServerControl`]: what the server is asked to do, beyond the library.
macro_rules! with_server_control {
    ($then:ident { $($pre:tt)* }) => {
        $then! {
            $($pre)*
            /// How the server keeps the library, as the user set it.
            fn settings() -> $crate::ServerSettings;
            /// Keeps `settings` and works by them from now on: a service newly allowed is
            /// asked what artists lack.
            fn set_settings(settings: &$crate::ServerSettings) -> ();
            /// Brings the library in line with its folders.
            fn rescan() -> ();
            /// Reads every file again and makes every cover again.
            fn rescan_files() -> ();
            /// Looks through `folders`, within the library's, only.
            fn scan_only(folders: &[::std::path::PathBuf]) -> ();
            fn stop_scan() -> ();
            /// Asks the internet what `artists` lack, every artist when none, once no scan runs.
            fn fetch(artists: Option<&[String]>) -> ();
            /// Notes that the track at `path` was played at `at`; whether it is in the library
            /// yet, as a scan may be writing.
            fn record_play(
                path: &::std::path::Path,
                part: Option<$crate::Part>,
                at: ::std::time::SystemTime,
            ) -> bool;
            /// Writes the plays noted, whether a scan runs or not.
            fn save() -> ();
            fn is_scanning() -> bool;
            /// The user's playlists, by name.
            fn playlists() -> Vec<$crate::SavedPlaylist>;
            /// Keeps the playlist `name` with `entries`, made or written anew.
            fn save_playlist(name: &str, entries: &[$crate::PlaylistEntry]) -> ();
            /// Names the playlist `from` `to`, with what the library keeps of it.
            fn rename_saved_playlist(from: &str, to: &str) -> ();
            /// Takes the playlist `name` away, with what the library keeps of it.
            fn delete_playlist(name: &str) -> ();
        }
    };
}

/// A trait of calls, the dispatch that answers them, and a remote implementation that makes
/// them through a [`Transport`].
macro_rules! calls {
    (
        $(#[doc = $trait_doc:literal])*
        trait $trait_name:ident, $dispatch:ident
        $(
            $(#[doc = $doc:literal])*
            fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty;
        )*
    ) => {
        $(#[doc = $trait_doc])*
        pub trait $trait_name: Send + Sync {
            $(
                $(#[doc = $doc])*
                fn $name(&self, $($arg: $ty),*) -> ApiResult<$ret>;
            )*
        }

        /// Answers the call of `method` with `args`, as [`Remote`] sent them, from `api`; the
        /// arguments back when there is no such call.
        pub fn $dispatch(
            api: &dyn $trait_name,
            method: &str,
            args: serde_json::Value,
        ) -> Result<ApiResult<serde_json::Value>, serde_json::Value> {
            match method {
                $(
                    stringify!($name) => Ok((|| {
                        let ($($arg,)*): ($(<$ty as WireArg<'_>>::Owned,)*) =
                            serde_json::from_value(args).map_err(wire_error)?;
                        let answer = api.$name($(<$ty as WireArg<'_>>::lend(&$arg)),*)?;
                        serde_json::to_value(answer).map_err(wire_error)
                    })()),
                )*
                _ => Err(args),
            }
        }

        impl<T: Transport> $trait_name for Remote<T> {
            $(
                fn $name(&self, $($arg: $ty),*) -> ApiResult<$ret> {
                    let args = serde_json::to_value(($($arg,)*)).map_err(wire_error)?;
                    let answer = self.0.call(stringify!($name), args)?;
                    serde_json::from_value(answer).map_err(wire_error)
                }
            )*
        }
    };
}

with_library_data!(calls {
    /// What the library holds, and what the user changes in it.
    trait LibraryData, dispatch_data
});

with_server_control!(calls {
    /// What the server that keeps the library is asked to do.
    trait ServerControl, dispatch_control
});

/// How an argument of a call goes over the network: as its owned form, which the call is lent.
pub trait WireArg<'a> {
    type Owned: serde::Serialize + serde::de::DeserializeOwned;
    fn lend(owned: &'a Self::Owned) -> Self;
}

macro_rules! copied_args {
    ($($t:ty),*) => {$(
        impl<'a> WireArg<'a> for $t {
            type Owned = $t;
            fn lend(owned: &'a $t) -> Self {
                *owned
            }
        }
    )*};
}

copied_args!(
    usize,
    u32,
    bool,
    std::time::SystemTime,
    std::time::Duration,
    crate::PictureKind,
    crate::ArtistOrder,
    crate::AlbumOrder
);

macro_rules! lent_args {
    ($($t:ty),*) => {$(
        impl<'a> WireArg<'a> for &'a $t {
            type Owned = $t;
            fn lend(owned: &'a $t) -> Self {
                owned
            }
        }
    )*};
}

lent_args!(
    crate::ServerSettings,
    crate::ArtistEdit,
    crate::GenreInfo,
    crate::AlbumMatch,
    crate::PlaylistInfo
);

impl<'a> WireArg<'a> for &'a str {
    type Owned = String;
    fn lend(owned: &'a String) -> Self {
        owned
    }
}

impl<'a> WireArg<'a> for Option<&'a str> {
    type Owned = Option<String>;
    fn lend(owned: &'a Option<String>) -> Self {
        owned.as_deref()
    }
}

impl<'a> WireArg<'a> for &'a Path {
    type Owned = std::path::PathBuf;
    fn lend(owned: &'a std::path::PathBuf) -> Self {
        owned
    }
}

impl<'a> WireArg<'a> for &'a [std::path::PathBuf] {
    type Owned = Vec<std::path::PathBuf>;
    fn lend(owned: &'a Vec<std::path::PathBuf>) -> Self {
        owned
    }
}

impl<'a> WireArg<'a> for Option<&'a [String]> {
    type Owned = Option<Vec<String>>;
    fn lend(owned: &'a Option<Vec<String>>) -> Self {
        owned.as_deref()
    }
}

impl<'a> WireArg<'a> for Option<crate::Part> {
    type Owned = Option<crate::Part>;
    fn lend(owned: &'a Option<crate::Part>) -> Self {
        *owned
    }
}

impl<'a> WireArg<'a> for &'a [crate::PlaylistEntry] {
    type Owned = Vec<crate::PlaylistEntry>;
    fn lend(owned: &'a Vec<crate::PlaylistEntry>) -> Self {
        owned
    }
}

impl<'a> WireArg<'a> for &'a [crate::PlaylistTrack] {
    type Owned = Vec<crate::PlaylistTrack>;
    fn lend(owned: &'a Vec<crate::PlaylistTrack>) -> Self {
        owned
    }
}

fn wire_error(e: impl std::fmt::Display) -> ApiError {
    ApiError(format!("could not read what was sent: {e}"))
}

/// What carries a call to a library elsewhere and brings back its answer.
pub trait Transport: Send + Sync {
    fn call(&self, method: &str, args: serde_json::Value) -> ApiResult<serde_json::Value>;
    /// The picture kept as `checksum`.
    fn picture(&self, checksum: &str) -> ApiResult<Option<Vec<u8>>>;
    /// Keeps the picture in `bytes`, at most `largest` wide or high when given: its checksum.
    fn keep_picture(&self, bytes: &[u8], largest: Option<u32>) -> ApiResult<String>;
    fn album_art(&self, track: &Path, largest: u32) -> ApiResult<Option<Vec<u8>>>;
    /// What happens at the server from now on, as it happens.
    fn events(&self) -> ApiResult<Receiver<ServiceEvent>>;
}

impl<T: Transport> ServerEvents for Remote<T> {
    fn events(&self) -> ApiResult<Receiver<ServiceEvent>> {
        self.0.events()
    }
}

impl<T: Transport> LibraryPictures for Remote<T> {
    fn picture(&self, checksum: &str) -> ApiResult<Option<Vec<u8>>> {
        self.0.picture(checksum)
    }

    fn put_picture(&self, bytes: &[u8]) -> ApiResult<String> {
        self.0.keep_picture(bytes, None)
    }

    fn take_picture(&self, bytes: &[u8], largest: u32) -> ApiResult<String> {
        self.0.keep_picture(bytes, Some(largest))
    }

    fn album_art(&self, track: &Path, largest: u32) -> ApiResult<Option<Vec<u8>>> {
        self.0.album_art(track, largest)
    }
}

/// Whether `checksum` names a picture as Freya names them: 64 hex digits, its SHA-256. Nothing
/// else is looked up, so no name leads out of the pictures' folder.
pub fn is_checksum(checksum: &str) -> bool {
    checksum.len() == 64 && checksum.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Where the API is served, under the server's address.
pub const API_PATH: &str = "/api/v1";

/// A library elsewhere, called through `T`.
pub struct Remote<T>(pub T);

/// The library's pictures, and its music's covers.
pub trait LibraryPictures: Send + Sync {
    /// The picture kept as `checksum`, as it was kept.
    fn picture(&self, checksum: &str) -> ApiResult<Option<Vec<u8>>>;
    /// Keeps the picture in `bytes` as it is: its checksum.
    fn put_picture(&self, bytes: &[u8]) -> ApiResult<String>;
    /// Keeps the picture in `bytes` at most `largest` wide or high: its checksum.
    fn take_picture(&self, bytes: &[u8], largest: u32) -> ApiResult<String>;
    /// The cover of the album whose track `track` is, at most `largest` wide or high, read
    /// from the music's file or folder.
    fn album_art(&self, track: &Path, largest: u32) -> ApiResult<Option<Vec<u8>>>;
}

/// All a client can ask of the library.
pub trait LibraryApi: LibraryData + LibraryPictures {}

impl<T: LibraryData + LibraryPictures> LibraryApi for T {}

/// What happens at the server, as it happens.
pub trait ServerEvents: Send + Sync {
    /// From now on. The first is [`ServiceEvent::Connected`], which says whether a scan runs.
    fn events(&self) -> ApiResult<Receiver<ServiceEvent>>;
}

/// All a client can ask of the server, beyond the library.
pub trait Server: ServerControl + ServerEvents {}

impl<T: ServerControl + ServerEvents> Server for T {}
