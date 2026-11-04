//! Edit mode: open windows step aside, and a translucent overlay per monitor
//! shows the grid. Widgets follow the pointer while dragged and settle into
//! their cell with an animation; the corner handle resizes them live; hover
//! controls remove a widget or open its settings. "Add widget" opens a
//! centred sheet: a sidebar (search, All, Favourites, each pack) and the
//! selection's widgets in every size they come in, previewed at their true
//! proportions; drag a size onto the desktop or press its "+" to add it.
//!
//! [`EditMode`] owns the overlay and the interaction state. It never touches
//! the layout itself: it turns pointer input into [`Action`]s for the runtime.

use std::collections::HashMap;
use std::time::Instant;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN,
    VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Interface, PCWSTR, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::grid::Geometry;
use crate::i18n::t;
use crate::runtime::HostMsg;
use crate::sheet::{self, Focus, Group, Move, Selection};
use vitrine_render::compose::WindowVisual;
use vitrine_render::gpu::Gpu;
use vitrine_render::image::{DirAssets, ImageCache};
use windows::Win32::Graphics::Imaging::IWICBitmap;

use std::collections::HashSet;
use std::sync::Arc;

const CLASS: PCWSTR = w!("VitrineEditOverlay");

pub type Cells = (i32, i32, i32, i32);

/// A widget as edit mode sees it.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub name: String,
    /// `"primary"` or a device name (normalized by the runtime).
    pub monitor: String,
    pub rect: Cells,
    pub min: (i32, i32),
    pub max: (i32, i32),
}

/// A widget that can be added from the gallery.
#[derive(Clone, Debug)]
pub struct GalleryItem {
    /// Package id.
    pub package: String,
    /// Localized package name (the gallery tabs, and searched).
    pub pack_name: String,
    pub widget: String,
    /// Localized widget name and description (both searched).
    pub name: String,
    pub description: String,
    /// Default, minimum and maximum size in cells.
    pub size: (i32, i32),
    pub min: (i32, i32),
    pub max: (i32, i32),
    /// Starred in the control panel.
    pub favourite: bool,
    pub dir: std::path::PathBuf,
    pub preview: Option<String>,
}

/// A size variant the sheet wants rendered (see [`EditMode::preview_jobs`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewJob {
    pub key: String,
    pub package: String,
    pub widget: String,
    pub size: (i32, i32),
}

#[derive(Clone, Copy, Debug)]
pub enum Input {
    Down(POINT),
    Move(POINT),
    Up(POINT),
    RightUp(POINT),
    Wheel(i16),
    Escape,
    /// A typed character (control characters are filtered out by the window).
    Char(char),
    /// Any other key press: virtual-key code, with Ctrl held or not.
    Key {
        vk: u16,
        ctrl: bool,
    },
}

/// A single-line text being edited: the text and the caret, a byte index that
/// always sits on a char boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextField {
    pub text: String,
    pub caret: usize,
}

impl TextField {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
    }

    pub fn insert(&mut self, s: &str) {
        self.text.insert_str(self.caret, s);
        self.caret += s.len();
    }

    fn prev(&self) -> usize {
        self.text[..self.caret]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.text[self.caret..]
            .chars()
            .next()
            .map_or(self.caret, |c| self.caret + c.len_utf8())
    }

    /// Start of the word before the caret, skipping the blanks right before it.
    fn word_start(&self) -> usize {
        let before = self.text[..self.caret].trim_end();
        before
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8())
    }

    /// End of the word after the caret, skipping the blanks right after it.
    fn word_end(&self) -> usize {
        let after = &self.text[self.caret..];
        let skip = after.len() - after.trim_start().len();
        let rest = &after[skip..];
        self.caret + skip + rest.find(char::is_whitespace).unwrap_or(rest.len())
    }

    fn remove(&mut self, from: usize, to: usize) -> bool {
        if from == to {
            return false;
        }
        self.text.replace_range(from..to, "");
        self.caret = from;
        true
    }

    /// Deletes the character before the caret; true when the text changed.
    pub fn backspace(&mut self) -> bool {
        self.remove(self.prev(), self.caret)
    }

    /// Ctrl+Backspace: deletes the word before the caret.
    pub fn backspace_word(&mut self) -> bool {
        self.remove(self.word_start(), self.caret)
    }

    /// Deletes the character after the caret.
    pub fn delete(&mut self) -> bool {
        let (at, next) = (self.caret, self.next());
        self.text.replace_range(at..next, "");
        at != next
    }

    pub fn left(&mut self, word: bool) {
        self.caret = if word { self.word_start() } else { self.prev() };
    }

    pub fn right(&mut self, word: bool) {
        self.caret = if word { self.word_end() } else { self.next() };
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.caret = self.text.len();
    }
}

/// What the runtime should do in response to input.
#[derive(Clone, Debug)]
pub enum Action {
    /// Show a widget at a pixel rectangle while it is dragged or resized.
    Preview {
        id: String,
        rect: RECT,
    },
    /// Drop: move the widget to `cells` on `monitor`, animating from `from`.
    Commit {
        id: String,
        monitor: String,
        cells: Cells,
        from: RECT,
    },
    /// Invalid drop: animate back to the layout position.
    Cancel {
        id: String,
        from: RECT,
    },
    Remove {
        id: String,
    },
    Settings {
        id: String,
    },
    /// Add a widget, at a cell span or (None) at `size` in the first free spot.
    Add {
        package: String,
        widget: String,
        size: (i32, i32),
        at: Option<(String, Cells)>,
    },
    Close,
}

#[derive(Clone, Debug, PartialEq)]
enum Hot {
    None,
    Frame(String),
    Remove(String),
    Settings(String),
    Handle(String),
    Add,
    Done,
    Sheet,
    /// The search field, and its clear button.
    Search,
    Clear,
    /// A sidebar entry: All, Favourites, then the packs.
    Side(usize),
    /// A size variant of the sheet (group, variant), and its "+" button.
    Tile(usize, usize),
    Plus(usize, usize),
}

#[derive(Clone, Debug)]
enum DragKind {
    Move {
        id: String,
        cells: Cells,
    },
    Resize {
        id: String,
        monitor: String,
        min: (i32, i32),
        max: (i32, i32),
    },
    /// A size variant dragged out of the sheet: gallery index, size, and
    /// where it sits in the sheet (group, variant).
    New {
        item: usize,
        size: (i32, i32),
        at: (usize, usize),
    },
}

#[derive(Clone, Debug)]
struct Drag {
    kind: DragKind,
    start: POINT,
    from: RECT,
    cur: RECT,
    target: Option<(String, Cells)>,
    valid: bool,
    moved: bool,
}

struct Overlay {
    hwnd: HWND,
    rect: RECT,
    monitor: String,
    geo: Geometry,
    text: Fonts,
    shown: bool,
}

struct Fonts {
    label: IDWriteTextFormat,
    button: IDWriteTextFormat,
    caption: IDWriteTextFormat,
    icon: IDWriteTextFormat,
    /// Count badges.
    chip: IDWriteTextFormat,
    /// Search field text (left-aligned, never trimmed).
    field: IDWriteTextFormat,
    /// Clear button glyph, and the empty-state glyph.
    icon_small: IDWriteTextFormat,
    icon_large: IDWriteTextFormat,
    /// The sheet: selection title, widget names and descriptions, sidebar
    /// labels and size captions.
    title: IDWriteTextFormat,
    name: IDWriteTextFormat,
    desc: IDWriteTextFormat,
    side: IDWriteTextFormat,
    tiny: IDWriteTextFormat,
}

/// Where the sheet's parts are, in screen pixels (at rest: the open/close
/// spring scales the whole sheet around its centre when painting).
#[derive(Clone, Debug, Default)]
struct SheetLayout {
    sheet: RECT,
    /// The sidebar pane, the search field and its clear button.
    side: RECT,
    field: RECT,
    clear: RECT,
    /// Sidebar rows: All, Favourites, then the packs.
    entries: Vec<RECT>,
    /// The selection's title row, and the scrolling area of the widgets.
    title: RECT,
    area: RECT,
    /// Widgets of the selection, scrolled.
    groups: Vec<GroupLayout>,
    /// Height of all the groups (for the scroll limit).
    content: i32,
}

/// One widget in the sheet: its size variants side by side, the size under
/// each, then its name and description.
#[derive(Clone, Debug, Default)]
struct GroupLayout {
    tiles: Vec<RECT>,
    labels: Vec<RECT>,
    name: RECT,
    desc: RECT,
    bounds: RECT,
}

/// Tallest preview tile in the sheet, DIPs.
const TILE_H: f32 = 128.0;
/// Previews are at most this fraction of the widget's real size.
const TILE_MAX_SCALE: f32 = 0.8;
const SIDEBAR_W: f32 = 240.0;
/// How long a "+" shows its check mark after adding.
const ADDED_SECS: f32 = 1.4;

/// Caret blink half-period, as in Windows' default.
const BLINK_MS: u32 = 530;

pub struct EditMode {
    overlays: Vec<Overlay>,
    /// One per overlay (kept apart so painting can borrow the rest immutably).
    visuals: Vec<WindowVisual>,
    gallery: Vec<GalleryItem>,
    /// Packages in gallery order: (id, localized name).
    packs: Vec<(String, String)>,
    /// Columns and rows of the main monitor's grid (size variants fit it).
    grid: (i32, i32),
    /// The sidebar's selection.
    selection: Selection,
    search: TextField,
    /// Blink phase starts here (reset by every edit so the caret shows while typing).
    caret_epoch: Instant,
    caret_on: bool,
    /// Widgets of the selection that pass the search, with their sizes.
    groups: Vec<Group>,
    /// Keyboard: which half of the sheet has it, the grid cursor (group,
    /// variant), and whether focus rings show (after a key, not a click).
    focus: Focus,
    cursor: (usize, usize),
    keyboard: bool,
    /// Sheet open/close spring: position (0 hidden, 1 open) and velocity.
    spring: (f32, f32),
    /// The sidebar's accent indicator: animated top and bottom, relative to the sheet top.
    pill: (f32, f32),
    /// Vertical scroll of the widgets, and where it is heading.
    scroll: f32,
    scroll_to: f32,
    /// A new widget was dragged out of the sheet, which steps aside until the drop.
    away: bool,
    /// Static previews (`preview` of the manifest) by gallery index.
    previews: HashMap<usize, Option<ID2D1Bitmap>>,
    /// Rendered previews by [`sheet::preview_key`]: premultiplied BGRA pixels
    /// as they arrive, and their bitmaps.
    pixels: HashMap<String, (u32, u32, Arc<Vec<u8>>)>,
    renders: HashMap<String, Option<ID2D1Bitmap>>,
    /// Keys already handed to the runtime for rendering.
    requested: HashSet<String>,
    /// Variants just added with their "+" (check mark feedback), by preview key.
    added: HashMap<String, Instant>,
    images: ImageCache,
    dwrite: IDWriteFactory5,
    hot: Hot,
    pressed: Hot,
    drag: Option<Drag>,
    pointer: POINT,
    gallery_open: bool,
    /// Smoothed 0..1 values for hover/press/sheet transitions.
    anim: HashMap<String, f32>,
    last_frame: Instant,
    dirty: bool,
    closing: bool,
}

pub fn register(hinst: HINSTANCE) -> Result<()> {
    vitrine_desktop::window::register_class(hinst, CLASS, proc)
}

fn color(hex: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 0xFF) as f32 / 255.0,
        g: ((hex >> 8) & 0xFF) as f32 / 255.0,
        b: (hex & 0xFF) as f32 / 255.0,
        a,
    }
}

