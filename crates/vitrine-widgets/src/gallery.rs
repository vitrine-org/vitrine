//! How the widget gallery is arranged (`gallery.json`): favourite widgets,
//! the order of the pack sections, which ones are collapsed or hidden, and
//! grid or list view. Shared by the control panel's "Add widgets" page and
//! the edit-mode sheet.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Result, fsutil};

/// Section id of the favourites (pack ids are reverse-DNS names, so this never clashes).
pub const FAVOURITES: &str = "*favourites";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Prefs {
    /// `package/widget`, in the order they were starred.
    pub favourites: Vec<String>,
    /// Collapsed section ids (packs, or [`FAVOURITES`]).
    pub collapsed: Vec<String>,
    /// Pack ids in the order the user arranged them; packs not listed follow
    /// in installation order.
    pub order: Vec<String>,
    /// Packs left out of the gallery (still installed).
    pub hidden: Vec<String>,
    /// Compact list instead of the grid of previews.
    pub list: bool,
}

/// Favourite key of a widget.
pub fn key(package: &str, widget: &str) -> String {
    format!("{package}/{widget}")
}

impl Prefs {
    /// A missing or unreadable file yields the defaults.
    pub fn load(path: &Path) -> Prefs {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fsutil::atomic_write(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn is_favourite(&self, package: &str, widget: &str) -> bool {
        let k = key(package, widget);
        self.favourites.contains(&k)
    }

    /// Stars or unstars a widget; returns whether it is now a favourite.
    pub fn toggle_favourite(&mut self, package: &str, widget: &str) -> bool {
        let k = key(package, widget);
        if let Some(i) = self.favourites.iter().position(|f| *f == k) {
            self.favourites.remove(i);
            false
        } else {
            self.favourites.push(k);
            true
        }
    }

    pub fn is_collapsed(&self, id: &str) -> bool {
        self.collapsed.iter().any(|c| c == id)
    }

    pub fn set_collapsed(&mut self, id: &str, collapsed: bool) {
        self.collapsed.retain(|c| c != id);
        if collapsed {
            self.collapsed.push(id.to_string());
        }
    }

    pub fn toggle_collapsed(&mut self, id: &str) {
        let now = !self.is_collapsed(id);
        self.set_collapsed(id, now);
    }

    pub fn is_hidden(&self, id: &str) -> bool {
        self.hidden.iter().any(|h| h == id)
    }

    pub fn set_hidden(&mut self, id: &str, hidden: bool) {
        self.hidden.retain(|h| h != id);
        if hidden {
            self.hidden.push(id.to_string());
        }
    }

    /// Installed packs (`installed`, in installation order) in the user's order.
    pub fn ordered(&self, installed: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = self
            .order
            .iter()
            .filter(|id| installed.contains(&id.as_str()))
            .cloned()
            .collect();
        for id in installed {
            if !out.iter().any(|o| o == id) {
                out.push(id.to_string());
            }
        }
        out
    }

    /// The sections shown: ordered, without the hidden packs.
    pub fn visible(&self, installed: &[&str]) -> Vec<String> {
        let mut v = self.ordered(installed);
        v.retain(|id| !self.is_hidden(id));
        v
    }

    /// Moves a pack `delta` places among the visible sections (hidden packs
    /// keep their place). False when it is already at that end.
    pub fn move_pack(&mut self, installed: &[&str], id: &str, delta: i32) -> bool {
        let mut all = self.ordered(installed);
        let visible = self.visible(installed);
        let Some(at) = visible.iter().position(|v| v == id) else {
            return false;
        };
        let to = at as i32 + delta;
        if to < 0 || to >= visible.len() as i32 {
            return false;
        }
        let other = &visible[to as usize];
        let (a, b) = (
            all.iter().position(|x| x == id).unwrap_or(0),
            all.iter().position(|x| x == other).unwrap_or(0),
        );
        all.swap(a, b);
        self.order = all;
        true
    }
}

/// Avatar colour of a pack (an RGB value), stable for its id: the panel's
/// section headers and chips and the edit-mode sidebar use the same one.
pub fn tint(id: &str) -> u32 {
    const TINTS: [u32; 8] = [
        0x0067C0, 0x8764B8, 0x00838F, 0xC239B3, 0x2D7D9A, 0xCA5010, 0x498205, 0x5D5A58,
    ];
    let h = id.bytes().fold(0x811C_9DC5u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    });
    TINTS[h as usize % TINTS.len()]
}

/// How a section shows up for the current search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionView {
    pub shown: bool,
    pub expanded: bool,
}

