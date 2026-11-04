//! Widget script runtime: one sandboxed QuickJS runtime per widget instance.
//!
//! The SDK (`@vitrine/sdk`) talks to the host through a single global object,
//! `__vitrine`, and receives events through a global function
//! `__vitrine_event(type, json)`. See `docs/dev/host-api.md`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rquickjs::prelude::Opt;
use rquickjs::{CaughtError, Context, Function, Object, Runtime};
use vitrine_render::scene::Op;
use vitrine_render::value::PropValue;

pub mod appearance;
pub mod locale;

/// A request from a widget script to the host application.
#[derive(Clone, Debug, PartialEq)]
pub enum HostRequest {
    Ops(Vec<Op>),
    /// Metric names (`cpu`, `memory`, …) and the wanted refresh interval.
    Subscribe {
        metrics: Vec<String>,
        interval_ms: u32,
    },
    Fetch {
        id: u32,
        url: String,
        method: String,
        headers: Vec<(String, String)>,
        body: Option<String>,
    },
    Log {
        level: LogLevel,
        message: String,
    },
    StorageSet {
        key: String,
        value: Option<String>,
    },
    OpenUrl(String),
    /// Ask the user for a line of text; the answer comes back as a "prompt" event.
    Prompt {
        id: u32,
        options: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug)]
pub struct ScriptError(pub String);

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ScriptError {}

pub struct Limits {
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    /// Maximum time a single entry into JS may run before it is interrupted.
    pub slice_ms: u64,
    /// Budget for evaluating the bundle at start-up.
    pub init_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: 48 << 20,
            stack_bytes: 4 << 20, // the host thread has an 8 MB stack
            slice_ms: 250,
            init_ms: 3000,
        }
    }
}

/// Data made available to the widget at start-up.
#[derive(Clone, Debug, Default)]
pub struct InitInfo {
    pub instance_id: String,
    pub widget_id: String,
    pub package_id: String,
    /// JSON object with the instance settings.
    pub settings_json: String,
    /// JSON object with the instance storage.
    pub storage_json: String,
    pub width: f32,
    pub height: f32,
    pub engine_version: String,
    /// Vitrine's UI language (`"en"`, `"it"`, `"pt-BR"`, `"zh"`, …).
    pub language: String,
}

#[derive(Default)]
struct HostState {
    requests: Vec<HostRequest>,
    /// due time (ms, monotonic) + timer id → repeat interval
    timers: BTreeMap<(u64, u32), Option<u64>>,
    settings_json: String,
    storage_json: String,
    size: (f32, f32),
}

pub struct WidgetScript {
    rt: Runtime,
    ctx: Context,
    state: Rc<RefCell<HostState>>,
    deadline: Arc<AtomicU64>,
    origin: Instant,
    limits: Limits,
    info: InitInfo,
}

fn now_ms(origin: Instant) -> u64 {
    origin.elapsed().as_millis() as u64
}

