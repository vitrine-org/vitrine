//! Windows 11 style popup menus (tray and widget menus).
//!
//! Each menu level is a small top-level window with the system acrylic
//! backdrop and rounded corners from DWM; the content (rows, icons, toggles)
//! is drawn with Direct2D on DirectComposition. [`show`] runs a modal loop,
//! like `TrackPopupMenu`, and returns the chosen command.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::shellapi::Item;
use vitrine_render::compose::WindowVisual;
use vitrine_render::gpu::Gpu;

/// How a command is presented: an icon (Segoe Fluent Icons code point) and
/// whether its checked state is shown as a toggle switch.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Decor {
    pub icon: Option<char>,
    pub toggle: bool,
}

/// Command id passed to the decor function for submenu rows.
pub const SUBMENU: u32 = u32::MAX;

/// Where the root menu opens relative to the anchor point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    /// Above the point (tray icon on the taskbar).
    Above(POINT),
    /// Below the point (right-click on the desktop).
    Below(POINT),
}

// ── metrics (DIPs) ────────────────────────────────────────────────────

const PAD: f32 = 4.0;
const ROW_H: f32 = 34.0;
const SEP_H: f32 = 9.0;
const INSET: f32 = 12.0;
const ICON_W: f32 = 16.0;
const ICON_GAP: f32 = 12.0;
const TRAIL_GAP: f32 = 28.0;
const TOGGLE_W: f32 = 40.0;
const TOGGLE_H: f32 = 20.0;
const CHEVRON_W: f32 = 12.0;
const MIN_W: f32 = 220.0;
const TEXT_PT: f32 = 14.0;
const SUBMENU_DELAY: Duration = Duration::from_millis(220);

const GLYPH_CHEVRON: char = '\u{E76C}';
const GLYPH_CHECK: char = '\u{E73E}';

/// Vertical layout of a menu: `(top, height)` of each item, and total height.
pub fn layout_rows(items: &[Item]) -> (Vec<(f32, f32)>, f32) {
    let mut y = PAD;
    let rows = items
        .iter()
        .map(|it| {
            let h = if matches!(it, Item::Separator) {
                SEP_H
            } else {
                ROW_H
            };
            let r = (y, h);
            y += h;
            r
        })
        .collect();
    (rows, y + PAD)
}

/// Width of a menu from its label widths and what each row shows.
pub fn panel_width(labels: &[f32], any_icon: bool, trailing: &[f32]) -> f32 {
    let lead = INSET + if any_icon { ICON_W + ICON_GAP } else { 0.0 };
    let widest = labels
        .iter()
        .zip(trailing.iter().chain(std::iter::repeat(&0.0)))
        .map(|(l, t)| lead + l + if *t > 0.0 { TRAIL_GAP + t } else { 0.0 } + INSET)
        .fold(0.0f32, f32::max);
    (widest + 2.0 * PAD).max(MIN_W).ceil()
}

/// Whether a row can be hovered and chosen.
pub fn selectable(item: &Item) -> bool {
    match item {
        Item::Action { enabled, .. } => *enabled,
        Item::Sub { .. } => true,
        Item::Separator => false,
    }
}

/// Row under `y` (DIPs), if it is selectable.
pub fn row_at(items: &[Item], rows: &[(f32, f32)], y: f32) -> Option<usize> {
    rows.iter()
        .position(|(top, h)| y >= *top && y < top + h)
        .filter(|&i| selectable(&items[i]))
}

/// Next selectable row from `from` in direction `dir` (±1), wrapping.
pub fn step(items: &[Item], from: Option<usize>, dir: i32) -> Option<usize> {
    let n = items.len() as i32;
    if n == 0 {
        return None;
    }
    let mut i = match from {
        Some(i) => i as i32,
        None if dir > 0 => -1,
        None => n,
    };
    for _ in 0..n {
        i = (i + dir).rem_euclid(n);
        if selectable(&items[i as usize]) {
            return Some(i as usize);
        }
    }
    None
}

// ── theme ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    pub accent: D2D1_COLOR_F,
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

