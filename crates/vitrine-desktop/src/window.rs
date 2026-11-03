//! Widget windows: layered, non-redirected children of the desktop layer whose
//! content comes from DirectComposition (see the findings in `shell.rs`).

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result};

use crate::shell::{self, DesktopLayer};

/// Creates a widget window covering `screen` (physical pixels), beneath the icons.
pub fn create_widget_window(
    layer: &DesktopLayer,
    class: PCWSTR,
    instance: HINSTANCE,
    screen: RECT,
) -> Result<HWND> {
    let local = shell::screen_to_parent(layer.parent, screen);
    unsafe {
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP | WS_EX_TRANSPARENT,
            class,
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            local.left,
            local.top,
            (local.right - local.left).max(1),
            (local.bottom - local.top).max(1),
            Some(layer.parent),
            None,
            Some(instance),
            None,
        )?;
        // Constant alpha 255 makes DWM compose the child; per-pixel alpha comes from DComp.
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
        restack(layer, hwnd)?;
        Ok(hwnd)
    }
}

/// Moves and resizes a widget window to `screen` (physical pixels).
pub fn place(layer: &DesktopLayer, hwnd: HWND, screen: RECT) -> Result<()> {
    let local = shell::screen_to_parent(layer.parent, screen);
    let after = layer.insert_after.unwrap_or(HWND_TOP);
    unsafe {
        SetWindowPos(
            hwnd,
            Some(after),
            local.left,
            local.top,
            (local.right - local.left).max(1),
            (local.bottom - local.top).max(1),
            SWP_NOACTIVATE,
        )
    }
}

/// Puts the window right beneath the icon view again.
pub fn restack(layer: &DesktopLayer, hwnd: HWND) -> Result<()> {
    let after = layer.insert_after.unwrap_or(HWND_TOP);
    unsafe {
        SetWindowPos(
            hwnd,
            Some(after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    }
}

/// Registers a window class with the arrow cursor.
pub fn register_class(
    instance: HINSTANCE,
    name: PCWSTR,
    proc: unsafe extern "system" fn(
        HWND,
        u32,
        windows::Win32::Foundation::WPARAM,
        windows::Win32::Foundation::LPARAM,
    ) -> windows::Win32::Foundation::LRESULT,
) -> Result<()> {
    let wc = WNDCLASSW {
        lpfnWndProc: Some(proc),
        hInstance: instance,
        lpszClassName: name,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        ..Default::default()
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        let err = windows::core::Error::from_thread();
        // Already registered (e.g. after an Explorer restart) is fine.
        if err.code() != windows::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS.to_hresult() {
            return Err(err);
        }
    }
    Ok(())
}
