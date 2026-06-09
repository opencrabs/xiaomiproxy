//! xiaomiproxy — key-injecting reverse proxy for the opencrabs x Xiaomi MiMo collab.
//!
//! The OpenCrabs binary points `[providers.xiaomi] base_url` at this proxy and
//! sends NO real key. The proxy holds the real Xiaomi key (env `MIMO_API_KEY`),
//! injects it server-side, and streams the response back. So the key never ships
//! in the (open-source) OpenCrabs binary.
//!
//! Access control for a time-boxed free window — pragmatic, not cryptographic
//! (an open-source client can't hold an unextractable secret):
//!   1. Hard cutoff date (free window ends; returns 410 after).
//!   2. Optional shared app token (`APP_TOKEN`) the client sends; rotatable.
//!   3. Optional User-Agent allowlist (`opencrabs`).
//!   4. Per-IP rate limit.
//!
//! The Xiaomi key stays safe under all of these — the worst abuse case is some
//! free-riding on the proxy, bounded by the rate limit and the 2-week window.

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const DEFAULT_UPSTREAM: &str = "https://api.xiaomimimo.com";
const DEFAULT_CUTOFF: &str = "2026-06-25";

struct AppState {
    client: reqwest::Client,
    mimo_key: String,
    app_token: Option<String>,
    require_ua: bool,
    cutoff: chrono::NaiveDate,
    upstream: String,
    rate: RateLimiter,
}

// ── Per-IP fixed-window rate limiter ────────────────────────────────────────

struct RateLimiter {
    inner: Mutex<HashMap<IpAddr, (Instant, u32)>>,
    max: u32,
    window: Duration,
}

impl RateLimiter {
    fn new(max: u32, window: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max,
            window,
        }
    }

    fn allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        // Keep the map from growing unbounded over the window's lifetime.
        if map.len() > 4096 {
            map.retain(|_, (start, _)| now.duration_since(*start) <= self.window);
        }
        let entry = map.entry(ip).or_insert((now, 0));
        if now.duration_since(entry.0) > self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.max
    }
}

// ── Guards ──────────────────────────────────────────────────────────────────

/// Real client IP — prefer `X-Forwarded-For` (set by the fronting nginx),
/// fall back to the socket peer.
fn client_ip(headers: &HeaderMap, peer: IpAddr) -> IpAddr {
    headers
        .get("x-forwarded-for")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(peer)
}

fn auth_ok(headers: &HeaderMap, expected: &str) -> bool {
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
    {
        if v.strip_prefix("Bearer ").map(str::trim) == Some(expected) {
            return true;
        }
    }
    headers
        .get("x-opencrabs-token")
        .and_then(|h| h.to_str().ok())
        .map(|v| v.trim() == expected)
        .unwrap_or(false)
}

fn ua_ok(headers: &HeaderMap) -> bool {
    headers
        .get(header::USER_AGENT)
        .and_then(|h| h.to_str().ok())
        .map(|ua| ua.to_ascii_lowercase().contains("opencrabs"))
        .unwrap_or(false)
}

/// Runs every gate. Returns `Some(rejection)` to deny, `None` to proceed.
fn gate(state: &AppState, headers: &HeaderMap, peer: IpAddr) -> Option<Response> {
    if chrono::Utc::now().date_naive() > state.cutoff {
        return Some((StatusCode::GONE, "Xiaomi free collab window has ended").into_response());
    }
    if let Some(tok) = &state.app_token {
        if !auth_ok(headers, tok) {
            return Some(
                (StatusCode::UNAUTHORIZED, "missing or invalid app token").into_response(),
            );
        }
    }
    if state.require_ua && !ua_ok(headers) {
        return Some((StatusCode::FORBIDDEN, "forbidden").into_response());
    }
    if !state.rate.allow(client_ip(headers, peer)) {
        return Some((StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response());
    }
    None
}

// ── Forwarding ──────────────────────────────────────────────────────────────

/// Inject the real key and stream the upstream response straight through. The
/// client's Authorization (empty during the free window) is always replaced.
async fn forward(state: &AppState, req: reqwest::RequestBuilder) -> Response {
    let req = req.header(header::AUTHORIZATION, format!("Bearer {}", state.mimo_key));
    match req.send().await {
        Ok(r) => {
            let mut builder = Response::builder().status(r.status());
            // Preserve content-type (text/event-stream for SSE, else json).
            for name in [header::CONTENT_TYPE, header::CACHE_CONTROL] {
                if let Some(v) = r.headers().get(&name) {
                    builder = builder.header(name, v.clone());
                }
            }
            builder
                .body(Body::from_stream(r.bytes_stream()))
                .unwrap_or_else(|_| {
                    (StatusCode::INTERNAL_SERVER_ERROR, "response build error").into_response()
                })
        }
        Err(e) => {
            tracing::warn!("upstream error: {e}");
            (StatusCode::BAD_GATEWAY, "upstream error").into_response()
        }
    }
}

async fn chat_completions(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(rejection) = gate(&state, &headers, peer.ip()) {
        return rejection;
    }
    let url = format!("{}/v1/chat/completions", state.upstream);
    let req = state
        .client
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body);
    forward(&state, req).await
}

