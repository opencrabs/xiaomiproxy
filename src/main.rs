//! xiaomiproxy — key-injecting reverse proxy for the opencrabs x Xiaomi MiMo collab.
//!
//! The OpenCrabs binary points `[providers.xiaomi] base_url` at this proxy and
//! sends NO real key. The proxy holds the real Xiaomi key (env `XIAOMI_MIMO_API_KEY`),
//! injects it server-side, and streams the response back. So the key never ships
//! in the (open-source) OpenCrabs binary.
//!
//! Access control for a time-boxed free window — pragmatic, not cryptographic:
//!   1. Hard cutoff date (free window ends; returns 410 after).
//!   2. Optional shared app token (`APP_TOKEN`) the client sends; rotatable.
//!   3. Optional User-Agent allowlist (`opencrabs`).
//!   4. Per-IP rate limit.

mod guard;
mod proxy;
mod rate;
mod tracker;
mod vision;

use axum::{
    body::Bytes,
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use std::{
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

const DEFAULT_UPSTREAM: &str = "https://api.xiaomimimo.com";
const DEFAULT_CUTOFF: &str = "2026-06-27";

pub struct AppState {
    client: reqwest::Client,
    mimo_key: String,
    app_token: Option<String>,
    require_ua: bool,
    cutoff: chrono::NaiveDate,
    upstream: String,
    rate: rate::RateLimiter,
    pub tracker: tracker::Tracker,
}

// ── Route handlers ──────────────────────────────────────────────────────────

async fn chat_completions(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ip = guard::client_ip(&headers, peer.ip()).to_string();

    if let Some(rejection) = guard::gate(&state, &headers, peer.ip()) {
        state.tracker.record(tracker::RequestRecord {
            ip,
            endpoint: "/v1/chat/completions",
            model_requested: String::new(),
            model_used: String::new(),
            status: rejection.status().as_u16(),
            latency_ms: 0,
            vision_swap: false,
        });
        return rejection;
    }

    // Parse body to check for image input and maybe swap model.
    let mut body_json: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            let url = format!("{}/v1/chat/completions", state.upstream);
            let req = state
                .client
                .post(&url)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body);
            let result = proxy::forward_with_metrics(&state, req).await;
            state.tracker.record(tracker::RequestRecord {
                ip,
                endpoint: "/v1/chat/completions",
                model_requested: String::new(),
                model_used: String::new(),
                status: result.status,
                latency_ms: result.latency_ms,
                vision_swap: false,
            });
            return result.response;
        }
    };

    let model_requested = body_json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let model_used = vision::maybe_swap_vision_model(&mut body_json);
    let vision_swap = model_requested != model_used;
    let body_bytes = body_json.to_string();

    let url = format!("{}/v1/chat/completions", state.upstream);
    let req = state
        .client
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body_bytes);
    let result = proxy::forward_with_metrics(&state, req).await;

    state.tracker.record(tracker::RequestRecord {
        ip,
        endpoint: "/v1/chat/completions",
        model_requested,
        model_used,
        status: result.status,
        latency_ms: result.latency_ms,
        vision_swap,
    });

    result.response
}

async fn list_models(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let ip = guard::client_ip(&headers, peer.ip()).to_string();

    if let Some(rejection) = guard::gate(&state, &headers, peer.ip()) {
        state.tracker.record(tracker::RequestRecord {
            ip,
            endpoint: "/v1/models",
            model_requested: String::new(),
            model_used: String::new(),
            status: rejection.status().as_u16(),
            latency_ms: 0,
            vision_swap: false,
        });
        return rejection;
    }

    let start = std::time::Instant::now();
    let url = format!("{}/v1/models", state.upstream);
    let resp = match state
        .client
        .get(&url)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.mimo_key),
        )
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            tracing::warn!("models upstream error: {e}");
            state.tracker.record(tracker::RequestRecord {
                ip,
                endpoint: "/v1/models",
                model_requested: String::new(),
                model_used: String::new(),
                status: 502,
                latency_ms,
                vision_swap: false,
            });
            return (StatusCode::BAD_GATEWAY, "upstream error").into_response();
        }
    };
    let status = resp.status().as_u16();
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            tracing::warn!("models read error: {e}");
            state.tracker.record(tracker::RequestRecord {
                ip,
                endpoint: "/v1/models",
                model_requested: String::new(),
                model_used: String::new(),
                status: 502,
                latency_ms,
                vision_swap: false,
            });
            return (StatusCode::BAD_GATEWAY, "upstream error").into_response();
        }
    };

    let latency_ms = start.elapsed().as_millis() as u64;

    // Filter to chat models; if the payload isn't the shape we expect, pass it
    // through untouched rather than dropping anything.
    let response = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(mut v) => {
            if let Some(arr) = v.get_mut("data").and_then(|d| d.as_array_mut()) {
                arr.retain(|m| {
                    m.get("id")
                        .and_then(|i| i.as_str())
                        .map(vision::is_chat_model)
                        .unwrap_or(true)
                });
            }
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
                [(header::CONTENT_TYPE, "application/json")],
                v.to_string(),
            )
                .into_response()
        }
        Err(_) => (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), bytes).into_response(),
    };

    state.tracker.record(tracker::RequestRecord {
        ip,
        endpoint: "/v1/models",
        model_requested: String::new(),
        model_used: String::new(),
        status,
        latency_ms,
        vision_swap: false,
    });

    response
}

async fn stats_endpoint(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(tok) = &state.app_token {
        if !guard::auth_ok(&headers, tok) {
            return (StatusCode::UNAUTHORIZED, "missing or invalid app token").into_response();
        }
    }
    let stats = state.tracker.stats();
    axum::Json(stats).into_response()
}

async fn health() -> &'static str {
    "ok"
}

// ── Boot ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
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
    let stats_db =
        std::env::var("STATS_DB").unwrap_or_else(|_| "./xiaomiproxy.db".to_string());

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
        rate: rate::RateLimiter::new(rate_max, Duration::from_secs(60)),
        tracker: tracker::Tracker::new(&stats_db),
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/stats", get(stats_endpoint))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|e| panic!("bind {bind}: {e}"));
    tracing::info!(
        "xiaomiproxy listening on {bind} (upstream {}, cutoff {}, rate {rate_max}/min, app_token {}, stats {stats_db})",
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
/// back to a runtime env var of the same name.
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