/// Converts a JSON value to a render property value.
pub fn json_to_prop(v: &serde_json::Value) -> PropValue {
    match v {
        serde_json::Value::Null => PropValue::Null,
        serde_json::Value::Bool(b) => PropValue::Bool(*b),
        serde_json::Value::Number(n) => PropValue::Number(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => PropValue::String(s.clone()),
        serde_json::Value::Array(a) => PropValue::List(a.iter().map(json_to_prop).collect()),
        serde_json::Value::Object(o) => PropValue::Map(
            o.iter()
                .map(|(k, v)| (k.clone(), json_to_prop(v)))
                .collect(),
        ),
    }
}

/// Parses a batch of ops encoded as JSON arrays:
/// `[0,id,kind]` create, `[1,id,name,value]` set prop, `[2,id,text]` set text,
/// `[3,parent,child,before|-1]` insert, `[4,parent,child]` remove, `[5,id]` destroy.
pub fn parse_ops(json: &str) -> Result<Vec<Op>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let arr = v.as_array().ok_or("ops must be an array")?;
    let mut out = Vec::with_capacity(arr.len());
    for op in arr {
        let a = op.as_array().ok_or("op must be an array")?;
        let n = |i: usize| {
            a.get(i)
                .and_then(|v| v.as_i64())
                .ok_or_else(|| format!("bad op {op}"))
        };
        let s = |i: usize| {
            a.get(i)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| format!("bad op {op}"))
        };
        out.push(match n(0)? {
            0 => Op::Create {
                id: n(1)? as u32,
                kind: s(2)?,
            },
            1 => Op::SetProp {
                id: n(1)? as u32,
                name: s(2)?,
                value: a.get(3).map(json_to_prop).unwrap_or_default(),
            },
            2 => Op::SetText {
                id: n(1)? as u32,
                text: s(2).unwrap_or_default(),
            },
            3 => Op::Insert {
                parent: n(1)? as u32,
                child: n(2)? as u32,
                before: a
                    .get(3)
                    .and_then(|v| v.as_i64())
                    .filter(|b| *b >= 0)
                    .map(|b| b as u32),
            },
            4 => Op::Remove {
                parent: n(1)? as u32,
                child: n(2)? as u32,
            },
            5 => Op::Destroy { id: n(1)? as u32 },
            other => return Err(format!("unknown op code {other}")),
        });
    }
    Ok(out)
}

impl WidgetScript {
    pub fn new(limits: Limits, info: InitInfo) -> Result<Self, ScriptError> {
        let err = |e: rquickjs::Error| ScriptError(e.to_string());
        let rt = Runtime::new().map_err(err)?;
        rt.set_memory_limit(limits.memory_bytes);
        rt.set_max_stack_size(limits.stack_bytes);
        rt.set_gc_threshold(4 << 20);
        let origin = Instant::now();
        let deadline = Arc::new(AtomicU64::new(u64::MAX));
        {
            let deadline = deadline.clone();
            rt.set_interrupt_handler(Some(Box::new(move || {
                now_ms(origin) > deadline.load(Ordering::Relaxed)
            })));
        }
        let ctx = Context::full(&rt).map_err(err)?;
        let state = Rc::new(RefCell::new(HostState {
            settings_json: if info.settings_json.is_empty() {
                "{}".into()
            } else {
                info.settings_json.clone()
            },
            storage_json: if info.storage_json.is_empty() {
                "{}".into()
            } else {
                info.storage_json.clone()
            },
            size: (info.width, info.height),
            ..Default::default()
        }));
        let script = Self {
            rt,
            ctx,
            state,
            deadline,
            origin,
            limits,
            info,
        };
        script.install().map_err(err)?;
        Ok(script)
    }

