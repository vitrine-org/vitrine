//! `vitrine.json` package manifest.

use std::collections::HashSet;
use std::fmt;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{Error, Result};

/// Name of the manifest file at the package root.
pub const MANIFEST_FILE: &str = "vitrine.json";

/// System metrics a widget may request.
pub const SYSTEM_METRICS: &[&str] = &["cpu", "memory", "battery", "drives", "processes", "network"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permission {
    /// `system.<metric>`
    System(String),
    /// `net:<host>`
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
            let ok = !h.is_empty()
                && h.len() <= 253
                && h.split('.').all(|l| {
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
    pub widgets: Vec<WidgetManifest>,
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
        Ok(())
    }

    /// Every file path referenced by the manifest (entry, settings, preview, fonts).
    pub fn referenced_files(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.fonts.iter().map(String::as_str).collect();
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
        assert!(is_safe_relative_path("widgets/clock/index.js"));
    }

    #[test]
    fn permissions() {
        assert!(Permission::parse("system.cpu").is_ok());
        assert!(Permission::parse("system.gpu").is_err());
        assert!(Permission::parse("fs.read").is_err());
        assert!(Permission::parse("net:").is_err());
        assert!(Permission::parse("net:a/b").is_err());
        let bad = VALID.replace("system.cpu", "system.webcam");
        assert!(PackageManifest::parse(&bad).is_err());
    }
}
