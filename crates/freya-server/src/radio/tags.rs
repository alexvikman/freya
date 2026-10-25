//! The directory's tags, made from what Radio Browser says of a station: its country, languages
//! and codec as they are, and its own tags, which anyone writes as they like ("Jazz", "smooth
//! jazz", "música jazz"), as the few genres, decades and formats Freya knows them by.

use std::collections::HashMap;

use freya_protocol::RadioTagKind;

/// Freya's genres: each tag's name after `prefix`, as shown, and how stations' own tags say it,
/// as [`clean`] leaves them.
const GENRES: &[(&str, &str, &[&str])] = &[
    (
        "pop",
        "Pop",
        &[
            "pop",
            "top 40",
            "top40",
            "top 40 hits",
            "hits",
            "top hits",
            "charts",
            "top charts",
            "contemporary hits",
            "chr",
            "adult contemporary",
            "hot adult contemporary",
            "adult hits",
            "top 100",
            "mainstream",
            "k pop",
            "kpop",
            "j pop",
            "jpop",
            "europop",
            "pop en espanol",
            "pop en ingles",
            "latin pop",
            "pop latino",
            "synthpop",
            "synth pop",
        ],
    ),
    (
        "rock",
        "Rock",
        &[
            "rock",
            "classic rock",
            "hard rock",
            "soft rock",
            "pop rock",
            "rock and roll",
            "rock n roll",
            "rocknroll",
            "progressive rock",
            "prog rock",
            "psychedelic rock",
            "southern rock",
            "glam rock",
            "rock classico",
            "rock clasico",
            "aor",
        ],
    ),
    (
        "alternative",
        "Alternative",
        &[
            "alternative",
            "alternative rock",
            "alt rock",
            "grunge",
            "post rock",
        ],
    ),
    (
        "indie",
        "Indie",
        &["indie", "indie rock", "indie pop", "indie folk", "shoegaze"],
    ),
    (
        "metal",
        "Metal",
        &[
            "metal",
            "heavy metal",
            "death metal",
            "black metal",
            "thrash metal",
            "power metal",
            "metalcore",
            "nu metal",
        ],
    ),
    (
        "punk",
        "Punk",
        &[
            "punk",
            "punk rock",
            "hardcore punk",
            "pop punk",
            "post punk",
        ],
    ),
    (
        "jazz",
        "Jazz",
        &[
            "jazz",
            "smooth jazz",
            "bebop",
            "swing",
            "big band",
            "acid jazz",
            "jazz fusion",
            "fusion",
            "nu jazz",
            "free jazz",
            "latin jazz",
            "vocal jazz",
            "jazz and blues",
        ],
    ),
    (
        "blues",
        "Blues",
        &["blues", "blues rock", "delta blues", "rhythm and blues"],
    ),
    (
        "soul",
        "Soul",
        &["soul", "motown", "northern soul", "neo soul"],
    ),
    (
        "rnb",
        "R&B",
        &[
            "rnb",
            "r and b",
            "randb",
            "r b",
            "urban",
            "urban contemporary",
        ],
    ),
    ("funk", "Funk", &["funk", "funky", "p funk"]),
    (
        "hip-hop",
        "Hip-hop",
        &[
            "hip hop",
            "hiphop",
            "rap",
            "trap",
            "hip hop and rnb",
            "hip hop rnb",
        ],
    ),
    (
        "reggae",
        "Reggae",
        &[
            "reggae",
            "dub",
            "ska",
            "dancehall",
            "roots reggae",
            "rocksteady",
        ],
    ),
    (
        "electronic",
        "Electronic",
        &[
            "electronic",
            "electronica",
            "electro",
            "edm",
            "electronic dance",
            "idm",
            "synthwave",
            "electro pop",
            "electropop",
        ],
    ),
    (
        "dance",
        "Dance",
        &[
            "dance",
            "eurodance",
            "club",
            "pop dance",
            "dance hits",
            "dance pop",
            "party",
            "club hits",
        ],
    ),
    (
        "house",
        "House",
        &[
            "house",
            "deep house",
            "tech house",
            "progressive house",
            "soulful house",
            "afro house",
        ],
    ),
    (
        "techno",
        "Techno",
        &[
            "techno",
            "minimal techno",
            "minimal",
            "hard techno",
            "detroit techno",
        ],
    ),
    (
        "trance",
        "Trance",
        &[
            "trance",
            "psytrance",
            "psy trance",
            "goa",
            "progressive trance",
            "uplifting trance",
        ],
    ),
    (
        "drum-and-bass",
        "Drum & bass",
        &[
            "drum and bass",
            "dnb",
            "drum n bass",
            "drumandbass",
            "jungle",
            "liquid funk",
        ],
    ),
    (
        "dubstep",
        "Dubstep",
        &[
            "dubstep",
            "bass",
            "uk garage",
            "garage",
            "breakbeat",
            "breaks",
        ],
    ),
    (
        "ambient",
        "Ambient",
        &[
            "ambient",
            "drone",
            "space",
            "dark ambient",
            "space ambient",
            "new age",
            "meditation",
        ],
    ),
    (
        "chillout",
        "Chillout",
        &[
            "chillout",
            "chill",
            "chill out",
            "downtempo",
            "relax",
            "relaxing",
            "relaxation",
            "trip hop",
            "triphop",
            "lofi",
            "lo fi",
            "chillhop",
            "chillwave",
        ],
    ),
    (
        "lounge",
        "Lounge",
        &["lounge", "bar", "cocktail", "nu lounge"],
    ),
    (
        "easy-listening",
        "Easy listening",
        &["easy listening", "easy", "beautiful", "soft"],
    ),
    (
        "classical",
        "Classical",
        &[
            "classical",
            "klassik",
            "klassisk",
            "klassiek",
            "classique",
            "clasica",
            "classica",
            "musique classique",
            "baroque",
            "barock",
            "symphony",
            "symphonic",
            "chamber",
            "orchestral",
            "early",
            "romantic era",
            "contemporary classical",
        ],
    ),
    ("opera", "Opera", &["opera", "oper", "lirica"]),
    (
        "soundtrack",
        "Soundtrack",
        &[
            "soundtrack",
            "soundtracks",
            "film",
            "movie",
            "movies",
            "film score",
            "score",
            "cinema",
            "video game",
            "anime",
        ],
    ),
    (
        "country",
        "Country",
        &[
            "country",
            "americana",
            "bluegrass",
            "country and western",
            "western",
            "new country",
        ],
    ),
    (
        "folk",
        "Folk",
        &[
            "folk",
            "singer songwriter",
            "acoustic",
            "folk rock",
            "celtic",
        ],
    ),
    (
        "world",
        "World",
        &[
            "world",
            "world music",
            "ethnic",
            "afrobeat",
            "afrobeats",
            "african",
            "balkan",
            "flamenco",
        ],
    ),
    (
        "latin",
        "Latin",
        &[
            "latin",
            "latino",
            "latina",
            "salsa",
            "cumbia",
            "reggaeton",
            "bachata",
            "merengue",
            "tropical",
            "bossa nova",
            "samba",
            "tango",
            "vallenato",
            "regional mexican",
            "regional mexicana",
            "grupera",
            "grupero",
            "banda",
            "norteno",
            "norteña",
            "ranchera",
            "mpb",
            "forro",
            "sertanejo",
        ],
    ),
    (
        "schlager",
        "Schlager",
        &["schlager", "volksmusik", "dansband", "volkstumliche"],
    ),
    (
        "chanson",
        "Chanson",
        &[
            "chanson",
            "chanson francaise",
            "variete",
            "variete francaise",
        ],
    ),
    (
        "oldies",
        "Oldies",
        &[
            "oldies",
            "classic hits",
            "golden oldies",
            "retro",
            "nostalgia",
            "evergreen",
        ],
    ),
    (
        "disco",
        "Disco",
        &["disco", "italo disco", "nu disco", "hi nrg"],
    ),
    (
        "gospel",
        "Gospel",
        &["gospel", "spirituals", "ccm", "christian rock", "worship"],
    ),
    (
        "christmas",
        "Christmas",
        &["christmas", "xmas", "weihnachten", "navidad", "noel", "jul"],
    ),
    (
        "experimental",
        "Experimental",
        &[
            "experimental",
            "avant garde",
            "avantgarde",
            "noise",
            "electroacoustic",
            "industrial",
        ],
    ),
    (
        "instrumental",
        "Instrumental",
        &["instrumental", "instrumentals"],
    ),
    (
        "eclectic",
        "Eclectic",
        &["eclectic", "freeform", "free form", "variety", "various"],
    ),
];

