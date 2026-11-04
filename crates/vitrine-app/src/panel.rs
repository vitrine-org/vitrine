//! The control panel (`vitrine --panel`): a separate Slint process that edits
//! `layout.json` / `settings.json` and asks the running Vitrine to reload.
//! Settings changes therefore apply live, without restarting widgets.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde_json::{Map, Value};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use vitrine_widgets::config::Config;
use vitrine_widgets::layout::{Instance, Layout};
use vitrine_widgets::package::{self, InstalledPackage};
use vitrine_widgets::paths;
use vitrine_widgets::settings::{SettingDef, SettingType, SettingsSchema};

use crate::grid::Geometry;
use crate::runtime;
use crate::shellapi;

slint::include_modules!();

struct State {
    packages: Vec<InstalledPackage>,
    layout: Layout,
    layout_stamp: Option<SystemTime>,
    config: Config,
    /// Instance being edited on the settings page.
    editing: Option<String>,
    schema: Option<SettingsSchema>,
    values: Map<String, Value>,
    /// Keys shown on the settings page, in order (to detect visibility changes).
    shown: Vec<String>,
    /// Widget shown on the detail page and its size choices (cells).
    detail: Option<Detail>,
    /// Latest preview request; older renders are discarded.
    preview_gen: Arc<AtomicU64>,
    /// Debounces settings-page previews while the user types.
    preview_timer: Rc<slint::Timer>,
}

fn layout_stamp() -> Option<SystemTime> {
    std::fs::metadata(paths::layout_path())
        .and_then(|m| m.modified())
        .ok()
}

/// Package id, widget id and size choices (cells) of the detail page.
type Detail = (String, String, Vec<(i32, i32)>);

impl State {
    fn empty() -> State {
        State {
            packages: Vec::new(),
            layout: Layout::default(),
            layout_stamp: None,
            config: Config::default(),
            editing: None,
            schema: None,
            values: Map::new(),
            shown: Vec::new(),
            detail: None,
            preview_gen: Arc::new(AtomicU64::new(0)),
            preview_timer: Rc::new(slint::Timer::default()),
        }
    }

    fn load() -> State {
        State {
            packages: package::scan(&paths::packages_dir()),
            layout: Layout::load(&paths::layout_path()).unwrap_or_default(),
            layout_stamp: layout_stamp(),
            config: Config::load(&paths::config_path()).unwrap_or_default(),
            editing: None,
            schema: None,
            values: Map::new(),
            shown: Vec::new(),
            detail: None,
            preview_gen: Arc::new(AtomicU64::new(0)),
            preview_timer: Rc::new(slint::Timer::default()),
        }
    }

    fn package(&self, id: &str) -> Option<&InstalledPackage> {
        self.packages.iter().find(|p| p.manifest.id == id)
    }

    fn reload_layout(&mut self) {
        self.layout = Layout::load(&paths::layout_path()).unwrap_or_default();
        self.layout_stamp = layout_stamp();
    }

    /// Re-reads, changes and saves the layout, then tells Vitrine.
    fn update_layout(&mut self, f: impl FnOnce(&mut Layout)) -> Result<(), String> {
        self.reload_layout();
        f(&mut self.layout);
        self.layout
            .save(&paths::layout_path())
            .map_err(|e| e.to_string())?;
        self.layout_stamp = layout_stamp();
        notify();
        Ok(())
    }
}

/// Asks the running Vitrine to reload; starts it if it is not running.
fn notify() {
    if !runtime::notify_reload()
        && let Ok(exe) = std::env::current_exe()
    {
        let _ = std::process::Command::new(exe).spawn();
    }
}

fn s(v: impl Into<SharedString>) -> SharedString {
    v.into()
}

fn preview_image(
    pkg: &InstalledPackage,
    w: &vitrine_widgets::manifest::WidgetManifest,
) -> slint::Image {
    w.preview
        .as_ref()
        .filter(|rel| vitrine_widgets::manifest::is_safe_relative_path(rel))
        .and_then(|rel| slint::Image::load_from_path(&pkg.resolve(rel)).ok())
        .unwrap_or_default()
}

#[derive(Clone, Copy)]
enum PreviewTarget {
    Detail,
    Settings,
}