fn mix(a: D2D1_COLOR_F, b: D2D1_COLOR_F, t: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

fn contains(r: &RECT, pt: POINT) -> bool {
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

fn overlaps(a: Cells, b: Cells) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

fn offset(r: RECT, dx: i32, dy: i32) -> RECT {
    RECT {
        left: r.left + dx,
        top: r.top + dy,
        right: r.right + dx,
        bottom: r.bottom + dy,
    }
}

fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

const ACCENT: u32 = 0x60CDFF; // Windows 11 default accent (dark theme)
const DANGER: u32 = 0xFF6B6B;

fn icon_family(dwrite: &IDWriteFactory5) -> PCWSTR {
    unsafe {
        let mut fonts = None;
        let base: &IDWriteFactory = dwrite;
        if base.GetSystemFontCollection(&mut fonts, false).is_ok()
            && let Some(f) = fonts
        {
            let (mut index, mut exists) = (0u32, BOOL(0));
            if f.FindFamilyName(w!("Segoe Fluent Icons"), &mut index, &mut exists)
                .is_ok()
                && exists.as_bool()
            {
                return w!("Segoe Fluent Icons");
            }
        }
    }
    w!("Segoe MDL2 Assets")
}

impl Fonts {
    fn new(dwrite: &IDWriteFactory5, s: f32) -> Result<Fonts> {
        let locale = windows::core::HSTRING::from(crate::i18n::locale_name());
        let make = |family: PCWSTR, size: f32, weight: DWRITE_FONT_WEIGHT, center: bool| unsafe {
            let f = dwrite.CreateTextFormat(
                family,
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size * s,
                &locale,
            )?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            if center {
                f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            }
            let trimming = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            };
            f.SetTrimming(&trimming, None)?;
            Ok::<_, windows::core::Error>(f)
        };
        let text = w!("Segoe UI Variable Text");
        let icons = icon_family(dwrite);
        let field = make(text, 14.0, DWRITE_FONT_WEIGHT_NORMAL, false)?;
        unsafe {
            let none = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_NONE,
                delimiter: 0,
                delimiterCount: 0,
            };
            field.SetTrimming(&none, None)?;
        }
        Ok(Fonts {
            label: make(text, 12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?,
            button: make(text, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?,
            caption: make(text, 13.0, DWRITE_FONT_WEIGHT_NORMAL, true)?,
            icon: make(icons, 14.0, DWRITE_FONT_WEIGHT_NORMAL, true)?,
            chip: make(text, 12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?,
            field,
            icon_small: make(icons, 10.0, DWRITE_FONT_WEIGHT_NORMAL, true)?,
            icon_large: make(icons, 28.0, DWRITE_FONT_WEIGHT_NORMAL, true)?,
            title: make(
                w!("Segoe UI Variable Display"),
                20.0,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                false,
            )?,
            name: make(text, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?,
            desc: make(text, 12.0, DWRITE_FONT_WEIGHT_NORMAL, false)?,
            side: make(text, 14.0, DWRITE_FONT_WEIGHT_NORMAL, false)?,
            tiny: make(text, 11.0, DWRITE_FONT_WEIGHT_NORMAL, true)?,
        })
    }
}

impl EditMode {
    pub fn open(
        gpu: &Gpu,
        hinst: HINSTANCE,
        monitors: Vec<(String, Geometry, RECT)>,
        gallery: Vec<GalleryItem>,
        tab: Option<String>,
    ) -> Result<EditMode> {
        let mut overlays = Vec::new();
        let mut visuals = Vec::new();
        for (monitor, geo, rect) in monitors {
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
                    CLASS,
                    w!("Vitrine — edit layout"),
                    WS_POPUP | WS_VISIBLE,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    None,
                    None,
                    Some(hinst),
                    None,
                )?
            };
            let mut visual = WindowVisual::new(gpu, hwnd)?;
            visual.set_opacity(gpu, 0.0)?;
            visuals.push(visual);
            let text = Fonts::new(&gpu.dwrite, geo.scale)?;
            overlays.push(Overlay {
                hwnd,
                rect,
                monitor,
                geo,
                text,
                shown: false,
            });
        }
        if let Some(o) = overlays.first() {
            unsafe {
                let _ = SetForegroundWindow(o.hwnd);
                let _ = SetFocus(Some(o.hwnd));
            }
        }
        Ok(EditMode::new(
            overlays,
            visuals,
            gallery,
            tab,
            ImageCache::new(gpu.wic.clone()),
            gpu.dwrite.clone(),
        ))
    }

    fn new(
        overlays: Vec<Overlay>,
        visuals: Vec<WindowVisual>,
        gallery: Vec<GalleryItem>,
        tab: Option<String>,
        images: ImageCache,
        dwrite: IDWriteFactory5,
    ) -> EditMode {
        let mut packs: Vec<(String, String)> = Vec::new();
        for g in &gallery {
            if !packs.iter().any(|(id, _)| *id == g.package) {
                packs.push((g.package.clone(), g.pack_name.clone()));
            }
        }
        // A package that is gone means "All".
        let selection = match Selection::from_key(tab.as_deref()) {
            Selection::Pack(p) if !packs.iter().any(|(id, _)| *id == p) => Selection::All,
            s => s,
        };
        let grid = overlays
            .first()
            .map_or((26, 14), |o| (o.geo.cols, o.geo.rows));
        let mut edit = EditMode {
            overlays,
            visuals,
            gallery,
            packs,
            grid,
            selection,
            search: TextField::default(),
            caret_epoch: Instant::now(),
            caret_on: true,
            groups: Vec::new(),
            focus: Focus::Side,
            cursor: (0, 0),
            keyboard: false,
            spring: (0.0, 0.0),
            pill: (0.0, 0.0),
            scroll: 0.0,
            scroll_to: 0.0,
            away: false,
            previews: HashMap::new(),
            pixels: HashMap::new(),
            renders: HashMap::new(),
            requested: HashSet::new(),
            added: HashMap::new(),
            images,
            dwrite,
            hot: Hot::None,
            pressed: Hot::None,
            drag: None,
            pointer: POINT::default(),
            gallery_open: false,
            anim: HashMap::new(),
            last_frame: Instant::now(),
            dirty: true,
            closing: false,
        };
        edit.refilter();
        edit.snap();
        edit
    }

    /// The sidebar selection, for the runtime to remember across sessions of edit mode.
    pub fn tab(&self) -> Option<String> {
        self.selection.key()
    }

    /// Fades the overlays out; the runtime drops the edit mode shortly after.
    pub fn begin_close(&mut self, gpu: &Gpu) {
        self.closing = true;
        for v in &mut self.visuals {
            let _ = v.animate_opacity(gpu, 1.0, 0.0, 0.16);
        }
    }

    /// Shows the sheet with a fresh search and the keyboard in the sidebar
    /// (the selection is kept).
    pub fn open_gallery(&mut self) {
        if !self.gallery_open {
            self.gallery_open = true;
            self.away = false;
            self.search.clear();
            self.focus = Focus::Side;
            self.keyboard = false;
            self.refilter();
            self.caret_epoch = Instant::now();
        }
        self.dirty = true;
    }

    /// Size variants the sheet shows and has not asked for yet, in the order
    /// they appear; the runtime renders them off-screen and hands them back
    /// with [`EditMode::add_preview`].
    pub fn preview_jobs(&mut self) -> Vec<PreviewJob> {
        if !self.gallery_open {
            return Vec::new();
        }
        let mut jobs = Vec::new();
        for g in &self.groups {
            let item = &self.gallery[g.item];
            for &size in &g.variants {
                let key = sheet::preview_key(item, size);
                if self.pixels.contains_key(&key) || !self.requested.insert(key.clone()) {
                    continue;
                }
                jobs.push(PreviewJob {
                    key,
                    package: item.package.clone(),
                    widget: item.widget.clone(),
                    size,
                });
            }
        }
        jobs
    }

    /// A rendered preview: premultiplied BGRA, `w` × `h` pixels.
    pub fn add_preview(&mut self, key: String, w: u32, h: u32, bgra: Arc<Vec<u8>>) {
        self.renders.remove(&key);
        self.pixels.insert(key, (w, h, bgra));
        self.dirty = true;
    }

    // ── sidebar, search and the widgets of the selection ──────────────

    fn entries(&self) -> Vec<Selection> {
        sheet::entries(&self.packs)
    }

    fn entry_index(&self) -> usize {
        self.entries()
            .iter()
            .position(|e| *e == self.selection)
            .unwrap_or(0)
    }

    fn entry_label(&self, e: &Selection) -> String {
        match e {
            Selection::All => t("edit.all_packs"),
            Selection::Favourites => t("gallery.favourites"),
            Selection::Pack(id) => self
                .packs
                .iter()
                .find(|(p, _)| p == id)
                .map_or_else(|| id.clone(), |(_, name)| name.clone()),
        }
    }

    /// Widgets of a sidebar entry that pass the search.
    fn entry_count(&self, e: &Selection) -> usize {
        self.gallery
            .iter()
            .filter(|g| e.includes(g) && sheet::matches(g, &self.search.text))
            .count()
    }

    /// Recomputes the widgets for the selection and the search. When they
    /// change, the list scrolls back to the top and fades in anew.
    fn refilter(&mut self) {
        let groups = sheet::groups(&self.gallery, &self.selection, &self.search.text, self.grid);
        if groups == self.groups {
            return;
        }
        self.groups = groups;
        self.scroll = 0.0;
        self.scroll_to = 0.0;
        self.cursor = (0, 0);
        self.anim.insert("content".into(), 0.0);
        self.dirty = true;
    }

    fn select(&mut self, i: usize) {
        if let Some(e) = self.entries().get(i).cloned() {
            self.selection = e;
            self.refilter();
        }
    }

    /// Settles the sheet's transitions at their targets (while it is hidden).
    fn snap(&mut self) {
        self.pill = self.pill_target();
        self.scroll = self.scroll_to;
    }

    /// Where the sidebar's accent indicator goes: centred on the selected row.
    fn pill_target(&self) -> (f32, f32) {
        let l = self.sheet_layout();
        let half = 8.0 * self.scale();
        l.entries.get(self.entry_index()).map_or((0.0, 0.0), |r| {
            let mid = (r.top + r.bottom) as f32 / 2.0 - l.sheet.top as f32;
            (mid - half, mid + half)
        })
    }

    /// The grid cursor, when the grid has the keyboard.
    fn focused(&self) -> Option<(usize, usize)> {
        if self.focus != Focus::Grid || self.groups.is_empty() {
            return None;
        }
        let g = self.cursor.0.min(self.groups.len() - 1);
        Some((
            g,
            self.cursor.1.min(self.groups[g].variants.len().max(1) - 1),
        ))
    }

    /// Adds a size variant at its first free spot ("+", a click, or Enter).
    fn add_variant(&mut self, g: usize, v: usize) -> Option<Action> {
        let group = self.groups.get(g)?;
        let size = *group.variants.get(v)?;
        let item = &self.gallery[group.item];
        self.added
            .insert(sheet::preview_key(item, size), Instant::now());
        Some(Action::Add {
            package: item.package.clone(),
            widget: item.widget.clone(),
            size,
            at: None,
        })
    }

    fn max_scroll(&self, l: &SheetLayout) -> f32 {
        (l.content - (l.area.bottom - l.area.top)).max(0) as f32
    }

    /// Scrolls so the widget under the keyboard cursor is in view.
    fn reveal(&mut self) {
        let Some((g, _)) = self.focused() else {
            return;
        };
        let l = self.sheet_layout();
        let Some(b) = l.groups.get(g).map(|g| g.bounds) else {
            return;
        };
        let pad = 16.0 * self.scale();
        let shift = self.scroll.round();
        let (top, bottom) = (b.top as f32 + shift, b.bottom as f32 + shift);
        let (view_top, view_bottom) = (l.area.top as f32, l.area.bottom as f32);
        if top - self.scroll_to < view_top + pad {
            self.scroll_to = top - view_top - pad;
        } else if bottom - self.scroll_to > view_bottom - pad {
            self.scroll_to = bottom - view_bottom + pad;
        }
        self.scroll_to = self.scroll_to.clamp(0.0, self.max_scroll(&l));
    }

    /// Keys while the sheet is open. Tab switches between the sidebar and the
    /// grid; in the sidebar, up and down change the selection and the other
    /// keys edit the search; in the grid, arrows (and Home, End) move between
    /// the sizes and Enter adds the one under the cursor.
    fn sheet_key(&mut self, vk: u16, ctrl: bool) -> Vec<Action> {
        let grid = self.focused().is_some();
        let movement = match vk {
            v if v == VK_LEFT.0 => Some(Move::Left),
            v if v == VK_RIGHT.0 => Some(Move::Right),
            v if v == VK_UP.0 => Some(Move::Up),
            v if v == VK_DOWN.0 => Some(Move::Down),
            v if v == VK_HOME.0 => Some(Move::Home),
            v if v == VK_END.0 => Some(Move::End),
            _ => None,
        };
        match (vk, movement) {
            (v, _) if v == VK_TAB.0 => {
                self.keyboard = true;
                self.focus = match self.focus {
                    Focus::Side if !self.groups.is_empty() => Focus::Grid,
                    _ => Focus::Side,
                };
                if let Some(c) = self.focused() {
                    self.cursor = c;
                }
                self.reveal();
            }
            (v, _) if v == VK_RETURN.0 => {
                self.keyboard = true;
                if let Some((g, v)) = self.focused() {
                    return self.add_variant(g, v).into_iter().collect();
                }
                if !self.groups.is_empty() {
                    self.focus = Focus::Grid;
                    self.cursor = (0, 0);
                    self.reveal();
                }
            }
            (_, Some(m)) if grid => {
                self.keyboard = true;
                self.cursor = sheet::step(&self.groups, self.cursor, m);
                self.reveal();
            }
            (_, Some(m @ (Move::Up | Move::Down))) => {
                self.keyboard = true;
                let last = self.entries().len() - 1;
                let i = self.entry_index();
                let next = if m == Move::Up {
                    i.saturating_sub(1)
                } else {
                    (i + 1).min(last)
                };
                self.select(next);
            }
            _ => {
                if self.edit_search(vk, ctrl) {
                    self.focus = Focus::Side;
                    self.refilter();
                }
            }
        }
        Vec::new()
    }

    /// Applies a key to the search field. True when the text changed.
    fn edit_search(&mut self, vk: u16, ctrl: bool) -> bool {
        let f = &mut self.search;
        let changed = match vk {
            v if v == VK_BACK.0 && ctrl => f.backspace_word(),
            v if v == VK_BACK.0 => f.backspace(),
            v if v == VK_DELETE.0 => f.delete(),
            v if v == VK_LEFT.0 => {
                f.left(ctrl);
                false
            }
            v if v == VK_RIGHT.0 => {
                f.right(ctrl);
                false
            }
            v if v == VK_HOME.0 => {
                f.home();
                false
            }
            v if v == VK_END.0 => {
                f.end();
                false
            }
            _ => return false,
        };
        self.caret_epoch = Instant::now();
        changed
    }

    fn clear_search(&mut self) {
        self.search.clear();
        self.caret_epoch = Instant::now();
        self.refilter();
    }

    /// Search text bounds and horizontal scroll: `(left, right, offset)` in screen
    /// pixels. The text scrolls left once the caret would leave the field.
    fn field_text(&self, l: &SheetLayout) -> (f32, f32, f32) {
        let s = self.scale();
        let left = l.field.left as f32 + 34.0 * s;
        let right = l.clear.left as f32 - 2.0 * s;
        let caret = match self.primary() {
            Some(o) => self.text_width(&self.search.text[..self.search.caret], &o.text.field),
            None => 0.0,
        };
        (left, right, (caret - (right - left) + 2.0 * s).max(0.0))
    }

    /// The char boundary nearest to screen x `x` in the search field.
    fn caret_at(&self, x: i32) -> usize {
        let Some(o) = self.primary() else {
            return 0;
        };
        let (left, _, off) = self.field_text(&self.sheet_layout());
        let want = x as f32 - left + off;
        let text = &self.search.text;
        text.char_indices()
            .map(|(i, _)| i)
            .chain([text.len()])
            .min_by(|&a, &b| {
                let da = (self.text_width(&text[..a], &o.text.field) - want).abs();
                let db = (self.text_width(&text[..b], &o.text.field) - want).abs();
                da.total_cmp(&db)
            })
            .unwrap_or(0)
    }

    fn primary(&self) -> Option<&Overlay> {
        self.overlays.first()
    }

    fn scale(&self) -> f32 {
        self.primary().map(|o| o.geo.scale).unwrap_or(1.0)
    }

    pub fn geometry(&self, monitor: &str) -> Option<Geometry> {
        self.overlays
            .iter()
            .find(|o| o.monitor.eq_ignore_ascii_case(monitor))
            .or(self.overlays.first())
            .map(|o| o.geo)
    }

    fn monitor_at(&self, pt: POINT) -> Option<(String, Geometry)> {
        self.overlays
            .iter()
            .find(|o| contains(&o.rect, pt))
            .map(|o| (o.monitor.clone(), o.geo))
    }

    // ── chrome geometry (screen pixels, primary monitor) ──────────────

    fn toolbar(&self) -> (RECT, RECT, RECT) {
        let Some(o) = self.primary() else {
            return Default::default();
        };
        let s = o.geo.scale;
        let px = |v: f32| (v * s).round() as i32;
        let label = |key: &str| self.text_width(&t(key), &o.text.button).ceil() as i32;
        let add_w = px(148.0).max(px(28.0 + 18.0) + label("edit.add_widget"));
        let done_w = px(100.0).max(px(32.0) + label("edit.done"));
        let (pad, h) = (px(6.0), px(44.0));
        let width = pad * 3 + add_w + done_w;
        let left = (o.rect.left + o.rect.right) / 2 - width / 2;
        let top = o.rect.top + px(20.0);
        let bar = RECT {
            left,
            top,
            right: left + width,
            bottom: top + h,
        };
        let add = RECT {
            left: left + pad,
            top: top + pad,
            right: left + pad + add_w,
            bottom: top + h - pad,
        };
        let done = RECT {
            left: add.right + pad,
            top: add.top,
            right: add.right + pad + done_w,
            bottom: add.bottom,
        };
        (bar, add, done)
    }

    /// Sheet opacity: it follows the spring in, and fades while a widget
    /// dragged out of it is placed.
    fn sheet_alpha(&self) -> f32 {
        (self.spring.0 * 1.6).clamp(0.0, 1.0) * (1.0 - self.a("away"))
    }

    /// The sheet answers the pointer while open and not stepped aside.
    fn sheet_live(&self) -> bool {
        self.gallery_open && !self.away
    }

    fn sheet(&self) -> RECT {
        self.sheet_layout().sheet
    }

    /// The sheet, centred in the space under the toolbar of the primary
    /// monitor: the sidebar on the left (search field, then the entries with
    /// a divider before the packs), the selection's title and its widgets on
    /// the right.
    fn sheet_layout(&self) -> SheetLayout {
        let Some(o) = self.primary() else {
            return Default::default();
        };
        let s = o.geo.scale;
        let px = |v: f32| (v * s).round() as i32;
        let (bar, _, _) = self.toolbar();
        let top_limit = bar.bottom + px(44.0);
        let grid_bottom =
            o.geo.origin.1 as i32 + (o.geo.rows as f32 * (o.geo.cell + o.geo.gap) * s) as i32;
        let bottom_limit = grid_bottom.min(o.rect.bottom - px(24.0));
        let width = px(980.0).min(o.rect.right - o.rect.left - px(96.0));
        let height = px(640.0).min(bottom_limit - top_limit).max(px(320.0));
        let left = (o.rect.left + o.rect.right) / 2 - width / 2;
        let top = top_limit + ((bottom_limit - top_limit - height) / 2).max(0);
        let sheet = RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        };
        let side = RECT {
            right: left + px(SIDEBAR_W),
            ..sheet
        };
        let field = RECT {
            left: side.left + px(12.0),
            top: top + px(14.0),
            right: side.right - px(12.0),
            bottom: top + px(46.0),
        };
        let b = field.bottom - field.top - px(8.0);
        let clear = RECT {
            left: field.right - px(4.0) - b,
            top: field.top + px(4.0),
            right: field.right - px(4.0),
            bottom: field.top + px(4.0) + b,
        };
        let mut y = field.bottom + px(14.0);
        let entries = (0..self.packs.len() + 2)
            .map(|i| {
                if i == 2 {
                    y += px(13.0);
                }
                let r = RECT {
                    left: side.left + px(8.0),
                    top: y,
                    right: side.right - px(8.0),
                    bottom: y + px(36.0),
                };
                y += px(38.0);
                r
            })
            .collect();
        let title = RECT {
            left: side.right + px(28.0),
            top: top + px(14.0),
            right: sheet.right - px(28.0),
            bottom: top + px(46.0),
        };
        let area = RECT {
            left: side.right + px(16.0),
            top: title.bottom + px(8.0),
            right: sheet.right - px(16.0),
            bottom: sheet.bottom - px(1.0),
        };
        let (groups, content) = self.group_layout(area, s);
        SheetLayout {
            sheet,
            side,
            field,
            clear,
            entries,
            title,
            area,
            groups,
            content,
        }
    }

    /// The widgets in `area`, scrolled: each one's sizes side by side at their
    /// true proportions (one scale per widget, so the sizes compare), aligned
    /// at the bottom with the size underneath, then the name and description.
    fn group_layout(&self, area: RECT, s: f32) -> (Vec<GroupLayout>, i32) {
        let px = |v: f32| (v * s).round() as i32;
        let pad = px(12.0);
        let left = area.left + pad;
        let room = (area.right - area.left - 2 * pad) as f32 / s;
        let start = area.top + pad - self.scroll.round() as i32;
        let mut y = start;
        let groups = self
            .groups
            .iter()
            .map(|g| {
                let dims: Vec<(f32, f32)> = g
                    .variants
                    .iter()
                    .map(|&(w, h)| crate::headless::cells_to_dips(w as u32, h as u32))
                    .collect();
                let gap = 20.0;
                let sum_w: f32 = dims.iter().map(|d| d.0).sum();
                let max_h = dims.iter().map(|d| d.1).fold(1.0, f32::max);
                let fit = room - gap * (dims.len() as f32 - 1.0).max(0.0);
                let k = (TILE_H / max_h)
                    .min(fit / sum_w.max(1.0))
                    .min(TILE_MAX_SCALE);
                let row = px(max_h * k);
                let mut x = left;
                let (mut tiles, mut labels) = (Vec::new(), Vec::new());
                for (w, h) in dims {
                    let (tw, th) = (px(w * k).max(px(8.0)), px(h * k).max(px(8.0)));
                    tiles.push(RECT {
                        left: x,
                        top: y + row - th,
                        right: x + tw,
                        bottom: y + row,
                    });
                    labels.push(RECT {
                        left: x - px(10.0),
                        top: y + row + px(6.0),
                        right: x + tw + px(10.0),
                        bottom: y + row + px(22.0),
                    });
                    x += tw + px(gap);
                }
                let name = RECT {
                    left,
                    top: y + row + px(28.0),
                    right: area.right - pad,
                    bottom: y + row + px(48.0),
                };
                let desc = RECT {
                    top: name.bottom,
                    bottom: name.bottom + px(18.0),
                    ..name
                };
                let bounds = RECT {
                    left,
                    top: y,
                    right: name.right,
                    bottom: desc.bottom,
                };
                y = desc.bottom + px(30.0);
                GroupLayout {
                    tiles,
                    labels,
                    name,
                    desc,
                    bounds,
                }
            })
            .collect();
        let content = if self.groups.is_empty() {
            0
        } else {
            y - px(30.0) - start + 2 * pad
        };
        (groups, content)
    }

    /// The "+" of a tile: a disc on its top-left corner.
    fn plus_rect(&self, tile: RECT) -> RECT {
        let s = self.scale();
        let (c, r) = (
            POINT {
                x: tile.left + (4.0 * s).round() as i32,
                y: tile.top + (4.0 * s).round() as i32,
            },
            (13.0 * s).round() as i32,
        );
        RECT {
            left: c.x - r,
            top: c.y - r,
            right: c.x + r,
            bottom: c.y + r,
        }
    }

    fn frame_px(&self, item: &Item) -> Option<RECT> {
        let g = self.geometry(&item.monitor)?;
        let (x, y, w, h) = item.rect;
        Some(g.rect(x, y, w, h))
    }

    /// Floating controls of a widget frame `r`: a pill above it (below when there
    /// is no room) holding the name, settings and remove; plus the resize handle.
    /// Returns `(pill, settings, remove, handle)` in screen pixels.
    fn chrome(&self, item: &Item, r: RECT) -> (RECT, RECT, RECT, RECT) {
        let o = self
            .overlays
            .iter()
            .find(|o| o.monitor.eq_ignore_ascii_case(&item.monitor))
            .or(self.overlays.first());
        let Some(o) = o else {
            return Default::default();
        };
        let s = o.geo.scale;
        let px = |v: f32| (v * s).round() as i32;
        let name_w = self.text_width(&item.name, &o.text.label).ceil() as i32;
        let (h, btn, pad) = (px(34.0), px(26.0), px(4.0));
        let width = px(12.0) + name_w.min(px(180.0)) + px(8.0) + btn + px(2.0) + btn + pad;
        let above = r.top - px(8.0) - h >= o.rect.top + px(2.0);
        let top = if above {
            r.top - px(8.0) - h
        } else {
            r.bottom + px(8.0)
        };
        let right = r.right.max(r.left + width);
        let pill = RECT {
            left: right - width,
            top,
            right,
            bottom: top + h,
        };
        let by = top + (h - btn) / 2;
        let remove = RECT {
            left: right - pad - btn,
            top: by,
            right: right - pad,
            bottom: by + btn,
        };
        let settings = offset(remove, -(btn + px(2.0)), 0);
        let hz = px(26.0);
        let handle = RECT {
            left: r.right - hz,
            top: r.bottom - hz,
            right: r.right + px(4.0),
            bottom: r.bottom + px(4.0),
        };
        (pill, settings, remove, handle)
    }

    fn text_width(&self, text: &str, format: &IDWriteTextFormat) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            if let Ok(layout) = self.dwrite.CreateTextLayout(&wide, format, 4096.0, 256.0) {
                let mut m = DWRITE_TEXT_METRICS::default();
                if layout.GetMetrics(&mut m).is_ok() {
                    return m.widthIncludingTrailingWhitespace;
                }
            }
        }
        0.0
    }

    fn hit(&self, items: &[Item], pt: POINT) -> Hot {
        let (bar, add, done) = self.toolbar();
        if contains(&add, pt) {
            return Hot::Add;
        }
        if contains(&done, pt) {
            return Hot::Done;
        }
        if contains(&bar, pt) {
            return Hot::None;
        }
        if self.sheet_live() {
            let l = self.sheet_layout();
            if contains(&l.sheet, pt) {
                if !self.search.is_empty() && contains(&l.clear, pt) {
                    return Hot::Clear;
                }
                if contains(&l.field, pt) {
                    return Hot::Search;
                }
                if let Some(i) = l.entries.iter().position(|r| contains(r, pt)) {
                    return Hot::Side(i);
                }
                if contains(&l.area, pt) {
                    for (g, gl) in l.groups.iter().enumerate() {
                        for (v, tile) in gl.tiles.iter().enumerate() {
                            if contains(&self.plus_rect(*tile), pt) {
                                return Hot::Plus(g, v);
                            }
                            if contains(tile, pt) {
                                return Hot::Tile(g, v);
                            }
                        }
                    }
                }
                return Hot::Sheet;
            }
        }
        // The floating pill of the hovered widget sits above other frames.
        for item in items.iter().rev() {
            if self.a(&format!("frame:{}", item.id)) < 0.05 {
                continue;
            }
            let Some(r) = self.frame_px(item) else {
                continue;
            };
            let (pill, settings, remove, _) = self.chrome(item, r);
            if contains(&remove, pt) {
                return Hot::Remove(item.id.clone());
            }
            if contains(&settings, pt) {
                return Hot::Settings(item.id.clone());
            }
            if contains(&pill, pt) {
                return Hot::Frame(item.id.clone());
            }
        }
        for item in items.iter().rev() {
            let Some(r) = self.frame_px(item) else {
                continue;
            };
            let (_, _, _, handle) = self.chrome(item, r);
            if contains(&handle, pt) {
                return Hot::Handle(item.id.clone());
            }
            if contains(&r, pt) {
                return Hot::Frame(item.id.clone());
            }
        }
        Hot::None
    }

    // ── input ─────────────────────────────────────────────────────────

    pub fn input(&mut self, input: Input, items: &[Item]) -> Vec<Action> {
        if self.closing {
            return Vec::new();
        }
        self.dirty = true;
        let mut actions = Vec::new();
        match input {
            // The first Esc closes the sheet, the next one leaves edit mode.
            Input::Escape => {
                if self.gallery_open {
                    self.gallery_open = false;
                } else {
                    actions.push(Action::Close);
                }
            }
            Input::Char(c) => {
                // While the sheet is open, typing goes to the search field.
                if self.gallery_open && !c.is_control() {
                    self.search.insert(c.encode_utf8(&mut [0; 4]));
                    self.caret_epoch = Instant::now();
                    self.focus = Focus::Side;
                    self.refilter();
                }
            }
            Input::Key { vk, ctrl } => {
                if ctrl && vk == u16::from(b'F') {
                    self.open_gallery();
                    self.focus = Focus::Side;
                } else if self.gallery_open {
                    actions.extend(self.sheet_key(vk, ctrl));
                }
            }
            Input::Wheel(delta) => {
                let l = self.sheet_layout();
                if self.sheet_live() && contains(&l.area, self.pointer) {
                    let step = delta as f32 / 120.0 * 96.0 * self.scale();
                    self.scroll_to = (self.scroll_to - step).clamp(0.0, self.max_scroll(&l));
                }
            }
            Input::Down(pt) => {
                self.pointer = pt;
                let hot = self.hit(items, pt);
                self.pressed = hot.clone();
                self.keyboard = false;
                if hot == Hot::Search {
                    self.search.caret = self.caret_at(pt.x);
                    self.caret_epoch = Instant::now();
                    self.focus = Focus::Side;
                }
                let found = |id: &str| {
                    items
                        .iter()
                        .find(|i| i.id == id)
                        .and_then(|i| Some((i, self.frame_px(i)?)))
                };
                self.drag = match &hot {
                    Hot::Frame(id) => found(id).map(|(it, r)| Drag {
                        kind: DragKind::Move {
                            id: id.clone(),
                            cells: it.rect,
                        },
                        start: pt,
                        from: r,
                        cur: r,
                        target: Some((it.monitor.clone(), it.rect)),
                        valid: true,
                        moved: false,
                    }),
                    Hot::Handle(id) => found(id).map(|(it, r)| Drag {
                        kind: DragKind::Resize {
                            id: id.clone(),
                            monitor: it.monitor.clone(),
                            min: it.min,
                            max: it.max,
                        },
                        start: pt,
                        from: r,
                        cur: r,
                        target: Some((it.monitor.clone(), it.rect)),
                        valid: true,
                        moved: false,
                    }),
                    &Hot::Tile(g, v) | &Hot::Plus(g, v) => {
                        self.focus = Focus::Grid;
                        self.cursor = (g, v);
                        let l = self.sheet_layout();
                        let r = l.groups[g].tiles[v];
                        Some(Drag {
                            kind: DragKind::New {
                                item: self.groups[g].item,
                                size: self.groups[g].variants[v],
                                at: (g, v),
                            },
                            start: pt,
                            from: r,
                            cur: r,
                            target: None,
                            valid: false,
                            moved: false,
                        })
                    }
                    _ => None,
                };
            }
            Input::Move(pt) => {
                self.pointer = pt;
                if let Some(drag) = self.drag.clone() {
                    if let Some(a) = self.drag_to(drag, pt, items) {
                        actions.push(a);
                    }
                } else {
                    let hot = self.hit(items, pt);
                    let cursor = match &hot {
                        Hot::Handle(_) => IDC_SIZENWSE,
                        Hot::Frame(_) => IDC_SIZEALL,
                        Hot::Search => IDC_IBEAM,
                        Hot::Add
                        | Hot::Done
                        | Hot::Remove(_)
                        | Hot::Settings(_)
                        | Hot::Tile(..)
                        | Hot::Plus(..)
                        | Hot::Clear
                        | Hot::Side(_) => IDC_HAND,
                        _ => IDC_ARROW,
                    };
                    unsafe {
                        let _ = SetCursor(LoadCursorW(None, cursor).ok());
                    }
                    self.hot = hot;
                }
            }
            Input::Up(pt) => {
                let pressed = std::mem::replace(&mut self.pressed, Hot::None);
                let released_on = self.hit(items, pt);
                if let Some(drag) = self.drag.take() {
                    if drag.moved {
                        actions.extend(self.drop(drag));
                        self.hot = self.hit(items, pt);
                        return actions;
                    }
                    if let DragKind::New { at: (g, v), .. } = drag.kind {
                        // A click on a size (or its "+"): the first free place.
                        actions.extend(self.add_variant(g, v));
                        return actions;
                    }
                }
                if pressed == released_on {
                    match pressed {
                        Hot::Done => actions.push(Action::Close),
                        Hot::Add if self.gallery_open => self.gallery_open = false,
                        Hot::Add => self.open_gallery(),
                        Hot::Clear => self.clear_search(),
                        Hot::Side(i) => {
                            self.focus = Focus::Side;
                            self.select(i);
                        }
                        Hot::Remove(id) => actions.push(Action::Remove { id }),
                        Hot::Settings(id) => actions.push(Action::Settings { id }),
                        _ => {}
                    }
                }
            }
            Input::RightUp(pt) => {
                if let Hot::Frame(id) | Hot::Remove(id) | Hot::Settings(id) | Hot::Handle(id) =
                    self.hit(items, pt)
                {
                    actions.push(Action::Remove { id });
                }
            }
        }
        actions
    }

    fn collides(items: &[Item], monitor: &str, cells: Cells, ignore: Option<&str>) -> bool {
        items.iter().any(|i| {
            Some(i.id.as_str()) != ignore
                && i.monitor.eq_ignore_ascii_case(monitor)
                && overlaps(i.rect, cells)
        })
    }

    fn drag_to(&mut self, mut drag: Drag, pt: POINT, items: &[Item]) -> Option<Action> {
        let (dx, dy) = (pt.x - drag.start.x, pt.y - drag.start.y);
        let threshold = (4.0 * self.scale()) as i32;
        if !drag.moved && dx.abs() < threshold && dy.abs() < threshold {
            return None;
        }
        drag.moved = true;
        let action = match &drag.kind {
            DragKind::Move { id, cells } => {
                drag.cur = offset(drag.from, dx, dy);
                if let Some((monitor, g)) = self.monitor_at(pt) {
                    let (x, y) = g.snap(drag.cur.left as f32, drag.cur.top as f32);
                    let target = g.clamp(x, y, cells.2, cells.3);
                    drag.valid = !Self::collides(items, &monitor, target, Some(id));
                    drag.target = Some((monitor, target));
                }
                Some(Action::Preview {
                    id: id.clone(),
                    rect: drag.cur,
                })
            }
            DragKind::Resize {
                id,
                monitor,
                min,
                max,
            } => {
                let g = self.geometry(monitor)?;
                let min_px = g.rect(0, 0, min.0, min.1);
                let max_px = g.rect(0, 0, max.0.min(g.cols), max.1.min(g.rows));
                let w = ((drag.from.right - drag.from.left) + dx).clamp(
                    min_px.right - min_px.left,
                    (max_px.right - max_px.left).max(min_px.right - min_px.left),
                );
                let h = ((drag.from.bottom - drag.from.top) + dy).clamp(
                    min_px.bottom - min_px.top,
                    (max_px.bottom - max_px.top).max(min_px.bottom - min_px.top),
                );
                drag.cur = RECT {
                    left: drag.from.left,
                    top: drag.from.top,
                    right: drag.from.left + w,
                    bottom: drag.from.top + h,
                };
                let (x, y) = g.snap(drag.from.left as f32, drag.from.top as f32);
                let cw = g.span(w as f32).clamp(min.0, max.0);
                let ch = g.span(h as f32).clamp(min.1, max.1);
                let target = g.clamp(x, y, cw, ch);
                drag.valid = !Self::collides(items, monitor, target, Some(id));
                drag.target = Some((monitor.clone(), target));
                Some(Action::Preview {
                    id: id.clone(),
                    rect: drag.cur,
                })
            }
            DragKind::New { size, .. } => {
                // Inside the sheet the tile just follows the pointer; once it
                // leaves, the sheet steps aside and the widget snaps to the grid.
                let sheet = self.sheet();
                if self.gallery_open && !contains(&sheet, pt) {
                    self.away = true;
                }
                let size = *size;
                match self
                    .monitor_at(pt)
                    .filter(|_| self.away || !self.gallery_open)
                {
                    Some((monitor, g)) => {
                        // The ghost is the widget at the chosen size, centred on the pointer.
                        let r = g.rect(0, 0, size.0, size.1);
                        let (w, h) = (r.right - r.left, r.bottom - r.top);
                        drag.cur = RECT {
                            left: pt.x - w / 2,
                            top: pt.y - h / 2,
                            right: pt.x + w / 2,
                            bottom: pt.y + h / 2,
                        };
                        let (x, y) = g.snap(drag.cur.left as f32, drag.cur.top as f32);
                        let target = g.clamp(x, y, size.0, size.1);
                        drag.valid = !Self::collides(items, &monitor, target, None);
                        drag.target = Some((monitor, target));
                    }
                    None => {
                        drag.cur = offset(drag.from, dx, dy);
                        drag.target = None;
                        drag.valid = false;
                    }
                }
                None
            }
        };
        self.drag = Some(drag);
        action
    }

    fn drop(&mut self, drag: Drag) -> Vec<Action> {
        match drag.kind {
            DragKind::Move { id, .. } | DragKind::Resize { id, .. } => match drag.target {
                Some((monitor, cells)) if drag.valid => vec![Action::Commit {
                    id,
                    monitor,
                    cells,
                    from: drag.cur,
                }],
                _ => vec![Action::Cancel { id, from: drag.cur }],
            },
            DragKind::New { item, size, .. } => match drag.target {
                // Dropped in place: the sheet closes (it stays out of the way
                // while it does). Anywhere else it comes back.
                Some((monitor, cells)) if drag.valid => {
                    let g = &self.gallery[item];
                    self.gallery_open = false;
                    vec![Action::Add {
                        package: g.package.clone(),
                        widget: g.widget.clone(),
                        size,
                        at: Some((monitor, cells)),
                    }]
                }
                _ => {
                    self.away = false;
                    Vec::new()
                }
            },
        }
    }

    // ── painting ──────────────────────────────────────────────────────

    /// Advances transitions; true while any of them is still moving.
    fn step(&mut self) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        let dragging = self.drag.as_ref().is_some_and(|d| d.moved);
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        let mut wanted: HashMap<String, f32> = HashMap::new();
        for k in self.anim.keys() {
            wanted.insert(k.clone(), 0.0);
        }
        wanted.insert("drag".into(), flag(dragging));
        wanted.insert("away".into(), flag(self.away));
        wanted.insert("hot:add".into(), flag(self.hot == Hot::Add));
        wanted.insert("hot:done".into(), flag(self.hot == Hot::Done));
        match &self.hot {
            Hot::Frame(id) | Hot::Handle(id) if !dragging => {
                wanted.insert(format!("frame:{id}"), 1.0);
            }
            Hot::Remove(id) if !dragging => {
                wanted.insert(format!("frame:{id}"), 1.0);
                wanted.insert(format!("btn:remove:{id}"), 1.0);
            }
            Hot::Settings(id) if !dragging => {
                wanted.insert(format!("frame:{id}"), 1.0);
                wanted.insert(format!("btn:settings:{id}"), 1.0);
            }
            Hot::Tile(g, v) if !dragging => {
                wanted.insert(format!("tile:{g}:{v}"), 1.0);
            }
            Hot::Plus(g, v) if !dragging => {
                wanted.insert(format!("tile:{g}:{v}"), 1.0);
                wanted.insert(format!("plus:{g}:{v}"), 1.0);
            }
            Hot::Side(i) => {
                wanted.insert(format!("side:{i}"), 1.0);
            }
            _ => {}
        }
        if self.keyboard
            && let Some((g, v)) = self.focused()
        {
            wanted.insert(format!("ring:{g}:{v}"), 1.0);
        }
        let open = self.gallery_open;
        wanted.insert("focus".into(), flag(open && self.focus == Focus::Side));
        wanted.insert("clear".into(), flag(!self.search.is_empty()));
        wanted.insert("hot:search".into(), flag(self.hot == Hot::Search));
        wanted.insert("hot:clear".into(), flag(self.hot == Hot::Clear));
        wanted.insert("empty".into(), flag(open && self.groups.is_empty()));
        wanted.insert("content".into(), 1.0);
        let mut moving = false;
        let rate = 1.0 - (-dt * 16.0).exp();
        for (k, target) in wanted {
            let v = self.anim.entry(k.clone()).or_insert(0.0);
            // The content fades in a little slower than hovers.
            let r = if k == "content" {
                1.0 - (-dt * 11.0).exp()
            } else {
                rate
            };
            let next = *v + (target - *v) * r;
            if (next - target).abs() < 0.004 {
                *v = target;
                let transient = ["frame:", "btn:", "tile:", "plus:", "side:", "ring:"]
                    .iter()
                    .any(|p| k.starts_with(p));
                if target == 0.0 && transient {
                    self.anim.remove(&k);
                }
            } else {
                *v = next;
                moving = true;
            }
        }
        self.added
            .retain(|_, at| at.elapsed().as_secs_f32() < ADDED_SECS);
        moving |= !self.added.is_empty();
        moving |= self.step_sheet(dt);
        moving
    }

    /// The open/close spring, the sidebar indicator and the scroll.
    fn step_sheet(&mut self, dt: f32) -> bool {
        // An underdamped spring: the sheet pops open with a slight overshoot.
        let target = if self.gallery_open { 1.0 } else { 0.0 };
        let (stiffness, damping) = (420.0, 2.0 * 420.0f32.sqrt() * 0.72);
        let mut moving = false;
        if (self.spring.0 - target).abs() > 0.0005 || self.spring.1.abs() > 0.005 {
            let n = (dt / (1.0 / 240.0)).ceil().max(1.0) as usize;
            let h = dt / n as f32;
            for _ in 0..n {
                let a = stiffness * (target - self.spring.0) - damping * self.spring.1;
                self.spring.1 += a * h;
                self.spring.0 += self.spring.1 * h;
            }
            moving = true;
        } else {
            self.spring = (target, 0.0);
        }
        if !self.gallery_open && self.spring.0 <= 0.0005 {
            self.snap();
            return moving;
        }
        let approach = |v: &mut f32, target: f32, rate: f32, eps: f32| {
            let next = *v + (target - *v) * rate;
            if (next - target).abs() < eps {
                *v = target;
                false
            } else {
                *v = next;
                true
            }
        };
        // The indicator stretches towards the new row: its leading edge runs ahead.
        let (tt, tb) = self.pill_target();
        let (fast, slow) = (1.0 - (-dt * 24.0).exp(), 1.0 - (-dt * 11.0).exp());
        let down = tb > self.pill.1;
        let (rt, rb) = if down { (slow, fast) } else { (fast, slow) };
        moving |= approach(&mut self.pill.0, tt, rt, 0.5);
        moving |= approach(&mut self.pill.1, tb, rb, 0.5);
        let to = self.scroll_to;
        moving |= approach(&mut self.scroll, to, 1.0 - (-dt * 18.0).exp(), 0.5);
        moving
    }

    fn a(&self, key: &str) -> f32 {
        *self.anim.get(key).unwrap_or(&0.0)
    }

    /// How fresh the check mark of a just-added variant is: 1 right after, 0 when gone.
    fn added_t(&self, key: &str) -> f32 {
        self.added
            .get(key)
            .map_or(0.0, |at| 1.0 - at.elapsed().as_secs_f32() / ADDED_SECS)
            .max(0.0)
    }

    /// The decoded preview of gallery item `i`. Cached per (package, widget):
    /// packs reuse paths like `src/weather/preview.png`, and keying by that path
    /// alone showed one pack's preview on every pack's card.
    fn preview_image(&mut self, i: usize) -> Option<IWICBitmap> {
        let item = &self.gallery[i];
        let rel = item.preview.as_ref()?;
        let key = format!("{}|{}|{rel}", item.package, item.widget);
        self.images
            .get_keyed(&key, rel, &DirAssets(item.dir.clone()))
    }

    fn preview_bitmap(&mut self, dc: &ID2D1DeviceContext, i: usize) -> Option<ID2D1Bitmap> {
        if let Some(b) = self.previews.get(&i) {
            return b.clone();
        }
        let bmp = self.preview_image(i).and_then(|wic| unsafe {
            dc.CreateBitmapFromWicBitmap(&wic, None)
                .ok()
                .and_then(|b| b.cast().ok())
        });
        self.previews.insert(i, bmp.clone());
        bmp
    }

    /// Turns rendered previews that arrived into bitmaps.
    fn upload_renders(&mut self, dc: &ID2D1DeviceContext) {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: windows::Win32::Graphics::Direct2D::Common::D2D1_PIXEL_FORMAT {
                format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode:
                    windows::Win32::Graphics::Direct2D::Common::D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        for (key, (w, h, bgra)) in &self.pixels {
            if self.renders.contains_key(key) {
                continue;
            }
            let bmp = unsafe {
                dc.CreateBitmap(
                    windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U {
                        width: *w,
                        height: *h,
                    },
                    Some(bgra.as_ptr() as *const _),
                    w * 4,
                    &props,
                )
            }
            .ok()
            .and_then(|b| b.cast().ok());
            self.renders.insert(key.clone(), bmp);
        }
    }

    /// The best preview of a variant: its rendering, else the manifest's picture.
    fn variant_bitmap(&self, item: usize, size: (i32, i32)) -> (Option<&ID2D1Bitmap>, bool) {
        let key = sheet::preview_key(&self.gallery[item], size);
        match self.renders.get(&key).and_then(Option::as_ref) {
            Some(b) => (Some(b), true),
            None => (self.previews.get(&item).and_then(Option::as_ref), false),
        }
    }

    /// Repaints when needed. Returns how soon the loop should call again.
    pub fn paint(&mut self, gpu: &Gpu, dc: &ID2D1DeviceContext, items: &[Item]) -> u32 {
        let moving = self.step();
        // The caret blinks while the search field has focus (the sheet is open).
        let blink = (self.gallery_open && !self.closing && self.focus == Focus::Side).then(|| {
            let ms = self.caret_epoch.elapsed().as_millis();
            let on = (ms / BLINK_MS as u128).is_multiple_of(2);
            if on != self.caret_on {
                self.caret_on = on;
                self.dirty = true;
            }
            (BLINK_MS - (ms % BLINK_MS as u128) as u32).max(1)
        });
        let idle = blink.unwrap_or(u32::MAX);
        if !self.dirty && !moving {
            return idle;
        }
        self.dirty = false;
        for i in 0..self.gallery.len() {
            self.preview_bitmap(dc, i);
        }
        self.upload_renders(dc);
        let mut visuals = std::mem::take(&mut self.visuals);
        for (i, visual) in visuals.iter_mut().enumerate() {
            let rect = self.overlays[i].rect;
            let (w, h) = (
                (rect.right - rect.left) as u32,
                (rect.bottom - rect.top) as u32,
            );
            let painter = Painter {
                edit: self,
                index: i,
                items,
            };
            let result = visual.draw(gpu, dc, w, h, |dc, origin| {
                let [_, _, _, _, ox, oy] = origin.0;
                painter.paint(dc, ox - rect.left as f32, oy - rect.top as f32)
            });
            if let Err(e) = result {
                log::error!("edit overlay: {e}");
            }
            if !self.overlays[i].shown {
                self.overlays[i].shown = true;
                let _ = visual.animate_opacity(gpu, 0.0, 1.0, 0.22);
            }
        }
        self.visuals = visuals;
        if moving { 16 } else { idle }
    }
}