/// `#rrggbb` → colour (blue if malformed).
pub fn parse_hex(s: &str) -> D2D1_COLOR_F {
    let v = u32::from_str_radix(s.trim_start_matches('#'), 16).unwrap_or(0x0078d4);
    let c = |shift: u32| ((v >> shift) & 0xFF) as f32 / 255.0;
    rgba(c(16), c(8), c(0), 1.0)
}

impl Theme {
    pub fn current() -> Theme {
        let json = vitrine_script::appearance::appearance_json();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
        let dark = v["dark"].as_bool().unwrap_or(true);
        let base = parse_hex(v["accent"].as_str().unwrap_or("#0078d4"));
        // Windows uses a lighter accent shade on dark surfaces.
        let accent = if dark {
            rgba(
                base.r + (1.0 - base.r) * 0.45,
                base.g + (1.0 - base.g) * 0.45,
                base.b + (1.0 - base.b) * 0.45,
                1.0,
            )
        } else {
            base
        };
        Theme { dark, accent }
    }

    fn ink(&self, a: f32) -> D2D1_COLOR_F {
        if self.dark {
            rgba(1.0, 1.0, 1.0, a)
        } else {
            rgba(0.0, 0.0, 0.0, a * 0.9)
        }
    }

    fn fill(&self) -> D2D1_COLOR_F {
        if self.dark {
            rgba(0.17, 0.17, 0.17, 0.82)
        } else {
            rgba(0.98, 0.98, 0.98, 0.82)
        }
    }

    fn hover(&self) -> f32 {
        if self.dark { 0.0605 } else { 0.0373 }
    }
}

// ── window plumbing ───────────────────────────────────────────────────

const CLASS: PCWSTR = w!("Vitrine.Flyout");

#[derive(Clone, Copy, Debug)]
enum Ev {
    Move(HWND, f32, f32),
    Leave(HWND),
    Down(HWND, f32, f32),
    Up(HWND, f32, f32),
    Key(u16),
    Deactivate(HWND),
}

thread_local! {
    static EVENTS: RefCell<Vec<Ev>> = const { RefCell::new(Vec::new()) };
}

fn push(ev: Ev) {
    EVENTS.with(|e| e.borrow_mut().push(ev));
}

fn client_dips(hwnd: HWND, lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    let s = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
    (x / s, y / s)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            let (x, y) = client_dips(hwnd, lparam);
            push(Ev::Move(hwnd, x, y));
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            push(Ev::Leave(hwnd));
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            let (x, y) = client_dips(hwnd, lparam);
            push(Ev::Down(hwnd, x, y));
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let (x, y) = client_dips(hwnd, lparam);
            push(Ev::Up(hwnd, x, y));
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            push(Ev::Key(wparam.0 as u16));
            LRESULT(0)
        }
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            push(Ev::Deactivate(HWND(lparam.0 as _)));
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn dpi_scale(pt: POINT) -> f32 {
    unsafe {
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let (mut x, mut y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        x.max(96) as f32 / 96.0
    }
}

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
        w!("Segoe MDL2 Assets")
    }
}

struct Fonts {
    dwrite: IDWriteFactory5,
    text: IDWriteTextFormat,
    icon: IDWriteTextFormat,
    small_icon: IDWriteTextFormat,
}

impl Fonts {
    fn new(dwrite: &IDWriteFactory5) -> Result<Fonts> {
        let locale = windows::core::HSTRING::from(crate::i18n::locale_name());
        unsafe {
            let make = |family: PCWSTR, size: f32, center: bool| -> Result<IDWriteTextFormat> {
                let f = dwrite.CreateTextFormat(
                    family,
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    &locale,
                )?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                if center {
                    f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                }
                Ok(f)
            };
            let icons = icon_family(dwrite);
            Ok(Fonts {
                dwrite: dwrite.clone(),
                text: make(w!("Segoe UI Variable Text"), TEXT_PT, false)?,
                icon: make(icons, 16.0, true)?,
                small_icon: make(icons, 12.0, true)?,
            })
        }
    }

    fn layout(
        &self,
        format: &IDWriteTextFormat,
        s: &str,
        w: f32,
        h: f32,
    ) -> Result<IDWriteTextLayout> {
        let text: Vec<u16> = s.encode_utf16().collect();
        unsafe { self.dwrite.CreateTextLayout(&text, format, w, h) }
    }

