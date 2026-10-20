//! What a client can ask of the library: [`LibraryApi`].

use std::path::Path;

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

/// Calls `$then!` with every call of [`LibraryData`]: each as the library's own method of the
/// name takes it. The one list the trait, and what serves and calls it, are made from.
#[macro_export]
macro_rules! with_library_data {
    ($then:ident) => {
        $then! {
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
            ) -> ::std::collections::HashMap<(::std::path::PathBuf, Option<::std::time::Duration>), u32>;
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
            fn album_favorites() -> ::std::collections::HashMap<$crate::AlbumKey, ::std::time::SystemTime>;
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
            fn rename_playlist(from: &str, to: &str) -> ();
            fn forget_playlist(playlist: &str) -> ();
            fn playlist_added(playlist: &str) -> ::std::collections::HashMap<$crate::PlaylistTrack, ::std::time::SystemTime>;
            fn note_playlist_added(playlist: &str, tracks: &[$crate::PlaylistTrack], at: ::std::time::SystemTime) -> ();
        }
    };
}

macro_rules! define_library_data {
    ($(
        $(#[doc = $doc:literal])*
        fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty;
    )*) => {
        /// What the library holds, and what the user changes in it.
        pub trait LibraryData: Send + Sync {
            $(
                $(#[doc = $doc])*
                fn $name(&self, $($arg: $ty),*) -> ApiResult<$ret>;
            )*
        }
    };
}

with_library_data!(define_library_data);

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