/// Draws one overlay. Coordinates are screen pixels shifted by (`dx`, `dy`).
struct Painter<'a> {
    edit: &'a EditMode,
    index: usize,
    items: &'a [Item],
}

impl Painter<'_> {
    fn paint(&self, dc: &ID2D1DeviceContext, dx: f32, dy: f32) -> Result<()> {
        let e = self.edit;
        let o = &e.overlays[self.index];
        let s = o.geo.scale;
        let rf = |r: RECT| D2D_RECT_F {
            left: r.left as f32 + dx,
            top: r.top as f32 + dy,
            right: r.right as f32 + dx,
            bottom: r.bottom as f32 + dy,
        };
        let rr = |r: D2D_RECT_F, radius: f32| D2D1_ROUNDED_RECT {
            rect: r,
            radiusX: radius * s,
            radiusY: radius * s,
        };
        unsafe {
            dc.SetTransform(&Matrix3x2::identity());
            let brush = dc.CreateSolidColorBrush(&color(0, 0.0), None)?;
            let fill = |r: D2D1_ROUNDED_RECT, c: D2D1_COLOR_F| {
                brush.SetColor(&c);
                dc.FillRoundedRectangle(&r, &brush);
            };
            let stroke = |r: D2D1_ROUNDED_RECT, c: D2D1_COLOR_F, width: f32| {
                brush.SetColor(&c);
                dc.DrawRoundedRectangle(&r, &brush, width * s, None);
            };
            let draw_text = |t: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, c: D2D1_COLOR_F| {
                brush.SetColor(&c);
                let text: Vec<u16> = t.encode_utf16().collect();
                dc.DrawText(
                    &text,
                    f,
                    &r,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            };
            let circle = |c: Vector2, radius: f32, col: D2D1_COLOR_F| {
                brush.SetColor(&col);
                dc.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: c,
                        radiusX: radius,
                        radiusY: radius,
                    },
                    &brush,
                );
            };
            // Soft shadow: a few expanding translucent rounded rects.
            let shadow = |r: D2D_RECT_F, radius: f32, strength: f32| {
                for k in 1..=5 {
                    let g = k as f32 * 2.5 * s;
                    let r = D2D_RECT_F {
                        left: r.left - g,
                        top: r.top - g + 3.0 * s,
                        right: r.right + g,
                        bottom: r.bottom + g + 3.0 * s,
                    };
                    fill(
                        rr(r, radius + k as f32 * 2.5),
                        color(0, strength / k as f32),
                    );
                }
            };

            // Scrim: the desktop stays readable underneath.
            let full = rf(o.rect);
            brush.SetColor(&color(0x000000, 0.30));
            dc.FillRectangle(&full, &brush);

            // Grid cells, brighter while dragging.
            let cell_c = color(0xFFFFFF, 0.05 + 0.05 * e.a("drag"));
            for y in 0..o.geo.rows {
                for x in 0..o.geo.cols {
                    fill(rr(rf(o.geo.rect(x, y, 1, 1)), 6.0), cell_c);
                }
            }

            let drag = e.drag.as_ref().filter(|d| d.moved);
            // Drop target.
            if let Some(d) = drag
                && let Some((monitor, (x, y, w, h))) = &d.target
                && monitor.eq_ignore_ascii_case(&o.monitor)
            {
                let r = rf(o.geo.rect(*x, *y, *w, *h));
                let tint = if d.valid { ACCENT } else { DANGER };
                fill(rr(r, 10.0), color(tint, 0.16));
                stroke(rr(r, 10.0), color(tint, 0.85), 1.5);
            }

            // Widget frames.
            let dragged_id = drag.and_then(|d| match &d.kind {
                DragKind::Move { id, .. } | DragKind::Resize { id, .. } => Some(id.as_str()),
                DragKind::New { .. } => None,
            });
            for item in self
                .items
                .iter()
                .filter(|i| i.monitor.eq_ignore_ascii_case(&o.monitor))
            {
                let Some(px) = e.frame_px(item) else { continue };
                let dragged = dragged_id == Some(item.id.as_str());
                let r = match (dragged, drag) {
                    (true, Some(d)) => rf(d.cur),
                    _ => rf(px),
                };
                let hover = e.a(&format!("frame:{}", item.id));
                if dragged {
                    shadow(r, 10.0, 0.10);
                }
                fill(
                    rr(r, 10.0),
                    color(ACCENT, if dragged { 0.10 } else { 0.03 + 0.07 * hover }),
                );
                stroke(
                    rr(r, 10.0),
                    mix(
                        color(0xFFFFFF, 0.28),
                        color(ACCENT, 1.0),
                        if dragged { 1.0 } else { hover },
                    ),
                    if dragged { 2.0 } else { 1.0 + hover },
                );
                // Floating pill with name, settings and remove (hover only).
                if !dragged && hover > 0.01 {
                    let (pill, settings, remove, _) = e.chrome(item, px);
                    // Slides 4 px into place as it fades in.
                    let lift = ((1.0 - hover) * 4.0 * s).round() as i32;
                    let (pill, settings, remove) = (
                        offset(pill, 0, lift),
                        offset(settings, 0, lift),
                        offset(remove, 0, lift),
                    );
                    let p = rf(pill);
                    shadow(p, 17.0, 0.10 * hover);
                    fill(rr(p, 17.0), color(0x1F1F1F, 0.96 * hover));
                    stroke(rr(p, 17.0), color(0xFFFFFF, 0.10 * hover), 1.0);
                    let tr = D2D_RECT_F {
                        left: p.left + 12.0 * s,
                        right: rf(settings).left - 6.0 * s,
                        ..p
                    };
                    draw_text(&item.name, &o.text.label, tr, color(0xFFFFFF, hover));
                    for (btn, glyph, key, tint) in [
                        (remove, "\u{E711}", "remove", DANGER),
                        (settings, "\u{E713}", "settings", ACCENT),
                    ] {
                        let h = e.a(&format!("btn:{key}:{}", item.id));
                        let b = rf(btn);
                        let c = Vector2 {
                            X: (b.left + b.right) / 2.0,
                            Y: (b.top + b.bottom) / 2.0,
                        };
                        circle(
                            c,
                            (b.right - b.left) / 2.0,
                            mix(color(0xFFFFFF, 0.0), color(tint, hover), h),
                        );
                        draw_text(
                            glyph,
                            &o.text.icon,
                            b,
                            mix(color(0xFFFFFF, 0.9 * hover), color(0x000000, hover), h),
                        );
                    }
                }
                // Resize handle.
                let handle_a = if dragged { 1.0 } else { 0.45 + 0.55 * hover };
                circle(
                    Vector2 {
                        X: r.right - 9.0 * s,
                        Y: r.bottom - 9.0 * s,
                    },
                    4.5 * s,
                    color(ACCENT, handle_a),
                );
            }

            if self.index == 0 {
                // Toolbar.
                let (bar, add, done) = e.toolbar();
                let b = rf(bar);
                shadow(b, 12.0, 0.12);
                fill(rr(b, 12.0), color(0x202020, 0.94));
                stroke(rr(b, 12.0), color(0xFFFFFF, 0.10), 1.0);
                let ar = rf(add);
                fill(
                    rr(ar, 8.0),
                    color(0xFFFFFF, 0.07 * e.a("hot:add") + 0.09 * e.a("sheet")),
                );
                draw_text(
                    "\u{E710}",
                    &o.text.icon,
                    D2D_RECT_F {
                        right: ar.left + 40.0 * s,
                        ..ar
                    },
                    color(0xFFFFFF, 0.95),
                );
                draw_text(
                    &t("edit.add_widget"),
                    &o.text.button,
                    D2D_RECT_F {
                        left: ar.left + 28.0 * s,
                        ..ar
                    },
                    color(0xFFFFFF, 0.95),
                );
                fill(
                    rr(rf(done), 8.0),
                    mix(
                        color(ACCENT, 1.0),
                        color(0xB8E6FF, 1.0),
                        e.a("hot:done") * 0.6,
                    ),
                );
                draw_text(
                    &t("edit.done"),
                    &o.text.button,
                    rf(done),
                    color(0x000000, 0.92),
                );
                // Hint.
                let hint_top = b.bottom + 10.0 * s;
                let hint = t(if e.gallery_open {
                    "edit.hint_gallery"
                } else {
                    "edit.hint"
                });
                let hint = hint.as_str();
                let hr = D2D_RECT_F {
                    left: full.left,
                    top: hint_top,
                    right: full.right,
                    bottom: hint_top + 22.0 * s,
                };
                let shadow_r = D2D_RECT_F {
                    top: hr.top + 1.0 * s,
                    bottom: hr.bottom + 1.0 * s,
                    ..hr
                };
                draw_text(hint, &o.text.caption, shadow_r, color(0x000000, 0.5));
                draw_text(hint, &o.text.caption, hr, color(0xFFFFFF, 0.9));

                // The "Add widget" sheet.
                let alpha = e.sheet_alpha();
                if alpha > 0.001 {
                    let l = e.sheet_layout();
                    // The desktop dims a little more behind it; the grid still shows.
                    brush.SetColor(&color(0x000000, 0.22 * alpha));
                    dc.FillRectangle(&full, &brush);
                    let sr = rf(l.sheet);
                    // Spring scale around the centre (it shrinks a little when it steps aside).
                    let k = (0.92 + 0.08 * e.spring.0) * (1.0 - 0.03 * e.a("away"));
                    let base = Matrix3x2::scale_around(
                        k,
                        k,
                        Vector2 {
                            X: (sr.left + sr.right) / 2.0,
                            Y: (sr.top + sr.bottom) / 2.0,
                        },
                    );
                    dc.SetTransform(&base);
                    // A soft, wide shadow, heavier below.
                    for i in 1..=10 {
                        let g = i as f32 * 3.2 * s;
                        let r = D2D_RECT_F {
                            left: sr.left - g,
                            top: sr.top - g * 0.6,
                            right: sr.right + g,
                            bottom: sr.bottom + g * 1.4,
                        };
                        fill(
                            rr(r, 12.0 + i as f32 * 3.2),
                            color(0x000000, 0.045 * alpha * (1.0 - i as f32 / 11.0)),
                        );
                    }
                    fill(rr(sr, 12.0), color(0x2B2B2B, 0.98 * alpha));
                    // Sidebar pane: a shade darker, behind the same rounded corners.
                    let side = rf(l.side);
                    dc.PushAxisAlignedClip(&side, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    fill(rr(sr, 12.0), color(0x202020, 0.98 * alpha));
                    dc.PopAxisAlignedClip();
                    brush.SetColor(&color(0xFFFFFF, 0.06 * alpha));
                    dc.FillRectangle(
                        &D2D_RECT_F {
                            left: side.right - s.round().max(1.0),
                            right: side.right,
                            ..side
                        },
                        &brush,
                    );
                    stroke(rr(sr, 12.0), color(0xFFFFFF, 0.10 * alpha), 1.0);

                    // Search field (Fluent text box: subtle fill, accent underline on focus).
                    let fr = rf(l.field);
                    let focus = e.a("focus");
                    let hover = e.a("hot:search").max(e.a("hot:clear"));
                    let rest = color(0xFFFFFF, 0.06 + 0.025 * hover);
                    let fill_c = mix(rest, color(0x1A1A1A, 0.85), focus);
                    fill(
                        rr(fr, 4.0),
                        D2D1_COLOR_F {
                            a: fill_c.a * alpha,
                            ..fill_c
                        },
                    );
                    stroke(rr(fr, 4.0), color(0xFFFFFF, 0.08 * alpha), 1.0);
                    brush.SetColor(&color(0xFFFFFF, 0.36 * alpha * (1.0 - focus)));
                    dc.FillRectangle(
                        &D2D_RECT_F {
                            left: fr.left + 3.0 * s,
                            top: fr.bottom - 1.0 * s,
                            right: fr.right - 3.0 * s,
                            bottom: fr.bottom,
                        },
                        &brush,
                    );
                    // The focus line grows out from the centre.
                    let mid = (fr.left + fr.right) / 2.0;
                    let half = ((fr.right - fr.left) / 2.0 - 3.0 * s) * ease(focus);
                    if half > 0.5 {
                        fill(
                            rr(
                                D2D_RECT_F {
                                    left: mid - half,
                                    top: fr.bottom - 2.0 * s,
                                    right: mid + half,
                                    bottom: fr.bottom,
                                },
                                1.0,
                            ),
                            color(ACCENT, alpha),
                        );
                    }
                    draw_text(
                        "\u{E721}",
                        &o.text.icon,
                        D2D_RECT_F {
                            left: fr.left + 6.0 * s,
                            right: fr.left + 32.0 * s,
                            ..fr
                        },
                        color(0xFFFFFF, (0.6 + 0.3 * focus) * alpha),
                    );
                    let (tl, tr, off) = e.field_text(&l);
                    let text_r = D2D_RECT_F {
                        left: tl + dx,
                        right: tr + dx,
                        ..fr
                    };
                    dc.PushAxisAlignedClip(&text_r, D2D1_ANTIALIAS_MODE_ALIASED);
                    if e.search.is_empty() {
                        draw_text(
                            &t("edit.search_placeholder"),
                            &o.text.field,
                            D2D_RECT_F {
                                right: text_r.left + 4096.0,
                                ..text_r
                            },
                            color(0xFFFFFF, 0.55 * alpha),
                        );
                    } else {
                        draw_text(
                            &e.search.text,
                            &o.text.field,
                            D2D_RECT_F {
                                left: text_r.left - off,
                                right: text_r.left - off + 4096.0,
                                ..text_r
                            },
                            color(0xFFFFFF, 0.95 * alpha),
                        );
                    }
                    if e.caret_on && focus > 0.5 && !e.closing {
                        let before = &e.search.text[..e.search.caret];
                        let x = (text_r.left - off + e.text_width(before, &o.text.field)).round();
                        brush.SetColor(&color(0xFFFFFF, 0.95 * alpha));
                        dc.FillRectangle(
                            &D2D_RECT_F {
                                left: x,
                                top: fr.top + 8.0 * s,
                                right: x + s.round().max(1.0),
                                bottom: fr.bottom - 8.0 * s,
                            },
                            &brush,
                        );
                    }
                    dc.PopAxisAlignedClip();
                    let cv = e.a("clear");
                    if cv > 0.01 {
                        let c = rf(l.clear);
                        fill(
                            rr(c, 4.0),
                            color(0xFFFFFF, 0.08 * e.a("hot:clear") * cv * alpha),
                        );
                        draw_text(
                            "\u{E711}",
                            &o.text.icon_small,
                            c,
                            color(0xFFFFFF, 0.8 * cv * alpha),
                        );
                    }

                    // Sidebar entries: hover and selection fills, then the
                    // accent indicator that slides (and stretches) between rows.
                    let entries = e.entries();
                    let selected = e.entry_index();
                    for (i, r) in l.entries.iter().enumerate() {
                        let h = e.a(&format!("side:{i}"));
                        let a = if i == selected {
                            0.075 + 0.02 * h
                        } else {
                            0.045 * h
                        };
                        if a > 0.001 {
                            fill(rr(rf(*r), 5.0), color(0xFFFFFF, a * alpha));
                        }
                    }
                    if let Some(first) = l.entries.first() {
                        let x = rf(*first).left;
                        let (top, bottom) = (sr.top + e.pill.0, sr.top + e.pill.1);
                        if bottom - top > 1.0 {
                            fill(
                                rr(
                                    D2D_RECT_F {
                                        left: x,
                                        top,
                                        right: x + 3.0 * s,
                                        bottom,
                                    },
                                    1.5,
                                ),
                                color(ACCENT, alpha),
                            );
                        }
                    }
                    if let Some(r) = l.entries.get(2) {
                        let y = rf(*r).top - 7.0 * s;
                        brush.SetColor(&color(0xFFFFFF, 0.08 * alpha));
                        dc.FillRectangle(
                            &D2D_RECT_F {
                                left: side.left + 16.0 * s,
                                top: y,
                                right: side.right - 16.0 * s,
                                bottom: y + s.round().max(1.0),
                            },
                            &brush,
                        );
                    }
                    for (i, (entry, r)) in entries.iter().zip(&l.entries).enumerate() {
                        let r = rf(*r);
                        let count = e.entry_count(entry);
                        let dim = if count == 0 { 0.45 } else { 1.0 };
                        let icon = D2D_RECT_F {
                            left: r.left + 10.0 * s,
                            right: r.left + 32.0 * s,
                            ..r
                        };
                        let c = Vector2 {
                            X: (icon.left + icon.right) / 2.0,
                            Y: (icon.top + icon.bottom) / 2.0,
                        };
                        match entry {
                            Selection::All => draw_text(
                                "\u{E71D}",
                                &o.text.icon,
                                icon,
                                color(0xFFFFFF, 0.85 * alpha * dim),
                            ),
                            Selection::Favourites => draw_text(
                                "\u{E735}",
                                &o.text.icon,
                                icon,
                                color(0xFFC83D, alpha * dim),
                            ),
                            Selection::Pack(id) => {
                                circle(
                                    c,
                                    10.0 * s,
                                    color(vitrine_widgets::gallery::tint(id), alpha * dim),
                                );
                                let initial: String = e
                                    .entry_label(entry)
                                    .chars()
                                    .find(|c| c.is_alphanumeric())
                                    .map(|c| c.to_uppercase().collect())
                                    .unwrap_or_default();
                                draw_text(
                                    &initial,
                                    &o.text.chip,
                                    icon,
                                    color(0xFFFFFF, alpha * dim),
                                );
                            }
                        }
                        let n = count.to_string();
                        let badge_w = e.text_width(&n, &o.text.chip).ceil() + 14.0 * s;
                        let badge = D2D_RECT_F {
                            left: r.right - 8.0 * s - badge_w,
                            top: (r.top + r.bottom) / 2.0 - 10.0 * s,
                            right: r.right - 8.0 * s,
                            bottom: (r.top + r.bottom) / 2.0 + 10.0 * s,
                        };
                        fill(rr(badge, 10.0), color(0xFFFFFF, 0.07 * alpha * dim));
                        draw_text(&n, &o.text.chip, badge, color(0xFFFFFF, 0.7 * alpha * dim));
                        let strong = if i == selected { 1.0 } else { 0.86 };
                        draw_text(
                            &e.entry_label(entry),
                            &o.text.side,
                            D2D_RECT_F {
                                left: r.left + 42.0 * s,
                                right: badge.left - 6.0 * s,
                                ..r
                            },
                            color(0xFFFFFF, strong * alpha * dim),
                        );
                    }

                    // The selection's title and how many widgets it shows.
                    let title_r = rf(l.title);
                    let title = e.entry_label(&e.selection);
                    draw_text(
                        &title,
                        &o.text.title,
                        title_r,
                        color(0xFFFFFF, 0.96 * alpha),
                    );
                    let tw = e.text_width(&title, &o.text.title);
                    draw_text(
                        &crate::i18n::tn("row.widgets", e.groups.len() as u64),
                        &o.text.desc,
                        D2D_RECT_F {
                            left: title_r.left + tw + 12.0 * s,
                            top: title_r.top + 3.0 * s,
                            ..title_r
                        },
                        color(0xFFFFFF, 0.55 * alpha),
                    );

                    // The widgets: each size on the neutral tile, at its true
                    // proportions. Hover lifts a tile and pops its "+".
                    let area = rf(l.area);
                    dc.PushAxisAlignedClip(&area, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    let content = ease(e.a("content"));
                    let rise = (1.0 - content) * 10.0 * s;
                    let lift = |r: RECT| {
                        let f = rf(r);
                        D2D_RECT_F {
                            top: f.top + rise,
                            bottom: f.bottom + rise,
                            ..f
                        }
                    };
                    let dragged = drag.and_then(|d| match d.kind {
                        DragKind::New { at, .. } => Some(at),
                        _ => None,
                    });
                    let ga = alpha * content;
                    let show_pack = !matches!(e.selection, Selection::Pack(_));
                    for (gi, (g, gl)) in e.groups.iter().zip(&l.groups).enumerate() {
                        if gl.bounds.bottom < l.area.top - 40 || gl.bounds.top > l.area.bottom + 40
                        {
                            continue;
                        }
                        let item = &e.gallery[g.item];
                        for (vi, (&size, (tile, label))) in g
                            .variants
                            .iter()
                            .zip(gl.tiles.iter().zip(&gl.labels))
                            .enumerate()
                        {
                            let hover = e.a(&format!("tile:{gi}:{vi}"));
                            let ring = e.a(&format!("ring:{gi}:{vi}"));
                            let faded = if dragged == Some((gi, vi)) { 0.4 } else { 1.0 };
                            let tr = lift(*tile);
                            let k = 1.0 + 0.03 * hover;
                            dc.SetTransform(
                                &(Matrix3x2::scale_around(
                                    k,
                                    k,
                                    Vector2 {
                                        X: (tr.left + tr.right) / 2.0,
                                        Y: (tr.top + tr.bottom) / 2.0,
                                    },
                                ) * base),
                            );
                            if hover > 0.01 {
                                shadow(tr, 8.0, 0.10 * hover * ga);
                            }
                            fill(rr(tr, 8.0), color(0x1F1F1F, ga * faded));
                            let (bmp, rendered) = e.variant_bitmap(g.item, size);
                            if let Some(bmp) = bmp {
                                if rendered {
                                    dc.DrawBitmap(
                                        bmp,
                                        Some(&tr),
                                        ga * faded,
                                        D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                                        None,
                                        None,
                                    );
                                } else {
                                    let inset = 6.0 * s;
                                    let inner = D2D_RECT_F {
                                        left: tr.left + inset,
                                        top: tr.top + inset,
                                        right: tr.right - inset,
                                        bottom: tr.bottom - inset,
                                    };
                                    draw_contained(dc, bmp, inner, 0.85 * ga * faded);
                                }
                            }
                            stroke(
                                rr(tr, 8.0),
                                mix(color(0xFFFFFF, 0.07 * ga), color(ACCENT, 0.95 * ga), hover),
                                1.0 + 0.5 * hover,
                            );
                            // Keyboard focus: the Fluent double ring.
                            if ring > 0.01 {
                                let grow = |r: D2D_RECT_F, d: f32| D2D_RECT_F {
                                    left: r.left - d,
                                    top: r.top - d,
                                    right: r.right + d,
                                    bottom: r.bottom + d,
                                };
                                stroke(
                                    rr(grow(tr, 3.0 * s), 11.0),
                                    color(0xFFFFFF, ring * ga),
                                    2.0,
                                );
                                stroke(
                                    rr(grow(tr, 1.5 * s), 9.5),
                                    color(0x000000, 0.7 * ring * ga),
                                    1.0,
                                );
                            }
                            // "+" pops in on hover (with a little overshoot);
                            // after adding it shows a check mark for a moment.
                            let key = sheet::preview_key(item, size);
                            let done = e.added_t(&key);
                            let show = hover.max(ring).max(if done > 0.0 { 1.0 } else { 0.0 });
                            if show > 0.01 {
                                let p = rf(e.plus_rect(*tile));
                                let c = Vector2 {
                                    X: (p.left + p.right) / 2.0,
                                    Y: (p.top + p.bottom) / 2.0 + rise,
                                };
                                let t = show.clamp(0.0, 1.0);
                                let back = 1.0 + 2.7 * (t - 1.0).powi(3) + 1.7 * (t - 1.0).powi(2);
                                let radius = 13.0 * s * back.max(0.0);
                                let ph = e.a(&format!("plus:{gi}:{vi}"));
                                circle(c, radius + 2.0 * s, color(0x2B2B2B, ga * t));
                                circle(
                                    c,
                                    radius,
                                    D2D1_COLOR_F {
                                        a: ga * t,
                                        ..mix(color(ACCENT, 1.0), color(0xB8E6FF, 1.0), ph * 0.6)
                                    },
                                );
                                let glyph = if done > 0.0 { "\u{E73E}" } else { "\u{E710}" };
                                draw_text(
                                    glyph,
                                    &o.text.icon,
                                    D2D_RECT_F {
                                        left: c.X - radius,
                                        top: c.Y - radius,
                                        right: c.X + radius,
                                        bottom: c.Y + radius,
                                    },
                                    color(0x000000, 0.9 * ga * t),
                                );
                            }
                            dc.SetTransform(&base);
                            draw_text(
                                &format!("{} × {}", size.0, size.1),
                                &o.text.tiny,
                                lift(*label),
                                color(0xFFFFFF, (0.5 + 0.4 * hover.max(ring)) * ga),
                            );
                        }
                        draw_text(
                            &item.name,
                            &o.text.name,
                            lift(gl.name),
                            color(0xFFFFFF, 0.95 * ga),
                        );
                        let desc = match (show_pack, item.description.is_empty()) {
                            (true, false) => format!("{} · {}", item.pack_name, item.description),
                            (true, true) => item.pack_name.clone(),
                            (false, _) => item.description.clone(),
                        };
                        draw_text(
                            &desc,
                            &o.text.desc,
                            lift(gl.desc),
                            color(0xFFFFFF, 0.6 * ga),
                        );
                    }
                    // Scrolled content fades under the title row.
                    if e.scroll > 0.5 {
                        for i in 0..8 {
                            let h = 2.0 * s;
                            let y = area.top + i as f32 * h;
                            brush.SetColor(&color(0x2B2B2B, 0.98 * alpha * (1.0 - i as f32 / 8.0)));
                            dc.FillRectangle(
                                &D2D_RECT_F {
                                    top: y,
                                    bottom: y + h,
                                    ..area
                                },
                                &brush,
                            );
                        }
                    }
                    dc.PopAxisAlignedClip();

                    // Nothing to show: say why, in the middle of the area.
                    let empty = e.a("empty");
                    if empty > 0.01 {
                        let rise = (1.0 - empty) * 6.0 * s;
                        let mid = (area.top + area.bottom) / 2.0 - 20.0 * s + rise;
                        draw_text(
                            "\u{E721}",
                            &o.text.icon_large,
                            D2D_RECT_F {
                                top: mid - 44.0 * s,
                                bottom: mid - 4.0 * s,
                                ..area
                            },
                            color(0xFFFFFF, 0.35 * empty * alpha),
                        );
                        let text = if e.gallery.is_empty() {
                            t("edit.no_widgets")
                        } else if !e.search.text.trim().is_empty() {
                            crate::i18n::tf("edit.no_results", &[("query", e.search.text.trim())])
                        } else if e.selection == Selection::Favourites {
                            t("edit.no_favourites")
                        } else {
                            t("edit.no_widgets")
                        };
                        draw_text(
                            &text,
                            &o.text.caption,
                            D2D_RECT_F {
                                top: mid + 2.0 * s,
                                bottom: mid + 26.0 * s,
                                ..area
                            },
                            color(0xFFFFFF, 0.7 * empty * alpha),
                        );
                    }
                    dc.SetTransform(&Matrix3x2::identity());
                }
            }

            // Ghost of a widget being dragged out of the sheet: the chosen
            // size, snapping to the grid once the sheet has stepped aside.
            if let Some(d) = drag
                && let DragKind::New { item, size, .. } = d.kind
                && contains(&o.rect, e.pointer)
            {
                let r = rf(d.cur);
                shadow(r, 10.0, 0.12);
                fill(rr(r, 10.0), color(0x1F1F1F, 0.9));
                let (bmp, rendered) = e.variant_bitmap(item, size);
                if let Some(bmp) = bmp {
                    if rendered {
                        dc.DrawBitmap(
                            bmp,
                            Some(&r),
                            0.95,
                            D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                            None,
                            None,
                        );
                    } else {
                        let inner = D2D_RECT_F {
                            left: r.left + 6.0 * s,
                            top: r.top + 6.0 * s,
                            right: r.right - 6.0 * s,
                            bottom: r.bottom - 6.0 * s,
                        };
                        draw_contained(dc, bmp, inner, 0.95);
                    }
                }
                stroke(rr(r, 10.0), color(ACCENT, 0.9), 2.0);
            }
        }
        Ok(())
    }
}

