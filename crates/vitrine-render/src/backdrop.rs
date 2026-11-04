//! Frosted glass (`backdropFilter`): a blurred, saturated crop of the wallpaper
//! behind a widget.
//!
//! Widgets live beneath the desktop icons, where DWM offers no backdrop, so the
//! engine recomposes each monitor's wallpaper itself. Per monitor and per filter
//! the wallpaper is drawn once at reduced resolution, blurred and saturated with
//! Direct2D effects, and cached as one small bitmap shared by every widget on
//! that monitor; painting a glass box is then a single bitmap-brush fill.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::*;
use windows::core::{HSTRING, Interface, Result};
use windows_numerics::Matrix3x2;

use crate::color::Color;
use crate::layout::RectF;
use crate::style::{BackdropFilter, Transform2D};

// ───────────────────────────── geometry ─────────────────────────────

/// How Windows positions the wallpaper image on a monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Center,
    Tile,
    Stretch,
    Fit,
    Fill,
    /// One image across the bounding box of all monitors.
    Span,
}

/// Where the wallpaper image lands on one monitor, in monitor-local pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// The image rectangle (for tiles: the first tile; the rest repeat it).
    pub rect: RectF,
    pub tiled: bool,
}

/// Reproduces the wallpaper position modes. `image` is the image size in pixels;
/// `monitor` and `desktop` (the bounding box of all monitors, used by `Span`)
/// are in virtual-screen pixels.
pub fn place_wallpaper(
    mode: Position,
    image: (f32, f32),
    monitor: RectF,
    desktop: RectF,
) -> Placement {
    let (iw, ih) = (image.0.max(1.0), image.1.max(1.0));
    let centered = |area: RectF, s: f32| {
        let (w, h) = (iw * s, ih * s);
        RectF {
            x: area.x + (area.w - w) / 2.0,
            y: area.y + (area.h - h) / 2.0,
            w,
            h,
        }
    };
    let local = RectF {
        x: 0.0,
        y: 0.0,
        w: monitor.w,
        h: monitor.h,
    };
    let single = |rect| Placement { rect, tiled: false };
    match mode {
        Position::Center => {
            let r = centered(local, 1.0);
            // Native pixels on whole-pixel positions, as Explorer draws them.
            single(RectF {
                x: r.x.floor(),
                y: r.y.floor(),
                ..r
            })
        }
        Position::Tile => Placement {
            rect: RectF {
                x: 0.0,
                y: 0.0,
                w: iw,
                h: ih,
            },
            tiled: true,
        },
        Position::Stretch => single(local),
        Position::Fit => single(centered(local, (monitor.w / iw).min(monitor.h / ih))),
        Position::Fill => single(centered(local, (monitor.w / iw).max(monitor.h / ih))),
        Position::Span => {
            let r = centered(desktop, (desktop.w / iw).max(desktop.h / ih));
            single(RectF {
                x: r.x - monitor.x,
                y: r.y - monitor.y,
                ..r
            })
        }
    }
}

/// Upper bound for one cached bitmap (and for each transient one while building).
const MAX_BYTES: u64 = 3 << 20;

/// Downsampling factor for a blur of `sigma` physical pixels on a `w` × `h`
/// monitor: the blur hides the lost resolution, and the bitmap stays within
/// [`MAX_BYTES`].
pub fn downsample(sigma: f32, w: u32, h: u32) -> u32 {
    let mut d = if sigma >= 8.0 {
        4
    } else if sigma >= 4.0 {
        2
    } else {
        1
    };
    while d < 16 && w.div_ceil(d) as u64 * h.div_ceil(d) as u64 * 4 > MAX_BYTES {
        d *= 2;
    }
    d
}

// ───────────────────────────── sources ─────────────────────────────

