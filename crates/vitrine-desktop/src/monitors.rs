//! Display enumeration: device names, work areas and per-monitor DPI.

use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::core::BOOL;

#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub device: String,
    pub primary: bool,
    /// Full monitor rectangle, physical pixels.
    pub rect: RECT,
    /// Work area (without the taskbar), physical pixels.
    pub work: RECT,
    /// DIP → pixel factor.
    pub scale: f32,
}

const MONITORINFOF_PRIMARY: u32 = 1;

pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn proc(hmon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let out = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(hmon, &mut info as *mut _ as *mut _) }.as_bool() {
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
            let len = info
                .szDevice
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(info.szDevice.len());
            out.push(Monitor {
                device: String::from_utf16_lossy(&info.szDevice[..len]),
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                rect: info.monitorInfo.rcMonitor,
                work: info.monitorInfo.rcWork,
                scale: dx as f32 / 96.0,
            });
        }
        BOOL(1)
    }
    let mut out: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(proc), LPARAM(&mut out as *mut _ as isize));
    }
    out.sort_by_key(|m| !m.primary);
    out
}

/// Finds the monitor a layout entry refers to (`"primary"` or a device name),
/// falling back to the primary monitor when the device is gone.
pub fn resolve<'a>(monitors: &'a [Monitor], name: &str) -> Option<&'a Monitor> {
    monitors
        .iter()
        .find(|m| m.device.eq_ignore_ascii_case(name))
        .or_else(|| monitors.iter().find(|m| m.primary))
        .or(monitors.first())
}
