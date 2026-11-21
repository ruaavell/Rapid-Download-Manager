use crate::Storage;
use rdm_common::{time::now_unix, DownloadStatus, DownloadTask, HeaderEntry, Result};
use rusqlite::{named_params, params, params_from_iter, Row};

const SELECT: &str = "SELECT id,url,filename,destination,directory,status,total_size,downloaded,
    connections,speed_limit,resumable,supports_ranges,mime_type,etag,last_modified,
    checksum_expected,checksum_actual,error,queue_id,priority,referrer,filename_explicit,
    retry_count,max_retries,created_at,started_at,completed_at FROM downloads";

impl Storage {
    pub fn insert_download(&self, task: &DownloadTask, headers: &[HeaderEntry]) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "INSERT INTO downloads (
                    id,url,filename,destination,directory,status,total_size,downloaded,
                    connections,speed_limit,resumable,supports_ranges,mime_type,etag,last_modified,
                    checksum_expected,checksum_actual,error,queue_id,priority,referrer,
                    filename_explicit,retry_count,max_retries,created_at,started_at,completed_at
                 ) VALUES (
                    :id,:url,:filename,:destination,:directory,:status,:total_size,:downloaded,
                    :connections,:speed_limit,:resumable,:supports_ranges,:mime_type,:etag,:last_modified,
                    :checksum_expected,:checksum_actual,:error,:queue_id,:priority,:referrer,
                    :filename_explicit,:retry_count,:max_retries,:created_at,:started_at,:completed_at
                 )",
                named_params! {
                    ":id": task.id,
                    ":url": task.url,
                    ":filename": task.filename,
                    ":destination": task.destination,
                    ":directory": task.directory,
                    ":status": task.status.as_str(),
                    ":total_size": task.total_size.map(|v| v as i64),
                    ":downloaded": task.downloaded as i64,
                    ":connections": task.connections as i64,
                    ":speed_limit": task.speed_limit.map(|v| v as i64),
                    ":resumable": task.resumable as i64,
                    ":supports_ranges": task.supports_ranges as i64,
                    ":mime_type": task.mime_type,
                    ":etag": task.etag,
                    ":last_modified": task.last_modified,
                    ":checksum_expected": task.checksum_expected,
                    ":checksum_actual": task.checksum_actual,
                    ":error": task.error,
                    ":queue_id": task.queue_id,
                    ":priority": task.priority,
                    ":referrer": task.referrer,
                    ":filename_explicit": task.filename_explicit as i64,
                    ":retry_count": task.retry_count as i64,
                    ":max_retries": task.max_retries as i64,
                    ":created_at": task.created_at,
                    ":started_at": task.started_at,
                    ":completed_at": task.completed_at,
                },
            )?;
            for header in headers {
                tx.execute(
                    "INSERT OR REPLACE INTO download_headers (download_id,name,value) VALUES (?1,?2,?3)",
                    params![task.id, header.name, header.value],
                )?;
            }
            Ok(())
        })
    }

    pub fn update_download(&self, task: &DownloadTask) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET
                    url=:url, filename=:filename, destination=:destination, directory=:directory,
                    status=:status, total_size=:total_size, downloaded=:downloaded,
                    connections=:connections, speed_limit=:speed_limit, resumable=:resumable,
                    supports_ranges=:supports_ranges, mime_type=:mime_type, etag=:etag,
                    last_modified=:last_modified, checksum_expected=:checksum_expected,
                    checksum_actual=:checksum_actual, error=:error, queue_id=:queue_id,
                    priority=:priority, referrer=:referrer, filename_explicit=:filename_explicit,
                    retry_count=:retry_count, max_retries=:max_retries, started_at=:started_at,
                    completed_at=:completed_at
                 WHERE id=:id",
                named_params! {
                    ":id": task.id,
                    ":url": task.url,
                    ":filename": task.filename,
                    ":destination": task.destination,
                    ":directory": task.directory,
                    ":status": task.status.as_str(),
                    ":total_size": task.total_size.map(|v| v as i64),
                    ":downloaded": task.downloaded as i64,
                    ":connections": task.connections as i64,
                    ":speed_limit": task.speed_limit.map(|v| v as i64),
                    ":resumable": task.resumable as i64,
                    ":supports_ranges": task.supports_ranges as i64,
                    ":mime_type": task.mime_type,
                    ":etag": task.etag,
                    ":last_modified": task.last_modified,
                    ":checksum_expected": task.checksum_expected,
                    ":checksum_actual": task.checksum_actual,
                    ":error": task.error,
                    ":queue_id": task.queue_id,
                    ":priority": task.priority,
                    ":referrer": task.referrer,
                    ":filename_explicit": task.filename_explicit as i64,
                    ":retry_count": task.retry_count as i64,
                    ":max_retries": task.max_retries as i64,
                    ":started_at": task.started_at,
                    ":completed_at": task.completed_at,
                },
            )?;
            Ok(())
        })
    }

    pub fn get_download(&self, id: &str) -> Result<Option<DownloadTask>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!("{SELECT} WHERE id=?1"))?;
            let mut rows = stmt.query_map([id], map_download)?;
            match rows.next() {
                Some(row) => Ok(Some(row?)),
                None => Ok(None),
            }
        })
    }

    pub fn list_downloads(&self) -> Result<Vec<DownloadTask>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!("{SELECT} ORDER BY created_at DESC"))?;
            let rows = stmt.query_map([], map_download)?;
            collect(rows)
        })
    }

    pub fn list_downloads_by_status(
        &self,
        statuses: &[DownloadStatus],
    ) -> Result<Vec<DownloadTask>> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (1..=statuses.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let values: Vec<&str> = statuses.iter().map(|status| status.as_str()).collect();
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "{SELECT} WHERE status IN ({placeholders}) ORDER BY priority DESC, created_at ASC"
            ))?;
            let rows = stmt.query_map(params_from_iter(values), map_download)?;
            collect(rows)
        })
    }

    pub fn delete_download(&self, id: &str) -> Result<bool> {
        self.with_conn(|conn| {
            let changed = conn.execute("DELETE FROM downloads WHERE id=?1", [id])?;
            Ok(changed > 0)
        })
    }

    pub fn update_progress(&self, id: &str, downloaded: u64, total: Option<u64>) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET downloaded=?2, total_size=?3 WHERE id=?1",
                params![id, downloaded as i64, total.map(|v| v as i64)],
            )?;
            Ok(())
        })
    }

    pub fn update_status(
        &self,
        id: &str,
        status: DownloadStatus,
        error: Option<&str>,
    ) -> Result<()> {
        let now = now_unix();
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET status=?2, error=?3,
                    started_at=CASE WHEN ?2 IN ('connecting','downloading') AND started_at IS NULL THEN ?4 ELSE started_at END,
                    completed_at=CASE WHEN ?2='completed' THEN ?4 ELSE completed_at END
                 WHERE id=?1",
                params![id, status.as_str(), error, now],
            )?;
            Ok(())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_metadata(
        &self,
        id: &str,
        total: Option<u64>,
        supports_ranges: bool,
        etag: Option<&str>,
        last_modified: Option<&str>,
        mime_type: Option<&str>,
        filename: Option<&str>,
        destination: Option<&str>,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET total_size=?2, supports_ranges=?3, etag=?4,
                    last_modified=?5, mime_type=?6, filename=COALESCE(?7, filename),
                    destination=COALESCE(?8, destination)
                 WHERE id=?1",
                params![
                    id,
                    total.map(|v| v as i64),
                    supports_ranges as i64,
                    etag,
                    last_modified,
                    mime_type,
                    filename,
                    destination
                ],
            )?;
            Ok(())
        })
    }

    pub fn set_speed_limit(&self, id: &str, limit: Option<u64>) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET speed_limit=?2 WHERE id=?1",
                params![id, limit.map(|v| v as i64)],
            )?;
            Ok(())
        })
    }

    pub fn set_connections(&self, id: &str, connections: u32) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET connections=?2 WHERE id=?1",
                params![id, connections as i64],
            )?;
            Ok(())
        })
    }

    pub fn set_queue(&self, id: &str, queue_id: Option<&str>) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET queue_id=?2 WHERE id=?1",
                params![id, queue_id],
            )?;
            Ok(())
        })
    }

    pub fn set_priority(&self, id: &str, priority: i32) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET priority=?2 WHERE id=?1",
                params![id, priority],
            )?;
            Ok(())
        })
    }

    pub fn increment_retry(&self, id: &str) -> Result<u32> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET retry_count=retry_count+1 WHERE id=?1",
                [id],
            )?;
            let value: i64 = conn.query_row(
                "SELECT retry_count FROM downloads WHERE id=?1",
                [id],
                |row| row.get(0),
            )?;
            Ok(value.max(0) as u32)
        })
    }

    pub fn reset_retry(&self, id: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("UPDATE downloads SET retry_count=0 WHERE id=?1", [id])?;
            Ok(())
        })
    }

    /// Mark every in-flight task as paused. Returns the ids that were running.
    pub fn recover_running(&self) -> Result<Vec<String>> {
        self.with_tx(|tx| {
            let ids: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM downloads WHERE status IN ('connecting','downloading','verifying')",
                )?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                collect(rows)?
            };
            tx.execute(
                "UPDATE downloads SET status='paused'
                 WHERE status IN ('connecting','downloading','verifying')",
                [],
            )?;
            Ok(ids)
        })
    }

    pub fn count_status(&self, status: DownloadStatus) -> Result<usize> {
        self.with_conn(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM downloads WHERE status=?1",
                [status.as_str()],
                |row| row.get(0),
            )?;
            Ok(count.max(0) as usize)
        })
    }

    pub fn count_all(&self) -> Result<usize> {
        self.with_conn(|conn| {
            let count: i64 =
                conn.query_row("SELECT COUNT(*) FROM downloads", [], |row| row.get(0))?;
            Ok(count.max(0) as usize)
        })
    }

    pub fn find_active_by_url(&self, url: &str) -> Result<Option<DownloadTask>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "{SELECT} WHERE url=?1 AND status IN ('queued','connecting','downloading','paused','verifying') LIMIT 1"
            ))?;
            let mut rows = stmt.query_map([url], map_download)?;
            match rows.next() {
                Some(row) => Ok(Some(row?)),
                None => Ok(None),
            }
        })
    }

    pub fn find_active_by_destination(&self, destination: &str) -> Result<Option<DownloadTask>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "{SELECT} WHERE destination=?1 AND status IN ('queued','connecting','downloading','paused','verifying') LIMIT 1"
            ))?;
            let mut rows = stmt.query_map([destination], map_download)?;
            match rows.next() {
                Some(row) => Ok(Some(row?)),
                None => Ok(None),
            }
        })
    }

    pub fn total_completed_bytes(&self) -> Result<u64> {
        self.with_conn(|conn| {
            let value: i64 = conn.query_row(
                "SELECT COALESCE(SUM(total_size),0) FROM downloads WHERE status='completed'",
                [],
                |row| row.get(0),
            )?;
            Ok(value.max(0) as u64)
        })
    }

    pub fn list_all_for_export(&self) -> Result<Vec<DownloadTask>> {
        self.list_downloads()
    }
}

