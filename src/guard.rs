use axum::{
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use std::net::IpAddr;

use crate::AppState;

/// Real client IP. Prefer `X-Forwarded-For` (set by the fronting nginx),
/// fall back to the socket peer.
pub fn client_ip(headers: &HeaderMap, peer: IpAddr) -> IpAddr {
    headers
        .get("x-forwarded-for")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(peer)
}

pub fn auth_ok(headers: &HeaderMap, expected: &str) -> bool {
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

pub fn ua_ok(headers: &HeaderMap) -> bool {
    headers
        .get(header::USER_AGENT)
        .and_then(|h| h.to_str().ok())
        .map(|ua| ua.to_ascii_lowercase().contains("opencrabs"))
        .unwrap_or(false)
}

/// Runs every gate. Returns `Some(rejection)` to deny, `None` to proceed.
pub fn gate(state: &AppState, headers: &HeaderMap, peer: IpAddr) -> Option<Response> {
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
