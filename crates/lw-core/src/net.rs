//! Shared `reqwest::Client` construction for every HTTP client in lw-core.
//!
//! All three long-lived clients — the Linewise API client, the Firebase
//! auth client, and the GCS upload backend — are built once at startup
//! and live for the whole session. Historically each called
//! `reqwest::Client::new()` (or its own ad-hoc builder) with no explicit
//! proxy, so they silently inherited the Windows system-proxy snapshot
//! taken at process launch. A user running v2ray who flips GLOBAL↔RULE
//! mid-session changes that system proxy out from under the already-built
//! clients, and the connections wedge until the app is restarted.
//!
//! Routing through a *fixed* proxy the user points at v2ray's local HTTP
//! inbound (e.g. `http://127.0.0.1:10809`) is stable across mode switches,
//! so the existing retry loops actually recover. This helper centralises
//! that wiring so the three call sites stay identical.
//!
//! NOTE: reqwest is built in this workspace with
//! `features = ["rustls", "json", "stream", "form"]` and **no `socks`
//! feature**, so only HTTP/HTTPS proxies work. `reqwest::Proxy::all`
//! tunnels HTTPS through an `http://host:port` proxy via the CONNECT
//! method, which is exactly what v2ray's HTTP inbound speaks. A SOCKS
//! URL would fail to build — point the setting at the HTTP inbound.

use std::time::Duration;

/// Build a `reqwest::Client` shared across lw-core's HTTP clients.
///
/// * `proxy` — optional proxy URL. `None` or an all-whitespace string
///   means "no explicit proxy" (the historical behaviour: reqwest still
///   honours the system proxy env/registry at build time). A non-empty
///   value is applied via [`reqwest::Proxy::all`], which covers both HTTP
///   and HTTPS (HTTPS is tunnelled with CONNECT). Use an `http://host:port`
///   URL — SOCKS is not compiled in (see module docs).
/// * `total` — optional overall request timeout (`Client::timeout`). The
///   GCS backend keeps its long 5-minute budget for large chunk PUTs; the
///   API and auth clients pass a shorter sane ceiling.
/// * `connect` — TCP/TLS connect timeout (`Client::connect_timeout`), so a
///   dead or wrong proxy fails fast instead of hanging the whole session.
/// * `read` — optional idle/read timeout (`Client::read_timeout`): the max gap
///   between received bytes. The GCS upload client sets this so a half-open
///   connection (peer silently gone — common on flaky/metered links) is broken
///   in seconds and retried, instead of stalling until the much longer `total`
///   budget elapses and holding an upload slot the whole time.
///
/// On an invalid proxy URL this does **not** panic and does not propagate
/// an error: it logs a warning and falls back to building the client with
/// no explicit proxy. A typo in the settings field must never brick every
/// network client at startup — degrading to "no proxy" keeps the app
/// usable (the user sees uploads behave as before and can fix the typo).
pub fn build_http_client(
    proxy: Option<&str>,
    total: Option<Duration>,
    connect: Duration,
    read: Option<Duration>,
) -> reqwest::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().connect_timeout(connect);
    if let Some(total) = total {
        builder = builder.timeout(total);
    }
    if let Some(read) = read {
        builder = builder.read_timeout(read);
    }

    let trimmed = proxy.map(str::trim).filter(|p| !p.is_empty());
    if let Some(url) = trimmed {
        match reqwest::Proxy::all(url) {
            Ok(proxy) => {
                tracing::info!(proxy = %url, "HTTP clients routing through configured proxy");
                builder = builder.proxy(proxy);
            }
            Err(e) => {
                tracing::warn!(
                    proxy = %url,
                    error = %e,
                    "invalid proxy URL — falling back to no explicit proxy. \
                     Expected an http://host:port URL (SOCKS is not supported)."
                );
            }
        }
    }

    builder.build()
}
