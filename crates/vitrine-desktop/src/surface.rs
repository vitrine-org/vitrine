//! A CPU-accessible, premultiplied BGRA surface that is presented to a layered
//! window with `UpdateLayeredWindow`. Only the window's own area is uploaded,
//! and only when the caller presents a new frame.

use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
    HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};

pub struct LayeredSurface {
    width: i32,
    height: i32,
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
}

impl LayeredSurface {
    pub fn new(width: i32, height: i32) -> windows::core::Result<Self> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            let dc = CreateCompatibleDC(None);
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let previous = SelectObject(dc, bitmap.into());
            Ok(Self {
                width,
                height,
                dc,
                bitmap,
                previous,
                bits: bits as *mut u32,
            })
        }
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    /// Premultiplied BGRA pixels, row-major, top row first (`0xAARRGGBB` per `u32`).
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.width * self.height) as usize) }
    }

    /// Uploads the surface to `hwnd`, which must have `WS_EX_LAYERED`.
    /// The window keeps its current position; its size becomes the surface size.
    pub fn present(&self, hwnd: HWND) -> windows::core::Result<()> {
        self.present_at(hwnd, None)
    }

    /// Like [`present`](Self::present), also passing the destination position
    /// (parent client coordinates for child windows) and the screen DC.
    pub fn present_at(&self, hwnd: HWND, dst: Option<POINT>) -> windows::core::Result<()> {
        use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
        let size = SIZE {
            cx: self.width,
            cy: self.height,
        };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            let screen = dst.map(|_| GetDC(None));
            let result = UpdateLayeredWindow(
                hwnd,
                screen,
                dst.as_ref().map(|p| p as *const POINT),
                Some(&size),
                Some(self.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            if let Some(dc) = screen {
                ReleaseDC(None, dc);
            }
            result
        }
    }
}

impl Drop for LayeredSurface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Packs a straight-alpha color into premultiplied `0xAARRGGBB`.
pub fn premultiply(r: u8, g: u8, b: u8, a: u8) -> u32 {
    let pm = |c: u8| (c as u32 * a as u32 + 127) / 255;
    (a as u32) << 24 | pm(r) << 16 | pm(g) << 8 | pm(b)
}
