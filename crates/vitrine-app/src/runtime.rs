//! The live application: widget windows beneath the desktop icons, the script
//! and render loop, the tray icon, power-aware pausing and Explorer restarts.
//!
//! Everything runs on the main (STA) thread except the input hook, the system
//! metrics collector, the icon-geometry poller and network requests, which
//! report back through a channel and wake the loop with `WM_APP_WAKE`.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use vitrine_desktop::input::{
    self, PointerEvent, PointerKind, RoutingState, Target, WM_VITRINE_POINTER,
};
use vitrine_desktop::monitors::{self, Monitor};
use vitrine_desktop::shell::{self, DesktopLayer};
use vitrine_desktop::window;
use vitrine_render::compose::WindowVisual;
use vitrine_render::gpu::Gpu;
use vitrine_render::scene::NodeId;
use vitrine_render::style::Transform2D;
use vitrine_system::{Collector, Metric};
use vitrine_widgets::config::Config;
use vitrine_widgets::layout::{Instance, Layout};
use vitrine_widgets::package::{self, InstalledPackage};
use vitrine_widgets::paths;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
};
use windows::Win32::UI::Shell::{QUERY_USER_NOTIFICATION_STATE, SHQueryUserNotificationState};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::edit::EditMode;
use crate::grid::Geometry;
use crate::instance::{Effect, WidgetInstance};
use crate::shellapi::{self, Item, Tray};

pub const HOST_CLASS: PCWSTR = w!("VitrineHost");
const WIDGET_CLASS: PCWSTR = w!("VitrineWidget");
/// Broadcast-free reload request: posted to the host window by `vitrine install`
/// and the control panel after they change packages, layout or settings.
pub const RELOAD_MESSAGE: PCWSTR = w!("Vitrine.Reload");
/// Runs a menu command (`wparam` = command id) in the running instance.
pub const COMMAND_MESSAGE: PCWSTR = w!("Vitrine.Command");
const HOTKEY_EDIT: i32 = 1;

const WM_APP_WAKE: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const TIMER_REATTACH: usize = 1;
const TIMER_POWER: usize = 2;
const TIMER_RELAYOUT: usize = 3;

const WTS_SESSION_LOCK: u32 = 0x7;
const WTS_SESSION_UNLOCK: u32 = 0x8;

/// Messages queued by the window procedures and handled by [`Runtime::tick`]
/// (so that no procedure ever needs to borrow the runtime).
#[derive(Debug)]
pub enum HostMsg {
    Pointer(PointerEvent),
    Tray(u32, POINT),
    Timer(usize),
    TaskbarCreated,
    DisplayChanged,
    Reload,
    Command(u32),
    SessionLocked(bool),
    DisplayOn(bool),
    Edit(crate::edit::Input),
}

enum Event {
    Fetch { instance: String, payload: String },
    System(Value),
}

thread_local! {
    static INBOX: RefCell<VecDeque<HostMsg>> = const { RefCell::new(VecDeque::new()) };
    static MESSAGES: RefCell<(u32, u32, u32)> = const { RefCell::new((0, 0, 0)) };
}

fn push(msg: HostMsg) {
    INBOX.with_borrow_mut(|q| q.push_back(msg));
}

/// Queues a message for the next tick (used by other window procedures).
pub(crate) fn push_host(msg: HostMsg) {
    push(msg);
}

