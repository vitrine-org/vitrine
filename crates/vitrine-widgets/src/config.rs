use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Result, fsutil};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub start_with_windows: bool,
    pub edit_hotkey: String,
    pub pause_on_battery_saver: bool,
    pub language: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            edit_hotkey: "Ctrl+Alt+V".to_string(),
            pause_on_battery_saver: true,
            // `"system"` (or no value) follows the Windows display language.
            language: Some("system".into()),
        }
    }
}

impl Config {
    /// Missing file yields the defaults.
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomic save (temp file + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        fsutil::atomic_write(path, &serde_json::to_vec_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("vitrine-cfg-{}", std::process::id()));
        let p = dir.join("settings.json");
        assert_eq!(Config::load(&p).unwrap(), Config::default());
        let c = Config {
            start_with_windows: true,
            language: Some("it".into()),
            ..Config::default()
        };
        c.save(&p).unwrap();
        assert_eq!(Config::load(&p).unwrap(), c);
        std::fs::write(&p, r#"{"startWithWindows":true}"#).unwrap();
        let c2 = Config::load(&p).unwrap();
        assert!(c2.start_with_windows && c2.pause_on_battery_saver);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
