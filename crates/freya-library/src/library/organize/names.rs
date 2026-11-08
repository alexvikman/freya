//! What the names of folders and albums say: the year, the title, and the edition.

use std::path::{Path, PathBuf};

use freya_protocol::FolderName;

/// Words that make a part of a title an edition's and not the music's name.
const EDITION_WORDS: [&str; 16] = [
    "remaster",
    "remastered",
    "remasters",
    "deluxe",
    "expanded",
    "anniversary",
    "edition",
    "reissue",
    "reissued",
    "bonus",
    "special",
    "collector",
    "collectors",
    "limited",
    "mono",
    "stereo",
];

/// The folder a music file's album is: the one it is in, or above that when it is a disc's
/// folder, "CD1" or "Disc 2".
pub fn album_folder(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or(file);
    let is_disc = parent
        .file_name()
        .is_some_and(|name| is_disc_name(&name.to_string_lossy()));
    match parent.parent() {
        Some(above) if is_disc => above.to_path_buf(),
        _ => parent.to_path_buf(),
    }
}

fn is_disc_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    let rest = ["cd", "disc", "disk"]
        .iter()
        .find_map(|word| lower.strip_prefix(word));
    let Some(rest) = rest else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '-', '_', '.']);
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    digits > 0
        && rest[digits..]
            .trim()
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric())
}

/// What a folder's name says: `(1986) Master Of Puppets (2016 Remastered)`,
/// `1986 - Master Of Puppets`, `Metallica - 1986 - Master Of Puppets`.
pub fn parse_folder_name(name: &str) -> FolderName {
    let mut rest = name.trim();
    let mut year = None;
    if let Some((found, after)) = leading_year(rest) {
        year = Some(found);
        rest = after;
    } else if let Some((_, after)) = rest.split_once(" - ")
        && let Some((found, after)) = leading_year(after)
    {
        year = Some(found);
        rest = after;
    }
    let (title, edition) = split_edition(rest);
    FolderName {
        year,
        title,
        edition,
    }
}

/// The year a name starts with, in brackets or followed by a dash, and what comes after it.
fn leading_year(name: &str) -> Option<(u32, &str)> {
    let (digits, after) = match name.chars().next()? {
        open @ ('(' | '[') => {
            let close = if open == '(' { ')' } else { ']' };
            let inner = name[1..].split_once(close)?;
            (inner.0, inner.1)
        }
        _ => {
            let (digits, after) = name.split_once(' ')?;
            let after = after.trim_start();
            let after = after
                .strip_prefix("- ")
                .or_else(|| after.strip_prefix("– "))?;
            (digits, after)
        }
    };
    let year: u32 = digits.trim().parse().ok()?;
    (digits.trim().len() == 4 && (1000..=2999).contains(&year)).then(|| (year, after.trim()))
}

/// A title without the edition its end names, and the edition: `Master Of Puppets` and
/// `2016 Remastered` of `Master Of Puppets (2016 Remastered)`. A part that does not name an
/// edition, `(Live)`, is the title's.
pub fn split_edition(title: &str) -> (String, Option<String>) {
    let mut base = title.trim();
    let mut parts = Vec::new();
    while let Some((before, part)) = last_part(base) {
        if !names_edition(part) {
            break;
        }
        parts.push(part.trim().to_owned());
        base = before.trim_end();
    }
    if base.is_empty() || parts.is_empty() {
        return (title.trim().to_owned(), None);
    }
    parts.reverse();
    (base.to_owned(), Some(parts.join(", ")))
}

/// A title's last part, in brackets or after a dash, and what is before it.
fn last_part(title: &str) -> Option<(&str, &str)> {
    let trimmed = title.trim_end();
    for (open, close) in [('(', ')'), ('[', ']')] {
        if let Some(inner) = trimmed.strip_suffix(close)
            && let Some(at) = inner.rfind(open)
        {
            return Some((&trimmed[..at], &inner[at + 1..]));
        }
    }
    trimmed
        .rsplit_once(" - ")
        .or_else(|| trimmed.rsplit_once(" – "))
}

fn names_edition(part: &str) -> bool {
    part.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| EDITION_WORDS.contains(&word))
}

/// A title as compared with others: its letters and digits, in lower case, the word order kept.
pub fn comparable(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') && !out.is_empty() {
            out.push(' ');
        }
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_names_of_folders() {
        let name = parse_folder_name("(1986) Master Of Puppets (2016 Remastered)");
        assert_eq!(name.year, Some(1986));
        assert_eq!(name.title, "Master Of Puppets");
        assert_eq!(name.edition.as_deref(), Some("2016 Remastered"));

        let name = parse_folder_name("(1991) Metallica");
        assert_eq!((name.year, name.title.as_str()), (Some(1991), "Metallica"));
        assert_eq!(name.edition, None);

        let name = parse_folder_name("1986 - Master Of Puppets");
        assert_eq!(name.year, Some(1986));
        assert_eq!(name.title, "Master Of Puppets");

        let name = parse_folder_name("Metallica - 1996 - Load");
        assert_eq!((name.year, name.title.as_str()), (Some(1996), "Load"));

        let name = parse_folder_name("Live Shit - Binge & Purge");
        assert_eq!(name.year, None);
        assert_eq!(name.title, "Live Shit - Binge & Purge");
    }

    #[test]
    fn tells_an_edition_from_the_title() {
        let split = |title| split_edition(title);
        assert_eq!(
            split("Hardwired...To Self-Destruct (Deluxe Edition)"),
            (
                "Hardwired...To Self-Destruct".to_owned(),
                Some("Deluxe Edition".to_owned())
            )
        );
        assert_eq!(
            split("Ride The Lightning [Remastered] (Deluxe)"),
            (
                "Ride The Lightning".to_owned(),
                Some("Remastered, Deluxe".to_owned())
            )
        );
        assert_eq!(
            split("Album - 2011 Remaster").1.as_deref(),
            Some("2011 Remaster")
        );
        assert_eq!(split("Alive (Live)"), ("Alive (Live)".to_owned(), None));
        assert_eq!(split("Stereo"), ("Stereo".to_owned(), None));
        assert_eq!(split("(Deluxe)"), ("(Deluxe)".to_owned(), None));
    }

    #[test]
    fn a_discs_folder_is_its_albums() {
        for disc in ["CD1", "cd 2", "Disc 1", "DISK-3", "Disc 1 - Studio"] {
            let file = PathBuf::from("/m/Artist/Album").join(disc).join("01.flac");
            assert_eq!(album_folder(&file), Path::new("/m/Artist/Album"), "{disc}");
        }
        for not in ["Album", "CD", "Discography", "Disco 1999"] {
            let file = PathBuf::from("/m/Artist").join(not).join("01.flac");
            assert_eq!(
                album_folder(&file),
                Path::new("/m/Artist").join(not),
                "{not}"
            );
        }
    }

    #[test]
    fn compares_titles_by_their_words() {
        assert_eq!(comparable("...And Justice For All"), "and justice for all");
        assert_eq!(comparable("St. Anger!"), "st anger");
    }
}
