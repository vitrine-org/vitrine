use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{Result, fsutil};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GridConfig {
    pub cell: f32,
    pub gap: f32,
    pub margin: f32,
}

impl Default for GridConfig {
    fn default() -> Self {
        Self {
            cell: 64.0,
            gap: 8.0,
            margin: 24.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub package: String,
    pub widget: String,
    /// Monitor device name, or `"primary"`.
    pub monitor: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    #[serde(default)]
    pub settings: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub version: u32,
    pub grid: GridConfig,
    pub instances: Vec<Instance>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            version: 1,
            grid: GridConfig::default(),
            instances: Vec::new(),
        }
    }
}

type Rect = (i32, i32, i32, i32);

fn overlaps(a: Rect, b: Rect) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

impl Layout {
    /// Missing file yields the default layout.
    pub fn load(path: &Path) -> Result<Layout> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Layout::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomic save (temp file + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        fsutil::atomic_write(path, &serde_json::to_vec_pretty(self)?)
    }

    /// Whether the rectangle `(x, y, w, h)` overlaps any instance on `monitor`,
    /// ignoring the instance whose id is `ignore` (used when moving/resizing).
    pub fn rect_collides(
        &self,
        monitor: &str,
        rect: (i32, i32, i32, i32),
        ignore: Option<&str>,
    ) -> bool {
        self.instances.iter().any(|i| {
            i.monitor == monitor
                && Some(i.id.as_str()) != ignore
                && overlaps(rect, (i.x, i.y, i.w, i.h))
        })
    }

    /// First free `w`x`h` spot in a `cols`x`rows` grid, scanning row-major.
    pub fn find_free_spot(
        &self,
        monitor: &str,
        cols: i32,
        rows: i32,
        w: i32,
        h: i32,
    ) -> Option<(i32, i32)> {
        if w < 1 || h < 1 || w > cols || h > rows {
            return None;
        }
        for y in 0..=(rows - h) {
            for x in 0..=(cols - w) {
                if !self.rect_collides(monitor, (x, y, w, h), None) {
                    return Some((x, y));
                }
            }
        }
        None
    }

    pub fn instance(&self, id: &str) -> Option<&Instance> {
        self.instances.iter().find(|i| i.id == id)
    }
}

/// Unique instance id based on time and a process-wide counter.
pub fn new_instance_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("w{ms:x}-{n:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(id: &str, monitor: &str, x: i32, y: i32, w: i32, h: i32) -> Instance {
        Instance {
            id: id.into(),
            package: "dev.x.y".into(),
            widget: "w".into(),
            monitor: monitor.into(),
            x,
            y,
            w,
            h,
            settings: Map::new(),
        }
    }

    #[test]
    fn collision_and_free_spot() {
        let mut l = Layout::default();
        l.instances.push(inst("a", "primary", 0, 0, 4, 3));
        assert!(l.rect_collides("primary", (3, 2, 2, 2), None));
        assert!(!l.rect_collides("primary", (4, 0, 2, 2), None));
        assert!(!l.rect_collides("other", (0, 0, 2, 2), None));
        assert!(!l.rect_collides("primary", (0, 0, 2, 2), Some("a")));
        assert_eq!(l.find_free_spot("primary", 8, 6, 4, 3), Some((4, 0)));
        assert_eq!(l.find_free_spot("primary", 4, 6, 4, 3), Some((0, 3)));
        assert_eq!(l.find_free_spot("primary", 4, 3, 4, 3), None);
        assert_eq!(l.find_free_spot("other", 4, 3, 4, 3), Some((0, 0)));
        assert_eq!(l.find_free_spot("primary", 4, 3, 5, 1), None);
    }

    #[test]
    fn free_spot_depends_on_the_size_asked_for() {
        // Row 0 has a 2-wide hole at x 2..4; row 1 is free from x 2.
        let l = Layout {
            instances: vec![
                inst("a", "primary", 0, 0, 2, 2),
                inst("b", "primary", 4, 0, 4, 1),
            ],
            ..Layout::default()
        };
        assert_eq!(l.find_free_spot("primary", 8, 4, 2, 1), Some((2, 0)));
        assert_eq!(l.find_free_spot("primary", 8, 4, 2, 2), Some((2, 0)));
        assert_eq!(l.find_free_spot("primary", 8, 4, 4, 2), Some((2, 1)));
        assert_eq!(l.find_free_spot("primary", 8, 4, 8, 2), Some((0, 2)));
        assert_eq!(l.find_free_spot("primary", 8, 4, 8, 3), None);
    }

    #[test]
    fn round_trip_and_missing() {
        let dir = std::env::temp_dir().join(format!("vitrine-layout-{}", std::process::id()));
        let p = dir.join("layout.json");
        assert_eq!(Layout::load(&p).unwrap(), Layout::default());
        let mut l = Layout::default();
        let mut i = inst("a", "primary", 1, 2, 3, 4);
        i.settings.insert("k".into(), Value::from(5));
        l.instances.push(i);
        l.save(&p).unwrap();
        l.save(&p).unwrap();
        assert_eq!(Layout::load(&p).unwrap(), l);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ids_are_unique() {
        assert_ne!(new_instance_id(), new_instance_id());
    }
}