fn contains(r: &RECT, pt: POINT) -> bool {
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

/// A widget instance placed on the desktop.
struct Live {
    entry: Instance,
    key: u32,
    inst: WidgetInstance,
    window: Option<(HWND, WindowVisual)>,
    screen: RECT,
    scale: f32,
    force_paint: bool,
    hover: Option<NodeId>,
    pressed: Option<NodeId>,
    storage: Map<String, Value>,
    /// Package version the instance was created from (to detect upgrades).
    version: String,
}

pub struct Runtime {
    gpu: Gpu,
    dc: ID2D1DeviceContext,
    hinst: HINSTANCE,
    host: HWND,
    tray: Option<Tray>,
    layer: Option<DesktopLayer>,
    monitors: Vec<Monitor>,
    packages: Vec<InstalledPackage>,
    pub(crate) layout: Layout,
    config: Config,
    widgets: Vec<Live>,
    next_key: u32,
    collector: Collector,
    routing: Arc<RwLock<RoutingState>>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    locked: bool,
    display_off: bool,
    fullscreen: bool,
    user_paused: bool,
    edit: Option<EditMode>,
    quit: bool,
}

fn storage_path(instance: &str) -> PathBuf {
    paths::data_dir()
        .join("storage")
        .join(format!("{instance}.json"))
}

fn rect_size(r: &RECT) -> (u32, u32) {
    (
        (r.right - r.left).max(1) as u32,
        (r.bottom - r.top).max(1) as u32,
    )
}

fn wake(host: isize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(host as *mut _)),
            WM_APP_WAKE,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

impl Runtime {
    fn new(hinst: HINSTANCE, host: HWND) -> Result<Runtime, String> {
        let gpu = Gpu::new(true).map_err(|e| format!("graphics: {e}"))?;
        let dc = gpu.new_context().map_err(|e| format!("graphics: {e}"))?;
        let (tx, rx) = channel();
        let host_id = host.0 as isize;
        let collector = {
            let tx = tx.clone();
            Collector::start(move |snapshot| {
                if tx.send(Event::System(snapshot)).is_ok() {
                    wake(host_id);
                }
            })
        };
        let routing = Arc::new(RwLock::new(RoutingState::default()));
        Ok(Runtime {
            gpu,
            dc,
            hinst,
            host,
            tray: None,
            layer: None,
            monitors: monitors::monitors(),
            packages: Vec::new(),
            layout: Layout::default(),
            config: Config::load(&paths::config_path()).unwrap_or_default(),
            widgets: Vec::new(),
            next_key: 1,
            collector,
            routing,
            tx,
            rx,
            locked: false,
            display_off: false,
            fullscreen: false,
            user_paused: false,
            edit: None,
            quit: false,
        })
    }

    fn paused(&self) -> bool {
        self.locked || self.display_off || self.fullscreen || self.user_paused
    }

    // ── packages and layout ───────────────────────────────────────────

    /// Installs packs shipped next to the executable (`packs\*.vitrine`) once per file.
    fn install_bundled(&self) {
        let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("packs")))
        else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let marker = paths::packages_dir().join(".bundled.json");
        let mut seen: Map<String, Value> = std::fs::read_to_string(&marker)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut changed = false;
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "vitrine") {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            let stamp = e.metadata().map(|m| m.len()).unwrap_or(0);
            if seen.get(&name).and_then(Value::as_u64) == Some(stamp) {
                continue;
            }
            match package::install(&path, &paths::packages_dir()) {
                Ok(p) => log::info!(
                    "installed bundled pack {} {}",
                    p.manifest.id,
                    p.manifest.version
                ),
                Err(e) => log::error!("bundled pack {name}: {e}"),
            }
            seen.insert(name, stamp.into());
            changed = true;
        }
        if changed {
            let _ = std::fs::write(&marker, Value::Object(seen).to_string());
        }
    }

    fn package(&self, id: &str) -> Option<&InstalledPackage> {
        self.packages.iter().find(|p| p.manifest.id == id)
    }

    fn geometry(&self, monitor: &str) -> Option<Geometry> {
        monitors::resolve(&self.monitors, monitor)
            .map(|m| Geometry::for_monitor(&self.layout.grid, m))
    }

    fn primary_geometry(&self) -> Option<Geometry> {
        self.geometry("primary")
    }

    /// First run: a clock and a system monitor in the top-right corner.
    fn seed_layout(&mut self) {
        let Some(g) = self.primary_geometry() else {
            return;
        };
        let Some(pkg) = self.package("dev.vitrine.essentials").cloned() else {
            return;
        };
        let mut y = 0;
        for wid in ["clock", "system"] {
            if let Some(w) = pkg.widget(wid) {
                let [cw, ch] = w.size.default;
                let (cw, ch) = (cw as i32, ch as i32);
                self.layout.instances.push(Instance {
                    id: vitrine_widgets::layout::new_instance_id(),
                    package: pkg.manifest.id.clone(),
                    widget: wid.into(),
                    monitor: "primary".into(),
                    x: (g.cols - cw).max(0),
                    y,
                    w: cw,
                    h: ch,
                    settings: Map::new(),
                });
                y += ch;
            }
        }
    }

    pub(crate) fn save_layout(&self) {
        if let Err(e) = self.layout.save(&paths::layout_path()) {
            log::error!("saving layout: {e}");
        }
    }

    /// Re-reads packages and layout from disk and reconciles running widgets.
    fn reload(&mut self) {
        self.packages = package::scan(&paths::packages_dir());
        let first_run = !paths::layout_path().exists();
        self.layout = Layout::load(&paths::layout_path()).unwrap_or_else(|e| {
            log::error!("layout.json: {e}; starting empty");
            Layout::default()
        });
        if first_run {
            self.seed_layout();
            self.save_layout();
        }
        self.config = Config::load(&paths::config_path()).unwrap_or_default();

        let wanted: Vec<Instance> = self.layout.instances.clone();
        // Drop widgets that disappeared, whose package changed, or failed to load.
        let mut i = 0;
        while i < self.widgets.len() {
            let live = &self.widgets[i];
            let keep = wanted.iter().any(|e| {
                e.id == live.entry.id
                    && e.package == live.entry.package
                    && e.widget == live.entry.widget
            }) && self
                .package(&live.entry.package)
                .is_some_and(|p| p.manifest.version == live.version);
            if keep {
                i += 1;
            } else {
                let mut live = self.widgets.remove(i);
                Self::destroy_window(&mut live);
            }
        }
        for entry in wanted {
            if let Some(live) = self.widgets.iter_mut().find(|l| l.entry.id == entry.id) {
                if live.entry.settings != entry.settings {
                    let settings = Self::settings_for(
                        self.packages
                            .iter()
                            .find(|p| p.manifest.id == entry.package),
                        &entry,
                    );
                    live.inst.set_settings(&settings);
                }
                live.entry = entry;
            } else {
                self.spawn(entry);
            }
        }
        self.relayout();
        self.update_plan();
    }

    fn settings_for(pkg: Option<&InstalledPackage>, entry: &Instance) -> String {
        let mut settings = pkg
            .and_then(|p| {
                p.widget(&entry.widget)
                    .and_then(|w| p.read_settings_schema(w).ok().flatten())
            })
            .map(|s| s.defaults())
            .unwrap_or_default();
        settings.extend(entry.settings.clone());
        Value::Object(settings).to_string()
    }

    fn spawn(&mut self, entry: Instance) {
        let Some(pkg) = self.package(&entry.package) else {
            log::warn!("widget {} uses missing package {}", entry.id, entry.package);
            return;
        };
        let Some(g) = self.geometry(&entry.monitor) else {
            return;
        };
        let storage: Map<String, Value> = std::fs::read_to_string(storage_path(&entry.id))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let settings = Self::settings_for(Some(pkg), &entry);
        let size = g.dips(entry.w, entry.h);
        let version = pkg.manifest.version.clone();
        match WidgetInstance::new(
            &self.gpu,
            pkg,
            &entry.widget,
            &entry.id,
            &settings,
            &Value::Object(storage.clone()).to_string(),
            size,
        ) {
            Ok(inst) => {
                let key = self.next_key;
                self.next_key += 1;
                log::info!("started {}/{} ({})", entry.package, entry.widget, entry.id);
                self.widgets.push(Live {
                    entry,
                    key,
                    inst,
                    window: None,
                    screen: RECT::default(),
                    scale: g.scale,
                    force_paint: true,
                    hover: None,
                    pressed: None,
                    storage,
                    version,
                });
            }
            Err(e) => log::error!("widget {}: {e}", entry.id),
        }
    }

    fn destroy_window(live: &mut Live) {
        if let Some((hwnd, mut visual)) = live.window.take() {
            let _ = visual.release();
            drop(visual);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
    }

    /// Positions every widget window from the layout (creating windows when attached).
    pub(crate) fn relayout(&mut self) {
        self.monitors = monitors::monitors();
        let grid = self.layout.grid;
        for live in &mut self.widgets {
            let Some(m) = monitors::resolve(&self.monitors, &live.entry.monitor) else {
                continue;
            };
            let g = Geometry::for_monitor(&grid, m);
            let (x, y, w, h) = g.clamp(live.entry.x, live.entry.y, live.entry.w, live.entry.h);
            let screen = g.rect(x, y, w, h);
            let size = g.dips(w, h);
            if live.inst.renderer.size() != size {
                live.inst.set_size(size.0, size.1);
            }
            let moved = screen != live.screen || g.scale != live.scale;
            live.screen = screen;
            live.scale = g.scale;
            let Some(layer) = &self.layer else { continue };
            match &live.window {
                Some((hwnd, _)) if moved => {
                    let _ = window::place(layer, *hwnd, screen);
                    live.force_paint = true;
                }
                Some(_) => {}
                None => match window::create_widget_window(layer, WIDGET_CLASS, self.hinst, screen)
                {
                    Ok(hwnd) => match WindowVisual::new(&self.gpu, hwnd) {
                        Ok(v) => {
                            live.window = Some((hwnd, v));
                            live.force_paint = true;
                        }
                        Err(e) => {
                            log::error!("composition for {}: {e}", live.entry.id);
                            unsafe {
                                let _ = DestroyWindow(hwnd);
                            }
                        }
                    },
                    Err(e) => log::error!("window for {}: {e}", live.entry.id),
                },
            }
        }
        self.update_routing();
    }

    pub(crate) fn update_routing(&self) {
        if let Ok(mut r) = self.routing.write() {
            r.targets = self
                .widgets
                .iter()
                .filter(|l| {
                    l.inst.widget.interactive && l.window.is_some() && l.inst.failed.is_none()
                })
                .map(|l| Target {
                    key: l.key,
                    rect: l.screen,
                })
                .collect();
            r.suspended = self.edit.is_some();
        }
    }

    fn update_plan(&self) {
        let mut plan: HashMap<Metric, u32> = HashMap::new();
        for live in &self.widgets {
            if let Some((metrics, interval)) = &live.inst.subscription {
                for m in metrics.iter().filter_map(|m| m.parse::<Metric>().ok()) {
                    let e = plan.entry(m).or_insert(*interval);
                    *e = (*e).min(*interval);
                }
            }
        }
        self.collector.set_plan(plan.into_iter().collect());
    }

    // ── desktop attachment ────────────────────────────────────────────

    fn attach(&mut self) {
        match shell::locate(Duration::from_secs(3)) {
            Ok(layer) => {
                log::info!("attached to desktop ({:?})", layer.kind);
                self.layer = Some(layer);
                self.relayout();
            }
            Err(e) => {
                log::error!("desktop not found: {e}; retrying");
                unsafe {
                    SetTimer(Some(self.host), TIMER_REATTACH, 3000, None);
                }
            }
        }
    }

    fn detach(&mut self) {
        for live in &mut self.widgets {
            Self::destroy_window(live);
            live.hover = None;
            live.pressed = None;
        }
        self.layer = None;
        self.update_routing();
    }

    // ── loop ──────────────────────────────────────────────────────────

    /// Handles queued work, runs scripts and paints. Returns how long the loop may sleep.
    fn tick(&mut self) -> u32 {
        while let Some(msg) = INBOX.with_borrow_mut(|q| q.pop_front()) {
            self.handle(msg);
            if self.quit {
                return 0;
            }
        }
        let mut plan_changed = false;
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Fetch { instance, payload } => {
                    if let Some(l) = self.widgets.iter_mut().find(|l| l.entry.id == instance) {
                        l.inst.deliver("fetch", &payload);
                    }
                }
                Event::System(snapshot) => {
                    let Value::Object(all) = snapshot else {
                        continue;
                    };
                    for l in &mut self.widgets {
                        let Some((metrics, _)) = &l.inst.subscription else {
                            continue;
                        };
                        let part: Map<String, Value> = all
                            .iter()
                            .filter(|(k, _)| metrics.contains(k))
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect();
                        if !part.is_empty() {
                            l.inst.deliver("system", &Value::Object(part).to_string());
                        }
                    }
                }
            }
        }

        let paused = self.paused();
        let mut timeout = u32::MAX;
        let host_id = self.host.0 as isize;
        for i in 0..self.widgets.len() {
            let live = &mut self.widgets[i];
            if !paused {
                live.inst.fire_timers();
            }
            for effect in live.inst.pump() {
                match effect {
                    Effect::Subscribe { .. } => plan_changed = true,
                    Effect::Fetch {
                        id,
                        url,
                        method,
                        headers,
                        body,
                    } => {
                        let tx = self.tx.clone();
                        let instance = live.entry.id.clone();
                        std::thread::spawn(move || {
                            let payload =
                                crate::net::perform(id, &method, &url, &headers, body.as_deref())
                                    .to_string();
                            if tx.send(Event::Fetch { instance, payload }).is_ok() {
                                wake(host_id);
                            }
                        });
                    }
                    Effect::StorageSet { key, value } => {
                        match value {
                            Some(v) => live.storage.insert(key, Value::String(v)),
                            None => live.storage.remove(&key),
                        };
                        let path = storage_path(&live.entry.id);
                        let _ = std::fs::create_dir_all(path.parent().unwrap());
                        if let Err(e) =
                            std::fs::write(&path, Value::Object(live.storage.clone()).to_string())
                        {
                            log::warn!("storage for {}: {e}", live.entry.id);
                        }
                    }
                    Effect::OpenUrl(url) => {
                        if url.starts_with("https://") || url.starts_with("http://") {
                            shellapi::open(&url);
                        }
                    }
                }
            }
            if paused && !live.force_paint {
                continue;
            }
            let now = live.inst.now();
            if let Some((_, visual)) = &mut live.window
                && (live.force_paint || live.inst.renderer.needs_paint(now))
            {
                let (pw, ph) = rect_size(&live.screen);
                let scale = live.scale;
                let gpu = &self.gpu;
                let renderer = &mut live.inst.renderer;
                let result = visual.draw(gpu, &self.dc, pw, ph, |dc, origin| {
                    renderer.paint(gpu, dc, Transform2D::scale(scale, scale).then(origin), now)
                });
                if let Err(e) = result {
                    log::error!("paint {}: {e}", live.entry.id);
                }
                live.force_paint = false;
                if live.inst.renderer.needs_paint(now + 16.0) {
                    timeout = timeout.min(16);
                }
            }
            if !paused && let Some(ms) = live.inst.script.next_timer_in() {
                timeout = timeout.min(ms.min(u32::MAX as u64) as u32);
            }
        }
        if plan_changed {
            self.update_plan();
        }
        if self.edit.is_some() {
            let items = self.edit_items();
            if let Some(edit) = &mut self.edit {
                timeout = timeout.min(edit.paint(&self.gpu, &self.dc, &items));
            }
        }
        timeout
    }

    fn handle(&mut self, msg: HostMsg) {
        match msg {
            HostMsg::Pointer(ev) => self.pointer(ev),
            HostMsg::Tray(event, pt) => {
                if event == WM_CONTEXTMENU || event == WM_LBUTTONUP || event == WM_USER
                /* NIN_SELECT */
                {
                    self.tray_menu(pt);
                }
            }
            HostMsg::Timer(TIMER_REATTACH) => {
                unsafe {
                    let _ = KillTimer(Some(self.host), TIMER_REATTACH);
                }
                self.attach();
                if let Some(t) = &mut self.tray {
                    t.show();
                }
            }
            HostMsg::Timer(TIMER_RELAYOUT) => {
                unsafe {
                    let _ = KillTimer(Some(self.host), TIMER_RELAYOUT);
                }
                self.relayout();
            }
            HostMsg::Timer(TIMER_POWER) => self.check_power(),
            HostMsg::Timer(_) => {}
            HostMsg::TaskbarCreated => {
                log::info!("Explorer restarted: re-attaching");
                self.detach();
                unsafe {
                    SetTimer(Some(self.host), TIMER_REATTACH, 1500, None);
                }
            }
            HostMsg::DisplayChanged => unsafe {
                // Coalesce bursts of display/DPI/work-area notifications.
                SetTimer(Some(self.host), TIMER_RELAYOUT, 300, None);
            },
            HostMsg::Reload => self.reload(),
            HostMsg::Command(cmd) => self.command(cmd, None),
            HostMsg::SessionLocked(l) => {
                self.locked = l;
                self.apply_pause();
            }
            HostMsg::DisplayOn(on) => {
                self.display_off = !on;
                self.apply_pause();
            }
            HostMsg::Edit(input) => self.edit_input(input),
        }
    }

    fn apply_pause(&self) {
        let paused = self.paused();
        self.collector.set_paused(paused);
        log::debug!("paused: {paused}");
    }

    fn check_power(&mut self) {
        let state =
            unsafe { SHQueryUserNotificationState() }.unwrap_or(QUERY_USER_NOTIFICATION_STATE(5));
        // 2 = busy (full-screen app), 3 = D3D full screen, 4 = presentation mode.
        let fullscreen = matches!(state.0, 2..=4);
        if fullscreen != self.fullscreen {
            self.fullscreen = fullscreen;
            self.apply_pause();
        }
        let slow = vitrine_system::power::battery_saver()
            || (self.config.pause_on_battery_saver && vitrine_system::power::on_battery());
        self.collector
            .set_interval_multiplier(if slow { 2.0 } else { 1.0 });
    }

    // ── input ─────────────────────────────────────────────────────────

    fn pointer(&mut self, ev: PointerEvent) {
        let Some(i) = self.widgets.iter().position(|l| l.key == ev.key) else {
            return;
        };
        if ev.kind == PointerKind::RightUp {
            self.widget_menu(i, ev.pt);
            return;
        }
        let live = &mut self.widgets[i];
        let x = (ev.pt.x - live.screen.left) as f32 / live.scale;
        let y = (ev.pt.y - live.screen.top) as f32 / live.scale;
        let target = if ev.kind == PointerKind::Leave {
            None
        } else {
            live.inst.renderer.hit_test(x, y)
        };
        let send = |inst: &mut WidgetInstance, ty: &str, node: NodeId, dy: f32| {
            let json = serde_json::json!({ "type": ty, "target": node, "x": x, "y": y, "deltaY": dy, "button": 0 });
            inst.deliver("pointer", &json.to_string());
        };
        if target != live.hover {
            if let Some(old) = live.hover {
                send(&mut live.inst, "pointerleave", old, 0.0);
            }
            if let Some(new) = target {
                send(&mut live.inst, "pointerenter", new, 0.0);
            }
            live.hover = target;
        }
        let Some(node) = target else {
            if ev.kind == PointerKind::Up {
                live.pressed = None;
            }
            return;
        };
        match ev.kind {
            PointerKind::Move => send(&mut live.inst, "pointermove", node, 0.0),
            PointerKind::Down => {
                live.pressed = Some(node);
                send(&mut live.inst, "pointerdown", node, 0.0);
            }
            PointerKind::Up => {
                send(&mut live.inst, "pointerup", node, 0.0);
                if live.pressed.take() == Some(node) {
                    send(&mut live.inst, "click", node, 0.0);
                }
            }
            PointerKind::Wheel => send(
                &mut live.inst,
                "wheel",
                node,
                -(ev.wheel as f32) / 120.0 * 100.0,
            ),
            _ => {}
        }
    }

    // ── menus ─────────────────────────────────────────────────────────

    pub const CMD_EDIT: u32 = 1;
    const CMD_PAUSE: u32 = 2;
    const CMD_AUTOSTART: u32 = 3;
    const CMD_FOLDER: u32 = 4;
    pub const CMD_QUIT: u32 = 5;
    const CMD_PANEL: u32 = 6;
    const CMD_REMOVE: u32 = 7;
    const CMD_SITE: u32 = 8;
    const CMD_ADD: u32 = 1000;

    fn add_menu(&self) -> Item {
        let mut items = Vec::new();
        let mut n = 0;
        for p in &self.packages {
            for w in &p.manifest.widgets {
                items.push(Item::action(Self::CMD_ADD + n, w.name.clone()));
                n += 1;
            }
            items.push(Item::Separator);
        }
        items.pop();
        if items.is_empty() {
            items.push(Item::disabled("No widget packages installed"));
        }
        Item::Sub {
            label: "Add widget".into(),
            items,
        }
    }

    fn nth_widget(&self, n: u32) -> Option<(String, String)> {
        self.packages
            .iter()
            .flat_map(|p| {
                p.manifest
                    .widgets
                    .iter()
                    .map(move |w| (p.manifest.id.clone(), w.id.clone()))
            })
            .nth(n as usize)
    }

    fn tray_menu(&mut self, pt: POINT) {
        let items = vec![
            self.add_menu(),
            Item::check(Self::CMD_EDIT, "Edit layout", self.edit.is_some()),
            Item::action(Self::CMD_PANEL, "Settings…"),
            Item::Separator,
            Item::check(Self::CMD_PAUSE, "Pause widgets", self.user_paused),
            Item::check(
                Self::CMD_AUTOSTART,
                "Start with Windows",
                shellapi::autostart_enabled(),
            ),
            Item::action(Self::CMD_FOLDER, "Open data folder"),
            Item::action(Self::CMD_SITE, "Get more widgets"),
            Item::Separator,
            Item::action(Self::CMD_QUIT, "Quit Vitrine"),
        ];
        if let Some(cmd) = shellapi::popup(self.host, pt, &items) {
            self.command(cmd, None);
        }
    }

    fn widget_menu(&mut self, i: usize, pt: POINT) {
        let items = vec![
            Item::action(Self::CMD_PANEL, "Widget settings…"),
            Item::action(Self::CMD_EDIT, "Edit layout"),
            Item::Separator,
            Item::action(Self::CMD_REMOVE, "Remove widget"),
        ];
        if let Some(cmd) = shellapi::popup(self.host, pt, &items) {
            let id = self.widgets.get(i).map(|l| l.entry.id.clone());
            self.command(cmd, id);
        }
    }

    pub(crate) fn command(&mut self, cmd: u32, instance: Option<String>) {
        match cmd {
            Self::CMD_EDIT => self.toggle_edit(),
            Self::CMD_PAUSE => {
                self.user_paused = !self.user_paused;
                self.apply_pause();
            }
            Self::CMD_AUTOSTART => {
                let on = !shellapi::autostart_enabled();
                if let Err(e) = shellapi::set_autostart(on) {
                    log::error!("autostart: {e}");
                }
                self.config.start_with_windows = on;
                let _ = self.config.save(&paths::config_path());
            }
            Self::CMD_FOLDER => shellapi::open(&paths::data_dir().to_string_lossy()),
            Self::CMD_SITE => shellapi::open("https://vitrine-widgets.github.io/"),
            Self::CMD_PANEL => crate::open_panel(instance.as_deref()),
            Self::CMD_REMOVE => {
                if let Some(id) = instance {
                    self.remove_instance(&id);
                }
            }
            Self::CMD_QUIT => {
                self.quit = true;
                unsafe { PostQuitMessage(0) };
            }
            n if n >= Self::CMD_ADD => {
                if let Some((pkg, wid)) = self.nth_widget(n - Self::CMD_ADD) {
                    self.add_instance(&pkg, &wid);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn add_instance(&mut self, package: &str, widget: &str) {
        let Some(g) = self.primary_geometry() else {
            return;
        };
        let Some(w) = self.package(package).and_then(|p| p.widget(widget)) else {
            return;
        };
        let [cw, ch] = w.size.default;
        let (cw, ch) = (cw as i32, ch as i32);
        let Some((x, y)) = self
            .layout
            .find_free_spot("primary", g.cols, g.rows, cw, ch)
        else {
            log::warn!("no free space for {package}/{widget}");
            return;
        };
        let entry = Instance {
            id: vitrine_widgets::layout::new_instance_id(),
            package: package.into(),
            widget: widget.into(),
            monitor: "primary".into(),
            x,
            y,
            w: cw,
            h: ch,
            settings: Map::new(),
        };
        self.layout.instances.push(entry.clone());
        self.save_layout();
        self.spawn(entry);
        self.relayout();
        self.update_plan();
    }

    pub(crate) fn remove_instance(&mut self, id: &str) {
        self.layout.instances.retain(|i| i.id != id);
        self.save_layout();
        if let Some(pos) = self.widgets.iter().position(|l| l.entry.id == id) {
            let mut live = self.widgets.remove(pos);
            Self::destroy_window(&mut live);
        }
        let _ = std::fs::remove_file(storage_path(id));
        self.update_routing();
        self.update_plan();
    }

    /// Applies an edited layout entry (from edit mode) to the running widget.
    pub(crate) fn set_entry_rect(
        &mut self,
        id: &str,
        monitor: &str,
        (x, y, w, h): (i32, i32, i32, i32),
    ) {
        if let Some(e) = self.layout.instances.iter_mut().find(|i| i.id == id) {
            (e.x, e.y, e.w, e.h) = (x, y, w, h);
            e.monitor = monitor.into();
        }
        if let Some(l) = self.widgets.iter_mut().find(|l| l.entry.id == id) {
            (l.entry.x, l.entry.y, l.entry.w, l.entry.h) = (x, y, w, h);
            l.entry.monitor = monitor.into();
        }
        self.relayout();
    }

    fn monitor_name(&self, name: &str) -> String {
        match monitors::resolve(&self.monitors, name) {
            Some(m) if !m.primary => m.device.clone(),
            _ => "primary".into(),
        }
    }

    fn edit_input(&mut self, input: crate::edit::Input) {
        use crate::edit::{Drag, Input};
        let items = self.edit_items();
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let item = |id: &str| items.iter().find(|i| i.id == id);
        match input {
            Input::Escape => self.close_edit(),
            Input::Down(pt) => {
                if contains(&edit.done_button(), pt) {
                    self.close_edit();
                } else if let Some((id, resize)) = edit.hit(&items, pt)
                    && let Some(it) = item(&id)
                {
                    edit.drag = Some(Drag {
                        id,
                        resize,
                        start: pt,
                        from: it.rect,
                        moved: false,
                        monitor: it.monitor.clone(),
                    });
                    edit.invalidate();
                }
            }
            Input::Move(pt) => {
                let Some(drag) = edit.drag.clone() else {
                    let hover = edit.hit(&items, pt);
                    let cursor = match &hover {
                        Some((_, true)) => IDC_SIZENWSE,
                        Some(_) => IDC_SIZEALL,
                        None => IDC_ARROW,
                    };
                    unsafe {
                        let _ = SetCursor(LoadCursorW(None, cursor).ok());
                    }
                    let id = hover.map(|h| h.0);
                    if id != edit.hover {
                        edit.hover = id;
                        edit.invalidate();
                    }
                    return;
                };
                let Some(it) = item(&drag.id) else { return };
                let Some(from_geo) = edit.geometry(&drag.monitor) else {
                    return;
                };
                let (fx, fy, fw, fh) = drag.from;
                let from_px = from_geo.rect(fx, fy, fw, fh);
                let (dx, dy) = ((pt.x - drag.start.x) as f32, (pt.y - drag.start.y) as f32);
                let (monitor, rect) = if drag.resize {
                    let w = from_geo
                        .span((from_px.right - from_px.left) as f32 + dx)
                        .clamp(it.min.0, it.max.0);
                    let h = from_geo
                        .span((from_px.bottom - from_px.top) as f32 + dy)
                        .clamp(it.min.1, it.max.1);
                    (drag.monitor.clone(), from_geo.clamp(fx, fy, w, h))
                } else {
                    let Some((monitor, g)) = edit.monitor_at(pt) else {
                        return;
                    };
                    let (x, y) = g.snap(from_px.left as f32 + dx, from_px.top as f32 + dy);
                    (monitor, g.clamp(x, y, fw, fh))
                };
                if (monitor.as_str(), rect) == (it.monitor.as_str(), it.rect)
                    || self.layout.rect_collides(&monitor, rect, Some(&drag.id))
                {
                    return;
                }
                if let Some(d) = &mut edit.drag {
                    d.moved = true;
                }
                edit.invalidate();
                self.set_entry_rect(&drag.id, &monitor, rect);
            }
            Input::Up => {
                let moved = edit.drag.take().is_some_and(|d| d.moved);
                edit.invalidate();
                if moved {
                    self.save_layout();
                }
            }
            Input::RightUp(pt) => {
                let Some((id, _)) = edit.hit(&items, pt) else {
                    return;
                };
                let menu = vec![
                    Item::action(Self::CMD_PANEL, "Widget settings…"),
                    Item::Separator,
                    Item::action(Self::CMD_REMOVE, "Remove widget"),
                ];
                if let Some(cmd) = shellapi::popup(self.host, pt, &menu) {
                    self.command(cmd, Some(id));
                }
                if let Some(e) = &mut self.edit {
                    e.invalidate();
                }
            }
        }
    }

    fn toggle_edit(&mut self) {
        if let Some(edit) = self.edit.take() {
            drop(edit);
            self.save_layout();
        } else {
            let geometries: Vec<(String, Geometry, RECT)> = self
                .monitors
                .iter()
                .map(|m| {
                    (
                        if m.primary {
                            "primary".to_string()
                        } else {
                            m.device.clone()
                        },
                        Geometry::for_monitor(&self.layout.grid, m),
                        m.rect,
                    )
                })
                .collect();
            let icons = self
                .routing
                .read()
                .map(|r| r.icons.rects.clone())
                .unwrap_or_default();
            match EditMode::open(&self.gpu, self.hinst, geometries, icons) {
                Ok(e) => self.edit = Some(e),
                Err(e) => log::error!("edit mode: {e}"),
            }
        }
        self.update_routing();
    }

    /// Instances as `(id, name, monitor, x, y, w, h, min, max)` for edit mode.
    pub(crate) fn edit_items(&self) -> Vec<crate::edit::Item> {
        self.layout
            .instances
            .iter()
            .map(|e| {
                let manifest = self.package(&e.package).and_then(|p| p.widget(&e.widget));
                crate::edit::Item {
                    id: e.id.clone(),
                    name: manifest
                        .map(|m| m.name.clone())
                        .unwrap_or_else(|| e.widget.clone()),
                    monitor: self.monitor_name(&e.monitor),
                    rect: (e.x, e.y, e.w, e.h),
                    min: manifest
                        .map(|m| (m.size.min[0] as i32, m.size.min[1] as i32))
                        .unwrap_or((1, 1)),
                    max: manifest
                        .map(|m| (m.size.max[0] as i32, m.size.max[1] as i32))
                        .unwrap_or((16, 16)),
                }
            })
            .collect()
    }

    pub(crate) fn close_edit(&mut self) {
        if self.edit.is_some() {
            self.toggle_edit();
        }
    }
}

// ── icon geometry poller ──────────────────────────────────────────────

/// Keeps `RoutingState::icons` current while the pointer is over the desktop.
fn start_icon_poller(routing: Arc<RwLock<RoutingState>>) {
    let _ = std::thread::Builder::new()
        .name("vitrine-icons".into())
        .spawn(move || unsafe {
            use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
            use windows::Win32::UI::WindowsAndMessaging::{
                FindWindowExW, FindWindowW, GetCursorPos, WindowFromPoint,
            };
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let mut view = None;
            let mut last = Instant::now() - Duration::from_secs(60);
            loop {
                std::thread::sleep(Duration::from_millis(500));
                if routing.read().map(|r| r.targets.is_empty()).unwrap_or(true) {
                    continue;
                }
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let class = vitrine_desktop::util::class_name(WindowFromPoint(pt));
                let on_desktop = matches!(
                    class.as_str(),
                    "SysListView32" | "SHELLDLL_DefView" | "WorkerW" | "Progman"
                );
                // Fresh while the pointer is over the desktop, occasionally otherwise.
                let due = if on_desktop {
                    Duration::from_millis(1500)
                } else {
                    Duration::from_secs(30)
                };
                if last.elapsed() < due {
                    continue;
                }
                if view.is_none() {
                    view = vitrine_desktop::icons::desktop_folder_view().ok();
                }
                let Some(v) = &view else { continue };
                let progman = FindWindowW(w!("Progman"), None).unwrap_or_default();
                let def_view = FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), None)
                    .unwrap_or_default();
                let list = FindWindowExW(Some(def_view), None, w!("SysListView32"), None)
                    .unwrap_or_default();
                match vitrine_desktop::icons::read_icon_map(v, list) {
                    Ok(map) => {
                        if let Ok(mut r) = routing.write() {
                            r.icons = map;
                        }
                    }
                    Err(_) => view = None, // Explorer restarted: reconnect next time
                }
                last = Instant::now();
            }
        });
}

// ── window procedures and entry point ─────────────────────────────────

unsafe extern "system" fn host_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let (taskbar_created, reload, command) = MESSAGES.with_borrow(|m| *m);
    if msg != 0 && msg == command {
        push(HostMsg::Command(wparam.0 as u32));
        return LRESULT(0);
    }
    if msg != 0 && msg == taskbar_created {
        push(HostMsg::TaskbarCreated);
        return LRESULT(0);
    }
    if msg != 0 && msg == reload {
        push(HostMsg::Reload);
        return LRESULT(0);
    }
    match msg {
        WM_VITRINE_POINTER => push(HostMsg::Pointer(PointerEvent::decode(wparam, lparam))),
        WM_APP_TRAY => {
            // NOTIFYICON_VERSION_4: event in LOWORD(lParam), anchor point in wParam.
            let event = (lparam.0 & 0xFFFF) as u32;
            let pt = POINT {
                x: (wparam.0 & 0xFFFF) as i16 as i32,
                y: ((wparam.0 >> 16) & 0xFFFF) as i16 as i32,
            };
            push(HostMsg::Tray(event, pt));
        }
        WM_TIMER => push(HostMsg::Timer(wparam.0)),
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_EDIT => push(HostMsg::Command(Runtime::CMD_EDIT)),
        WM_DISPLAYCHANGE | WM_DPICHANGED => push(HostMsg::DisplayChanged),
        WM_SETTINGCHANGE if wparam.0 as u32 == SPI_SETWORKAREA.0 => push(HostMsg::DisplayChanged),
        WM_WTSSESSION_CHANGE => match wparam.0 as u32 {
            WTS_SESSION_LOCK => push(HostMsg::SessionLocked(true)),
            WTS_SESSION_UNLOCK => push(HostMsg::SessionLocked(false)),
            _ => {}
        },
        WM_POWERBROADCAST if wparam.0 as u32 == PBT_POWERSETTINGCHANGE => {
            let setting = unsafe {
                &*(lparam.0 as *const windows::Win32::System::Power::POWERBROADCAST_SETTING)
            };
            if setting.PowerSetting
                == windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE
            {
                push(HostMsg::DisplayOn(setting.Data[0] != 0));
            }
            return LRESULT(1);
        }
        WM_APP_WAKE => {}
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

unsafe extern "system" fn widget_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

thread_local! {
    pub(crate) static RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) };
}

