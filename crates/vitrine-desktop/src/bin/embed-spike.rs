//! Milestone 1 spike: one translucent "widget" window placed beneath the desktop
//! icons, with click forwarding that skips icons.
//!
//! Usage: `embed-spike [--seconds N]`
//!
//! What to check:
//! * the widget is drawn above the wallpaper and below the icons, with the
//!   wallpaper visible through its translucent fill;
//! * icons over the widget can be selected, opened and dragged;
//! * clicking an empty spot of the widget toggles its color (and adds a dot);
//! * after `taskkill /f /im explorer.exe` + restarting explorer, the widget comes back.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use vitrine_desktop::dcomp::CompositionSurface;
use vitrine_desktop::icons::{self, IconMap};
use vitrine_desktop::shell::{self, DesktopLayer};
use vitrine_desktop::surface::{LayeredSurface, premultiply};
use vitrine_desktop::util::class_name;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Shell::IFolderView;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const WM_WIDGET_CLICK: u32 = WM_APP + 1;
const TIMER_ICONS: usize = 1;
const TIMER_QUIT: usize = 2;
const TIMER_REATTACH: usize = 3;
const TIMER_RESTORE_DEFVIEW: usize = 4;

thread_local! {
    static HINST: Cell<HINSTANCE> = Cell::new(HINSTANCE::default());
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    // Read by the mouse hook: keep these cheap, never hold them across COM calls.
    static WIDGET: Cell<HWND> = Cell::new(HWND::default());
    static DEF_VIEW: Cell<HWND> = Cell::new(HWND::default());
    static WIDGET_RECT: Cell<RECT> = Cell::new(RECT::default());
    static ICONS: RefCell<IconMap> = RefCell::new(IconMap::default());
    // Owned by the UI thread.
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

struct App {
    surface: LayeredSurface,
    comp: Option<CompositionSurface>,
    clicks: u32,
    view: Option<IFolderView>,
    list_view: HWND,
}

fn main() -> windows::core::Result<()> {
    let seconds: Option<u32> = std::env::args()
        .skip_while(|a| a != "--seconds")
        .nth(1)
        .and_then(|s| s.parse().ok());

    unsafe {
        // Normally already set by the embedded manifest; ignore "access denied".
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        HINST.set(GetModuleHandleW(None)?.into());
        TASKBAR_CREATED.set(RegisterWindowMessageW(w!("TaskbarCreated")));

        register_class(w!("VitrineSpikeWidget"), widget_proc)?;
        register_class(w!("VitrineSpikeHost"), host_proc)?;

        // Hidden top-level window: receives the TaskbarCreated broadcast and timers.
        let host = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("VitrineSpikeHost"),
            w!("Vitrine spike host"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(HINST.get()),
            None,
        )?;

        attach()?;

        SetTimer(Some(host), TIMER_ICONS, 2000, None);
        if let Some(s) = seconds {
            SetTimer(Some(host), TIMER_QUIT, s * 1000, None);
        }
        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(HINST.get()), 0)?;
        println!(
            "mouse hook installed; running{}",
            seconds.map(|s| format!(" for {s}s")).unwrap_or_default()
        );

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWindowsHookEx(hook);

        // Release COM proxies and windows while COM is still alive: dropping them
        // from thread-local destructors during process teardown can deadlock.
        APP.with_borrow_mut(|app| *app = None);
        let widget = WIDGET.replace(HWND::default());
        if !widget.is_invalid() {
            let _ = DestroyWindow(widget);
        }
        let _ = DestroyWindow(host);
        windows::Win32::System::Com::CoUninitialize();
    }
    println!("bye");
    Ok(())
}

unsafe fn register_class(
    name: PCWSTR,
    proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
) -> windows::core::Result<()> {
    let wc = WNDCLASSW {
        lpfnWndProc: Some(proc),
        hInstance: HINST.get(),
        lpszClassName: name,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        ..Default::default()
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        return Err(windows::core::Error::from_thread());
    }
    Ok(())
}