/// Straight-alpha RGBA8 pixels.
#[derive(Debug, PartialEq)]
pub struct Pixels {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WallpaperImage {
    /// A solid background colour only.
    None,
    /// An image file; `stamp` (its modification time) detects in-place changes.
    File {
        path: PathBuf,
        stamp: Option<SystemTime>,
    },
    Pixels(Arc<Pixels>),
}

/// What one monitor shows behind the widgets.
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorSource {
    /// Monitor key (the GDI device name; `"preview"` off-screen).
    pub monitor: String,
    /// Monitor rectangle, virtual-screen pixels.
    pub rect: RectF,
    /// Bounding box of all monitors (for `Span`).
    pub desktop: RectF,
    /// DIP → pixel factor (blur radii are in DIPs).
    pub scale: f32,
    pub image: WallpaperImage,
    pub position: Position,
    pub background: Color,
}

/// A soft, neutral gradient used when no wallpaper is known (headless previews).
pub fn default_backdrop() -> Arc<Pixels> {
    let (w, h) = (192u32, 120u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    // Colour blobs over a dusk gradient; blurred, they read as calm glass.
    let blobs: [((f32, f32), f32, [f32; 3]); 3] = [
        ((0.18, 0.22), 0.55, [0.95, 0.55, 0.45]),
        ((0.85, 0.35), 0.50, [0.35, 0.55, 0.95]),
        ((0.55, 0.95), 0.60, [0.45, 0.80, 0.75]),
    ];
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 / (w - 1) as f32, y as f32 / (h - 1) as f32);
            let mut c = [0.20 + 0.10 * u, 0.22 + 0.06 * v, 0.34 + 0.10 * (1.0 - v)];
            for ((bx, by), r, col) in blobs {
                let d2 = ((u - bx) * 1.6).powi(2) + (v - by).powi(2);
                let k = (-d2 / (r * r) * 3.0).exp() * 0.55;
                for i in 0..3 {
                    c[i] += (col[i] - c[i]) * k;
                }
            }
            rgba.extend(c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8));
            rgba.push(255);
        }
    }
    Arc::new(Pixels { w, h, rgba })
}

// ───────────────────────────── cache keys ─────────────────────────────

/// A filter in physical pixels, quantized so that near-identical parameters
/// share one bitmap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FilterKey {
    /// Standard deviation in half physical pixels.
    pub blur: u32,
    /// Saturation in hundredths.
    pub saturate: u32,
}

impl FilterKey {
    pub fn new(f: &BackdropFilter, scale: f32) -> Self {
        Self {
            blur: (f.blur.max(0.0) * scale * 2.0).round().min(4096.0) as u32,
            saturate: (f.saturate.max(0.0) * 100.0).round().min(10_000.0) as u32,
        }
    }

    pub fn sigma(&self) -> f32 {
        self.blur as f32 / 2.0
    }

    pub fn saturation(&self) -> f32 {
        self.saturate as f32 / 100.0
    }
}

/// Bookkeeping of the cache, independent of the GPU: the monitor sources and
/// one value per (monitor, filter).
pub struct Store<V> {
    sources: HashMap<String, MonitorSource>,
    entries: HashMap<(String, FilterKey), V>,
    stale: bool,
}

impl<V> Default for Store<V> {
    fn default() -> Self {
        Self {
            sources: HashMap::new(),
            entries: HashMap::new(),
            stale: true,
        }
    }
}