    fn width(&self, s: &str) -> f32 {
        self.layout(&self.text, s, 4096.0, ROW_H)
            .and_then(|l| unsafe {
                let mut m = DWRITE_TEXT_METRICS::default();
                l.GetMetrics(&mut m)?;
                Ok(m.widthIncludingTrailingWhitespace)
            })
            .unwrap_or(s.chars().count() as f32 * 7.5)
    }
}

// ── panels ────────────────────────────────────────────────────────────

struct Panel {
    hwnd: HWND,
    visual: WindowVisual,
    items: Vec<Item>,
    decor: Vec<Decor>,
    labels: Vec<Option<IDWriteTextLayout>>,
    rows: Vec<(f32, f32)>,
    any_icon: bool,
    /// Screen rectangle, pixels.
    rect: RECT,
    scale: f32,
    hover: Option<usize>,
    pressed: Option<usize>,
    /// Hover highlight per row, animated towards 0 or 1.
    glow: Vec<f32>,
    /// Row whose submenu is open.
    open: Option<usize>,
    /// Painted at least once and faded in.
    shown: bool,
}

impl Panel {
    fn width_dips(&self) -> f32 {
        (self.rect.right - self.rect.left) as f32 / self.scale
    }
}

fn trailing_width(item: &Item, decor: &Decor) -> f32 {
    match item {
        Item::Sub { .. } => CHEVRON_W,
        Item::Action { checked, .. } if decor.toggle => {
            let _ = checked;
            TOGGLE_W
        }
        Item::Action { checked: true, .. } => CHEVRON_W + 4.0,
        _ => 0.0,
    }
}

struct Menu<'a> {
    gpu: &'a Gpu,
    dc: &'a ID2D1DeviceContext,
    hinst: HINSTANCE,
    fonts: Fonts,
    theme: Theme,
    decor: &'a dyn Fn(u32) -> Decor,
    panels: Vec<Panel>,
    /// Hovered row waiting to open (or close) a submenu: (panel, row, since).
    pending: Option<(usize, usize, Instant)>,
    keyboard: bool,
    result: Option<Option<u32>>,
    last_frame: Instant,
}

