use crate::Storage;
use rdm_common::{time::now_unix, DownloadSegment, Result, SegmentStatus};
use rusqlite::{params, Row};

impl Storage {
    /// Replace the segment plan and return the persisted rows (with ids).
    pub fn replace_segments(
        &self,
        download_id: &str,
        specs: &[(u32, u64, Option<u64>)],
    ) -> Result<Vec<DownloadSegment>> {
        self.with_tx(|tx| {
            tx.execute(
                "DELETE FROM download_segments WHERE download_id=?1",
                [download_id],
            )?;
            let now = now_unix();
            for (index, start, end) in specs {
                tx.execute(
                    "INSERT INTO download_segments
                        (download_id, idx, start_offset, end_offset, downloaded, status, updated_at)
                     VALUES (?1,?2,?3,?4,0,'pending',?5)",
                    params![
                        download_id,
                        *index as i64,
                        *start as i64,
                        end.map(|v| v as i64),
                        now
                    ],
                )?;
            }
            Ok(())
        })?;
        self.list_segments(download_id)
    }

    pub fn list_segments(&self, download_id: &str) -> Result<Vec<DownloadSegment>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id,download_id,idx,start_offset,end_offset,downloaded,status,updated_at
                 FROM download_segments WHERE download_id=?1 ORDER BY idx ASC",
            )?;
            let rows = stmt.query_map([download_id], map_segment)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    pub fn update_segment(
        &self,
        download_id: &str,
        index: u32,
        downloaded: u64,
        status: SegmentStatus,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE download_segments SET downloaded=?3, status=?4, updated_at=?5
                 WHERE download_id=?1 AND idx=?2",
                params![
                    download_id,
                    index as i64,
                    downloaded as i64,
                    status.as_str(),
                    now_unix()
                ],
            )?;
            Ok(())
        })
    }

    /// Persist a batch of per-segment byte counters in one transaction.
    pub fn update_segments_batch(
        &self,
        download_id: &str,
        values: &[(u32, u64, SegmentStatus)],
    ) -> Result<()> {
        self.with_tx(|tx| {
            let now = now_unix();
            for (index, downloaded, status) in values {
                tx.execute(
                    "UPDATE download_segments SET downloaded=?3, status=?4, updated_at=?5
                     WHERE download_id=?1 AND idx=?2",
                    params![
                        download_id,
                        *index as i64,
                        *downloaded as i64,
                        status.as_str(),
                        now
                    ],
                )?;
            }
            Ok(())
        })
    }

    pub fn delete_segments(&self, download_id: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM download_segments WHERE download_id=?1",
                [download_id],
            )?;
            Ok(())
        })
    }

    pub fn sum_segment_downloaded(&self, download_id: &str) -> Result<u64> {
        self.with_conn(|conn| {
            let value: i64 = conn.query_row(
                "SELECT COALESCE(SUM(downloaded),0) FROM download_segments WHERE download_id=?1",
                [download_id],
                |row| row.get(0),
            )?;
            Ok(value.max(0) as u64)
        })
    }
}

fn map_segment(row: &Row<'_>) -> rusqlite::Result<DownloadSegment> {
    let status: String = row.get("status")?;
    Ok(DownloadSegment {
        id: row.get("id")?,
        download_id: row.get("download_id")?,
        index: row.get::<_, i64>("idx")?.max(0) as u32,
        start: row.get::<_, i64>("start_offset")?.max(0) as u64,
        end: row
            .get::<_, Option<i64>>("end_offset")?
            .map(|value| value.max(0) as u64),
        downloaded: row.get::<_, i64>("downloaded")?.max(0) as u64,
        status: SegmentStatus::parse(&status),
        updated_at: row.get("updated_at")?,
    })
}
