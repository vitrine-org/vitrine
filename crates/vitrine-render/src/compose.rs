//! Presenting widget frames: DirectComposition surfaces for live windows and
//! off-screen bitmaps for headless rendering.

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectComposition::{
    IDCompositionSurface, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::Win32::Graphics::Dxgi::IDXGISurface;
use windows::core::{Error, Result};

use crate::gpu::Gpu;
use crate::style::Transform2D;

fn bitmap_props(options: D2D1_BITMAP_OPTIONS) -> D2D1_BITMAP_PROPERTIES1 {
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

/// A window's DirectComposition content: one visual with one surface.
pub struct WindowVisual {
    target: IDCompositionTarget,
    visual: IDCompositionVisual,
    surface: Option<(IDCompositionSurface, u32, u32)>,
}

impl WindowVisual {
    pub fn new(gpu: &Gpu, hwnd: HWND) -> Result<Self> {
        let dcomp = gpu.dcomp.as_ref().ok_or_else(|| {
            Error::new(windows::Win32::Foundation::E_FAIL, "no composition device")
        })?;
        unsafe {
            let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            target.SetRoot(&visual)?;
            Ok(Self {
                target,
                visual,
                surface: None,
            })
        }
    }

    /// Draws a full frame of `w` × `h` pixels. `draw` receives the context and the
    /// transform mapping surface-local pixels into the (atlas) surface.
    pub fn draw(
        &mut self,
        gpu: &Gpu,
        dc: &ID2D1DeviceContext,
        w: u32,
        h: u32,
        draw: impl FnOnce(&ID2D1DeviceContext, Transform2D) -> Result<()>,
    ) -> Result<()> {
        let dcomp = gpu.dcomp.as_ref().unwrap();
        let (w, h) = (w.max(1), h.max(1));
        unsafe {
            if !matches!(&self.surface, Some((_, sw, sh)) if *sw == w && *sh == h) {
                let s = dcomp.CreateSurface(
                    w,
                    h,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_ALPHA_MODE_PREMULTIPLIED,
                )?;
                self.visual.SetContent(&s)?;
                self.surface = Some((s, w, h));
            }
            let (surface, _, _) = self.surface.as_ref().unwrap();
            let mut offset = POINT::default();
            let dxgi: IDXGISurface = surface.BeginDraw(None, &mut offset)?;
            let bitmap = dc.CreateBitmapFromDxgiSurface(
                &dxgi,
                Some(&bitmap_props(
                    D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                )),
            )?;
            dc.SetTarget(&bitmap);
            dc.BeginDraw();
            let origin = Transform2D::translate(offset.x as f32, offset.y as f32);
            dc.SetTransform(&windows_numerics::Matrix3x2::translation(
                offset.x as f32,
                offset.y as f32,
            ));
            // Clear only our rectangle of the (possibly shared) surface.
            dc.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: offset.x as f32,
                    top: offset.y as f32,
                    right: (offset.x + w as i32) as f32,
                    bottom: (offset.y + h as i32) as f32,
                },
                D2D1_ANTIALIAS_MODE_ALIASED,
            );
            dc.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));
            let result = draw(dc, origin);
            dc.PopAxisAlignedClip();
            let end = dc.EndDraw(None, None);
            dc.SetTarget(None);
            surface.EndDraw()?;
            result?;
            end?;
            dcomp.Commit()
        }
    }

    /// Drops the surface (e.g. while hidden) to free GPU memory.
    pub fn release(&mut self) -> Result<()> {
        self.surface = None;
        unsafe {
            self.visual.SetContent(None)?;
            let _ = &self.target;
        }
        Ok(())
    }
}

/// Renders off-screen and returns straight-alpha RGBA8 pixels.
pub fn render_offscreen(
    dc: &ID2D1DeviceContext,
    w: u32,
    h: u32,
    draw: impl FnOnce(&ID2D1DeviceContext) -> Result<()>,
) -> Result<Vec<u8>> {
    unsafe {
        let size = D2D_SIZE_U {
            width: w,
            height: h,
        };
        let target = dc.CreateBitmap(size, None, 0, &bitmap_props(D2D1_BITMAP_OPTIONS_TARGET))?;
        dc.SetTarget(&target);
        dc.BeginDraw();
        dc.SetTransform(&windows_numerics::Matrix3x2::identity());
        dc.Clear(Some(&D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }));
        let result = draw(dc);
        let end = dc.EndDraw(None, None);
        dc.SetTarget(None);
        result?;
        end?;
        let cpu = dc.CreateBitmap(
            size,
            None,
            0,
            &bitmap_props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW),
        )?;
        cpu.CopyFromBitmap(None, &target, None)?;
        let map = cpu.Map(D2D1_MAP_OPTIONS_READ)?;
        let mut out = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            let row =
                std::slice::from_raw_parts(map.bits.add(y * map.pitch as usize), w as usize * 4);
            for x in 0..w as usize {
                let (b, g, r, a) = (row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]);
                let un = |c: u8| {
                    if a == 0 {
                        0
                    } else {
                        ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
                    }
                };
                let o = (y * w as usize + x) * 4;
                out[o..o + 4].copy_from_slice(&[un(r), un(g), un(b), a]);
            }
        }
        cpu.Unmap()?;
        Ok(out)
    }
}

/// Writes RGBA8 pixels as a PNG file.
pub fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(std::io::Error::other)?;
    writer.write_image_data(rgba).map_err(std::io::Error::other)
}
