//! UI language: compile-time string tables (`locales/<lang>.json`), the
//! language picked from Windows or `config.json`, and `t()` lookups.
//!
//! Keys starting with `panel.` are the control panel's Slint strings: there
//! the English text is the `@tr()` source string, and `build.rs` turns the
//! tables into the `.po` files bundled into the panel.
//!
//! Values may hold `{name}` placeholders, and plural forms separated by `|`
//! (one / other, or one / few / many for Russian; a single form for CJK).

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// UI languages: code (as in `config.json` and `locales/`) and native name.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("it", "Italiano"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("pt-BR", "Português (Brasil)"),
    ("zh", "简体中文"),
    ("ru", "Русский"),
    ("ja", "日本語"),
    ("ko", "한국어"),
];

/// `config.json` value that follows the Windows display language.
pub const SYSTEM: &str = "system";

const SOURCES: &[&str] = &[
    include_str!("../locales/en.json"),
    include_str!("../locales/it.json"),
    include_str!("../locales/fr.json"),
    include_str!("../locales/de.json"),
    include_str!("../locales/es.json"),
    include_str!("../locales/pt-BR.json"),
    include_str!("../locales/zh.json"),
    include_str!("../locales/ru.json"),
    include_str!("../locales/ja.json"),
    include_str!("../locales/ko.json"),
];

type Table = HashMap<String, String>;

fn tables() -> &'static [Table] {
    static TABLES: OnceLock<Vec<Table>> = OnceLock::new();
    TABLES.get_or_init(|| {
        SOURCES
            .iter()
            .map(|s| serde_json::from_str(s).expect("locale files are valid JSON"))
            .collect()
    })
}

static CURRENT: AtomicUsize = AtomicUsize::new(0);

fn index_of(code: &str) -> Option<usize> {
    LANGUAGES
        .iter()
        .position(|(c, _)| c.eq_ignore_ascii_case(code))
}

/// The current UI language code (`"de"`, `"pt-BR"`).
pub fn language() -> &'static str {
    LANGUAGES[CURRENT.load(Ordering::Relaxed)].0
}

/// Applies a `language` setting (`"system"`, `None` or a code). The
/// `VITRINE_LANGUAGE` environment variable overrides it.
pub fn init(setting: Option<&str>) {
    let env = std::env::var("VITRINE_LANGUAGE").ok();
    set(env.as_deref().or(setting));
}

/// Applies a `language` setting, ignoring `VITRINE_LANGUAGE`.
pub fn set(setting: Option<&str>) {
    let code = resolve(setting, &system_languages());
    CURRENT.store(index_of(code).unwrap_or(0), Ordering::Relaxed);
}

/// The UI language for a setting, given the Windows preferred UI languages.
pub fn resolve(setting: Option<&str>, system: &[String]) -> &'static str {
    match setting.map(str::trim) {
        Some(s) if !s.is_empty() && !s.eq_ignore_ascii_case(SYSTEM) => {
            match_language(&[s.to_string()])
        }
        _ => match_language(system),
    }
}

/// First supported language among BCP 47 tags in preference order:
/// `zh-Hans`, `zh-CN` and `zh-SG` are Simplified Chinese (`zh`), any `pt-*` is
/// `pt-BR`; Traditional Chinese and unsupported languages fall through, and
/// English is the fallback.
pub fn match_language(tags: &[String]) -> &'static str {
    for tag in tags {
        let c = vitrine_widgets::locale::canonical(tag);
        let base = c.split('-').next().unwrap_or("");
        let code = match base {
            "zh" if c == "zh" => Some("zh"),
            "zh" => None,
            "pt" => Some("pt-BR"),
            b => LANGUAGES.iter().map(|(c, _)| *c).find(|c| *c == b),
        };
        if let Some(code) = code {
            return code;
        }
    }
    "en"
}

/// The user's preferred Windows UI languages (`GetUserPreferredUILanguages`).
pub fn system_languages() -> Vec<String> {
    use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    use windows::core::PWSTR;
    unsafe {
        let mut count = 0u32;
        let mut len = 0u32;
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut len).is_err() {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buf.as_mut_ptr())),
            &mut len,
        )
        .is_err()
        {
            return Vec::new();
        }
        buf.split(|c| *c == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }
}

