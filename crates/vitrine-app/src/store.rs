//! The Vitrine Store: the catalogue published by `vitrine-org/store`, widget
//! previews cached on disk, and safe downloads of packs from GitHub releases
//! (allow-listed hosts, size cap, SHA-256 from the catalogue).

use std::cmp::Ordering;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const INDEX_URL: &str = "https://vitrine-org.github.io/store/index.json";
/// Previews in the catalogue are relative to this.
pub const BASE_URL: &str = "https://vitrine-org.github.io/store/";
/// Same cap as the store's own checks.
pub const MAX_PACKAGE: u64 = 50 * 1024 * 1024;
const MAX_INDEX: u64 = 8 * 1024 * 1024;
const MAX_PREVIEW: u64 = 4 * 1024 * 1024;
/// Hosts a pack download (and each of its redirects) may come from.
const DOWNLOAD_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    // GitHub's release CDN, where github.com/<repo>/releases/download redirects.
    "release-assets.githubusercontent.com",
];
const MAX_REDIRECTS: usize = 5;
/// How often the panel looks for updates on its own.
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct Catalogue {
    pub version: u32,
    #[serde(default)]
    pub generated: String,
    pub packs: Vec<StorePack>,
}

// Every field of the format is parsed, even those the panel does not show yet.
#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorePack {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: Option<String>,
    pub repo: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub official: bool,
    #[serde(default)]
    pub min_engine: Option<String>,
    /// `icons/<pack>.png|svg`, when the pack has its own icon.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub stars: u64,
    #[serde(default)]
    pub published: Option<String>,
    pub download: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub widgets: Vec<StoreWidget>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct StoreWidget {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub size: StoreSize,
    #[serde(default)]
    pub interactive: bool,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub preview: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct StoreSize {
    pub default: [u32; 2],
    #[serde(default)]
    pub min: Option<[u32; 2]>,
    #[serde(default)]
    pub max: Option<[u32; 2]>,
}

impl StorePack {
    /// Permissions of all widgets, deduplicated, in first-seen order.
    pub fn permissions(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in self.widgets.iter().flat_map(|w| &w.permissions) {
            if !out.contains(p) {
                out.push(p.clone());
            }
        }
        out
    }

    /// The pack's GitHub page; None when `repo` is not `owner/name`.
    pub fn repo_url(&self) -> Option<String> {
        let valid = self.repo.split('/').count() == 2
            && self.repo.split('/').all(|part| {
                !part.is_empty()
                    && part != ".."
                    && part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-._".contains(c))
            });
        valid.then(|| format!("https://github.com/{}", self.repo))
    }

    /// True when this Vitrine is new enough for the pack.
    pub fn supported(&self) -> bool {
        self.min_engine
            .as_deref()
            .is_none_or(|m| compare_versions(ENGINE_VERSION, m) != Ordering::Less)
    }
}

/// Parses `index.json`, dropping packs whose fields are unusable.
pub fn parse_catalogue(text: &str) -> Result<Catalogue, String> {
    let mut c: Catalogue = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if c.version != 1 {
        return Err(format!("unsupported catalogue version {}", c.version));
    }
    c.packs.retain(|p| {
        vitrine_widgets::manifest::is_valid_package_id(&p.id)
            && parse_version(&p.version).is_some()
            && p.sha256.len() == 64
            && p.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    });
    Ok(c)
}

// ── versions ──────────────────────────────────────────────────────────

pub use vitrine_widgets::manifest::compare_versions;
use vitrine_widgets::manifest::{ENGINE_VERSION, parse_version};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackState {
    NotInstalled,
    Installed,
    UpdateAvailable,
}

pub fn pack_state(store_version: &str, installed: Option<&str>) -> PackState {
    match installed {
        None => PackState::NotInstalled,
        Some(v) if compare_versions(store_version, v) == Ordering::Greater => {
            PackState::UpdateAvailable
        }
        Some(_) => PackState::Installed,
    }
}

// ── network ───────────────────────────────────────────────────────────

/// True for `https://` URLs on the download hosts (no credentials, default port).
pub fn download_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') || authority.contains(':') {
        return false;
    }
    let host = authority.to_ascii_lowercase();
    DOWNLOAD_HOSTS.contains(&host.as_str())
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .user_agent(concat!("Vitrine/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn get(url: &str, timeout: Duration, max: u64) -> Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent(concat!("Vitrine/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let response = agent.get(url).call().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let mut out = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(max + 1)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() as u64 > max {
        return Err("response too large".into());
    }
    Ok(out)
}

/// Downloads the catalogue and keeps a copy in the cache.
pub fn fetch_catalogue() -> Result<Catalogue, String> {
    let bytes = get(INDEX_URL, Duration::from_secs(15), MAX_INDEX)?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let catalogue = parse_catalogue(&text)?;
    let path = index_cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, text);
    Ok(catalogue)
}

