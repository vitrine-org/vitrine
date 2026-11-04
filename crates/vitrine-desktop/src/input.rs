//! Pointer input for widgets that live beneath the desktop icons.
//!
//! A `WH_MOUSE_LL` hook on a dedicated thread (so slow scripts on the main
//! thread can never stall system input) watches the pointer. When the desktop
//! itself is under the pointer, the point lies in an interactive widget and not
//! on an icon, button and wheel events are swallowed and posted to the app's
//! window as [`PointerEvent`]s; moves are passed through and reported for hover.

use std::sync::{Arc, OnceLock, RwLock};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::icons::IconMap;
use crate::util::class_name;

/// Message posted to the notify window. `wparam` = packed [`PointerKind`],
/// target key and wheel delta; `lparam` = packed screen point. Decode with
/// [`PointerEvent::decode`].
pub const WM_VITRINE_POINTER: u32 = WM_APP + 0x31;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PointerKind {
    Move = 0,
    Down = 1,
    Up = 2,
    Wheel = 3,
    /// The pointer left every interactive widget.
    Leave = 4,
    RightDown = 5,
    RightUp = 6,
}

#[derive(Clone, Copy, Debug)]
pub struct PointerEvent {
    pub kind: PointerKind,
    /// Key of the [`Target`] under the pointer.
    pub key: u32,
    pub pt: POINT,
    /// Wheel delta in notches × 120 (positive = away from the user).
    pub wheel: i16,
}

impl PointerEvent {
    pub fn encode(&self) -> (WPARAM, LPARAM) {
        let w = (self.kind as usize)
            | ((self.key as usize & 0xFF_FFFF) << 8)
            | ((self.wheel as u16 as usize) << 32);
        let l = ((self.pt.x as u32 as u64) << 32 | self.pt.y as u32 as u64) as isize;
        (WPARAM(w), LPARAM(l))
    }

    pub fn decode(wparam: WPARAM, lparam: LPARAM) -> PointerEvent {
        let w = wparam.0;
        let kind = match w & 0xFF {
            1 => PointerKind::Down,
            2 => PointerKind::Up,
            3 => PointerKind::Wheel,
            4 => PointerKind::Leave,
            5 => PointerKind::RightDown,
            6 => PointerKind::RightUp,
            _ => PointerKind::Move,
        };
        let l = lparam.0 as u64;
        PointerEvent {
            kind,
            key: ((w >> 8) & 0xFF_FFFF) as u32,
            pt: POINT {
                x: (l >> 32) as u32 as i32,
                y: l as u32 as i32,
            },
            wheel: (w >> 32) as u16 as i16,
        }
    }
}

/// An interactive widget area in screen pixels.
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub key: u32,
    pub rect: RECT,
}

#[derive(Default)]
pub struct RoutingState {
    pub targets: Vec<Target>,
    pub icons: IconMap,
    /// Set while edit mode or a menu owns the pointer.
    pub suspended: bool,
}

struct Shared {
    state: Arc<RwLock<RoutingState>>,
    notify: isize,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

thread_local! {
    static HOVER: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    static CAPTURE: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Starts the hook thread. Only one router can exist per process.
pub fn start(state: Arc<RwLock<RoutingState>>, notify: HWND) -> bool {
    if SHARED
        .set(Shared {
            state,
            notify: notify.0 as isize,
        })
        .is_err()
    {
        return false;
    }
    std::thread::Builder::new()
        .name("vitrine-input".into())
        .spawn(|| unsafe {
            // Input must never wait: highest priority, and no EcoQoS throttling.
            use windows::Win32::System::Threading::*;
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
            let state = THREAD_POWER_THROTTLING_STATE {
                Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
                StateMask: 0,
            };
            let _ = SetThreadInformation(
                GetCurrentThread(),
                ThreadPowerThrottling,
                &state as *const _ as *const _,
                std::mem::size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
            );
            let module = GetModuleHandleW(None).ok();
            let hook =
                match SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), module.map(Into::into), 0) {
                    Ok(h) => h,
                    Err(e) => {
                        log::error!("mouse hook failed: {e}");
                        return;
                    }
                };
            log::debug!("input hook installed");
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
            let _ = UnhookWindowsHookEx(hook);
        })
        .is_ok()
}

