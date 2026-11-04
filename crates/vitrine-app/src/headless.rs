//! `vitrine render`: runs one widget off-screen and writes a PNG.
//! Used for previews, documentation and golden tests.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vitrine_render::backdrop::BackdropCache;
use vitrine_render::compose::{render_offscreen, write_png};
use vitrine_render::gpu::Gpu;
use vitrine_render::style::Transform2D;
use vitrine_system::{Metric, Sampler};
use vitrine_widgets::package::{self, InstalledPackage};

use crate::glass::{self, Backdrop};
use crate::instance::{Effect, WidgetInstance};

#[derive(Debug, PartialEq)]
pub struct RenderArgs {
    pub package: PathBuf,
    pub widget: String,
    pub size: (f32, f32),
    pub scale: f32,
    pub settings: Option<String>,
    pub out: PathBuf,
    pub wait_ms: u64,
    pub network: bool,
    /// Image behind `backdropFilter` glass (default: a soft gradient).
    pub backdrop: Option<PathBuf>,
}

pub const CELL: f32 = 64.0;
pub const GAP: f32 = 8.0;

/// Size in DIPs of a widget spanning `w` × `h` grid cells.
pub fn cells_to_dips(w: u32, h: u32) -> (f32, f32) {
    let f = |n: u32| n as f32 * CELL + n.saturating_sub(1) as f32 * GAP;
    (f(w), f(h))
}

