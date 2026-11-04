//! The control panel (`vitrine --panel`): a separate Slint process that edits
//! `layout.json` / `settings.json` and asks the running Vitrine to reload.
//! Settings changes therefore apply live, without restarting widgets.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde_json::{Map, Value};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use vitrine_widgets::config::Config;
use vitrine_widgets::gallery::{self, FAVOURITES, Prefs};
use vitrine_widgets::layout::{Instance, Layout};
use vitrine_widgets::package::{self, InstalledPackage};
use vitrine_widgets::paths;
use vitrine_widgets::settings::{SettingDef, SettingType, SettingsSchema};

use crate::grid::Geometry;
use crate::i18n::{self, t, tf, tn};
use crate::runtime;
use crate::search::{Facts, Filter, SizeClass};
use crate::shellapi;
use crate::store::{self, Catalogue, PackState, StorePack};

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
    /// The same packages in their original language (search matches both).
    originals: Vec<InstalledPackage>,
    gallery: Gallery,
    store: StoreState,
}

/// The gallery: favourites, then one collapsible section per pack. Cards
/// are filtered in place so they can animate between positions.
#[derive(Default)]
struct Gallery {
    /// One per installed widget, in package order: its card and what the filters look at.
    rows: Vec<LibraryRow>,
    facts: Vec<Facts>,
    sections: Rc<VecModel<GallerySection>>,
    /// Parallel to `sections`: favourites first, then the visible packs.
    parts: Vec<Part>,
    /// Package of each chip (None for "All packages").
    chips: Vec<Option<String>>,
    chip_model: Rc<VecModel<PackChip>>,
    /// Packages and versions the model was built from.
    signature: String,
    prefs: Prefs,
}

/// A section's cards, and the widget (index into `Gallery::rows`) behind each.
struct Part {
    id: String,
    model: Rc<VecModel<LibraryRow>>,
    members: Vec<usize>,
}

#[derive(Default)]
struct StoreState {
    catalogue: Option<Catalogue>,
    /// Downloaded during this session (otherwise it is the cached copy).
    fresh: bool,
    loading: bool,
    rows: Rc<VecModel<StoreRow>>,
    /// Preview of each row, as a catalogue path.
    row_previews: Vec<String>,
    tags: Vec<Option<String>>,
    tag_model: Rc<VecModel<PackChip>>,
    /// Pack on the detail page and its widgets' preview paths.
    detail: Option<String>,
    detail_previews: Vec<String>,
    previews: HashMap<String, slint::Image>,
    requested: HashSet<String>,
    installing: Option<String>,
}