fn contains(r: &RECT, pt: POINT) -> bool {
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

/// The interactive widget that should receive input at `pt`, if any.
fn route(shared: &Shared, pt: POINT) -> Option<u32> {
    let state = shared.state.try_read().ok()?;
    if state.suspended {
        return None;
    }
    let key = state.targets.iter().find(|t| contains(&t.rect, pt))?.key;
    if state.icons.hit(pt) {
        return None;
    }
    drop(state);
    // Only when the desktop itself is under the pointer (no app window in front).
    let top = unsafe { WindowFromPoint(pt) };
    matches!(
        class_name(top).as_str(),
        "SysListView32" | "SHELLDLL_DefView" | "WorkerW" | "Progman"
    )
    .then_some(key)
}

fn post(shared: &Shared, ev: PointerEvent) {
    let (w, l) = ev.encode();
    unsafe {
        let _ = PostMessageW(
            Some(HWND(shared.notify as *mut _)),
            WM_VITRINE_POINTER,
            w,
            l,
        );
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && let Some(shared) = SHARED.get()
    {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let msg = wparam.0 as u32;
        let pt = info.pt;
        let ev = |kind, key, wheel| PointerEvent {
            kind,
            key,
            pt,
            wheel,
        };
        match msg {
            WM_MOUSEMOVE => {
                let target = CAPTURE.get().or_else(|| route(shared, pt));
                match (HOVER.get(), target) {
                    (_, Some(key)) => post(shared, ev(PointerKind::Move, key, 0)),
                    (Some(old), None) => post(shared, ev(PointerKind::Leave, old, 0)),
                    (None, None) => {}
                }
                HOVER.set(target);
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                if let Some(key) = route(shared, pt) {
                    CAPTURE.set(Some(key));
                    let kind = if msg == WM_LBUTTONDOWN {
                        PointerKind::Down
                    } else {
                        PointerKind::RightDown
                    };
                    post(shared, ev(kind, key, 0));
                    return LRESULT(1); // the desktop must not start a selection here
                }
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                if let Some(key) = CAPTURE.take() {
                    let kind = if msg == WM_LBUTTONUP {
                        PointerKind::Up
                    } else {
                        PointerKind::RightUp
                    };
                    post(shared, ev(kind, key, 0));
                    return LRESULT(1);
                }
            }
            WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK => {
                if route(shared, pt).is_some() {
                    return LRESULT(1);
                }
            }
            WM_MOUSEWHEEL => {
                if let Some(key) = route(shared, pt) {
                    let delta = (info.mouseData >> 16) as u16 as i16;
                    post(shared, ev(PointerKind::Wheel, key, delta));
                    return LRESULT(1);
                }
            }
            _ => {}
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_event_round_trips() {
        let ev = PointerEvent {
            kind: PointerKind::Wheel,
            key: 4242,
            pt: POINT { x: -1920, y: 1079 },
            wheel: -120,
        };
        let (w, l) = ev.encode();
        let back = PointerEvent::decode(w, l);
        assert_eq!(back.kind, PointerKind::Wheel);
        assert_eq!(back.key, 4242);
        assert_eq!((back.pt.x, back.pt.y), (-1920, 1079));
        assert_eq!(back.wheel, -120);
    }

    #[test]
    fn every_kind_and_extreme_values_round_trip() {
        use PointerKind::*;
        for kind in [Move, Down, Up, Wheel, Leave, RightDown, RightUp] {
            for (x, y) in [(0, 0), (i32::MIN, i32::MAX), (-1, -1), (7680, -2160)] {
                for wheel in [0, 1, -1, i16::MAX, i16::MIN] {
                    for key in [0, 1, 0xFF_FFFF] {
                        let ev = PointerEvent {
                            kind,
                            key,
                            pt: POINT { x, y },
                            wheel,
                        };
                        let (w, l) = ev.encode();
                        let back = PointerEvent::decode(w, l);
                        assert_eq!(back.kind, kind);
                        assert_eq!(back.key, key);
                        assert_eq!((back.pt.x, back.pt.y), (x, y));
                        assert_eq!(back.wheel, wheel);
                    }
                }
            }
        }
    }

    #[test]
    fn keys_keep_their_low_24_bits() {
        let ev = PointerEvent {
            kind: PointerKind::Down,
            key: 0x1234_5678,
            pt: POINT { x: 1, y: 2 },
            wheel: 0,
        };
        let (w, l) = ev.encode();
        let back = PointerEvent::decode(w, l);
        assert_eq!(back.key, 0x34_5678);
        assert_eq!(back.kind, PointerKind::Down);
    }

    #[test]
    fn unknown_kinds_decode_as_move() {
        let back = PointerEvent::decode(WPARAM(0xFF), LPARAM(0));
        assert_eq!(back.kind, PointerKind::Move);
    }

    #[test]
    fn contains_is_half_open() {
        let r = RECT {
            left: 0,
            top: 0,
            right: 10,
            bottom: 10,
        };
        assert!(contains(&r, POINT { x: 0, y: 0 }));
        assert!(contains(&r, POINT { x: 9, y: 9 }));
        assert!(!contains(&r, POINT { x: 10, y: 5 }));
        assert!(!contains(&r, POINT { x: 5, y: -1 }));
    }
}
