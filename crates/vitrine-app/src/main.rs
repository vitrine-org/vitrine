#![windows_subsystem = "windows"]
//! Vitrine — desktop widgets for Windows, drawn beneath the desktop icons.

mod edit;
mod grid;
mod headless;
mod instance;
mod logger;
mod net;
mod panel;
mod peek;
mod power;
mod runtime;
mod shellapi;

use std::path::PathBuf;

fn usage() -> &'static str {
    "Vitrine 2

USAGE:
  vitrine                         run Vitrine (tray icon, widgets on the desktop)
  vitrine --dev <package-dir>     run and live-reload an unpacked widget package
  vitrine --panel                 open the control panel
  vitrine --edit | --quit         toggle edit mode / quit the running instance
  vitrine install <file.vitrine>  install a widget package
  vitrine render <package> <widget> [--cells WxH | --size WxH] [--scale S]
                 [--settings JSON] [--wait MS] [--network] --out <file.png>
  vitrine --version"
}

/// Attaches to the parent console so CLI subcommands can print (the exe is a GUI app).
fn attach_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn parse_pair(s: &str) -> Option<(f32, f32)> {
    let (a, b) = s.split_once(['x', 'X'])?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// Opens the control panel (a separate process), optionally on one widget instance.
pub fn open_panel(instance: Option<&str>) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--panel");
    if let Some(id) = instance {
        cmd.args(["--instance", id]);
    }
    if let Err(e) = cmd.spawn() {
        log::error!("cannot open the control panel: {e}");
    }
}

/// Returns false if another Vitrine is already running in this session.
fn single_instance() -> bool {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        let handle = CreateMutexW(None, true, windows::core::w!("Local\\Vitrine.Singleton"));
        // Leaked on purpose: the mutex lives as long as the process.
        handle.is_ok() && GetLastError() != ERROR_ALREADY_EXISTS
    }
}

fn install(path: &str) -> i32 {
    match vitrine_widgets::package::install(
        std::path::Path::new(path),
        &vitrine_widgets::paths::packages_dir(),
    ) {
        Ok(p) => {
            println!(
                "installed {} {} ({} widgets)",
                p.manifest.id,
                p.manifest.version,
                p.manifest.widgets.len()
            );
            runtime::notify_reload();
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str);
    match cmd {
        Some("--version") | Some("-V") => {
            attach_console();
            println!("vitrine {}", env!("CARGO_PKG_VERSION"));
        }
        Some("--help") | Some("-h") => {
            attach_console();
            println!("{}", usage());
        }
        Some("render") => {
            attach_console();
            logger::init_stderr();
            let positional: Vec<&String> = args[1..]
                .iter()
                .enumerate()
                .filter(|(i, a)| !a.starts_with("--") && (*i == 0 || !args[*i].starts_with("--")))
                .map(|(_, a)| a)
                .collect();
            let (Some(pkg), Some(widget)) = (positional.first(), positional.get(1)) else {
                eprintln!("{}", usage());
                std::process::exit(2);
            };
            let size = if let Some(c) = flag(&args, "--cells").and_then(parse_pair) {
                headless::cells_to_dips(c.0 as u32, c.1 as u32)
            } else {
                flag(&args, "--size")
                    .and_then(parse_pair)
                    .unwrap_or(headless::cells_to_dips(4, 2))
            };
            let result = headless::run(headless::RenderArgs {
                package: PathBuf::from(pkg),
                widget: widget.to_string(),
                size,
                scale: flag(&args, "--scale")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(2.0),
                settings: flag(&args, "--settings").map(str::to_string),
                out: PathBuf::from(flag(&args, "--out").unwrap_or("widget.png")),
                wait_ms: flag(&args, "--wait")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(300),
                network: args.iter().any(|a| a == "--network"),
            });
            if let Err(e) = result {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Some("install") => {
            attach_console();
            let Some(path) = args.get(1) else {
                eprintln!("{}", usage());
                std::process::exit(2);
            };
            std::process::exit(install(path));
        }
        Some("--edit") | Some("--quit") | Some("--gallery") => {
            let cmd = match cmd {
                Some("--edit") => runtime::Runtime::CMD_EDIT,
                Some("--gallery") => runtime::Runtime::CMD_GALLERY,
                _ => runtime::Runtime::CMD_QUIT,
            };
            if !runtime::send_command(cmd) {
                attach_console();
                eprintln!("Vitrine is not running");
                std::process::exit(1);
            }
        }
        Some("--panel") => {
            logger::init_file();
            if let Err(e) = panel::run(flag(&args, "--instance").map(str::to_string)) {
                log::error!("control panel: {e}");
                std::process::exit(1);
            }
        }
        Some("--dev") => {
            attach_console();
            logger::init_stderr();
            let Some(dir) = args.get(1).map(PathBuf::from) else {
                eprintln!("{}", usage());
                std::process::exit(2);
            };
            let dir = dir
                .canonicalize()
                .map(|p| {
                    // Drop the verbatim prefix so paths match the ones Windows reports.
                    let s = p.to_string_lossy();
                    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
                })
                .unwrap_or(dir);
            if let Err(e) = vitrine_widgets::package::open_dir(&dir) {
                eprintln!("error: {}: {e}", dir.display());
                std::process::exit(1);
            }
            if !single_instance() {
                eprintln!("Vitrine is already running: quit it first (vitrine --quit)");
                std::process::exit(1);
            }
            let _ = runtime::DEV_DIR.set(dir.clone());
            println!(
                "dev mode: {} (Ctrl+C or `vitrine --quit` to stop)",
                dir.display()
            );
            if let Err(e) = runtime::run() {
                eprintln!("fatal: {e}");
                std::process::exit(1);
            }
        }
        None | Some("--background") => {
            logger::init_file();
            if !single_instance() {
                log::info!("already running; asking it to reload");
                runtime::notify_reload();
                return;
            }
            log::info!("Vitrine {} starting", env!("CARGO_PKG_VERSION"));
            if let Err(e) = runtime::run() {
                log::error!("fatal: {e}");
                std::process::exit(1);
            }
            log::info!("Vitrine stopped");
        }
        Some(path) if path.ends_with(".vitrine") => {
            // Double-clicked package (file association).
            logger::init_file();
            let code = install(path);
            if code == 0 && !runtime::notify_reload() {
                let _ = std::process::Command::new(std::env::current_exe().unwrap()).spawn();
            }
        }
        _ => {
            attach_console();
            eprintln!("{}", usage());
        }
    }
}