/// Draws `bmp` scaled to fit inside `r`, centred.
fn draw_contained(dc: &ID2D1DeviceContext, bmp: &ID2D1Bitmap, r: D2D_RECT_F, opacity: f32) {
    unsafe {
        let size = bmp.GetSize();
        if size.width <= 0.0 || size.height <= 0.0 {
            return;
        }
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let k = (w / size.width).min(h / size.height);
        let (dw, dh) = (size.width * k, size.height * k);
        let dest = D2D_RECT_F {
            left: r.left + (w - dw) / 2.0,
            top: r.top + (h - dh) / 2.0,
            right: r.left + (w + dw) / 2.0,
            bottom: r.top + (h + dh) / 2.0,
        };
        dc.DrawBitmap(
            bmp,
            Some(&dest),
            opacity,
            D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
            None,
            None,
        );
    }
}

impl Drop for EditMode {
    fn drop(&mut self) {
        for mut v in self.visuals.drain(..) {
            let _ = v.release();
        }
        for o in self.overlays.drain(..) {
            unsafe {
                let _ = DestroyWindow(o.hwnd);
            }
        }
    }
}

fn screen_point(hwnd: HWND, lparam: LPARAM) -> POINT {
    let mut pt = POINT {
        x: (lparam.0 & 0xFFFF) as i16 as i32,
        y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    };
    unsafe {
        let _ = ClientToScreen(hwnd, &mut pt);
    }
    pt
}