impl<V> Store<V> {
    /// Marks the sources as outdated; they are re-read before the next use.
    pub fn invalidate(&mut self) {
        self.stale = true;
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// Installs fresh sources and drops the entries of every monitor whose
    /// source changed or disappeared. Returns whether anything changed.
    pub fn set_sources(&mut self, list: Vec<MonitorSource>) -> bool {
        let new: HashMap<String, MonitorSource> =
            list.into_iter().map(|s| (s.monitor.clone(), s)).collect();
        let changed = new != self.sources;
        let old = &self.sources;
        self.entries
            .retain(|(m, _), _| new.get(m).is_some_and(|s| old.get(m) == Some(s)));
        self.sources = new;
        self.stale = false;
        changed
    }

    pub fn source(&self, monitor: &str) -> Option<&MonitorSource> {
        self.sources.get(monitor)
    }

    /// The key of `filter` on `monitor` (blur radii scale with the monitor DPI).
    pub fn key(&self, monitor: &str, filter: &BackdropFilter) -> Option<(String, FilterKey)> {
        let s = self.sources.get(monitor)?;
        Some((monitor.to_string(), FilterKey::new(filter, s.scale)))
    }

    pub fn get(&self, monitor: &str, filter: &BackdropFilter) -> Option<&V> {
        self.entries.get(&self.key(monitor, filter)?)
    }

    pub fn contains(&self, key: &(String, FilterKey)) -> bool {
        self.entries.contains_key(key)
    }

    pub fn insert(&mut self, key: (String, FilterKey), value: V) {
        self.entries.insert(key, value);
    }

    /// Keeps only the entries some widget still uses.
    pub fn retain_used<'a>(
        &mut self,
        used: impl IntoIterator<Item = (&'a str, &'a BackdropFilter)>,
    ) {
        let keep: HashSet<(String, FilterKey)> = used
            .into_iter()
            .filter_map(|(m, f)| self.key(m, f))
            .collect();
        self.entries.retain(|k, _| keep.contains(k));
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.values()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Drops every entry (e.g. after a device loss) and re-reads the sources.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.stale = true;
    }
}

// ───────────────────────────── GPU cache ─────────────────────────────

/// One blurred wallpaper: `factor` monitor pixels per bitmap pixel.
pub struct Blurred {
    pub bitmap: ID2D1Bitmap1,
    pub factor: f32,
    pub bytes: usize,
}

/// Blurred wallpapers shared by all widgets. Failed builds are remembered (as
/// `None`) so they are not retried every frame.
#[derive(Default)]
pub struct BackdropCache {
    pub store: Store<Option<Blurred>>,
}

fn props(options: D2D1_BITMAP_OPTIONS) -> D2D1_BITMAP_PROPERTIES1 {
    D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: options,
        colorContext: std::mem::ManuallyDrop::new(None),
    }
}

fn rect_f(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.x + r.w,
        bottom: r.y + r.h,
    }
}

