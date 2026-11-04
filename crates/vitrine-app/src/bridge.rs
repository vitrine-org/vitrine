//! Keeps the control panel single-instance. The first panel owns a hidden
//! "bridge" window; a later `vitrine --panel ...` (or the tray) hands its page
//! request to that window with WM_COPYDATA, lets it take the foreground and
//! exits, so the existing panel switches page and comes to the front instead
//! of a second panel opening (or the request being lost behind other windows).

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, CreateWindowExW, DefWindowProcW, EnumThreadWindows,
    FindWindowW, GWL_EXSTYLE, GWL_STYLE, GetWindowLongW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, SMTO_ABORTIFHUNG, SW_RESTORE, SendMessageTimeoutW, SetForegroundWindow,
    ShowWindow, WINDOW_EX_STYLE, WM_COPYDATA, WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_POPUP,
};
use windows::core::{BOOL, PCWSTR, w};

const CLASS: PCWSTR = w!("Vitrine.PanelBridge");
/// Tags our WM_COPYDATA payloads ("VTPN").
const MAGIC: usize = 0x5654_504E;

/// What a panel launch asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Request {
    pub page: Option<String>,
    pub instance: Option<String>,
}

impl Request {
    pub fn encode(&self) -> String {
        let mut out = String::new();
        if let Some(p) = &self.page {
            out.push_str("page=");
            out.push_str(p);
            out.push('\n');
        }
        if let Some(i) = &self.instance {
            out.push_str("instance=");
            out.push_str(i);
            out.push('\n');
        }
        out
    }

    pub fn decode(s: &str) -> Request {
        let mut r = Request::default();
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("page=") {
                r.page = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("instance=") {
                r.instance = Some(v.to_string());
            }
        }
        r
    }
}

/// Hands the request to a running panel. True when one took it.
pub fn forward(req: &Request) -> bool {
    let Ok(hwnd) = (unsafe { FindWindowW(CLASS, None) }) else {
        return false;
    };
    if hwnd.is_invalid() {
        return false;
    }
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        // We hold the foreground right (the user just clicked us): pass it on.
        let _ = AllowSetForegroundWindow(pid);
    }
    let data = req.encode();
    let cds = COPYDATASTRUCT {
        dwData: MAGIC,
        cbData: data.len() as u32,
        lpData: data.as_ptr() as *mut _,
    };
    let mut result = 0usize;
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&cds as *const _ as isize),
            SMTO_ABORTIFHUNG,
            3000,
            Some(&mut result),
        )
    };
    sent.0 != 0 && result == 1
}

/// True if this is the first panel. A later one waits briefly for the first
/// panel's bridge (it may still be starting) and forwards the request to it.
pub fn claim_or_forward(req: &Request) -> bool {
    let first = unsafe {
        let h = CreateMutexW(None, true, w!("Local\\Vitrine.Panel"));
        // Leaked on purpose: the mutex lives as long as the process.
        h.is_ok()
            && windows::Win32::Foundation::GetLastError()
                != windows::Win32::Foundation::ERROR_ALREADY_EXISTS
    };
    if first {
        return true;
    }
    for _ in 0..50 {
        if forward(req) {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // The other panel never answered (hung?): open anyway.
    true
}

type Handler = Box<dyn Fn(Request)>;

thread_local! {
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA && lparam.0 != 0 {
        let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
        if cds.dwData == MAGIC {
            let bytes = if cds.lpData.is_null() || cds.cbData == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) }
            };
            let req = Request::decode(&String::from_utf8_lossy(bytes));
            // Handle it after the sender is released, on the UI event loop.
            let _ = slint::invoke_from_event_loop(move || {
                HANDLER.with(|h| {
                    if let Some(f) = &*h.borrow() {
                        f(req);
                    }
                });
            });
            return LRESULT(1);
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Creates the bridge window; `on_request` runs on the UI thread.
pub fn listen(on_request: impl Fn(Request) + 'static) {
    HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(on_request)));
    unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else {
            return;
        };
        if let Err(e) = vitrine_desktop::window::register_class(hinst.into(), CLASS, proc) {
            log::warn!("panel bridge: {e}");
            return;
        }
        // A hidden top-level window (message-only ones are invisible to FindWindow).
        if let Err(e) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("Vitrine panel bridge"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        ) {
            log::warn!("panel bridge: {e}");
        }
    }
}

/// Restores and raises the panel window. Only captioned app windows count:
/// the UI thread also owns hidden helper windows (winit's event target, the
/// bridge) that must not take the foreground.
pub fn bring_to_front() {
    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> BOOL {
        unsafe {
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            let app = IsWindowVisible(hwnd).as_bool()
                && style & WS_CAPTION.0 == WS_CAPTION.0
                && ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) == 0;
            if app {
                *(found.0 as *mut HWND) = hwnd;
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut found = HWND::default();
    unsafe {
        let _ = EnumThreadWindows(
            GetCurrentThreadId(),
            Some(each),
            LPARAM(&mut found as *mut HWND as isize),
        );
        if found.is_invalid() {
            return;
        }
        if IsIconic(found).as_bool() {
            let _ = ShowWindow(found, SW_RESTORE);
        }
        let _ = BringWindowToTop(found);
        let _ = SetForegroundWindow(found);
    }
}

/// Lets a process we just started take the foreground (its window would
/// otherwise open behind the active one).
pub fn allow_foreground(pid: u32) {
    unsafe {
        let _ = AllowSetForegroundWindow(pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        for r in [
            Request::default(),
            Request {
                page: Some("store".into()),
                instance: None,
            },
            Request {
                page: None,
                instance: Some("w1a2-0".into()),
            },
            Request {
                page: Some("gallery".into()),
                instance: Some("x".into()),
            },
        ] {
            assert_eq!(Request::decode(&r.encode()), r);
        }
    }

    #[test]
    fn unknown_lines_are_ignored() {
        let r = Request::decode("bogus\npage=general\nfoo=bar\n");
        assert_eq!(r.page.as_deref(), Some("general"));
        assert_eq!(r.instance, None);
    }
}
