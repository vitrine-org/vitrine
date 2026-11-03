//! Shared graphics devices: one D3D11 device used by Direct2D, DirectComposition
//! and off-screen rendering.

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::{
    D2D1_DEBUG_LEVEL_NONE, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_OPTIONS,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1CreateFactory, ID2D1Device, ID2D1DeviceContext,
    ID2D1Factory1,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice, IDCompositionDevice};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory5,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{Interface, Result};

pub struct Gpu {
    pub d3d: ID3D11Device,
    pub dxgi: IDXGIDevice,
    pub d2d_factory: ID2D1Factory1,
    pub d2d_device: ID2D1Device,
    pub dwrite: IDWriteFactory5,
    pub wic: IWICImagingFactory,
    /// `None` when created for off-screen use only.
    pub dcomp: Option<IDCompositionDevice>,
    pub software: bool,
}

fn create_d3d(driver: D3D_DRIVER_TYPE) -> Result<ID3D11Device> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    Ok(device.unwrap())
}

impl Gpu {
    /// Creates the devices. COM must be initialized on the calling thread.
    /// Falls back to the WARP software rasterizer when no hardware device exists.
    pub fn new(with_composition: bool) -> Result<Self> {
        let (d3d, software) = match create_d3d(D3D_DRIVER_TYPE_HARDWARE) {
            Ok(d) => (d, false),
            Err(_) => (create_d3d(D3D_DRIVER_TYPE_WARP)?, true),
        };
        let dxgi: IDXGIDevice = d3d.cast()?;
        unsafe {
            let options = D2D1_FACTORY_OPTIONS {
                debugLevel: D2D1_DEBUG_LEVEL_NONE,
            };
            let d2d_factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, Some(&options))?;
            let d2d_device = d2d_factory.CreateDevice(&dxgi)?;
            let dwrite: IDWriteFactory5 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let dcomp = if with_composition {
                Some(DCompositionCreateDevice(&dxgi)?)
            } else {
                None
            };
            Ok(Self {
                d3d,
                dxgi,
                d2d_factory,
                d2d_device,
                dwrite,
                wic,
                dcomp,
                software,
            })
        }
    }

    pub fn new_context(&self) -> Result<ID2D1DeviceContext> {
        unsafe {
            self.d2d_device
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
        }
    }
}