fn lookup(key: &str) -> &'static str {
    let t = tables();
    t[CURRENT.load(Ordering::Relaxed)]
        .get(key)
        .or_else(|| t[0].get(key))
        .map(String::as_str)
        .unwrap_or_else(|| {
            debug_assert!(false, "missing string `{key}`");
            ""
        })
}

fn fill(mut text: String, args: &[(&str, &str)]) -> String {
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

/// The text of `key` in the current language.
pub fn t(key: &str) -> String {
    lookup(key).to_string()
}

/// `t()` with `{name}` placeholders filled in.
pub fn tf(key: &str, args: &[(&str, &str)]) -> String {
    fill(t(key), args)
}

/// Plural form of `key` for `n`; `{n}` is filled in with `n`.
pub fn tn(key: &str, n: u64) -> String {
    let forms: Vec<&str> = lookup(key).split('|').collect();
    let form = forms[plural_index(language(), n).min(forms.len() - 1)];
    fill(form.to_string(), &[("n", &n.to_string())])
}

/// The text of a package error, localised where the user can act on it.
pub fn package_error(e: &vitrine_widgets::Error) -> String {
    match e {
        vitrine_widgets::Error::EngineTooOld {
            name,
            required,
            running,
        } => tf(
            "status.needs_engine",
            &[("name", name), ("required", required), ("running", running)],
        ),
        e => e.to_string(),
    }
}

/// Number of plural forms a language's `|`-separated values have.
#[cfg(test)]
pub fn plural_forms(lang: &str) -> usize {
    match lang {
        "zh" | "ja" | "ko" => 1,
        "ru" => 3,
        _ => 2,
    }
}

/// CLDR cardinal rules, reduced to integers.
pub fn plural_index(lang: &str, n: u64) -> usize {
    match lang {
        "zh" | "ja" | "ko" => 0,
        "fr" | "pt-BR" => usize::from(n > 1),
        "ru" => {
            let (m10, m100) = (n % 10, n % 100);
            if m10 == 1 && m100 != 11 {
                0
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                1
            } else {
                2
            }
        }
        _ => usize::from(n != 1),
    }
}

/// BCP 47 locale for DirectWrite (picks the right CJK glyph variants).
pub fn locale_name() -> &'static str {
    match language() {
        "en" => "en-US",
        "it" => "it-IT",
        "fr" => "fr-FR",
        "de" => "de-DE",
        "es" => "es-ES",
        "zh" => "zh-CN",
        "ru" => "ru-RU",
        "ja" => "ja-JP",
        "ko" => "ko-KR",
        other => other,
    }
}

/// The Windows UI font for the current language.
pub fn ui_font() -> &'static str {
    match language() {
        "zh" => "Microsoft YaHei UI",
        "ja" => "Yu Gothic UI",
        "ko" => "Malgun Gothic",
        _ => "Segoe UI Variable Text",
    }
}

