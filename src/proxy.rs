use axum::{
    body::Body,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use std::time::Instant;

use crate::AppState;
use crate::tracker::TokenUsage;

pub struct ForwardResult {
    pub response: Response,
    pub status: u16,
    pub latency_ms: u64,
    pub usage: Option<TokenUsage>,
}

/// Inject the real key, buffer the upstream response to extract token usage,
/// then forward the body to the client.
pub async fn forward_with_metrics(
    state: &AppState,
    req: reqwest::RequestBuilder,
) -> ForwardResult {
    let start = Instant::now();
    let req = req.header(header::AUTHORIZATION, format!("Bearer {}", state.mimo_key));
    match req.send().await {
        Ok(r) => {
            let status = r.status().as_u16();
            let resp_headers = r.headers().clone();
            let latency_ms = start.elapsed().as_millis() as u64;

            // Buffer the body so we can extract token usage.
            // Trade-off: streaming SSE bodies are fully buffered before
            // forwarding. Token tracking > real-time streaming for now.
            match r.bytes().await {
                Ok(body_bytes) => {
                    let usage = extract_usage(&body_bytes);

                    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
                    let mut builder = Response::builder().status(code);
                    for name in [header::CONTENT_TYPE, header::CACHE_CONTROL] {
                        if let Some(v) = resp_headers.get(&name) {
                            builder = builder.header(name, v.clone());
                        }
                    }
                    let response = builder
                        .body(Body::from(body_bytes))
                        .unwrap_or_else(|_| {
                            (StatusCode::INTERNAL_SERVER_ERROR, "response build error")
                                .into_response()
                        });
                    ForwardResult {
                        response,
                        status,
                        latency_ms,
                        usage,
                    }
                }
                Err(e) => {
                    tracing::warn!("upstream body read error: {e}");
                    ForwardResult {
                        response: (StatusCode::BAD_GATEWAY, "upstream error").into_response(),
                        status: 502,
                        latency_ms,
                        usage: None,
                    }
                }
            }
        }
        Err(e) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            tracing::warn!("upstream error: {e}");
            ForwardResult {
                response: (StatusCode::BAD_GATEWAY, "upstream error").into_response(),
                status: 502,
                latency_ms,
                usage: None,
            }
        }
    }
}

/// Extract token usage from a response body.
/// Tries JSON first (non-streaming), then SSE (streaming).
fn extract_usage(body: &[u8]) -> Option<TokenUsage> {
    extract_usage_json(body).or_else(|| extract_usage_sse(body))
}

/// Non-streaming: top-level `"usage"` object.
fn extract_usage_json(body: &[u8]) -> Option<TokenUsage> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    parse_usage_value(v.get("usage")?)
}

/// Streaming SSE: scan `data: {...}` lines for a chunk containing `"usage"`.
fn extract_usage_sse(body: &[u8]) -> Option<TokenUsage> {
    let text = std::str::from_utf8(body).ok()?;
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            continue;
        }
        if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data) {
            if let Some(usage) = chunk.get("usage") {
                return parse_usage_value(usage);
            }
        }
    }
    None
}

fn parse_usage_value(v: &serde_json::Value) -> Option<TokenUsage> {
    Some(TokenUsage {
        prompt_tokens: v.get("prompt_tokens")?.as_u64()?,
        completion_tokens: v.get("completion_tokens")?.as_u64()?,
        total_tokens: v.get("total_tokens")?.as_u64()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_usage_from_json_body() {
        let body = serde_json::json!({
            "id": "chatcmpl-123",
            "choices": [],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        });
        let usage = extract_usage(body.to_string().as_bytes()).unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
        assert_eq!(usage.total_tokens, 30);
    }

    #[test]
    fn extract_usage_from_sse_body() {
        let body = b"data: {\"id\":\"1\",\"choices\":[]}\n\ndata: {\"id\":\"2\",\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":15,\"total_tokens\":20}}\n\ndata: [DONE]\n";
        let usage = extract_usage(body).unwrap();
        assert_eq!(usage.prompt_tokens, 5);
        assert_eq!(usage.completion_tokens, 15);
        assert_eq!(usage.total_tokens, 20);
    }

    #[test]
    fn extract_usage_returns_none_when_missing() {
        let body = b"data: {\"id\":\"1\",\"choices\":[]}\n\ndata: [DONE]\n";
        assert!(extract_usage(body).is_none());
    }

    #[test]
    fn extract_usage_returns_none_for_empty() {
        assert!(extract_usage(b"").is_none());
    }
}
