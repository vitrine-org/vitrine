//! `vitrine.json` package manifest.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::locale;
use crate::{Error, Result};

/// Name of the manifest file at the package root.
pub const MANIFEST_FILE: &str = "vitrine.json";

/// The `system.<name>` permissions. Each grants the metric of the same name,
/// except `system.info`, which grants the `system` metric (host name, OS
/// version, uptime): see [`metric_permission`].
pub const SYSTEM_METRICS: &[&str] = &[
    "cpu",
    "memory",
    "battery",
    "drives",
    "processes",
    "network",
    "info",
];

/// The `system.<name>` permission that grants a metric `useSystem()` asks
/// for (`None` for a name that is not a metric).
pub fn metric_permission(metric: &str) -> Option<&str> {
    match metric {
        "system" => Some("info"),
        "info" => None,
        m => Some(m),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permission {
    /// `system.<metric>`
    System(String),
    /// `net:<host>`, or `net:*.<host>` for any subdomain of `<host>` (not
    /// `<host>` itself).
    Net(String),
}

impl Permission {
    pub fn parse(s: &str) -> std::result::Result<Permission, String> {
        if let Some(m) = s.strip_prefix("system.") {
            if SYSTEM_METRICS.contains(&m) {
                return Ok(Permission::System(m.to_string()));
            }
            return Err(format!("unknown permission `{s}`"));
        }
        if let Some(h) = s.strip_prefix("net:") {
            // A wildcard is only allowed as the whole first label, before at
            // least two labels (`*.com` would match every `.com` site).
            let (host, wildcard) = match h.strip_prefix("*.") {
                Some(rest) => (rest, true),
                None => (h, false),
            };
            let ok = !host.is_empty()
                && h.len() <= 253
                && (!wildcard || host.contains('.'))
                && host.split('.').all(|l| {
                    !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                });
            if ok {
                return Ok(Permission::Net(h.to_ascii_lowercase()));
            }
            return Err(format!("invalid host in permission `{s}`"));
        }
        Err(format!("unknown permission `{s}`"))
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Permission::System(m) => write!(f, "system.{m}"),
            Permission::Net(h) => write!(f, "net:{h}"),
        }
    }
}

impl Serialize for Permission {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Permission {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Permission::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeSpec {
    pub default: [u32; 2],
    pub min: [u32; 2],
    pub max: [u32; 2],
}

impl Default for SizeSpec {
    fn default() -> Self {
        Self {
            default: [4, 3],
            min: [1, 1],
            max: [16, 16],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub entry: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default)]
    pub size: SizeSpec,
    #[serde(default)]
    pub interactive: bool,
    #[serde(default)]
    pub permissions: Vec<Permission>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_engine: Option<String>,
    #[serde(default)]
    pub fonts: Vec<String>,
    /// Square PNG or SVG that stands for the pack in the panel and the Store
    /// (256 x 256 or larger, transparent background). Packs without one get a
    /// monogram.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub widgets: Vec<WidgetManifest>,
    /// Localised names and descriptions, by language tag (`"it"`, `"pt-BR"`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub locales: BTreeMap<String, PackageLocale>,
}

/// One language of `locales`: every field is optional and falls back to the
/// manifest's own (English) text.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PackageLocale {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// By widget id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub widgets: BTreeMap<String, WidgetLocale>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WidgetLocale {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn set_text(dst: &mut String, src: &Option<String>) {
    if let Some(v) = src
        && !v.trim().is_empty()
    {
        *dst = v.clone();
    }
}

fn package_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9]+(\.[a-z0-9-]+)+$").unwrap())
}

fn widget_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9-]+$").unwrap())
}

/// True if `id` is a valid reverse-DNS package id.
pub fn is_valid_package_id(id: &str) -> bool {
    package_id_re().is_match(id) && !has_reserved_segment(id)
}

fn has_reserved_segment(id: &str) -> bool {
    // Windows device names are reserved even with an extension ("con.txt").
    let first = id.split('.').next().unwrap_or("");
    matches!(first, "con" | "prn" | "aux" | "nul")
        || ((first.starts_with("com") || first.starts_with("lpt"))
            && first.len() == 4
            && first.as_bytes()[3].is_ascii_digit())
}

