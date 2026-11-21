use crate::Storage;
use rdm_common::{time::now_unix, DownloadTask, HistoryEntry, Result};
use rusqlite::{params, Row};

impl Storage {
    pub fn add_history(&self, task: &DownloadTask) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO download_history
                    (download_id,filename,url,destination,total_size,status,checksum_sha256,completed_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    task.id,
                    task.filename,
                    task.url,
                    task.destination,
                    task.total_size.map(|v| v as i64),
                    task.status.as_str(),
                    task.checksum_actual,
                    task.completed_at.unwrap_or_else(now_unix)
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_history(&self, limit: usize, offset: usize) -> Result<Vec<HistoryEntry>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id,download_id,filename,url,destination,total_size,status,checksum_sha256,completed_at
                 FROM download_history ORDER BY completed_at DESC LIMIT ?1 OFFSET ?2",
            )?;
            let rows = stmt.query_map(params![limit as i64, offset as i64], map_history)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    pub fn clear_history(&self) -> Result<usize> {
        self.with_conn(|conn| {
            let changed = conn.execute("DELETE FROM download_history", [])?;
            Ok(changed)
        })
    }

    pub fn delete_history(&self, id: i64) -> Result<bool> {
        self.with_conn(|conn| {
            let changed = conn.execute("DELETE FROM download_history WHERE id=?1", [id])?;
            Ok(changed > 0)
        })
    }
}

fn map_history(row: &Row<'_>) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: row.get("id")?,
        download_id: row.get("download_id")?,
        filename: row.get("filename")?,
        url: row.get("url")?,
        destination: row.get("destination")?,
        total_size: row
            .get::<_, Option<i64>>("total_size")?
            .map(|value| value.max(0) as u64),
        status: row.get("status")?,
        checksum_sha256: row.get("checksum_sha256")?,
        completed_at: row.get("completed_at")?,
    })
}