/// Installed packages localised, and as written (for search in both languages).
fn scan_both() -> (Vec<InstalledPackage>, Vec<InstalledPackage>) {
    let originals = package::scan(&paths::packages_dir());
    let mut packages = originals.clone();
    for p in &mut packages {
        p.manifest.localize(i18n::language());
    }
    (packages, originals)
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
            originals: Vec::new(),
            gallery: Gallery::default(),
            store: StoreState::default(),
        }
    }

    fn load() -> State {
        let (packages, originals) = scan_both();
        State {
            originals,
            gallery: Gallery::default(),
            store: StoreState::default(),
            packages,
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

    fn rescan(&mut self) {
        (self.packages, self.originals) = scan_both();
    }

    fn installed_version(&self, id: &str) -> Option<&str> {
        self.package(id).map(|p| p.manifest.version.as_str())
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
                &crate::glass::Backdrop::Wallpaper,
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

/// The language selector: "Windows default", then each language in its own name.
fn set_languages(ui: &PanelWindow, config: &Config) {
    let mut names = vec![s(t("language.system"))];
    names.extend(i18n::LANGUAGES.iter().map(|(_, name)| s(*name)));
    ui.set_languages(ModelRc::new(VecModel::from(names)));
    let chosen = config.language.as_deref().unwrap_or(i18n::SYSTEM);
    let index = i18n::LANGUAGES
        .iter()
        .position(|(c, _)| c.eq_ignore_ascii_case(chosen))
        .map_or(0, |i| i as i32 + 1);
    ui.set_language_index(index);
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
        slot: 0,
        shown: true,
        ..Default::default()
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
                t("row.main_monitor")
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
                    None => tf("row.missing_package", &[("id", &i.package)]),
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

/// Columns and rows of the main monitor's grid (size variants must fit it).
fn grid_limit(st: &State) -> (i32, i32) {
    let monitors = vitrine_desktop::monitors::monitors();
    monitors
        .iter()
        .find(|m| m.primary)
        .or(monitors.first())
        .map(|m| {
            let g = Geometry::for_monitor(&st.layout.grid, m);
            (g.cols, g.rows)
        })
        .unwrap_or((26, 14))
}

/// "2×1 · 4×2": the sizes a widget comes in.
fn variants_text(w: &vitrine_widgets::manifest::WidgetManifest, grid: (i32, i32)) -> String {
    let c = |a: [u32; 2]| (a[0] as i32, a[1] as i32);
    gallery::size_variants(c(w.size.min), c(w.size.default), c(w.size.max), grid)
        .iter()
        .map(|(w, h)| format!("{w}×{h}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn library_rows(st: &State) -> Vec<LibraryRow> {
    let grid = grid_limit(st);
    let prefs = &st.gallery.prefs;
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
                slot: 0,
                shown: true,
                sizes: s(variants_text(w, grid)),
                favourite: prefs.is_favourite(&p.manifest.id, &w.id),
            })
        })
        .collect()
}

/// What the gallery filters on, per widget, in `library_rows` order.
fn library_facts(packages: &[InstalledPackage], originals: &[InstalledPackage]) -> Vec<Facts> {
    use vitrine_widgets::manifest::Permission;
    packages
        .iter()
        .flat_map(|p| {
            let original = originals.iter().find(|o| o.manifest.id == p.manifest.id);
            p.manifest.widgets.iter().map(move |w| {
                let mut text = vec![
                    w.name.clone(),
                    w.description.clone(),
                    p.manifest.name.clone(),
                ];
                if let Some(o) = original {
                    text.push(o.manifest.name.clone());
                    if let Some(ow) = o.widget(&w.id) {
                        text.push(ow.name.clone());
                        text.push(ow.description.clone());
                    }
                }
                Facts {
                    text,
                    pack: p.manifest.id.clone(),
                    size: (w.size.default[0] as i32, w.size.default[1] as i32),
                    interactive: w.interactive,
                    online: w
                        .permissions
                        .iter()
                        .any(|p| matches!(p, Permission::Net(_))),
                }
            })
        })
        .collect()
}

/// Avatar colour of a package chip, stable for its id.
fn tint(id: &str) -> slint::Color {
    let c = gallery::tint(id);
    slint::Color::from_rgb_u8((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

/// Positions of the results: matching items take the next slot; the others
/// keep their old one (so they fade out where they were). Returns the count.
fn assign_slots(shown: &[bool], old_slots: &mut [i32]) -> usize {
    let mut next = 0;
    for (slot, &on) in old_slots.iter_mut().zip(shown) {
        if on {
            *slot = next;
            next += 1;
        }
    }
    next as usize
}

/// Rebuilds the gallery when the installed packages changed.
fn rebuild_gallery(ui: &PanelWindow, st: &mut State) {
    let signature: String = st
        .packages
        .iter()
        .map(|p| format!("{}@{};", p.manifest.id, p.manifest.version))
        .chain(std::iter::once(i18n::language().to_string()))
        .collect();
    if signature == st.gallery.signature && !st.gallery.rows.is_empty() {
        return;
    }
    st.gallery.signature = signature;
    st.gallery.prefs = Prefs::load(&paths::gallery_path());
    st.gallery.rows = library_rows(st);
    st.gallery.facts = library_facts(&st.packages, &st.originals);
    build_sections(ui, st);
}

fn save_prefs(st: &State) {
    if let Err(e) = st.gallery.prefs.save(&paths::gallery_path()) {
        log::warn!("gallery.json: {e}");
    }
}

/// Builds the sections (favourites, then the visible packs in the user's
/// order), the package chips and the hidden-packs menu.
fn build_sections(ui: &PanelWindow, st: &mut State) {
    let installed: Vec<&str> = st.packages.iter().map(|p| p.manifest.id.as_str()).collect();
    let g = &mut st.gallery;
    let visible = g.prefs.visible(&installed);
    let in_view = |i: &usize| visible.iter().any(|v| *v == g.rows[*i].package.as_str());
    let favourites: Vec<usize> = g
        .prefs
        .favourites
        .iter()
        .filter_map(|k| {
            g.rows
                .iter()
                .position(|r| gallery::key(&r.package, &r.widget) == *k)
        })
        .filter(in_view)
        .collect();
    let part = |id: &str, members: Vec<usize>| Part {
        id: id.to_string(),
        model: Rc::new(VecModel::from(
            members
                .iter()
                .map(|&i| g.rows[i].clone())
                .collect::<Vec<_>>(),
        )),
        members,
    };
    let mut parts = vec![part(FAVOURITES, favourites)];
    for id in &visible {
        let members = (0..g.rows.len())
            .filter(|&i| g.rows[i].package.as_str() == id)
            .collect();
        parts.push(part(id, members));
    }
    let name_of = |id: &str| {
        st.packages
            .iter()
            .find(|p| p.manifest.id == id)
            .map(|p| p.manifest.name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    let n = parts.len();
    let sections: Vec<GallerySection> = parts
        .iter()
        .enumerate()
        .map(|(k, part)| {
            let fav = part.id == FAVOURITES;
            let name = if fav {
                t("gallery.favourites")
            } else {
                name_of(&part.id)
            };
            GallerySection {
                id: s(part.id.as_str()),
                initial: s(initial(&name)),
                name: s(name),
                tint: tint(&part.id),
                favourites: fav,
                count: part.members.len() as i32,
                shown: !part.members.is_empty(),
                expanded: !g.prefs.is_collapsed(&part.id),
                first: k <= 1,
                last: k + 1 == n,
                items: ModelRc::from(part.model.clone()),
            }
        })
        .collect();
    let total = parts.iter().skip(1).map(|p| p.members.len()).sum::<usize>();
    g.parts = parts;
    g.sections = Rc::new(VecModel::from(sections));

    let selected = g
        .chips
        .get(ui.get_pack_index().max(0) as usize)
        .cloned()
        .flatten()
        .filter(|id| visible.contains(id));
    g.chips = std::iter::once(None)
        .chain(visible.iter().map(|id| Some(id.clone())))
        .collect();
    let chip = |id: &str| PackChip {
        id: s(id),
        name: s(name_of(id)),
        initial: s(initial(&name_of(id))),
        tint: tint(id),
        count: 0,
    };
    let chips: Vec<PackChip> = std::iter::once(PackChip {
        name: s(t("gallery.all_packs")),
        ..Default::default()
    })
    .chain(visible.iter().map(|id| chip(id)))
    .collect();
    g.chip_model = Rc::new(VecModel::from(chips));
    let hidden: Vec<PackChip> = installed
        .iter()
        .filter(|id| g.prefs.is_hidden(id))
        .map(|id| chip(id))
        .collect();
    ui.set_pack_index(g.chips.iter().position(|c| *c == selected).unwrap_or(0) as i32);
    ui.set_sections(ModelRc::from(g.sections.clone()));
    ui.set_pack_chips(ModelRc::from(g.chip_model.clone()));
    ui.set_hidden_packs(ModelRc::new(VecModel::from(hidden)));
    ui.set_library_total(total as i32);
    ui.set_list_view(g.prefs.list);
    apply_gallery_filter(ui, st);
}

/// Stars or unstars a widget: its cards update in place and the favourites
/// section gains or loses a card.
fn toggle_favourite(ui: &PanelWindow, st: &mut State, package: &str, widget: &str) {
    let g = &mut st.gallery;
    let on = g.prefs.toggle_favourite(package, widget);
    let Some(i) = g
        .rows
        .iter()
        .position(|r| r.package == package && r.widget == widget)
    else {
        return;
    };
    g.rows[i].favourite = on;
    for part in &g.parts {
        for (j, _) in part.members.iter().enumerate().filter(|(_, m)| **m == i) {
            if let Some(mut row) = part.model.row_data(j) {
                row.favourite = on;
                part.model.set_row_data(j, row);
            }
        }
    }
    let in_view = g.parts.iter().skip(1).any(|p| p.members.contains(&i));
    if let Some(fav) = g.parts.first_mut() {
        if on && in_view {
            fav.members.push(i);
            fav.model.push(LibraryRow {
                slot: fav.members.len() as i32 - 1,
                ..g.rows[i].clone()
            });
        } else if let Some(j) = fav.members.iter().position(|&m| m == i) {
            fav.members.remove(j);
            fav.model.remove(j);
        }
    }
    save_prefs(st);
    apply_gallery_filter(ui, st);
}

fn gallery_filter(ui: &PanelWindow, st: &State) -> Filter {
    Filter {
        query: ui.get_gallery_query().to_string(),
        pack: st
            .gallery
            .chips
            .get(ui.get_pack_index().max(0) as usize)
            .cloned()
            .flatten(),
        size: match ui.get_size_filter() {
            1 => Some(SizeClass::Small),
            2 => Some(SizeClass::Medium),
            3 => Some(SizeClass::Large),
            _ => None,
        },
        interactive: ui.get_only_interactive(),
        online: ui.get_only_online(),
    }
}

/// Shows the widgets that match the search and filters, opens the sections
/// with matches and hides the others, and updates the counts.
fn apply_gallery_filter(ui: &PanelWindow, st: &State) {
    let filter = gallery_filter(ui, st);
    let filtering = !filter.query.trim().is_empty()
        || filter.size.is_some()
        || filter.interactive
        || filter.online;
    let g = &st.gallery;
    let mut count = 0;
    let mut any_expanded = false;
    for (k, part) in g.parts.iter().enumerate() {
        let shown: Vec<bool> = part
            .members
            .iter()
            .map(|&i| filter.accepts_any_pack(&g.facts[i]))
            .collect();
        let mut slots: Vec<i32> = (0..part.model.row_count())
            .map(|i| part.model.row_data(i).map_or(0, |r| r.slot))
            .collect();
        let n = assign_slots(&shown, &mut slots);
        for (i, (&on, &slot)) in shown.iter().zip(&slots).enumerate() {
            if let Some(mut row) = part.model.row_data(i)
                && (row.shown != on || row.slot != slot)
            {
                row.shown = on;
                row.slot = slot;
                part.model.set_row_data(i, row);
            }
        }
        let view = gallery::section_view(&g.prefs, &part.id, n, filtering, filter.pack.as_deref());
        if view.shown && part.id != FAVOURITES {
            count += n;
        }
        any_expanded |= view.shown && !g.prefs.is_collapsed(&part.id);
        if let Some(mut sec) = g.sections.row_data(k)
            && (sec.count != n as i32 || sec.shown != view.shown || sec.expanded != view.expanded)
        {
            sec.count = n as i32;
            sec.shown = view.shown;
            sec.expanded = view.expanded;
            g.sections.set_row_data(k, sec);
        }
    }
    for (i, pack) in g.chips.iter().enumerate() {
        let n = g
            .parts
            .iter()
            .skip(1)
            .filter(|p| pack.as_ref().is_none_or(|id| *id == p.id))
            .flat_map(|p| &p.members)
            .filter(|&&m| filter.accepts_any_pack(&g.facts[m]))
            .count() as i32;
        if let Some(mut chip) = g.chip_model.row_data(i)
            && chip.count != n
        {
            chip.count = n;
            g.chip_model.set_row_data(i, chip);
        }
    }
    ui.set_any_expanded(any_expanded);
    ui.set_result_count(count as i32);
    ui.set_result_text(s(tn("row.widgets", count as u64)));
}

fn package_rows(st: &State) -> Vec<PackageRow> {
    st.packages
        .iter()
        .map(|p| {
            let mut detail = format!(
                "{} · {}",
                tf("row.version", &[("version", &p.manifest.version)]),
                tn("row.widgets", p.manifest.widgets.len() as u64)
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
    let schema = pkg.read_settings_schema(&w).ok().flatten().map(|mut s| {
        s.localize(i18n::language());
        s
    });
    let mut values = schema.as_ref().map(|s| s.defaults()).unwrap_or_default();
    values.extend(inst.settings.clone());
    st.editing = Some(id.to_string());
    st.schema = schema;
    st.values = values;
    ui.set_settings_title(s(w.name.as_str()));
    ui.set_settings_subtitle(s(tf(
        "row.settings_subtitle",
        &[("pack", &pkg.manifest.name)],
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
                ui.set_status(s(tf("status.could_not_save", &[("error", &e)])));
            }
            String::new()
        }
        Err(errors) => {
            st.values = values;
            errors
                .into_iter()
                .find(|(k, _)| k == key)
                .map(|(_, m)| error_text(&schema, key, m))
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

/// A validation error in the UI language: the widget's own (localised)
/// `patternMessage`, or a generic message for the engine's checks.
fn error_text(schema: &SettingsSchema, key: &str, message: String) -> String {
    if schema
        .get(key)
        .and_then(|d| d.pattern_message.as_ref())
        .is_some_and(|m| *m == message)
    {
        return message;
    }
    t(match message.as_str() {
        "expected a number" => "setting.error_number",
        "value is not one of the allowed options" => "setting.error_option",
        _ => "setting.error_invalid",
    })
}

fn refresh(ui: &PanelWindow, st: &mut State) {
    ui.set_instances(ModelRc::new(VecModel::from(instance_rows(st))));
    rebuild_gallery(ui, st);
    ui.set_packages(ModelRc::new(VecModel::from(package_rows(st))));
    if st.store.catalogue.is_some() {
        update_store_states(ui, st);
    }
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
    let mut placed = Err(tf(
        "status.no_space",
        &[("name", &w.name), ("size", &format!("{cw}×{ch}"))],
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
            placed = Ok(tf("status.added", &[("name", &w.name)]));
        }
    })?;
    placed
}

// ── store ─────────────────────────────────────────────────────────────

/// The panel and its state.
type Shared = (slint::Weak<PanelWindow>, Rc<RefCell<State>>);

thread_local! {
    /// The panel and its state, for results that come back from worker threads.
    static PANEL: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

/// Runs `f` on the UI thread with the panel and its state.
fn on_ui(f: impl FnOnce(&PanelWindow, &mut State) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        PANEL.with(|p| {
            if let Some((weak, st)) = &*p.borrow()
                && let Some(ui) = weak.upgrade()
            {
                f(&ui, &mut st.borrow_mut());
            }
        });
    });
}

/// "918 KB", "1,2 MB": sizes with the decimal separator of the UI language.
fn format_size(bytes: u64) -> String {
    let (key, value) = if bytes < 1024 * 1024 {
        (
            "store.size_kb",
            format!("{}", (bytes as f64 / 1024.0).round().max(1.0)),
        )
    } else {
        (
            "store.size_mb",
            format!("{:.1}", bytes as f64 / (1024.0 * 1024.0)),
        )
    };
    let comma = matches!(i18n::language(), "it" | "fr" | "de" | "es" | "pt-BR" | "ru");
    let value = if comma {
        value.replace('.', ",")
    } else {
        value
    };
    tf(key, &[("size", &value)])
}

fn state_code(state: PackState) -> i32 {
    match state {
        PackState::NotInstalled => 0,
        PackState::Installed => 1,
        PackState::UpdateAvailable => 2,
    }
}

fn pack_state_of(st: &State, pack: &StorePack) -> PackState {
    store::pack_state(&pack.version, st.installed_version(&pack.id))
}

/// Packs with a newer version in the catalogue than installed.
fn count_updates(st: &State) -> i32 {
    st.store.catalogue.as_ref().map_or(0, |c| {
        c.packs
            .iter()
            .filter(|p| pack_state_of(st, p) == PackState::UpdateAvailable)
            .count() as i32
    })
}

/// Shows a cached preview, or downloads it in the background.
fn store_preview(st: &mut State, rel: &str) -> slint::Image {
    if let Some(img) = st.store.previews.get(rel) {
        return img.clone();
    }
    if !rel.is_empty() && st.store.requested.insert(rel.to_string()) {
        let rel = rel.to_string();
        let _ = std::thread::Builder::new()
            .name("vitrine-store-preview".into())
            .spawn(move || {
                let path = store::preview(&rel);
                on_ui(move |ui, st| {
                    let img = path
                        .and_then(|p| slint::Image::load_from_path(&p).ok())
                        .unwrap_or_default();
                    st.store.previews.insert(rel.clone(), img.clone());
                    for (i, key) in st.store.row_previews.iter().enumerate() {
                        if *key == rel
                            && let Some(mut row) = st.store.rows.row_data(i)
                        {
                            row.preview = img.clone();
                            st.store.rows.set_row_data(i, row);
                        }
                    }
                    let widgets = ui.get_store_detail_widgets();
                    for (i, key) in st.store.detail_previews.iter().enumerate() {
                        if *key == rel
                            && let Some(mut row) = widgets.row_data(i)
                        {
                            row.preview = img.clone();
                            widgets.set_row_data(i, row);
                        }
                    }
                });
            });
    }
    slint::Image::default()
}

fn store_summary(pack: &StorePack) -> String {
    format!(
        "{} · {}",
        tn("row.widgets", pack.widgets.len() as u64),
        format_size(pack.size)
    )
}

/// Builds the Store grid and its tag chips from the catalogue.
fn rebuild_store(ui: &PanelWindow, st: &mut State) {
    let Some(catalogue) = st.store.catalogue.clone() else {
        return;
    };
    let mut rows = Vec::new();
    let mut previews = Vec::new();
    for pack in &catalogue.packs {
        let rel = pack
            .widgets
            .iter()
            .find_map(|w| w.preview.clone())
            .unwrap_or_default();
        rows.push(StoreRow {
            id: s(pack.id.as_str()),
            name: s(pack.name.as_str()),
            author: s(tf("store.by", &[("author", &pack.author)])),
            description: s(pack.description.as_str()),
            summary: s(store_summary(pack)),
            official: pack.official,
            state: state_code(pack_state_of(st, pack)),
            preview: store_preview(st, &rel),
            slot: 0,
            shown: true,
        });
        previews.push(rel);
    }
    st.store.row_previews = previews;
    st.store.rows = Rc::new(VecModel::from(rows));
    // Tags, most used first.
    let mut tags: Vec<(String, usize)> = Vec::new();
    for tag in catalogue.packs.iter().flat_map(|p| &p.tags) {
        match tags.iter_mut().find(|(t, _)| t == tag) {
            Some((_, n)) => *n += 1,
            None => tags.push((tag.clone(), 1)),
        }
    }
    tags.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let selected = st
        .store
        .tags
        .get(ui.get_store_tag().max(0) as usize)
        .cloned()
        .flatten();
    st.store.tags = std::iter::once(None)
        .chain(tags.iter().map(|(t, _)| Some(t.clone())))
        .collect();
    let chips: Vec<PackChip> = std::iter::once(PackChip {
        name: s(t("store.all_tags")),
        ..Default::default()
    })
    .chain(tags.iter().map(|(tag, _)| PackChip {
        id: s(tag.as_str()),
        name: s(tag.as_str()),
        ..Default::default()
    }))
    .collect();
    st.store.tag_model = Rc::new(VecModel::from(chips));
    ui.set_store_tag(
        st.store
            .tags
            .iter()
            .position(|t| *t == selected)
            .unwrap_or(0) as i32,
    );
    ui.set_store_packs(ModelRc::from(st.store.rows.clone()));
    ui.set_store_tags(ModelRc::from(st.store.tag_model.clone()));
    ui.set_store_updates(count_updates(st));
    apply_store_filter(ui, st);
}

/// True when `pack` matches the Store search box and tag chip.
fn store_matches(pack: &StorePack, query: &str, tag: Option<&str>) -> bool {
    if tag.is_some_and(|t| !pack.tags.iter().any(|x| x == t)) {
        return false;
    }
    let mut fields: Vec<&str> = vec![&pack.name, &pack.description, &pack.author, &pack.id];
    fields.extend(pack.tags.iter().map(String::as_str));
    for w in &pack.widgets {
        fields.push(&w.name);
        fields.push(&w.description);
    }
    crate::search::matches(query, &fields)
}

fn apply_store_filter(ui: &PanelWindow, st: &State) {
    let Some(catalogue) = &st.store.catalogue else {
        return;
    };
    let query = ui.get_store_query().to_string();
    let tag = st
        .store
        .tags
        .get(ui.get_store_tag().max(0) as usize)
        .cloned()
        .flatten();
    let rows = &st.store.rows;
    let shown: Vec<bool> = catalogue
        .packs
        .iter()
        .map(|p| store_matches(p, &query, tag.as_deref()))
        .collect();
    let mut slots: Vec<i32> = (0..rows.row_count())
        .map(|i| rows.row_data(i).map_or(0, |r| r.slot))
        .collect();
    let count = assign_slots(&shown, &mut slots);
    for (i, (&on, &slot)) in shown.iter().zip(&slots).enumerate() {
        if let Some(mut row) = rows.row_data(i)
            && (row.shown != on || row.slot != slot)
        {
            row.shown = on;
            row.slot = slot;
            rows.set_row_data(i, row);
        }
    }
    for (i, t) in st.store.tags.iter().enumerate() {
        let n = catalogue
            .packs
            .iter()
            .filter(|p| store_matches(p, &query, t.as_deref()))
            .count() as i32;
        if let Some(mut chip) = st.store.tag_model.row_data(i)
            && chip.count != n
        {
            chip.count = n;
            st.store.tag_model.set_row_data(i, chip);
        }
    }
    ui.set_store_count(count as i32);
    ui.set_store_count_text(s(tn("store.packs", count as u64)));
}

/// Installed / update badges after packages changed.
fn update_store_states(ui: &PanelWindow, st: &mut State) {
    let Some(catalogue) = st.store.catalogue.clone() else {
        return;
    };
    for (i, pack) in catalogue.packs.iter().enumerate() {
        let code = state_code(pack_state_of(st, pack));
        if let Some(mut row) = st.store.rows.row_data(i)
            && row.state != code
        {
            row.state = code;
            st.store.rows.set_row_data(i, row);
        }
    }
    ui.set_store_updates(count_updates(st));
    if let Some(id) = st.store.detail.clone()
        && st.store.installing.is_none()
    {
        fill_store_detail(ui, st, &id);
    }
}

/// Downloads the catalogue in the background. `quiet` keeps the page as it
/// is while loading (the daily update check).
fn fetch_store(ui: &PanelWindow, st: &mut State, quiet: bool) {
    if st.store.loading {
        return;
    }
    st.store.loading = true;
    if !quiet && st.store.catalogue.is_none() {
        ui.set_store_state(0);
    }
    let _ = std::thread::Builder::new()
        .name("vitrine-store".into())
        .spawn(move || {
            let result = store::fetch_catalogue();
            on_ui(move |ui, st| {
                st.store.loading = false;
                match result {
                    Ok(c) => {
                        st.store.catalogue = Some(c);
                        st.store.fresh = true;
                        ui.set_store_stale(false);
                        ui.set_store_state(1);
                        rebuild_store(ui, st);
                        if let Some(id) = st.store.detail.clone()
                            && ui.get_page() == 7
                        {
                            fill_store_detail(ui, st, &id);
                        }
                    }
                    Err(e) => {
                        log::warn!("store: {e}");
                        // Shown when the Store page is (or becomes) visible.
                        if st.store.catalogue.is_some() {
                            ui.set_store_stale(true);
                            ui.set_store_state(1);
                        } else {
                            ui.set_store_state(2);
                        }
                    }
                }
            });
        });
}

/// The Store page became visible.
fn open_store(ui: &PanelWindow, st: &mut State) {
    if st.store.catalogue.is_some() {
        ui.set_store_state(1);
        if st.store.rows.row_count() == 0 {
            rebuild_store(ui, st);
        }
    }
    if !st.store.fresh {
        fetch_store(ui, st, false);
    }
}

fn permission_rows(pack: &StorePack) -> Vec<PermissionRow> {
    let mut rows = Vec::new();
    for perm in pack.permissions() {
        let users: Vec<&str> = pack
            .widgets
            .iter()
            .filter(|w| w.permissions.contains(&perm))
            .map(|w| w.name.as_str())
            .collect();
        let detail = tf("perm.used_by", &[("widgets", &users.join(", "))]);
        let (glyph, title) = if let Some(host) = perm.strip_prefix("net:") {
            ("\u{E774}", tf("perm.net", &[("host", host)]))
        } else {
            let metric = perm.strip_prefix("system.").unwrap_or(&perm);
            let glyph = match metric {
                "battery" => "\u{E83F}",
                "drives" => "\u{EDA2}",
                "network" => "\u{E839}",
                "info" => "\u{E770}",
                _ => "\u{E9D9}",
            };
            let key = format!("perm.{metric}");
            let known = vitrine_widgets::manifest::SYSTEM_METRICS.contains(&metric);
            (glyph, if known { t(&key) } else { perm.clone() })
        };
        rows.push(PermissionRow {
            glyph: s(glyph),
            title: s(title),
            detail: s(detail),
        });
    }
    if rows.is_empty() {
        rows.push(PermissionRow {
            glyph: s("\u{E72E}"),
            title: s(t("perm.none")),
            detail: s(t("perm.none_detail")),
        });
    }
    rows
}

/// Fills the pack detail page.
fn fill_store_detail(ui: &PanelWindow, st: &mut State, id: &str) {
    let Some(pack) = st
        .store
        .catalogue
        .as_ref()
        .and_then(|c| c.packs.iter().find(|p| p.id == id))
        .cloned()
    else {
        return;
    };
    let state = pack_state_of(st, &pack);
    let index = st
        .store
        .catalogue
        .as_ref()
        .and_then(|c| c.packs.iter().position(|p| p.id == id))
        .unwrap_or(0);
    let mut row = st.store.rows.row_data(index).unwrap_or_default();
    row.state = state_code(state);
    ui.set_store_detail(row);
    ui.set_store_detail_meta(s(format!(
        "{} · {} · {}",
        tf("store.by", &[("author", &pack.author)]),
        tf("row.version", &[("version", &pack.version)]),
        format_size(pack.size)
    )));
    ui.set_store_detail_description(s(pack.description.as_str()));
    let mut widgets = Vec::new();
    let mut previews = Vec::new();
    for w in &pack.widgets {
        let rel = w.preview.clone().unwrap_or_default();
        widgets.push(StoreWidgetRow {
            name: s(w.name.as_str()),
            description: s(w.description.as_str()),
            size: s(format!("{}×{}", w.size.default[0], w.size.default[1])),
            interactive: w.interactive,
            online: w.permissions.iter().any(|p| p.starts_with("net:")),
            preview: store_preview(st, &rel),
        });
        previews.push(rel);
    }
    st.store.detail_previews = previews;
    ui.set_store_detail_widgets(ModelRc::new(VecModel::from(widgets)));
    ui.set_store_detail_permissions(ModelRc::new(VecModel::from(permission_rows(&pack))));
    let mut facts = vec![
        (t("store.fact_version"), pack.version.clone()),
        (t("store.fact_author"), pack.author.clone()),
        (
            t("store.fact_license"),
            pack.license
                .clone()
                .unwrap_or_else(|| t("store.license_unknown")),
        ),
        (t("store.fact_size"), format_size(pack.size)),
    ];
    if let Some(date) = pack.published.as_deref().and_then(|d| d.get(..10)) {
        facts.push((t("store.fact_updated"), date.to_string()));
    }
    facts.push((t("store.fact_repo"), format!("github.com/{}", pack.repo)));
    if !pack.tags.is_empty() {
        facts.push((t("store.fact_tags"), pack.tags.join(", ")));
    }
    ui.set_store_detail_facts(ModelRc::new(VecModel::from(
        facts
            .into_iter()
            .map(|(label, value)| DetailRow {
                label: s(label),
                value: s(value),
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_store_action(s(match state {
        PackState::NotInstalled => t("store.install"),
        PackState::UpdateAvailable => tf("store.update", &[("version", &pack.version)]),
        PackState::Installed => String::new(),
    }));
    ui.set_store_note(s(if pack.supported() {
        String::new()
    } else {
        tf(
            "store.needs_engine",
            &[("version", pack.min_engine.as_deref().unwrap_or(""))],
        )
    }));
    if st.store.detail.as_deref() != Some(id) {
        ui.set_store_error(s(""));
    }
    st.store.detail = Some(id.to_string());
}

fn download_error_text(e: &store::DownloadError) -> String {
    match e {
        store::DownloadError::NotAllowed(_) => t("store.error_blocked"),
        store::DownloadError::Network(_) => t("store.error_network"),
        store::DownloadError::TooLarge => t("store.error_too_large"),
        store::DownloadError::Checksum => t("store.error_checksum"),
    }
}

/// Downloads, verifies and installs the pack on the detail page.
fn install_from_store(ui: &PanelWindow, st: &mut State) {
    let Some(id) = st.store.detail.clone() else {
        return;
    };
    if st.store.installing.is_some() {
        return;
    }
    let Some(pack) = st
        .store
        .catalogue
        .as_ref()
        .and_then(|c| c.packs.iter().find(|p| p.id == id))
        .cloned()
    else {
        return;
    };
    if !pack.supported() {
        return;
    }
    let update = st.installed_version(&id).is_some();
    st.store.installing = Some(id);
    ui.set_store_error(s(""));
    ui.set_store_busy(true);
    ui.set_store_progress(0.0);
    ui.set_store_progress_text(s(tf("store.downloading", &[("percent", "0")])));
    let _ = std::thread::Builder::new()
        .name("vitrine-store-install".into())
        .spawn(move || {
            let mut shown = -1i64;
            let result = store::download(&pack, |got, total| {
                let percent = (got * 100 / total.max(1)).min(100) as i64;
                if percent != shown {
                    shown = percent;
                    on_ui(move |ui, _| {
                        ui.set_store_progress(percent as f32 / 100.0);
                        ui.set_store_progress_text(s(tf(
                            "store.downloading",
                            &[("percent", &percent.to_string())],
                        )));
                    });
                }
            })
            .map_err(|e| {
                log::warn!("store download {}: {e:?}", pack.id);
                download_error_text(&e)
            })
            .and_then(|bytes| {
                on_ui(|ui, _| {
                    ui.set_store_progress(1.0);
                    ui.set_store_progress_text(s(t("store.installing")));
                });
                store::install_bytes(&pack, &bytes, &paths::packages_dir())
                    .map_err(|e| tf("status.could_not_install", &[("error", &e)]))
            });
            on_ui(move |ui, st| {
                st.store.installing = None;
                ui.set_store_busy(false);
                match result {
                    Ok(installed) => {
                        let mut m = installed.manifest.clone();
                        m.localize(i18n::language());
                        let args = [("name", m.name.as_str()), ("version", m.version.as_str())];
                        ui.set_status(s(if update {
                            tf("status.updated", &args)
                        } else {
                            tf("status.installed", &args)
                        }));
                        st.rescan();
                        notify();
                        refresh(ui, st);
                    }
                    Err(e) => ui.set_store_error(s(e)),
                }
            });
        });
}

/// Once a day, looks for pack updates so the Store item can show a badge.
fn check_updates(ui: &PanelWindow, st: &mut State) {
    match store::cached_catalogue() {
        Some((c, age)) => {
            st.store.catalogue = Some(c);
            ui.set_store_updates(count_updates(st));
            if age > store::CHECK_EVERY {
                fetch_store(ui, st, true);
            }
        }
        None => fetch_store(ui, st, true),
    }
}

/// Shows the system file picker for `.vitrine` packages.
fn pick_package() -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::*;
    use windows::core::{HSTRING, PCWSTR, w};
    let packages = HSTRING::from(t("dialog.filter_packages"));
    let all = HSTRING::from(t("dialog.filter_all"));
    let title = HSTRING::from(t("dialog.install_title"));
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let filters = [
            COMDLG_FILTERSPEC {
                pszName: PCWSTR(packages.as_ptr()),
                pszSpec: w!("*.vitrine"),
            },
            COMDLG_FILTERSPEC {
                pszName: PCWSTR(all.as_ptr()),
                pszSpec: w!("*.*"),
            },
        ];
        dialog.SetFileTypes(&filters).ok()?;
        dialog.SetTitle(&title).ok()?;
        dialog.Show(None).ok()?;
        let item = dialog.GetResult().ok()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let out = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        out.map(Into::into)
    }
}

/// Shows the page a launch asked for (`store`, `gallery`, ...) and, with an
/// instance id, that widget's settings.
fn navigate(ui: &PanelWindow, st: &mut State, page: Option<&str>, instance: Option<&str>) {
    match page {
        Some("gallery" | "add") => ui.set_page(1),
        Some("packages") => ui.set_page(2),
        Some("general") => ui.set_page(3),
        Some("store") => {
            ui.set_page(6);
            open_store(ui, st);
        }
        _ => {}
    }
    if let Some(id) = instance {
        open_settings(ui, st, id);
    }
}

pub fn run(instance: Option<String>, page: Option<String>) -> Result<(), String> {
    let req = crate::bridge::Request {
        page: page.clone(),
        instance: instance.clone(),
    };
    if !crate::bridge::claim_or_forward(&req) {
        // Another panel took the request and came to the front.
        return Ok(());
    }
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
    }
    let ui = PanelWindow::new().map_err(|e| e.to_string())?;
    i18n::apply_to_slint();
    ui.set_ui_font(s(i18n::ui_font()));
    ui.set_theme(match std::env::var("VITRINE_THEME").as_deref() {
        Ok("light") => 1,
        Ok("dark") => 2,
        _ => 0,
    });
    let st = Rc::new(RefCell::new(State::load()));
    PANEL.with(|p| *p.borrow_mut() = Some((ui.as_weak(), st.clone())));
    {
        let st = &mut st.borrow_mut();
        set_languages(&ui, &st.config);
        ui.set_version(s(env!("CARGO_PKG_VERSION")));
        ui.set_autostart(shellapi::autostart_enabled());
        ui.set_slow_on_battery(st.config.pause_on_battery_saver);
        ui.set_hotkey(s(st.config.edit_hotkey.as_str()));
        refresh(&ui, st);
        check_updates(&ui, st);
        navigate(&ui, st, page.as_deref(), instance.as_deref());
    }
    {
        let weak = ui.as_weak();
        let st = st.clone();
        crate::bridge::listen(move |req| {
            if let Some(ui) = weak.upgrade() {
                navigate(
                    &ui,
                    &mut st.borrow_mut(),
                    req.page.as_deref(),
                    req.instance.as_deref(),
                );
                crate::bridge::bring_to_front();
            }
        });
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
            Ok(()) => ui.set_status(s(t("status.widget_removed"))),
            Err(e) => ui.set_status(s(tf("status.could_not_save", &[("error", &e)]))),
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
            ui.set_status(s(t("status.place_hint")));
        } else {
            ui.set_status(s(t("status.not_running")));
        }
    }));
    ui.on_uninstall(with!(|ui, st, id: SharedString| {
        let id = id.to_string();
        match package::uninstall(&id, &paths::packages_dir()) {
            Ok(()) => {
                let _ = st.update_layout(|l| l.instances.retain(|i| i.package != id));
                st.rescan();
                ui.set_status(s(t("status.uninstalled")));
            }
            Err(e) => ui.set_status(s(tf(
                "status.could_not_uninstall",
                &[("error", &e.to_string())],
            ))),
        }
        refresh(&ui, st);
    }));
    ui.on_install_file(with!(|ui, st| {
        let Some(path) = pick_package() else { return };
        match package::install(&path, &paths::packages_dir()) {
            Ok(p) => {
                let mut m = p.manifest.clone();
                m.localize(i18n::language());
                ui.set_status(s(tf(
                    "status.installed",
                    &[("name", &m.name), ("version", &m.version)],
                )));
                st.rescan();
                notify();
                ui.set_page(1);
            }
            Err(e) => ui.set_status(s(tf(
                "status.could_not_install",
                &[("error", &i18n::package_error(&e))],
            ))),
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
        ui.set_status(s(t("status.settings_reset")));
    }));
    ui.on_set_autostart(with!(|ui, st, on: bool| {
        if let Err(e) = shellapi::set_autostart(on) {
            ui.set_status(s(tf(
                "status.could_not_autostart",
                &[("error", &e.to_string())],
            )));
        }
        st.config.start_with_windows = on;
        let _ = st.config.save(&paths::config_path());
    }));
    ui.on_set_slow_on_battery(with!(|ui, st, on: bool| {
        st.config.pause_on_battery_saver = on;
        let _ = st.config.save(&paths::config_path());
        notify();
        ui.set_status(s(t("status.saved")));
    }));
    ui.on_set_language(with!(|ui, st, index: i32| {
        let code = match index {
            i if i <= 0 => i18n::SYSTEM,
            i => i18n::LANGUAGES
                .get(i as usize - 1)
                .map_or(i18n::SYSTEM, |(c, _)| *c),
        };
        st.config.language = Some(code.to_string());
        if let Err(e) = st.config.save(&paths::config_path()) {
            ui.set_status(s(tf("status.could_not_save", &[("error", &e.to_string())])));
        }
        // The panel switches now; Vitrine on reload.
        i18n::set(Some(code));
        i18n::apply_to_slint();
        ui.set_ui_font(s(i18n::ui_font()));
        set_languages(&ui, &st.config);
        st.rescan();
        refresh(&ui, st);
        if let Some(id) = st.editing.clone()
            && ui.get_page() == 4
        {
            open_settings(&ui, st, &id);
        }
        if let Some((package, widget, _)) = st.detail.clone()
            && ui.get_page() == 5
        {
            let size = ui.get_detail_size();
            open_detail(&ui, st, &package, &widget);
            ui.set_detail_size(size);
        }
        ui.set_status(s(""));
        notify();
    }));
    ui.on_set_hotkey(with!(|ui, st, text: SharedString| {
        if runtime::parse_hotkey(&text).is_some() {
            ui.set_hotkey_error(s(""));
            st.config.edit_hotkey = text.trim().to_string();
            let _ = st.config.save(&paths::config_path());
        } else {
            ui.set_hotkey_error(s(t("status.hotkey_invalid")));
        }
    }));
    ui.on_open_folder(|| shellapi::open(&paths::data_dir().to_string_lossy()));
    ui.on_filter_changed(with!(|ui, st| { apply_gallery_filter(&ui, st) }));
    ui.on_toggle_section(with!(|ui, st, id: SharedString| {
        st.gallery.prefs.toggle_collapsed(&id);
        save_prefs(st);
        apply_gallery_filter(&ui, st);
    }));
    ui.on_set_all_expanded(with!(|ui, st, expand: bool| {
        let ids: Vec<String> = st.gallery.parts.iter().map(|p| p.id.clone()).collect();
        for id in ids {
            st.gallery.prefs.set_collapsed(&id, !expand);
        }
        save_prefs(st);
        apply_gallery_filter(&ui, st);
    }));
    ui.on_toggle_favourite(with!(
        |ui, st, package: SharedString, widget: SharedString| {
            toggle_favourite(&ui, st, &package, &widget)
        }
    ));
    ui.on_move_section(with!(|ui, st, id: SharedString, delta: i32| {
        let installed: Vec<String> = st.packages.iter().map(|p| p.manifest.id.clone()).collect();
        let installed: Vec<&str> = installed.iter().map(String::as_str).collect();
        if st.gallery.prefs.move_pack(&installed, &id, delta) {
            save_prefs(st);
            build_sections(&ui, st);
        }
    }));
    ui.on_hide_section(with!(|ui, st, id: SharedString| {
        st.gallery.prefs.set_hidden(&id, true);
        save_prefs(st);
        build_sections(&ui, st);
    }));
    ui.on_show_section(with!(|ui, st, id: SharedString| {
        st.gallery.prefs.set_hidden(&id, false);
        save_prefs(st);
        build_sections(&ui, st);
    }));
    ui.on_set_list_view(with!(|_ui, st, list: bool| {
        st.gallery.prefs.list = list;
        save_prefs(st);
    }));
    ui.on_store_open(with!(|ui, st| { open_store(&ui, st) }));
    ui.on_store_retry(with!(|ui, st| {
        st.store.fresh = false;
        fetch_store(&ui, st, false);
    }));
    ui.on_store_filter_changed(with!(|ui, st| { apply_store_filter(&ui, st) }));
    ui.on_store_open_pack(with!(|ui, st, id: SharedString| {
        st.store.detail = None;
        fill_store_detail(&ui, st, &id);
        ui.set_page(7);
    }));
    ui.on_store_install(with!(|ui, st| { install_from_store(&ui, st) }));
    ui.on_store_open_repo(with!(|_ui, st| {
        let pack = st.store.detail.as_ref().and_then(|id| {
            st.store
                .catalogue
                .as_ref()
                .and_then(|c| c.packs.iter().find(|p| p.id == *id))
        });
        if let Some(url) = pack.and_then(|p| p.repo_url()) {
            shellapi::open(&url);
        }
    }));
    ui.on_edit_layout(with!(|ui, _st| {
        if !runtime::send_command(runtime::Runtime::CMD_EDIT) {
            ui.set_status(s(t("status.not_running")));
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
                st.rescan();
                refresh(&ui, st);
            }
        }),
    );

    ui.run().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;
    use vitrine_widgets::layout::Instance;
    use vitrine_widgets::manifest::{PackageManifest, WidgetManifest};

    fn widget(default: [u32; 2], min: [u32; 2], max: [u32; 2]) -> WidgetManifest {
        serde_json::from_value(serde_json::json!({
            "id": "w", "name": "W", "entry": "index.js",
            "size": { "default": default, "min": min, "max": max }
        }))
        .unwrap()
    }

    fn def(json: Value) -> SettingDef {
        serde_json::from_value(json).unwrap()
    }

    fn state(packages: Vec<InstalledPackage>, instances: Vec<Instance>) -> State {
        State {
            packages,
            layout: Layout {
                instances,
                ..Default::default()
            },
            layout_stamp: None,
            config: Config::default(),
            editing: None,
            schema: None,
            values: Map::new(),
            shown: Vec::new(),
            detail: None,
            preview_gen: Arc::new(AtomicU64::new(0)),
            preview_timer: Rc::new(slint::Timer::default()),
            originals: Vec::new(),
            gallery: Gallery::default(),
            store: StoreState::default(),
        }
    }

    fn package(json: Value) -> InstalledPackage {
        InstalledPackage {
            manifest: PackageManifest::parse(&json.to_string()).unwrap(),
            dir: std::path::PathBuf::from(r"C:\nowhere"),
        }
    }

    fn instance(id: &str, package: &str, widget: &str, monitor: &str) -> Instance {
        Instance {
            id: id.into(),
            package: package.into(),
            widget: widget.into(),
            monitor: monitor.into(),
            x: 0,
            y: 0,
            w: 3,
            h: 2,
            settings: Map::new(),
        }
    }

    #[test]
    fn size_choices_cover_min_default_and_bigger() {
        let w = widget([4, 2], [2, 2], [8, 4]);
        assert_eq!(size_choices(&w), vec![(2, 2), (4, 2), (5, 3), (8, 4)]);
    }

    #[test]
    fn size_choices_are_deduplicated_and_capped() {
        // Fixed size: one choice.
        let w = widget([3, 3], [3, 3], [3, 3]);
        assert_eq!(size_choices(&w), vec![(3, 3)]);
        // Max right above default: mid and big coincide.
        let w = widget([2, 2], [1, 1], [3, 3]);
        assert_eq!(size_choices(&w), vec![(1, 1), (2, 2), (3, 3)]);
        // Sorted by area.
        let w = widget([4, 1], [1, 1], [12, 1]);
        let c = size_choices(&w);
        assert!(c.windows(2).all(|p| p[0].0 * p[0].1 <= p[1].0 * p[1].1));
        assert_eq!(c, vec![(1, 1), (4, 1), (5, 1), (8, 1)]);
    }

    #[test]
    fn integral_numbers() {
        assert!(is_integral(&def(serde_json::json!({ "type": "number" }))));
        assert!(is_integral(&def(
            serde_json::json!({ "type": "number", "min": 0, "max": 10, "step": 2 })
        )));
        assert!(!is_integral(&def(
            serde_json::json!({ "type": "number", "min": 0, "max": 1, "step": 0.1 })
        )));
        assert!(!is_integral(&def(
            serde_json::json!({ "type": "number", "min": 0.5 })
        )));
    }

    #[test]
    fn boolean_row() {
        let d =
            def(serde_json::json!({ "type": "boolean", "label": "Seconds", "description": "d" }));
        let row = setting_row("seconds", &d, &Value::Bool(true), "Clock", false);
        assert_eq!(row.kind, 0);
        assert!(row.bool_value);
        assert_eq!(row.key, "seconds");
        assert_eq!(row.label, "Seconds");
        assert_eq!(row.section, "Clock");
        assert_eq!(row.description, "d");
        assert!(!row.enabled);
        // A non-boolean value reads as false; a missing label falls back to the key.
        let d = def(serde_json::json!({ "type": "boolean" }));
        let row = setting_row("flag", &d, &Value::from(3), "", true);
        assert!(!row.bool_value);
        assert_eq!(row.label, "flag");
    }

    #[test]
    fn integer_row() {
        let d = def(serde_json::json!({ "type": "number", "min": 1, "max": 60, "step": 5 }));
        let row = setting_row("n", &d, &Value::from(14.6), "", true);
        assert_eq!(row.kind, 1);
        assert_eq!(row.int_value, 15);
        assert_eq!((row.min, row.max, row.step), (1, 60, 5));
        // Unbounded, step defaults to 1 (and never below 1).
        let d = def(serde_json::json!({ "type": "number", "step": 0 }));
        let row = setting_row("n", &d, &Value::Null, "", true);
        assert_eq!(row.int_value, 0);
        assert_eq!((row.min, row.max, row.step), (-1_000_000, 1_000_000, 1));
    }

    #[test]
    fn decimal_and_text_rows() {
        let d = def(serde_json::json!({ "type": "number", "step": 0.25 }));
        let row = setting_row("x", &d, &Value::from(1.5), "", true);
        assert_eq!(row.kind, 4);
        assert_eq!(row.text_value, "1.5");
        let d = def(serde_json::json!({ "type": "string" }));
        let row = setting_row("city", &d, &Value::from("Rome"), "", true);
        assert_eq!(row.kind, 2);
        assert_eq!(row.text_value, "Rome");
        let row = setting_row("city", &d, &Value::Null, "", true);
        assert_eq!(row.text_value, "");
    }

    #[test]
    fn option_rows() {
        let d = def(serde_json::json!({
            "type": "string",
            "options": [ { "value": "c", "label": "Celsius" }, { "value": "f" } ]
        }));
        let row = setting_row("unit", &d, &Value::from("f"), "", true);
        assert_eq!(row.kind, 3);
        assert_eq!(row.options.row_count(), 2);
        assert_eq!(row.options.row_data(0).unwrap(), "Celsius");
        // Without a label the value is shown, unquoted.
        assert_eq!(row.options.row_data(1).unwrap(), "f");
        assert_eq!(row.selected, 1);
        // Unknown values select the first option.
        let row = setting_row("unit", &d, &Value::from("k"), "", true);
        assert_eq!(row.selected, 0);
        // Numeric options.
        let d = def(
            serde_json::json!({ "type": "number", "options": [ { "value": 1 }, { "value": 2 } ] }),
        );
        let row = setting_row("n", &d, &Value::from(2), "", true);
        assert_eq!((row.kind, row.selected), (3, 1));
        assert_eq!(row.options.row_data(0).unwrap(), "1");
    }

    #[test]
    fn settings_rows_follow_visibility() {
        let mut st = state(Vec::new(), Vec::new());
        assert!(settings_rows(&mut st).is_empty());
        st.schema = Some(
            SettingsSchema::parse(
                r#"[
                  { "title": "Look", "settings": {
                      "custom": { "type": "boolean", "default": false },
                      "color": { "type": "string", "default": "red",
                                 "visibleWhen": { "key": "custom", "value": true } } } },
                  { "title": "More", "settings": {
                      "size": { "type": "number", "default": 3,
                                "disabledWhen": { "key": "custom", "value": false } } } }
                ]"#,
            )
            .unwrap(),
        );
        let rows = settings_rows(&mut st);
        assert_eq!(st.shown, vec!["custom".to_string(), "size".to_string()]);
        // The section title is only on the first row of each section.
        assert_eq!(rows[0].section, "Look");
        assert_eq!(rows[1].section, "More");
        assert!(!rows[1].enabled);
        assert_eq!(rows[1].int_value, 3);

        st.values.insert("custom".into(), Value::Bool(true));
        let rows = settings_rows(&mut st);
        assert_eq!(st.shown, vec!["custom", "color", "size"]);
        assert_eq!(rows[1].section, "");
        assert_eq!(rows[1].text_value, "red");
        assert!(rows[2].enabled);
    }

    #[test]
    fn package_rows_describe_packages() {
        let st = state(
            vec![
                package(
                    serde_json::json!({ "id": "dev.a", "name": "A", "version": "2.1.0",
                    "author": "me", "widgets": [ { "id": "x", "name": "X", "entry": "i.js" } ] }),
                ),
                package(
                    serde_json::json!({ "id": "dev.b", "name": "B", "version": "1.0.0",
                    "widgets": [ { "id": "x", "name": "X", "entry": "i.js" },
                                 { "id": "y", "name": "Y", "entry": "i.js" } ] }),
                ),
            ],
            Vec::new(),
        );
        let rows = package_rows(&st);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "dev.a");
        assert_eq!(rows[0].detail, "Version 2.1.0 · 1 widget · me");
        assert_eq!(rows[1].detail, "Version 1.0.0 · 2 widgets");

        let lib = library_rows(&st);
        assert_eq!(lib.len(), 3);
        assert_eq!(
            (lib[2].package.as_str(), lib[2].widget.as_str()),
            ("dev.b", "y")
        );
        assert_eq!(lib[2].pack, "B");
    }

    #[test]
    fn instance_rows_describe_placed_widgets() {
        let st = state(
            vec![package(
                serde_json::json!({ "id": "dev.a", "name": "Pack A", "version": "2.0.0",
                "widgets": [ { "id": "clock", "name": "Clock", "entry": "i.js",
                               "settings": "settings.json" },
                             { "id": "plain", "name": "Plain", "entry": "i.js" } ] }),
            )],
            vec![
                instance("1", "dev.a", "clock", "primary"),
                instance("2", "dev.a", "plain", r"\\.\DISPLAY2"),
                instance("3", "dev.gone", "old", "primary"),
            ],
        );
        let rows = instance_rows(&st);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "Clock");
        assert_eq!(rows[0].detail, "Pack A · 3×2 · Main display");
        assert!(rows[0].has_settings);
        assert_eq!(rows[1].detail, "Pack A · 3×2 · DISPLAY2");
        assert!(!rows[1].has_settings);
        // A missing package keeps the widget id as its name.
        assert_eq!(rows[2].name, "old");
        assert_eq!(rows[2].detail, "Missing package dev.gone");
    }

    #[test]
    fn slots_follow_the_results_and_keep_hidden_places() {
        let mut slots = vec![0, 1, 2, 3];
        assert_eq!(assign_slots(&[true, false, true, true], &mut slots), 3);
        assert_eq!(slots, [0, 1, 1, 2], "the hidden card stays where it was");
        assert_eq!(assign_slots(&[false, true, false, false], &mut slots), 1);
        assert_eq!(slots, [0, 0, 1, 2]);
    }

    #[test]
    fn store_search_matches_names_tags_and_widgets() {
        let text = r#"{"version":1,"packs":[{"id":"dev.vitrine.mond","name":"Mond","version":"2.1.1",
            "author":"Emanuele","description":"A wide geometric weekday","repo":"vitrine-org/mond",
            "tags":["theme","minimal"],"download":"https://github.com/x/y.vitrine","size":1,
            "sha256":"6726651db3d79decb4d169275fb2c1a6f71c79d786a581c869081d017010b302",
            "widgets":[{"id":"weather","name":"Weather","size":{"default":[6,2]}}]}]}"#;
        let pack = &store::parse_catalogue(text).unwrap().packs[0];
        assert!(store_matches(pack, "", None));
        assert!(store_matches(pack, "geometric", None));
        assert!(store_matches(pack, "weather emanuele", Some("theme")));
        assert!(!store_matches(pack, "", Some("clock")));
        assert!(!store_matches(pack, "calendar", None));
    }

    #[test]
    fn library_facts_search_both_languages() {
        let original = package(serde_json::json!({
            "id": "dev.pack", "name": "Pack", "version": "1.0.0",
            "widgets": [{"id": "clock", "name": "Clock", "entry": "c.js",
                "size": {"default": [2, 2], "min": [1, 1], "max": [4, 4]},
                "permissions": ["net:example.com"]}],
            "locales": {"it": {"widgets": {"clock": {"name": "Orologio"}}}}
        }));
        let mut local = original.clone();
        local.manifest.localize("it");
        let facts = library_facts(&[local], &[original]);
        assert_eq!(facts.len(), 1);
        assert!(facts[0].online);
        let f = Filter {
            query: "clock".into(),
            ..Filter::default()
        };
        assert!(f.accepts(&facts[0]), "the English name still matches");
        let f = Filter {
            query: "orologio".into(),
            ..Filter::default()
        };
        assert!(f.accepts(&facts[0]));
    }
}