pub fn index_cache_path() -> PathBuf {
    vitrine_widgets::paths::cache_dir().join("store-index.json")
}

/// The cached catalogue and how old it is.
pub fn cached_catalogue() -> Option<(Catalogue, Duration)> {
    let path = index_cache_path();
    let age = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .unwrap_or(Duration::MAX);
    let text = std::fs::read_to_string(&path).ok()?;
    Some((parse_catalogue(&text).ok()?, age))
}

/// Where a preview (`previews/<pack>/<widget>.png`) or a pack icon
/// (`icons/<pack>.png`) is cached; None for unsafe paths.
pub fn preview_cache_path(rel: &str) -> Option<PathBuf> {
    let known = rel.starts_with("previews/") || rel.starts_with("icons/");
    if !known || !vitrine_widgets::manifest::is_safe_relative_path(rel) {
        return None;
    }
    let mut p = vitrine_widgets::paths::cache_dir().join("store");
    for seg in rel.split('/') {
        p.push(seg);
    }
    Some(p)
}

/// Returns the cached preview file, downloading it first if needed.
pub fn preview(rel: &str) -> Option<PathBuf> {
    let path = preview_cache_path(rel)?;
    if path.is_file() {
        return Some(path);
    }
    let bytes = get(
        &format!("{BASE_URL}{rel}"),
        Duration::from_secs(15),
        MAX_PREVIEW,
    )
    .ok()?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, &bytes).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// True when `bytes` hash to `expected` (hex, any case).
pub fn verify_sha256(bytes: &[u8], expected: &str) -> bool {
    expected.len() == 64 && sha256_hex(bytes).eq_ignore_ascii_case(expected)
}

#[derive(Debug, PartialEq)]
pub enum DownloadError {
    /// The URL or a redirect left the allowed hosts.
    NotAllowed(String),
    Network(String),
    TooLarge,
    /// The file does not match the catalogue's SHA-256.
    Checksum,
}

/// Downloads a pack, following redirects only within the allowed hosts and
/// reporting `(received, total)` progress; verifies size and SHA-256.
pub fn download(
    pack: &StorePack,
    mut progress: impl FnMut(u64, u64),
) -> Result<Vec<u8>, DownloadError> {
    if pack.size > MAX_PACKAGE {
        return Err(DownloadError::TooLarge);
    }
    let agent = agent(Duration::from_secs(300));
    let mut url = pack.download.clone();
    for _ in 0..=MAX_REDIRECTS {
        if !download_allowed(&url) {
            return Err(DownloadError::NotAllowed(url));
        }
        let response = agent
            .get(&url)
            .call()
            .map_err(|e| DownloadError::Network(e.to_string()))?;
        let status = response.status();
        if status.is_redirection() {
            url = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| DownloadError::Network("redirect without location".into()))?
                .to_string();
            continue;
        }
        if !status.is_success() {
            return Err(DownloadError::Network(format!("HTTP {}", status.as_u16())));
        }
        let total = pack.size;
        let mut reader = response.into_body().into_reader().take(MAX_PACKAGE + 1);
        let mut out = Vec::with_capacity(total.min(MAX_PACKAGE) as usize);
        let mut buf = [0u8; 64 * 1024];
        progress(0, total);
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| DownloadError::Network(e.to_string()))?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            if out.len() as u64 > MAX_PACKAGE {
                return Err(DownloadError::TooLarge);
            }
            progress(out.len() as u64, total);
        }
        if !verify_sha256(&out, &pack.sha256) {
            return Err(DownloadError::Checksum);
        }
        return Ok(out);
    }
    Err(DownloadError::Network("too many redirects".into()))
}