/// Non-chat models (text-to-speech, speech recognition, voice) — hidden from
/// the model list so the OpenCrabs picker only offers chat-capable models.
fn is_chat_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    !(id.contains("tts") || id.contains("asr") || id.contains("voice"))
}

async fn list_models(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(rejection) = gate(&state, &headers, peer.ip()) {
        return rejection;
    }
    let url = format!("{}/v1/models", state.upstream);
    let resp = match state
        .client
        .get(&url)
        .header(header::AUTHORIZATION, format!("Bearer {}", state.mimo_key))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("models upstream error: {e}");
            return (StatusCode::BAD_GATEWAY, "upstream error").into_response();
        }
    };
    let status = resp.status();
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("models read error: {e}");
            return (StatusCode::BAD_GATEWAY, "upstream error").into_response();
        }
    };
    // Filter to chat models; if the payload isn't the shape we expect, pass it
    // through untouched rather than dropping anything.
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(mut v) => {
            if let Some(arr) = v.get_mut("data").and_then(|d| d.as_array_mut()) {
                arr.retain(|m| {
                    m.get("id")
                        .and_then(|i| i.as_str())
                        .map(is_chat_model)
                        .unwrap_or(true)
                });
            }
            (
                status,
                [(header::CONTENT_TYPE, "application/json")],
                v.to_string(),
            )
                .into_response()
        }
        Err(_) => (status, bytes).into_response(),
    }
}

async fn health() -> &'static str {
    "ok"
}

// ── Boot ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Baked in from .env at build time (build.rs); runtime env can override.
    let mimo_key = bundled_or_runtime("XIAOMI_MIMO_API_KEY")
        .expect("XIAOMI_MIMO_API_KEY must be in .env at build time, or set at runtime");
    let app_token = bundled_or_runtime("APP_TOKEN");
    let require_ua = env_flag("REQUIRE_OPENCRABS_UA");
    let upstream =
        std::env::var("UPSTREAM_BASE_URL").unwrap_or_else(|_| DEFAULT_UPSTREAM.to_string());
    let cutoff_str = std::env::var("CUTOFF_DATE").unwrap_or_else(|_| DEFAULT_CUTOFF.to_string());
    let cutoff = chrono::NaiveDate::parse_from_str(&cutoff_str, "%Y-%m-%d")
        .expect("CUTOFF_DATE must be YYYY-MM-DD");
    let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".to_string());
    let rate_max: u32 = std::env::var("RATE_MAX_PER_MIN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    if app_token.is_none() {
        tracing::warn!(
            "APP_TOKEN not set — only the rate limit and cutoff guard this proxy. Set APP_TOKEN in production."
        );
    }

    let state = Arc::new(AppState {
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .expect("build reqwest client"),
        mimo_key,
        app_token,
        require_ua,
        cutoff,
        upstream,
        rate: RateLimiter::new(rate_max, Duration::from_secs(60)),
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|e| panic!("bind {bind}: {e}"));
    tracing::info!(
        "xiaomiproxy listening on {bind} (upstream {}, cutoff {}, rate {rate_max}/min, app_token {})",
        state.upstream,
        state.cutoff,
        if state.app_token.is_some() { "on" } else { "off" },
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .expect("server error");
}

/// Prefer the value baked in at build time (from `.env` via build.rs); fall
/// back to a runtime env var of the same name. `option_env!` needs a literal,
/// hence the explicit match.
fn bundled_or_runtime(name: &str) -> Option<String> {
    let bundled = match name {
        "XIAOMI_MIMO_API_KEY" => option_env!("XIAOMI_MIMO_API_KEY"),
        "APP_TOKEN" => option_env!("APP_TOKEN"),
        _ => None,
    };
    bundled
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var(name).ok().filter(|s| !s.is_empty()))
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => tracing::warn!("SIGTERM handler failed: {e}"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutting down");
}
