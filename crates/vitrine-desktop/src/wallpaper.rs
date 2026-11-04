//! The desktop wallpaper of each monitor (`IDesktopWallpaper`), for widgets
//! that draw frosted glass over it.

use std::path::PathBuf;
use std::time::SystemTime;

use windows::Win32::Foundation::RECT;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::{
    DWPOS_CENTER, DWPOS_FIT, DWPOS_SPAN, DWPOS_STRETCH, DWPOS_TILE, DesktopWallpaper,
    IDesktopWallpaper,
};
use windows::core::{PCWSTR, PWSTR};

use crate::monitors::Monitor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Center,
    Tile,
    Stretch,
    Fit,
    Fill,
    Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Wallpaper {
    /// [`Monitor::device`] of the monitor showing it.
    pub device: String,
    /// The image; `None` for a solid colour (or an unreadable image).
    pub path: Option<PathBuf>,
    /// Modification time of the image, to notice files rewritten in place.
    pub stamp: Option<SystemTime>,
    pub position: Position,
    /// Background colour, `0xRRGGBB`.
    pub color: u32,
}

/// Explorer's copy of the current wallpaper (also used by Spotlight and
/// slideshows), for when the original file is gone or unreadable.
fn transcoded() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("APPDATA")?)
        .join(r"Microsoft\Windows\Themes\TranscodedWallpaper");
    p.is_file().then_some(p)
}

fn overlap(a: &RECT, b: &RECT) -> i64 {
    let w = (a.right.min(b.right) - a.left.max(b.left)).max(0) as i64;
    let h = (a.bottom.min(b.bottom) - a.top.max(b.top)).max(0) as i64;
    w * h
}

unsafe fn take(p: PWSTR) -> String {
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

fn color_of(colorref: u32) -> u32 {
    // COLORREF is 0x00BBGGRR.
    ((colorref & 0xFF) << 16) | (colorref & 0xFF00) | ((colorref >> 16) & 0xFF)
}

/// The wallpaper of every monitor in `monitors`. Needs COM on the calling
/// thread. Never fails: unknown monitors get Explorer's transcoded copy.
pub fn read(monitors: &[Monitor]) -> Vec<Wallpaper> {
    let found = unsafe { query() }.unwrap_or_else(|e| {
        log::warn!("wallpaper: {e}");
        Vec::new()
    });
    let fallback_position = found.first().map(|f| f.2).unwrap_or(Position::Fill);
    let fallback_color = found.first().map(|f| f.3).unwrap_or(0);
    monitors
        .iter()
        .map(|m| {
            // Match by area: DPI virtualization may shift rectangles slightly.
            let best = found
                .iter()
                .max_by_key(|(r, ..)| overlap(r, &m.rect))
                .filter(|(r, ..)| overlap(r, &m.rect) > 0);
            let (path, position, color) = match best {
                Some((_, p, pos, c)) => (p.clone(), *pos, *c),
                None => (transcoded(), fallback_position, fallback_color),
            };
            let stamp = path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok()?.modified().ok());
            Wallpaper {
                device: m.device.clone(),
                path,
                stamp,
                position,
                color,
            }
        })
        .collect()
}

/// The image shown on the primary monitor, if any.
pub fn primary_image(monitors: &[Monitor]) -> Option<PathBuf> {
    let primary = monitors.iter().find(|m| m.primary).or(monitors.first())?;
    read(std::slice::from_ref(primary)).pop()?.path
}

type Found = (RECT, Option<PathBuf>, Position, u32);

unsafe fn query() -> windows::core::Result<Vec<Found>> {
    unsafe {
        let dw: IDesktopWallpaper = CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL)?;
        let position = match dw.GetPosition() {
            Ok(DWPOS_CENTER) => Position::Center,
            Ok(DWPOS_TILE) => Position::Tile,
            Ok(DWPOS_STRETCH) => Position::Stretch,
            Ok(DWPOS_FIT) => Position::Fit,
            Ok(DWPOS_SPAN) => Position::Span,
            _ => Position::Fill,
        };
        let color = dw.GetBackgroundColor().map(|c| color_of(c.0)).unwrap_or(0);
        let mut out = Vec::new();
        for i in 0..dw.GetMonitorDevicePathCount()? {
            let Ok(id) = dw.GetMonitorDevicePathAt(i) else {
                continue;
            };
            let rect = dw.GetMonitorRECT(PCWSTR(id.0));
            let image = dw.GetWallpaper(PCWSTR(id.0)).map(|p| take(p));
            CoTaskMemFree(Some(id.0 as *const _));
            let Ok(rect) = rect else {
                continue; // not attached
            };
            let path = match image {
                // An empty path means a solid colour.
                Ok(s) if s.is_empty() => None,
                Ok(s) => {
                    let p = PathBuf::from(s);
                    if p.is_file() { Some(p) } else { transcoded() }
                }
                Err(_) => transcoded(),
            };
            out.push((rect, path, position, color));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorref_to_rgb() {
        assert_eq!(color_of(0x00332211), 0x112233);
    }

    #[test]
    fn overlap_area() {
        let r = |l, t, r, b| RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        };
        assert_eq!(overlap(&r(0, 0, 10, 10), &r(5, 5, 20, 20)), 25);
        assert_eq!(overlap(&r(0, 0, 10, 10), &r(10, 0, 20, 10)), 0);
    }
}