pub fn run() -> Result<(), String> {
    unsafe {
        use windows::Win32::System::Com::{
            COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
        };
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| e.to_string())?;
        let hinst: HINSTANCE = GetModuleHandleW(None).map_err(|e| e.to_string())?.into();
        MESSAGES.with_borrow_mut(|m| {
            *m = (
                RegisterWindowMessageW(w!("TaskbarCreated")),
                RegisterWindowMessageW(RELOAD_MESSAGE),
                RegisterWindowMessageW(COMMAND_MESSAGE),
            )
        });
        window::register_class(hinst, HOST_CLASS, host_proc).map_err(|e| e.to_string())?;
        window::register_class(hinst, WIDGET_CLASS, widget_proc).map_err(|e| e.to_string())?;
        crate::edit::register(hinst).map_err(|e| e.to_string())?;

        // A hidden top-level window (not message-only: it must receive broadcasts).
        let host = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            HOST_CLASS,
            w!("Vitrine"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        )
        .map_err(|e| e.to_string())?;
        let _ = WTSRegisterSessionNotification(host, NOTIFY_FOR_THIS_SESSION);
        let _ = windows::Win32::System::Power::RegisterPowerSettingNotification(
            windows::Win32::Foundation::HANDLE(host.0),
            &windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );
        // The process is a low-priority background app: let Windows schedule it efficiently.
        crate::power::enable_eco_qos();

        let mut rt = Runtime::new(hinst, host)?;
        rt.install_bundled();
        input::start(rt.routing.clone(), host);
        start_icon_poller(rt.routing.clone());
        rt.tray = Some(Tray::add(host, WM_APP_TRAY, "Vitrine"));
        rt.reload();
        rt.attach();
        rt.check_power();
        register_hotkey(host, &rt.config.edit_hotkey);
        SetTimer(Some(host), TIMER_POWER, 5000, None);
        RUNTIME.with_borrow_mut(|r| *r = Some(rt));

        let mut msg = MSG::default();
        'outer: loop {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'outer;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let timeout = RUNTIME.with_borrow_mut(|rt| rt.as_mut().map(|r| r.tick()).unwrap_or(0));
            if INBOX.with_borrow(|q| !q.is_empty()) {
                continue;
            }
            MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }

        // Release windows and COM objects while COM is still initialized.
        RUNTIME.with_borrow_mut(|r| {
            if let Some(mut rt) = r.take() {
                rt.edit = None;
                rt.detach();
            }
        });
        let _ = DestroyWindow(host);
        CoUninitialize();
    }
    Ok(())
}