impl Menu<'_> {
    fn open_panel(&mut self, items: Vec<Item>, at: Placement) -> Result<()> {
        let anchor_pt = match at {
            Placement::Root(Anchor::Above(p) | Anchor::Below(p)) => p,
            Placement::Beside { pt, .. } => pt,
        };
        let scale = dpi_scale(anchor_pt);
        let decor: Vec<Decor> = items
            .iter()
            .map(|it| match it {
                Item::Action { id, .. } => (self.decor)(*id),
                Item::Sub { .. } => (self.decor)(SUBMENU),
                Item::Separator => Decor::default(),
            })
            .collect();
        let any_icon = decor.iter().any(|d| d.icon.is_some())
            || items
                .iter()
                .zip(&decor)
                .any(|(it, d)| matches!(it, Item::Action { checked: true, .. }) && !d.toggle);
        let label_of = |it: &Item| match it {
            Item::Action { label, .. } | Item::Sub { label, .. } => Some(label.clone()),
            Item::Separator => None,
        };
        let widths: Vec<f32> = items
            .iter()
            .map(|it| label_of(it).map(|l| self.fonts.width(&l)).unwrap_or(0.0))
            .collect();
        let trailing: Vec<f32> = items
            .iter()
            .zip(&decor)
            .map(|(it, d)| trailing_width(it, d))
            .collect();
        let width = panel_width(&widths, any_icon, &trailing);
        let (rows, height) = layout_rows(&items);
        let lead = INSET + if any_icon { ICON_W + ICON_GAP } else { 0.0 };
        let labels = items
            .iter()
            .map(|it| {
                label_of(it).and_then(|l| {
                    self.fonts
                        .layout(&self.fonts.text, &l, width - lead - PAD * 2.0, ROW_H)
                        .ok()
                })
            })
            .collect();

        let size = SIZE {
            cx: (width * scale).round() as i32,
            cy: (height * scale).round() as i32,
        };
        let mut rect = RECT::default();
        unsafe {
            match at {
                Placement::Root(a) => {
                    let (pt, v) = match a {
                        Anchor::Above(p) => (p, TPM_BOTTOMALIGN),
                        Anchor::Below(p) => (p, TPM_TOPALIGN),
                    };
                    CalculatePopupWindowPosition(
                        &pt,
                        &size,
                        (TPM_LEFTALIGN | v | TPM_WORKAREA).0,
                        None,
                        &mut rect,
                    )?;
                }
                Placement::Beside { pt, exclude } => {
                    CalculatePopupWindowPosition(
                        &pt,
                        &size,
                        (TPM_LEFTALIGN | TPM_TOPALIGN | TPM_WORKAREA).0,
                        Some(&exclude),
                        &mut rect,
                    )?;
                }
            }
        }

        let root = self.panels.is_empty();
        let mut ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP;
        if !root {
            ex |= WS_EX_NOACTIVATE;
        }
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                CLASS,
                w!("Vitrine menu"),
                WS_POPUP,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                None,
                None,
                Some(self.hinst),
                None,
            )?
        };
        unsafe {
            let set = |attr: DWMWINDOWATTRIBUTE, v: i32| {
                let _ = DwmSetWindowAttribute(hwnd, attr, &v as *const i32 as *const _, 4);
            };
            set(DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND.0);
            set(DWMWA_USE_IMMERSIVE_DARK_MODE, self.theme.dark as i32);
            set(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW.0);
            let m = MARGINS {
                cxLeftWidth: -1,
                cxRightWidth: -1,
                cyTopHeight: -1,
                cyBottomHeight: -1,
            };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        }
        let mut visual = WindowVisual::new(self.gpu, hwnd)?;
        visual.set_opacity(self.gpu, 0.0)?;
        let n = items.len();
        self.panels.push(Panel {
            hwnd,
            visual,
            items,
            decor,
            labels,
            rows,
            any_icon,
            rect,
            scale,
            hover: None,
            pressed: None,
            glow: vec![0.0; n],
            open: None,
            shown: false,
        });
        let i = self.panels.len() - 1;
        self.paint(i);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            if root {
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(hwnd));
            }
        }
        Ok(())
    }

    fn close_from(&mut self, level: usize) {
        while self.panels.len() > level {
            let mut p = self.panels.pop().unwrap();
            let _ = p.visual.release();
            unsafe {
                let _ = DestroyWindow(p.hwnd);
            }
        }
        if let Some(parent) = self.panels.last_mut() {
            parent.open = None;
        }
    }

    fn open_sub(&mut self, level: usize, row: usize) {
        self.close_from(level + 1);
        let p = &self.panels[level];
        let Item::Sub { items, .. } = &p.items[row] else {
            return;
        };
        let items = items.clone();
        let (top, _) = p.rows[row];
        let pt = POINT {
            x: p.rect.right - (2.0 * p.scale) as i32,
            y: p.rect.top + ((top - PAD) * p.scale) as i32,
        };
        let exclude = p.rect;
        self.panels[level].open = Some(row);
        if let Err(e) = self.open_panel(items, Placement::Beside { pt, exclude }) {
            log::warn!("submenu: {e}");
        }
    }

    fn level_of(&self, hwnd: HWND) -> Option<usize> {
        self.panels.iter().position(|p| p.hwnd == hwnd)
    }

    fn activate(&mut self, level: usize, row: usize) {
        match &self.panels[level].items[row] {
            Item::Action {
                id, enabled: true, ..
            } => self.result = Some(Some(*id)),
            Item::Sub { .. } => {
                self.open_sub(level, row);
                if let Some(sub) = self.panels.get_mut(level + 1) {
                    sub.hover = step(&sub.items, None, 1);
                }
            }
            _ => {}
        }
    }

    fn handle(&mut self, ev: Ev) {
        match ev {
            Ev::Move(hwnd, x, y) | Ev::Down(hwnd, x, y) | Ev::Up(hwnd, x, y) => {
                let Some(level) = self.level_of(hwnd) else {
                    return;
                };
                let p = &mut self.panels[level];
                let hit = row_at(&p.items, &p.rows, y).filter(|_| x >= 0.0 && x < p.width_dips());
                if let Ev::Move(..) = ev {
                    self.keyboard = false;
                    if hit != p.hover {
                        p.hover = hit;
                        p.pressed = None;
                        if let Some(r) = hit {
                            self.pending = Some((level, r, Instant::now()));
                        }
                    }
                } else if let Ev::Down(..) = ev {
                    p.pressed = hit;
                } else {
                    let was = p.pressed.take();
                    if let Some(r) = hit
                        && (was == Some(r) || was.is_none())
                    {
                        self.activate(level, r);
                    }
                }
            }
            Ev::Leave(hwnd) => {
                if let Some(level) = self.level_of(hwnd) {
                    let p = &mut self.panels[level];
                    // Keep the row that owns an open submenu highlighted.
                    p.hover = p.open;
                    p.pressed = None;
                }
            }
            Ev::Key(vk) => {
                let level = self.panels.len() - 1;
                self.keyboard = true;
                let p = &mut self.panels[level];
                match VIRTUAL_KEY(vk) {
                    VK_DOWN => p.hover = step(&p.items, p.hover, 1),
                    VK_UP => p.hover = step(&p.items, p.hover, -1),
                    VK_HOME => p.hover = step(&p.items, None, 1),
                    VK_END => p.hover = step(&p.items, None, -1),
                    VK_RIGHT => {
                        if let Some(r) = p.hover
                            && matches!(p.items[r], Item::Sub { .. })
                        {
                            self.activate(level, r);
                        }
                    }
                    VK_RETURN | VK_SPACE => {
                        if let Some(r) = p.hover {
                            self.activate(level, r);
                        }
                    }
                    VK_LEFT if level > 0 => self.close_from(level),
                    VK_ESCAPE if level > 0 => self.close_from(level),
                    VK_ESCAPE | VK_MENU | VK_F10 => self.result = Some(None),
                    _ => {}
                }
            }
            Ev::Deactivate(to) => {
                if self.level_of(to).is_none() {
                    self.result = Some(None);
                }
            }
        }
    }

    /// Opens or closes submenus once the pointer rests on a row.
    fn settle_hover(&mut self) {
        let Some((level, row, since)) = self.pending else {
            return;
        };
        if since.elapsed() < SUBMENU_DELAY || level >= self.panels.len() {
            return;
        }
        self.pending = None;
        if self.panels[level].hover != Some(row) {
            return;
        }
        let is_sub = matches!(self.panels[level].items[row], Item::Sub { .. });
        if is_sub {
            if self.panels[level].open != Some(row) {
                self.open_sub(level, row);
            }
        } else {
            self.close_from(level + 1);
        }
    }

    /// Advances hover animations; true while something is still moving.
    fn animate(&mut self) -> bool {
        let dt = self.last_frame.elapsed().as_secs_f32().min(0.05);
        self.last_frame = Instant::now();
        let mut moving = false;
        for i in 0..self.panels.len() {
            let p = &mut self.panels[i];
            let mut changed = false;
            for (r, g) in p.glow.iter_mut().enumerate() {
                let target = if p.hover == Some(r) || p.open == Some(r) {
                    1.0
                } else {
                    0.0
                };
                if (*g - target).abs() > 0.001 {
                    // ~90 ms in, a little slower out, like WinUI.
                    let speed = if target > *g { 11.0 } else { 7.0 };
                    *g += (target - *g) * (1.0 - (-speed * dt).exp());
                    if (*g - target).abs() < 0.02 {
                        *g = target;
                    }
                    changed = true;
                }
            }
            if changed {
                moving = true;
                self.paint(i);
            }
        }
        moving
    }

    fn paint(&mut self, i: usize) {
        let theme = self.theme;
        let fonts = &self.fonts;
        let p = &mut self.panels[i];
        let (w, h) = (
            (p.rect.right - p.rect.left) as u32,
            (p.rect.bottom - p.rect.top) as u32,
        );
        let scale = p.scale;
        let keyboard = self.keyboard;
        let (items, decor, labels, rows, glow, any_icon, pressed, hover) = (
            &p.items, &p.decor, &p.labels, &p.rows, &p.glow, p.any_icon, p.pressed, p.hover,
        );
        let width = w as f32 / scale;
        let result = p.visual.draw(self.gpu, self.dc, w, h, |dc, origin| unsafe {
            let [_, _, _, _, ox, oy] = origin.0;
            dc.SetTransform(&Matrix3x2 {
                M11: scale,
                M12: 0.0,
                M21: 0.0,
                M22: scale,
                M31: ox,
                M32: oy,
            });
            let brush = dc.CreateSolidColorBrush(&theme.fill(), None)?;
            dc.FillRectangle(
                &D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: width,
                    bottom: h as f32 / scale,
                },
                &brush,
            );
            for (r, item) in items.iter().enumerate() {
                let (top, rh) = rows[r];
                if let Item::Separator = item {
                    brush.SetColor(&theme.ink(if theme.dark { 0.0837 } else { 0.0803 }));
                    let y = top + rh / 2.0;
                    dc.FillRectangle(
                        &D2D_RECT_F {
                            left: 0.0,
                            top: y - 0.5,
                            right: width,
                            bottom: y + 0.5,
                        },
                        &brush,
                    );
                    continue;
                }
                let enabled = selectable(item);
                let row = D2D_RECT_F {
                    left: PAD,
                    top: top + 1.0,
                    right: width - PAD,
                    bottom: top + rh - 1.0,
                };
                let g = glow[r];
                if g > 0.0 {
                    let a = theme.hover() * g * if pressed == Some(r) { 0.7 } else { 1.0 };
                    brush.SetColor(&theme.ink(a));
                    dc.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: row,
                            radiusX: 4.0,
                            radiusY: 4.0,
                        },
                        &brush,
                    );
                    // Keyboard focus also gets the WinUI accent pill.
                    if keyboard && hover == Some(r) {
                        brush.SetColor(&theme.accent);
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: PAD,
                                    top: top + rh / 2.0 - 8.0,
                                    right: PAD + 3.0,
                                    bottom: top + rh / 2.0 + 8.0,
                                },
                                radiusX: 1.5,
                                radiusY: 1.5,
                            },
                            &brush,
                        );
                    }
                }
                let ink = theme.ink(if enabled { 1.0 } else { 0.36 });
                let d = decor[r];
                let checked = matches!(item, Item::Action { checked: true, .. });
                let mut x = PAD + INSET;
                if any_icon {
                    let glyph = d.icon.or((checked && !d.toggle).then_some(GLYPH_CHECK));
                    if let Some(c) = glyph {
                        let tint = if checked && !d.toggle {
                            theme.accent
                        } else {
                            ink
                        };
                        brush.SetColor(&tint);
                        if let Ok(l) = fonts.layout(&fonts.icon, &c.to_string(), ICON_W + 8.0, rh) {
                            dc.DrawTextLayout(
                                Vector2 { X: x - 4.0, Y: top },
                                &l,
                                &brush,
                                D2D1_DRAW_TEXT_OPTIONS_NONE,
                            );
                        }
                    }
                    x += ICON_W + ICON_GAP;
                }
                if let Some(Some(l)) = labels.get(r) {
                    brush.SetColor(&ink);
                    dc.DrawTextLayout(
                        Vector2 { X: x, Y: top },
                        l,
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
                let right = width - PAD - INSET;
                match item {
                    Item::Sub { .. } => {
                        brush.SetColor(&theme.ink(0.8));
                        if let Ok(l) = fonts.layout(
                            &fonts.small_icon,
                            &GLYPH_CHEVRON.to_string(),
                            CHEVRON_W + 8.0,
                            rh,
                        ) {
                            dc.DrawTextLayout(
                                Vector2 {
                                    X: right - CHEVRON_W - 4.0,
                                    Y: top,
                                },
                                &l,
                                &brush,
                                D2D1_DRAW_TEXT_OPTIONS_NONE,
                            );
                        }
                    }
                    Item::Action { checked, .. } if d.toggle => {
                        let cy = top + rh / 2.0;
                        let track = D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: right - TOGGLE_W,
                                top: cy - TOGGLE_H / 2.0,
                                right,
                                bottom: cy + TOGGLE_H / 2.0,
                            },
                            radiusX: TOGGLE_H / 2.0,
                            radiusY: TOGGLE_H / 2.0,
                        };
                        let knob_r = 6.0 + g * 1.0;
                        let (kx, knob) = if *checked {
                            brush.SetColor(&theme.accent);
                            dc.FillRoundedRectangle(&track, &brush);
                            let k = if theme.dark {
                                rgba(0.0, 0.0, 0.0, 0.9)
                            } else {
                                rgba(1.0, 1.0, 1.0, 1.0)
                            };
                            (right - TOGGLE_H / 2.0, k)
                        } else {
                            brush.SetColor(&theme.ink(0.6));
                            dc.DrawRoundedRectangle(
                                &D2D1_ROUNDED_RECT {
                                    rect: D2D_RECT_F {
                                        left: track.rect.left + 0.5,
                                        top: track.rect.top + 0.5,
                                        right: track.rect.right - 0.5,
                                        bottom: track.rect.bottom - 0.5,
                                    },
                                    radiusX: TOGGLE_H / 2.0 - 0.5,
                                    radiusY: TOGGLE_H / 2.0 - 0.5,
                                },
                                &brush,
                                1.0,
                                None,
                            );
                            (right - TOGGLE_W + TOGGLE_H / 2.0, theme.ink(0.8))
                        };
                        brush.SetColor(&knob);
                        dc.FillEllipse(
                            &D2D1_ELLIPSE {
                                point: Vector2 { X: kx, Y: cy },
                                radiusX: knob_r,
                                radiusY: knob_r,
                            },
                            &brush,
                        );
                    }
                    _ => {}
                }
            }
            Ok(())
        });
        if let Err(e) = result {
            log::warn!("menu paint: {e}");
        }
        if !p.shown {
            p.shown = true;
            let _ = p.visual.animate_opacity(self.gpu, 0.0, 1.0, 0.12);
        }
    }

    fn cursor_inside(&self) -> bool {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        self.panels.iter().any(|p| {
            pt.x >= p.rect.left && pt.x < p.rect.right && pt.y >= p.rect.top && pt.y < p.rect.bottom
        })
    }
}

