//! "Peek at the desktop" for edit mode: minimizes the user's open windows and
//! restores exactly those windows, in their original stacking order, afterwards.

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::BOOL;

use vitrine_desktop::util::class_name;

pub struct Peek {
    /// Minimized windows, topmost first.
    windows: Vec<HWND>,
    foreground: HWND,
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4).is_ok()
            && cloaked != 0
    }
}

/// A visible, restorable application window that covers part of the desktop.
fn is_app_window(hwnd: HWND, own_pid: u32) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() || is_cloaked(hwnd) {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == own_pid {
            return false;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 || ex & WS_EX_NOACTIVATE.0 != 0 {
            return false;
        }
        // Owned popups follow their owner; only windows that can be minimized.
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) || style & WS_MINIMIZEBOX.0 == 0
        {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() || r.right - r.left < 8 || r.bottom - r.top < 8 {
            return false;
        }
        !matches!(
            class_name(hwnd).as_str(),
            "Progman"
                | "WorkerW"
                | "Shell_TrayWnd"
                | "Shell_SecondaryTrayWnd"
                | "Windows.UI.Core.CoreWindow"
        )
    }
}

impl Peek {
    pub fn show_desktop() -> Peek {
        struct Ctx {
            pid: u32,
            out: Vec<HWND>,
        }
        unsafe extern "system" fn proc(hwnd: HWND, l: LPARAM) -> BOOL {
            let ctx = unsafe { &mut *(l.0 as *mut Ctx) };
            if is_app_window(hwnd, ctx.pid) {
                ctx.out.push(hwnd);
            }
            BOOL(1)
        }
        let mut ctx = Ctx {
            pid: unsafe { GetCurrentProcessId() },
            out: Vec::new(),
        };
        let foreground = unsafe { GetForegroundWindow() };
        unsafe {
            let _ = EnumWindows(Some(proc), LPARAM(&mut ctx as *mut _ as isize));
            for &hwnd in &ctx.out {
                // Async: a hung application must never block edit mode.
                let _ = ShowWindowAsync(hwnd, SW_SHOWMINNOACTIVE);
            }
        }
        log::debug!("edit mode: minimized {} windows", ctx.out.len());
        Peek {
            windows: ctx.out,
            foreground,
        }
    }

    /// Restores the windows, bottom-most first so the stacking order comes back.
    pub fn restore(self) {
        unsafe {
            for &hwnd in self.windows.iter().rev() {
                if IsWindow(Some(hwnd)).as_bool() && IsIconic(hwnd).as_bool() {
                    let _ = ShowWindowAsync(hwnd, SW_RESTORE);
                }
            }
            if !self.foreground.is_invalid() && self.windows.contains(&self.foreground) {
                let _ = SetForegroundWindow(self.foreground);
            }
        }
    }
}