/// Parses `Ctrl+Alt+V`-style shortcuts and registers the edit-mode hotkey.
fn register_hotkey(host: HWND, spec: &str) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let Some((mods, key)) = parse_hotkey(spec) else {
        log::warn!("invalid edit hotkey `{spec}`");
        return;
    };
    if let Err(e) = unsafe {
        RegisterHotKey(
            Some(host),
            HOTKEY_EDIT,
            HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT,
            key,
        )
    } {
        log::warn!("edit hotkey `{spec}` unavailable: {e}");
    }
}

/// `(MOD_* flags, virtual key)` for strings like `Ctrl+Alt+V` or `Win+Shift+F9`.
pub fn parse_hotkey(spec: &str) -> Option<(u32, u32)> {
    let mut mods = 0u32;
    let mut key = None;
    for part in spec.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "ctrl" | "control" => mods |= 0x2,
            "alt" => mods |= 0x1,
            "shift" => mods |= 0x4,
            "win" => mods |= 0x8,
            p if p.len() == 1 && p.as_bytes()[0].is_ascii_alphanumeric() => {
                key = Some(p.to_ascii_uppercase().as_bytes()[0] as u32)
            }
            p if p.starts_with('f')
                && p[1..].parse::<u32>().is_ok_and(|n| (1..=24).contains(&n)) =>
            {
                key = Some(0x6F + p[1..].parse::<u32>().unwrap())
            }
            _ => return None,
        }
    }
    (mods != 0).then_some(())?;
    key.map(|k| (mods, k))
}

/// Runs a command in the running instance (see `COMMAND_MESSAGE`).
pub fn send_command(cmd: u32) -> bool {
    unsafe {
        let Ok(host) = FindWindowW(HOST_CLASS, None) else {
            return false;
        };
        let msg = RegisterWindowMessageW(COMMAND_MESSAGE);
        PostMessageW(Some(host), msg, WPARAM(cmd as usize), LPARAM(0)).is_ok()
    }
}

/// Asks a running Vitrine to reload packages, layout and settings.
pub fn notify_reload() -> bool {
    unsafe {
        let Ok(host) = FindWindowW(HOST_CLASS, None) else {
            return false;
        };
        let msg = RegisterWindowMessageW(RELOAD_MESSAGE);
        PostMessageW(Some(host), msg, WPARAM(0), LPARAM(0)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_hotkeys() {
        assert_eq!(super::parse_hotkey("Ctrl+Alt+V"), Some((0x3, 'V' as u32)));
        assert_eq!(super::parse_hotkey("win + shift + f9"), Some((0xC, 0x78)));
        assert_eq!(super::parse_hotkey("V"), None);
        assert_eq!(super::parse_hotkey("Ctrl+Nope"), None);
    }
}
