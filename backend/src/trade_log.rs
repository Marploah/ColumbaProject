use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize)]
pub struct TradeRecord {
    pub id: i64,
    pub created_at: i64,
    pub symbol: String,
    pub entry_price: f64,
    pub take_profit: f64,
    pub stop_loss: f64,
    pub thesis: Option<String>,
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
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at  INTEGER NOT NULL,
                symbol      TEXT    NOT NULL,
                entry_price REAL    NOT NULL,
                take_profit REAL    NOT NULL,
                stop_loss   REAL    NOT NULL,
                thesis      TEXT
            );",
        )
        .context("failed to create trade_log table")?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub fn insert(
        &self,
        symbol: &str,
        entry_price: f64,
        take_profit: f64,
        stop_loss: f64,
        thesis: Option<&str>,
    ) -> Result<i64> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO trade_log (created_at, symbol, entry_price, take_profit, stop_loss, thesis)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![now, symbol, entry_price, take_profit, stop_loss, thesis],
        )
        .context("failed to insert trade record")?;
        Ok(conn.last_insert_rowid())
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<TradeRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, created_at, symbol, entry_price, take_profit, stop_loss, thesis
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
                })
            })
            .context("failed to query trade log")?
            .filter_map(|r| r.ok())
            .collect();

        Ok(records)
    }
}