/// `matches`: widgets of the section passing the search and facets;
/// `filtering`: a search or facet is active; `only`: the pack chip.
/// While filtering, empty sections hide and the others open; a pack chip
/// shows only that pack's section, open.
pub fn section_view(
    prefs: &Prefs,
    id: &str,
    matches: usize,
    filtering: bool,
    only: Option<&str>,
) -> SectionView {
    let chosen = only.is_none_or(|p| p == id);
    SectionView {
        shown: chosen && matches > 0 && !prefs.is_hidden(id),
        expanded: filtering || only.is_some() || !prefs.is_collapsed(id),
    }
}

/// The outline of a widget, from the aspect ratio of its box: a widget lays
/// out by shape and only scales with size, so the gallery offers one size per
/// shape. The thresholds match the SDK's `shapeOf`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Square,
    Wide,
    Tall,
}

/// Shape of a `w` × `h` cell box (64-DIP cells with 8-DIP gaps).
pub fn shape_of((w, h): (i32, i32)) -> Shape {
    let dips = |n: i32| (72 * n.max(1) - 8) as f32;
    let aspect = dips(w) / dips(h);
    if aspect >= 1.25 {
        Shape::Wide
    } else if aspect <= 0.8 {
        Shape::Tall
    } else {
        Shape::Square
    }
}