/// Locates the desktop layer and creates the widget window in it.
fn attach() -> windows::core::Result<()> {
    let mut layer = shell::locate(Duration::from_secs(3)).map_err(|e| {
        eprintln!("locate failed: {e}");
        windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)
    })?;
    // Experiment: `--in-worker` parents the widget to the wallpaper WorkerW itself
    // (child of Progman on 24H2+) instead of making it a sibling below DefView.
    if std::env::args().any(|a| a == "--in-worker") && layer.kind == shell::LayerKind::Modern {
        let worker = unsafe { FindWindowExW(Some(layer.parent), None, w!("WorkerW"), None) }?;
        layer.parent = worker;
        layer.insert_after = None;
        println!("experiment: parenting to WorkerW {:#x}", worker.0 as usize);
    }
    // Control experiment: `--top` puts the widget above DefView (Vitrine 1 behaviour).
    if std::env::args().any(|a| a == "--top") {
        layer.insert_after = None;
        println!("experiment: above DefView");
    }
    println!(
        "layer: {:?} parent={} ({:#x})",
        layer.kind,
        class_name(layer.parent),
        layer.parent.0 as usize
    );

    let scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
    let mut work = RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work as *mut _ as *mut _),
            Default::default(),
        )?;
    }
    let (w, h) = ((420.0 * scale) as i32, (240.0 * scale) as i32);
    // `--at X,Y` places the widget's top-left corner at screen pixel X,Y.
    let at: Option<(i32, i32)> = std::env::args()
        .skip_while(|a| a != "--at")
        .nth(1)
        .and_then(|s| {
            s.split_once(',')
                .and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)))
        });
    let (left, top) = at.unwrap_or((
        work.left + (work.right - work.left) / 2 - w / 2,
        work.top + (120.0 * scale) as i32,
    ));
    let screen = RECT {
        left,
        top,
        right: 0,
        bottom: 0,
    };
    let screen = RECT {
        right: screen.left + w,
        bottom: screen.top + h,
        ..screen
    };
    let local = shell::screen_to_parent(layer.parent, screen);

    let widget = create_widget(&layer, local, screen, w, h)?;
    WIDGET.set(widget);
    WIDGET_RECT.set(screen);

    let mut surface = LayeredSurface::new(w, h)?;
    paint(&mut surface, false, 0, scale);
    let comp = if flag("--dcomp") {
        let comp = CompositionSurface::new(widget, w as u32, h as u32)?;
        comp.upload(surface.pixels_mut())?;
        println!("presented through DirectComposition");
        Some(comp)
    } else {
        if !flag("--opaque") && !flag("--slwa") {
            if flag("--ulw-pos") {
                surface.present_at(
                    widget,
                    Some(POINT {
                        x: local.left,
                        y: local.top,
                    }),
                )?;
            } else {
                surface.present(widget)?;
            }
        }
        None
    };

    let list_view = unsafe { FindWindowExW(Some(layer.def_view), None, w!("SysListView32"), None) }
        .unwrap_or_default();
    let view = icons::desktop_folder_view()
        .map_err(|e| eprintln!("IFolderView unavailable: {e}"))
        .ok();

    println!("children of parent (top first):");
    for line in shell::describe_children(layer.parent) {
        println!("  {line}");
    }

    // Diagnostic: `--probe-defview` hides the icon view for 3 s to see what is beneath it.
    if flag("--probe-defview") {
        unsafe {
            let _ = ShowWindow(layer.def_view, SW_HIDE);
            DEF_VIEW.set(layer.def_view);
            let host = FindWindowW(w!("VitrineSpikeHost"), None).unwrap_or_default();
            SetTimer(Some(host), TIMER_RESTORE_DEFVIEW, 3000, None);
        }
        println!("probe: DefView hidden for 3s");
    }

    APP.with_borrow_mut(|app| {
        *app = Some(App {
            surface,
            comp,
            clicks: 0,
            view,
            list_view,
        })
    });
    refresh_icons();
    Ok(())
}

fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

