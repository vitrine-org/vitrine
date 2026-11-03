//! `vitrine render`: runs one widget off-screen and writes a PNG.
//! Used for previews, documentation and golden tests.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vitrine_render::compose::{render_offscreen, write_png};
use vitrine_render::gpu::Gpu;
use vitrine_render::style::Transform2D;
use vitrine_system::{Metric, Sampler};
use vitrine_widgets::package::{self, InstalledPackage};

use crate::instance::{Effect, WidgetInstance};

pub struct RenderArgs {
    pub package: PathBuf,
    pub widget: String,
    pub size: (f32, f32),
    pub scale: f32,
    pub settings: Option<String>,
    pub out: PathBuf,
    pub wait_ms: u64,
    pub network: bool,
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

pub fn run(args: RenderArgs) -> Result<(), String> {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
    }
    let pkg = open_package(&args.package)?;
    let widget = pkg.widget(&args.widget).ok_or_else(|| format!("no widget `{}`", args.widget))?.clone();
    let mut settings = match pkg.read_settings_schema(&widget).map_err(|e| e.to_string())? {
        Some(schema) => schema.defaults(),
        None => serde_json::Map::new(),
    };
    if let Some(s) = &args.settings {
        let overrides: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(s).map_err(|e| format!("--settings: {e}"))?;
        settings.extend(overrides);
    }
    let gpu = Gpu::new(false).map_err(|e| e.to_string())?;
    let dc = gpu.new_context().map_err(|e| e.to_string())?;
    let mut inst = WidgetInstance::new(
        &gpu,
        &pkg,
        &args.widget,
        "preview",
        &serde_json::Value::Object(settings).to_string(),
        "{}",
        args.size,
    )?;
    if let Some(err) = &inst.failed {
        return Err(err.clone());
    }

    let mut sampler = Sampler::new();
    let deadline = Instant::now() + Duration::from_millis(args.wait_ms);
    let mut subscribed: Vec<Metric> = Vec::new();
    let mut primed = false;
    loop {
        for effect in inst.pump() {
            match effect {
                Effect::Subscribe { metrics, .. } => {
                    subscribed = metrics.iter().filter_map(|m| m.parse().ok()).collect();
                    primed = false;
                }
                Effect::Fetch { id, url, method, headers, body } => {
                    let result = if args.network {
                        crate::net::perform(id, &method, &url, &headers, body.as_deref())
                    } else {
                        serde_json::json!({ "id": id, "error": "network disabled (use --network)" })
                    };
                    inst.deliver("fetch", &result.to_string());
                }
                Effect::StorageSet { .. } | Effect::OpenUrl(_) => {}
            }
        }
        if !subscribed.is_empty() && !primed {
            // CPU and network rates need two samples.
            sampler.sample(&subscribed);
            std::thread::sleep(Duration::from_millis(400));
            let snap = sampler.sample(&subscribed);
            inst.deliver("system", &snap.to_string());
            primed = true;
            continue;
        }
        if Instant::now() >= deadline {
            break;
        }
        let wait = inst.script.next_timer_in().unwrap_or(u64::MAX).min(
            deadline.saturating_duration_since(Instant::now()).as_millis() as u64,
        );
        std::thread::sleep(Duration::from_millis(wait.min(50)));
        inst.fire_timers();
    }
    inst.pump();

    let (pw, ph) = ((args.size.0 * args.scale).round() as u32, (args.size.1 * args.scale).round() as u32);
    let now = inst.now() + 10_000.0; // settle running transitions
    let rgba = render_offscreen(&dc, pw, ph, |dc| {
        inst.renderer.paint(&gpu, dc, Transform2D::scale(args.scale, args.scale), now)
    })
    .map_err(|e| e.to_string())?;
    write_png(&args.out, pw, ph, &rgba).map_err(|e| e.to_string())?;
    println!("rendered {}/{} at {}x{} px → {}", pkg.manifest.id, args.widget, pw, ph, args.out.display());
    Ok(())
}