/// The sizes a widget is offered in (cells), each kept on a `cols` × `rows`
/// grid, without duplicates, smallest first. A pack's own `variants` win;
/// otherwise the default, minimum and maximum, keeping one per [`Shape`] (the
/// default first), since a responsive widget only scales within a shape.
pub fn size_variants(
    min: (i32, i32),
    default: (i32, i32),
    max: (i32, i32),
    declared: &[(i32, i32)],
    (cols, rows): (i32, i32),
) -> Vec<(i32, i32)> {
    let fit = |(w, h): (i32, i32)| (w.clamp(1, cols.max(1)), h.clamp(1, rows.max(1)));
    let within = |&(w, h): &(i32, i32)| w >= min.0 && h >= min.1 && w <= max.0 && h <= max.1;
    let declared: Vec<(i32, i32)> = declared.iter().copied().filter(within).collect();
    let mut out: Vec<(i32, i32)> = Vec::new();
    if declared.is_empty() {
        let mut shapes = Vec::new();
        for v in [default, min, max].map(fit) {
            let shape = shape_of(v);
            if !shapes.contains(&shape) {
                shapes.push(shape);
                out.push(v);
            }
        }
    } else {
        for v in declared.into_iter().map(fit) {
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out.sort_by_key(|&(w, h)| (w * h, w));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKS: [&str; 3] = ["a", "b", "c"];

    #[test]
    fn round_trip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("vitrine-gallery-{}", std::process::id()));
        let p = dir.join("gallery.json");
        assert_eq!(Prefs::load(&p), Prefs::default());
        let mut prefs = Prefs::default();
        assert!(prefs.toggle_favourite("a", "clock"));
        prefs.toggle_collapsed("b");
        prefs.set_hidden("c", true);
        assert!(prefs.move_pack(&PACKS, "b", -1));
        prefs.list = true;
        prefs.save(&p).unwrap();
        let back = Prefs::load(&p);
        assert_eq!(back, prefs);
        assert!(back.is_favourite("a", "clock") && back.is_collapsed("b") && back.is_hidden("c"));
        assert_eq!(back.ordered(&PACKS), ["b", "a", "c"]);
        // Unknown fields and a broken file fall back gracefully.
        std::fs::write(&p, r#"{"list":true,"future":1}"#).unwrap();
        assert!(Prefs::load(&p).list);
        std::fs::write(&p, "{").unwrap();
        assert_eq!(Prefs::load(&p), Prefs::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn favourites_toggle() {
        let mut p = Prefs::default();
        assert!(p.toggle_favourite("a", "x"));
        assert!(p.toggle_favourite("b", "y"));
        assert_eq!(p.favourites, ["a/x", "b/y"]);
        assert!(!p.toggle_favourite("a", "x"));
        assert!(!p.is_favourite("a", "x"));
        assert_eq!(p.favourites, ["b/y"]);
    }

    #[test]
    fn collapse_and_hide_are_idempotent() {
        let mut p = Prefs::default();
        p.set_collapsed("a", true);
        p.set_collapsed("a", true);
        assert_eq!(p.collapsed, ["a"]);
        p.toggle_collapsed("a");
        assert!(!p.is_collapsed("a"));
        p.set_hidden("b", true);
        p.set_hidden("b", true);
        assert_eq!(p.hidden, ["b"]);
        assert_eq!(p.visible(&PACKS), ["a", "c"]);
        p.set_hidden("b", false);
        assert_eq!(p.visible(&PACKS), PACKS);
    }

    #[test]
    fn order_follows_the_user_and_skips_hidden_packs() {
        let mut p = Prefs::default();
        assert_eq!(p.ordered(&PACKS), PACKS);
        assert!(p.move_pack(&PACKS, "c", -1));
        assert_eq!(p.ordered(&PACKS), ["a", "c", "b"]);
        assert!(!p.move_pack(&PACKS, "a", -1), "already first");
        assert!(!p.move_pack(&PACKS, "b", 1), "already last");
        // A hidden pack in between is skipped over.
        p.set_hidden("c", true);
        assert!(p.move_pack(&PACKS, "b", -1));
        assert_eq!(p.visible(&PACKS), ["b", "a"]);
        // Uninstalled packs drop out; new ones come last.
        assert_eq!(p.ordered(&["a", "b", "d"]), ["b", "a", "d"]);
    }

    #[test]
    fn sections_while_searching() {
        let mut p = Prefs::default();
        p.set_collapsed("a", true);
        p.set_hidden("h", true);
        let v = |id, matches, filtering, only| section_view(&p, id, matches, filtering, only);
        // At rest: collapsed state as saved, empty sections hidden.
        assert_eq!(
            v("a", 3, false, None),
            SectionView {
                shown: true,
                expanded: false
            }
        );
        assert!(v("b", 3, false, None).expanded);
        assert!(!v(FAVOURITES, 0, false, None).shown);
        // Searching: matching sections open, the others hide.
        assert_eq!(
            v("a", 1, true, None),
            SectionView {
                shown: true,
                expanded: true
            }
        );
        assert!(!v("b", 0, true, None).shown);
        // A pack chip shows only that section, open.
        assert!(v("a", 3, false, Some("a")).expanded);
        assert!(!v("b", 3, false, Some("a")).shown);
        assert!(!v(FAVOURITES, 2, false, Some("a")).shown);
        // Hidden packs never show.
        assert!(!v("h", 5, false, None).shown);
    }

    #[test]
    fn shapes_follow_the_aspect_ratio() {
        assert_eq!(shape_of((2, 2)), Shape::Square);
        assert_eq!(shape_of((4, 4)), Shape::Square);
        assert_eq!(shape_of((3, 2)), Shape::Wide);
        assert_eq!(shape_of((6, 5)), Shape::Square);
        assert_eq!(shape_of((4, 2)), Shape::Wide);
        assert_eq!(shape_of((2, 1)), Shape::Wide);
        assert_eq!(shape_of((4, 5)), Shape::Tall);
        assert_eq!(shape_of((2, 3)), Shape::Tall);
        assert_eq!(shape_of((4, 8)), Shape::Tall);
    }

    #[test]
    fn size_variants_keep_one_size_per_shape() {
        let grid = (26, 14);
        // Same shape at three sizes: only the default.
        assert_eq!(size_variants((2, 2), (3, 3), (4, 4), &[], grid), [(3, 3)]);
        assert_eq!(size_variants((2, 1), (2, 1), (4, 2), &[], grid), [(2, 1)]);
        assert_eq!(size_variants((3, 3), (3, 3), (3, 3), &[], grid), [(3, 3)]);
        // Different shapes stay, smallest first.
        assert_eq!(
            size_variants((2, 2), (2, 2), (6, 2), &[], grid),
            [(2, 2), (6, 2)]
        );
        assert_eq!(
            size_variants((1, 1), (4, 3), (16, 4), &[], (12, 8)),
            [(1, 1), (4, 3)]
        );
        // Narrower first at the same area.
        assert_eq!(
            size_variants((2, 4), (4, 2), (4, 4), &[], grid),
            [(2, 4), (4, 2), (4, 4)]
        );
    }

    #[test]
    fn declared_variants_win_within_the_range() {
        let grid = (26, 14);
        assert_eq!(
            size_variants((2, 2), (2, 2), (4, 5), &[(4, 5), (2, 2), (4, 2)], grid),
            [(2, 2), (4, 2), (4, 5)]
        );
        // Out-of-range entries are ignored; none left falls back to shapes.
        assert_eq!(
            size_variants((2, 2), (2, 2), (4, 4), &[(8, 8)], grid),
            [(2, 2)]
        );
        // Cut to the grid.
        assert_eq!(
            size_variants((2, 2), (2, 2), (8, 8), &[(2, 2), (8, 8)], (6, 6)),
            [(2, 2), (6, 6)]
        );
    }
}
