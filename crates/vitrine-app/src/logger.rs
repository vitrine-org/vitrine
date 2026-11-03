//! Minimal logger: to stderr for CLI commands, to a daily file for the app.

use std::io::Write;
use std::sync::Mutex;

struct Logger {
    file: Option<Mutex<std::fs::File>>,
    level: log::LevelFilter,
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = time_of_day();
        let line = format!("{now} {:5} {}\n", record.level(), record.args());
        match &self.file {
            Some(f) => {
                if let Ok(mut f) = f.lock() {
                    let _ = f.write_all(line.as_bytes());
                }
            }
            None => eprint!("{line}"),
        }
    }

    fn flush(&self) {}
}

fn time_of_day() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

pub fn init_stderr() {
    let level = if std::env::var_os("VITRINE_DEBUG").is_some() {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    let _ = log::set_boxed_logger(Box::new(Logger { file: None, level }));
    log::set_max_level(level);
}

/// Logs to `%APPDATA%\Vitrine\logs\vitrine-YYYY-MM-DD.log`, keeping 7 days.
pub fn init_file() {
    let dir = vitrine_widgets::paths::logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let day = &time_of_day()[..10];
    let path = dir.join(format!("vitrine-{day}.log"));
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut logs: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "log"))
            .collect();
        logs.sort();
        while logs.len() > 7 {
            let _ = std::fs::remove_file(logs.remove(0));
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
        .map(Mutex::new);
    let level = if std::env::var_os("VITRINE_DEBUG").is_some() {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    let _ = log::set_boxed_logger(Box::new(Logger { file, level }));
    log::set_max_level(level);
}