fn color_f(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

/// The CSS `saturate()` matrix in Direct2D's row-vector 5×4 layout.
pub fn saturate_matrix(s: f32) -> [f32; 20] {
    let (r, g, b) = (0.213, 0.715, 0.072);
    [
        r + (1.0 - r) * s,
        r - r * s,
        r - r * s,
        0.0,
        g - g * s,
        g + (1.0 - g) * s,
        g - g * s,
        0.0,
        b - b * s,
        b - b * s,
        b + (1.0 - b) * s,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        0.0,
        0.0,
        0.0,
    ]
}

fn f32_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// The wallpaper image as a D2D bitmap, decoded no larger than `want` pixels.
/// Returns the bitmap and the image's full size.
type Loaded = (ID2D1Bitmap1, (f32, f32));

fn image_size(wic: &IWICImagingFactory, image: &WallpaperImage) -> Option<(u32, u32)> {
    match image {
        WallpaperImage::None => None,
        WallpaperImage::Pixels(p) => Some((p.w, p.h)),
        WallpaperImage::File { path, .. } => unsafe {
            let decoder = wic
                .CreateDecoderFromFilename(
                    &HSTRING::from(path.as_os_str()),
                    None,
                    GENERIC_READ,
                    WICDecodeMetadataCacheOnDemand,
                )
                .ok()?;
            let frame = decoder.GetFrame(0).ok()?;
            let (mut w, mut h) = (0, 0);
            frame.GetSize(&mut w, &mut h).ok()?;
            (w > 0 && h > 0).then_some((w, h))
        },
    }
}

fn load_image(
    dc: &ID2D1DeviceContext,
    wic: &IWICImagingFactory,
    image: &WallpaperImage,
    want: (u32, u32),
) -> Result<Option<Loaded>> {
    unsafe {
        match image {
            WallpaperImage::None => Ok(None),
            WallpaperImage::Pixels(p) => {
                let mut bgra = Vec::with_capacity(p.rgba.len());
                for px in p.rgba.as_chunks::<4>().0 {
                    let a = px[3] as u32;
                    let m = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                    bgra.extend_from_slice(&[m(px[2]), m(px[1]), m(px[0]), px[3]]);
                }
                let bmp = dc.CreateBitmap(
                    D2D_SIZE_U {
                        width: p.w,
                        height: p.h,
                    },
                    Some(bgra.as_ptr() as *const _),
                    p.w * 4,
                    &props(D2D1_BITMAP_OPTIONS_NONE),
                )?;
                Ok(Some((bmp, (p.w as f32, p.h as f32))))
            }
            WallpaperImage::File { path, .. } => {
                let decoder = wic.CreateDecoderFromFilename(
                    &HSTRING::from(path.as_os_str()),
                    None,
                    GENERIC_READ,
                    WICDecodeMetadataCacheOnDemand,
                )?;
                let frame = decoder.GetFrame(0)?;
                let (mut w, mut h) = (0, 0);
                frame.GetSize(&mut w, &mut h)?;
                let mut source: IWICBitmapSource = frame.cast()?;
                let (tw, th) = (want.0.clamp(1, w.max(1)), want.1.clamp(1, h.max(1)));
                if tw < w || th < h {
                    // Decoding straight to the reduced size keeps big photos cheap.
                    let scaler = wic.CreateBitmapScaler()?;
                    scaler.Initialize(&source, tw, th, WICBitmapInterpolationModeFant)?;
                    source = scaler.cast()?;
                }
                let converter = wic.CreateFormatConverter()?;
                converter.Initialize(
                    &source,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeMedianCut,
                )?;
                let bmp = dc.CreateBitmapFromWicBitmap(&converter, None)?;
                Ok(Some((bmp, (w as f32, h as f32))))
            }
        }
    }
}

impl BackdropCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// GPU memory held by the cached bitmaps.
    pub fn bytes(&self) -> usize {
        self.store.values().flatten().map(|b| b.bytes).sum()
    }

    /// Builds the missing bitmaps for `filters` on `monitor`. Must be called
    /// outside `BeginDraw`; leaves the context without a target.
    pub fn ensure(
        &mut self,
        dc: &ID2D1DeviceContext,
        wic: &IWICImagingFactory,
        monitor: &str,
        filters: &[BackdropFilter],
    ) {
        for f in filters {
            let Some(key) = self.store.key(monitor, f) else {
                continue;
            };
            if self.store.contains(&key) {
                continue;
            }
            let src = self.store.source(monitor).unwrap();
            let built = match build(dc, wic, src, key.1) {
                Ok(b) => {
                    log::debug!(
                        "backdrop {monitor} σ={} s={}: {} KB",
                        key.1.sigma(),
                        key.1.saturation(),
                        b.bytes / 1024
                    );
                    Some(b)
                }
                Err(e) => {
                    log::warn!("backdrop for {monitor}: {e}");
                    None
                }
            };
            self.store.insert(key, built);
        }
    }

    /// What a widget on `monitor` needs to paint its glass. `monitor_to_target`
    /// maps monitor-local pixels to the render target's pixels.
    pub fn view<'a>(
        &'a self,
        monitor: &'a str,
        monitor_to_target: Transform2D,
    ) -> BackdropView<'a> {
        BackdropView {
            cache: self,
            monitor,
            monitor_to_target,
        }
    }
}

