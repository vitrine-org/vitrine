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

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Cmd {
    Version,
    Help,
    Render(headless::RenderArgs),
    Install(String),
    /// `--edit`, `--gallery`, `--quit`: a command for the running instance.
    Send(u32),
    Panel {
        instance: Option<String>,
    },
    /// `--dev <dir>` (not canonicalized yet).
    Dev(PathBuf),
    /// No arguments or `--background`: run Vitrine.
    Run,
    /// A double-clicked package (file association).
    Open(String),
    /// A required argument is missing: usage on stderr, exit code 2.
    MissingArgs,
    /// Unknown arguments: usage on stderr.
    Unknown,
}

/// Parses the arguments after the program name.
fn parse_args(args: &[String]) -> Cmd {
    let cmd = args.first().map(String::as_str);
    match cmd {
        Some("--version") | Some("-V") => Cmd::Version,
        Some("--help") | Some("-h") => Cmd::Help,
        Some("render") => {
            let positional: Vec<&String> = args[1..]
                .iter()
                .enumerate()
                .filter(|(i, a)| !a.starts_with("--") && (*i == 0 || !args[*i].starts_with("--")))
                .map(|(_, a)| a)
                .collect();
            let (Some(pkg), Some(widget)) = (positional.first(), positional.get(1)) else {
                return Cmd::MissingArgs;
            };
            let size = if let Some(c) = flag(args, "--cells").and_then(parse_pair) {
                headless::cells_to_dips(c.0 as u32, c.1 as u32)
            } else {
                flag(args, "--size")
                    .and_then(parse_pair)
                    .unwrap_or(headless::cells_to_dips(4, 2))
            };
            Cmd::Render(headless::RenderArgs {
                package: PathBuf::from(pkg),
                widget: widget.to_string(),
                size,
                scale: flag(args, "--scale")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(2.0),
                settings: flag(args, "--settings").map(str::to_string),
                out: PathBuf::from(flag(args, "--out").unwrap_or("widget.png")),
                wait_ms: flag(args, "--wait")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(300),
                network: args.iter().any(|a| a == "--network"),
            })
        }
        Some("install") => match args.get(1) {
            Some(path) => Cmd::Install(path.clone()),
            None => Cmd::MissingArgs,
        },
        Some("--edit") => Cmd::Send(runtime::Runtime::CMD_EDIT),
        Some("--gallery") => Cmd::Send(runtime::Runtime::CMD_GALLERY),
        Some("--quit") => Cmd::Send(runtime::Runtime::CMD_QUIT),
        Some("--panel") => Cmd::Panel {
            instance: flag(args, "--instance").map(str::to_string),
        },
        Some("--dev") => match args.get(1) {
            Some(dir) => Cmd::Dev(PathBuf::from(dir)),
            None => Cmd::MissingArgs,
        },
        None | Some("--background") => Cmd::Run,
        Some(path) if path.ends_with(".vitrine") => Cmd::Open(path.to_string()),
        _ => Cmd::Unknown,
    }
}