/// Selects the panel's bundled translation (Slint `@tr()` strings).
pub fn apply_to_slint() {
    let code = language();
    if let Err(e) = slint::select_bundled_translation(if code == "en" { "" } else { code }) {
        log::warn!("panel translation {code}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(s: &str) -> Vec<String> {
        let mut out: Vec<String> = s
            .match_indices('{')
            .filter_map(|(i, _)| s[i..].find('}').map(|j| s[i..i + j + 1].to_string()))
            .collect();
        out.sort();
        out
    }

    fn keys(t: &Table) -> BTreeSet<&str> {
        t.keys().map(String::as_str).collect()
    }

    #[test]
    fn every_locale_has_exactly_the_english_keys() {
        assert_eq!(SOURCES.len(), LANGUAGES.len());
        let en = keys(&tables()[0]);
        for (i, (code, _)) in LANGUAGES.iter().enumerate().skip(1) {
            let k = keys(&tables()[i]);
            let missing: Vec<_> = en.difference(&k).collect();
            let extra: Vec<_> = k.difference(&en).collect();
            assert!(missing.is_empty(), "{code}: missing {missing:?}");
            assert!(extra.is_empty(), "{code}: extra {extra:?}");
        }
    }

    #[test]
    fn placeholders_and_plural_forms_match() {
        let en = &tables()[0];
        for (i, (code, _)) in LANGUAGES.iter().enumerate() {
            for (key, value) in &tables()[i] {
                assert!(!value.trim().is_empty(), "{code}: {key} is empty");
                let source = &en[key];
                let plural = source.contains('|');
                let forms: Vec<&str> = value.split('|').collect();
                if plural {
                    assert_eq!(
                        forms.len(),
                        plural_forms(code),
                        "{code}: {key} plural forms"
                    );
                } else {
                    assert_eq!(forms.len(), 1, "{code}: {key} has a stray `|`");
                }
                let want = placeholders(source.split('|').next_back().unwrap());
                for f in forms {
                    assert_eq!(placeholders(f), want, "{code}: {key} placeholders");
                }
            }
        }
    }

    #[test]
    fn panel_strings_match_the_slint_source() {
        let slint = include_str!("../ui/panel.slint");
        let mut used = BTreeSet::new();
        let mut rest = slint;
        while let Some(i) = rest.find("@tr(\"") {
            rest = &rest[i + 5..];
            let end = rest.find('"').unwrap();
            used.insert(rest[..end].to_string());
            rest = &rest[end..];
        }
        let en = &tables()[0];
        let mut panel: Vec<&String> = en
            .iter()
            .filter(|(k, _)| k.starts_with("panel."))
            .map(|(_, v)| v)
            .collect();
        let defined: BTreeSet<String> = panel.iter().map(|v| v.to_string()).collect();
        assert_eq!(used, defined, "@tr() strings and panel.* keys differ");
        panel.sort();
        panel.dedup();
        assert_eq!(panel.len(), defined.len(), "panel.* texts must be unique");
    }

    #[test]
    fn language_matching() {
        let m =
            |tags: &[&str]| match_language(&tags.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(m(&["it-IT"]), "it");
        assert_eq!(m(&["de-CH", "en-US"]), "de");
        assert_eq!(m(&["zh-Hans-CN"]), "zh");
        assert_eq!(m(&["zh-CN"]), "zh");
        assert_eq!(m(&["zh-SG"]), "zh");
        assert_eq!(m(&["zh-TW", "ja-JP"]), "ja");
        assert_eq!(m(&["zh-Hant"]), "en");
        assert_eq!(m(&["pt-PT"]), "pt-BR");
        assert_eq!(m(&["pt-BR"]), "pt-BR");
        assert_eq!(m(&["nl-NL", "fr-FR"]), "fr");
        assert_eq!(m(&["nl-NL"]), "en");
        assert_eq!(m(&[]), "en");
        assert_eq!(m(&["ko-KR"]), "ko");
        assert_eq!(m(&["ru"]), "ru");

        let sys = vec!["es-ES".to_string()];
        assert_eq!(resolve(None, &sys), "es");
        assert_eq!(resolve(Some("system"), &sys), "es");
        assert_eq!(resolve(Some(""), &sys), "es");
        assert_eq!(resolve(Some("ja"), &sys), "ja");
        assert_eq!(resolve(Some("pt-br"), &sys), "pt-BR");
        assert_eq!(resolve(Some("klingon"), &sys), "en");
    }

    #[test]
    fn plurals() {
        assert_eq!(plural_index("en", 1), 0);
        assert_eq!(plural_index("en", 0), 1);
        assert_eq!(plural_index("fr", 0), 0);
        assert_eq!(plural_index("fr", 2), 1);
        assert_eq!(plural_index("ru", 1), 0);
        assert_eq!(plural_index("ru", 3), 1);
        assert_eq!(plural_index("ru", 5), 2);
        assert_eq!(plural_index("ru", 11), 2);
        assert_eq!(plural_index("ru", 21), 0);
        assert_eq!(plural_index("ru", 22), 1);
        assert_eq!(plural_index("ja", 7), 0);
        assert_eq!(tn("row.widgets", 1), "1 widget");
        assert_eq!(tn("row.widgets", 3), "3 widgets");
    }

    #[test]
    fn package_errors() {
        let e = vitrine_widgets::Error::EngineTooOld {
            name: "Pack".into(),
            required: "9.0.0".into(),
            running: "2.0.0".into(),
        };
        let text = package_error(&e);
        assert!(
            text.contains("Pack") && text.contains("9.0.0") && text.contains("2.0.0"),
            "{text}"
        );
        assert!(!text.contains('{'), "{text}");
        let other = vitrine_widgets::Error::NotFound("x".into());
        assert_eq!(package_error(&other), other.to_string());
    }

    #[test]
    fn lookups_fill_placeholders() {
        assert_eq!(tf("status.added", &[("name", "Clock")]), "Added Clock");
    }
}