/// Renders a widget off the UI thread (scripts need a big stack and their own
/// graphics device) and shows it when done, unless a newer request came in.
fn request_preview(
    ui: &PanelWindow,
    st: &State,
    pkg: InstalledPackage,
    widget: String,
    cells: (i32, i32),
    settings: Option<String>,
    target: PreviewTarget,
) {
    let generation = st.preview_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let latest = st.preview_gen.clone();
    match target {
        PreviewTarget::Detail => ui.set_detail_busy(true),
        PreviewTarget::Settings => ui.set_settings_preview_busy(true),
    }
    let weak = ui.as_weak();
    let _ = std::thread::Builder::new()
        .name("vitrine-preview".into())
        .stack_size(8 << 20)
        .spawn(move || {
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                );
            }
            let size = crate::headless::cells_to_dips(cells.0.max(1) as u32, cells.1.max(1) as u32);
            let result = crate::headless::render_rgba(
                &pkg,
                &widget,
                size,
                1.5,
                settings.as_deref(),
                700,
                true,
            );
            if latest.load(Ordering::SeqCst) != generation {
                return;
            }
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let image = match result {
                    Ok((w, h, rgba)) => {
                        let buffer =
                            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                &rgba, w, h,
                            );
                        slint::Image::from_rgba8(buffer)
                    }
                    Err(e) => {
                        log::warn!("preview: {e}");
                        slint::Image::default()
                    }
                };
                match target {
                    PreviewTarget::Detail => {
                        ui.set_detail_preview(image);
                        ui.set_detail_busy(false);
                    }
                    PreviewTarget::Settings => {
                        ui.set_settings_preview(image);
                        ui.set_settings_preview_busy(false);
                    }
                }
            });
        });
}