/// Composes the wallpaper of one monitor at reduced resolution, then blurs and
/// saturates it into the returned bitmap. Transient bitmaps are released here.
fn build(
    dc: &ID2D1DeviceContext,
    wic: &IWICImagingFactory,
    src: &MonitorSource,
    key: FilterKey,
) -> Result<Blurred> {
    let (mw, mh) = (
        src.rect.w.round().max(1.0) as u32,
        src.rect.h.round().max(1.0) as u32,
    );
    let d = downsample(key.sigma(), mw, mh);
    let (bw, bh) = (mw.div_ceil(d), mh.div_ceil(d));
    let size = D2D_SIZE_U {
        width: bw,
        height: bh,
    };
    let k = 1.0 / d as f32;
    unsafe {
        // 1. The wallpaper as Explorer draws it, scaled by 1/d.
        let base = dc.CreateBitmap(size, None, 0, &props(D2D1_BITMAP_OPTIONS_TARGET))?;
        let loaded = match image_size(wic, &src.image) {
            Some((iw, ih)) => {
                let p =
                    place_wallpaper(src.position, (iw as f32, ih as f32), src.rect, src.desktop);
                let want = (
                    (p.rect.w * k).ceil().max(1.0) as u32,
                    (p.rect.h * k).ceil().max(1.0) as u32,
                );
                match load_image(dc, wic, &src.image, want) {
                    Ok(Some((bmp, _))) => Some((bmp, p)),
                    Ok(None) => None,
                    Err(e) => {
                        log::warn!("wallpaper for {}: {e}", src.monitor);
                        None
                    }
                }
            }
            None => None,
        };
        dc.SetTarget(&base);
        dc.BeginDraw();
        dc.SetTransform(&Matrix3x2::identity());
        dc.Clear(Some(&color_f(Color {
            a: 1.0,
            ..src.background
        })));
        if let Some((bmp, p)) = &loaded {
            let bs = bmp.GetPixelSize();
            let dest = RectF {
                x: p.rect.x * k,
                y: p.rect.y * k,
                w: p.rect.w * k,
                h: p.rect.h * k,
            };
            if p.tiled {
                let brush = dc.CreateBitmapBrush(
                    bmp,
                    Some(&D2D1_BITMAP_BRUSH_PROPERTIES1 {
                        extendModeX: D2D1_EXTEND_MODE_WRAP,
                        extendModeY: D2D1_EXTEND_MODE_WRAP,
                        interpolationMode: D2D1_INTERPOLATION_MODE_LINEAR,
                    }),
                    Some(&D2D1_BRUSH_PROPERTIES {
                        opacity: 1.0,
                        transform: Matrix3x2 {
                            M11: dest.w / bs.width.max(1) as f32,
                            M12: 0.0,
                            M21: 0.0,
                            M22: dest.h / bs.height.max(1) as f32,
                            M31: dest.x,
                            M32: dest.y,
                        },
                    }),
                )?;
                dc.FillRectangle(
                    &D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: bw as f32,
                        bottom: bh as f32,
                    },
                    &brush,
                );
            } else {
                dc.DrawBitmap(
                    bmp,
                    Some(&rect_f(dest)),
                    1.0,
                    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                    None,
                    None,
                );
            }
        }
        let end = dc.EndDraw(None, None);
        dc.SetTarget(None);
        end?;
        drop(loaded);

        // 2. Clamp the edges (no dark fringe), blur, saturate.
        let border = dc.CreateEffect(&CLSID_D2D1Border)?;
        border.SetInput(0, &base, true);
        let clamp = (D2D1_BORDER_EDGE_MODE_CLAMP.0 as u32).to_le_bytes();
        border.SetValue(
            D2D1_BORDER_PROP_EDGE_MODE_X.0 as u32,
            D2D1_PROPERTY_TYPE_ENUM,
            &clamp,
        )?;
        border.SetValue(
            D2D1_BORDER_PROP_EDGE_MODE_Y.0 as u32,
            D2D1_PROPERTY_TYPE_ENUM,
            &clamp,
        )?;
        let mut output: ID2D1Effect = border;
        let sigma = key.sigma() * k;
        if sigma > 0.05 {
            let blur = dc.CreateEffect(&CLSID_D2D1GaussianBlur)?;
            blur.SetInput(0, &output.GetOutput()?, true);
            blur.SetValue(
                D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32,
                D2D1_PROPERTY_TYPE_FLOAT,
                &sigma.to_le_bytes(),
            )?;
            blur.SetValue(
                D2D1_GAUSSIANBLUR_PROP_OPTIMIZATION.0 as u32,
                D2D1_PROPERTY_TYPE_ENUM,
                &(D2D1_GAUSSIANBLUR_OPTIMIZATION_QUALITY.0 as u32).to_le_bytes(),
            )?;
            output = blur;
        }
        if key.saturate != 100 {
            let cm = dc.CreateEffect(&CLSID_D2D1ColorMatrix)?;
            cm.SetInput(0, &output.GetOutput()?, true);
            cm.SetValue(
                D2D1_COLORMATRIX_PROP_COLOR_MATRIX.0 as u32,
                D2D1_PROPERTY_TYPE_MATRIX_5X4,
                &f32_bytes(&saturate_matrix(key.saturation())),
            )?;
            cm.SetValue(
                D2D1_COLORMATRIX_PROP_CLAMP_OUTPUT.0 as u32,
                D2D1_PROPERTY_TYPE_BOOL,
                &1u32.to_le_bytes(),
            )?;
            output = cm;
        }
        let image = output.GetOutput()?;
        let out = dc.CreateBitmap(size, None, 0, &props(D2D1_BITMAP_OPTIONS_TARGET))?;
        dc.SetTarget(&out);
        dc.BeginDraw();
        dc.SetTransform(&Matrix3x2::identity());
        dc.DrawImage(
            &image,
            None,
            None,
            D2D1_INTERPOLATION_MODE_LINEAR,
            D2D1_COMPOSITE_MODE_SOURCE_COPY,
        );
        let end = dc.EndDraw(None, None);
        dc.SetTarget(None);
        end?;
        Ok(Blurred {
            bitmap: out,
            factor: d as f32,
            bytes: (bw * bh * 4) as usize,
        })
    }
}