fn collect<T>(rows: impl Iterator<Item = rusqlite::Result<T>>) -> Result<Vec<T>> {
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

fn map_download(row: &Row<'_>) -> rusqlite::Result<DownloadTask> {
    let status: String = row.get("status")?;
    Ok(DownloadTask {
        id: row.get("id")?,
        url: row.get("url")?,
        filename: row.get("filename")?,
        destination: row.get("destination")?,
        directory: row.get("directory")?,
        status: DownloadStatus::parse(&status),
        total_size: opt_u64(row.get("total_size")?),
        downloaded: u64_value(row.get("downloaded")?),
        connections: u32_value(row.get("connections")?),
        speed_limit: opt_u64(row.get("speed_limit")?),
        resumable: row.get::<_, i64>("resumable")? != 0,
        supports_ranges: row.get::<_, i64>("supports_ranges")? != 0,
        mime_type: row.get("mime_type")?,
        etag: row.get("etag")?,
        last_modified: row.get("last_modified")?,
        checksum_expected: row.get("checksum_expected")?,
        checksum_actual: row.get("checksum_actual")?,
        error: row.get("error")?,
        queue_id: row.get("queue_id")?,
        priority: row.get("priority")?,
        referrer: row.get("referrer")?,
        filename_explicit: row.get::<_, i64>("filename_explicit")? != 0,
        retry_count: u32_value(row.get("retry_count")?),
        max_retries: u32_value(row.get("max_retries")?),
        created_at: row.get("created_at")?,
        started_at: row.get("started_at")?,
        completed_at: row.get("completed_at")?,
    })
}

fn u64_value(value: i64) -> u64 {
    value.max(0) as u64
}

fn opt_u64(value: Option<i64>) -> Option<u64> {
    value.map(u64_value)
}

fn u32_value(value: i64) -> u32 {
    value.clamp(0, u32::MAX as i64) as u32
}
