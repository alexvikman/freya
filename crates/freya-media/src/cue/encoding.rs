//! Turning the bytes of a CUE sheet or a playlist into text. They are usually ASCII or UTF-8,
//! but older rippers and players wrote them in the Windows code page of the machine, mostly Western European
//! (Windows-1252) or Cyrillic (Windows-1251), and some write UTF-16.

/// Decodes a CUE sheet: UTF-8 or UTF-16 when it says so with a byte order mark, UTF-8 when it
/// is valid as such, and otherwise Windows-1251 or Windows-1252, whichever it looks like.
pub fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        return utf16(rest, u16::from_be_bytes);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    let high = if looks_cyrillic(bytes) {
        |b: u8| WINDOWS_1251[usize::from(b - 0x80)]
    } else {
        |b: u8| WINDOWS_1252[usize::from(b - 0x80)]
    };
    bytes
        .iter()
        .map(|&b| if b < 0x80 { char::from(b) } else { high(b) })
        .collect()
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units = bytes.as_chunks::<2>().0.iter().map(|&pair| unit(pair));
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Whether text in an 8-bit code page is more likely Cyrillic than Western European. In
/// Windows-1251 every Cyrillic letter is a byte from 0xC0 up, so whole words are runs of such
/// bytes. Western European text has its accented letters one at a time between ASCII ones.
fn looks_cyrillic(bytes: &[u8]) -> bool {
    let is_letter = |b: &u8| *b >= 0xC0;
    let letters = bytes.iter().filter(|b| is_letter(b)).count();
    let in_runs = bytes
        .windows(2)
        .filter(|pair| is_letter(&pair[0]) && is_letter(&pair[1]))
        .count();
    in_runs * 2 > letters
}

/// Windows-1252 from 0x80 up. Unassigned bytes keep their Latin-1 meaning.
const WINDOWS_1252: [char; 128] = {
    #[rustfmt::skip]
    let specials = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}',
        '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
    ];
    let mut table = ['\0'; 128];
    let mut i = 0;
    while i < 128 {
        table[i] = if i < specials.len() {
            specials[i]
        } else {
            // From 0xA0 up, Windows-1252 is Latin-1, which is Unicode's first 256 characters.
            char::from_u32(0x80 + i as u32).unwrap()
        };
        i += 1;
    }
    table
};

/// Windows-1251 from 0x80 up. The unassigned byte 0x98 keeps its Latin-1 meaning.
const WINDOWS_1251: [char; 128] = {
    #[rustfmt::skip]
    let specials = [
        'Ђ', 'Ѓ', '‚', 'ѓ', '„', '…', '†', '‡', '€', '‰', 'Љ', '‹', 'Њ', 'Ќ', 'Ћ', 'Џ',
        'ђ', '‘', '’', '“', '”', '•', '–', '—', '\u{98}', '™', 'љ', '›', 'њ', 'ќ', 'ћ', 'џ',
        '\u{A0}', 'Ў', 'ў', 'Ј', '¤', 'Ґ', '¦', '§', 'Ё', '©', 'Є', '«', '¬', '\u{AD}', '®', 'Ї',
        '°', '±', 'І', 'і', 'ґ', 'µ', '¶', '·', 'ё', '№', 'є', '»', 'ј', 'Ѕ', 'ѕ', 'ї',
    ];
    let mut table = ['\0'; 128];
    let mut i = 0;
    while i < 128 {
        table[i] = if i < specials.len() {
            specials[i]
        } else {
            // From 0xC0 up: А to я, in Unicode's order.
            char::from_u32(0x0410 + (i - specials.len()) as u32).unwrap()
        };
        i += 1;
    }
    table
};
