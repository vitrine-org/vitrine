//! Native Windows 11 window chrome for the Slint windows (panel, prompt): the
//! title bar follows the panel's light or dark palette, and on Windows 11 22H2+
//! the window gets the Mica backdrop under both the title bar and the
//! (transparent) client area, so caption and navigation pane are one surface,
//! as in Settings or File Explorer.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW, DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::core::BOOL;

/// First Windows 11 build with `DWMWA_SYSTEMBACKDROP_TYPE` (22H2).
const MICA_BUILD: u32 = 22621;

/// Whether the system backdrop is available: decided before the window shows,
/// since the Slint window is only transparent when Mica will fill it.
pub fn mica_supported() -> bool {
    windows_build().is_some_and(supports_mica)
}

fn supports_mica(build: u32) -> bool {
    build >= MICA_BUILD
}

fn windows_build() -> Option<u32> {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::w;
    let mut buf = [0u16; 32];
    let mut len = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n]).trim().parse().ok()
}

fn hwnd(window: &slint::Window) -> Option<HWND> {
    match window.window_handle().window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
        _ => None,
    }
}

/// Which backdrop a window gets.
#[derive(Clone, Copy)]
pub enum Kind {
    /// Long-lived main window: Mica.
    Main,
    /// Short-lived dialog: acrylic, like flyouts and dialogs.
    Dialog,
}

/// Runs `f` as soon as the native window exists: the winit backend creates it
/// once the event loop runs, after `show()` returns.
pub fn when_ready<C: slint::ComponentHandle + 'static>(ui: &C, f: impl Fn(&C) + 'static) {
    let weak = ui.as_weak();
    let tries = std::cell::Cell::new(0u32);
    let timer = std::rc::Rc::new(slint::Timer::default());
    let t = timer.clone();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(1),
        move || {
            let Some(ui) = weak.upgrade() else {
                t.stop();
                return;
            };
            tries.set(tries.get() + 1);
            if hwnd(ui.window()).is_some() {
                f(&ui);
                t.stop();
            } else if tries.get() > 2000 {
                t.stop();
            }
        },
    );
}

/// Applies the title bar colour scheme and, when `mica`, the backdrop. Safe to
/// call again when the palette changes.
pub fn apply(window: &slint::Window, kind: Kind, dark: bool, mica: bool) {
    let Some(hwnd) = hwnd(window) else {
        return;
    };
    unsafe {
        let dark = BOOL::from(dark);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark as *const BOOL).cast(),
            size_of::<BOOL>() as u32,
        );
        if mica {
            let backdrop: DWM_SYSTEMBACKDROP_TYPE = match kind {
                Kind::Main => DWMSBT_MAINWINDOW,
                Kind::Dialog => DWMSBT_TRANSIENTWINDOW,
            };
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                (&backdrop as *const DWM_SYSTEMBACKDROP_TYPE).cast(),
                size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
            );
            let all = MARGINS {
                cxLeftWidth: -1,
                cxRightWidth: -1,
                cyTopHeight: -1,
                cyBottomHeight: -1,
            };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &all);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mica_needs_22h2() {
        assert!(!supports_mica(19045)); // Windows 10 22H2
        assert!(!supports_mica(22000)); // Windows 11 21H2
        assert!(supports_mica(22621));
        assert!(supports_mica(26200));
    }

    #[test]
    fn reads_the_running_build() {
        assert!(windows_build().is_some_and(|b| b >= 10240));
    }
}
