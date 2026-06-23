use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize)]
pub struct Stats {
    pub total_requests: u64,
    pub requests_by_model: HashMap<String, u64>,
    pub requests_by_endpoint: HashMap<String, u64>,
    pub unique_ips: u64,
    pub vision_swaps: u64,
    pub errors: u64,
    pub avg_latency_ms: f64,
    pub hourly: Vec<HourlyCount>,
}

#[derive(Serialize)]
pub struct HourlyCount {
    pub hour: String,
    pub count: u64,
}

impl Stats {
    pub fn from_db(db: &rusqlite::Connection) -> Self {
        let total: u64 = db
            .query_row("SELECT COUNT(*) FROM requests", [], |r| r.get(0))
            .unwrap_or(0);

        let requests_by_model = Self::group_by(
            db,
            "SELECT COALESCE(model_used, 'unknown'), COUNT(*) FROM requests GROUP BY model_used",
        );
        let requests_by_endpoint =
            Self::group_by(db, "SELECT endpoint, COUNT(*) FROM requests GROUP BY endpoint");

        let unique_ips: u64 = db
            .query_row("SELECT COUNT(DISTINCT ip) FROM requests", [], |r| r.get(0))
            .unwrap_or(0);

        let vision_swaps: u64 = db
            .query_row(
                "SELECT COUNT(*) FROM requests WHERE vision_swap = 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let errors: u64 = db
            .query_row(
                "SELECT COUNT(*) FROM requests WHERE status >= 400",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let avg_latency_ms: f64 = db
            .query_row(
                "SELECT COALESCE(AVG(latency_ms), 0.0) FROM requests",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0.0);

        let hourly = Self::hourly(db);

        Stats {
            total_requests: total,
            requests_by_model,
            requests_by_endpoint,
            unique_ips,
            vision_swaps,
            errors,
            avg_latency_ms,
            hourly,
        }
    }

    fn group_by(db: &rusqlite::Connection, sql: &str) -> HashMap<String, u64> {
        let mut map = HashMap::new();
        if let Ok(mut stmt) = db.prepare(sql) {
            if let Ok(rows) =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))
            {
                for row in rows.flatten() {
                    map.insert(row.0, row.1);
                }
            }
        }
        map
    }

    fn hourly(db: &rusqlite::Connection) -> Vec<HourlyCount> {
        let mut out = Vec::new();
        if let Ok(mut stmt) = db.prepare(
            "SELECT strftime('%Y-%m-%dT%H:00', ts) AS hour, COUNT(*) AS cnt \
             FROM requests GROUP BY hour ORDER BY hour",
        ) {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok(HourlyCount {
                    hour: r.get::<_, String>(0)?,
                    count: r.get::<_, u64>(1)?,
                })
            }) {
                for row in rows.flatten() {
                    out.push(row);
                }
            }
        }
        out
    }
}
