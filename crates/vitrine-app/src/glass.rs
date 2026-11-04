//! Wallpaper sources for frosted glass (`backdropFilter`), live and headless.

use std::path::{Path, PathBuf};

use vitrine_desktop::monitors::Monitor;
use vitrine_desktop::wallpaper;
use vitrine_render::backdrop::{MonitorSource, Position, WallpaperImage, default_backdrop};
use vitrine_render::color::Color;
use vitrine_render::layout::RectF;
use windows::Win32::Foundation::RECT;

fn rect_f(r: &RECT) -> RectF {
    RectF {
        x: r.left as f32,
        y: r.top as f32,
        w: (r.right - r.left) as f32,
        h: (r.bottom - r.top) as f32,
    }
}

/// Bounding box of all monitors (the canvas of a spanned wallpaper).
pub fn desktop_rect(monitors: &[Monitor]) -> RectF {
    let mut it = monitors.iter().map(|m| m.rect);
    let Some(first) = it.next() else {
        return RectF::default();
    };
    let r = it.fold(first, |a, b| RECT {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    });
    rect_f(&r)
}

/// The monitor under the centre of `r` (or the nearest one), so a widget
/// dragged across monitors picks up the wallpaper it is over.
pub fn monitor_at<'a>(monitors: &'a [Monitor], r: &RECT) -> Option<&'a Monitor> {
    let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
    let dist = |m: &Monitor| {
        let dx = (m.rect.left - cx).max(cx - m.rect.right + 1).max(0) as i64;
        let dy = (m.rect.top - cy).max(cy - m.rect.bottom + 1).max(0) as i64;
        dx * dx + dy * dy
    };
    monitors.iter().min_by_key(|m| dist(m))
}

fn color(rgb: u32) -> Color {
    let c = |s: u32| ((rgb >> s) & 0xFF) as f32 / 255.0;
    Color::rgba(c(16), c(8), c(0), 1.0)
}

/// What every monitor shows behind the widgets. Needs COM.
pub fn sources(monitors: &[Monitor]) -> Vec<MonitorSource> {
    let desktop = desktop_rect(monitors);
    wallpaper::read(monitors)
        .into_iter()
        .filter_map(|w| {
            let m = monitors.iter().find(|m| m.device == w.device)?;
            Some(MonitorSource {
                monitor: w.device,
                rect: rect_f(&m.rect),
                desktop,
                scale: m.scale,
                image: match w.path {
                    Some(path) => WallpaperImage::File {
                        path,
                        stamp: w.stamp,
                    },
                    None => WallpaperImage::None,
                },
                position: match w.position {
                    wallpaper::Position::Center => Position::Center,
                    wallpaper::Position::Tile => Position::Tile,
                    wallpaper::Position::Stretch => Position::Stretch,
                    wallpaper::Position::Fit => Position::Fit,
                    wallpaper::Position::Fill => Position::Fill,
                    wallpaper::Position::Span => Position::Span,
                },
                background: color(w.color),
            })
        })
        .collect()
}

/// The backdrop of an off-screen render.
#[derive(Clone, Debug, PartialEq)]
pub enum Backdrop {
    /// A soft built-in gradient.
    Default,
    /// An image file, filling the widget.
    Image(PathBuf),
    /// A solid colour (`0xRRGGBB`): the neutral tile of the gallery previews.
    Solid(u32),
}

/// Monitor key used off-screen.
pub const PREVIEW: &str = "preview";

/// The off-screen "monitor": exactly the widget's `pw` × `ph` pixels.
pub fn preview_source(backdrop: &Backdrop, pw: u32, ph: u32, scale: f32) -> MonitorSource {
    let file = |p: &Path| WallpaperImage::File {
        path: p.to_path_buf(),
        stamp: None,
    };
    let image = match backdrop {
        Backdrop::Image(p) => file(p),
        Backdrop::Default => WallpaperImage::Pixels(default_backdrop()),
        Backdrop::Solid(_) => WallpaperImage::None,
    };
    let background = match backdrop {
        Backdrop::Solid(rgb) => color(*rgb),
        _ => Color::rgba(0.16, 0.18, 0.24, 1.0),
    };
    let area = RectF {
        x: 0.0,
        y: 0.0,
        w: pw as f32,
        h: ph as f32,
    };
    MonitorSource {
        monitor: PREVIEW.into(),
        rect: area,
        desktop: area,
        scale,
        image,
        position: Position::Fill,
        background,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(device: &str, l: i32, t: i32, r: i32, b: i32) -> Monitor {
        let rect = RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        };
        Monitor {
            device: device.into(),
            primary: l == 0 && t == 0,
            rect,
            work: rect,
            scale: 1.0,
        }
    }

    #[test]
    fn desktop_bounds_cover_every_monitor() {
        let ms = [mon("A", 0, 0, 1920, 1080), mon("B", -1280, -200, 0, 824)];
        let d = desktop_rect(&ms);
        assert_eq!((d.x, d.y, d.w, d.h), (-1280.0, -200.0, 3200.0, 1280.0));
        assert_eq!(desktop_rect(&[]), RectF::default());
    }

    #[test]
    fn widget_monitor_follows_its_centre() {
        let ms = [mon("A", 0, 0, 1920, 1080), mon("B", 1920, 0, 3840, 1080)];
        let r = |l, t, r, b| RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        };
        assert_eq!(monitor_at(&ms, &r(100, 100, 300, 300)).unwrap().device, "A");
        // Straddling: the centre decides.
        assert_eq!(monitor_at(&ms, &r(1800, 0, 2200, 100)).unwrap().device, "B");
        // Off-screen: the nearest monitor.
        assert_eq!(monitor_at(&ms, &r(4000, 0, 4100, 100)).unwrap().device, "B");
    }

    #[test]
    fn background_colours() {
        let c = color(0xFF8000);
        assert_eq!((c.r, c.g, c.b, c.a), (1.0, 128.0 / 255.0, 0.0, 1.0));
    }

    #[test]
    fn preview_fills_the_widget() {
        let s = preview_source(&Backdrop::Image("x.png".into()), 300, 200, 2.0);
        assert_eq!((s.rect.w, s.rect.h, s.scale), (300.0, 200.0, 2.0));
        assert_eq!(s.position, Position::Fill);
        assert!(matches!(s.image, WallpaperImage::File { .. }));
        let s = preview_source(&Backdrop::Default, 1, 1, 1.0);
        assert!(matches!(s.image, WallpaperImage::Pixels(_)));
    }
}