fn create_widget(
    layer: &DesktopLayer,
    local: RECT,
    screen: RECT,
    w: i32,
    h: i32,
) -> windows::core::Result<HWND> {
    unsafe {
        if flag("--toplevel") {
            // Control experiment: plain top-level layered popup (validates capture + drawing).
            return CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                w!("VitrineSpikeWidget"),
                w!("Vitrine spike widget"),
                WS_POPUP | WS_VISIBLE,
                screen.left,
                screen.top,
                w,
                h,
                None,
                None,
                Some(HINST.get()),
                None,
            );
        }
        if flag("--reparent") {
            // Create a layered popup first, then re-parent it into the desktop layer.
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("VitrineSpikeWidget"),
                w!("Vitrine spike widget"),
                WS_POPUP | WS_VISIBLE,
                screen.left,
                screen.top,
                w,
                h,
                None,
                None,
                Some(HINST.get()),
                None,
            )?;
            SetParent(hwnd, Some(layer.parent))?;
            let after = layer.insert_after.unwrap_or(HWND_TOP);
            SetWindowPos(
                hwnd,
                Some(after),
                local.left,
                local.top,
                w,
                h,
                SWP_NOACTIVATE,
            )?;
            return Ok(hwnd);
        }
        let mut ex = if flag("--opaque") {
            WS_EX_NOACTIVATE
        } else {
            WS_EX_LAYERED | WS_EX_NOACTIVATE
        };
        if flag("--noredir") {
            ex |= WS_EX_NOREDIRECTIONBITMAP;
        }
        let hwnd = CreateWindowExW(
            ex,
            w!("VitrineSpikeWidget"),
            w!("Vitrine spike widget"),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            local.left,
            local.top,
            w,
            h,
            Some(layer.parent),
            None,
            Some(HINST.get()),
            None,
        )?;
        if flag("--slwa") {
            // Layered via SetLayeredWindowAttributes (constant alpha), painted by WM_PAINT.
            SetLayeredWindowAttributes(
                hwnd,
                windows::Win32::Foundation::COLORREF(0),
                200,
                LWA_ALPHA,
            )?;
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, true);
        } else if flag("--dcomp") {
            // Layered with constant alpha 255 so DWM composes it; content comes from DComp.
            SetLayeredWindowAttributes(
                hwnd,
                windows::Win32::Foundation::COLORREF(0),
                255,
                LWA_ALPHA,
            )?;
        }
        let after = layer.insert_after.unwrap_or(HWND_TOP);
        SetWindowPos(
            hwnd,
            Some(after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )?;
        Ok(hwnd)
    }
}

fn refresh_icons() {
    // Take the COM object out so that no RefCell borrow is held while COM pumps messages.
    let (view, list_view) = APP.with_borrow_mut(|app| match app {
        Some(a) => (a.view.take(), a.list_view),
        None => (None, HWND::default()),
    });
    let Some(view) = view else { return };
    match icons::read_icon_map(&view, list_view) {
        Ok(map) => {
            let changed = ICONS.with_borrow(|old| old.rects.len() != map.rects.len());
            if changed {
                println!("icons: {} on desktop", map.rects.len());
                for r in &map.rects {
                    println!("  icon [{},{} - {},{}]", r.left, r.top, r.right, r.bottom);
                }
            }
            ICONS.with_borrow_mut(|m| *m = map);
        }
        Err(e) => eprintln!("icon refresh failed: {e}"),
    }
    APP.with_borrow_mut(|app| {
        if let Some(a) = app {
            a.view = Some(view);
        }
    });
}