/// Parses `major.minor.patch[-pre][+build]`.
pub fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.split('+').next()?;
    let (core, pre) = match core.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (core, None),
    };
    if let Some(p) = pre
        && (p.is_empty()
            || !p
                .split('.')
                .all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')))
    {
        return None;
    }
    let mut it = core.split('.');
    let mut next = || -> Option<u64> {
        let p = it.next()?;
        if p.is_empty()
            || (p.len() > 1 && p.starts_with('0'))
            || !p.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        p.parse().ok()
    };
    let (a, b, c) = (next()?, next()?, next()?);
    if it.next().is_some() {
        return None;
    }
    Some((a, b, c))
}

/// A relative path inside a package: forward slashes only, no `..`, no
/// absolute paths, no drive letters, no empty segments.
pub fn is_safe_relative_path(p: &str) -> bool {
    if p.is_empty() || p.starts_with('/') || p.ends_with('/') {
        return false;
    }
    if p.contains(['\\', ':', '\0']) {
        return false;
    }
    p.split('/').all(|seg| {
        !seg.is_empty()
            && seg != "."
            && seg != ".."
            && !seg.ends_with(['.', ' '])
            && !seg.contains(['<', '>', '"', '|', '?', '*'])
    })
}

/// The running engine's version (the workspace version, shared by every crate).
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Version core and pre-release identifiers.
pub type Version<'a> = ([u64; 3], Vec<&'a str>);

/// `MAJOR.MINOR.PATCH[-pre][+build]`, with an optional leading `v`.
pub fn parse_version(v: &str) -> Option<Version<'_>> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split('+').next()?;
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, p.split('.').collect()),
        None => (v, Vec::new()),
    };
    let mut nums = [0u64; 3];
    let mut parts = core.split('.');
    for n in &mut nums {
        *n = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some((nums, pre))
}

/// Semantic-version order; unparsable versions sort first.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let (Some((an, ap)), Some((bn, bp))) = (parse_version(a), parse_version(b)) else {
        return parse_version(a).is_some().cmp(&parse_version(b).is_some());
    };
    an.cmp(&bn)
        .then_with(|| match (ap.is_empty(), bp.is_empty()) {
            (true, true) => Ordering::Equal,
            // A pre-release comes before its release.
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (x, y) in ap.iter().zip(&bp) {
                    let o = match (x.parse::<u64>(), y.parse::<u64>()) {
                        (Ok(x), Ok(y)) => x.cmp(&y),
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => x.cmp(y),
                    };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                ap.len().cmp(&bp.len())
            }
        })
}

fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Manifest(msg.into()))
}

impl PackageManifest {
    /// Parses a manifest from JSON text (does not call [`validate`](Self::validate)).
    pub fn parse(json: &str) -> Result<PackageManifest> {
        Ok(serde_json::from_str(json)?)
    }

    /// Parses and validates.
    pub fn parse_validated(json: &str) -> Result<PackageManifest> {
        let m = Self::parse(json)?;
        m.validate()?;
        Ok(m)
    }

