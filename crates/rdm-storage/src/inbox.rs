use crate::Storage;
use rdm_common::{time::now_unix, BrowserInboxEntry, HeaderEntry, RdmError, Result};
use rusqlite::{params, Row};

impl Storage {
    pub fn add_browser_entry(
        &self,
        url: &str,
        referrer: Option<&str>,
        filename: Option<&str>,
        headers: &[HeaderEntry],
        cookies: Option<&str>,
    ) -> Result<i64> {
        let headers = serde_json::to_string(headers)
            .map_err(|err| RdmError::other(format!("invalid browser headers: {err}")))?;
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO browser_inbox (url,referrer,filename,headers,cookies,created_at,processed)
                 VALUES (?1,?2,?3,?4,?5,?6,0)",
                params![url, referrer, filename, headers, cookies, now_unix()],
            )?;
            Ok(conn.last_insert_rowid())
        })
    }

    pub fn list_pending_browser_entries(&self, limit: usize) -> Result<Vec<BrowserInboxEntry>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id,url,referrer,filename,headers,cookies,created_at,processed
                 FROM browser_inbox WHERE processed=0 ORDER BY id ASC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit as i64], map_inbox)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    pub fn mark_browser_entry_processed(&self, id: i64) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("UPDATE browser_inbox SET processed=1 WHERE id=?1", [id])?;
            Ok(())
        })
    }

    pub fn delete_processed_browser_entries(&self) -> Result<usize> {
        self.with_conn(|conn| {
            let changed = conn.execute("DELETE FROM browser_inbox WHERE processed=1", [])?;
            Ok(changed)
        })
    }

    pub fn count_pending_browser_entries(&self) -> Result<usize> {
        self.with_conn(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM browser_inbox WHERE processed=0",
                [],
                |row| row.get(0),
            )?;
            Ok(count.max(0) as usize)
        })
    }
}

fn map_inbox(row: &Row<'_>) -> rusqlite::Result<BrowserInboxEntry> {
    let headers: String = row.get("headers")?;
    Ok(BrowserInboxEntry {
        id: row.get("id")?,
        url: row.get("url")?,
        referrer: row.get("referrer")?,
        filename: row.get("filename")?,
        headers: serde_json::from_str(&headers).unwrap_or_default(),
        cookies: row.get("cookies")?,
        created_at: row.get("created_at")?,
        processed: row.get::<_, i64>("processed")? != 0,
    })
}
