//! Where a file belongs, by a pattern of its tags:
//! `{album_artist}/({original_year}) {album}[ ({edition})]/[{disc}-]{track} {title}`. A part in
//! square brackets goes when a field in it is empty.

#[cfg(test)]
use freya_protocol::SUGGESTED_PATTERN;
use freya_protocol::{TagField, TagMap};

use super::names::split_edition;

/// The longest a folder's or file's name is made, in bytes, its extension aside.
const MOST_BYTES: usize = 200;
/// Names Windows keeps for itself, whatever follows them.
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Why a pattern cannot be used, if so.
pub fn pattern_problem(pattern: &str) -> Option<String> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Some("No file name pattern is set.".to_owned());
    }
    if pattern.starts_with(['/', '\\']) || pattern.contains(':') {
        return Some(
            "A pattern is within a music folder: it starts with no / or drive.".to_owned(),
        );
    }
    let parts: Vec<&str> = pattern.split(['/', '\\']).collect();
    if parts
        .iter()
        .any(|part| matches!(part.trim(), "" | "." | ".."))
    {
        return Some("A pattern has no empty, . or .. folders.".to_owned());
    }
    let last = parts.last().copied().unwrap_or("");
    if !last.contains("{title}") && !last.contains("{track}") {
        return Some("A file's name needs {title} or {track}, to tell files apart.".to_owned());
    }
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            return Some("A { has no }.".to_owned());
        };
        let name = &rest[start + 1..start + end];
        if !KNOWN.contains(&name) {
            return Some(format!("There is no field {{{name}}}."));
        }
        rest = &rest[start + end + 1..];
    }
    None
}

const KNOWN: [&str; 10] = [
    "album_artist",
    "artist",
    "album",
    "title",
    "year",
    "original_year",
    "edition",
    "track",
    "disc",
    "genre",
];

