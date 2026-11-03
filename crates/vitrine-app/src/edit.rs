//! Edit mode: a translucent full-screen overlay per monitor showing the grid,
//! the desktop icons and every widget's frame. Widgets are dragged to move,
//! dragged by the corner handle to resize, and right-clicked to remove.
//! The real widget windows (beneath the icons) follow live.

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::grid::Geometry;
use crate::runtime::HostMsg;
use vitrine_render::compose::WindowVisual;
use vitrine_render::gpu::Gpu;

const CLASS: PCWSTR = w!("VitrineEditOverlay");

/// A widget as edit mode sees it.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub monitor: String,
    pub rect: (i32, i32, i32, i32),
    pub min: (i32, i32),
    pub max: (i32, i32),
}

#[derive(Clone, Copy, Debug)]
pub enum Input {
    Down(POINT),
    Move(POINT),
    Up,
    RightUp(POINT),
    Escape,
}

#[derive(Clone, Debug)]
pub struct Drag {
    pub id: String,
    pub resize: bool,
    pub start: POINT,
    pub from: (i32, i32, i32, i32),
    /// Monitor of the widget when the drag started (`from` is relative to it).
    pub monitor: String,
    pub moved: bool,
}

struct Overlay {
    hwnd: HWND,
    visual: WindowVisual,
    rect: RECT,
    /// `"primary"` or the device name.
    monitor: String,
    geo: Geometry,
}