/// Writes a verified download to the cache and installs it with the regular
/// installer (instance settings live in the layout, so upgrades keep them).
pub fn install_bytes(
    pack: &StorePack,
    bytes: &[u8],
    packages_dir: &Path,
) -> Result<vitrine_widgets::package::InstalledPackage, String> {
    let dir = vitrine_widgets::paths::cache_dir().join("downloads");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(format!("{}-{}.vitrine", pack.id, pack.version));
    let mut f = std::fs::File::create(&file).map_err(|e| e.to_string())?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    drop(f);
    let result = vitrine_widgets::package::install(&file, packages_dir)
        .map_err(|e| crate::i18n::package_error(&e));
    let _ = std::fs::remove_file(&file);
    let installed = result?;
    if installed.manifest.id != pack.id {
        return Err(format!(
            "package id {} does not match the store entry {}",
            installed.manifest.id, pack.id
        ));
    }
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An excerpt of the real `index.json`, as `scripts/build-index.mjs` writes it.
    const INDEX: &str = r#"{
  "version": 1,
  "generated": "2026-11-04T10:05:43.152Z",
  "packs": [
    {
      "id": "dev.vitrine.alnitak",
      "name": "Alnitak",
      "version": "2.1.0",
      "author": "Emanuele Frascella",
      "description": "The date as a 3×3 letter grid, the time in words, weather, calendar, to-dos, system meters and a countdown, in Hack",
      "homepage": "https://github.com/vitrine-org/alnitak",
      "repo": "vitrine-org/alnitak",
      "license": "MIT",
      "tags": ["theme", "minimal"],
      "official": true,
      "minEngine": "2.0.0",
      "stars": 0,
      "published": "2026-11-04T09:57:45Z",
      "download": "https://github.com/vitrine-org/alnitak/releases/download/v2.1.0/alnitak-2.1.0.vitrine",
      "size": 918457,
      "sha256": "6726651db3d79decb4d169275fb2c1a6f71c79d786a581c869081d017010b302",
      "widgets": [
        {
          "id": "date",
          "name": "Date grid",
          "description": "Month, day and weekday as a 3×3 grid of letters",
          "size": { "default": [3, 3], "min": [2, 2], "max": [6, 6] },
          "interactive": false,
          "permissions": [],
          "preview": "previews/dev.vitrine.alnitak/date.png"
        },
        {
          "id": "weather",
          "name": "Weather",
          "description": "Today's weather in three monospaced lines, from Open-Meteo",
          "size": { "default": [6, 2], "min": [5, 2], "max": [8, 3] },
          "interactive": false,
          "permissions": ["net:api.open-meteo.com", "net:geocoding-api.open-meteo.com"],
          "preview": "previews/dev.vitrine.alnitak/weather.png"
        },
        {
          "id": "meters",
          "name": "Meters",
          "description": "",
          "size": { "default": [4, 2], "min": [3, 2], "max": [8, 4] },
          "interactive": true,
          "permissions": ["system.cpu", "net:api.open-meteo.com"],
          "preview": "previews/dev.vitrine.alnitak/meters.png"
        }
      ]
    },
    {
      "id": "dev.you.widgets",
      "name": "Yours",
      "version": "0.3.0-beta.1",
      "author": "you",
      "description": "",
      "homepage": "https://github.com/you/my-widgets",
      "repo": "you/my-widgets",
      "license": null,
      "tags": [],
      "official": false,
      "minEngine": "9.0.0",
      "stars": 4,
      "published": "2026-10-01T00:00:00Z",
      "download": "https://github.com/you/my-widgets/releases/download/v0.3.0-beta.1/w.vitrine",
      "size": 1200,
      "sha256": "AB26651db3d79decb4d169275fb2c1a6f71c79d786a581c869081d017010b302",
      "widgets": []
    },
    {
      "id": "Not An Id",
      "name": "Broken",
      "version": "1.0.0",
      "repo": "x/y",
      "download": "https://github.com/x/y/releases/download/v1/y.vitrine",
      "size": 1,
      "sha256": "00"
    }
  ]
}"#;

    #[test]
    fn parses_the_real_index_format() {
        let c = parse_catalogue(INDEX).unwrap();
        assert_eq!(c.version, 1);
        assert_eq!(c.packs.len(), 2, "invalid entries are dropped");
        let a = &c.packs[0];
        assert_eq!(a.id, "dev.vitrine.alnitak");
        assert!(a.official);
        assert_eq!(a.license.as_deref(), Some("MIT"));
        assert_eq!(a.size, 918457);
        assert_eq!(a.widgets.len(), 3);
        assert_eq!(a.widgets[1].size.default, [6, 2]);
        assert_eq!(a.widgets[2].size.max, Some([8, 4]));
        assert!(a.widgets[2].interactive);
        assert_eq!(
            a.permissions(),
            [
                "net:api.open-meteo.com",
                "net:geocoding-api.open-meteo.com",
                "system.cpu"
            ]
        );
        assert_eq!(
            a.repo_url().as_deref(),
            Some("https://github.com/vitrine-org/alnitak")
        );
        let mut bad = a.clone();
        bad.repo = "x/../../evil?".into();
        assert_eq!(bad.repo_url(), None);
        assert!(a.supported());
        let b = &c.packs[1];
        assert_eq!(b.license, None);
        assert!(!b.supported(), "needs a newer Vitrine");
        assert!(parse_catalogue(r#"{"version":2,"packs":[]}"#).is_err());
        assert!(parse_catalogue("<html>").is_err());
    }

    #[test]
    fn semver_order() {
        use Ordering::*;
        assert_eq!(compare_versions("2.1.0", "2.0.9"), Greater);
        assert_eq!(compare_versions("2.10.0", "2.9.0"), Greater);
        assert_eq!(compare_versions("1.0.0", "1.0.0"), Equal);
        assert_eq!(compare_versions("v1.2.3", "1.2.3"), Equal);
        assert_eq!(compare_versions("1.2.3+build.5", "1.2.3"), Equal);
        assert_eq!(compare_versions("1.0.0-alpha", "1.0.0"), Less);
        // The example order from semver.org.
        let order = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for w in order.windows(2) {
            assert_eq!(compare_versions(w[0], w[1]), Less, "{} < {}", w[0], w[1]);
            assert_eq!(compare_versions(w[1], w[0]), Greater);
        }
        assert_eq!(compare_versions("junk", "1.0.0"), Less);
        assert_eq!(
            compare_versions("1.0", "0.0.1"),
            Less,
            "two parts is not semver"
        );
    }

    #[test]
    fn pack_states() {
        assert_eq!(pack_state("2.1.0", None), PackState::NotInstalled);
        assert_eq!(
            pack_state("2.1.0", Some("2.0.0")),
            PackState::UpdateAvailable
        );
        assert_eq!(pack_state("2.1.0", Some("2.1.0")), PackState::Installed);
        assert_eq!(pack_state("2.1.0", Some("2.2.0-dev")), PackState::Installed);
        assert_eq!(
            pack_state("2.1.0", Some("2.1.0-rc.1")),
            PackState::UpdateAvailable
        );
    }

    #[test]
    fn sha256_verification() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(verify_sha256(
            b"abc",
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
        ));
        assert!(!verify_sha256(
            b"abd",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ));
        assert!(!verify_sha256(b"abc", ""));
        assert!(!verify_sha256(b"abc", "ba7816bf"));
    }

    #[test]
    fn download_url_allow_list() {
        for ok in [
            "https://github.com/vitrine-org/alnitak/releases/download/v2.1.0/alnitak-2.1.0.vitrine",
            "https://objects.githubusercontent.com/github-production-release-asset/1/2?x=y",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/2",
            "https://GitHub.com/a/b",
        ] {
            assert!(download_allowed(ok), "{ok}");
        }
        for bad in [
            "http://github.com/a/b",
            "https://github.com.evil.example/a",
            "https://evil.example/github.com/a",
            "https://user@github.com/a",
            "https://github.com:8443/a",
            "https://gist.github.com/a",
            "https://raw.githubusercontent.com/a/b/main/x.vitrine",
            "https://vitrine-org.github.io/store/x.vitrine",
            "file:///C:/x.vitrine",
            "",
        ] {
            assert!(!download_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn download_refuses_disallowed_urls_before_connecting() {
        let mut pack = parse_catalogue(INDEX).unwrap().packs.remove(0);
        pack.download = "https://example.com/pack.vitrine".into();
        assert!(matches!(
            download(&pack, |_, _| {}),
            Err(DownloadError::NotAllowed(_))
        ));
        pack.download = "https://github.com/a/b.vitrine".into();
        pack.size = MAX_PACKAGE + 1;
        assert_eq!(download(&pack, |_, _| {}), Err(DownloadError::TooLarge));
    }

    #[test]
    fn preview_paths_stay_in_the_cache() {
        let p = preview_cache_path("previews/dev.vitrine.alnitak/date.png").unwrap();
        assert!(p.ends_with("store/previews/dev.vitrine.alnitak/date.png"));
        let p = preview_cache_path("icons/dev.vitrine.alnitak.png").unwrap();
        assert!(p.ends_with("store/icons/dev.vitrine.alnitak.png"));
        assert!(preview_cache_path("other/x.png").is_none());
        assert!(p.starts_with(vitrine_widgets::paths::cache_dir()));
        for bad in [
            "previews/../../x.png",
            "/previews/a.png",
            "previews\\a.png",
            "index.json",
            "C:/x.png",
        ] {
            assert!(preview_cache_path(bad).is_none(), "{bad}");
        }
    }
}