    /// Refuses a package whose `minEngine` is newer than `engine`.
    pub fn check_engine(&self, engine: &str) -> Result<()> {
        match &self.min_engine {
            Some(min) if compare_versions(engine, min) == Ordering::Less => {
                Err(Error::EngineTooOld {
                    name: self.name.clone(),
                    required: min.clone(),
                    running: engine.to_string(),
                })
            }
            _ => Ok(()),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if !is_valid_package_id(&self.id) {
            return err(format!("invalid package id `{}`", self.id));
        }
        if self.name.trim().is_empty() {
            return err("package name is empty");
        }
        if parse_semver(&self.version).is_none() {
            return err(format!("invalid version `{}`", self.version));
        }
        if let Some(me) = &self.min_engine
            && parse_semver(me).is_none()
        {
            return err(format!("invalid minEngine `{me}`"));
        }
        for f in &self.fonts {
            if !is_safe_relative_path(f) {
                return err(format!("unsafe font path `{f}`"));
            }
        }
        if let Some(icon) = &self.icon {
            let lower = icon.to_ascii_lowercase();
            if !is_safe_relative_path(icon) {
                return err(format!("unsafe icon path `{icon}`"));
            }
            if !(lower.ends_with(".png") || lower.ends_with(".svg")) {
                return err(format!("icon `{icon}` must be a .png or .svg file"));
            }
        }
        if self.widgets.is_empty() {
            return err("package has no widgets");
        }
        let mut seen = HashSet::new();
        for w in &self.widgets {
            if !widget_id_re().is_match(&w.id) {
                return err(format!("invalid widget id `{}`", w.id));
            }
            if !seen.insert(w.id.as_str()) {
                return err(format!("duplicate widget id `{}`", w.id));
            }
            w.validate()?;
        }
        for (tag, l) in &self.locales {
            if !locale::is_valid_tag(tag) {
                return err(format!("locales: invalid language tag `{tag}`"));
            }
            let blank = |v: &Option<String>| v.as_ref().is_some_and(|v| v.trim().is_empty());
            if blank(&l.name) || blank(&l.description) {
                return err(format!("locales.{tag}: empty text"));
            }
            for (id, wl) in &l.widgets {
                if !seen.contains(id.as_str()) {
                    return err(format!("locales.{tag}: unknown widget `{id}`"));
                }
                if blank(&wl.name) || blank(&wl.description) {
                    return err(format!("locales.{tag}.widgets.{id}: empty text"));
                }
            }
        }
        Ok(())
    }

    /// Replaces names and descriptions with their `lang` variants from
    /// `locales` (closest tag; missing texts keep the manifest's own).
    pub fn localize(&mut self, lang: &str) {
        let Some(l) = locale::best(&self.locales, lang).cloned() else {
            return;
        };
        set_text(&mut self.name, &l.name);
        set_text(&mut self.description, &l.description);
        for w in &mut self.widgets {
            if let Some(wl) = l.widgets.get(&w.id) {
                set_text(&mut w.name, &wl.name);
                set_text(&mut w.description, &wl.description);
            }
        }
    }

    /// Every file path referenced by the manifest (entry, settings, preview, fonts).
    pub fn referenced_files(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.fonts.iter().map(String::as_str).collect();
        v.extend(self.icon.as_deref());
        for w in &self.widgets {
            v.push(&w.entry);
            v.extend(w.settings.as_deref());
            v.extend(w.preview.as_deref());
        }
        v
    }
}

impl WidgetManifest {
    pub fn validate(&self) -> Result<()> {
        let id = &self.id;
        if self.name.trim().is_empty() {
            return err(format!("widget `{id}`: name is empty"));
        }
        for (label, p) in [
            ("entry", Some(&self.entry)),
            ("settings", self.settings.as_ref()),
            ("preview", self.preview.as_ref()),
        ] {
            if let Some(p) = p
                && !is_safe_relative_path(p)
            {
                return err(format!("widget `{id}`: unsafe {label} path `{p}`"));
            }
        }
        let s = &self.size;
        for (label, v) in [("default", s.default), ("min", s.min), ("max", s.max)] {
            if v[0] < 1 || v[1] < 1 {
                return err(format!("widget `{id}`: size.{label} must be >= 1"));
            }
        }
        for i in 0..2 {
            if !(s.min[i] <= s.default[i] && s.default[i] <= s.max[i]) {
                return err(format!(
                    "widget `{id}`: size must satisfy min <= default <= max"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"{
      "id": "dev.vitrine.sargas", "name": "Sargas", "version": "2.0.0", "author": "me",
      "description": "d", "homepage": "https://x.y", "minEngine": "2.0.0",
      "fonts": ["assets/fonts/BebasNeue-Regular.ttf"],
      "widgets": [{ "id": "clock", "name": "Clock", "description": "c",
        "entry": "widgets/clock/index.js", "settings": "widgets/clock/settings.json",
        "preview": "widgets/clock/preview.png",
        "size": { "default": [4,3], "min": [2,2], "max": [8,6] }, "interactive": false,
        "permissions": ["system.cpu","system.network","net:api.open-meteo.com"] }]
    }"#;

    fn with(f: impl FnOnce(&mut PackageManifest)) -> Result<()> {
        let mut m = PackageManifest::parse(VALID).unwrap();
        f(&mut m);
        m.validate()
    }

    const LOCALISED: &str = r#"{
      "id": "dev.vitrine.sargas", "name": "Sargas", "version": "2.0.0",
      "widgets": [{ "id": "clock", "name": "Clock", "description": "Time", "entry": "i.js" },
                  { "id": "date", "name": "Date", "entry": "d.js" }],
      "locales": {
        "it": { "name": "Sargas IT", "widgets": { "clock": { "name": "Orologio", "description": "Ora" } } },
        "zh-CN": { "widgets": { "date": { "name": "日期" } } },
        "pt": { "description": "Pacote" }
      }
    }"#;

    #[test]
    fn locales_parse_and_apply() {
        let m = PackageManifest::parse_validated(LOCALISED).unwrap();
        assert_eq!(m.locales.len(), 3);
        assert_eq!(
            m.locales["it"].widgets["clock"].name.as_deref(),
            Some("Orologio")
        );

        let mut it = m.clone();
        it.localize("it");
        assert_eq!(it.name, "Sargas IT");
        assert_eq!(
            (
                it.widgets[0].name.as_str(),
                it.widgets[0].description.as_str()
            ),
            ("Orologio", "Ora")
        );
        assert_eq!(it.widgets[1].name, "Date");

        let mut zh = m.clone();
        zh.localize("zh");
        assert_eq!(zh.name, "Sargas");
        assert_eq!(zh.widgets[1].name, "日期");

        let mut pt = m.clone();
        pt.localize("pt-BR");
        assert_eq!(pt.description, "Pacote");

        let mut de = m.clone();
        de.localize("de");
        assert_eq!(de.widgets[0].name, "Clock");

        // Manifests without `locales` still parse; serialising skips the empty map.
        let plain = PackageManifest::parse(VALID).unwrap();
        assert!(plain.locales.is_empty());
        assert!(!serde_json::to_string(&plain).unwrap().contains("locales"));
    }

    #[test]
    fn bad_locales_are_rejected() {
        let bad = |locales: &str| {
            let json =
                LOCALISED.replace(r#""locales": {"#, &format!(r#""locales": {{ {locales}, "#));
            PackageManifest::parse_validated(&json).is_err()
        };
        assert!(bad(r#""it!": {}"#));
        assert!(bad(r#""de": { "name": " " }"#));
        assert!(bad(r#""de": { "widgets": { "nope": { "name": "X" } } }"#));
        assert!(bad(
            r#""de": { "widgets": { "clock": { "description": "" } } }"#
        ));
        assert!(
            PackageManifest::parse(
                &LOCALISED.replace(r#""locales": {"#, r#""locales": { "de": 3, "#)
            )
            .is_err()
        );
    }

    #[test]
    fn valid_manifest() {
        let m = PackageManifest::parse_validated(VALID).unwrap();
        assert_eq!(
            m.widgets[0].permissions[0],
            Permission::System("cpu".into())
        );
        assert_eq!(
            m.widgets[0].permissions[2],
            Permission::Net("api.open-meteo.com".into())
        );
        assert_eq!(m.referenced_files().len(), 4);
        let again = PackageManifest::parse(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(m, again);
    }

    #[test]
    fn bad_ids_and_versions() {
        assert!(with(|m| m.id = "sargas".into()).is_err());
        assert!(with(|m| m.id = "Dev.Vitrine".into()).is_err());
        assert!(with(|m| m.id = "con.x".into()).is_err());
        assert!(with(|m| m.id = "dev.a-b.c9".into()).is_ok());
        assert!(with(|m| m.version = "2.0".into()).is_err());
        assert!(with(|m| m.version = "2.0.0-beta.1".into()).is_ok());
        assert!(with(|m| m.min_engine = Some("x".into())).is_err());
        assert!(with(|m| m.widgets[0].id = "Bad_Id".into()).is_err());
        assert!(with(|m| m.widgets.clear()).is_err());
    }

    #[test]
    fn duplicate_widget_ids() {
        assert!(
            with(|m| {
                let w = m.widgets[0].clone();
                m.widgets.push(w);
            })
            .is_err()
        );
    }

    #[test]
    fn size_rules() {
        assert!(with(|m| m.widgets[0].size.default = [1, 3]).is_err());
        assert!(with(|m| m.widgets[0].size.default = [9, 3]).is_err());
        assert!(with(|m| m.widgets[0].size.min = [0, 2]).is_err());
        assert!(with(|m| m.widgets[0].size.max = [8, 2]).is_err());
    }

    #[test]
    fn path_rules() {
        for bad in [
            "/a", "../a", "a/../b", "C:/a", "a\\b", "a//b", "", "a/", "./a",
        ] {
            assert!(!is_safe_relative_path(bad), "{bad}");
            assert!(with(|m| m.widgets[0].entry = bad.into()).is_err(), "{bad}");
        }
        assert!(with(|m| m.fonts = vec!["../x.ttf".into()]).is_err());
        assert!(with(|m| m.widgets[0].preview = Some("C:\\x.png".into())).is_err());
        assert!(with(|m| m.icon = Some("assets/icon.png".into())).is_ok());
        assert!(with(|m| m.icon = Some("assets/Icon.SVG".into())).is_ok());
        assert!(with(|m| m.icon = Some("../icon.png".into())).is_err());
        assert!(with(|m| m.icon = Some("icon.ico".into())).is_err());
        assert!(is_safe_relative_path("widgets/clock/index.js"));
    }

    #[test]
    fn engine_requirement() {
        let with = |me: Option<&str>| {
            let mut m = PackageManifest::parse(VALID).unwrap();
            m.min_engine = me.map(String::from);
            m
        };
        assert!(with(None).check_engine("2.0.0").is_ok());
        assert!(with(Some("2.0.0")).check_engine("2.0.0").is_ok());
        assert!(with(Some("1.9.9")).check_engine("2.0.0").is_ok());
        assert!(with(Some("2.0.0-rc.1")).check_engine("2.0.0").is_ok());
        assert!(with(Some("2.0.0")).check_engine("2.0.0-rc.1").is_err());
        let e = with(Some("2.10.0")).check_engine("2.9.3").unwrap_err();
        assert!(
            matches!(&e, Error::EngineTooOld { required, running, .. }
                if required == "2.10.0" && running == "2.9.3"),
            "{e}"
        );
        assert!(with(Some("99.0.0")).check_engine(ENGINE_VERSION).is_err());
    }

    #[test]
    fn permissions() {
        assert!(Permission::parse("system.cpu").is_ok());
        assert!(Permission::parse("system.gpu").is_err());
        assert!(Permission::parse("system.info").is_ok());
        assert!(Permission::parse("system.system").is_err());
        assert_eq!(metric_permission("system"), Some("info"));
        assert_eq!(metric_permission("cpu"), Some("cpu"));
        assert_eq!(metric_permission("info"), None);
        assert!(Permission::parse("fs.read").is_err());
        assert!(Permission::parse("net:").is_err());
        assert!(Permission::parse("net:a/b").is_err());
        assert_eq!(
            Permission::parse("net:*.Open-Meteo.com"),
            Ok(Permission::Net("*.open-meteo.com".into()))
        );
        for bad in [
            "net:*",
            "net:*.",
            "net:*.com",
            "net:**.a.com",
            "net:a.*.com",
            "net:*a.com",
            "net:a*.com",
            "net:api.*",
            "net:*.*.a.com",
            "net:.a.com",
        ] {
            assert!(Permission::parse(bad).is_err(), "{bad}");
        }
        let bad = VALID.replace("system.cpu", "system.webcam");
        assert!(PackageManifest::parse(&bad).is_err());
    }
}