/// Size choices for a widget: its minimum, default and maximum, plus one step
/// between default and maximum.
fn size_choices(w: &vitrine_widgets::manifest::WidgetManifest) -> Vec<(i32, i32)> {
    let c = |a: [u32; 2]| (a[0] as i32, a[1] as i32);
    let (min, def, max) = (c(w.size.min), c(w.size.default), c(w.size.max));
    let mid = ((def.0 + 1).min(max.0), (def.1 + 1).min(max.1));
    let big = ((def.0 * 2).min(max.0), (def.1 * 2).min(max.1));
    let mut out = Vec::new();
    for v in [min, def, mid, big] {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out.sort_by_key(|(w, h)| w * h);
    out
}

fn open_detail(ui: &PanelWindow, st: &mut State, package: &str, widget: &str) {
    let Some(pkg) = st.package(package).cloned() else {
        return;
    };
    let Some(w) = pkg.widget(widget).cloned() else {
        return;
    };
    let sizes = size_choices(&w);
    let def = (w.size.default[0] as i32, w.size.default[1] as i32);
    let index = sizes.iter().position(|s| *s == def).unwrap_or(0);
    ui.set_detail(LibraryRow {
        package: s(package),
        widget: s(widget),
        name: s(w.name.as_str()),
        description: s(w.description.as_str()),
        pack: s(pkg.manifest.name.as_str()),
        size: s(format!("{}×{}", def.0, def.1)),
        preview: preview_image(&pkg, &w),
    });
    ui.set_detail_preview(preview_image(&pkg, &w));
    ui.set_detail_sizes(ModelRc::new(VecModel::from(
        sizes
            .iter()
            .map(|(w, h)| s(format!("{w} × {h}")))
            .collect::<Vec<_>>(),
    )));
    ui.set_detail_size(index as i32);
    ui.set_page(5);
    st.detail = Some((package.to_string(), widget.to_string(), sizes.clone()));
    request_preview(
        ui,
        st,
        pkg,
        widget.to_string(),
        sizes[index],
        None,
        PreviewTarget::Detail,
    );
}

/// Re-renders the settings-page preview shortly after the last change.
fn schedule_settings_preview(ui: &PanelWindow, st: &State) {
    let Some(id) = st.editing.clone() else { return };
    let Some(inst) = st.layout.instance(&id).cloned() else {
        return;
    };
    let Some(pkg) = st.package(&inst.package).cloned() else {
        return;
    };
    let settings = Value::Object(st.values.clone()).to_string();
    let weak = ui.as_weak();
    let counter = st.preview_gen.clone();
    st.preview_timer.start(
        slint::TimerMode::SingleShot,
        std::time::Duration::from_millis(250),
        move || {
            let Some(ui) = weak.upgrade() else { return };
            // A throwaway State just to reach `request_preview` with the shared counter.
            let probe = State {
                preview_gen: counter.clone(),
                ..State::empty()
            };
            request_preview(
                &ui,
                &probe,
                pkg.clone(),
                inst.widget.clone(),
                (inst.w, inst.h),
                Some(settings.clone()),
                PreviewTarget::Settings,
            );
        },
    );
}

fn instance_rows(st: &State) -> Vec<InstanceRow> {
    st.layout
        .instances
        .iter()
        .map(|i| {
            let pkg = st.package(&i.package);
            let w = pkg.and_then(|p| p.widget(&i.widget));
            let monitor = if i.monitor.eq_ignore_ascii_case("primary") {
                "main monitor".to_string()
            } else {
                i.monitor.trim_start_matches(r"\\.\").to_string()
            };
            InstanceRow {
                id: s(i.id.as_str()),
                name: s(w
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| i.widget.clone())),
                detail: s(match pkg {
                    Some(p) => format!("{} · {}×{} · {}", p.manifest.name, i.w, i.h, monitor),
                    None => format!("Missing package {}", i.package),
                }),
                has_settings: w.is_some_and(|w| w.settings.is_some()),
                preview: match (pkg, w) {
                    (Some(p), Some(w)) => preview_image(p, w),
                    _ => slint::Image::default(),
                },
            }
        })
        .collect()
}

fn library_rows(st: &State) -> Vec<LibraryRow> {
    st.packages
        .iter()
        .flat_map(|p| {
            p.manifest.widgets.iter().map(move |w| LibraryRow {
                package: s(p.manifest.id.as_str()),
                widget: s(w.id.as_str()),
                name: s(w.name.as_str()),
                description: s(w.description.as_str()),
                pack: s(p.manifest.name.as_str()),
                size: s(format!("{}×{}", w.size.default[0], w.size.default[1])),
                preview: preview_image(p, w),
            })
        })
        .collect()
}

fn package_rows(st: &State) -> Vec<PackageRow> {
    st.packages
        .iter()
        .map(|p| {
            let n = p.manifest.widgets.len();
            let mut detail = format!(
                "Version {} · {} widget{}",
                p.manifest.version,
                n,
                if n == 1 { "" } else { "s" }
            );
            if !p.manifest.author.is_empty() {
                detail.push_str(&format!(" · {}", p.manifest.author));
            }
            PackageRow {
                id: s(p.manifest.id.as_str()),
                name: s(p.manifest.name.as_str()),
                detail: s(detail),
            }
        })
        .collect()
}

fn is_integral(def: &SettingDef) -> bool {
    [def.min, def.max, def.step]
        .iter()
        .all(|v| v.is_none_or(|n| n.fract() == 0.0))
}

fn setting_row(
    key: &str,
    def: &SettingDef,
    value: &Value,
    section: &str,
    enabled: bool,
) -> SettingRow {
    let label = if def.label.is_empty() {
        key.to_string()
    } else {
        def.label.clone()
    };
    let mut row = SettingRow {
        key: s(key),
        section: s(section),
        label: s(label),
        description: s(def.description.as_str()),
        enabled,
        ..Default::default()
    };
    if !def.options.is_empty() {
        row.kind = 3;
        row.options = ModelRc::new(VecModel::from(
            def.options
                .iter()
                .map(|o| {
                    s(if o.label.is_empty() {
                        o.value.to_string().trim_matches('"').to_string()
                    } else {
                        o.label.clone()
                    })
                })
                .collect::<Vec<_>>(),
        ));
        row.selected = def
            .options
            .iter()
            .position(|o| &o.value == value)
            .map(|i| i as i32)
            .unwrap_or(0);
        return row;
    }
    match def.kind {
        SettingType::Boolean => {
            row.kind = 0;
            row.bool_value = value.as_bool().unwrap_or(false);
        }
        SettingType::Number if is_integral(def) => {
            row.kind = 1;
            row.int_value = value.as_f64().unwrap_or(0.0).round() as i32;
            row.min = def.min.map(|v| v as i32).unwrap_or(-1_000_000);
            row.max = def.max.map(|v| v as i32).unwrap_or(1_000_000);
            row.step = def.step.map(|v| v as i32).unwrap_or(1).max(1);
        }
        SettingType::Number => {
            row.kind = 4;
            row.text_value = s(value.as_f64().map(|n| n.to_string()).unwrap_or_default());
        }
        SettingType::String => {
            row.kind = 2;
            row.text_value = s(value.as_str().unwrap_or(""));
        }
    }
    row
}

/// Rows for the settings page; also records which keys are shown.
fn settings_rows(st: &mut State) -> Vec<SettingRow> {
    let mut rows = Vec::new();
    let mut shown = Vec::new();
    if let Some(schema) = &st.schema {
        for section in &schema.sections {
            let mut title = section.title.clone();
            for (key, def) in &section.settings {
                if !schema.is_visible(key, &st.values) {
                    continue;
                }
                let value = st
                    .values
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| def.default_value());
                let enabled = !schema.is_disabled(key, &st.values);
                rows.push(setting_row(key, def, &value, &title, enabled));
                shown.push(key.clone());
                title.clear();
            }
        }
    }
    st.shown = shown;
    rows
}

