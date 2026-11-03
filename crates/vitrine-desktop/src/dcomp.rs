//! DirectComposition-backed content for a widget window: a premultiplied BGRA
//! surface composed by DWM with per-pixel alpha over whatever lies beneath.
//!
//! `UpdateLayeredWindow` does not work for child windows whose parent belongs to
//! another process (Progman lives in explorer.exe), so widgets present through
//! DirectComposition instead.

use windows::Win32::Foundation::{HMODULE, HWND, POINT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionSurface, IDCompositionTarget,
    IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::core::{Interface, Result};

pub struct CompositionSurface {
    width: u32,
    height: u32,
    _d3d: ID3D11Device,
    context: ID3D11DeviceContext,
    device: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    surface: IDCompositionSurface,
}

impl CompositionSurface {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        unsafe {
            let mut d3d = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                Some(&mut context),
            )?;
            let d3d: ID3D11Device = d3d.unwrap();
            let context = context.unwrap();
            let dxgi: IDXGIDevice = d3d.cast()?;
            let device: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let target = device.CreateTargetForHwnd(hwnd, true)?;
            let visual = device.CreateVisual()?;
            let surface = device.CreateSurface(
                width,
                height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_ALPHA_MODE_PREMULTIPLIED,
            )?;
            visual.SetContent(&surface)?;
            target.SetRoot(&visual)?;
            Ok(Self {
                width,
                height,
                _d3d: d3d,
                context,
                device,
                _target: target,
                _visual: visual,
                surface,
            })
        }
    }

    /// Uploads a full frame of premultiplied BGRA pixels (`0xAARRGGBB`, top row first).
    pub fn upload(&self, pixels: &[u32]) -> Result<()> {
        assert_eq!(pixels.len(), (self.width * self.height) as usize);
        unsafe {
            let mut offset = POINT::default();
            let texture: ID3D11Texture2D = self.surface.BeginDraw(None, &mut offset)?;
            let bounds = D3D11_BOX {
                left: offset.x as u32,
                top: offset.y as u32,
                front: 0,
                right: offset.x as u32 + self.width,
                bottom: offset.y as u32 + self.height,
                back: 1,
            };
            self.context.UpdateSubresource(
                &texture,
                0,
                Some(&bounds),
                pixels.as_ptr().cast(),
                self.width * 4,
                0,
            );
            self.surface.EndDraw()?;
            self.device.Commit()
        }
    }
}