/// Drops the verbatim prefix (`\\?\`) of a canonical path so it matches the
/// paths Windows reports.
fn strip_verbatim(p: &std::path::Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args) {
        Cmd::Version => {
            attach_console();
            println!("vitrine {}", env!("CARGO_PKG_VERSION"));
        }
        Cmd::Help => {
            attach_console();
            println!("{}", usage());
        }
        Cmd::Render(render) => {
            attach_console();
            logger::init_stderr();
            if let Err(e) = headless::run(render) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Cmd::Install(path) => {
            attach_console();
            std::process::exit(install(&path));
        }
        Cmd::Send(cmd) => {
            if !runtime::send_command(cmd) {
                attach_console();
                eprintln!("Vitrine is not running");
                std::process::exit(1);
            }
        }
        Cmd::Panel { instance } => {
            logger::init_file();
            if let Err(e) = panel::run(instance) {
                log::error!("control panel: {e}");
                std::process::exit(1);
            }
        }
        Cmd::Dev(dir) => {
            attach_console();
            logger::init_stderr();
            let dir = dir
                .canonicalize()
                .map(|p| strip_verbatim(&p))
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
        Cmd::Run => {
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
        Cmd::Open(path) => {
            logger::init_file();
            let code = install(&path);
            if code == 0 && !runtime::notify_reload() {
                let _ = std::process::Command::new(std::env::current_exe().unwrap()).spawn();
            }
        }
        Cmd::MissingArgs => {
            attach_console();
            eprintln!("{}", usage());
            std::process::exit(2);
        }
        Cmd::Unknown => {
            attach_console();
            eprintln!("{}", usage());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cmd {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_args(&args)
    }

    fn render(args: &[&str]) -> headless::RenderArgs {
        match parse(args) {
            Cmd::Render(r) => r,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn run_by_default() {
        assert_eq!(parse(&[]), Cmd::Run);
        assert_eq!(parse(&["--background"]), Cmd::Run);
    }

    #[test]
    fn info_flags() {
        assert_eq!(parse(&["--version"]), Cmd::Version);
        assert_eq!(parse(&["-V"]), Cmd::Version);
        assert_eq!(parse(&["--help"]), Cmd::Help);
        assert_eq!(parse(&["-h"]), Cmd::Help);
        assert_eq!(parse(&["--nope"]), Cmd::Unknown);
        assert_eq!(parse(&["widget.zip"]), Cmd::Unknown);
    }

    #[test]
    fn commands_for_the_running_instance() {
        use runtime::Runtime;
        assert_eq!(parse(&["--edit"]), Cmd::Send(Runtime::CMD_EDIT));
        assert_eq!(parse(&["--gallery"]), Cmd::Send(Runtime::CMD_GALLERY));
        assert_eq!(parse(&["--quit"]), Cmd::Send(Runtime::CMD_QUIT));
    }

    #[test]
    fn panel() {
        assert_eq!(parse(&["--panel"]), Cmd::Panel { instance: None });
        assert_eq!(
            parse(&["--panel", "--instance", "a1b2"]),
            Cmd::Panel {
                instance: Some("a1b2".into())
            }
        );
        // `--instance` without a value.
        assert_eq!(
            parse(&["--panel", "--instance"]),
            Cmd::Panel { instance: None }
        );
    }

    #[test]
    fn install_and_open() {
        assert_eq!(
            parse(&["install", r"C:\x\pack.vitrine"]),
            Cmd::Install(r"C:\x\pack.vitrine".into())
        );
        assert_eq!(parse(&["install"]), Cmd::MissingArgs);
        assert_eq!(
            parse(&[r"C:\Users\me\Downloads\Weather Pack.vitrine"]),
            Cmd::Open(r"C:\Users\me\Downloads\Weather Pack.vitrine".into())
        );
    }

    #[test]
    fn dev() {
        assert_eq!(
            parse(&["--dev", r"widgets\clock"]),
            Cmd::Dev(PathBuf::from(r"widgets\clock"))
        );
        assert_eq!(parse(&["--dev"]), Cmd::MissingArgs);
    }

    #[test]
    fn strips_the_verbatim_prefix() {
        assert_eq!(
            strip_verbatim(std::path::Path::new(r"\\?\C:\dev\clock")),
            PathBuf::from(r"C:\dev\clock")
        );
        assert_eq!(
            strip_verbatim(std::path::Path::new(r"C:\dev\clock")),
            PathBuf::from(r"C:\dev\clock")
        );
        // Only a leading prefix is removed.
        assert_eq!(
            strip_verbatim(std::path::Path::new(r"C:\a\\?\b")),
            PathBuf::from(r"C:\a\\?\b")
        );
        // A canonicalized directory loses it.
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        let s = strip_verbatim(&here);
        assert!(!s.to_string_lossy().starts_with(r"\\?\"), "{s:?}");
        assert!(s.is_dir());
    }

    #[test]
    fn render_defaults() {
        let r = render(&["render", "pkg", "clock"]);
        assert_eq!(
            r,
            headless::RenderArgs {
                package: PathBuf::from("pkg"),
                widget: "clock".into(),
                size: headless::cells_to_dips(4, 2),
                scale: 2.0,
                settings: None,
                out: PathBuf::from("widget.png"),
                wait_ms: 300,
                network: false,
            }
        );
    }

    #[test]
    fn render_options() {
        let r = render(&[
            "render",
            "--scale",
            "1.5",
            "pkg",
            "--cells",
            "3x2",
            "clock",
            "--settings",
            r#"{"a":1}"#,
            "--wait",
            "50",
            "--network",
            "--out",
            "o.png",
        ]);
        assert_eq!(r.package, PathBuf::from("pkg"));
        assert_eq!(r.widget, "clock");
        assert_eq!(r.size, headless::cells_to_dips(3, 2));
        assert_eq!(r.scale, 1.5);
        assert_eq!(r.settings.as_deref(), Some(r#"{"a":1}"#));
        assert_eq!(r.wait_ms, 50);
        assert!(r.network);
        assert_eq!(r.out, PathBuf::from("o.png"));
    }

    #[test]
    fn render_size_and_bad_values() {
        let r = render(&["render", "p", "w", "--size", "300 X 150.5"]);
        assert_eq!(r.size, (300.0, 150.5));
        // `--cells` wins over `--size`.
        let r = render(&["render", "p", "w", "--size", "1x1", "--cells", "2x2"]);
        assert_eq!(r.size, headless::cells_to_dips(2, 2));
        // Unparsable values fall back to the defaults.
        let r = render(&[
            "render", "p", "w", "--cells", "big", "--scale", "x", "--wait", "-1",
        ]);
        assert_eq!(r.size, headless::cells_to_dips(4, 2));
        assert_eq!(r.scale, 2.0);
        assert_eq!(r.wait_ms, 300);
    }

    #[test]
    fn render_needs_package_and_widget() {
        assert_eq!(parse(&["render"]), Cmd::MissingArgs);
        assert_eq!(parse(&["render", "pkg"]), Cmd::MissingArgs);
        // Flag values are not positional.
        assert_eq!(
            parse(&["render", "pkg", "--out", "x.png"]),
            Cmd::MissingArgs
        );
    }

    #[test]
    fn pairs_and_flags() {
        assert_eq!(parse_pair("4x2"), Some((4.0, 2.0)));
        assert_eq!(parse_pair(" 4 X 2 "), Some((4.0, 2.0)));
        assert_eq!(parse_pair("4"), None);
        assert_eq!(parse_pair("ax2"), None);
        let args: Vec<String> = ["a", "--k", "v", "--last"].map(String::from).to_vec();
        assert_eq!(flag(&args, "--k"), Some("v"));
        assert_eq!(flag(&args, "--last"), None);
        assert_eq!(flag(&args, "--missing"), None);
    }
}