/// A widget's access to the shared blurred wallpapers while it paints.
pub struct BackdropView<'a> {
    cache: &'a BackdropCache,
    monitor: &'a str,
    monitor_to_target: Transform2D,
}

impl BackdropView<'_> {
    /// The blurred bitmap for `filter` and the transform from its pixels to
    /// the render target's pixels.
    pub fn lookup(&self, filter: &BackdropFilter) -> Option<(&ID2D1Bitmap1, Transform2D)> {
        let b = self.cache.store.get(self.monitor, filter)?.as_ref()?;
        Some((
            &b.bitmap,
            Transform2D::scale(b.factor, b.factor).then(self.monitor_to_target),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f32, y: f32, w: f32, h: f32) -> RectF {
        RectF { x, y, w, h }
    }

    const MON: RectF = RectF {
        x: 0.0,
        y: 0.0,
        w: 1920.0,
        h: 1080.0,
    };

    fn place(mode: Position, img: (f32, f32)) -> Placement {
        let mut p = place_wallpaper(mode, img, MON, MON);
        // Compare to a hundredth of a pixel.
        let q = |v: f32| (v * 100.0).round() / 100.0;
        p.rect = r(q(p.rect.x), q(p.rect.y), q(p.rect.w), q(p.rect.h));
        p
    }

    #[test]
    fn fill_covers_and_crops_centred() {
        // A 4:3 image on a 16:9 monitor: width matches, height overflows.
        let p = place(Position::Fill, (800.0, 600.0));
        assert_eq!(p.rect, r(0.0, -180.0, 1920.0, 1440.0));
        assert!(!p.tiled);
        // A very wide image: height matches, sides are cropped.
        let p = place(Position::Fill, (4000.0, 1000.0));
        assert_eq!(p.rect, r(-1200.0, 0.0, 4320.0, 1080.0));
    }

    #[test]
    fn fit_letterboxes_centred() {
        let p = place(Position::Fit, (800.0, 600.0));
        assert_eq!(p.rect, r(240.0, 0.0, 1440.0, 1080.0));
        let p = place(Position::Fit, (4000.0, 1000.0));
        assert_eq!(p.rect, r(0.0, 300.0, 1920.0, 480.0));
    }

    #[test]
    fn stretch_fills_the_monitor() {
        let p = place(Position::Stretch, (800.0, 600.0));
        assert_eq!(p.rect, r(0.0, 0.0, 1920.0, 1080.0));
    }

    #[test]
    fn center_keeps_native_size_on_whole_pixels() {
        let p = place(Position::Center, (801.0, 600.0));
        assert_eq!(p.rect, r(559.0, 240.0, 801.0, 600.0));
        // Larger than the monitor: centred and cropped.
        let p = place(Position::Center, (2000.0, 1200.0));
        assert_eq!(p.rect, r(-40.0, -60.0, 2000.0, 1200.0));
    }

    #[test]
    fn tile_repeats_from_the_monitor_origin() {
        let p = place(Position::Tile, (64.0, 48.0));
        assert_eq!(p.rect, r(0.0, 0.0, 64.0, 48.0));
        assert!(p.tiled);
    }

    #[test]
    fn span_covers_all_monitors_and_crops_per_monitor() {
        // Two 1920×1080 monitors side by side, the second one to the right.
        let left = r(0.0, 0.0, 1920.0, 1080.0);
        let right = r(1920.0, 0.0, 1920.0, 1080.0);
        let desktop = r(0.0, 0.0, 3840.0, 1080.0);
        let img = (3840.0, 1080.0);
        let a = place_wallpaper(Position::Span, img, left, desktop);
        let b = place_wallpaper(Position::Span, img, right, desktop);
        assert_eq!(a.rect, r(0.0, 0.0, 3840.0, 1080.0));
        assert_eq!(b.rect, r(-1920.0, 0.0, 3840.0, 1080.0));
        // A monitor left of the primary (negative coordinates).
        let neg = r(-1920.0, 0.0, 1920.0, 1080.0);
        let desktop = r(-1920.0, 0.0, 3840.0, 1080.0);
        let c = place_wallpaper(Position::Span, (1920.0, 1080.0), neg, desktop);
        // Cover: scaled ×2 to 3840×2160, centred vertically.
        assert_eq!(c.rect, r(0.0, -540.0, 3840.0, 2160.0));
    }

    #[test]
    fn downsampling_follows_blur_and_memory() {
        assert_eq!(downsample(1.0, 800, 600), 1);
        assert_eq!(downsample(5.0, 800, 600), 2);
        assert_eq!(downsample(24.0, 800, 600), 4);
        // A 4K monitor stays under the budget even with a tiny blur.
        let d = downsample(1.0, 3840, 2160);
        assert!(3840u64.div_ceil(d as u64) * 2160u64.div_ceil(d as u64) * 4 <= MAX_BYTES);
        assert_eq!(d, 4);
        // 1440p with blur(16px) at 150%: σ = 24 px.
        assert_eq!(downsample(24.0, 2560, 1440), 4);
    }

    fn source(monitor: &str, scale: f32, image: WallpaperImage) -> MonitorSource {
        MonitorSource {
            monitor: monitor.into(),
            rect: MON,
            desktop: MON,
            scale,
            image,
            position: Position::Fill,
            background: Color::rgba(0.0, 0.0, 0.0, 1.0),
        }
    }

    fn file(p: &str) -> WallpaperImage {
        WallpaperImage::File {
            path: p.into(),
            stamp: None,
        }
    }

    const GLASS: BackdropFilter = BackdropFilter {
        blur: 16.0,
        saturate: 1.6,
    };

    #[test]
    fn keys_quantize_and_follow_dpi() {
        let a = FilterKey::new(&GLASS, 1.5);
        assert_eq!(
            a,
            FilterKey {
                blur: 48,
                saturate: 160
            }
        );
        assert_eq!(a.sigma(), 24.0);
        assert!((a.saturation() - 1.6).abs() < 1e-6);
        // Imperceptible differences share a bitmap.
        let b = BackdropFilter {
            blur: 16.1,
            saturate: 1.601,
        };
        assert_eq!(FilterKey::new(&b, 1.5), a);
        // Another DPI is another bitmap.
        assert_ne!(FilterKey::new(&GLASS, 1.0), a);
    }

    #[test]
    fn store_keys_by_monitor_and_filter() {
        let mut s: Store<u32> = Store::default();
        assert!(s.is_stale());
        s.set_sources(vec![
            source("A", 1.0, file("a.jpg")),
            source("B", 2.0, file("a.jpg")),
        ]);
        assert!(!s.is_stale());
        let (ka, kb) = (s.key("A", &GLASS).unwrap(), s.key("B", &GLASS).unwrap());
        assert_ne!(ka, kb, "same filter on two monitors");
        assert_eq!(kb.1.blur, 64, "monitor B is at 200%");
        assert!(s.key("C", &GLASS).is_none(), "unknown monitor");
        s.insert(ka.clone(), 1);
        s.insert(kb.clone(), 2);
        let other = BackdropFilter {
            blur: 4.0,
            saturate: 1.0,
        };
        s.insert(s.key("A", &other).unwrap(), 3);
        assert_eq!(s.get("A", &GLASS), Some(&1));
        assert_eq!(s.get("A", &other), Some(&3));
        assert_eq!(s.len(), 3);

        // Only the entries still in use survive.
        s.retain_used([("A", &GLASS), ("B", &GLASS)]);
        assert_eq!(s.len(), 2);
        assert_eq!(s.get("A", &other), None);

        // Unchanged sources keep the bitmaps.
        assert!(!s.set_sources(vec![
            source("A", 1.0, file("a.jpg")),
            source("B", 2.0, file("a.jpg")),
        ]));
        assert_eq!(s.len(), 2);

        // A new wallpaper on B drops only B's bitmap; a removed monitor drops its own.
        assert!(s.set_sources(vec![
            source("A", 1.0, file("a.jpg")),
            source("B", 2.0, file("b.jpg")),
        ]));
        assert_eq!((s.get("A", &GLASS), s.get("B", &GLASS)), (Some(&1), None));
        assert!(s.set_sources(vec![source("B", 2.0, file("b.jpg"))]));
        assert!(s.is_empty());

        // A file rewritten in place (same path, new time) is a new source.
        s.insert(s.key("B", &GLASS).unwrap(), 4);
        let stamped = WallpaperImage::File {
            path: "b.jpg".into(),
            stamp: Some(SystemTime::UNIX_EPOCH),
        };
        assert!(s.set_sources(vec![source("B", 2.0, stamped)]));
        assert!(s.is_empty());

        s.insert(s.key("B", &GLASS).unwrap(), 5);
        s.retain_used(std::iter::empty());
        assert!(s.is_empty(), "freed when no widget uses a backdrop");

        s.insert(s.key("B", &GLASS).unwrap(), 6);
        s.clear();
        assert!(s.is_empty() && s.is_stale());
    }

    #[test]
    fn saturate_matrix_matches_css() {
        // saturate(1) is the identity; saturate(0) maps every colour to its luma.
        let id = saturate_matrix(1.0);
        for (i, v) in id.iter().enumerate() {
            let want = if i % 5 == 0 && i < 16 { 1.0 } else { 0.0 };
            assert!((v - want).abs() < 1e-6, "{i}: {v}");
        }
        let grey = saturate_matrix(0.0);
        let apply = |m: &[f32; 20], c: [f32; 3]| {
            [0, 1, 2].map(|j| c[0] * m[j] + c[1] * m[4 + j] + c[2] * m[8 + j])
        };
        let out = apply(&grey, [1.0, 0.0, 0.0]);
        assert!(out.iter().all(|v| (v - 0.213).abs() < 1e-6));
        // White stays white at any saturation.
        let out = apply(&saturate_matrix(1.8), [1.0, 1.0, 1.0]);
        assert!(out.iter().all(|v| (v - 1.0).abs() < 1e-5));
    }

    #[test]
    fn default_backdrop_is_opaque_and_soft() {
        let p = default_backdrop();
        assert_eq!(p.rgba.len(), (p.w * p.h * 4) as usize);
        assert!(p.rgba.as_chunks::<4>().0.iter().all(|px| px[3] == 255));
    }
}
