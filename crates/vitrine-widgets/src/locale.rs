//! Language tags for localised pack texts (`locales` in `vitrine.json`,
//! `labels` / `descriptions` / `titles` in settings schemas).
//!
//! Vitrine's UI languages are `en`, `it`, `fr`, `de`, `es`, `pt-BR`, `zh`
//! (Simplified Chinese), `ru`, `ja` and `ko`; pack authors may use any
//! BCP 47 tag, and the closest one is picked.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;

/// Localised variants of a text, by language tag.
pub type Translations = BTreeMap<String, String>;

fn tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8})*$").unwrap())
}

/// True for a well-formed language tag (`it`, `pt-BR`, `zh-Hans`).
pub fn is_valid_tag(tag: &str) -> bool {
    tag_re().is_match(tag)
}

/// Lower-case canonical form: Simplified Chinese is `zh`, Traditional is
/// `zh-hant`, any Portuguese is `pt-br`'s family (`pt`).
pub fn canonical(tag: &str) -> String {
    let t = tag.trim().replace('_', "-").to_ascii_lowercase();
    let mut parts = t.split('-');
    let base = parts.next().unwrap_or("").to_string();
    let rest: Vec<&str> = parts.collect();
    if base == "zh" {
        let traditional = rest
            .iter()
            .any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo"));
        return if traditional {
            "zh-hant".into()
        } else {
            "zh".into()
        };
    }
    if rest.is_empty() {
        base
    } else {
        format!("{base}-{}", rest.join("-"))
    }
}

fn base(canonical: &str) -> &str {
    canonical.split('-').next().unwrap_or(canonical)
}

/// The variant of `map` that best fits `lang`: same tag, then same language
/// (`pt` for `pt-BR`, `zh-CN` for `zh`; never Traditional for Simplified
/// Chinese). `None` when the pack has no text for that language.
pub fn pick<'a>(map: &'a Translations, lang: &str) -> Option<&'a str> {
    best(map, lang).map(String::as_str)
}

/// The entry of a map keyed by language tag that best fits `lang` (see [`pick`]).
pub fn best<'a, V>(map: &'a BTreeMap<String, V>, lang: &str) -> Option<&'a V> {
    let want = canonical(lang);
    let exact = map.iter().find(|(k, _)| canonical(k) == want);
    let near = || {
        map.iter().find(|(k, _)| {
            let c = canonical(k);
            base(&c) == base(&want) && (c == "zh-hant") == (want == "zh-hant")
        })
    };
    exact.or_else(near).map(|(_, v)| v)
}

/// Replaces `text` with its variant for `lang`, if any.
pub fn apply(text: &mut String, map: &Translations, lang: &str) {
    if let Some(v) = pick(map, lang)
        && !v.trim().is_empty()
    {
        *text = v.to_string();
    }
}

/// Checks the tags and texts of a translation map; `what` names it in errors.
pub fn validate(map: &Translations, what: &str) -> Result<(), String> {
    for (tag, text) in map {
        if !is_valid_tag(tag) {
            return Err(format!("{what}: invalid language tag `{tag}`"));
        }
        if text.trim().is_empty() {
            return Err(format!("{what}: empty text for `{tag}`"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> Translations {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn tags() {
        assert!(is_valid_tag("it"));
        assert!(is_valid_tag("pt-BR"));
        assert!(is_valid_tag("zh-Hans-CN"));
        assert!(!is_valid_tag(""));
        assert!(!is_valid_tag("italian!"));
        assert!(!is_valid_tag("-it"));
        assert_eq!(canonical("zh-CN"), "zh");
        assert_eq!(canonical("zh_Hans"), "zh");
        assert_eq!(canonical("zh-TW"), "zh-hant");
        assert_eq!(canonical("pt-BR"), "pt-br");
        assert_eq!(canonical("DE"), "de");
    }

    #[test]
    fn picks_the_closest_variant() {
        let m = map(&[("it", "Orologio"), ("pt", "Relógio"), ("zh-CN", "时钟")]);
        assert_eq!(pick(&m, "it"), Some("Orologio"));
        assert_eq!(pick(&m, "it-IT"), Some("Orologio"));
        assert_eq!(pick(&m, "pt-BR"), Some("Relógio"));
        assert_eq!(pick(&m, "zh"), Some("时钟"));
        assert_eq!(pick(&m, "zh-Hant"), None);
        assert_eq!(pick(&m, "de"), None);
        // An exact tag wins over a same-language one.
        let m = map(&[("pt-PT", "Relógio (PT)"), ("pt-BR", "Relógio")]);
        assert_eq!(pick(&m, "pt-BR"), Some("Relógio"));
        let m = map(&[("zh-Hant", "時鐘"), ("zh-Hans", "时钟")]);
        assert_eq!(pick(&m, "zh"), Some("时钟"));
    }

    #[test]
    fn validation() {
        assert!(validate(&map(&[("it", "Ciao")]), "x").is_ok());
        assert!(validate(&map(&[("it!", "Ciao")]), "x").is_err());
        assert!(validate(&map(&[("it", " ")]), "x").is_err());
    }
}
