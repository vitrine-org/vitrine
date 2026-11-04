//! Widget search shared by the control panel and the edit-mode gallery:
//! accent- and case-insensitive matching of every typed word, plus the
//! size classes used as a filter.

/// Lowercases `s` and drops accents from Latin letters, so "metéo" finds "Meteo".
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        let base = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
            'ç' | 'ć' | 'č' => 'c',
            'ď' => 'd',
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => 'e',
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' => 'i',
            'ł' | 'ľ' => 'l',
            'ñ' | 'ń' | 'ň' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => 'o',
            'ř' => 'r',
            'ś' | 'š' | 'ş' => 's',
            'ť' => 't',
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => 'u',
            'ý' | 'ÿ' => 'y',
            'ź' | 'ż' | 'ž' => 'z',
            'ß' => {
                out.push_str("ss");
                continue;
            }
            'ё' => 'е',
            other => other,
        };
        out.push(base);
    }
    out
}

/// True when every word of `query` appears in at least one of `fields`
/// (an empty query matches everything).
pub fn matches(query: &str, fields: &[&str]) -> bool {
    let words: Vec<String> = fold(query).split_whitespace().map(str::to_string).collect();
    if words.is_empty() {
        return true;
    }
    let hay: Vec<String> = fields.iter().map(|f| fold(f)).collect();
    words
        .iter()
        .all(|w| hay.iter().any(|h| h.contains(w.as_str())))
}

/// Size filter: by the widget's default size in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeClass {
    Small,
    Medium,
    Large,
}

impl SizeClass {
    pub fn of(cells: (i32, i32)) -> SizeClass {
        match cells.0.max(1) * cells.1.max(1) {
            0..=4 => SizeClass::Small,
            5..=9 => SizeClass::Medium,
            _ => SizeClass::Large,
        }
    }
}

/// What the gallery knows about a widget, for filtering.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// Searchable text: localised and original names, descriptions, pack name.
    pub text: Vec<String>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub pack: String,
    pub size: (i32, i32),
    pub interactive: bool,
    pub online: bool,
}

/// The gallery's search box, package chip and facet chips.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    pub query: String,
    /// Package id; None for all packages.
    pub pack: Option<String>,
    pub size: Option<SizeClass>,
    pub interactive: bool,
    pub online: bool,
}

impl Filter {
    /// Everything but the package chip (used for the per-package counts).
    pub fn accepts_any_pack(&self, f: &Facts) -> bool {
        let text: Vec<&str> = f.text.iter().map(String::as_str).collect();
        self.size.is_none_or(|s| SizeClass::of(f.size) == s)
            && (!self.interactive || f.interactive)
            && (!self.online || f.online)
            && matches(&self.query, &text)
    }

    #[cfg(test)]
    pub fn accepts(&self, f: &Facts) -> bool {
        self.pack.as_ref().is_none_or(|p| *p == f.pack) && self.accepts_any_pack(f)
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        *self == Filter::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(name: &str, pack: &str, size: (i32, i32), interactive: bool, online: bool) -> Facts {
        Facts {
            text: vec![name.into(), format!("{pack} pack")],
            pack: pack.into(),
            size,
            interactive,
            online,
        }
    }

    #[test]
    fn filter_combines_query_pack_and_facets() {
        let all = [
            facts("Clock", "a", (2, 2), false, false),
            facts("Weather", "a", (6, 2), false, true),
            facts("To-do", "b", (3, 3), true, false),
            facts("Météo", "b", (4, 2), false, true),
        ];
        let hits =
            |f: &Filter| -> Vec<usize> { (0..all.len()).filter(|&i| f.accepts(&all[i])).collect() };
        let mut f = Filter::default();
        assert!(f.is_empty());
        assert_eq!(hits(&f), [0, 1, 2, 3]);
        f.query = "meteo".into();
        assert_eq!(hits(&f), [3]);
        f.query = "b pack".into();
        assert_eq!(hits(&f), [2, 3]);
        f.query.clear();
        f.pack = Some("a".into());
        assert_eq!(hits(&f), [0, 1]);
        assert!(f.accepts_any_pack(&all[2]), "counts ignore the pack chip");
        f.pack = None;
        f.online = true;
        assert_eq!(hits(&f), [1, 3]);
        f.size = Some(SizeClass::Medium);
        assert_eq!(hits(&f), [3]);
        f = Filter {
            interactive: true,
            ..Filter::default()
        };
        assert_eq!(hits(&f), [2]);
        assert!(!f.is_empty());
    }

    #[test]
    fn folding_ignores_case_and_accents() {
        assert_eq!(fold("Météo Ärger Straße"), "meteo arger strasse");
        assert_eq!(fold("ÉTÉ"), "ete");
        assert_eq!(fold("時計"), "時計");
    }

    #[test]
    fn every_word_must_match_some_field() {
        let fields = ["Word clock", "The time in words", "Alnitak"];
        assert!(matches("", &fields));
        assert!(matches("   ", &fields));
        assert!(matches("clock", &fields));
        assert!(matches("CLOCK alnitak", &fields));
        assert!(matches("time words", &fields));
        assert!(!matches("clock weather", &fields));
        assert!(matches("orologio", &["Orologio a parole"]));
        assert!(matches("meteo", &["Météo"]));
        assert!(matches("météo", &["Meteo"]));
        assert!(matches("時計", &["デジタル時計"]));
    }

    #[test]
    fn size_classes() {
        assert_eq!(SizeClass::of((2, 2)), SizeClass::Small);
        assert_eq!(SizeClass::of((4, 1)), SizeClass::Small);
        assert_eq!(SizeClass::of((3, 3)), SizeClass::Medium);
        assert_eq!(SizeClass::of((4, 2)), SizeClass::Medium);
        assert_eq!(SizeClass::of((6, 2)), SizeClass::Large);
        assert_eq!(SizeClass::of((0, 0)), SizeClass::Small);
    }
}
