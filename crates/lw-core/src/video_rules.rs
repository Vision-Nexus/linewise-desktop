use crate::config::AppConfig;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Default rule set, embedded at compile time. Used as the seed when no
/// disk cache exists yet, and as a recovery seed when the cached file is
/// corrupt. Never read directly by the validator — it always lands on
/// disk first, so the file is the single source of truth at runtime.
const EMBEDDED_RULES_JSON: &str = include_str!("../resources/video_quality_rules.json");

/// Public CDN endpoint that serves the latest rules. The fetch sends an
/// `If-None-Match` header with the cached ETag, so the steady-state cost
/// is one 304 round-trip per launch.
const RULES_URL: &str = "https://vl.linewise.io/video_quality_rules.json";

/// Hard timeout for the network refresh. Five seconds is short enough not
/// to noticeably stretch launch on a flaky network, long enough to clear
/// a normal TLS handshake and ETag check on a healthy one.
const FETCH_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone, Deserialize)]
pub struct VideoRules {
    pub schema_version: u32,
    pub numeric: NumericRules,
    pub provenance: ProvenanceRules,
    pub telemetry: TelemetryRules,
    pub camera_settings_guide_url: String,
    pub camera_settings_guide_footer: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NumericRules {
    pub fps: FpsRules,
    pub bitrate_kbps: BitrateRules,
    pub duration_seconds: DurationRules,
    pub resolution: ResolutionRules,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DurationRules {
    pub target: f64,
    pub recommend: Band<f64>,
    pub accept: Band<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FpsRules {
    pub target: f64,
    pub recommend: Band<f64>,
    pub accept: Band<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BitrateRules {
    pub target: u64,
    pub recommend: Band<u64>,
    pub accept: Band<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolutionRules {
    /// Soft-warning band over pixel count (`width * height`). A
    /// zero-width band (`min == max`) captures "nudge the user toward
    /// this exact target", e.g. exactly 1080p.
    pub recommend: Band<u64>,
    /// Hard-rejection band over pixel count.
    pub accept: Band<u64>,
}

/// One band of a numeric dimension (fps, bitrate, etc.). Either edge can
/// be omitted for an open-sided range; the shared `message` is rendered
/// with `{bound}` substituted to the side that tripped (`"below"` or
/// `"above"`).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Band<T> {
    #[serde(default = "default_none")]
    pub min: Option<T>,
    #[serde(default = "default_none")]
    pub max: Option<T>,
    pub message: String,
}

fn default_none<T>() -> Option<T> {
    None
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProvenanceRules {
    pub camera_fingerprint_keys: Vec<String>,
    pub encoder_keys: Vec<String>,
    pub reencode_signatures: Vec<String>,
    /// Wrapped in `Arc` so UI components that show device-info popovers
    /// can clone a refcount instead of the underlying vector. Built once
    /// during JSON parse via [`vec_into_arc`]; the JSON shape on disk is
    /// still a plain array.
    #[serde(deserialize_with = "vec_into_arc")]
    pub device_encoder_signatures: Arc<Vec<DeviceEncoderSignature>>,
    pub messages: ProvenanceMessages,
}

fn vec_into_arc<'de, D, T>(deserializer: D) -> Result<Arc<Vec<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let vec: Vec<T> = Vec::deserialize(deserializer)?;
    Ok(Arc::new(vec))
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DeviceEncoderSignature {
    pub needle: String,
    pub vendor_label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProvenanceMessages {
    pub reencoded: String,
    pub stripped: String,
    pub reencoded_warning: String,
    pub stripped_warning: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryRules {
    pub tags: Vec<TelemetryTag>,
    pub action_camera_keywords: Vec<String>,
    pub missing_telemetry_warning: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryTag {
    /// Four-character codec tag stored on the data stream's
    /// `codec_tag`. We expect a length-4 ASCII string in the JSON; it
    /// gets compared byte-by-byte against the little-endian four-CC
    /// from FFmpeg.
    pub fourcc: String,
    pub label: String,
}

impl TelemetryTag {
    /// Return the four bytes of `fourcc` if it really is four ASCII
    /// characters; otherwise None. Bad rule entries log a warning at
    /// detect time rather than crashing the validator.
    pub fn fourcc_bytes(&self) -> Option<[u8; 4]> {
        let bytes = self.fourcc.as_bytes();
        if bytes.len() == 4 {
            Some([bytes[0], bytes[1], bytes[2], bytes[3]])
        } else {
            None
        }
    }
}

impl VideoRules {
    /// Boot-time loader. Two phases:
    ///
    /// **Phase 1 — refresh.** Seed the cache from `EMBEDDED_RULES_JSON`
    /// if the file is absent, then attempt a conditional GET against
    /// [`RULES_URL`]. On 200 OK, overwrite the body and ETag; on 304 or
    /// any error, leave the cache as-is.
    ///
    /// **Phase 2 — read.** Parse `rules.json` from disk and return it.
    /// If the disk file fails to parse (corrupt manual edit, partial
    /// write), re-seed from the embedded copy and parse that.
    ///
    /// The function never returns an error. Failures during refresh are
    /// logged at `warn!`; the validator's view is whatever lands on
    /// disk by the end of phase 1.
    pub async fn load_for_startup() -> Arc<VideoRules> {
        Self::load_with(RULES_URL, CachePaths::resolve()).await
    }

    /// Inner loader the public entry point delegates to. Parameterised
    /// on URL and cache paths so tests can drive it against a temp dir
    /// and a mock server. The behaviour is identical to
    /// [`Self::load_for_startup`].
    async fn load_with(url: &str, cache: CachePaths) -> Arc<VideoRules> {
        if let Err(e) = cache.ensure_dir() {
            tracing::warn!("Could not create video rules cache dir: {e}");
        }

        // Phase 1a: seed if absent.
        if !cache.body.exists()
            && let Err(e) = std::fs::write(&cache.body, EMBEDDED_RULES_JSON)
        {
            tracing::warn!("Could not seed video rules cache: {e}");
        }

        // Phase 1b: best-effort refresh from the network.
        match refresh(url, &cache).await {
            Ok(Refresh::Updated) => tracing::info!("Video rules refreshed from {url}"),
            Ok(Refresh::NotModified) => tracing::debug!("Video rules cache hit (304)"),
            Err(e) => tracing::warn!("Video rules refresh failed: {e}"),
        }

        // Phase 2: read the disk file. If it parses, we're done.
        match read_and_parse(&cache.body) {
            Ok(rules) => Arc::new(rules),
            Err(e) => {
                tracing::warn!(
                    "Cached video rules at {} unparseable ({e}); re-seeding from embedded copy",
                    cache.body.display()
                );
                if let Err(write_err) = std::fs::write(&cache.body, EMBEDDED_RULES_JSON) {
                    tracing::warn!("Could not re-seed video rules cache: {write_err}");
                }
                // The embedded copy is parsed at compile time by a unit
                // test; any failure here would be a programming error,
                // not a runtime condition.
                Arc::new(
                    serde_json::from_str(EMBEDDED_RULES_JSON)
                        .expect("embedded rules JSON must parse — see embedded_rules_parse test"),
                )
            }
        }
    }

    /// Parse the rules embedded in the binary. Useful for tests that
    /// need a real `VideoRules` without going through the cache loader,
    /// and as a synchronous fallback for any caller that's willing to
    /// accept the bundled defaults. The embedded JSON is asserted to
    /// parse by [`tests::embedded_rules_parse`] — if that test passes
    /// at build time, this method cannot panic at runtime.
    pub fn embedded() -> Arc<VideoRules> {
        Arc::new(
            serde_json::from_str(EMBEDDED_RULES_JSON)
                .expect("embedded rules JSON must parse — see embedded_rules_parse test"),
        )
    }
}

/// Cache file paths under `AppConfig::data_dir()`. Body and ETag live
/// side by side so the conditional-GET pair stays consistent.
struct CachePaths {
    dir: PathBuf,
    body: PathBuf,
    etag: PathBuf,
}

impl CachePaths {
    fn resolve() -> Self {
        Self::under(AppConfig::data_dir().join("video_rules"))
    }

    fn under(dir: PathBuf) -> Self {
        let body = dir.join("rules.json");
        let etag = dir.join("etag.txt");
        Self { dir, body, etag }
    }

    fn ensure_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)
    }
}

enum Refresh {
    Updated,
    NotModified,
}

async fn refresh(url: &str, cache: &CachePaths) -> Result<Refresh, RefreshError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()?;

    let mut request = client.get(url);
    if let Ok(etag) = std::fs::read_to_string(&cache.etag) {
        let trimmed = etag.trim();
        if !trimmed.is_empty() {
            request = request.header(reqwest::header::IF_NONE_MATCH, trimmed);
        }
    }

    let response = request.send().await?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(Refresh::NotModified);
    }
    if !status.is_success() {
        return Err(RefreshError::BadStatus(status.as_u16()));
    }

    let new_etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = response.bytes().await?;

    // Validate the body parses before clobbering the cache. A server
    // serving a malformed file shouldn't be allowed to break the next
    // launch.
    serde_json::from_slice::<VideoRules>(&body)?;

    std::fs::write(&cache.body, &body)?;
    match new_etag {
        Some(tag) => std::fs::write(&cache.etag, tag)?,
        None => {
            // No ETag header — drop any stale one we had so the next
            // launch doesn't send a bogus If-None-Match.
            let _ = std::fs::remove_file(&cache.etag);
        }
    }
    Ok(Refresh::Updated)
}

#[derive(Debug, thiserror::Error)]
enum RefreshError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("response did not parse as VideoRules: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("server returned status {0}")]
    BadStatus(u16),
}

fn read_and_parse(path: &std::path::Path) -> Result<VideoRules, ReadError> {
    let bytes = std::fs::read(path)?;
    let rules = serde_json::from_slice::<VideoRules>(&bytes)?;
    Ok(rules)
}

#[derive(Debug, thiserror::Error)]
enum ReadError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(#[from] serde_json::Error),
}

/// Render a message template by substituting `{name}`, `{name:.0}`, or
/// `{name:.1}` placeholders. `vars` provides string values for the named
/// keys; values are inserted verbatim. The numeric format spec is
/// honoured only when the key resolves to a [`Sub::Float`] — that's how
/// the caller signals "this came from an `f64`, format it with N decimal
/// places". Unknown names and unmatched braces are left intact, so a
/// malformed template still produces something readable.
pub fn render(template: &str, vars: &[(&str, Sub)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch != '{' {
            out.push(ch);
            continue;
        }
        // Find the matching `}` from `i+1`.
        let rest = &template[i + 1..];
        let Some(end_rel) = rest.find('}') else {
            out.push('{');
            continue;
        };
        let inside = &rest[..end_rel];
        // Advance the iterator past the `}`.
        let consume_until = i + 1 + end_rel;
        while let Some(&(j, _)) = chars.peek() {
            if j > consume_until {
                break;
            }
            chars.next();
        }

        let (name, spec) = match inside.split_once(':') {
            Some((n, s)) => (n, Some(s)),
            None => (inside, None),
        };
        match vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v) {
            Some(Sub::Str(s)) => out.push_str(s),
            Some(Sub::Float(f)) => match spec {
                Some(".0") => out.push_str(&format!("{f:.0}")),
                Some(".1") => out.push_str(&format!("{f:.1}")),
                Some(".2") => out.push_str(&format!("{f:.2}")),
                _ => out.push_str(&format!("{f}")),
            },
            None => {
                // Unknown placeholder — emit it verbatim so authors can
                // see what they typoed in the rule file.
                out.push('{');
                out.push_str(inside);
                out.push('}');
            }
        }
    }
    out
}

/// Substitution value for [`render`]. Floats carry their value so the
/// renderer can apply `{name:.0}` / `{name:.1}` formatting; strings are
/// already pre-rendered (e.g. `format_bitrate` output).
#[derive(Debug, Clone)]
pub enum Sub {
    Str(String),
    Float(f64),
}

impl From<&str> for Sub {
    fn from(s: &str) -> Self {
        Sub::Str(s.to_owned())
    }
}
impl From<String> for Sub {
    fn from(s: String) -> Self {
        Sub::Str(s)
    }
}
impl From<f64> for Sub {
    fn from(f: f64) -> Self {
        Sub::Float(f)
    }
}
impl From<u32> for Sub {
    fn from(v: u32) -> Self {
        Sub::Str(v.to_string())
    }
}
impl From<u64> for Sub {
    fn from(v: u64) -> Self {
        Sub::Str(v.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_rules_parse() {
        // The embedded JSON must always parse — it's the runtime safety
        // net for a missing or corrupt cache file. CI catches a broken
        // bundled file here rather than at startup.
        let rules: VideoRules = serde_json::from_str(EMBEDDED_RULES_JSON).expect("parse");
        assert_eq!(rules.schema_version, 1);
        assert!(rules.numeric.fps.accept.min.is_some());
        assert!(rules.numeric.fps.accept.max.is_some());
        assert!(rules.numeric.bitrate_kbps.accept.min.is_some());
        assert!(rules.numeric.bitrate_kbps.accept.max.is_some());
        assert!(!rules.numeric.fps.accept.message.is_empty());
        assert!(!rules.provenance.camera_fingerprint_keys.is_empty());
        assert!(!rules.telemetry.tags.is_empty());
        // All telemetry fourCCs must be exactly 4 ASCII bytes.
        for tag in &rules.telemetry.tags {
            assert!(
                tag.fourcc_bytes().is_some(),
                "tag {} is not a 4-byte fourcc",
                tag.fourcc
            );
        }
    }

    #[test]
    fn band_one_end_only() {
        // A band can omit either edge for an open-sided range. The
        // shared message is required, but the numeric ends are
        // independently optional.
        let json = r#"{ "min": 8000, "message": "low" }"#;
        let band: Band<u64> = serde_json::from_str(json).unwrap();
        assert_eq!(band.min, Some(8000));
        assert!(band.max.is_none());
        assert_eq!(band.message, "low");
    }

    #[test]
    fn render_substitutes_named_keys() {
        let out = render(
            "Frame rate {fps:.1}fps is below {floor:.0}fps",
            &[("fps", Sub::Float(29.7)), ("floor", Sub::Float(20.0))],
        );
        assert_eq!(out, "Frame rate 29.7fps is below 20fps");
    }

    #[test]
    fn render_handles_string_values() {
        let out = render(
            "Bitrate {bitrate} is below the floor",
            &[("bitrate", Sub::Str("12.0Mbps".to_owned()))],
        );
        assert_eq!(out, "Bitrate 12.0Mbps is below the floor");
    }

    #[test]
    fn render_leaves_unknown_keys_alone() {
        let out = render("Hello {who}", &[]);
        assert_eq!(out, "Hello {who}");
    }

    // ---- loader integration tests --------------------------------------
    //
    // These spin up a tiny_http server on a random localhost port, point
    // the loader at a temp cache dir, and assert observable filesystem
    // and return-value behaviour for each branch (200 OK, 304 Not
    // Modified, network error, corrupt cache).

    use std::sync::mpsc;
    use std::thread;

    fn make_temp_cache() -> CachePaths {
        let dir = std::env::temp_dir()
            .join("lw-test-rules")
            .join(uuid::Uuid::new_v4().to_string());
        CachePaths::under(dir)
    }

    /// Spawn a one-shot HTTP server on a free localhost port. The
    /// handler closure inspects each incoming request and writes a
    /// `tiny_http::Response`. The thread shuts down when the
    /// `tiny_http::Server` is dropped — we drop it via the join handle
    /// after the request count hits the expected number.
    fn spawn_server(
        handler: impl Fn(usize, &tiny_http::Request) -> tiny_http::Response<std::io::Cursor<Vec<u8>>>
        + Send
        + 'static,
        expected_requests: usize,
    ) -> (String, mpsc::Receiver<Vec<RecordedRequest>>) {
        let server =
            tiny_http::Server::http("127.0.0.1:0").expect("bind ephemeral port for test server");
        let url = format!(
            "http://{}/video_quality_rules.json",
            server.server_addr().to_ip().unwrap()
        );
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut recorded: Vec<RecordedRequest> = Vec::new();
            for i in 0..expected_requests {
                match server.recv() {
                    Ok(request) => {
                        recorded.push(RecordedRequest::from(&request));
                        let response = handler(i, &request);
                        let _ = request.respond(response);
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(recorded);
        });
        (url, rx)
    }

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        if_none_match: Option<String>,
    }

    impl From<&tiny_http::Request> for RecordedRequest {
        fn from(req: &tiny_http::Request) -> Self {
            let if_none_match = req
                .headers()
                .iter()
                .find(|h| {
                    h.field
                        .as_str()
                        .as_str()
                        .eq_ignore_ascii_case("If-None-Match")
                })
                .map(|h| h.value.as_str().to_string());
            Self { if_none_match }
        }
    }

    fn ok_response(body: &[u8], etag: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
        tiny_http::Response::from_data(body.to_vec())
            .with_header(tiny_http::Header::from_bytes(b"ETag".as_ref(), etag.as_bytes()).unwrap())
    }

    fn not_modified_response() -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
        tiny_http::Response::from_data(Vec::new()).with_status_code(tiny_http::StatusCode(304))
    }

    #[tokio::test]
    async fn loader_seeds_when_cache_absent_and_writes_etag_on_200() {
        // Server returns 200 with the embedded body and a fresh ETag.
        let (url, rx) = spawn_server(
            |_, _| ok_response(EMBEDDED_RULES_JSON.as_bytes(), "\"v1\""),
            1,
        );

        let cache = make_temp_cache();
        let body_path = cache.body.clone();
        let etag_path = cache.etag.clone();

        let _rules = VideoRules::load_with(&url, cache).await;
        let recorded = rx.recv().expect("server recorded");

        // The first request had no ETag (cache was empty).
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].if_none_match.is_none());
        // The server's response now lives in the cache, with its ETag.
        assert!(body_path.exists(), "body should have been written");
        let etag = std::fs::read_to_string(&etag_path).expect("etag written");
        assert_eq!(etag, "\"v1\"");
    }

    #[tokio::test]
    async fn loader_keeps_cache_on_304() {
        // First call: server 200, body + etag get written. Second call:
        // server 304, cache must stay byte-for-byte identical.
        let (url, _rx) = spawn_server(
            move |i, _| match i {
                0 => ok_response(EMBEDDED_RULES_JSON.as_bytes(), "\"v1\""),
                _ => not_modified_response(),
            },
            2,
        );

        let cache = make_temp_cache();
        let body_path = cache.body.clone();
        let etag_path = cache.etag.clone();

        let _ = VideoRules::load_with(&url, CachePaths::under(cache.dir.clone())).await;
        let body_after_first = std::fs::read(&body_path).unwrap();
        let etag_after_first = std::fs::read_to_string(&etag_path).unwrap();

        let _ = VideoRules::load_with(&url, CachePaths::under(cache.dir.clone())).await;
        let body_after_second = std::fs::read(&body_path).unwrap();
        let etag_after_second = std::fs::read_to_string(&etag_path).unwrap();

        assert_eq!(body_after_first, body_after_second);
        assert_eq!(etag_after_first, etag_after_second);
        assert_eq!(etag_after_second, "\"v1\"");
    }

    #[tokio::test]
    async fn loader_recovers_from_corrupt_cache_via_embedded_seed() {
        // Cache file exists but contains garbage. Network is dead
        // (URL points at a port nothing is listening on). The loader
        // must re-seed the cache from the embedded copy and return
        // a valid VideoRules.
        let cache = make_temp_cache();
        cache.ensure_dir().expect("mkdir");
        std::fs::write(&cache.body, b"{ this is not json }").unwrap();

        // Pick a port that's almost certainly closed: bind+drop to
        // capture a free one without keeping it open.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = probe.local_addr().unwrap().port();
        drop(probe);
        let dead_url = format!("http://127.0.0.1:{dead_port}/rules.json");

        let body_path = cache.body.clone();
        let rules = VideoRules::load_with(&dead_url, cache).await;

        // Returned rules came from the embedded copy.
        assert_eq!(rules.schema_version, 1);
        // The cache was re-seeded, so the next launch parses cleanly
        // even without network.
        let after = std::fs::read_to_string(&body_path).unwrap();
        let _: VideoRules = serde_json::from_str(&after).expect("re-seeded body parses");
    }
}
