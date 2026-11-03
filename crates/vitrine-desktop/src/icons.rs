//! Desktop icon geometry, read through the shell's `IFolderView`.
//!
//! The result is cached by callers and refreshed outside of input hooks: the
//! COM calls go cross-process into explorer.exe and must never run inside a
//! low-level hook callback.

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::MapWindowPoints;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, IServiceProvider};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    CSIDL_DESKTOP, IEnumIDList, IFolderView, IShellBrowser, IShellWindows, SID_STopLevelBrowser,
    SVGIO_ALLVIEW, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows,
};
use windows::core::Interface;

/// Screen-space rectangles of every desktop icon.
#[derive(Clone, Debug, Default)]
pub struct IconMap {
    pub rects: Vec<RECT>,
}

impl IconMap {
    pub fn hit(&self, pt: POINT) -> bool {
        self.rects
            .iter()
            .any(|r| pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom)
    }
}

/// Opens the desktop folder view. Requires COM to be initialized on the thread.
pub fn desktop_folder_view() -> windows::core::Result<IFolderView> {
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL)?;
        let loc = VARIANT::from(CSIDL_DESKTOP as i32);
        let root = VARIANT::default();
        let mut hwnd = 0i32;
        let disp = windows.FindWindowSW(&loc, &root, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
        let provider: IServiceProvider = disp.cast()?;
        let browser: IShellBrowser = provider.QueryService(&SID_STopLevelBrowser)?;
        let view = browser.QueryActiveShellView()?;
        view.cast()
    }
}

/// Reads the position of every icon and converts it to screen coordinates.
/// `list_view` is the desktop `SysListView32` (child of `SHELLDLL_DefView`).
pub fn read_icon_map(view: &IFolderView, list_view: HWND) -> windows::core::Result<IconMap> {
    unsafe {
        let mut spacing = POINT::default();
        view.GetSpacing(&mut spacing)?;

        let items: IEnumIDList = view.Items(SVGIO_ALLVIEW)?;
        let mut rects = Vec::new();
        loop {
            let mut pidl = [std::ptr::null_mut::<ITEMIDLIST>()];
            let mut fetched = 0u32;
            if items.Next(&mut pidl, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            if let Ok(pos) = view.GetItemPosition(pidl[0]) {
                let mut pts = [
                    pos,
                    POINT {
                        x: pos.x + spacing.x,
                        y: pos.y + spacing.y,
                    },
                ];
                MapWindowPoints(Some(list_view), None, &mut pts);
                rects.push(RECT {
                    left: pts[0].x,
                    top: pts[0].y,
                    right: pts[1].x,
                    bottom: pts[1].y,
                });
            }
            CoTaskMemFree(Some(pidl[0] as *const _));
        }
        Ok(IconMap { rects })
    }
}