    fn install(&self) -> rquickjs::Result<()> {
        let origin = self.origin;
        let info = self.info.clone();
        self.ctx.with(|ctx| {
            let host = Object::new(ctx.clone())?;
            let st = self.state.clone();
            host.set(
                "ops",
                Function::new(ctx.clone(), move |json: String| -> rquickjs::Result<()> {
                    match parse_ops(&json) {
                        Ok(ops) => st.borrow_mut().requests.push(HostRequest::Ops(ops)),
                        Err(e) => st.borrow_mut().requests.push(HostRequest::Log {
                            level: LogLevel::Error,
                            message: format!("bad ops: {e}"),
                        }),
                    }
                    Ok(())
                })?,
            )?;
            let st = self.state.clone();
            host.set(
                "setTimer",
                Function::new(
                    ctx.clone(),
                    move |id: u32, delay: f64, repeat: Opt<bool>| {
                        let delay = delay.max(0.0) as u64;
                        let due = now_ms(origin) + delay;
                        let mut s = st.borrow_mut();
                        s.timers.retain(|(_, tid), _| *tid != id);
                        s.timers
                            .insert((due, id), repeat.0.unwrap_or(false).then_some(delay.max(1)));
                    },
                )?,
            )?;
            let st = self.state.clone();
            host.set(
                "clearTimer",
                Function::new(ctx.clone(), move |id: u32| {
                    st.borrow_mut().timers.retain(|(_, tid), _| *tid != id);
                })?,
            )?;
            let st = self.state.clone();
            host.set(
                "subscribe",
                Function::new(ctx.clone(), move |json: String, interval: Opt<f64>| {
                    let metrics: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
                    st.borrow_mut().requests.push(HostRequest::Subscribe {
                        metrics,
                        interval_ms: interval.0.unwrap_or(2000.0).max(1000.0) as u32,
                    });
                })?,
            )?;
            let st = self.state.clone();
            host.set(
                "fetch",
                Function::new(
                    ctx.clone(),
                    move |id: u32,
                          url: String,
                          method: Opt<Option<String>>,
                          headers: Opt<Option<String>>,
                          body: Opt<Option<String>>| {
                        let headers: BTreeMap<String, String> = headers
                            .0
                            .flatten()
                            .and_then(|h| serde_json::from_str(&h).ok())
                            .unwrap_or_default();
                        st.borrow_mut().requests.push(HostRequest::Fetch {
                            id,
                            url,
                            method: method
                                .0
                                .flatten()
                                .unwrap_or_else(|| "GET".into())
                                .to_uppercase(),
                            headers: headers.into_iter().collect(),
                            body: body.0.flatten(),
                        });
                    },
                )?,
            )?;
            let st = self.state.clone();
            host.set(
                "log",
                Function::new(ctx.clone(), move |level: String, message: String| {
                    let level = match level.as_str() {
                        "debug" => LogLevel::Debug,
                        "warn" => LogLevel::Warn,
                        "error" => LogLevel::Error,
                        _ => LogLevel::Info,
                    };
                    st.borrow_mut()
                        .requests
                        .push(HostRequest::Log { level, message });
                })?,
            )?;
            let st = self.state.clone();
            host.set(
                "settings",
                Function::new(ctx.clone(), move || st.borrow().settings_json.clone())?,
            )?;
            let st = self.state.clone();
            host.set(
                "storage",
                Function::new(ctx.clone(), move || st.borrow().storage_json.clone())?,
            )?;
            let st = self.state.clone();
            host.set(
                "storageSet",
                Function::new(
                    ctx.clone(),
                    move |key: String, value: Opt<Option<String>>| {
                        let value = Opt(value.0.flatten());
                        let mut s = st.borrow_mut();
                        let mut map: serde_json::Map<String, serde_json::Value> =
                            serde_json::from_str(&s.storage_json).unwrap_or_default();
                        match &value.0 {
                            Some(v) => {
                                map.insert(key.clone(), serde_json::Value::String(v.clone()))
                            }
                            None => map.remove(&key),
                        };
                        s.storage_json = serde_json::Value::Object(map).to_string();
                        s.requests.push(HostRequest::StorageSet {
                            key,
                            value: value.0,
                        });
                    },
                )?,
            )?;
            let st = self.state.clone();
            host.set(
                "size",
                Function::new(ctx.clone(), move || {
                    let (w, h) = st.borrow().size;
                    format!("{{\"width\":{w},\"height\":{h}}}")
                })?,
            )?;
            let st = self.state.clone();
            host.set(
                "openUrl",
                Function::new(ctx.clone(), move |url: String| {
                    if url.starts_with("https://") || url.starts_with("http://") {
                        st.borrow_mut().requests.push(HostRequest::OpenUrl(url));
                    }
                })?,
            )?;
            let st = self.state.clone();
            host.set(
                "prompt",
                Function::new(ctx.clone(), move |id: u32, options: Opt<String>| {
                    st.borrow_mut().requests.push(HostRequest::Prompt {
                        id,
                        options: options.0.unwrap_or_else(|| "{}".into()),
                    });
                })?,
            )?;
            host.set(
                "appearance",
                Function::new(ctx.clone(), appearance::appearance_json)?,
            )?;
            host.set(
                "localeInfo",
                Function::new(ctx.clone(), move |name: Opt<String>| {
                    locale::locale_info_json(name.0.as_deref())
                })?,
            )?;
            host.set(
                "now",
                Function::new(ctx.clone(), move || {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as f64)
                        .unwrap_or(0.0)
                })?,
            )?;
            let meta = serde_json::json!({
                "instanceId": info.instance_id,
                "widgetId": info.widget_id,
                "packageId": info.package_id,
                "engineVersion": info.engine_version,
                "language": if info.language.is_empty() { "en" } else { info.language.as_str() },
            })
            .to_string();
            host.set("info", meta)?;
            ctx.globals().set("__vitrine", host)?;
            Ok(())
        })
    }

    fn with_budget<R>(&self, ms: u64, f: impl FnOnce() -> R) -> R {
        self.deadline
            .store(now_ms(self.origin) + ms, Ordering::Relaxed);
        let r = f();
        self.deadline.store(u64::MAX, Ordering::Relaxed);
        r
    }

    fn describe(ctx: &rquickjs::Ctx<'_>, e: rquickjs::Error) -> ScriptError {
        let caught = CaughtError::from_error(ctx, e);
        ScriptError(match caught {
            CaughtError::Exception(ex) => {
                let msg = ex.message().unwrap_or_default();
                match ex.stack() {
                    Some(stack) if !stack.is_empty() => format!("{msg}\n{stack}"),
                    _ => msg,
                }
            }
            other => other.to_string(),
        })
    }

    /// Evaluates the widget bundle (a classic script).
    pub fn run_bundle(&mut self, code: &str, name: &str) -> Result<(), ScriptError> {
        let budget = self.limits.init_ms;
        let r = self.with_budget(budget, || {
            self.ctx.with(|ctx| {
                let mut opts = rquickjs::context::EvalOptions::default();
                opts.global = true;
                opts.strict = false;
                let _ = name;
                ctx.eval_with_options::<(), _>(code, opts)
                    .map_err(|e| Self::describe(&ctx, e))
            })
        });
        self.run_jobs();
        r
    }

    /// Runs pending promise jobs (microtasks).
    pub fn run_jobs(&mut self) {
        let slice = self.limits.slice_ms;
        self.with_budget(slice, || {
            for _ in 0..10_000 {
                match self.rt.execute_pending_job() {
                    Ok(true) => continue,
                    Ok(false) => break,
                    Err(e) => {
                        let msg = format!("unhandled error in job: {e:?}");
                        self.state.borrow_mut().requests.push(HostRequest::Log {
                            level: LogLevel::Error,
                            message: msg,
                        });
                    }
                }
            }
        });
    }

    /// Calls `__vitrine_event(type, json)`.
    pub fn dispatch(&mut self, ty: &str, json: &str) -> Result<(), ScriptError> {
        let slice = self.limits.slice_ms;
        let r = self.with_budget(slice, || {
            self.ctx.with(|ctx| {
                let f: Option<Function> = ctx.globals().get("__vitrine_event").ok();
                match f {
                    Some(f) => f
                        .call::<_, ()>((ty, json))
                        .map_err(|e| Self::describe(&ctx, e)),
                    None => Ok(()),
                }
            })
        });
        self.run_jobs();
        r
    }

    pub fn set_settings(&mut self, json: &str) -> Result<(), ScriptError> {
        self.state.borrow_mut().settings_json = json.to_string();
        self.dispatch("settings", json)
    }

    pub fn set_size(&mut self, width: f32, height: f32) -> Result<(), ScriptError> {
        self.state.borrow_mut().size = (width, height);
        self.dispatch(
            "resize",
            &format!("{{\"width\":{width},\"height\":{height}}}"),
        )
    }

    /// Milliseconds (on this script's monotonic clock) until the next timer, if any.
    pub fn next_timer_in(&self) -> Option<u64> {
        let due = self.state.borrow().timers.keys().next().map(|(d, _)| *d)?;
        Some(due.saturating_sub(now_ms(self.origin)))
    }

    /// Fires all due timers.
    pub fn fire_timers(&mut self) -> Result<(), ScriptError> {
        let now = now_ms(self.origin);
        let due: Vec<(u64, u32)> = self
            .state
            .borrow()
            .timers
            .range(..=(now, u32::MAX))
            .map(|(k, _)| *k)
            .collect();
        let mut first_err = None;
        for key in due {
            let repeat = self.state.borrow_mut().timers.remove(&key).flatten();
            if let Some(interval) = repeat {
                self.state
                    .borrow_mut()
                    .timers
                    .insert((now + interval, key.1), Some(interval));
            }
            if let Err(e) = self.dispatch("timer", &key.1.to_string()) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Takes the requests accumulated since the last call.
    pub fn drain(&mut self) -> Vec<HostRequest> {
        std::mem::take(&mut self.state.borrow_mut().requests)
    }

    pub fn memory_usage(&self) -> i64 {
        self.rt.memory_usage().memory_used_size
    }

    pub fn collect_garbage(&self) {
        self.rt.run_gc();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> WidgetScript {
        WidgetScript::new(
            Limits::default(),
            InitInfo {
                settings_json: r#"{"a":1}"#.into(),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn info_carries_the_ui_language() {
        let mut s = WidgetScript::new(
            Limits::default(),
            InitInfo {
                language: "pt-BR".into(),
                ..Default::default()
            },
        )
        .unwrap();
        s.run_bundle(
            "__vitrine.log('info', JSON.parse(__vitrine.info).language);",
            "t.js",
        )
        .unwrap();
        let reqs = s.drain();
        assert!(
            reqs.iter()
                .any(|r| matches!(r, HostRequest::Log { message, .. } if message == "pt-BR"))
        );
        // Without a language the widget sees English.
        let mut s = script();
        s.run_bundle(
            "__vitrine.log('info', JSON.parse(__vitrine.info).language);",
            "t.js",
        )
        .unwrap();
        assert!(
            s.drain()
                .iter()
                .any(|r| matches!(r, HostRequest::Log { message, .. } if message == "en"))
        );
    }

    #[test]
    fn ops_and_events() {
        let mut s = script();
        s.run_bundle(
            r#"
            var count = 0;
            globalThis.__vitrine_event = function (type, json) {
              if (type === 'timer') { count++; __vitrine.ops(JSON.stringify([[2, 5, 'tick ' + count]])); }
            };
            __vitrine.ops(JSON.stringify([[0, 5, '#text'], [3, 0, 5, -1], [1, 5, 'style', {color: 'red'}]]));
            __vitrine.setTimer(1, 0, true);
            Promise.resolve().then(() => __vitrine.log('info', 'microtask ran ' + JSON.parse(__vitrine.settings()).a));
            "#,
            "test.js",
        )
        .unwrap();
        let reqs = s.drain();
        assert!(matches!(&reqs[0], HostRequest::Ops(ops) if ops.len() == 3));
        assert!(
            matches!(&reqs[1], HostRequest::Log { message, .. } if message == "microtask ran 1")
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.fire_timers().unwrap();
        let reqs = s.drain();
        assert_eq!(
            reqs,
            vec![HostRequest::Ops(vec![Op::SetText {
                id: 5,
                text: "tick 1".into()
            }])]
        );
        assert!(s.next_timer_in().is_some());
    }

    #[test]
    fn runaway_script_is_interrupted() {
        let mut s = WidgetScript::new(
            Limits {
                init_ms: 100,
                ..Default::default()
            },
            InitInfo::default(),
        )
        .unwrap();
        let t = Instant::now();
        let r = s.run_bundle("while (true) {}", "loop.js");
        assert!(r.is_err());
        assert!(t.elapsed().as_millis() < 2000);
        // the runtime stays usable afterwards
        s.run_bundle("__vitrine.log('info', 'alive')", "ok.js")
            .unwrap();
        assert_eq!(s.drain().len(), 1);
    }

    #[test]
    fn errors_are_reported() {
        let mut s = script();
        let e = s
            .run_bundle("function f() { throw new Error('boom') } f()", "err.js")
            .unwrap_err();
        assert!(e.0.contains("boom"), "{}", e.0);
    }
}