/// Opens an unpacked package folder or extracts a `.vitrine` archive to a temp dir.
pub fn open_package(path: &Path) -> Result<InstalledPackage, String> {
    if path.is_dir() {
        return package::open_dir(path).map_err(|e| e.to_string());
    }
    let tmp = std::env::temp_dir().join(format!("vitrine-render-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    package::install(path, &tmp).map_err(|e| e.to_string())
}

/// Settings for gallery previews on a neutral tile: a widget's text shadow
/// (a boolean `shadow` setting, as the themes have) reads as a dark halo there,
/// so previews turn it off. `None` when the widget has no such setting.
pub fn preview_settings(pkg: &InstalledPackage, widget_id: &str) -> Option<String> {
    let widget = pkg.widget(widget_id)?;
    let schema = pkg.read_settings_schema(widget).ok()??;
    let def = schema.get("shadow")?;
    (def.kind == vitrine_widgets::settings::SettingType::Boolean)
        .then(|| r#"{"shadow":false}"#.to_string())
}

/// Whether a preview belongs on a light tile: the widget draws dark text
/// straight on the wallpaper (a theme with `theme` light, or auto in light
/// mode, and `background` none). Everything else reads on the dark tile.
pub fn light_tile(pkg: &InstalledPackage, widget_id: &str, settings: Option<&str>) -> bool {
    let mut values = pkg
        .widget(widget_id)
        .and_then(|w| pkg.read_settings_schema(w).ok().flatten())
        .map(|s| s.defaults())
        .unwrap_or_default();
    if let Some(o) = settings
        .and_then(|s| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(s).ok())
    {
        values.extend(o);
    }
    let text = |k: &str| values.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let dark_text = match text("theme") {
        "light" => true,
        "auto" | "system" => !vitrine_script::appearance::is_dark(),
        _ => false,
    };
    dark_text && text("background") == "none"
}

/// Off-screen renders end up as published previews: they never show this PC's
/// name.
fn anonymize(snap: &mut serde_json::Value) {
    if let Some(sys) = snap.get_mut("system").and_then(|s| s.as_object_mut())
        && sys.contains_key("hostname")
    {
        sys.insert("hostname".into(), "DESKTOP".into());
    }
}

/// Runs a widget off-screen for `wait_ms` and returns `(width, height, rgba)`.
/// `settings` overrides the schema defaults. Needs COM on the calling thread.
#[allow(clippy::too_many_arguments)]
pub fn render_rgba(
    pkg: &InstalledPackage,
    widget_id: &str,
    size: (f32, f32),
    scale: f32,
    settings: Option<&str>,
    wait_ms: u64,
    network: bool,
    backdrop: &Backdrop,
) -> Result<(u32, u32, Vec<u8>), String> {
    let widget = pkg
        .widget(widget_id)
        .ok_or_else(|| format!("no widget `{widget_id}`"))?
        .clone();
    let mut values = match pkg
        .read_settings_schema(&widget)
        .map_err(|e| e.to_string())?
    {
        Some(schema) => schema.defaults(),
        None => serde_json::Map::new(),
    };
    if let Some(s) = settings {
        let overrides: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(s).map_err(|e| format!("settings: {e}"))?;
        values.extend(overrides);
    }
    let gpu = Gpu::new(false).map_err(|e| e.to_string())?;
    let dc = gpu.new_context().map_err(|e| e.to_string())?;
    let mut inst = WidgetInstance::new(
        &gpu,
        pkg,
        widget_id,
        "preview",
        &serde_json::Value::Object(values).to_string(),
        "{}",
        size,
    )?;
    if let Some(err) = &inst.failed {
        return Err(err.clone());
    }

    let mut sampler = Sampler::new();
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    let mut subscribed: Vec<Metric> = Vec::new();
    let mut primed = false;
    loop {
        for effect in inst.pump() {
            match effect {
                Effect::Subscribe { metrics, .. } => {
                    subscribed = metrics.iter().filter_map(|m| m.parse().ok()).collect();
                    primed = false;
                }
                Effect::Fetch {
                    id,
                    url,
                    method,
                    headers,
                    body,
                } => {
                    let result = if network {
                        crate::net::perform(id, &method, &url, &headers, body.as_deref())
                    } else {
                        serde_json::json!({ "id": id, "error": "network disabled (use --network)" })
                    };
                    inst.deliver("fetch", &result.to_string());
                }
                Effect::StorageSet { .. } | Effect::OpenUrl(_) => {}
                Effect::Prompt { id, .. } => {
                    inst.deliver(
                        "prompt",
                        &serde_json::json!({ "id": id, "value": null }).to_string(),
                    );
                }
            }
        }
        if !subscribed.is_empty() && !primed {
            // CPU and network rates need two samples.
            sampler.sample(&subscribed);
            std::thread::sleep(Duration::from_millis(400));
            let mut snap = sampler.sample(&subscribed);
            anonymize(&mut snap);
            inst.deliver("system", &snap.to_string());
            primed = true;
            continue;
        }
        if Instant::now() >= deadline {
            break;
        }
        let wait = inst.script.next_timer_in().unwrap_or(u64::MAX).min(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64,
        );
        std::thread::sleep(Duration::from_millis(wait.min(50)));
        inst.fire_timers();
    }
    inst.pump();

    let (pw, ph) = (
        (size.0 * scale).round() as u32,
        (size.1 * scale).round() as u32,
    );
    let now = inst.now() + 10_000.0; // settle running transitions
    let filters = inst.renderer.prepare(now).to_vec();
    let mut glass_cache = BackdropCache::new();
    if !filters.is_empty() {
        glass_cache
            .store
            .set_sources(vec![glass::preview_source(backdrop, pw, ph, scale)]);
        glass_cache.ensure(&dc, &gpu.wic, glass::PREVIEW, &filters);
    }
    let view = glass_cache.view(glass::PREVIEW, Transform2D::IDENTITY);
    let rgba = render_offscreen(&dc, pw, ph, |dc| {
        inst.renderer
            .paint(&gpu, dc, Transform2D::scale(scale, scale), now, Some(&view))
    })
    .map_err(|e| e.to_string())?;
    Ok((pw, ph, rgba))
}

pub fn run(args: RenderArgs) -> Result<(), String> {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
    }
    if let Some(p) = &args.backdrop
        && !p.is_file()
    {
        return Err(format!("backdrop image not found: {}", p.display()));
    }
    let pkg = open_package(&args.package)?;
    let (pw, ph, rgba) = render_rgba(
        &pkg,
        &args.widget,
        args.size,
        args.scale,
        args.settings.as_deref(),
        args.wait_ms,
        args.network,
        &match &args.backdrop {
            Some(p) => Backdrop::Image(p.clone()),
            None => Backdrop::Default,
        },
    )?;
    write_png(&args.out, pw, ph, &rgba).map_err(|e| e.to_string())?;
    println!(
        "rendered {}/{} at {}x{} px → {}",
        pkg.manifest.id,
        args.widget,
        pw,
        ph,
        args.out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_to_dips_counts_gaps_between_cells() {
        assert_eq!(cells_to_dips(1, 1), (CELL, CELL));
        assert_eq!(
            cells_to_dips(4, 2),
            (4.0 * 64.0 + 3.0 * 8.0, 2.0 * 64.0 + 8.0)
        );
        assert_eq!(cells_to_dips(2, 3), (136.0, 208.0));
        // Zero cells is empty, not negative.
        assert_eq!(cells_to_dips(0, 0), (0.0, 0.0));
    }

    /// An unpacked pack with a theme-like widget (`text`) and a plain one.
    fn pack(dir: &Path) -> InstalledPackage {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("vitrine.json"),
            r#"{ "id": "dev.test.tiles", "name": "T", "version": "1.0.0", "widgets": [
                 { "id": "text", "name": "Text", "entry": "a.js", "settings": "text.json" },
                 { "id": "plain", "name": "Plain", "entry": "a.js" } ] }"#,
        )
        .unwrap();
        std::fs::write(dir.join("a.js"), "").unwrap();
        std::fs::write(
            dir.join("text.json"),
            r#"[ { "title": "Look", "settings": {
                 "theme": { "type": "string", "default": "dark" },
                 "background": { "type": "string", "default": "none" },
                 "shadow": { "type": "boolean", "default": true } } } ]"#,
        )
        .unwrap();
        package::open_dir(dir).unwrap()
    }

    #[test]
    fn renders_hide_the_host_name() {
        let mut snap =
            serde_json::json!({ "system": { "hostname": "MY-PC", "os": "W" }, "cpu": 1 });
        anonymize(&mut snap);
        assert_eq!(snap["system"]["hostname"], "DESKTOP");
        assert_eq!(snap["system"]["os"], "W");
        let mut other = serde_json::json!({ "cpu": 1 });
        anonymize(&mut other);
        assert_eq!(other, serde_json::json!({ "cpu": 1 }));
    }

    #[test]
    fn previews_drop_the_text_shadow_and_pick_the_opposite_tile() {
        let dir = std::env::temp_dir().join(format!("vitrine-tiles-{}", std::process::id()));
        let p = pack(&dir);
        assert_eq!(
            preview_settings(&p, "text").as_deref(),
            Some(r#"{"shadow":false}"#)
        );
        assert_eq!(preview_settings(&p, "plain"), None);

        // Light text by default: dark tile. Dark text on the wallpaper: light tile.
        assert!(!light_tile(&p, "text", None));
        assert!(light_tile(&p, "text", Some(r#"{"theme":"light"}"#)));
        // On a card the widget brings its own background.
        assert!(!light_tile(
            &p,
            "text",
            Some(r#"{"theme":"light","background":"acrylic"}"#)
        ));
        assert!(!light_tile(&p, "plain", Some(r#"{"theme":"light"}"#)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
