//! Small Win32 shell helpers: tray icon, popup menus, autostart, opening URLs.

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Controls::{LIM_SMALL, LoadIconMetric};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn copy_tip(dst: &mut [u16], s: &str) {
    for (d, c) in dst
        .iter_mut()
        .zip(s.encode_utf16().chain(std::iter::once(0)))
    {
        *d = c;
    }
    if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

pub struct Tray {
    data: NOTIFYICONDATAW,
}

impl Tray {
    /// Adds the tray icon; clicks arrive as `callback` messages on `hwnd`.
    pub fn add(hwnd: HWND, callback: u32, tip: &str) -> Tray {
        let instance = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }
            .unwrap_or_default();
        let icon = unsafe { LoadIconMetric(Some(instance.into()), PCWSTR(1 as _), LIM_SMALL) }
            .or_else(|_| unsafe { LoadIconW(None, IDI_APPLICATION) })
            .unwrap_or_default();
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: callback,
            hIcon: icon,
            ..Default::default()
        };
        copy_tip(&mut data.szTip, tip);
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let mut tray = Tray { data };
        tray.show();
        tray
    }

    /// (Re-)adds the icon, e.g. after Explorer restarted.
    pub fn show(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data);
            if Shell_NotifyIconW(NIM_ADD, &self.data).as_bool() {
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &self.data);
            } else {
                log::warn!("tray icon could not be added");
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data);
        }
    }
}

/// A popup menu built from a simple description.
pub enum Item {
    Action {
        id: u32,
        label: String,
        checked: bool,
        enabled: bool,
    },
    Sub {
        label: String,
        items: Vec<Item>,
    },
    Separator,
}

impl Item {
    pub fn action(id: u32, label: impl Into<String>) -> Item {
        Item::Action {
            id,
            label: label.into(),
            checked: false,
            enabled: true,
        }
    }
    pub fn check(id: u32, label: impl Into<String>, checked: bool) -> Item {
        Item::Action {
            id,
            label: label.into(),
            checked,
            enabled: true,
        }
    }
    pub fn disabled(label: impl Into<String>) -> Item {
        Item::Action {
            id: 0,
            label: label.into(),
            checked: false,
            enabled: false,
        }
    }
}

fn build(items: &[Item]) -> HMENU {
    unsafe {
        let menu = CreatePopupMenu().unwrap_or_default();
        for item in items {
            match item {
                Item::Action {
                    id,
                    label,
                    checked,
                    enabled,
                } => {
                    let mut flags = MF_STRING;
                    if *checked {
                        flags |= MF_CHECKED;
                    }
                    if !*enabled {
                        flags |= MF_GRAYED;
                    }
                    let text = wide(label);
                    let _ = AppendMenuW(menu, flags, *id as usize, PCWSTR(text.as_ptr()));
                }
                Item::Sub { label, items } => {
                    let sub = build(items);
                    let text = wide(label);
                    let _ = AppendMenuW(
                        menu,
                        MF_POPUP | MF_STRING,
                        sub.0 as usize,
                        PCWSTR(text.as_ptr()),
                    );
                }
                Item::Separator => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
            }
        }
        menu
    }
}

/// Shows a menu at `pt` (screen pixels) and returns the chosen id.
pub fn popup(owner: HWND, pt: POINT, items: &[Item]) -> Option<u32> {
    unsafe {
        let menu = build(items);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(owner);
        let id = TrackPopupMenuEx(
            menu,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN).0,
            pt.x,
            pt.y,
            owner,
            None,
        );
        let _ = PostMessageW(Some(owner), WM_NULL, Default::default(), Default::default());
        let _ = DestroyMenu(menu);
        (id.0 != 0).then_some(id.0 as u32)
    }
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("Vitrine");

pub fn autostart_enabled() -> bool {
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            RUN_VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        )
        .is_ok()
    }
}

pub fn set_autostart(enable: bool) -> windows::core::Result<()> {
    unsafe {
        let mut key = HKEY::default();
        RegCreateKeyW(HKEY_CURRENT_USER, RUN_KEY, &mut key).ok()?;
        let result = if enable {
            let exe = std::env::current_exe().map_err(|_| windows::core::Error::from_thread())?;
            let value = wide(&format!("\"{}\"", exe.display()));
            let bytes = std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2);
            RegSetValueExW(key, RUN_VALUE, None, REG_SZ, Some(bytes)).ok()
        } else {
            let r = RegDeleteValueW(key, RUN_VALUE);
            if r == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                r.ok()
            }
        };
        let _ = RegCloseKey(key);
        result
    }
}

/// Opens an http(s) URL or a folder with the default handler.
pub fn open(target: &str) {
    let t = wide(target);
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(t.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}