pub struct EditMode {
    overlays: Vec<Overlay>,
    icons: Vec<RECT>,
    pub drag: Option<Drag>,
    pub hover: Option<String>,
    dirty: bool,
    label: IDWriteTextFormat,
    title: IDWriteTextFormat,
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

/// Uniform scale by `s` around the point (`x`, `y`).
fn scale_at(s: f32, x: f32, y: f32) -> Matrix3x2 {
    Matrix3x2 {
        M11: s,
        M12: 0.0,
        M21: 0.0,
        M22: s,
        M31: x - s * x,
        M32: y - s * y,
    }
}

fn contains(r: &RECT, pt: POINT) -> bool {
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

const ACCENT: u32 = 0x60CDFF; // Windows 11 default dark-theme accent

impl EditMode {
    pub fn open(
        gpu: &Gpu,
        hinst: HINSTANCE,
        monitors: Vec<(String, Geometry, RECT)>,
        icons: Vec<RECT>,
    ) -> Result<EditMode> {
        let font = |size: f32, weight: DWRITE_FONT_WEIGHT| unsafe {
            gpu.dwrite.CreateTextFormat(
                w!("Segoe UI Variable Text"),
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!(""),
            )
        };
        let label = font(13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let title = font(14.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        unsafe {
            title.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            title.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            label.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        }
        let mut overlays = Vec::new();
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
            let visual = WindowVisual::new(gpu, hwnd)?;
            overlays.push(Overlay {
                hwnd,
                visual,
                rect,
                monitor,
                geo,
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
            icons,
            drag: None,
            hover: None,
            dirty: true,
            label,
            title,
        })
    }

    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn overlay_index(&self, monitor: &str) -> usize {
        self.overlays
            .iter()
            .position(|o| o.monitor.eq_ignore_ascii_case(monitor))
            .unwrap_or(0)
    }

    /// Geometry of the monitor a layout entry lives on.
    pub fn geometry(&self, monitor: &str) -> Option<Geometry> {
        self.overlays
            .get(self.overlay_index(monitor))
            .map(|o| o.geo)
    }

    /// Monitor name and geometry under a screen point.
    pub fn monitor_at(&self, pt: POINT) -> Option<(String, Geometry)> {
        self.overlays
            .iter()
            .find(|o| contains(&o.rect, pt))
            .map(|o| (o.monitor.clone(), o.geo))
    }

    /// The "Done" button, screen pixels (top centre of the first overlay).
    pub fn done_button(&self) -> RECT {
        let Some(o) = self.overlays.first() else {
            return RECT::default();
        };
        let s = o.geo.scale;
        let cx = (o.rect.left + o.rect.right) / 2;
        let top = o.rect.top + (20.0 * s) as i32;
        let half = (56.0 * s) as i32;
        RECT {
            left: cx - half,
            top,
            right: cx + half,
            bottom: top + (36.0 * s) as i32,
        }
    }

    /// Widget frame under a point and whether the point is on its resize handle.
    pub fn hit(&self, items: &[Item], pt: POINT) -> Option<(String, bool)> {
        for item in items.iter().rev() {
            let Some(g) = self.geometry(&item.monitor) else {
                continue;
            };
            let (x, y, w, h) = item.rect;
            let r = g.rect(x, y, w, h);
            if contains(&r, pt) {
                let handle = (24.0 * g.scale) as i32;
                let on_handle = pt.x >= r.right - handle && pt.y >= r.bottom - handle;
                return Some((item.id.clone(), on_handle));
            }
        }
        None
    }

    /// Repaints the overlays if needed. Returns the loop timeout it needs.
    pub fn paint(&mut self, gpu: &Gpu, dc: &ID2D1DeviceContext, items: &[Item]) -> u32 {
        if !self.dirty {
            return u32::MAX;
        }
        self.dirty = false;
        let done = self.done_button();
        for i in 0..self.overlays.len() {
            let (w, h) = {
                let o = &self.overlays[i];
                (
                    (o.rect.right - o.rect.left) as u32,
                    (o.rect.bottom - o.rect.top) as u32,
                )
            };
            let o = &self.overlays[i];
            let (rect, geo, monitor) = (o.rect, o.geo, o.monitor.clone());
            let (icons, drag, hover, label, title) = (
                &self.icons,
                &self.drag,
                &self.hover,
                &self.label,
                &self.title,
            );
            let overlay = &mut self.overlays[i];
            let result = overlay.visual.draw(gpu, dc, w, h, |dc, origin| unsafe {
                let [_, _, _, _, ox, oy] = origin.0;
                let local = |r: RECT| D2D_RECT_F {
                    left: (r.left - rect.left) as f32 + ox,
                    top: (r.top - rect.top) as f32 + oy,
                    right: (r.right - rect.left) as f32 + ox,
                    bottom: (r.bottom - rect.top) as f32 + oy,
                };
                dc.SetTransform(&Matrix3x2::identity());
                let brush = dc.CreateSolidColorBrush(&color(0x000000, 0.42), None)?;
                dc.FillRectangle(&D2D_RECT_F { left: ox, top: oy, right: ox + w as f32, bottom: oy + h as f32 }, &brush);

                let s = geo.scale;
                let rr = |r: D2D_RECT_F, radius: f32| D2D1_ROUNDED_RECT { rect: r, radiusX: radius * s, radiusY: radius * s };
                // Grid cells.
                brush.SetColor(&color(0xFFFFFF, 0.07));
                for y in 0..geo.rows {
                    for x in 0..geo.cols {
                        dc.FillRoundedRectangle(&rr(local(geo.rect(x, y, 1, 1)), 6.0), &brush);
                    }
                }
                // Desktop icons, for orientation.
                brush.SetColor(&color(0xFFFFFF, 0.28));
                for r in icons.iter().filter(|r| r.left >= rect.left && r.left < rect.right && r.top >= rect.top && r.top < rect.bottom) {
                    dc.DrawRoundedRectangle(&rr(local(*r), 4.0), &brush, 1.0 * s, None);
                }
                // Widget frames.
                for item in items.iter().filter(|it| it.monitor.eq_ignore_ascii_case(&monitor)) {
                    let (x, y, iw, ih) = item.rect;
                    let r = local(geo.rect(x, y, iw, ih));
                    let active = drag.as_ref().is_some_and(|d| d.id == item.id);
                    let hovered = hover.as_deref() == Some(item.id.as_str());
                    brush.SetColor(&color(ACCENT, if active { 0.26 } else if hovered { 0.18 } else { 0.10 }));
                    dc.FillRoundedRectangle(&rr(r, 8.0), &brush);
                    brush.SetColor(&color(ACCENT, if active || hovered { 1.0 } else { 0.75 }));
                    dc.DrawRoundedRectangle(&rr(r, 8.0), &brush, 2.0 * s, None);
                    // Name.
                    let text: Vec<u16> = item.name.encode_utf16().collect();
                    brush.SetColor(&color(0xFFFFFF, 0.95));
                    dc.SetTransform(&scale_at(s, r.left + 10.0 * s, r.top + 8.0 * s));
                    dc.DrawText(
                        &text,
                        label,
                        &D2D_RECT_F {
                            left: r.left + 10.0 * s,
                            top: r.top + 8.0 * s,
                            right: r.left + 10.0 * s + ((r.right - r.left) / s - 20.0).max(0.0),
                            bottom: r.top + 8.0 * s + 20.0,
                        },
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                    dc.SetTransform(&Matrix3x2::identity());
                    // Resize handle.
                    brush.SetColor(&color(ACCENT, 1.0));
                    let c = Vector2 { X: r.right - 9.0 * s, Y: r.bottom - 9.0 * s };
                    dc.FillEllipse(&D2D1_ELLIPSE { point: c, radiusX: 5.0 * s, radiusY: 5.0 * s }, &brush);
                }
                // Done button and hint on the first monitor.
                if i == 0 {
                    let b = local(done);
                    brush.SetColor(&color(ACCENT, 1.0));
                    dc.FillRoundedRectangle(&rr(b, 18.0), &brush);
                    brush.SetColor(&color(0x000000, 0.9));
                    let text: Vec<u16> = "Done".encode_utf16().collect();
                    let cx = (b.left + b.right) / 2.0;
                    let cy = (b.top + b.bottom) / 2.0;
                    dc.SetTransform(&scale_at(s, cx, cy));
                    dc.DrawText(&text, title, &D2D_RECT_F { left: cx - 60.0, top: cy - 18.0, right: cx + 60.0, bottom: cy + 18.0 }, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                    brush.SetColor(&color(0xFFFFFF, 0.8));
                    let hint: Vec<u16> = "Drag to move · drag the dot to resize · right-click to remove · Esc to finish"
                        .encode_utf16()
                        .collect();
                    let hy = b.bottom + 22.0 * s;
                    dc.SetTransform(&scale_at(s, cx, hy));
                    dc.DrawText(&hint, title, &D2D_RECT_F { left: cx - 400.0, top: hy - 12.0, right: cx + 400.0, bottom: hy + 12.0 }, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                    dc.SetTransform(&Matrix3x2::identity());
                }
                Ok(())
            });
            if let Err(e) = result {
                log::error!("edit overlay: {e}");
            }
        }
        u32::MAX
    }
}

impl Drop for EditMode {
    fn drop(&mut self) {
        for o in self.overlays.drain(..) {
            let mut visual = o.visual;
            let _ = visual.release();
            drop(visual);
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
            push(Input::Up);
        }
        WM_RBUTTONUP => push(Input::RightUp(screen_point(hwnd, lparam))),
        WM_KEYDOWN if wparam.0 as u16 == VK_ESCAPE.0 => push(Input::Escape),
        WM_MOUSEACTIVATE => return LRESULT(MA_ACTIVATE as isize),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}
