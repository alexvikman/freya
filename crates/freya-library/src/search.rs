//! How well a name matches what the user typed, as a search ranks it: the whole name first,
//! then a name that starts with it, then one with words that each start as a word of the
//! name. Letters are compared without case or accents, so "beyonce" finds "Beyoncé" and "asa"
//! finds "Åsa".

/// How well a name matches; lower is better.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rank {
    /// The whole name.
    Exact,
    /// The start of the name.
    Start,
    /// Each word typed starts a word of the name.
    Words,
}

/// How well `name` matches `query`, already folded with [`fold`]; none when it does not.
pub fn rank(name: &str, query: &str) -> Option<Rank> {
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let name = fold(name);
    let query = words.join(" ");
    if name == query {
        return Some(Rank::Exact);
    }
    if name.starts_with(&query) {
        return Some(Rank::Start);
    }
    let name_words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    words
        .iter()
        .all(|word| {
            let word = word.trim_matches(|c: char| !c.is_alphanumeric());
            word.is_empty() || name_words.iter().any(|w| w.starts_with(word))
        })
        .then_some(Rank::Words)
}

/// `text` without case and without accents on Latin letters, for comparing.
pub fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
            'ç' | 'ć' | 'č' => 'c',
            'ď' | 'đ' => 'd',
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => 'e',
            'ğ' => 'g',
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => 'i',
            'ł' | 'ľ' | 'ĺ' => 'l',
            'ñ' | 'ń' | 'ň' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => 'o',
            'ŕ' | 'ř' => 'r',
            'ś' | 'š' | 'ş' | 'ș' => 's',
            'ť' | 'ţ' | 'ț' => 't',
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => 'u',
            'ý' | 'ÿ' => 'y',
            'ź' | 'ż' | 'ž' => 'z',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_the_whole_name_then_its_start_then_its_words() {
        let q = |text: &str| fold(text);
        assert_eq!(
            rank("Anna Ternheim", &q("anna ternheim")),
            Some(Rank::Exact)
        );
        assert_eq!(rank("Anna Ternheim", &q("Anna Te")), Some(Rank::Start));
        assert_eq!(rank("Anna Ternheim", &q("ternheim")), Some(Rank::Words));
        assert_eq!(rank("Anna Ternheim", &q("tern ann")), Some(Rank::Words));
        assert_eq!(
            rank("Anna Ternheim", &q("heim")),
            None,
            "not the start of a word"
        );
        assert_eq!(rank("Beyoncé", &q("beyonce")), Some(Rank::Exact));
        assert_eq!(rank("Åsa Jinder", &q("asa")), Some(Rank::Start));
        assert_eq!(rank("AC/DC", &q("dc")), Some(Rank::Words));
        assert_eq!(rank("Anything", &q("  ")), None);
        assert!(Rank::Exact < Rank::Start && Rank::Start < Rank::Words);
    }
}
