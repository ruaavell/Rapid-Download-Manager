use super::{DownloadStatus, DownloadTask};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct TaskStore {
    conn: Arc<Mutex<Connection>>,
}

impl TaskStore {
    pub fn open_default() -> Result<Self> {
        let base = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("rdm");
        std::fs::create_dir_all(&base)?;
        Self::open(base.join("rdm-desktop.sqlite3"))
    }

    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).context("Could not open RDM database")?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS downloads (
               id TEXT PRIMARY KEY,
               url TEXT NOT NULL,
               filename TEXT NOT NULL,
               destination TEXT NOT NULL,
               downloaded INTEGER NOT NULL DEFAULT 0,
               total INTEGER,
               status TEXT NOT NULL,
               speed_limit INTEGER,
               error TEXT,
               created_at INTEGER NOT NULL
             );",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn upsert(&self, task: &DownloadTask) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("Database lock poisoned"))?;
        conn.execute(
            "INSERT INTO downloads
             (id,url,filename,destination,downloaded,total,status,speed_limit,error,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(id) DO UPDATE SET
               url=excluded.url,filename=excluded.filename,destination=excluded.destination,
               downloaded=excluded.downloaded,total=excluded.total,status=excluded.status,
               speed_limit=excluded.speed_limit,error=excluded.error",
            params![
                task.id,
                task.url,
                task.filename,
                task.destination,
                task.downloaded as i64,
                task.total.map(|v| v as i64),
                status_string(&task.status),
                task.speed_limit.map(|v| v as i64),
                task.error,
                task.created_at
            ],
        )?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<DownloadTask>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("Database lock poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT id,url,filename,destination,downloaded,total,status,speed_limit,error,created_at
             FROM downloads ORDER BY created_at DESC"
        )?;
        let rows = stmt.query_map([], |row| {
            let status: String = row.get(6)?;
            Ok(DownloadTask {
                id: row.get(0)?,
                url: row.get(1)?,
                filename: row.get(2)?,
                destination: row.get(3)?,
                downloaded: row.get::<_, i64>(4)? as u64,
                total: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                status: parse_status(&status),
                speed_limit: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
                error: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("Database lock poisoned"))?;
        conn.execute("DELETE FROM downloads WHERE id=?1", [id])?;
        Ok(())
    }
}

fn status_string(s: &DownloadStatus) -> &'static str {
    match s {
        DownloadStatus::Queued => "queued",
        DownloadStatus::Downloading => "downloading",
        DownloadStatus::Paused => "paused",
        DownloadStatus::Completed => "completed",
        DownloadStatus::Failed => "failed",
        DownloadStatus::Cancelled => "cancelled",
        DownloadStatus::Merging => "merging",
    }
}

fn parse_status(s: &str) -> DownloadStatus {
    match s {
        "downloading" => DownloadStatus::Downloading,
        "paused" => DownloadStatus::Paused,
        "completed" => DownloadStatus::Completed,
        "failed" => DownloadStatus::Failed,
        "cancelled" => DownloadStatus::Cancelled,
        "merging" => DownloadStatus::Merging,
        _ => DownloadStatus::Queued,
    }
}