/// Decides whether a pointer event at `pt` belongs to the widget.
fn route(pt: POINT) -> Option<HWND> {
    let r = WIDGET_RECT.get();
    if pt.x < r.left || pt.x >= r.right || pt.y < r.top || pt.y >= r.bottom {
        return None;
    }
    // Only when the desktop itself is under the pointer (no app window in front).
    let top = unsafe { WindowFromPoint(pt) };
    let class = class_name(top);
    if !matches!(
        class.as_str(),
        "SysListView32" | "SHELLDLL_DefView" | "WorkerW" | "Progman"
    ) {
        return None;
    }
    let on_icon = ICONS.with(|m| m.try_borrow().map(|m| m.hit(pt)).unwrap_or(true));
    if on_icon {
        return None;
    }
    Some(WIDGET.get())
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let msg = wparam.0 as u32;
        if matches!(
            msg,
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MOUSEWHEEL
        ) {
            if let Some(widget) = route(info.pt) {
                if msg == WM_LBUTTONDOWN {
                    unsafe {
                        let _ = PostMessageW(Some(widget), WM_WIDGET_CLICK, WPARAM(0), LPARAM(0));
                    }
                }
                return LRESULT(1); // swallow: the desktop must not start a selection here
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn widget_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_WIDGET_CLICK => {
            let scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
            APP.with_borrow_mut(|app| {
                if let Some(a) = app {
                    a.clicks += 1;
                    paint(&mut a.surface, a.clicks % 2 == 1, a.clicks, scale);
                    match &a.comp {
                        Some(c) => {
                            let _ = c.upload(a.surface.pixels_mut());
                        }
                        None => {
                            let _ = a.surface.present(hwnd);
                        }
                    }
                    println!("widget clicked ({} total)", a.clicks);
                }
            });
            LRESULT(0)
        }
        WM_PAINT => {
            // Only reached by the non-layered `--opaque` experiment.
            use windows::Win32::Graphics::Gdi::{
                BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, PAINTSTRUCT,
            };
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                if flag("--opaque") || flag("--slwa") {
                    let brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x0045_7AFF)); // BGR: orange
                    FillRect(hdc, &ps.rcPaint, brush);
                    let _ = DeleteObject(brush.into());
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            println!("widget window destroyed");
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe extern "system" fn host_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == TASKBAR_CREATED.get() && msg != 0 {
        println!("explorer restarted: re-attaching shortly");
        APP.with_borrow_mut(|app| *app = None);
        WIDGET.set(HWND::default());
        WIDGET_RECT.set(RECT::default());
        unsafe { SetTimer(Some(hwnd), TIMER_REATTACH, 1500, None) };
        return LRESULT(0);
    }
    match msg {
        WM_TIMER => {
            match wparam.0 {
                TIMER_ICONS => refresh_icons(),
                TIMER_QUIT => unsafe { PostQuitMessage(0) },
                TIMER_RESTORE_DEFVIEW => unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_RESTORE_DEFVIEW);
                    let _ = ShowWindow(DEF_VIEW.get(), SW_SHOWNA);
                    println!("probe: DefView restored");
                },
                TIMER_REATTACH => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_REATTACH);
                    }
                    if let Err(e) = attach() {
                        eprintln!("re-attach failed: {e}");
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Software-rasterized translucent rounded card, enough for the spike.
fn paint(surface: &mut LayeredSurface, toggled: bool, clicks: u32, scale: f32) {
    let (w, h) = (surface.width() as f32, surface.height() as f32);
    let radius = 24.0 * scale;
    let border = 2.0 * scale;
    let (from, to) = if toggled {
        ([255.0, 200.0, 87.0], [255.0, 122.0, 69.0])
    } else {
        ([61.0, 90.0, 254.0], [124.0, 58.0, 237.0])
    };
    let dot_r = 6.0 * scale;
    let width = surface.width() as usize;
    let px = surface.pixels_mut();
    for y in 0..h as usize {
        for x in 0..width {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = rounded_rect_sdf(fx, fy, w, h, radius);
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover <= 0.0 {
                px[y * width + x] = 0;
                continue;
            }
            let t = (fx / w + fy / h) * 0.5;
            let mut rgb = [0.0f32; 3];
            for i in 0..3 {
                rgb[i] = from[i] + (to[i] - from[i]) * t;
            }
            let mut alpha = 150.0;
            // light inner border
            let edge = (d + border + 0.5).clamp(0.0, 1.0);
            for c in &mut rgb {
                *c += (255.0 - *c) * edge * 0.8;
            }
            alpha += (230.0 - alpha) * edge;
            // one dot per click, top-left row
            for i in 0..clicks.min(12) {
                let cx = 24.0 * scale + i as f32 * 18.0 * scale;
                let cy = 24.0 * scale;
                let dd = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() - dot_r;
                let dot = (0.5 - dd).clamp(0.0, 1.0);
                for c in &mut rgb {
                    *c += (255.0 - *c) * dot;
                }
                alpha += (255.0 - alpha) * dot;
            }
            let a = (alpha * cover).round() as u8;
            px[y * width + x] = premultiply(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8, a);
        }
    }
}

fn rounded_rect_sdf(x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let qx = (x - w / 2.0).abs() - (w / 2.0 - r);
    let qy = (y - h / 2.0).abs() - (h / 2.0 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}
