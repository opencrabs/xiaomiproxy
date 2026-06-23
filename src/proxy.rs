use axum::{
    body::Body,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use std::time::Instant;

use crate::AppState;

pub struct ForwardResult {
    pub response: Response,
    pub status: u16,
    pub latency_ms: u64,
}

/// Inject the real key and stream the upstream response straight through.
/// Returns the response along with upstream status code and latency.
pub async fn forward_with_metrics(
    state: &AppState,
    req: reqwest::RequestBuilder,
) -> ForwardResult {
    let start = Instant::now();
    let req = req.header(header::AUTHORIZATION, format!("Bearer {}", state.mimo_key));
    match req.send().await {
        Ok(r) => {
            let status = r.status().as_u16();
            let latency_ms = start.elapsed().as_millis() as u64;
            let mut builder = Response::builder().status(r.status());
            // Preserve content-type (text/event-stream for SSE, else json).
            for name in [header::CONTENT_TYPE, header::CACHE_CONTROL] {
                if let Some(v) = r.headers().get(&name) {
                    builder = builder.header(name, v.clone());
                }
            }
            let response = builder
                .body(Body::from_stream(r.bytes_stream()))
                .unwrap_or_else(|_| {
                    (StatusCode::INTERNAL_SERVER_ERROR, "response build error").into_response()
                });
            ForwardResult {
                response,
                status,
                latency_ms,
            }
        }
        Err(e) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            tracing::warn!("upstream error: {e}");
            ForwardResult {
                response: (StatusCode::BAD_GATEWAY, "upstream error").into_response(),
                status: 502,
                latency_ms,
            }
        }
    }
}