/// Formats: what a station is besides music.
const FORMATS: &[(&str, &str, &[&str])] = &[
    (
        "news",
        "News",
        &[
            "news",
            "noticias",
            "nachrichten",
            "nyheter",
            "nieuws",
            "notizie",
            "informations",
            "information",
            "info",
            "actualites",
            "local news",
            "news talk",
            "noticias locales",
            "informativa",
        ],
    ),
    (
        "talk",
        "Talk",
        &[
            "talk",
            "talk and speech",
            "speech",
            "spoken word",
            "hablada",
            "wort",
            "parlato",
            "comedy",
        ],
    ),
    (
        "sports",
        "Sports",
        &[
            "sports",
            "sport",
            "deportes",
            "football",
            "soccer",
            "sports news",
        ],
    ),
    (
        "religious",
        "Religious",
        &[
            "religious",
            "religion",
            "christian",
            "catholic",
            "islamic",
            "islam",
            "quran",
            "bible",
            "cristiana",
            "church",
            "orthodox",
        ],
    ),
    (
        "kids",
        "Kids",
        &["kids", "children", "childrens", "kinder", "infantil"],
    ),
    (
        "community",
        "Community",
        &[
            "community",
            "college",
            "university",
            "student",
            "non commercial",
            "noncommercial",
        ],
    ),
    (
        "public",
        "Public",
        &["public", "public service", "npr", "publica"],
    ),
];