fn open_settings(ui: &PanelWindow, st: &mut State, id: &str) {
    st.reload_layout();
    let Some(inst) = st.layout.instance(id).cloned() else {
        return;
    };
    let Some(pkg) = st.package(&inst.package).cloned() else {
        return;
    };
    let Some(w) = pkg.widget(&inst.widget).cloned() else {
        return;
    };
    let schema = pkg.read_settings_schema(&w).ok().flatten();
    let mut values = schema.as_ref().map(|s| s.defaults()).unwrap_or_default();
    values.extend(inst.settings.clone());
    st.editing = Some(id.to_string());
    st.schema = schema;
    st.values = values;
    ui.set_settings_title(s(w.name.as_str()));
    ui.set_settings_subtitle(s(format!(
        "{} · changes apply immediately",
        pkg.manifest.name
    )));
    ui.set_settings(ModelRc::new(VecModel::from(settings_rows(st))));
    ui.set_settings_preview(preview_image(&pkg, &w));
    ui.set_page(4);
    schedule_settings_preview(ui, st);
}

/// Validates `key = value`; on success saves the instance settings and returns true.
fn apply_setting(ui: &PanelWindow, st: &mut State, key: &str, value: Value) {
    let (Some(id), Some(schema)) = (st.editing.clone(), st.schema.clone()) else {
        return;
    };
    let mut values = st.values.clone();
    values.insert(key.to_string(), value);
    let error = match schema.normalize(&values) {
        Ok(normalized) => {
            st.values = normalized.clone();
            // Store only what differs from the defaults, so pack updates can change defaults.
            let defaults = schema.defaults();
            let stored: Map<String, Value> = normalized
                .into_iter()
                .filter(|(k, v)| defaults.get(k) != Some(v))
                .collect();
            if let Err(e) = st.update_layout(|l| {
                if let Some(i) = l.instances.iter_mut().find(|i| i.id == id) {
                    i.settings = stored;
                }
            }) {
                ui.set_status(s(format!("Could not save: {e}")));
            }
            String::new()
        }
        Err(errors) => {
            st.values = values;
            errors
                .into_iter()
                .find(|(k, _)| k == key)
                .map(|(_, m)| m)
                .unwrap_or_default()
        }
    };
    if error.is_empty() {
        schedule_settings_preview(ui, st);
    }
    // Rebuild when visibility changed; otherwise update rows in place (keeps focus while typing).
    let before = st.shown.clone();
    let rows = settings_rows(st);
    let model = ui.get_settings();
    if before != st.shown || model.row_count() != rows.len() {
        ui.set_settings(ModelRc::new(VecModel::from(rows)));
        return;
    }
    for (i, mut row) in rows.into_iter().enumerate() {
        if row.key.as_str() == key {
            row.error = s(error.as_str());
            if let Some(old) = model.row_data(i) {
                // Keep what the user typed, even if invalid.
                row.text_value = old.text_value;
            }
        }
        model.set_row_data(i, row);
    }
}

fn refresh(ui: &PanelWindow, st: &State) {
    ui.set_instances(ModelRc::new(VecModel::from(instance_rows(st))));
    ui.set_library(ModelRc::new(VecModel::from(library_rows(st))));
    ui.set_packages(ModelRc::new(VecModel::from(package_rows(st))));
}

fn add_widget(
    st: &mut State,
    package: &str,
    widget: &str,
    size: Option<(i32, i32)>,
) -> Result<String, String> {
    let w = st
        .package(package)
        .and_then(|p| p.widget(widget))
        .cloned()
        .ok_or("unknown widget")?;
    let monitors = vitrine_desktop::monitors::monitors();
    let primary = monitors
        .iter()
        .find(|m| m.primary)
        .or(monitors.first())
        .ok_or("no monitor")?;
    let g = Geometry::for_monitor(&st.layout.grid, primary);
    let (cw, ch) = size.unwrap_or((w.size.default[0] as i32, w.size.default[1] as i32));
    let mut placed = Err(format!(
        "No free space for {} ({cw}×{ch}). Make room in Edit layout.",
        w.name
    ));
    st.update_layout(|l| {
        if let Some((x, y)) = l.find_free_spot("primary", g.cols, g.rows, cw, ch) {
            l.instances.push(Instance {
                id: vitrine_widgets::layout::new_instance_id(),
                package: package.into(),
                widget: widget.into(),
                monitor: "primary".into(),
                x,
                y,
                w: cw,
                h: ch,
                settings: Map::new(),
            });
            placed = Ok(format!("Added {}", w.name));
        }
    })?;
    placed
}

