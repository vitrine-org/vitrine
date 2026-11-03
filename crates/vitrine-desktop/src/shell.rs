//! Locating the window that widgets must be parented to so that they are drawn
//! above the wallpaper but beneath the desktop icons.
//!
//! Two shell layouts exist:
//!
//! * **Modern** (Windows 11 24H2 and later): after Progman receives message
//!   `0x052C`, both the icon view (`SHELLDLL_DefView`) and the wallpaper
//!   `WorkerW` are *children* of `Progman`. Widgets become children of `Progman`
//!   inserted in z-order right below `SHELLDLL_DefView`.
//! * **Legacy** (Windows 10, Windows 11 before 24H2): `0x052C` spawns a
//!   *top-level* `WorkerW` behind the top-level window hosting
//!   `SHELLDLL_DefView`. Widgets become children of that `WorkerW`.
//!
//! Findings from the milestone 1 spike (Windows 11 25H2, build 26200):
//!
//! * `SHELLDLL_DefView` is itself a full-screen `WS_EX_LAYERED` child, so
//!   siblings below it show through wherever there is no icon.
//! * A child window is only composed by DWM if it is layered. Plain children and
//!   layered children updated with `UpdateLayeredWindow` stay invisible, because
//!   the parent belongs to another process (explorer.exe).
//! * What works: a `WS_CHILD` window with `WS_EX_LAYERED | WS_EX_NOREDIRECTIONBITMAP`,
//!   `SetLayeredWindowAttributes(255, LWA_ALPHA)`, and content presented through
//!   DirectComposition (see [`crate::dcomp`]) with premultiplied alpha.

use std::fmt;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::MapWindowPoints;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, GW_CHILD, GW_HWNDNEXT, GetWindow, GetWindowRect,
    SMTO_NORMAL, SendMessageTimeoutW,
};
use windows::core::{BOOL, w};

use crate::util::class_name;

/// Undocumented Progman message that makes the shell create the wallpaper `WorkerW`.
const WM_SPAWN_WORKERW: u32 = 0x052C;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    Modern,
    Legacy,
}

/// The place in the shell window tree where widget windows live.
#[derive(Clone, Copy, Debug)]
pub struct DesktopLayer {
    pub kind: LayerKind,
    /// Parent for widget windows.
    pub parent: HWND,
    /// Widget windows are inserted right after (i.e. beneath) this sibling.
    /// `None` means "top of the parent's children".
    pub insert_after: Option<HWND>,
    /// The icon view, used for diagnostics and input routing.
    pub def_view: HWND,
}

#[derive(Debug)]
pub enum LocateError {
    ProgmanNotFound,
    DefViewNotFound,
    WorkerWNotFound,
}

impl fmt::Display for LocateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProgmanNotFound => {
                write!(f, "Progman window not found (is explorer.exe running?)")
            }
            Self::DefViewNotFound => write!(f, "SHELLDLL_DefView not found"),
            Self::WorkerWNotFound => write!(f, "wallpaper WorkerW not found"),
        }
    }
}

impl std::error::Error for LocateError {}

/// Finds the desktop layer, asking the shell to create the wallpaper `WorkerW`
/// if needed. Waits up to `timeout` for the shell to settle.
pub fn locate(timeout: Duration) -> Result<DesktopLayer, LocateError> {
    let progman = unsafe { FindWindowW(w!("Progman"), None) }
        .ok()
        .filter(|h| !h.is_invalid())
        .ok_or(LocateError::ProgmanNotFound)?;

    unsafe {
        // Same parameters used by most wallpaper engines; the second form is a
        // fallback for older builds.
        SendMessageTimeoutW(
            progman,
            WM_SPAWN_WORKERW,
            WPARAM(0xD),
            LPARAM(0x1),
            SMTO_NORMAL,
            1000,
            None,
        );
        SendMessageTimeoutW(
            progman,
            WM_SPAWN_WORKERW,
            WPARAM(0),
            LPARAM(0),
            SMTO_NORMAL,
            1000,
            None,
        );
    }

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(layer) = try_modern(progman).or_else(try_legacy) {
            return Ok(layer);
        }
        if Instant::now() >= deadline {
            return Err(if find_child(progman, "SHELLDLL_DefView").is_some() {
                LocateError::WorkerWNotFound
            } else {
                LocateError::DefViewNotFound
            });
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn find_child(parent: HWND, class: &str) -> Option<HWND> {
    let wide = crate::util::WideStr::new(class);
    unsafe { FindWindowExW(Some(parent), None, wide.as_pcwstr(), None) }
        .ok()
        .filter(|h| !h.is_invalid())
}

fn try_modern(progman: HWND) -> Option<DesktopLayer> {
    let def_view = find_child(progman, "SHELLDLL_DefView")?;
    // The wallpaper WorkerW must also be a child for the modern layout.
    find_child(progman, "WorkerW")?;
    Some(DesktopLayer {
        kind: LayerKind::Modern,
        parent: progman,
        insert_after: Some(def_view),
        def_view,
    })
}

fn try_legacy() -> Option<DesktopLayer> {
    struct Found {
        host: HWND,
        def_view: HWND,
    }
    let mut found: Option<Found> = None;

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = unsafe { &mut *(lparam.0 as *mut Option<Found>) };
        if let Some(def_view) = find_child(hwnd, "SHELLDLL_DefView") {
            *out = Some(Found {
                host: hwnd,
                def_view,
            });
            return BOOL(0);
        }
        BOOL(1)
    }

    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut found as *mut _ as isize));
    }
    let found = found?;
    // The wallpaper WorkerW is the top-level window that follows the DefView host.
    let worker = unsafe { FindWindowExW(None, Some(found.host), w!("WorkerW"), None) }
        .ok()
        .filter(|h| !h.is_invalid())?;
    Some(DesktopLayer {
        kind: LayerKind::Legacy,
        parent: worker,
        insert_after: None,
        def_view: found.def_view,
    })
}

/// Converts a rectangle in screen coordinates to client coordinates of `parent`.
pub fn screen_to_parent(parent: HWND, rect: RECT) -> RECT {
    let mut pts = [
        POINT {
            x: rect.left,
            y: rect.top,
        },
        POINT {
            x: rect.right,
            y: rect.bottom,
        },
    ];
    unsafe { MapWindowPoints(None, Some(parent), &mut pts) };
    RECT {
        left: pts[0].x,
        top: pts[0].y,
        right: pts[1].x,
        bottom: pts[1].y,
    }
}

/// `style=… ex=… visible=…` for diagnostics.
pub fn describe_styles(hwnd: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, IsWindowVisible,
    };
    unsafe {
        format!(
            "style={:08x} ex={:08x} visible={}",
            GetWindowLongPtrW(hwnd, GWL_STYLE) as u32,
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32,
            IsWindowVisible(hwnd).as_bool()
        )
    }
}

/// Human-readable dump of the children of `hwnd` in z-order (top first).
pub fn describe_children(hwnd: HWND) -> Vec<String> {
    let mut out = Vec::new();
    let mut child = unsafe { GetWindow(hwnd, GW_CHILD) }.ok();
    while let Some(c) = child.filter(|h| !h.is_invalid()) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(c, &mut r);
        }
        out.push(format!(
            "{:#x} {} [{},{} {}x{}] {}",
            c.0 as usize,
            class_name(c),
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            describe_styles(c)
        ));
        child = unsafe { GetWindow(c, GW_HWNDNEXT) }.ok();
    }
    out
}
