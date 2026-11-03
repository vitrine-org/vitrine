//! HTTP requests made on behalf of widgets (after permission checks).

use std::io::Read;
use std::time::Duration;

const MAX_BODY: u64 = 5 * 1024 * 1024;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .user_agent(concat!("Vitrine/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Performs a request and returns the JSON payload for the widget's `fetch` event.
pub fn perform(id: u32, method: &str, url: &str, headers: &[(String, String)], body: Option<&str>) -> serde_json::Value {
    let result = (|| -> Result<serde_json::Value, String> {
        let agent = agent();
        let mut builder = ureq::http::Request::builder().method(method).uri(url);
        for (k, v) in headers {
            builder = builder.header(k, v);
        }
        let response = match body {
            Some(b) => agent.run(builder.body(b.to_string()).map_err(|e| e.to_string())?),
            None => agent.run(builder.body(()).map_err(|e| e.to_string())?),
        }
        .map_err(|e| e.to_string())?;
        let status = response.status();
        let mut hdrs = serde_json::Map::new();
        for (k, v) in response.headers() {
            if let Ok(v) = v.to_str() {
                hdrs.insert(k.as_str().to_string(), v.into());
            }
        }
        let mut text = String::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_BODY)
            .read_to_string(&mut text)
            .map_err(|e| format!("reading body: {e}"))?;
        Ok(serde_json::json!({
            "id": id,
            "ok": status.is_success(),
            "status": status.as_u16(),
            "statusText": status.canonical_reason().unwrap_or(""),
            "headers": hdrs,
            "body": text,
        }))
    })();
    result.unwrap_or_else(|e| serde_json::json!({ "id": id, "error": e }))
}
