/// Per-request record written to SQLite after each proxied call.
pub struct RequestRecord {
    pub ip: String,
    pub endpoint: &'static str,
    pub model_requested: String,
    pub model_used: String,
    pub status: u16,
    pub latency_ms: u64,
    pub vision_swap: bool,
    pub usage: Option<TokenUsage>,
}

/// Token usage extracted from the upstream response body.
#[derive(Debug, Clone)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}
