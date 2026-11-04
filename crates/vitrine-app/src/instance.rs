//! A running widget: its script, its renderer and the permission checks
//! between them and the rest of the system.

use std::time::Instant;

use vitrine_render::gpu::Gpu;
use vitrine_render::image::DirAssets;
use vitrine_render::widget::WidgetRenderer;
use vitrine_script::{HostRequest, InitInfo, Limits, LogLevel, WidgetScript};
use vitrine_widgets::manifest::{Permission, WidgetManifest};
use vitrine_widgets::package::InstalledPackage;

/// Side effects a widget asks the application to perform.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
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
    StorageSet {
        key: String,
        value: Option<String>,
    },
    OpenUrl(String),
    /// Show a text prompt; reply with a "prompt" event `{id, value}`.
    Prompt {
        id: u32,
        options: String,
    },
}

pub struct WidgetInstance {
    pub package_id: String,
    pub widget: WidgetManifest,
    pub script: WidgetScript,
    pub renderer: WidgetRenderer,
    /// Metrics currently requested (already filtered by permissions).
    pub subscription: Option<(Vec<String>, u32)>,
    /// Set when the script failed to load; the widget then shows nothing.
    pub failed: Option<String>,
    origin: Instant,
}

fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit('@').next()?;
    Some(host.split(':').next()?.to_ascii_lowercase())
}

/// Whether a `net:<pattern>` permission allows `host` (`*.example.com` matches subdomains).
pub fn net_allowed(perms: &[Permission], url: &str) -> bool {
    let Some(host) = host_of(url) else {
        return false;
    };
    perms.iter().any(|p| match p {
        Permission::Net(pattern) => {
            let pattern = pattern.to_ascii_lowercase();
            match pattern.strip_prefix("*.") {
                Some(suffix) => host == suffix || host.ends_with(&format!(".{suffix}")),
                None => host == pattern,
            }
        }
        _ => false,
    })
}

impl WidgetInstance {
    pub fn new(
        gpu: &Gpu,
        package: &InstalledPackage,
        widget_id: &str,
        instance_id: &str,
        settings_json: &str,
        storage_json: &str,
        size: (f32, f32),
    ) -> Result<Self, String> {
        let widget = package
            .widget(widget_id)
            .ok_or_else(|| {
                format!(
                    "package {} has no widget `{widget_id}`",
                    package.manifest.id
                )
            })?
            .clone();
        let fonts: Vec<Vec<u8>> = package
            .manifest
            .fonts
            .iter()
            .filter_map(|f| std::fs::read(package.resolve(f)).ok())
            .collect();
        let mut renderer =
            WidgetRenderer::new(gpu, Box::new(DirAssets(package.dir.clone())), &fonts);
        renderer.set_size(size.0, size.1);
        let info = InitInfo {
            instance_id: instance_id.into(),
            widget_id: widget_id.into(),
            package_id: package.manifest.id.clone(),
            settings_json: settings_json.into(),
            storage_json: storage_json.into(),
            width: size.0,
            height: size.1,
            engine_version: env!("CARGO_PKG_VERSION").into(),
        };
        let script = WidgetScript::new(Limits::default(), info).map_err(|e| e.to_string())?;
        let mut inst = Self {
            package_id: package.manifest.id.clone(),
            widget,
            script,
            renderer,
            subscription: None,
            failed: None,
            origin: Instant::now(),
        };
        let entry = package.resolve(&inst.widget.entry);
        match std::fs::read_to_string(&entry) {
            Ok(code) => {
                if let Err(e) = inst.script.run_bundle(&code, &inst.widget.entry) {
                    inst.fail(format!("script error: {e}"));
                }
            }
            Err(e) => inst.fail(format!("cannot read {}: {e}", entry.display())),
        }
        Ok(inst)
    }

    fn fail(&mut self, msg: String) {
        log::error!("[{}/{}] {msg}", self.package_id, self.widget.id);
        self.failed = Some(msg);
    }

    /// Milliseconds since the instance started (the renderer's animation clock).
    pub fn now(&self) -> f64 {
        self.origin.elapsed().as_secs_f64() * 1000.0
    }

