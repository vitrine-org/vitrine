//! Text prompts for widgets. Widgets never get keyboard focus (they live
//! beneath the desktop icons), so `prompt()` opens a small Fluent dialog in a
//! helper process (`vitrine --prompt`) next to the widget and returns the text.

use std::process::{Command, Stdio};

use serde_json::Value;
use slint::ComponentHandle;

use crate::panel::PromptWindow;

/// Parses `--at x,y` (physical pixels).
pub fn parse_at(s: &str) -> Option<(i32, i32)> {
    let (x, y) = s.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Reads the dialog's answer from its standard output.
pub fn parse_answer(stdout: &str) -> Option<String> {
    let v: Value = serde_json::from_str(stdout.lines().last()?.trim()).ok()?;
    v.get("value")?.as_str().map(str::to_string)
}

/// Shows the prompt and waits for the answer (`None` when cancelled).
/// Blocks: call it from a worker thread.
pub fn ask(options: &str, at: (i32, i32)) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let out = Command::new(exe)
        .args(["--prompt", options, "--at", &format!("{},{}", at.0, at.1)])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| log::warn!("prompt: {e}"))
        .ok()?;
    parse_answer(&String::from_utf8_lossy(&out.stdout))
}

/// Limits what a widget can put in the dialog.
fn text(v: &Value, key: &str, max: usize) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

/// The `--prompt` process: shows the dialog and prints `{"value": …}`.
pub fn run(options: &str, at: Option<(i32, i32)>) -> Result<(), String> {
    let v: Value = serde_json::from_str(options).unwrap_or_default();
    let ui = PromptWindow::new().map_err(|e| e.to_string())?;
    crate::i18n::apply_to_slint();
    ui.set_ui_font(crate::i18n::ui_font().into());
    let heading = text(&v, "title", 80);
    if !heading.is_empty() {
        ui.set_heading(heading.into());
    }
    ui.set_placeholder(text(&v, "placeholder", 120).into());
    ui.set_initial(text(&v, "value", 2000).into());
    let answer = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
    let a = answer.clone();
    ui.on_accepted(move |s| {
        *a.borrow_mut() = Some(s.to_string());
        let _ = slint::quit_event_loop();
    });
    ui.on_cancelled(|| {
        let _ = slint::quit_event_loop();
    });
    if let Some((x, y)) = at {
        ui.window().set_position(slint::PhysicalPosition::new(x, y));
    }
    ui.show().map_err(|e| e.to_string())?;
    slint::run_event_loop().map_err(|e| e.to_string())?;
    let value = answer.borrow().clone();
    println!("{}", serde_json::json!({ "value": value }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_is_two_integers() {
        assert_eq!(parse_at("10,20"), Some((10, 20)));
        assert_eq!(parse_at(" -5 , 7 "), Some((-5, 7)));
        assert_eq!(parse_at("10"), None);
        assert_eq!(parse_at("a,b"), None);
    }

    #[test]
    fn answers_come_from_the_last_line() {
        assert_eq!(
            parse_answer("{\"value\":\"Buy milk\"}\n"),
            Some("Buy milk".into())
        );
        assert_eq!(parse_answer("noise\n{\"value\":\"x\"}"), Some("x".into()));
        assert_eq!(parse_answer("{\"value\":null}"), None);
        assert_eq!(parse_answer(""), None);
        assert_eq!(parse_answer("garbage"), None);
    }

    #[test]
    fn widget_text_is_bounded_and_clean() {
        let v = serde_json::json!({ "title": "a\u{7}b\nc", "value": "x".repeat(5000) });
        assert_eq!(text(&v, "title", 80), "abc");
        assert_eq!(text(&v, "value", 2000).len(), 2000);
        assert_eq!(text(&v, "missing", 10), "");
    }
}
