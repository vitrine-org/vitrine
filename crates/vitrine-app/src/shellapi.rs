//! Small Win32 shell helpers: tray icon, popup menus, autostart, opening URLs.

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Controls::{LIM_SMALL, LoadIconMetric};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCSTR, PCWSTR, w};

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
#[derive(Clone, Debug, PartialEq)]
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

/// Makes the process's popup menus follow the light/dark app theme.
///
/// Uses the undocumented but stable uxtheme exports that Explorer and the
/// inbox apps use (`SetPreferredAppMode` #135, `FlushMenuThemes` #136).
/// Does nothing if they are missing.
pub fn set_menu_theme(dark: bool) -> bool {
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    unsafe {
        let Ok(uxtheme) = LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
        else {
            return false;
        };
        let Some(set_mode) = GetProcAddress(uxtheme, PCSTR(135 as _)) else {
            return false;
        };
        // PreferredAppMode: 2 = ForceDark, 3 = ForceLight.
        let set_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
        set_mode(if dark { 2 } else { 3 });
        if let Some(flush) = GetProcAddress(uxtheme, PCSTR(136 as _)) {
            let flush: extern "system" fn() = std::mem::transmute(flush);
            flush();
        }
        true
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

/// The `Run` value that starts `exe` at logon (quoted: the path may contain spaces).
fn run_command(exe: &std::path::Path) -> String {
    format!("\"{}\"", exe.display())
}

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
            let value = wide(&run_command(&exe));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_is_nul_terminated_utf16() {
        assert_eq!(wide(""), vec![0]);
        assert_eq!(wide("Ab"), vec![b'A' as u16, b'b' as u16, 0]);
        // Non-BMP characters take two code units.
        assert_eq!(wide("é😀").len(), 1 + 2 + 1);
        assert_eq!(*wide("é😀").last().unwrap(), 0);
    }

    #[test]
    fn copy_tip_fits_and_terminates() {
        let mut buf = [0xFFFFu16; 8];
        copy_tip(&mut buf, "abc");
        assert_eq!(&buf[..4], &[b'a' as u16, b'b' as u16, b'c' as u16, 0]);
        // The rest is left untouched.
        assert_eq!(buf[4], 0xFFFF);
    }

    #[test]
    fn copy_tip_truncates_long_text() {
        let mut buf = [0xFFFFu16; 4];
        copy_tip(&mut buf, "abcdefgh");
        assert_eq!(buf, [b'a' as u16, b'b' as u16, b'c' as u16, 0]);
        // Exactly full: the terminator replaces the last character.
        let mut buf = [0xFFFFu16; 3];
        copy_tip(&mut buf, "abc");
        assert_eq!(buf, [b'a' as u16, b'b' as u16, 0]);
    }

    #[test]
    fn copy_tip_into_real_tip_buffer() {
        let mut data = NOTIFYICONDATAW::default();
        let long = "x".repeat(500);
        copy_tip(&mut data.szTip, &long);
        assert_eq!(data.szTip[data.szTip.len() - 1], 0);
        assert_eq!(data.szTip[0], b'x' as u16);
        copy_tip(&mut data.szTip, "");
        assert_eq!(data.szTip[0], 0);
        // An empty buffer is a no-op, not a panic.
        copy_tip(&mut [], "abc");
    }

    #[test]
    fn item_constructors() {
        assert_eq!(
            Item::action(4, "Open"),
            Item::Action {
                id: 4,
                label: "Open".into(),
                checked: false,
                enabled: true
            }
        );
        assert_eq!(
            Item::check(2, "Pause", true),
            Item::Action {
                id: 2,
                label: "Pause".into(),
                checked: true,
                enabled: true
            }
        );
        assert_eq!(
            Item::disabled("None"),
            Item::Action {
                id: 0,
                label: "None".into(),
                checked: false,
                enabled: false
            }
        );
    }

    struct Info {
        id: u32,
        text: String,
        checked: bool,
        grayed: bool,
        separator: bool,
        sub: Option<HMENU>,
    }

    fn info(menu: HMENU, pos: u32) -> Info {
        let mut buf = [0u16; 128];
        let mut mii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_ID | MIIM_STATE | MIIM_FTYPE | MIIM_SUBMENU | MIIM_STRING,
            dwTypeData: windows::core::PWSTR(buf.as_mut_ptr()),
            cch: buf.len() as u32,
            ..Default::default()
        };
        unsafe { GetMenuItemInfoW(menu, pos, true, &mut mii) }.unwrap();
        Info {
            id: mii.wID,
            text: String::from_utf16_lossy(&buf[..mii.cch as usize]),
            checked: (mii.fState & MFS_CHECKED).0 != 0,
            grayed: (mii.fState & MFS_GRAYED).0 != 0,
            separator: (mii.fType & MFT_SEPARATOR).0 != 0,
            sub: (!mii.hSubMenu.is_invalid()).then_some(mii.hSubMenu),
        }
    }

    fn count(menu: HMENU) -> i32 {
        unsafe { GetMenuItemCount(Some(menu)) }
    }

    #[test]
    fn build_creates_real_menus() {
        let items = vec![
            Item::action(9, "Add widgets…"),
            Item::Sub {
                label: "Quick add".into(),
                items: vec![
                    Item::action(1000, "Clock"),
                    Item::Separator,
                    Item::action(1001, "Weather"),
                ],
            },
            Item::check(1, "Edit layout", true),
            Item::check(2, "Pause widgets", false),
            Item::Separator,
            Item::disabled("Nothing here"),
        ];
        let menu = build(&items);
        assert!(!menu.is_invalid());
        assert_eq!(count(menu), 6);

        let a = info(menu, 0);
        assert_eq!((a.id, a.text.as_str()), (9, "Add widgets…"));
        assert!(!a.checked && !a.grayed && !a.separator && a.sub.is_none());

        let sub = info(menu, 1);
        assert_eq!(sub.text, "Quick add");
        let sub_menu = sub.sub.expect("submenu");
        assert_eq!(count(sub_menu), 3);
        assert_eq!(info(sub_menu, 0).id, 1000);
        assert_eq!(info(sub_menu, 0).text, "Clock");
        assert!(info(sub_menu, 1).separator);
        assert_eq!(info(sub_menu, 2).id, 1001);

        let edit = info(menu, 2);
        assert_eq!(edit.id, 1);
        assert!(edit.checked && !edit.grayed);
        let pause = info(menu, 3);
        assert_eq!(pause.id, 2);
        assert!(!pause.checked);

        assert!(info(menu, 4).separator);

        let none = info(menu, 5);
        assert_eq!((none.id, none.text.as_str()), (0, "Nothing here"));
        assert!(none.grayed && !none.checked);

        // Destroying the parent destroys the submenu too.
        unsafe { DestroyMenu(menu) }.unwrap();
        assert_eq!(count(sub_menu), -1);
    }

    #[test]
    fn build_empty_and_nested() {
        let menu = build(&[]);
        assert_eq!(count(menu), 0);
        unsafe { DestroyMenu(menu) }.unwrap();

        let menu = build(&[Item::Sub {
            label: "a".into(),
            items: vec![Item::Sub {
                label: "b".into(),
                items: vec![Item::action(77, "c")],
            }],
        }]);
        let a = info(menu, 0).sub.unwrap();
        let b = info(a, 0).sub.unwrap();
        assert_eq!(info(a, 0).text, "b");
        assert_eq!(info(b, 0).id, 77);
        unsafe { DestroyMenu(menu) }.unwrap();
    }

    #[test]
    fn run_command_is_quoted() {
        let p = std::path::Path::new(r"C:\Program Files\Vitrine\vitrine.exe");
        assert_eq!(
            run_command(p),
            "\"C:\\Program Files\\Vitrine\\vitrine.exe\""
        );
        let p = std::path::Path::new(r"C:\Users\Zoë Ünïcode\vitrine.exe");
        let cmd = run_command(p);
        assert!(cmd.starts_with('"') && cmd.ends_with('"'));
        assert_eq!(&cmd[1..cmd.len() - 1], p.to_str().unwrap());
        // The registry value is the NUL-terminated UTF-16 of that string.
        assert_eq!(wide(&cmd).len(), cmd.encode_utf16().count() + 1);
    }

    #[test]
    fn menu_theme_is_available() {
        assert!(set_menu_theme(true));
        assert!(set_menu_theme(false));
    }
}
