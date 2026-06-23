/// Per-request record written to SQLite after each proxied call.
pub struct RequestRecord {
    pub ip: String,
    pub endpoint: &'static str,
    pub model_requested: String,
    pub model_used: String,
    pub status: u16,
    pub latency_ms: u64,
    pub vision_swap: bool,
}