thread_local! {
    /// The high half of a surrogate pair, waiting for its low half.
    static HIGH_SURROGATE: std::cell::Cell<Option<u16>> = const { std::cell::Cell::new(None) };
}

/// Turns WM_CHAR code units into characters: pairs surrogates (characters
/// outside the BMP arrive as two messages) and drops control characters.
fn utf16_char(unit: u16) -> Option<char> {
    HIGH_SURROGATE.with(|high| {
        if (0xD800..0xDC00).contains(&unit) {
            high.set(Some(unit));
            return None;
        }
        let units: Vec<u16> = match high.take() {
            Some(h) if (0xDC00..0xE000).contains(&unit) => vec![h, unit],
            _ => vec![unit],
        };
        char::decode_utf16(units)
            .next()
            .and_then(|r| r.ok())
            .filter(|c| !c.is_control())
    })
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let push = |i: Input| crate::runtime::push_host(HostMsg::Edit(i));
    match msg {
        WM_LBUTTONDOWN => {
            unsafe { SetCapture(hwnd) };
            push(Input::Down(screen_point(hwnd, lparam)));
        }
        WM_MOUSEMOVE => push(Input::Move(screen_point(hwnd, lparam))),
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            push(Input::Up(screen_point(hwnd, lparam)));
        }
        WM_RBUTTONUP => push(Input::RightUp(screen_point(hwnd, lparam))),
        // Wheel coordinates are already in screen space; the pointer position is tracked.
        WM_MOUSEWHEEL => push(Input::Wheel(((wparam.0 >> 16) & 0xFFFF) as u16 as i16)),
        WM_KEYDOWN if wparam.0 as u16 == VK_ESCAPE.0 => push(Input::Escape),
        WM_KEYDOWN => {
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            push(Input::Key {
                vk: wparam.0 as u16,
                ctrl,
            });
        }
        // IME results arrive here too: WM_IME_* fall through to DefWindowProc,
        // which turns composed text into WM_CHAR.
        WM_CHAR => {
            if let Some(c) = utf16_char(wparam.0 as u16) {
                push(Input::Char(c));
            }
        }
        WM_SETCURSOR => return LRESULT(1), // set on mouse move
        WM_MOUSEACTIVATE => return LRESULT(MA_ACTIVATE as isize),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitrine_widgets::layout::GridConfig;
    use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };

    const PRIMARY: &str = "primary";
    const SECOND: &str = r"\\.\DISPLAY2";

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    fn center(r: RECT) -> POINT {
        POINT {
            x: (r.left + r.right) / 2,
            y: (r.top + r.bottom) / 2,
        }
    }

    fn pt(x: i32, y: i32) -> POINT {
        POINT { x, y }
    }

    /// An edit mode without windows or GPU: overlays have no HWND and no visual.
    fn mode_with(monitors: &[(&str, RECT, f32)], gallery: Vec<GalleryItem>) -> EditMode {
        mode_full(monitors, gallery, None)
    }

    fn mode_full(
        monitors: &[(&str, RECT, f32)],
        gallery: Vec<GalleryItem>,
        tab: Option<String>,
    ) -> EditMode {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let dwrite: IDWriteFactory5 =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.unwrap();
        let wic: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .unwrap();
        let overlays = monitors
            .iter()
            .map(|(name, r, scale)| Overlay {
                hwnd: HWND::default(),
                rect: *r,
                monitor: name.to_string(),
                geo: Geometry::new(&GridConfig::default(), *r, *scale),
                text: Fonts::new(&dwrite, *scale).unwrap(),
                shown: true,
            })
            .collect();
        let mut m = EditMode::new(
            overlays,
            Vec::new(),
            gallery,
            tab,
            ImageCache::new(wic),
            dwrite,
        );
        m.dirty = false;
        m
    }

    fn mode() -> EditMode {
        mode_with(&[(PRIMARY, rect(0, 0, 1920, 1080), 1.0)], gallery(2))
    }

    fn gallery(n: usize) -> Vec<GalleryItem> {
        (0..n)
            .map(|i| GalleryItem {
                package: "dev.pack".into(),
                pack_name: "Dev".into(),
                widget: format!("w{i}"),
                name: format!("Widget {i}"),
                description: String::new(),
                size: (2, 2),
                min: (2, 2),
                max: (4, 2),
                favourite: false,
                dir: std::path::PathBuf::new(),
                preview: None,
            })
            .collect()
    }

    fn item(id: &str, rect: Cells) -> Item {
        Item {
            id: id.into(),
            name: format!("Widget {id}"),
            monitor: PRIMARY.into(),
            rect,
            min: (1, 1),
            max: (6, 4),
        }
    }

    fn items() -> Vec<Item> {
        vec![item("a", (2, 4, 3, 2)), item("b", (10, 4, 2, 2))]
    }

    fn pitch(m: &EditMode) -> i32 {
        let g = m.geometry(PRIMARY).unwrap();
        ((g.cell + g.gap) * g.scale).round() as i32
    }

    /// Presses at `from`, moves to `to` in a few steps, releases there.
    fn drag(m: &mut EditMode, items: &[Item], from: POINT, to: POINT) -> Vec<Action> {
        assert!(m.input(Input::Down(from), items).is_empty());
        let mut previews = Vec::new();
        for k in 1..=4 {
            let p = pt(
                from.x + (to.x - from.x) * k / 4,
                from.y + (to.y - from.y) * k / 4,
            );
            previews.extend(m.input(Input::Move(p), items));
        }
        assert!(
            previews.iter().all(|a| matches!(a, Action::Preview { .. })),
            "{previews:?}"
        );
        m.input(Input::Up(to), items)
    }

    fn click(m: &mut EditMode, items: &[Item], at: POINT) -> Vec<Action> {
        let mut out = m.input(Input::Down(at), items);
        out.extend(m.input(Input::Up(at), items));
        out
    }

    // ── small helpers ──

    #[test]
    fn rect_helpers() {
        let r = rect(10, 20, 30, 40);
        assert!(contains(&r, pt(10, 20)));
        assert!(contains(&r, pt(29, 39)));
        assert!(!contains(&r, pt(30, 20)));
        assert!(!contains(&r, pt(10, 40)));
        assert!(!contains(&r, pt(9, 25)));
        assert_eq!(offset(r, -5, 7), rect(5, 27, 25, 47));
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert_eq!(ease(2.0), 1.0);
    }

    #[test]
    fn overlap_and_collision() {
        assert!(overlaps((0, 0, 2, 2), (1, 1, 2, 2)));
        assert!(overlaps((0, 0, 4, 4), (1, 1, 1, 1)));
        // Touching edges do not overlap.
        assert!(!overlaps((0, 0, 2, 2), (2, 0, 2, 2)));
        assert!(!overlaps((0, 0, 2, 2), (0, 2, 2, 2)));

        let items = items();
        assert!(EditMode::collides(&items, PRIMARY, (3, 5, 1, 1), None));
        assert!(EditMode::collides(&items, "PRIMARY", (3, 5, 1, 1), None));
        // Ignoring the dragged item itself.
        assert!(!EditMode::collides(
            &items,
            PRIMARY,
            (3, 5, 1, 1),
            Some("a")
        ));
        assert!(!EditMode::collides(&items, SECOND, (3, 5, 1, 1), None));
        assert!(!EditMode::collides(&items, PRIMARY, (5, 4, 5, 2), None));
    }

    #[test]
    fn color_helpers() {
        let c = color(0xFF8000, 0.5);
        assert_eq!((c.r, c.g, c.a), (1.0, 128.0 / 255.0, 0.5));
        assert_eq!(c.b, 0.0);
        let m = mix(color(0x000000, 0.0), color(0xFFFFFF, 1.0), 0.25);
        assert!((m.r - 0.25).abs() < 1e-6 && (m.a - 0.25).abs() < 1e-6);
    }

    // ── geometry and hit-testing ──

    #[test]
    fn toolbar_is_centred_at_the_top() {
        let m = mode();
        let (bar, add, done) = m.toolbar();
        assert_eq!((bar.left + bar.right) / 2, 960);
        assert_eq!(bar.top, 20);
        assert!(add.right <= done.left);
        assert!(contains(&bar, center(add)) && contains(&bar, center(done)));
    }

    #[test]
    fn hit_targets() {
        let mut m = mode();
        let items = items();
        let (bar, add, done) = m.toolbar();
        assert_eq!(m.hit(&items, center(add)), Hot::Add);
        assert_eq!(m.hit(&items, center(done)), Hot::Done);
        assert_eq!(m.hit(&items, pt(bar.left + 1, bar.top + 1)), Hot::None);

        let r = m.frame_px(&items[0]).unwrap();
        assert_eq!(m.hit(&items, center(r)), Hot::Frame("a".into()));
        assert_eq!(
            m.hit(&items, pt(r.right - 3, r.bottom - 3)),
            Hot::Handle("a".into())
        );
        // The handle reaches slightly outside the frame.
        assert_eq!(
            m.hit(&items, pt(r.right + 2, r.bottom + 2)),
            Hot::Handle("a".into())
        );
        assert_eq!(m.hit(&items, pt(r.left - 20, r.top + 5)), Hot::None);

        // The pill (remove/settings) only reacts once the frame is shown.
        let (pill, settings, remove, _) = m.chrome(&items[0], r);
        assert!(pill.bottom <= r.top, "pill above the frame");
        assert_eq!(m.hit(&items, center(remove)), Hot::None);
        m.anim.insert("frame:a".into(), 1.0);
        assert_eq!(m.hit(&items, center(remove)), Hot::Remove("a".into()));
        assert_eq!(m.hit(&items, center(settings)), Hot::Settings("a".into()));
        assert_eq!(
            m.hit(&items, pt(pill.left + 2, pill.top + 2)),
            Hot::Frame("a".into())
        );
    }

    #[test]
    fn pill_goes_below_at_the_top_edge() {
        let m = mode();
        let it = item("t", (0, 0, 2, 2));
        let r = m.frame_px(&it).unwrap();
        let (pill, settings, remove, handle) = m.chrome(&it, r);
        assert!(pill.top >= r.bottom, "{pill:?} vs {r:?}");
        assert!(contains(&pill, center(settings)) && contains(&pill, center(remove)));
        assert!(settings.right <= remove.left);
        assert!(contains(&handle, pt(r.right - 1, r.bottom - 1)));
    }

    #[test]
    fn later_items_are_on_top() {
        let m = mode();
        let items = vec![item("under", (2, 4, 4, 4)), item("over", (3, 5, 1, 1))];
        let r = m.frame_px(&items[1]).unwrap();
        assert_eq!(m.hit(&items, center(r)), Hot::Frame("over".into()));
    }

    // ── toolbar and keyboard ──

    #[test]
    fn escape_closes_gallery_then_edit_mode() {
        let mut m = mode();
        m.open_gallery();
        assert!(m.input(Input::Escape, &[]).is_empty());
        assert!(!m.gallery_open);
        assert!(matches!(
            m.input(Input::Escape, &[]).as_slice(),
            [Action::Close]
        ));
    }

    #[test]
    fn done_and_add_buttons() {
        let mut m = mode();
        let (_, add, done) = m.toolbar();
        assert!(click(&mut m, &[], center(add)).is_empty());
        assert!(m.gallery_open);
        assert!(click(&mut m, &[], center(add)).is_empty());
        assert!(!m.gallery_open);
        assert!(matches!(
            click(&mut m, &[], center(done)).as_slice(),
            [Action::Close]
        ));
        // Pressing on one button and releasing on another does nothing.
        m.input(Input::Down(center(add)), &[]);
        assert!(m.input(Input::Up(center(done)), &[]).is_empty());
        assert!(!m.gallery_open);
    }

    #[test]
    fn closing_ignores_input() {
        let mut m = mode();
        m.closing = true;
        assert!(m.input(Input::Escape, &[]).is_empty());
        let (_, _, done) = m.toolbar();
        assert!(click(&mut m, &[], center(done)).is_empty());
    }

    #[test]
    fn remove_and_settings_buttons() {
        let mut m = mode();
        let items = items();
        m.anim.insert("frame:b".into(), 1.0);
        let r = m.frame_px(&items[1]).unwrap();
        let (_, settings, remove, _) = m.chrome(&items[1], r);
        assert!(matches!(
            click(&mut m, &items, center(remove)).as_slice(),
            [Action::Remove { id }] if id == "b"
        ));
        assert!(matches!(
            click(&mut m, &items, center(settings)).as_slice(),
            [Action::Settings { id }] if id == "b"
        ));
    }

    #[test]
    fn right_click_removes() {
        let mut m = mode();
        let items = items();
        let r = m.frame_px(&items[0]).unwrap();
        assert!(matches!(
            m.input(Input::RightUp(center(r)), &items).as_slice(),
            [Action::Remove { id }] if id == "a"
        ));
        assert!(m.input(Input::RightUp(pt(5, 1000)), &items).is_empty());
    }

    // ── moving ──

    #[test]
    fn click_on_a_frame_does_nothing() {
        let mut m = mode();
        let items = items();
        let c = center(m.frame_px(&items[0]).unwrap());
        assert!(click(&mut m, &items, c).is_empty());
        // Jitter under the threshold is not a drag either.
        m.input(Input::Down(c), &items);
        assert!(
            m.input(Input::Move(pt(c.x + 2, c.y - 2)), &items)
                .is_empty()
        );
        assert!(m.input(Input::Up(pt(c.x + 2, c.y - 2)), &items).is_empty());
    }

    #[test]
    fn move_snaps_to_cells() {
        let mut m = mode();
        let items = items();
        let r = m.frame_px(&items[0]).unwrap();
        let p = pitch(&m);
        let from = center(r);
        // Two cells right, one down, plus a little less than half a cell.
        let to = pt(from.x + 2 * p + p / 3, from.y + p + p / 3);
        let actions = drag(&mut m, &items, from, to);
        match actions.as_slice() {
            [
                Action::Commit {
                    id,
                    monitor,
                    cells,
                    from,
                },
            ] => {
                assert_eq!(id, "a");
                assert_eq!(monitor, PRIMARY);
                assert_eq!(*cells, (4, 5, 3, 2));
                // Animates from where the pointer left the widget.
                assert_eq!(*from, offset(r, to.x - center(r).x, to.y - center(r).y));
            }
            other => panic!("{other:?}"),
        }
        assert!(m.drag.is_none());
    }

    #[test]
    fn move_previews_follow_the_pointer() {
        let mut m = mode();
        let items = items();
        let r = m.frame_px(&items[0]).unwrap();
        let c = center(r);
        m.input(Input::Down(c), &items);
        match m
            .input(Input::Move(pt(c.x + 37, c.y - 11)), &items)
            .as_slice()
        {
            [Action::Preview { id, rect }] => {
                assert_eq!(id, "a");
                assert_eq!(*rect, offset(r, 37, -11));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn move_is_clamped_to_the_grid() {
        let mut m = mode();
        let items = items();
        let g = m.geometry(PRIMARY).unwrap();
        let from = center(m.frame_px(&items[0]).unwrap());
        // Far beyond the bottom-right corner (still on the monitor).
        let actions = drag(&mut m, &items, from, pt(1915, 1075));
        match actions.as_slice() {
            [Action::Commit { cells, .. }] => {
                assert_eq!(*cells, (g.cols - 3, g.rows - 2, 3, 2));
            }
            other => panic!("{other:?}"),
        }
        // And beyond the top-left one.
        let mut m = mode();
        let actions = drag(&mut m, &items, from, pt(1, 70));
        assert!(matches!(
            actions.as_slice(),
            [Action::Commit {
                cells: (0, _, 3, 2),
                ..
            }]
        ));
    }

    #[test]
    fn drop_on_another_widget_cancels() {
        let mut m = mode();
        let items = items();
        let ra = m.frame_px(&items[0]).unwrap();
        let rb = m.frame_px(&items[1]).unwrap();
        // Top-left of "a" onto the top-left of "b".
        let from = pt(ra.left + 5, ra.top + 5);
        let to = pt(rb.left + 5, rb.top + 5);
        match drag(&mut m, &items, from, to).as_slice() {
            [Action::Cancel { id, from }] => {
                assert_eq!(id, "a");
                assert_eq!(from.left, rb.left);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn drop_onto_own_old_place_is_fine() {
        let mut m = mode();
        let items = items();
        let c = center(m.frame_px(&items[0]).unwrap());
        let p = pitch(&m);
        // Overlaps its own previous span: not a collision.
        let actions = drag(&mut m, &items, c, pt(c.x + p, c.y));
        assert!(matches!(
            actions.as_slice(),
            [Action::Commit {
                cells: (3, 4, 3, 2),
                ..
            }]
        ));
    }

    #[test]
    fn move_to_another_monitor() {
        let mut m = mode_with(
            &[
                (PRIMARY, rect(0, 0, 1920, 1080), 1.0),
                (SECOND, rect(1920, 0, 3840, 1080), 1.0),
            ],
            Vec::new(),
        );
        let items = items();
        let c = center(m.frame_px(&items[0]).unwrap());
        let actions = drag(&mut m, &items, c, pt(c.x + 1920, c.y));
        match actions.as_slice() {
            [Action::Commit { monitor, cells, .. }] => {
                assert_eq!(monitor, SECOND);
                // Same cell position on an identical monitor.
                assert_eq!(*cells, items[0].rect);
            }
            other => panic!("{other:?}"),
        }
        // Items on the second monitor use its geometry.
        let mut on2 = item("c", (0, 4, 2, 2));
        on2.monitor = SECOND.into();
        assert!(m.frame_px(&on2).unwrap().left >= 1920);
        // Unknown monitors fall back to the primary one.
        on2.monitor = "gone".into();
        assert!(m.frame_px(&on2).unwrap().left < 1920);
    }

    // ── resizing ──

    #[test]
    fn resize_snaps_and_commits() {
        let mut m = mode();
        let items = items();
        let r = m.frame_px(&items[0]).unwrap();
        let p = pitch(&m);
        let from = pt(r.right - 4, r.bottom - 4);
        let actions = drag(&mut m, &items, from, pt(from.x + p, from.y + p));
        match actions.as_slice() {
            [
                Action::Commit {
                    id, cells, from, ..
                },
            ] => {
                assert_eq!(id, "a");
                assert_eq!(*cells, (2, 4, 4, 3));
                // Top-left stays put while resizing.
                assert_eq!((from.left, from.top), (r.left, r.top));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn resize_respects_min_and_max() {
        let items = items();
        // Much bigger than max (6x4).
        let mut m = mode();
        let r = m.frame_px(&items[0]).unwrap();
        let from = pt(r.right - 4, r.bottom - 4);
        let actions = drag(&mut m, &items, from, pt(1900, 1070));
        assert!(matches!(
            actions.as_slice(),
            [Action::Commit {
                cells: (2, 4, 6, 4),
                ..
            }]
        ));
        // Much smaller than min (1x1): the preview never gets smaller than one cell.
        let mut m = mode();
        m.input(Input::Down(from), &items);
        let a = m.input(Input::Move(pt(r.left - 300, r.top - 300)), &items);
        let g = m.geometry(PRIMARY).unwrap();
        let one = g.rect(0, 0, 1, 1);
        match a.as_slice() {
            [Action::Preview { rect, .. }] => {
                assert_eq!(rect.right - rect.left, one.right - one.left);
                assert_eq!(rect.bottom - rect.top, one.bottom - one.top);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            m.input(Input::Up(pt(r.left - 300, r.top - 300)), &items)
                .as_slice(),
            [Action::Commit {
                cells: (2, 4, 1, 1),
                ..
            }]
        ));
    }

    #[test]
    fn resize_into_a_neighbour_cancels() {
        let mut m = mode();
        let items = items();
        let r = m.frame_px(&items[0]).unwrap();
        let p = pitch(&m);
        let from = pt(r.right - 4, r.bottom - 4);
        // "b" right next to "a": growing "a" by two columns runs into it.
        let items = vec![items[0].clone(), item("b", (5, 4, 2, 2))];
        let actions = drag(&mut m, &items, from, pt(from.x + 2 * p, from.y));
        assert!(matches!(
            actions.as_slice(),
            [Action::Cancel { id, .. }] if id == "a"
        ));
    }

    // ── gallery ──

    fn open_sheet(m: &mut EditMode) {
        m.open_gallery();
        m.spring = (1.0, 0.0);
        m.snap();
    }

    /// Centre of the tile of size variant `v` of the `g`-th widget shown.
    fn tile(m: &EditMode, g: usize, v: usize) -> POINT {
        center(m.sheet_layout().groups[g].tiles[v])
    }

    /// Gallery indices of the widgets the sheet shows.
    fn shown(m: &EditMode) -> Vec<usize> {
        m.groups.iter().map(|g| g.item).collect()
    }

    #[test]
    fn sheet_is_centred_with_a_sidebar_and_the_sizes() {
        let mut m = mode();
        open_sheet(&mut m);
        let l = m.sheet_layout();
        let (bar, _, _) = m.toolbar();
        assert!(l.sheet.top > bar.bottom);
        assert!(((l.sheet.left + l.sheet.right) / 2 - 960).abs() <= 1);
        assert!(l.side.right < l.area.left && l.field.bottom < l.entries[0].top);
        // All, Favourites, one pack.
        assert_eq!(l.entries.len(), 3);
        // Two widgets, each in its two sizes, side by side and bottom-aligned.
        assert_eq!(shown(&m), [0, 1]);
        assert_eq!(m.groups[0].variants, [(2, 2), (4, 2)]);
        let g = &l.groups[0];
        assert!(g.tiles[0].right < g.tiles[1].left);
        assert_eq!(g.tiles[0].bottom, g.tiles[1].bottom);
        // True proportions: 4×2 is twice as wide as 2×2 (plus a gap), as tall.
        let w = |r: RECT| r.right - r.left;
        let h = |r: RECT| r.bottom - r.top;
        assert_eq!(h(g.tiles[0]), h(g.tiles[1]));
        assert!((w(g.tiles[1]) as f32 / w(g.tiles[0]) as f32 - 280.0 / 136.0).abs() < 0.05);
        assert!(l.groups[1].bounds.top > g.bounds.bottom);
        assert_eq!(m.hit(&[], tile(&m, 1, 1)), Hot::Tile(1, 1));
        assert_eq!(m.hit(&[], center(l.entries[2])), Hot::Side(2));
        assert_eq!(
            m.hit(&[], pt(l.sheet.right - 3, l.sheet.bottom - 3)),
            Hot::Sheet
        );
        // The sheet is only hit while it is open.
        m.gallery_open = false;
        assert_ne!(m.hit(&[], tile(&m, 1, 1)), Hot::Tile(1, 1));
    }

    #[test]
    fn the_sheet_springs_open_and_closed() {
        let mut m = mode();
        m.open_gallery();
        let mut peak: f32 = 0.0;
        for _ in 0..120 {
            m.last_frame = Instant::now() - std::time::Duration::from_millis(16);
            m.step();
            peak = peak.max(m.spring.0);
        }
        assert!(peak > 1.0 && peak < 1.08, "a slight overshoot: {peak}");
        assert_eq!(m.spring, (1.0, 0.0));
        assert_eq!(m.sheet_alpha(), 1.0);
        m.input(Input::Escape, &[]);
        settle(&mut m);
        assert_eq!(m.spring.0, 0.0);
        assert_eq!(m.sheet_alpha(), 0.0);
    }

    #[test]
    fn click_or_plus_adds_that_size_at_the_first_free_spot() {
        let mut m = mode();
        open_sheet(&mut m);
        let add = |actions: Vec<Action>| match actions.as_slice() {
            [
                Action::Add {
                    package,
                    widget,
                    size,
                    at: None,
                },
            ] => (package.clone(), widget.clone(), *size),
            other => panic!("{other:?}"),
        };
        let at = tile(&m, 1, 1);
        assert_eq!(
            add(click(&mut m, &[], at)),
            ("dev.pack".into(), "w1".into(), (4, 2))
        );
        // The "+" sits on the tile's top-left corner.
        let plus = center(m.plus_rect(m.sheet_layout().groups[0].tiles[0]));
        assert_eq!(m.hit(&[], plus), Hot::Plus(0, 0));
        assert_eq!(
            add(click(&mut m, &[], plus)),
            ("dev.pack".into(), "w0".into(), (2, 2))
        );
        // It shows a check mark for a moment, and the sheet stays open.
        assert!(m.added_t(&sheet::preview_key(&m.gallery[0], (2, 2))) > 0.9);
        assert!(m.gallery_open);
    }

    #[test]
    fn hover_reveals_the_plus() {
        let mut m = mode();
        open_sheet(&mut m);
        m.input(Input::Move(tile(&m, 0, 1)), &[]);
        assert_eq!(m.hot, Hot::Tile(0, 1));
        settle(&mut m);
        assert_eq!(m.a("tile:0:1"), 1.0);
        assert_eq!(m.a("plus:0:1"), 0.0);
        let plus = center(m.plus_rect(m.sheet_layout().groups[0].tiles[1]));
        m.input(Input::Move(plus), &[]);
        settle(&mut m);
        assert_eq!((m.a("tile:0:1"), m.a("plus:0:1")), (1.0, 1.0));
    }

    #[test]
    fn drag_a_size_onto_the_desktop() {
        let mut m = mode();
        open_sheet(&mut m);
        let items = items();
        let g = m.geometry(PRIMARY).unwrap();
        // A free 4x2 span left of the sheet.
        let target = center(g.rect(1, 9, 4, 2));
        let from = tile(&m, 0, 1);
        assert!(m.input(Input::Down(from), &items).is_empty());
        m.input(Input::Move(pt(from.x + 30, from.y)), &items);
        assert!(!m.away, "still over the sheet");
        m.input(Input::Move(target), &items);
        assert!(m.away, "the sheet steps aside once the widget leaves it");
        assert_eq!(
            m.hit(&items, from),
            Hot::None,
            "and no longer takes the pointer"
        );
        let d = m.drag.clone().unwrap();
        assert_eq!(d.target, Some((PRIMARY.to_string(), (1, 9, 4, 2))));
        assert!(d.valid);
        // The ghost is the widget at the size dragged.
        let r = g.rect(0, 0, 4, 2);
        assert!(((d.cur.right - d.cur.left) - (r.right - r.left)).abs() <= 1);
        match m.input(Input::Up(target), &items).as_slice() {
            [
                Action::Add {
                    package,
                    widget,
                    size,
                    at: Some((monitor, cells)),
                },
            ] => {
                assert_eq!((package.as_str(), widget.as_str()), ("dev.pack", "w0"));
                assert_eq!(*size, (4, 2));
                assert_eq!((monitor.as_str(), *cells), (PRIMARY, (1, 9, 4, 2)));
            }
            other => panic!("{other:?}"),
        }
        assert!(!m.gallery_open, "the sheet closes after a drop");
    }

    #[test]
    fn a_drop_on_a_widget_or_back_on_the_sheet_adds_nothing() {
        let items = items();
        let mut m = mode();
        open_sheet(&mut m);
        let onto = center(m.frame_px(&items[0]).unwrap());
        let from = tile(&m, 0, 0);
        assert!(drag(&mut m, &items, from, onto).is_empty());
        assert!(m.gallery_open && !m.away, "the sheet comes back");

        let mut m = mode();
        open_sheet(&mut m);
        let sheet = m.sheet();
        let back = pt(sheet.right - 5, sheet.bottom - 5);
        let from = tile(&m, 0, 0);
        assert!(drag(&mut m, &items, from, back).is_empty());
        assert!(m.gallery_open);
    }

    #[test]
    fn wheel_scrolls_a_long_list() {
        let mut m = mode_with(&[(PRIMARY, rect(0, 0, 1920, 1080), 1.0)], gallery(20));
        open_sheet(&mut m);
        let l = m.sheet_layout();
        m.pointer = center(l.area);
        m.input(Input::Wheel(-120), &[]);
        assert_eq!(m.scroll_to, 96.0);
        settle(&mut m);
        assert_eq!(m.scroll, 96.0);
        assert_eq!(
            m.sheet_layout().groups[0].bounds.top,
            l.groups[0].bounds.top - 96
        );
        m.input(Input::Wheel(600), &[]);
        assert_eq!(m.scroll_to, 0.0);
        for _ in 0..100 {
            m.input(Input::Wheel(-120), &[]);
        }
        settle(&mut m);
        let l = m.sheet_layout();
        let last = l.groups.last().unwrap().bounds;
        assert!(last.bottom <= l.area.bottom && last.bottom > l.area.bottom - 40);
        // Over the sidebar the wheel does nothing.
        let before = m.scroll_to;
        m.pointer = center(l.entries[0]);
        m.input(Input::Wheel(120), &[]);
        assert_eq!(m.scroll_to, before);
    }

    #[test]
    fn short_list_does_not_scroll() {
        let mut m = mode();
        open_sheet(&mut m);
        m.pointer = tile(&m, 0, 0);
        m.input(Input::Wheel(-120), &[]);
        assert_eq!(m.scroll_to, 0.0);
    }

    // ── transitions ──

    #[test]
    fn step_settles_and_forgets_transients() {
        let mut m = mode();
        m.hot = Hot::Frame("a".into());
        m.last_frame = Instant::now() - std::time::Duration::from_millis(100);
        assert!(m.step());
        assert!(m.a("frame:a") > 0.0);
        for _ in 0..200 {
            m.last_frame = Instant::now() - std::time::Duration::from_millis(100);
            m.step();
        }
        assert_eq!(m.a("frame:a"), 1.0);
        m.hot = Hot::None;
        for _ in 0..200 {
            m.last_frame = Instant::now() - std::time::Duration::from_millis(100);
            m.step();
        }
        assert!(!m.anim.contains_key("frame:a"));
        assert_eq!(m.a("sheet"), 0.0);
        assert!(!m.step());
    }

    // ── search field ──

    fn field(text: &str, caret: usize) -> TextField {
        TextField {
            text: text.into(),
            caret,
        }
    }

    #[test]
    fn text_field_inserts_at_the_caret() {
        let mut f = TextField::default();
        f.insert("wrd");
        f.left(false);
        f.left(false);
        f.insert("o");
        assert_eq!(f, field("word", 2));
        f.end();
        f.insert(" ×時計");
        assert_eq!(f.text, "word ×時計");
        assert_eq!(f.caret, f.text.len());
        f.home();
        assert_eq!(f.caret, 0);
        f.left(false);
        assert_eq!(f.caret, 0, "stops at the start");
        f.end();
        f.right(false);
        assert_eq!(f.caret, f.text.len(), "stops at the end");
    }

    #[test]
    fn text_field_moves_and_deletes_whole_characters() {
        let mut f = field("aé😀b", 0);
        f.right(false);
        f.right(false);
        assert_eq!(f.caret, 3, "é is two bytes");
        f.right(false);
        assert_eq!(f.caret, 7, "the emoji is four");
        assert!(f.backspace());
        assert_eq!(f, field("aéb", 3));
        assert!(f.delete());
        assert_eq!(f, field("aé", 3));
        assert!(!f.delete(), "nothing after the caret");
        f.left(false);
        assert!(f.backspace());
        assert_eq!(f, field("é", 0));
        assert!(!f.backspace(), "nothing before the caret");
    }

    #[test]
    fn text_field_word_editing() {
        let mut f = field("weather  clock now", 18);
        assert!(f.backspace_word());
        assert_eq!(f, field("weather  clock ", 15));
        // Blanks right before the caret go with the word.
        assert!(f.backspace_word());
        assert_eq!(f, field("weather  ", 9));
        f.left(true);
        assert_eq!(f.caret, 0);
        f.right(true);
        assert_eq!(f.caret, 7);
        f.right(true);
        assert_eq!(f.caret, 9, "to the end of the text");
        f.home();
        assert!(!f.backspace_word());
        let mut f = field("météo 時計", "météo 時計".len());
        assert!(f.backspace_word());
        assert_eq!(f, field("météo ", 8));
    }

    #[test]
    fn wm_char_pairs_surrogates_and_drops_control_characters() {
        assert_eq!(utf16_char(b'a' as u16), Some('a'));
        assert_eq!(utf16_char(0x00E9), Some('é'));
        assert_eq!(utf16_char(0xD83D), None);
        assert_eq!(utf16_char(0xDE00), Some('😀'));
        // Backspace, Ctrl+Backspace, Esc, Ctrl+F and Enter are control characters.
        for c in [0x08, 0x7F, 0x1B, 0x06, 0x0D] {
            assert_eq!(utf16_char(c), None);
        }
        // A lone low surrogate is not a character.
        assert_eq!(utf16_char(0xDE00), None);
    }

    fn type_text(m: &mut EditMode, text: &str) {
        for c in text.chars() {
            m.input(Input::Char(c), &[]);
        }
    }

    fn key(m: &mut EditMode, vk: u16, ctrl: bool) -> Vec<Action> {
        m.input(Input::Key { vk, ctrl }, &[])
    }

    fn settle(m: &mut EditMode) {
        for _ in 0..200 {
            m.last_frame = Instant::now() - std::time::Duration::from_millis(100);
            m.step();
        }
    }

    /// A w×h 24-bit BMP (WIC decodes it like a PNG).
    fn bmp(w: u32, h: u32) -> Vec<u8> {
        let row = (w * 3).div_ceil(4) * 4;
        let size = 54 + row * h;
        let mut b = Vec::new();
        b.extend_from_slice(b"BM");
        for v in [size, 0, 54, 40] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&(w as i32).to_le_bytes());
        b.extend_from_slice(&(h as i32).to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&24u16.to_le_bytes());
        for v in [0u32, row * h, 2835, 2835, 0, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.resize(size as usize, 0x80);
        b
    }

    #[test]
    fn previews_with_the_same_path_stay_per_package() {
        // Two packs both ship `src/weather/preview.bmp`, with different images.
        let root = std::env::temp_dir().join(format!("vitrine-previews-{}", std::process::id()));
        let item = |pack: &str, w: u32| {
            let dir = root.join(pack);
            std::fs::create_dir_all(dir.join("src/weather")).unwrap();
            std::fs::write(dir.join("src/weather/preview.bmp"), bmp(w, 2)).unwrap();
            GalleryItem {
                package: format!("dev.vitrine.{pack}"),
                pack_name: pack.into(),
                widget: "weather".into(),
                name: "Weather".into(),
                description: String::new(),
                size: (2, 2),
                min: (2, 2),
                max: (2, 2),
                favourite: false,
                dir,
                preview: Some("src/weather/preview.bmp".into()),
            }
        };
        let gallery = vec![item("mond", 3), item("sargas", 5), item("mond2", 7)];
        let mut m = mode_with(&[(PRIMARY, rect(0, 0, 1920, 1080), 1.0)], gallery);
        let widths: Vec<u32> = (0..3)
            .map(|i| {
                let img = m.preview_image(i).expect("decoded");
                let (mut w, mut h) = (0, 0);
                unsafe { img.GetSize(&mut w, &mut h) }.unwrap();
                w
            })
            .collect();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(widths, [3, 5, 7]);
    }

    fn packs_gallery() -> Vec<GalleryItem> {
        let mk = |package: &str,
                  pack: &str,
                  widget: &str,
                  name: &str,
                  description: &str,
                  favourite: bool| GalleryItem {
            package: package.into(),
            pack_name: pack.into(),
            widget: widget.into(),
            name: name.into(),
            description: description.into(),
            size: (2, 2),
            min: (2, 1),
            max: (4, 2),
            favourite,
            dir: std::path::PathBuf::new(),
            preview: None,
        };
        vec![
            mk(
                "dev.clocks",
                "Clocks",
                "analog",
                "Analog clock",
                "Hands and a dial",
                true,
            ),
            mk(
                "dev.clocks",
                "Clocks",
                "word",
                "Word clock",
                "The time in words",
                false,
            ),
            mk(
                "dev.weather",
                "Weather",
                "now",
                "Météo",
                "Current conditions",
                false,
            ),
            mk(
                "dev.weather",
                "Weather",
                "week",
                "Forecast",
                "The next seven days",
                true,
            ),
        ]
    }

    fn packs_mode() -> EditMode {
        mode_with(&[(PRIMARY, rect(0, 0, 1920, 1080), 1.0)], packs_gallery())
    }

    #[test]
    fn typing_filters_by_name_description_and_package() {
        let mut m = packs_mode();
        // Typing goes nowhere while the sheet is closed.
        type_text(&mut m, "clock");
        assert!(m.search.is_empty());
        open_sheet(&mut m);
        for (query, want) in [
            ("clock", &[0, 1][..]),
            ("words", &[1]),
            ("WEATHER", &[2, 3]),
            ("meteo", &[2]),
            ("seven weather", &[3]),
            ("", &[0, 1, 2, 3]),
        ] {
            m.search = TextField::default();
            type_text(&mut m, query);
            m.refilter();
            assert_eq!(shown(&m), want, "{query:?}");
        }
    }

    #[test]
    fn search_keys_edit_the_query() {
        let mut m = packs_mode();
        open_sheet(&mut m);
        type_text(&mut m, "word clock");
        assert_eq!(shown(&m), [1]);
        key(&mut m, VK_BACK.0, true);
        assert_eq!(m.search.text, "word ");
        key(&mut m, VK_BACK.0, false);
        key(&mut m, VK_HOME.0, false);
        key(&mut m, VK_DELETE.0, false);
        assert_eq!(m.search, field("ord", 0));
        type_text(&mut m, "w");
        assert_eq!(m.search, field("word", 1));
        key(&mut m, VK_END.0, false);
        type_text(&mut m, "s");
        key(&mut m, VK_LEFT.0, false);
        assert_eq!(m.search, field("words", 4));
        assert_eq!(shown(&m), [1], "found in the description");
    }

    #[test]
    fn ctrl_f_opens_the_sheet_with_a_fresh_search() {
        let mut m = packs_mode();
        assert!(key(&mut m, b'F' as u16, true).is_empty());
        assert!(m.gallery_open);
        type_text(&mut m, "météo");
        assert_eq!(m.search.text, "météo");
        // Ctrl+F again keeps the query; a plain F key is left to WM_CHAR.
        key(&mut m, b'F' as u16, true);
        key(&mut m, b'F' as u16, false);
        assert_eq!(m.search.text, "météo");
        m.input(Input::Escape, &[]);
        assert!(!m.gallery_open);
        m.open_gallery();
        assert!(m.search.is_empty() && shown(&m) == [0, 1, 2, 3]);
    }

    #[test]
    fn escape_closes_the_sheet_then_edit_mode() {
        let mut m = packs_mode();
        open_sheet(&mut m);
        type_text(&mut m, "zzz");
        assert!(m.input(Input::Escape, &[]).is_empty());
        assert!(!m.gallery_open);
        assert!(matches!(
            m.input(Input::Escape, &[]).as_slice(),
            [Action::Close]
        ));
    }

    #[test]
    fn clear_button_and_caret_placement() {
        let mut m = packs_mode();
        open_sheet(&mut m);
        let l = m.sheet_layout();
        // No clear button while the field is empty.
        assert_eq!(m.hit(&[], center(l.clear)), Hot::Search);
        assert_eq!(m.hit(&[], center(l.field)), Hot::Search);
        type_text(&mut m, "clock");
        assert_eq!(m.hit(&[], center(l.clear)), Hot::Clear);
        click(&mut m, &[], pt(l.field.left + 36, center(l.field).y));
        assert_eq!(m.search.caret, 0);
        click(&mut m, &[], pt(l.clear.left - 4, center(l.field).y));
        assert_eq!(m.search.caret, 5);
        assert!(click(&mut m, &[], center(l.clear)).is_empty());
        assert!(m.search.is_empty() && m.gallery_open);
    }

    #[test]
    fn typing_restarts_the_caret_blink() {
        let mut m = packs_mode();
        open_sheet(&mut m);
        m.caret_epoch = Instant::now() - std::time::Duration::from_secs(5);
        type_text(&mut m, "a");
        assert!(m.caret_epoch.elapsed().as_millis() < BLINK_MS as u128);
        m.caret_epoch = Instant::now() - std::time::Duration::from_secs(5);
        key(&mut m, VK_LEFT.0, false);
        assert!(m.caret_epoch.elapsed().as_millis() < BLINK_MS as u128);
    }

    // ── sidebar ──

    #[test]
    fn sidebar_selects_all_favourites_or_a_pack() {
        let mut m = packs_mode();
        open_sheet(&mut m);
        let l = m.sheet_layout();
        assert_eq!(l.entries.len(), 4);
        assert!(l.entries.windows(2).all(|w| w[0].bottom <= w[1].top));
        // A divider separates All and Favourites from the packs.
        assert!(l.entries[2].top - l.entries[1].bottom > l.entries[1].top - l.entries[0].bottom);
        assert_eq!(
            m.entry_label(&Selection::Pack("dev.weather".into())),
            "Weather"
        );
        assert_eq!(m.entry_count(&Selection::Favourites), 2);

        assert!(click(&mut m, &[], center(l.entries[1])).is_empty());
        assert_eq!(m.selection, Selection::Favourites);
        assert_eq!(shown(&m), [0, 3]);
        click(&mut m, &[], center(l.entries[3]));
        assert_eq!(m.tab().as_deref(), Some("dev.weather"));
        assert_eq!(shown(&m), [2, 3]);
        // The selection and the search combine.
        type_text(&mut m, "seven");
        assert_eq!(shown(&m), [3]);
        type_text(&mut m, " clock");
        assert!(shown(&m).is_empty());
        settle(&mut m);
        assert_eq!(m.a("empty"), 1.0);
        assert_eq!(m.hit(&[], center(m.sheet_layout().area)), Hot::Sheet);
        m.clear_search();
        click(&mut m, &[], center(l.entries[0]));
        assert_eq!(m.tab(), None);
        assert_eq!(shown(&m), [0, 1, 2, 3]);
    }

    #[test]
    fn the_indicator_slides_to_the_selected_entry() {
        let mut m = packs_mode();
        open_sheet(&mut m);
        let start = m.pill;
        assert_eq!(start, m.pill_target());
        m.select(3);
        let end = m.pill_target();
        m.last_frame = Instant::now() - std::time::Duration::from_millis(16);
        assert!(m.step());
        // Moving down, the leading (bottom) edge runs ahead of the top one.
        let top = (m.pill.0 - start.0) / (end.0 - start.0);
        let bottom = (m.pill.1 - start.1) / (end.1 - start.1);
        assert!(bottom > top, "{top} {bottom}");
        settle(&mut m);
        assert_eq!(m.pill, end);
    }

    #[test]
    fn the_last_selection_is_remembered() {
        let monitors = [(PRIMARY, rect(0, 0, 1920, 1080), 1.0)];
        let m = mode_full(&monitors, packs_gallery(), Some("dev.weather".into()));
        assert_eq!(m.tab().as_deref(), Some("dev.weather"));
        assert_eq!(shown(&m), [2, 3]);
        let m = mode_full(
            &monitors,
            packs_gallery(),
            Some(vitrine_widgets::gallery::FAVOURITES.into()),
        );
        assert_eq!(m.selection, Selection::Favourites);
        // A package that is no longer installed falls back to all.
        let m = mode_full(&monitors, packs_gallery(), Some("dev.gone".into()));
        assert_eq!(m.tab(), None);
        assert_eq!(shown(&m), [0, 1, 2, 3]);
    }

    #[test]
    fn filtering_resets_the_scroll_and_fades_the_list_in() {
        let mut m = mode_with(&[(PRIMARY, rect(0, 0, 1920, 1080), 1.0)], gallery(20));
        open_sheet(&mut m);
        settle(&mut m);
        assert_eq!(m.a("content"), 1.0);
        m.pointer = center(m.sheet_layout().area);
        m.input(Input::Wheel(-360), &[]);
        settle(&mut m);
        assert!(m.scroll > 0.0);
        type_text(&mut m, "19");
        assert_eq!(shown(&m), [19]);
        assert_eq!((m.scroll, m.scroll_to), (0.0, 0.0));
        assert_eq!(m.a("content"), 0.0);
        settle(&mut m);
        assert_eq!(m.a("content"), 1.0);
    }

    // ── keyboard ──

    #[test]
    fn tab_arrows_and_enter_add_from_the_keyboard() {
        let mut m = packs_mode();
        open_sheet(&mut m);
        assert_eq!(m.focus, Focus::Side);
        // In the sidebar, up and down change the selection.
        key(&mut m, VK_DOWN.0, false);
        assert_eq!(m.selection, Selection::Favourites);
        key(&mut m, VK_UP.0, false);
        key(&mut m, VK_UP.0, false);
        assert_eq!(m.selection, Selection::All);
        // Tab moves to the grid, where arrows walk the sizes.
        key(&mut m, VK_TAB.0, false);
        assert_eq!((m.focus, m.focused()), (Focus::Grid, Some((0, 0))));
        assert!(m.keyboard);
        key(&mut m, VK_RIGHT.0, false);
        key(&mut m, VK_RIGHT.0, false);
        assert_eq!(m.focused(), Some((0, 2)));
        key(&mut m, VK_DOWN.0, false);
        assert_eq!(m.focused(), Some((1, 2)));
        settle(&mut m);
        assert_eq!(m.a("ring:1:2"), 1.0);
        // Enter adds the size under the cursor (2×1, 2×2, 4×2).
        match key(&mut m, VK_RETURN.0, false).as_slice() {
            [
                Action::Add {
                    widget,
                    size,
                    at: None,
                    ..
                },
            ] => assert_eq!((widget.as_str(), *size), ("word", (4, 2))),
            other => panic!("{other:?}"),
        }
        // Tab goes back to the sidebar; typing searches from anywhere.
        key(&mut m, VK_TAB.0, false);
        assert_eq!((m.focus, m.focused()), (Focus::Side, None));
        key(&mut m, VK_TAB.0, false);
        type_text(&mut m, "fore");
        assert_eq!(m.focus, Focus::Side);
        assert_eq!(shown(&m), [3]);
        // Enter in the sidebar jumps into the grid.
        key(&mut m, VK_RETURN.0, false);
        assert_eq!(m.focused(), Some((0, 0)));
        // A click hides the focus rings.
        m.input(Input::Down(pt(2, 2)), &[]);
        assert!(!m.keyboard);
    }

    #[test]
    fn keyboard_scrolls_the_focused_widget_into_view() {
        let mut m = mode_with(&[(PRIMARY, rect(0, 0, 1920, 1080), 1.0)], gallery(20));
        open_sheet(&mut m);
        key(&mut m, VK_TAB.0, false);
        key(&mut m, VK_END.0, false);
        assert_eq!(m.focused(), Some((19, 1)));
        settle(&mut m);
        let l = m.sheet_layout();
        let b = l.groups[19].bounds;
        assert!(b.bottom <= l.area.bottom && b.top >= l.area.top, "{b:?}");
        key(&mut m, VK_HOME.0, false);
        settle(&mut m);
        assert_eq!(m.scroll, 0.0);
    }

    #[test]
    fn preview_jobs_cover_the_selection_once() {
        let mut m = packs_mode();
        assert!(
            m.preview_jobs().is_empty(),
            "nothing while the sheet is closed"
        );
        open_sheet(&mut m);
        m.select(3);
        let jobs = m.preview_jobs();
        // Two weather widgets, three sizes each.
        assert_eq!(jobs.len(), 6);
        assert_eq!((jobs[0].widget.as_str(), jobs[0].size), ("now", (2, 1)));
        assert!(m.preview_jobs().is_empty(), "asked only once");
        m.add_preview(jobs[0].key.clone(), 2, 1, Arc::new(vec![0; 8]));
        assert!(m.pixels.contains_key(&jobs[0].key));
        m.select(0);
        assert_eq!(m.preview_jobs().len(), 6, "the clocks' sizes follow");
    }
}
