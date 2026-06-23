mod record;
mod stats;

pub use record::{RequestRecord, TokenUsage};
pub use stats::Stats;

use std::sync::Mutex;

pub struct Tracker {
    db: Mutex<rusqlite::Connection>,
}

impl Tracker {
    pub fn new(path: &str) -> Self {
        let conn = rusqlite::Connection::open(path).expect("open stats db");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS requests (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                ts                TEXT    NOT NULL DEFAULT (datetime('now')),
                ip                TEXT    NOT NULL,
                endpoint          TEXT    NOT NULL,
                model_requested   TEXT,
                model_used        TEXT,
                status            INTEGER NOT NULL,
                latency_ms        INTEGER,
                vision_swap       INTEGER NOT NULL DEFAULT 0,
                prompt_tokens     INTEGER,
                completion_tokens INTEGER,
                total_tokens      INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_req_ts    ON requests(ts);
            CREATE INDEX IF NOT EXISTS idx_req_model ON requests(model_used);
            CREATE INDEX IF NOT EXISTS idx_req_ip    ON requests(ip);",
        )
        .expect("create stats tables");
        tracing::info!("stats db: {path}");
        Self {
            db: Mutex::new(conn),
        }
    }

    pub fn record(&self, rec: RequestRecord) {
        let db = self.db.lock().unwrap();
        let (pt, ct, tt) = match &rec.usage {
            Some(u) => (
                Some(u.prompt_tokens as i64),
                Some(u.completion_tokens as i64),
                Some(u.total_tokens as i64),
            ),
            None => (None, None, None),
        };
        if let Err(e) = db.execute(
            "INSERT INTO requests \
             (ip, endpoint, model_requested, model_used, status, latency_ms, vision_swap, \
              prompt_tokens, completion_tokens, total_tokens) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                rec.ip,
                rec.endpoint,
                rec.model_requested,
                rec.model_used,
                rec.status,
                rec.latency_ms,
                rec.vision_swap as i32,
                pt,
                ct,
                tt,
            ],
        ) {
            tracing::warn!("stats record error: {e}");
        }
    }

    pub fn stats(&self) -> Stats {
        let db = self.db.lock().unwrap();
        Stats::from_db(&db)
    }
}
