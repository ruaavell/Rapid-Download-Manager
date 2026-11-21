use crate::Storage;
use rdm_common::{HeaderEntry, Result};
use rusqlite::params;

impl Storage {
    pub fn get_headers(&self, download_id: &str) -> Result<Vec<HeaderEntry>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT name,value FROM download_headers WHERE download_id=?1 ORDER BY id ASC",
            )?;
            let rows = stmt.query_map([download_id], |row| {
                Ok(HeaderEntry {
                    name: row.get(0)?,
                    value: row.get(1)?,
                })
            })?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    pub fn set_headers(&self, download_id: &str, headers: &[HeaderEntry]) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "DELETE FROM download_headers WHERE download_id=?1",
                [download_id],
            )?;
            for header in headers {
                tx.execute(
                    "INSERT OR REPLACE INTO download_headers (download_id,name,value) VALUES (?1,?2,?3)",
                    params![download_id, header.name, header.value],
                )?;
            }
            Ok(())
        })
    }
}
