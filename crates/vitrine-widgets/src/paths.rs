//! Well-known locations. Vitrine 1 files (`config.json`, `themes/`) live in the
//! same folder and must never be touched.

use std::path::PathBuf;

/// `%APPDATA%\Vitrine`.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|p| PathBuf::from(p).join("AppData").join("Roaming"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Vitrine")
}

pub fn packages_dir() -> PathBuf {
    data_dir().join("packages")
}

pub fn layout_path() -> PathBuf {
    data_dir().join("layout.json")
}

pub fn config_path() -> PathBuf {
    data_dir().join("settings.json")
}

/// How the widget gallery is arranged (favourites, section order).
pub fn gallery_path() -> PathBuf {
    data_dir().join("gallery.json")
}

pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// `%LOCALAPPDATA%\Vitrine\cache`: downloads and store previews, safe to delete.
pub fn cache_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData").join("Local"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Vitrine").join("cache")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_under_data_dir() {
        let d = data_dir();
        assert!(d.ends_with("Vitrine"));
        assert_eq!(layout_path(), d.join("layout.json"));
        assert_eq!(config_path(), d.join("settings.json"));
        assert_eq!(packages_dir(), d.join("packages"));
        assert_ne!(config_path(), d.join("config.json"));
        assert_eq!(logs_dir(), d.join("logs"));
    }
}