/// Shows the system file picker for `.vitrine` packages.
fn pick_package() -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::*;
    use windows::core::w;
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let filters = [
            COMDLG_FILTERSPEC {
                pszName: w!("Vitrine packages"),
                pszSpec: w!("*.vitrine"),
            },
            COMDLG_FILTERSPEC {
                pszName: w!("All files"),
                pszSpec: w!("*.*"),
            },
        ];
        dialog.SetFileTypes(&filters).ok()?;
        dialog.SetTitle(w!("Install a widget package")).ok()?;
        dialog.Show(None).ok()?;
        let item = dialog.GetResult().ok()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let out = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        out.map(Into::into)
    }
}

pub fn run(instance: Option<String>) -> Result<(), String> {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
    }
    let ui = PanelWindow::new().map_err(|e| e.to_string())?;
    let st = Rc::new(RefCell::new(State::load()));
    {
        let st = st.borrow();
        ui.set_version(s(env!("CARGO_PKG_VERSION")));
        ui.set_autostart(shellapi::autostart_enabled());
        ui.set_slow_on_battery(st.config.pause_on_battery_saver);
        ui.set_hotkey(s(st.config.edit_hotkey.as_str()));
        refresh(&ui, &st);
    }
    if let Some(id) = instance {
        open_settings(&ui, &mut st.borrow_mut(), &id);
    }

    let weak = ui.as_weak();
    macro_rules! with {
        (|$ui:ident, $st:ident $(, $arg:ident : $ty:ty)*| $body:block) => {{
            let weak = weak.clone();
            let st = st.clone();
            move |$($arg: $ty),*| {
                let Some($ui) = weak.upgrade() else { return };
                let mut guard = st.borrow_mut();
                let $st: &mut State = &mut guard;
                $body
            }
        }};
    }

    ui.on_open_settings(with!(|ui, st, id: SharedString| {
        open_settings(&ui, st, &id)
    }));
    ui.on_remove_instance(with!(|ui, st, id: SharedString| {
        let id = id.to_string();
        match st.update_layout(|l| l.instances.retain(|i| i.id != id)) {
            Ok(()) => ui.set_status(s("Widget removed")),
            Err(e) => ui.set_status(s(format!("Could not save: {e}"))),
        }
        refresh(&ui, st);
    }));
    ui.on_add_widget(with!(|ui,
                            st,
                            package: SharedString,
                            widget: SharedString| {
        let msg = add_widget(st, &package, &widget, None).unwrap_or_else(|e| e);
        ui.set_status(s(msg));
        refresh(&ui, st);
    }));
    ui.on_open_detail(with!(|ui,
                             st,
                             package: SharedString,
                             widget: SharedString| {
        open_detail(&ui, st, &package, &widget)
    }));
    ui.on_choose_size(with!(|ui, st, index: i32| {
        let Some((package, widget, sizes)) = st.detail.clone() else {
            return;
        };
        let Some(pkg) = st.package(&package).cloned() else {
            return;
        };
        if let Some(size) = sizes.get(index.max(0) as usize) {
            request_preview(&ui, st, pkg, widget, *size, None, PreviewTarget::Detail);
        }
    }));
    ui.on_add_detail(with!(|ui, st| {
        let Some((package, widget, sizes)) = st.detail.clone() else {
            return;
        };
        let size = sizes.get(ui.get_detail_size().max(0) as usize).copied();
        match add_widget(st, &package, &widget, size) {
            Ok(msg) => {
                ui.set_status(s(msg));
                refresh(&ui, st);
                ui.set_page(0);
            }
            Err(e) => ui.set_status(s(e)),
        }
    }));
    ui.on_place_detail(with!(|ui, _st| {
        if runtime::send_command(runtime::Runtime::CMD_GALLERY) {
            ui.set_status(s("Drag the widget from the gallery onto your desktop"));
        } else {
            ui.set_status(s("Vitrine is not running"));
        }
    }));
    ui.on_uninstall(with!(|ui, st, id: SharedString| {
        let id = id.to_string();
        match package::uninstall(&id, &paths::packages_dir()) {
            Ok(()) => {
                let _ = st.update_layout(|l| l.instances.retain(|i| i.package != id));
                st.packages = package::scan(&paths::packages_dir());
                ui.set_status(s("Package uninstalled"));
            }
            Err(e) => ui.set_status(s(format!("Could not uninstall: {e}"))),
        }
        refresh(&ui, st);
    }));
    ui.on_install_file(with!(|ui, st| {
        let Some(path) = pick_package() else { return };
        match package::install(&path, &paths::packages_dir()) {
            Ok(p) => {
                ui.set_status(s(format!(
                    "Installed {} {}",
                    p.manifest.name, p.manifest.version
                )));
                st.packages = package::scan(&paths::packages_dir());
                notify();
                ui.set_page(1);
            }
            Err(e) => ui.set_status(s(format!("Could not install: {e}"))),
        }
        refresh(&ui, st);
    }));
    ui.on_set_bool(with!(|ui, st, key: SharedString, v: bool| {
        apply_setting(&ui, st, &key, Value::Bool(v))
    }));
    ui.on_set_int(with!(|ui, st, key: SharedString, v: i32| {
        apply_setting(&ui, st, &key, Value::from(v))
    }));
    ui.on_set_text(with!(|ui, st, key: SharedString, v: SharedString| {
        let is_number = st
            .schema
            .as_ref()
            .and_then(|s| s.get(&key))
            .is_some_and(|d| d.kind == SettingType::Number);
        let value = if is_number {
            v.trim()
                .replace(',', ".")
                .parse::<f64>()
                .map(Value::from)
                .unwrap_or(Value::String(v.to_string()))
        } else {
            Value::String(v.to_string())
        };
        apply_setting(&ui, st, &key, value)
    }));
    ui.on_set_choice(with!(|ui, st, key: SharedString, index: i32| {
        let value = st
            .schema
            .as_ref()
            .and_then(|s| s.get(&key))
            .and_then(|d| d.options.get(index.max(0) as usize))
            .map(|o| o.value.clone());
        if let Some(v) = value {
            apply_setting(&ui, st, &key, v);
        }
    }));
    ui.on_reset_settings(with!(|ui, st| {
        let Some(id) = st.editing.clone() else { return };
        let _ = st.update_layout(|l| {
            if let Some(i) = l.instances.iter_mut().find(|i| i.id == id) {
                i.settings = Map::new();
            }
        });
        open_settings(&ui, st, &id);
        ui.set_status(s("Settings reset"));
    }));
    ui.on_set_autostart(with!(|ui, st, on: bool| {
        if let Err(e) = shellapi::set_autostart(on) {
            ui.set_status(s(format!("Could not change autostart: {e}")));
        }
        st.config.start_with_windows = on;
        let _ = st.config.save(&paths::config_path());
    }));
    ui.on_set_slow_on_battery(with!(|ui, st, on: bool| {
        st.config.pause_on_battery_saver = on;
        let _ = st.config.save(&paths::config_path());
        notify();
        ui.set_status(s("Saved"));
    }));
    ui.on_set_hotkey(with!(|ui, st, text: SharedString| {
        if runtime::parse_hotkey(&text).is_some() {
            ui.set_hotkey_error(s(""));
            st.config.edit_hotkey = text.trim().to_string();
            let _ = st.config.save(&paths::config_path());
        } else {
            ui.set_hotkey_error(s(
                "Use modifiers and a key, e.g. Ctrl+Alt+V or Win+Shift+F9.",
            ));
        }
    }));
    ui.on_open_folder(|| shellapi::open(&paths::data_dir().to_string_lossy()));
    ui.on_open_site(|| shellapi::open("https://vitrine-widgets.github.io/"));
    ui.on_edit_layout(with!(|ui, _st| {
        if !runtime::send_command(runtime::Runtime::CMD_EDIT) {
            ui.set_status(s("Vitrine is not running"));
        }
    }));

    // Follow layout changes made elsewhere (edit mode, the tray menu).
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_secs(1),
        with!(|ui, st| {
            if layout_stamp() != st.layout_stamp {
                st.reload_layout();
                st.packages = package::scan(&paths::packages_dir());
                refresh(&ui, st);
            }
        }),
    );

    ui.run().map_err(|e| e.to_string())
}