/// The decades, by the name of their tags, and how stations say them.
const ERAS: &[(&str, &[&str])] = &[
    ("50s", &["50s", "1950s", "50er", "fifties"]),
    ("60s", &["60s", "1960s", "60er", "sixties"]),
    ("70s", &["70s", "1970s", "70er", "seventies"]),
    ("80s", &["80s", "1980s", "80er", "eighties"]),
    ("90s", &["90s", "1990s", "90er", "nineties"]),
    ("2000s", &["2000s", "00s", "00er", "2000er", "noughties"]),
    ("2010s", &["2010s", "10s", "2010er"]),
];

/// Languages by their ISO 639 codes, of either length, as stations give them.
const LANGUAGES: &[(&[&str], &str, &str)] = &[
    (&["en", "eng"], "en", "English"),
    (&["es", "spa"], "es", "Spanish"),
    (&["de", "deu", "ger", "gsw", "nds"], "de", "German"),
    (&["fr", "fra", "fre"], "fr", "French"),
    (&["zh", "zho", "chi", "cmn"], "zh", "Chinese"),
    (&["yue"], "yue", "Cantonese"),
    (&["ru", "rus"], "ru", "Russian"),
    (&["it", "ita"], "it", "Italian"),
    (&["el", "ell", "gre", "gr"], "el", "Greek"),
    (&["nl", "nld", "dut"], "nl", "Dutch"),
    (&["pt", "por"], "pt", "Portuguese"),
    (&["pl", "pol"], "pl", "Polish"),
    (&["ar", "ara"], "ar", "Arabic"),
    (&["hi", "hin"], "hi", "Hindi"),
    (&["ro", "ron", "rum"], "ro", "Romanian"),
    (&["tr", "tur"], "tr", "Turkish"),
    (&["sr", "srp"], "sr", "Serbian"),
    (&["ta", "tam"], "ta", "Tamil"),
    (&["hu", "hun"], "hu", "Hungarian"),
    (&["uk", "ukr"], "uk", "Ukrainian"),
    (&["id", "ind"], "id", "Indonesian"),
    (&["hr", "hrv"], "hr", "Croatian"),
    (&["bg", "bul"], "bg", "Bulgarian"),
    (&["cs", "ces", "cze", "cz"], "cs", "Czech"),
    (&["ne", "nep"], "ne", "Nepali"),
    (&["ja", "jpn"], "ja", "Japanese"),
    (&["sk", "slk", "slo"], "sk", "Slovak"),
    (&["sv", "swe"], "sv", "Swedish"),
    (&["ko", "kor"], "ko", "Korean"),
    (&["da", "dan"], "da", "Danish"),
    (&["ca", "cat"], "ca", "Catalan"),
    (&["bs", "bos"], "bs", "Bosnian"),
    (&["ml", "mal"], "ml", "Malayalam"),
    (&["fi", "fin"], "fi", "Finnish"),
    (&["lv", "lav"], "lv", "Latvian"),
    (&["sl", "slv"], "sl", "Slovenian"),
    (&["sq", "sqi", "alb"], "sq", "Albanian"),
    (&["th", "tha"], "th", "Thai"),
    (&["et", "est"], "et", "Estonian"),
    (&["he", "heb"], "he", "Hebrew"),
    (&["no", "nor", "nb", "nob", "nn", "nno"], "no", "Norwegian"),
    (&["ur", "urd"], "ur", "Urdu"),
    (&["lt", "lit"], "lt", "Lithuanian"),
    (&["sw", "swa"], "sw", "Swahili"),
    (&["mk", "mkd", "mac"], "mk", "Macedonian"),
    (&["si", "sin"], "si", "Sinhala"),
    (&["az", "aze"], "az", "Azerbaijani"),
    (&["fa", "fas", "per"], "fa", "Persian"),
    (&["ms", "msa", "may"], "ms", "Malay"),
    (&["te", "tel"], "te", "Telugu"),
    (&["be", "bel"], "be", "Belarusian"),
    (&["kn", "kan"], "kn", "Kannada"),
    (&["ku", "kur"], "ku", "Kurdish"),
    (&["kk", "kaz"], "kk", "Kazakh"),
    (&["af", "afr"], "af", "Afrikaans"),
    (&["eu", "eus", "baq"], "eu", "Basque"),
    (&["tl", "tgl", "fil"], "tl", "Filipino"),
    (&["mn", "mon"], "mn", "Mongolian"),
    (&["gl", "glg"], "gl", "Galician"),
    (&["ka", "kat", "geo"], "ka", "Georgian"),
    (&["bn", "ben"], "bn", "Bengali"),
    (&["hy", "hye", "arm"], "hy", "Armenian"),
    (&["is", "isl", "ice"], "is", "Icelandic"),
    (&["am", "amh"], "am", "Amharic"),
    (&["vi", "vie"], "vi", "Vietnamese"),
    (&["la", "lat"], "la", "Latin"),
    (&["ga", "gle"], "ga", "Irish"),
    (&["jv", "jav"], "jv", "Javanese"),
    (&["fo", "fao"], "fo", "Faroese"),
    (&["lb", "ltz"], "lb", "Luxembourgish"),
    (&["cy", "cym", "wel"], "cy", "Welsh"),
    (&["gd", "gla"], "gd", "Scottish Gaelic"),
    (&["br", "bre"], "br", "Breton"),
    (&["oc", "oci"], "oc", "Occitan"),
    (&["mt", "mlt"], "mt", "Maltese"),
    (&["yo", "yor"], "yo", "Yoruba"),
    (&["ha", "hau"], "ha", "Hausa"),
    (&["mr", "mar"], "mr", "Marathi"),
    (&["uz", "uzb"], "uz", "Uzbek"),
    (&["km", "khm"], "km", "Khmer"),
    (&["my", "mya", "bur"], "my", "Burmese"),
    (&["pa", "pan"], "pa", "Punjabi"),
    (&["gu", "guj"], "gu", "Gujarati"),
];

