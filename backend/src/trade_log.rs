use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Serialize)]
pub struct TradeSummary {
    pub total: usize,
    pub closed: usize,
    pub tp_hits: usize,
    pub sl_hits: usize,
    pub expired: usize,
    pub pending: usize,
    /// TP count / closed count. None when no closed trades.
    pub win_rate: Option<f64>,
    /// Average R-multiple across closed trades: TP_HIT = +reward/risk, SL_HIT = -1.0.
    pub avg_r_multiple: Option<f64>,
    /// avg_r_multiple expressed as expectancy (same value, named for clarity).
    pub expectancy: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TradeRecord {
    pub id: i64,
    pub created_at: i64,
    pub symbol: String,
    pub entry_price: f64,
    pub take_profit: f64,
    pub stop_loss: f64,
    pub thesis: Option<String>,
    pub outcome: Option<String>,
    pub position_size_pct: Option<f64>,
    pub leverage: Option<f64>,
}

#[derive(Clone)]
pub struct TradeLog {
    conn: Arc<Mutex<Connection>>,
}

impl TradeLog {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).context("failed to open trade log database")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS trade_log (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at        INTEGER NOT NULL,
                symbol            TEXT    NOT NULL,
                entry_price       REAL    NOT NULL,
                take_profit       REAL    NOT NULL,
                stop_loss         REAL    NOT NULL,
                thesis            TEXT,
                outcome           TEXT,
                position_size_pct REAL,
                leverage          REAL
            );",
        )
        .context("failed to create trade_log table")?;
        // Migrate existing databases that predate optional columns.
        let _ = conn.execute("ALTER TABLE trade_log ADD COLUMN outcome TEXT", []);
        let _ = conn.execute("ALTER TABLE trade_log ADD COLUMN position_size_pct REAL", []);
        let _ = conn.execute("ALTER TABLE trade_log ADD COLUMN leverage REAL", []);
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub fn insert(
        &self,
        symbol: &str,
        entry_price: f64,
        take_profit: f64,
        stop_loss: f64,
        thesis: Option<&str>,
        position_size_pct: Option<f64>,
        leverage: Option<f64>,
    ) -> Result<i64> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO trade_log
             (created_at, symbol, entry_price, take_profit, stop_loss, thesis, position_size_pct, leverage)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![now, symbol, entry_price, take_profit, stop_loss, thesis, position_size_pct, leverage],
        )
        .context("failed to insert trade record")?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_outcome(&self, id: i64, outcome: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE trade_log SET outcome = ?1 WHERE id = ?2",
            params![outcome, id],
        )
        .context("failed to update trade outcome")?;
        Ok(())
    }

    pub fn summary(&self) -> Result<TradeSummary> {
        let records = self.recent(10_000)?;

        let mut tp_hits = 0usize;
        let mut sl_hits = 0usize;
        let mut expired = 0usize;
        let mut pending = 0usize;
        let mut r_multiples: Vec<f64> = Vec::new();

        for r in &records {
            let risk = (r.entry_price - r.stop_loss).abs();
            let reward = (r.take_profit - r.entry_price).abs();

            match r.outcome.as_deref() {
                Some("TP_HIT") => {
                    tp_hits += 1;
                    if risk > f64::EPSILON {
                        r_multiples.push(reward / risk);
                    }
                }
                Some("SL_HIT") => {
                    sl_hits += 1;
                    r_multiples.push(-1.0);
                }
                Some("EXPIRED") => expired += 1,
                _ => pending += 1,
            }
        }

        let closed = tp_hits + sl_hits;
        let win_rate = if closed > 0 {
            Some(tp_hits as f64 / closed as f64)
        } else {
            None
        };
        let avg_r = if r_multiples.is_empty() {
            None
        } else {
            Some(r_multiples.iter().sum::<f64>() / r_multiples.len() as f64)
        };

        Ok(TradeSummary {
            total: records.len(),
            closed,
            tp_hits,
            sl_hits,
            expired,
            pending,
            win_rate,
            avg_r_multiple: avg_r,
            expectancy: avg_r,
        })
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<TradeRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, created_at, symbol, entry_price, take_profit, stop_loss, thesis, outcome,
                        position_size_pct, leverage
                 FROM trade_log ORDER BY created_at DESC LIMIT ?1",
            )
            .context("failed to prepare trade log query")?;

        let records = stmt
            .query_map(params![limit as i64], |row| {
                Ok(TradeRecord {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    symbol: row.get(2)?,
                    entry_price: row.get(3)?,
                    take_profit: row.get(4)?,
                    stop_loss: row.get(5)?,
                    thesis: row.get(6)?,
                    outcome: row.get(7)?,
                    position_size_pct: row.get(8)?,
                    leverage: row.get(9)?,
                })
            })
            .context("failed to query trade log")?
            .filter_map(|r| r.ok())
            .collect();

        Ok(records)
    }
}
