use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// Minimal, fire-and-forget PostHog analytics client. Product analytics only —
/// error/crash monitoring stays with Sentry. Never blocks, never panics; all
/// send errors are swallowed (logged at debug). An empty api_key = no-op.
pub struct Analytics {
    client: reqwest::Client,
    host: String,
    api_key: String,
    environment: &'static str,
    version: String,
    device_id: String,
    distinct_id: Arc<Mutex<Option<String>>>,
}

/// Build the PostHog `/capture/` JSON body, merging the standard properties
/// (environment, app_version, $lib) into the caller's properties object.
fn build_capture_payload(
    api_key: &str,
    event: &str,
    distinct_id: &str,
    mut props: Value,
    environment: &str,
    version: &str,
    timestamp: &str,
) -> Value {
    if let Value::Object(map) = &mut props {
        map.insert("environment".into(), json!(environment));
        map.insert("app_version".into(), json!(version));
        map.insert("$lib".into(), json!("linewise-desktop"));
    }
    json!({
        "api_key": api_key,
        "event": event,
        "distinct_id": distinct_id,
        "timestamp": timestamp,
        "properties": props,
    })
}

impl Analytics {
    pub fn new(
        api_key: String,
        host: String,
        proxy_url: Option<&str>,
        environment: &'static str,
        version: String,
        device_id: String,
    ) -> Self {
        let mut builder = reqwest::Client::builder().use_rustls_tls();
        if let Some(p) = proxy_url
            && let Ok(proxy) = reqwest::Proxy::all(p)
        {
            builder = builder.proxy(proxy);
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            host,
            api_key,
            environment,
            version,
            device_id,
            distinct_id: Arc::new(Mutex::new(None)),
        }
    }

    /// The distinct_id to attribute events to: the identified user id once
    /// `identify` has run, otherwise the persistent per-install device id.
    pub fn current_distinct_id(&self) -> String {
        self.distinct_id
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_else(|| self.device_id.clone())
    }

    /// Capture a product-analytics event (fire-and-forget).
    pub fn capture(&self, event: &str, props: Value) {
        if self.api_key.is_empty() {
            return;
        }
        let ts = chrono::Utc::now().to_rfc3339();
        let payload = build_capture_payload(
            &self.api_key,
            event,
            &self.current_distinct_id(),
            props,
            self.environment,
            &self.version,
            &ts,
        );
        self.send(payload);
    }

    /// Identify the current user, aliasing prior anonymous (device) events.
    pub fn identify(&self, uid: &str, props: Value) {
        if let Ok(mut g) = self.distinct_id.lock() {
            *g = Some(uid.to_string());
        }
        if self.api_key.is_empty() {
            return;
        }
        let ts = chrono::Utc::now().to_rfc3339();
        let payload = build_capture_payload(
            &self.api_key,
            "$identify",
            uid,
            json!({ "$set": props, "$anon_distinct_id": self.device_id }),
            self.environment,
            &self.version,
            &ts,
        );
        self.send(payload);
    }

    /// Stop attributing events to a user (on sign-out): revert to device id.
    pub fn reset(&self) {
        if let Ok(mut g) = self.distinct_id.lock() {
            *g = None;
        }
    }

    fn send(&self, payload: Value) {
        let client = self.client.clone();
        let url = format!("{}/capture/", self.host.trim_end_matches('/'));
        tokio::spawn(async move {
            if let Err(e) = client.post(&url).json(&payload).send().await {
                tracing::debug!("posthog capture failed: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_merges_lib_env_version_and_keeps_caller_props() {
        let p = build_capture_payload(
            "phc_x",
            "upload_completed",
            "user-1",
            serde_json::json!({ "size": 100u64 }),
            "production",
            "0.1.6",
            "2026-07-07T00:00:00Z",
        );
        assert_eq!(p["api_key"], "phc_x");
        assert_eq!(p["event"], "upload_completed");
        assert_eq!(p["distinct_id"], "user-1");
        assert_eq!(p["timestamp"], "2026-07-07T00:00:00Z");
        assert_eq!(p["properties"]["size"], 100);
        assert_eq!(p["properties"]["environment"], "production");
        assert_eq!(p["properties"]["app_version"], "0.1.6");
        assert_eq!(p["properties"]["$lib"], "linewise-desktop");
    }

    #[test]
    fn distinct_id_is_device_until_identified_then_uid() {
        let a = Analytics::new(
            String::new(),
            "https://h".into(),
            None,
            "dev",
            "v".into(),
            "device-9".into(),
        );
        assert_eq!(a.current_distinct_id(), "device-9");
        a.identify("user-1", serde_json::json!({}));
        assert_eq!(a.current_distinct_id(), "user-1");
        a.reset();
        assert_eq!(a.current_distinct_id(), "device-9");
    }
}