/// Countries' names as shown, for those Radio Browser names otherwise, or not at all.
const COUNTRIES: &[(&str, &str)] = &[
    ("us", "United States"),
    ("gb", "United Kingdom"),
    ("ru", "Russia"),
    ("kr", "South Korea"),
    ("kp", "North Korea"),
    ("tw", "Taiwan"),
    ("ve", "Venezuela"),
    ("ir", "Iran"),
    ("sy", "Syria"),
    ("tz", "Tanzania"),
    ("md", "Moldova"),
    ("mk", "North Macedonia"),
    ("la", "Laos"),
    ("cd", "DR Congo"),
    ("cg", "Congo"),
    ("ps", "Palestine"),
    ("ci", "Côte d'Ivoire"),
    ("va", "Vatican City"),
    ("bn", "Brunei"),
    ("um", "US Minor Outlying Islands"),
    ("fk", "Falkland Islands"),
    ("sh", "Saint Helena"),
    ("sx", "Sint Maarten"),
    ("mf", "Saint Martin"),
    ("fm", "Micronesia"),
    ("cc", "Cocos Islands"),
    // The countries of the stations Freya picks, for when Radio Browser has not said yet.
    ("ch", "Switzerland"),
    ("cz", "Czechia"),
    ("de", "Germany"),
    ("dk", "Denmark"),
    ("es", "Spain"),
    ("fi", "Finland"),
    ("fr", "France"),
    ("it", "Italy"),
    ("jp", "Japan"),
    ("nl", "Netherlands"),
    ("no", "Norway"),
    ("se", "Sweden"),
    ("at", "Austria"),
    ("be", "Belgium"),
    ("ca", "Canada"),
    ("au", "Australia"),
    ("ie", "Ireland"),
    ("pt", "Portugal"),
    ("br", "Brazil"),
    ("is", "Iceland"),
    ("pl", "Poland"),
    ("gr", "Greece"),
];

