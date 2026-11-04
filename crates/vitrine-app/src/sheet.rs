//! The model of edit mode's "Add widget" sheet: the sidebar (All,
//! Favourites, then each pack), the widgets of the selection with their size
//! variants, and keyboard navigation between them. Geometry, input and
//! painting live in `edit.rs`.

use vitrine_widgets::gallery::{FAVOURITES, size_variants};

use crate::edit::GalleryItem;

/// What the sidebar has selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    All,
    Favourites,
    Pack(String),
}

impl Selection {
    /// From the key the runtime remembers between sessions of edit mode.
    pub fn from_key(key: Option<&str>) -> Selection {
        match key {
            None => Selection::All,
            Some(FAVOURITES) => Selection::Favourites,
            Some(p) => Selection::Pack(p.to_string()),
        }
    }

    pub fn key(&self) -> Option<String> {
        match self {
            Selection::All => None,
            Selection::Favourites => Some(FAVOURITES.to_string()),
            Selection::Pack(p) => Some(p.clone()),
        }
    }

    pub fn includes(&self, g: &GalleryItem) -> bool {
        match self {
            Selection::All => true,
            Selection::Favourites => g.favourite,
            Selection::Pack(p) => *p == g.package,
        }
    }
}

/// The sidebar: All, Favourites, then the packs in gallery order.
pub fn entries(packs: &[(String, String)]) -> Vec<Selection> {
    [Selection::All, Selection::Favourites]
        .into_iter()
        .chain(packs.iter().map(|(id, _)| Selection::Pack(id.clone())))
        .collect()
}

pub fn matches(g: &GalleryItem, query: &str) -> bool {
    crate::search::matches(query, &[&g.name, &g.description, &g.pack_name])
}

/// A widget of the selection and the sizes it is offered in (cells).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// Gallery index.
    pub item: usize,
    pub variants: Vec<(i32, i32)>,
}

/// The widgets passing the selection and the search, each with its minimum,
/// default and maximum size (deduplicated, kept on a `grid` of cols × rows).
pub fn groups(
    gallery: &[GalleryItem],
    selection: &Selection,
    query: &str,
    grid: (i32, i32),
) -> Vec<Group> {
    gallery
        .iter()
        .enumerate()
        .filter(|(_, g)| selection.includes(g) && matches(g, query))
        .map(|(item, g)| Group {
            item,
            variants: size_variants(g.min, g.size, g.max, &g.variants, grid),
        })
        .collect()
}

/// Which half of the sheet has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// The search field and the sidebar list.
    Side,
    /// The size variants.
    Grid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// Moves the grid cursor `(group, variant)`. Left and right walk through
/// every variant in reading order; up and down jump between widgets, keeping
/// the column where the next widget has one.
pub fn step(groups: &[Group], (g, v): (usize, usize), m: Move) -> (usize, usize) {
    if groups.is_empty() {
        return (0, 0);
    }
    let last = groups.len() - 1;
    let g = g.min(last);
    let len = |g: usize| groups[g].variants.len().max(1);
    let v = v.min(len(g) - 1);
    match m {
        Move::Right if v + 1 < len(g) => (g, v + 1),
        Move::Right if g < last => (g + 1, 0),
        Move::Left if v > 0 => (g, v - 1),
        Move::Left if g > 0 => (g - 1, len(g - 1) - 1),
        Move::Down if g < last => (g + 1, v.min(len(g + 1) - 1)),
        Move::Up if g > 0 => (g - 1, v.min(len(g - 1) - 1)),
        Move::Home => (0, 0),
        Move::End => (last, len(last) - 1),
        _ => (g, v),
    }
}

