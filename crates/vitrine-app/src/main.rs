#![windows_subsystem = "windows"]
//! Vitrine — desktop widgets for Windows, drawn beneath the desktop icons.

mod headless;
mod instance;
mod logger;
mod net;

use std::path::PathBuf;

fn usage() -> &'static str {
    "Vitrine 2

USAGE:
  vitrine                         run Vitrine (tray icon, widgets on the desktop)
  vitrine --dev <package-dir>     run and live-reload an unpacked widget package
  vitrine --panel                 open the control panel
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
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
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
                flag(&args, "--size").and_then(parse_pair).unwrap_or(headless::cells_to_dips(4, 2))
            };
            let result = headless::run(headless::RenderArgs {
                package: PathBuf::from(pkg),
                widget: widget.to_string(),
                size,
                scale: flag(&args, "--scale").and_then(|s| s.parse().ok()).unwrap_or(2.0),
                settings: flag(&args, "--settings").map(str::to_string),
                out: PathBuf::from(flag(&args, "--out").unwrap_or("widget.png")),
                wait_ms: flag(&args, "--wait").and_then(|s| s.parse().ok()).unwrap_or(300),
                network: args.iter().any(|a| a == "--network"),
            });
            if let Err(e) = result {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        _ => {
            attach_console();
            eprintln!("{}", usage());
        }
    }
}
