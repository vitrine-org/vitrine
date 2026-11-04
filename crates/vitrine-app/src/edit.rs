//! Edit mode: open windows step aside, and a translucent overlay per monitor
//! shows the grid. Widgets follow the pointer while dragged and settle into
//! their cell with an animation; the corner handle resizes them live; hover
//! controls remove a widget or open its settings; the gallery sheet lets you
//! drag new widgets onto the desktop.
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
    ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Interface, PCWSTR, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::grid::Geometry;
use crate::runtime::HostMsg;
use vitrine_render::compose::WindowVisual;
use vitrine_render::gpu::Gpu;
use vitrine_render::image::{DirAssets, ImageCache};

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
    pub package: String,
    pub widget: String,
    pub name: String,
    pub size: (i32, i32),
    pub dir: std::path::PathBuf,
    pub preview: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub enum Input {
    Down(POINT),
    Move(POINT),
    Up(POINT),
    RightUp(POINT),
    Wheel(i16),
    Escape,
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
    /// Add a widget, at a cell span or (None) the first free spot.
    Add {
        package: String,
        widget: String,
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
    Card(usize),
    Sheet,
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
    New {
        card: usize,
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
}

pub struct EditMode {
    overlays: Vec<Overlay>,
    /// One per overlay (kept apart so painting can borrow the rest immutably).
    visuals: Vec<WindowVisual>,
    gallery: Vec<GalleryItem>,
    previews: HashMap<usize, Option<ID2D1Bitmap>>,
    images: ImageCache,
    dwrite: IDWriteFactory5,
    hot: Hot,
    pressed: Hot,
    drag: Option<Drag>,
    pointer: POINT,
    gallery_open: bool,
    scroll: f32,
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
        let make = |family: PCWSTR, size: f32, weight: DWRITE_FONT_WEIGHT, center: bool| unsafe {
            let f = dwrite.CreateTextFormat(
                family,
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size * s,
                w!(""),
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
        Ok(Fonts {
            label: make(text, 12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?,
            button: make(text, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?,
            caption: make(text, 13.0, DWRITE_FONT_WEIGHT_NORMAL, true)?,
            icon: make(icon_family(dwrite), 14.0, DWRITE_FONT_WEIGHT_NORMAL, true)?,
        })
    }
}

impl EditMode {
    pub fn open(
        gpu: &Gpu,
        hinst: HINSTANCE,
        monitors: Vec<(String, Geometry, RECT)>,
        gallery: Vec<GalleryItem>,
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
        Ok(EditMode {
            overlays,
            visuals,
            gallery,
            previews: HashMap::new(),
            images: ImageCache::new(gpu.wic.clone()),
            dwrite: gpu.dwrite.clone(),
            hot: Hot::None,
            pressed: Hot::None,
            drag: None,
            pointer: POINT::default(),
            gallery_open: false,
            scroll: 0.0,
            anim: HashMap::new(),
            last_frame: Instant::now(),
            dirty: true,
            closing: false,
        })
    }

    /// Fades the overlays out; the runtime drops the edit mode shortly after.
    pub fn begin_close(&mut self, gpu: &Gpu) {
        self.closing = true;
        for v in &mut self.visuals {
            let _ = v.animate_opacity(gpu, 1.0, 0.0, 0.16);
        }
    }

    pub fn open_gallery(&mut self) {
        self.gallery_open = true;
        self.dirty = true;
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
        let (add_w, done_w, pad, h) = (px(148.0), px(100.0), px(6.0), px(44.0));
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

    fn sheet_t(&self) -> f32 {
        ease(*self.anim.get("sheet").unwrap_or(&0.0))
    }

    /// Gallery sheet and its cards (slides up from the bottom of the primary monitor).
    fn sheet(&self) -> (RECT, Vec<RECT>) {
        let Some(o) = self.primary() else {
            return Default::default();
        };
        let s = o.geo.scale;
        let px = |v: f32| (v * s).round() as i32;
        let (card_w, card_h, gap, pad, head) = (px(176.0), px(150.0), px(12.0), px(16.0), px(28.0));
        let n = self.gallery.len().max(1) as i32;
        let content = n * card_w + (n - 1) * gap + 2 * pad;
        let max_w = (o.rect.right - o.rect.left) - px(48.0);
        let width = content.min(max_w);
        let height = pad + head + card_h + pad;
        let hidden = height + px(40.0);
        let slide = ((1.0 - self.sheet_t()) * hidden as f32).round() as i32;
        let left = (o.rect.left + o.rect.right) / 2 - width / 2;
        let grid_bottom =
            o.geo.origin.1 as i32 + (o.geo.rows as f32 * (o.geo.cell + o.geo.gap) * s) as i32;
        let bottom = grid_bottom.min(o.rect.bottom - px(24.0)) + slide;
        let sheet = RECT {
            left,
            top: bottom - height,
            right: left + width,
            bottom,
        };
        let scroll = self.scroll.round() as i32;
        let cards = (0..self.gallery.len() as i32)
            .map(|i| {
                let x = sheet.left + pad + i * (card_w + gap) - scroll;
                RECT {
                    left: x,
                    top: sheet.top + pad + head,
                    right: x + card_w,
                    bottom: sheet.top + pad + head + card_h,
                }
            })
            .collect();
        (sheet, cards)
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
        if self.gallery_open {
            let (sheet, cards) = self.sheet();
            if contains(&sheet, pt) {
                return cards
                    .iter()
                    .position(|c| contains(c, pt))
                    .map(Hot::Card)
                    .unwrap_or(Hot::Sheet);
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
            Input::Escape => {
                if self.gallery_open {
                    self.gallery_open = false;
                } else {
                    actions.push(Action::Close);
                }
            }
            Input::Wheel(delta) => {
                if self.gallery_open
                    && matches!(self.hit(items, self.pointer), Hot::Card(_) | Hot::Sheet)
                {
                    let (sheet, cards) = self.sheet();
                    let s = self.scale();
                    let content = match (cards.first(), cards.last()) {
                        (Some(a), Some(b)) => b.right - a.left,
                        _ => 0,
                    };
                    let max =
                        (content - (sheet.right - sheet.left) + (32.0 * s) as i32).max(0) as f32;
                    self.scroll = (self.scroll - delta as f32 / 120.0 * 96.0 * s).clamp(0.0, max);
                }
            }
            Input::Down(pt) => {
                self.pointer = pt;
                let hot = self.hit(items, pt);
                self.pressed = hot.clone();
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
                    Hot::Card(i) => {
                        let r = self.sheet().1[*i];
                        Some(Drag {
                            kind: DragKind::New { card: *i },
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
                        Hot::Add | Hot::Done | Hot::Remove(_) | Hot::Settings(_) | Hot::Card(_) => {
                            IDC_HAND
                        }
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
                    if let DragKind::New { card } = drag.kind {
                        // A click on a card: first free place.
                        let g = &self.gallery[card];
                        actions.push(Action::Add {
                            package: g.package.clone(),
                            widget: g.widget.clone(),
                            at: None,
                        });
                        return actions;
                    }
                }
                if pressed == released_on {
                    match pressed {
                        Hot::Done => actions.push(Action::Close),
                        Hot::Add => self.gallery_open = !self.gallery_open,
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
            DragKind::New { card } => {
                let item = &self.gallery[*card];
                let (sheet, _) = self.sheet();
                let over_sheet = self.gallery_open && contains(&sheet, pt);
                match self.monitor_at(pt).filter(|_| !over_sheet) {
                    Some((monitor, g)) => {
                        // The ghost is the widget at its default size, centred on the pointer.
                        let size = g.rect(0, 0, item.size.0, item.size.1);
                        let (w, h) = (size.right - size.left, size.bottom - size.top);
                        drag.cur = RECT {
                            left: pt.x - w / 2,
                            top: pt.y - h / 2,
                            right: pt.x + w / 2,
                            bottom: pt.y + h / 2,
                        };
                        let (x, y) = g.snap(drag.cur.left as f32, drag.cur.top as f32);
                        let target = g.clamp(x, y, item.size.0, item.size.1);
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
            DragKind::New { card } => match drag.target {
                Some((monitor, cells)) if drag.valid => {
                    let g = &self.gallery[card];
                    self.gallery_open = false;
                    vec![Action::Add {
                        package: g.package.clone(),
                        widget: g.widget.clone(),
                        at: Some((monitor, cells)),
                    }]
                }
                _ => Vec::new(),
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
        let mut wanted: HashMap<String, f32> = HashMap::new();
        for k in self.anim.keys() {
            wanted.insert(k.clone(), 0.0);
        }
        wanted.insert("sheet".into(), if self.gallery_open { 1.0 } else { 0.0 });
        wanted.insert("drag".into(), if dragging { 1.0 } else { 0.0 });
        wanted.insert(
            "hot:add".into(),
            if self.hot == Hot::Add { 1.0 } else { 0.0 },
        );
        wanted.insert(
            "hot:done".into(),
            if self.hot == Hot::Done { 1.0 } else { 0.0 },
        );
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
            Hot::Card(i) => {
                wanted.insert(format!("card:{i}"), 1.0);
            }
            _ => {}
        }
        let mut moving = false;
        let rate = 1.0 - (-dt * 16.0).exp();
        for (k, target) in wanted {
            let v = self.anim.entry(k.clone()).or_insert(0.0);
            let next = *v + (target - *v) * rate;
            if (next - target).abs() < 0.004 {
                *v = target;
                let transient =
                    k.starts_with("frame:") || k.starts_with("card:") || k.starts_with("btn:");
                if target == 0.0 && transient {
                    self.anim.remove(&k);
                }
            } else {
                *v = next;
                moving = true;
            }
        }
        moving
    }

    fn a(&self, key: &str) -> f32 {
        *self.anim.get(key).unwrap_or(&0.0)
    }

    fn preview_bitmap(&mut self, dc: &ID2D1DeviceContext, i: usize) -> Option<ID2D1Bitmap> {
        if let Some(b) = self.previews.get(&i) {
            return b.clone();
        }
        let item = &self.gallery[i];
        let bmp = item.preview.as_ref().and_then(|rel| {
            let wic = self.images.get(rel, &DirAssets(item.dir.clone()))?;
            unsafe {
                dc.CreateBitmapFromWicBitmap(&wic, None)
                    .ok()
                    .and_then(|b| b.cast().ok())
            }
        });
        self.previews.insert(i, bmp.clone());
        bmp
    }

    /// Repaints when needed. Returns how soon the loop should call again.
    pub fn paint(&mut self, gpu: &Gpu, dc: &ID2D1DeviceContext, items: &[Item]) -> u32 {
        let moving = self.step();
        if !self.dirty && !moving {
            return u32::MAX;
        }
        self.dirty = false;
        let previews: Vec<Option<ID2D1Bitmap>> = (0..self.gallery.len())
            .map(|i| self.preview_bitmap(dc, i))
            .collect();
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
                previews: &previews,
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
        if moving { 16 } else { u32::MAX }
    }
}

/// Draws one overlay. Coordinates are screen pixels shifted by (`dx`, `dy`).
struct Painter<'a> {
    edit: &'a EditMode,
    index: usize,
    items: &'a [Item],
    previews: &'a [Option<ID2D1Bitmap>],
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
                    "Add widget",
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
                draw_text("Done", &o.text.button, rf(done), color(0x000000, 0.92));
                // Hint.
                let hint_top = b.bottom + 10.0 * s;
                let hint = if e.gallery_open {
                    "Drag a widget onto the desktop, or click it to add it"
                } else {
                    "Drag widgets to move them · drag the corner dot to resize · Esc to finish"
                };
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

                // Gallery sheet.
                let alpha = e.sheet_t();
                if alpha > 0.001 {
                    let (sheet, cards) = e.sheet();
                    let sr = rf(sheet);
                    shadow(sr, 16.0, 0.14 * alpha);
                    fill(rr(sr, 16.0), color(0x202020, 0.96 * alpha));
                    stroke(rr(sr, 16.0), color(0xFFFFFF, 0.10 * alpha), 1.0);
                    let title = D2D_RECT_F {
                        left: sr.left + 18.0 * s,
                        top: sr.top + 12.0 * s,
                        right: sr.right - 18.0 * s,
                        bottom: sr.top + 40.0 * s,
                    };
                    draw_text(
                        "Widgets",
                        &o.text.label,
                        title,
                        color(0xFFFFFF, 0.7 * alpha),
                    );
                    dc.PushAxisAlignedClip(&sr, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    let new_drag = drag.and_then(|d| match d.kind {
                        DragKind::New { card } => Some(card),
                        _ => None,
                    });
                    for (i, c) in cards.iter().enumerate() {
                        let hover = e.a(&format!("card:{i}"));
                        let lift = 3.0 * s * hover;
                        let base = rf(*c);
                        let cr = D2D_RECT_F {
                            top: base.top - lift,
                            bottom: base.bottom - lift,
                            ..base
                        };
                        let faded = if new_drag == Some(i) { 0.4 } else { 1.0 };
                        fill(rr(cr, 10.0), color(0x2B2B2B, alpha * faded));
                        stroke(
                            rr(cr, 10.0),
                            mix(color(0xFFFFFF, 0.08), color(ACCENT, 0.9), hover),
                            1.0,
                        );
                        let img = D2D_RECT_F {
                            left: cr.left + 8.0 * s,
                            top: cr.top + 8.0 * s,
                            right: cr.right - 8.0 * s,
                            bottom: cr.bottom - 36.0 * s,
                        };
                        fill(rr(img, 6.0), color(0x161616, alpha * faded));
                        if let Some(bmp) = &self.previews[i] {
                            draw_contained(dc, bmp, img, alpha * faded);
                        }
                        let label = D2D_RECT_F {
                            left: cr.left + 10.0 * s,
                            top: cr.bottom - 32.0 * s,
                            right: cr.right - 10.0 * s,
                            bottom: cr.bottom - 8.0 * s,
                        };
                        draw_text(
                            &e.gallery[i].name,
                            &o.text.label,
                            label,
                            color(0xFFFFFF, 0.92 * alpha * faded),
                        );
                    }
                    dc.PopAxisAlignedClip();
                }
            }

            // Ghost of a widget being dragged out of the gallery.
            if let Some(d) = drag
                && let DragKind::New { card } = d.kind
                && contains(&o.rect, e.pointer)
            {
                let r = rf(d.cur);
                shadow(r, 10.0, 0.12);
                fill(rr(r, 10.0), color(0x202020, 0.85));
                stroke(rr(r, 10.0), color(ACCENT, 0.9), 2.0);
                if let Some(bmp) = &self.previews[card] {
                    let inner = D2D_RECT_F {
                        left: r.left + 6.0 * s,
                        top: r.top + 6.0 * s,
                        right: r.right - 6.0 * s,
                        bottom: r.bottom - 6.0 * s,
                    };
                    draw_contained(dc, bmp, inner, 0.95);
                }
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
        EditMode {
            overlays,
            visuals: Vec::new(),
            gallery,
            previews: HashMap::new(),
            images: ImageCache::new(wic),
            dwrite,
            hot: Hot::None,
            pressed: Hot::None,
            drag: None,
            pointer: POINT::default(),
            gallery_open: false,
            scroll: 0.0,
            anim: HashMap::new(),
            last_frame: Instant::now(),
            dirty: false,
            closing: false,
        }
    }

    fn mode() -> EditMode {
        mode_with(&[(PRIMARY, rect(0, 0, 1920, 1080), 1.0)], gallery(2))
    }

    fn gallery(n: usize) -> Vec<GalleryItem> {
        (0..n)
            .map(|i| GalleryItem {
                package: "dev.pack".into(),
                widget: format!("w{i}"),
                name: format!("Widget {i}"),
                size: (2, 2),
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
        m.anim.insert("sheet".into(), 1.0);
    }

    #[test]
    fn sheet_slides_up_and_holds_cards() {
        let mut m = mode();
        let (hidden, _) = m.sheet();
        open_sheet(&mut m);
        let (sheet, cards) = m.sheet();
        assert!(hidden.top > sheet.top);
        assert_eq!(cards.len(), 2);
        assert!(cards.iter().all(|c| contains(&sheet, center(*c))));
        assert_eq!(m.hit(&[], center(cards[1])), Hot::Card(1));
        assert_eq!(m.hit(&[], pt(sheet.left + 3, sheet.top + 3)), Hot::Sheet);
        // The sheet is only hit while the gallery is open.
        m.gallery_open = false;
        assert_ne!(m.hit(&[], center(cards[1])), Hot::Card(1));
    }

    #[test]
    fn click_on_a_card_adds_at_first_free_spot() {
        let mut m = mode();
        open_sheet(&mut m);
        let card = m.sheet().1[1];
        match click(&mut m, &[], center(card)).as_slice() {
            [
                Action::Add {
                    package,
                    widget,
                    at: None,
                },
            ] => {
                assert_eq!(package, "dev.pack");
                assert_eq!(widget, "w1");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn drag_a_card_onto_the_desktop() {
        let mut m = mode();
        open_sheet(&mut m);
        let items = items();
        let card = m.sheet().1[0];
        let g = m.geometry(PRIMARY).unwrap();
        // Centre of a free 2x2 span at (16, 2).
        let target = center(g.rect(16, 2, 2, 2));
        let actions = drag(&mut m, &items, center(card), target);
        match actions.as_slice() {
            [
                Action::Add {
                    package,
                    widget,
                    at: Some((monitor, cells)),
                },
            ] => {
                assert_eq!((package.as_str(), widget.as_str()), ("dev.pack", "w0"));
                assert_eq!(monitor, PRIMARY);
                assert_eq!(*cells, (16, 2, 2, 2));
            }
            other => panic!("{other:?}"),
        }
        assert!(!m.gallery_open, "the sheet closes after a drop");
    }

    #[test]
    fn card_dropped_on_a_widget_or_the_sheet_is_dropped() {
        let items = items();
        let mut m = mode();
        open_sheet(&mut m);
        let card = m.sheet().1[0];
        let onto = center(m.frame_px(&items[0]).unwrap());
        assert!(drag(&mut m, &items, center(card), onto).is_empty());
        assert!(m.gallery_open);

        let mut m = mode();
        open_sheet(&mut m);
        let (sheet, cards) = m.sheet();
        let back = pt(sheet.right - 5, sheet.top + 5);
        assert!(drag(&mut m, &items, center(cards[0]), back).is_empty());
    }

    #[test]
    fn wheel_scrolls_a_long_gallery() {
        let mut m = mode_with(&[(PRIMARY, rect(0, 0, 1920, 1080), 1.0)], gallery(20));
        open_sheet(&mut m);
        let (sheet, _) = m.sheet();
        m.pointer = pt(sheet.left + 30, sheet.bottom - 30);
        m.input(Input::Wheel(-120), &[]);
        assert_eq!(m.scroll, 96.0);
        // Scrolling back past the start clamps at 0.
        m.input(Input::Wheel(600), &[]);
        assert_eq!(m.scroll, 0.0);
        // And past the end clamps at the content width.
        for _ in 0..100 {
            m.input(Input::Wheel(-120), &[]);
        }
        let (sheet, cards) = m.sheet();
        let last = cards.last().unwrap();
        assert!(last.right <= sheet.right && last.right > sheet.right - 64);
        // Off the sheet the wheel does nothing.
        let before = m.scroll;
        m.pointer = pt(5, 500);
        m.input(Input::Wheel(120), &[]);
        assert_eq!(m.scroll, before);
    }

    #[test]
    fn short_gallery_does_not_scroll() {
        let mut m = mode();
        open_sheet(&mut m);
        m.pointer = center(m.sheet().1[0]);
        m.input(Input::Wheel(-120), &[]);
        assert_eq!(m.scroll, 0.0);
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
}