/// Key of a rendered preview: widget, size and UI language.
pub fn preview_key(g: &GalleryItem, (w, h): (i32, i32)) -> String {
    format!(
        "{}/{}@{w}x{h}:{}",
        g.package,
        g.widget,
        crate::i18n::language()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(pack: &str, widget: &str, sizes: [(i32, i32); 3], favourite: bool) -> GalleryItem {
        GalleryItem {
            package: pack.into(),
            pack_name: pack.to_uppercase(),
            widget: widget.into(),
            name: widget.into(),
            description: format!("{widget} description"),
            min: sizes[0],
            size: sizes[1],
            max: sizes[2],
            variants: Vec::new(),
            favourite,
            dir: std::path::PathBuf::new(),
            preview: None,
        }
    }

    fn gallery() -> Vec<GalleryItem> {
        vec![
            item("a", "clock", [(2, 1), (2, 1), (4, 2)], true),
            item("a", "calendar", [(3, 3), (3, 3), (4, 4)], false),
            item("b", "weather", [(4, 1), (5, 1), (8, 2)], true),
            item("b", "date", [(3, 3), (3, 3), (3, 3)], false),
        ]
    }

    #[test]
    fn selection_keys_round_trip() {
        for s in [
            Selection::All,
            Selection::Favourites,
            Selection::Pack("b".into()),
        ] {
            assert_eq!(Selection::from_key(s.key().as_deref()), s);
        }
        let packs = [("a".to_string(), "A".to_string())];
        assert_eq!(
            entries(&packs),
            [
                Selection::All,
                Selection::Favourites,
                Selection::Pack("a".into())
            ]
        );
    }

    #[test]
    fn groups_follow_selection_and_search() {
        let g = gallery();
        let ids = |s: &Selection, q: &str| -> Vec<usize> {
            groups(&g, s, q, (26, 14)).iter().map(|g| g.item).collect()
        };
        assert_eq!(ids(&Selection::All, ""), [0, 1, 2, 3]);
        assert_eq!(ids(&Selection::Favourites, ""), [0, 2]);
        assert_eq!(ids(&Selection::Pack("b".into()), ""), [2, 3]);
        assert_eq!(ids(&Selection::All, "cal"), [1]);
        assert_eq!(ids(&Selection::Favourites, "cal"), Vec::<usize>::new());
        // The pack name is searched too.
        assert_eq!(ids(&Selection::All, "B"), [2, 3]);
    }

    #[test]
    fn variants_are_deduplicated_per_widget() {
        let all = groups(&gallery(), &Selection::All, "", (26, 14));
        // One size per shape: these all keep a single outline.
        assert_eq!(all[0].variants, [(2, 1)]);
        assert_eq!(all[1].variants, [(3, 3)]);
        assert_eq!(all[2].variants, [(5, 1)]);
        assert_eq!(all[3].variants, [(3, 3)]);
        // A pack's own variants win, cut to the grid.
        let mut g = gallery();
        g[1].variants = vec![(3, 3), (4, 4)];
        let small = groups(&g, &Selection::All, "", (3, 3));
        assert_eq!(small[1].variants, [(3, 3)]);
    }

    #[test]
    fn keyboard_walks_the_variants() {
        let mut g = gallery();
        g[0].variants = vec![(2, 1), (4, 2)];
        g[1].variants = vec![(3, 3), (4, 4)];
        g[2].variants = vec![(4, 1), (5, 1), (8, 2)];
        let gs = groups(&g, &Selection::All, "", (26, 14));
        // Right runs through each widget's sizes, then on to the next widget.
        let mut c = (0, 0);
        let mut seen = vec![c];
        for _ in 0..8 {
            c = step(&gs, c, Move::Right);
            seen.push(c);
        }
        assert_eq!(
            seen,
            [
                (0, 0),
                (0, 1),
                (1, 0),
                (1, 1),
                (2, 0),
                (2, 1),
                (2, 2),
                (3, 0),
                (3, 0)
            ]
        );
        assert_eq!(step(&gs, (1, 0), Move::Left), (0, 1));
        assert_eq!(step(&gs, (0, 0), Move::Left), (0, 0));
        // Up and down keep the column when they can.
        assert_eq!(step(&gs, (1, 1), Move::Down), (2, 1));
        assert_eq!(step(&gs, (2, 2), Move::Down), (3, 0));
        assert_eq!(step(&gs, (2, 2), Move::Up), (1, 1));
        assert_eq!(step(&gs, (0, 1), Move::Up), (0, 1));
        assert_eq!(step(&gs, (2, 1), Move::Home), (0, 0));
        assert_eq!(step(&gs, (0, 0), Move::End), (3, 0));
        // A stale cursor is clamped; no groups, no cursor.
        assert_eq!(step(&gs, (9, 9), Move::Left), (2, 2));
        assert_eq!(step(&[], (2, 1), Move::Right), (0, 0));
    }
}