fn first_four(date: Option<&String>) -> String {
    date.and_then(|date| date.get(..4))
        .filter(|year| year.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or("")
        .to_owned()
}

/// What a field of the pattern says for a file's `fields`; blank when they do not say.
fn value(name: &str, fields: &TagMap) -> String {
    let get = |field| fields.get(&field).map(|v: &String| v.trim()).unwrap_or("");
    match name {
        "artist" => get(TagField::Artist).to_owned(),
        "album_artist" => match get(TagField::AlbumArtist) {
            "" => get(TagField::Artist).to_owned(),
            artist => artist.to_owned(),
        },
        "album" => {
            let album = get(TagField::Album);
            // The edition has a place of its own: not said twice.
            if get(TagField::Edition).is_empty() {
                album.to_owned()
            } else {
                split_edition(album).0
            }
        }
        "title" => get(TagField::Title).to_owned(),
        "genre" => get(TagField::Genre).to_owned(),
        "edition" => get(TagField::Edition).to_owned(),
        "year" => first_four(fields.get(&TagField::Date)),
        "original_year" => match first_four(fields.get(&TagField::OriginalDate)) {
            year if year.is_empty() => first_four(fields.get(&TagField::Date)),
            year => year,
        },
        "track" => match get(TagField::Track).parse::<u32>() {
            Ok(number) => format!("{number:02}"),
            Err(_) => String::new(),
        },
        // Named only when there are several discs.
        "disc" => {
            let many = get(TagField::DiscTotal).parse::<u32>().is_ok_and(|n| n > 1)
                || get(TagField::Disc).parse::<u32>().is_ok_and(|n| n > 1);
            if many {
                get(TagField::Disc).to_owned()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

/// One part of a pattern filled in, or the field it lacks.
fn fill(part: &str, fields: &TagMap) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = part;
    while let Some(start) = rest.find(['{', '[']) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        if rest.starts_with('[') {
            let Some(end) = rest.find(']') else {
                out.push_str(rest);
                return Ok(out);
            };
            // Gone with the field that is empty.
            if let Ok(filled) = fill(&rest[1..end], fields) {
                out.push_str(&filled);
            }
            rest = &rest[end + 1..];
        } else {
            let Some(end) = rest.find('}') else {
                out.push_str(rest);
                return Ok(out);
            };
            let name = &rest[1..end];
            let filled = value(name, fields);
            if filled.is_empty() {
                return Err(name.to_owned());
            }
            out.push_str(&filled);
            rest = &rest[end + 1..];
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// A name every system can hold, and that a scan does not take for a hidden file's.
pub fn safe_name(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = replaced
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' ']);
    let mut end = trimmed.len().min(MOST_BYTES);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let cut = trimmed[..end].trim_end_matches(['.', ' ']);
    let stem = cut.split('.').next().unwrap_or(cut).to_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        format!("_{cut}")
    } else {
        cut.to_owned()
    }
}

/// Where a file with `fields` belongs under its music folder, by `pattern`, without its
/// extension; or what it lacks to have a place.
pub fn relative_path(pattern: &str, fields: &TagMap) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    for part in pattern.trim().split(['/', '\\']) {
        let filled = fill(part, fields).map_err(|field| format!("it has no {field}"))?;
        let safe = safe_name(&filled);
        if safe.is_empty() {
            return Err(format!("\"{part}\" comes to nothing for it"));
        }
        parts.push(safe);
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(TagField, &str)]) -> TagMap {
        pairs.iter().map(|(f, v)| (*f, (*v).to_owned())).collect()
    }

    fn puppets() -> TagMap {
        fields(&[
            (TagField::AlbumArtist, "Metallica"),
            (TagField::Album, "Master of Puppets"),
            (TagField::Edition, "2016 Remastered"),
            (TagField::Title, "Battery"),
            (TagField::Track, "1"),
            (TagField::Disc, "1"),
            (TagField::DiscTotal, "1"),
            (TagField::Date, "2016-04-15"),
            (TagField::OriginalDate, "1986-03-03"),
        ])
    }

    #[test]
    fn makes_the_users_own_folder_names() {
        assert_eq!(
            relative_path(SUGGESTED_PATTERN, &puppets()).unwrap(),
            [
                "Metallica",
                "(1986) Master of Puppets (2016 Remastered)",
                "01 Battery"
            ]
        );
    }

    #[test]
    fn a_part_in_brackets_goes_with_its_empty_field() {
        let mut album = puppets();
        album.remove(&TagField::Edition);
        assert_eq!(
            relative_path(SUGGESTED_PATTERN, &album).unwrap()[1],
            "(1986) Master of Puppets"
        );
    }

    #[test]
    fn a_disc_is_named_only_with_several() {
        let mut album = puppets();
        album.insert(TagField::Disc, "2".to_owned());
        album.insert(TagField::DiscTotal, "2".to_owned());
        assert_eq!(
            relative_path(SUGGESTED_PATTERN, &album).unwrap()[2],
            "2-01 Battery"
        );
    }

    #[test]
    fn an_album_says_its_edition_once() {
        let mut album = puppets();
        album.insert(TagField::Album, "Master of Puppets (Remastered)".to_owned());
        assert_eq!(
            relative_path(SUGGESTED_PATTERN, &album).unwrap()[1],
            "(1986) Master of Puppets (2016 Remastered)"
        );
    }

    #[test]
    fn says_what_a_file_lacks() {
        let mut album = puppets();
        album.remove(&TagField::Title);
        assert_eq!(
            relative_path(SUGGESTED_PATTERN, &album),
            Err("it has no title".to_owned())
        );
    }

    #[test]
    fn names_are_safe_on_every_system() {
        assert_eq!(safe_name("AC/DC"), "AC_DC");
        assert_eq!(safe_name("What?: Now"), "What__ Now");
        assert_eq!(safe_name("...And Justice For All"), "And Justice For All");
        assert_eq!(safe_name("Album."), "Album");
        assert_eq!(safe_name("CON"), "_CON");
        assert_eq!(safe_name("con.mp3"), "_con.mp3");
        assert_eq!(safe_name(&"å".repeat(300)).len(), 200);
    }

    #[test]
    fn a_field_cannot_leave_the_music_folder() {
        let mut album = puppets();
        album.insert(TagField::AlbumArtist, "../../etc".to_owned());
        let parts = relative_path("{album_artist}/{title}", &album).unwrap();
        assert_eq!(parts, ["_.._etc", "Battery"]);
    }

    #[test]
    fn a_bad_pattern_is_refused() {
        for bad in [
            "",
            "/abs/{title}",
            "C:/{title}",
            "../{title}",
            "{album}//{title}",
            "{album}",
            "{title} {nope}",
            "{title",
        ] {
            assert!(pattern_problem(bad).is_some(), "{bad}");
        }
        assert_eq!(pattern_problem(SUGGESTED_PATTERN), None);
    }
}