/// A bitrate this high or higher, of a lossy stream, sounds as good as most can hear.
pub const HIGH_BITRATE: u32 = 256;

/// The tags' names Freya knows, and how stations' own tags say them.
pub struct Tags {
    /// From a station's own tag, as [`clean`] leaves it, to the tags it is.
    known: HashMap<String, String>,
    /// Every tag's name, by its id.
    names: HashMap<String, String>,
    /// From a language's code, of either length, to its tag.
    languages: HashMap<&'static str, String>,
}

impl Tags {
    pub fn new() -> Self {
        let mut tags = Self {
            known: HashMap::new(),
            names: HashMap::new(),
            languages: HashMap::new(),
        };
        for (kind, table) in [
            (RadioTagKind::Genre, GENRES),
            (RadioTagKind::Format, FORMATS),
        ] {
            for (id, name, said) in table {
                let tag = format!("{}:{id}", kind.prefix());
                tags.names.insert(tag.clone(), (*name).to_owned());
                for words in *said {
                    tags.known.insert(clean(words), tag.clone());
                }
            }
        }
        for (id, said) in ERAS {
            let tag = format!("era:{id}");
            tags.names.insert(tag.clone(), (*id).to_owned());
            for words in *said {
                tags.known.insert(clean(words), tag.clone());
            }
        }
        for (codes, id, name) in LANGUAGES {
            let tag = format!("language:{id}");
            tags.names.insert(tag.clone(), (*name).to_owned());
            for code in *codes {
                tags.languages.insert(code, tag.clone());
            }
        }
        for (code, name) in COUNTRIES {
            tags.names
                .insert(format!("country:{code}"), (*name).to_owned());
        }
        for (id, name) in [
            ("hi-res", "Hi-Res"),
            ("lossless", "Lossless"),
            ("high", "High bitrate"),
        ] {
            tags.names.insert(format!("quality:{id}"), name.to_owned());
        }
        tags
    }