    fn allows_metric(&self, m: &str) -> bool {
        self.widget
            .permissions
            .iter()
            .any(|p| matches!(p, Permission::System(s) if s == m))
    }

    /// Applies script output to the renderer and returns effects for the app.
    pub fn pump(&mut self) -> Vec<Effect> {
        let now = self.now();
        let mut effects = Vec::new();
        for req in self.script.drain() {
            match req {
                HostRequest::Ops(ops) => self.renderer.apply(ops, now),
                HostRequest::Subscribe {
                    metrics,
                    interval_ms,
                } => {
                    let (allowed, denied): (Vec<String>, Vec<String>) =
                        metrics.into_iter().partition(|m| self.allows_metric(m));
                    if !denied.is_empty() {
                        log::warn!(
                            "[{}/{}] metrics not declared in permissions: {}",
                            self.package_id,
                            self.widget.id,
                            denied.join(", ")
                        );
                    }
                    self.subscription =
                        (!allowed.is_empty()).then(|| (allowed.clone(), interval_ms));
                    effects.push(Effect::Subscribe {
                        metrics: allowed,
                        interval_ms,
                    });
                }
                HostRequest::Fetch {
                    id,
                    url,
                    method,
                    headers,
                    body,
                } => {
                    if net_allowed(&self.widget.permissions, &url) {
                        effects.push(Effect::Fetch {
                            id,
                            url,
                            method,
                            headers,
                            body,
                        });
                    } else {
                        let msg = format!("host not allowed by permissions: {url}");
                        log::warn!("[{}/{}] {msg}", self.package_id, self.widget.id);
                        self.deliver(
                            "fetch",
                            &serde_json::json!({ "id": id, "error": msg }).to_string(),
                        );
                    }
                }
                HostRequest::Log { level, message } => {
                    let target = format!("{}/{}", self.package_id, self.widget.id);
                    match level {
                        LogLevel::Debug => log::debug!("[{target}] {message}"),
                        LogLevel::Info => log::info!("[{target}] {message}"),
                        LogLevel::Warn => log::warn!("[{target}] {message}"),
                        LogLevel::Error => log::error!("[{target}] {message}"),
                    }
                }
                HostRequest::StorageSet { key, value } => {
                    effects.push(Effect::StorageSet { key, value })
                }
                HostRequest::OpenUrl(url) => effects.push(Effect::OpenUrl(url)),
                HostRequest::Prompt { id, options } => effects.push(Effect::Prompt { id, options }),
            }
        }
        effects
    }

    /// Dispatches an event to the script (errors are logged, not fatal).
    pub fn deliver(&mut self, ty: &str, json: &str) {
        if self.failed.is_some() {
            return;
        }
        if let Err(e) = self.script.dispatch(ty, json) {
            log::error!("[{}/{}] {ty} handler: {e}", self.package_id, self.widget.id);
        }
    }

    pub fn fire_timers(&mut self) {
        if self.failed.is_some() {
            return;
        }
        if let Err(e) = self.script.fire_timers() {
            log::error!("[{}/{}] timer: {e}", self.package_id, self.widget.id);
        }
    }

    pub fn set_size(&mut self, w: f32, h: f32) {
        self.renderer.set_size(w, h);
        if self.failed.is_none() {
            let _ = self.script.set_size(w, h);
        }
    }

    pub fn set_settings(&mut self, json: &str) {
        if self.failed.is_none() {
            let _ = self.script.set_settings(json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_permissions() {
        let p = vec![
            Permission::Net("api.open-meteo.com".into()),
            Permission::Net("*.example.com".into()),
        ];
        assert!(net_allowed(
            &p,
            "https://api.open-meteo.com/v1/forecast?x=1"
        ));
        assert!(net_allowed(&p, "https://a.b.example.com/x"));
        assert!(net_allowed(&p, "https://example.com"));
        assert!(!net_allowed(&p, "https://evil.com/?api.open-meteo.com"));
        assert!(!net_allowed(&p, "https://api.open-meteo.com.evil.com/"));
        assert!(!net_allowed(&p, "ftp://api.open-meteo.com/"));
        assert!(net_allowed(&p, "https://user@api.open-meteo.com:443/"));
    }
}