#[derive(Clone, Copy)]
enum Placement {
    Root(Anchor),
    Beside { pt: POINT, exclude: RECT },
}

fn buttons_down() -> bool {
    unsafe {
        [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]
            .iter()
            .any(|k| GetAsyncKeyState(k.0 as i32) as u16 & 0x8000 != 0)
    }
}

/// Pumps messages until the mouse buttons are up (and a little after), so a
/// click that closed the menu is fully delivered. Returns a WM_QUIT exit code.
fn drain_click() -> Option<i32> {
    let start = Instant::now();
    let mut released: Option<Instant> = None;
    loop {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return Some(msg.wParam.0 as i32);
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if released.is_none() && !buttons_down() {
            released = Some(Instant::now());
        }
        let settled = released.is_some_and(|t| t.elapsed() >= Duration::from_millis(120));
        if settled || start.elapsed() >= Duration::from_secs(3) {
            return None;
        }
        unsafe {
            MsgWaitForMultipleObjects(None, false, 15, QS_ALLINPUT);
        }
    }
}

/// Shows a menu and returns the chosen command id. Runs a modal loop until the
/// user picks an item, presses Esc or clicks elsewhere.
pub fn show(
    gpu: &Gpu,
    dc: &ID2D1DeviceContext,
    hinst: HINSTANCE,
    anchor: Anchor,
    items: &[Item],
    decor: &dyn Fn(u32) -> Decor,
) -> Option<u32> {
    if let Err(e) = vitrine_desktop::window::register_class(hinst, CLASS, proc) {
        log::warn!("menu class: {e}");
        return None;
    }
    let fonts = match Fonts::new(&gpu.dwrite) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("menu fonts: {e}");
            return None;
        }
    };
    let mut menu = Menu {
        gpu,
        dc,
        hinst,
        fonts,
        theme: Theme::current(),
        decor,
        panels: Vec::new(),
        pending: None,
        keyboard: false,
        result: None,
        last_frame: Instant::now(),
    };
    if let Err(e) = menu.open_panel(items.to_vec(), Placement::Root(anchor)) {
        log::warn!("menu: {e}");
        return None;
    }
    EVENTS.with(|e| e.borrow_mut().clear());
    // A click outside closes the menu, once the button that opened it is up.
    let mut armed = !buttons_down();
    let mut quit = None;
    let mut outside = false;
    while menu.result.is_none() {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    quit = Some(msg.wParam.0 as i32);
                    menu.result = Some(None);
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        for ev in EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut())) {
            menu.handle(ev);
            if menu.result.is_some() {
                break;
            }
        }
        if menu.result.is_some() {
            break;
        }
        let down = buttons_down();
        if !down {
            armed = true;
        } else if armed && !menu.cursor_inside() {
            menu.result = Some(None);
            outside = true;
            break;
        }
        menu.settle_hover();
        let moving = menu.animate();
        let wait = if moving || menu.pending.is_some() {
            8
        } else {
            40
        };
        unsafe {
            MsgWaitForMultipleObjects(None, false, wait, QS_ALLINPUT);
        }
    }
    menu.close_from(0);
    if outside && quit.is_none() {
        // Let the click that closed the menu finish here, so its release (and
        // the shell's notification when it landed on the tray icon) arrives
        // while the menu is still "open" instead of reopening it.
        quit = drain_click();
    }
    if let Some(code) = quit {
        unsafe { PostQuitMessage(code) };
    }
    menu.result.flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<Item> {
        vec![
            Item::action(1, "One"),
            Item::Separator,
            Item::disabled("Nothing"),
            Item::Sub {
                label: "More".into(),
                items: vec![Item::action(2, "Two")],
            },
            Item::check(3, "Three", true),
        ]
    }

    #[test]
    fn rows_stack_with_padding() {
        let (rows, h) = layout_rows(&items());
        assert_eq!(rows[0], (PAD, ROW_H));
        assert_eq!(rows[1], (PAD + ROW_H, SEP_H));
        assert_eq!(rows[2].0, PAD + ROW_H + SEP_H);
        assert_eq!(h, PAD * 2.0 + ROW_H * 4.0 + SEP_H);
    }

    #[test]
    fn hit_testing_skips_separators_and_disabled_rows() {
        let it = items();
        let (rows, _) = layout_rows(&it);
        assert_eq!(row_at(&it, &rows, PAD + 1.0), Some(0));
        assert_eq!(row_at(&it, &rows, rows[1].0 + 2.0), None);
        assert_eq!(row_at(&it, &rows, rows[2].0 + 2.0), None);
        assert_eq!(row_at(&it, &rows, rows[3].0 + 2.0), Some(3));
        assert_eq!(row_at(&it, &rows, 0.0), None);
        assert_eq!(row_at(&it, &rows, 10_000.0), None);
    }

    #[test]
    fn keyboard_steps_wrap_over_selectable_rows() {
        let it = items();
        assert_eq!(step(&it, None, 1), Some(0));
        assert_eq!(step(&it, None, -1), Some(4));
        assert_eq!(step(&it, Some(0), 1), Some(3));
        assert_eq!(step(&it, Some(4), 1), Some(0));
        assert_eq!(step(&it, Some(0), -1), Some(4));
        assert_eq!(step(&[Item::Separator], None, 1), None);
        assert_eq!(step(&[], None, 1), None);
    }

    #[test]
    fn width_fits_the_widest_row() {
        assert_eq!(panel_width(&[10.0], false, &[0.0]), MIN_W);
        let w = panel_width(&[300.0, 100.0], true, &[0.0, TOGGLE_W]);
        assert_eq!(
            w,
            (INSET + ICON_W + ICON_GAP + 300.0 + INSET + 2.0 * PAD).ceil()
        );
        let w = panel_width(&[250.0], false, &[TOGGLE_W]);
        assert_eq!(
            w,
            (INSET + 250.0 + TRAIL_GAP + TOGGLE_W + INSET + 2.0 * PAD).ceil()
        );
    }

    #[test]
    fn hex_colours_parse() {
        let c = parse_hex("#0078d4");
        assert!((c.r - 0.0).abs() < 1e-6 && (c.g - 120.0 / 255.0).abs() < 1e-6);
        assert!((parse_hex("nonsense").b - 212.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn theme_reads_the_system_setting() {
        let t = Theme::current();
        assert_eq!(t.dark, vitrine_script::appearance::is_dark());
        assert!(t.accent.a == 1.0);
    }
}