    /// The genres, decades and formats a station's own tags say, once each, in the order said.
    pub fn of_own(&self, own: &str) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        for said in own.split([',', '/', ';', '|']) {
            if let Some(tag) = self.known.get(&clean(said))
                && !found.contains(tag)
            {
                found.push(tag.clone());
            }
        }
        found
    }

    /// The tag of a language its code names, if Freya knows it.
    pub fn language(&self, code: &str) -> Option<&str> {
        self.languages
            .get(code.trim().to_ascii_lowercase().as_str())
            .map(String::as_str)
    }

    /// A country's tag by its ISO 3166 code, with its name: Radio Browser's `said`, or Freya's
    /// own where that is long or missing.
    pub fn country(&mut self, code: &str, said: &str) -> Option<String> {
        let code = code.trim().to_ascii_lowercase();
        if code.len() != 2 || !code.bytes().all(|b| b.is_ascii_lowercase()) {
            return None;
        }
        let tag = format!("country:{code}");
        if !self.names.contains_key(&tag) {
            let said = said.trim();
            let name = said.strip_prefix("The ").unwrap_or(said);
            if name.is_empty() {
                return None;
            }
            self.names.insert(tag.clone(), name.to_owned());
        }
        Some(tag)
    }

    /// The name a tag is shown by: "Jazz" for "genre:jazz".
    pub fn name<'a>(&'a self, tag: &'a str) -> &'a str {
        self.names.get(tag).map_or_else(
            || tag.split_once(':').map_or(tag, |(_, id)| id),
            String::as_str,
        )
    }

    /// Whether a tag is one Freya can name.
    #[cfg(test)]
    pub fn knows(&self, tag: &str) -> bool {
        self.names.contains_key(tag)
    }
}

/// A station's own tag as Freya compares them: in lower case, without accents, "music" or
/// "radio", and with words apart only by single spaces: "Música Clásica" is "clasica".
pub fn clean(said: &str) -> String {
    let folded: String = said
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            '-' | '_' | '.' | '+' => ' ',
            c => c,
        })
        .filter(|c| !matches!(c, '\'' | '’' | '`' | '"' | '(' | ')'))
        .collect::<String>()
        .replace('&', " and ");
    folded
        .split_whitespace()
        .filter(|word| {
            !matches!(
                *word,
                "music"
                    | "musica"
                    | "musik"
                    | "musique"
                    | "muziek"
                    | "radio"
                    | "musiikki"
                    | "musikk"
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A word to look for as Freya compares them: lower case, without accents.
pub fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stations_own_tags_are_freyas_few() {
        let tags = Tags::new();
        assert_eq!(
            tags.of_own("Jazz,smooth jazz,Música Clásica,80's,news talk,something else"),
            ["genre:jazz", "genre:classical", "era:80s", "format:news"]
        );
        assert_eq!(
            tags.of_own("alternative / indie,Hip-Hop,R&B,Drum & Bass,talk radio"),
            [
                "genre:alternative",
                "genre:indie",
                "genre:hip-hop",
                "genre:rnb",
                "genre:drum-and-bass",
                "format:talk"
            ]
        );
        assert!(tags.of_own("").is_empty());
        assert_eq!(tags.name("genre:drum-and-bass"), "Drum & bass");
    }

    #[test]
    fn languages_and_countries_are_named() {
        let mut tags = Tags::new();
        assert_eq!(tags.language("ITA"), Some("language:it"));
        assert_eq!(tags.language("it"), Some("language:it"));
        assert_eq!(tags.language("xx"), None);
        assert_eq!(tags.name("language:it"), "Italian");
        let us = tags.country("US", "The United States Of America").unwrap();
        assert_eq!(tags.name(&us), "United States");
        let nl = tags.country("NL", "The Netherlands").unwrap();
        assert_eq!(tags.name(&nl), "Netherlands");
        let ug = tags.country("UG", "Uganda").unwrap();
        assert_eq!(tags.name(&ug), "Uganda");
        assert_eq!(tags.country("", ""), None);
        assert_eq!(tags.country("XYZ", "Nowhere"), None);
    }
}
