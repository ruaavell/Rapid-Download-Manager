use crate::Storage;
use rdm_common::{RdmError, Result, Settings};
use rusqlite::{params, OptionalExtension};

const SETTINGS_KEY: &str = "settings";

impl Storage {
    pub fn load_settings(&self) -> Result<Settings> {
        let raw: Option<String> = self.with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM application_settings WHERE key=?1",
                [SETTINGS_KEY],
                |row| row.get(0),
            )
            .optional()
            .map_err(RdmError::from)
        })?;
        let Some(raw) = raw else {
            return Ok(Settings::default());
        };
        match serde_json::from_str::<Settings>(&raw) {
            Ok(settings) => Ok(settings),
            Err(err) => {
                tracing::error!(error = %err, "settings JSON is invalid; preserving it and using defaults");
                let _ = self.set_setting("settings_invalid_backup", &raw);
                Ok(Settings::default())
            }
        }
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        settings.validate()?;
        let json = serde_json::to_string(settings)
            .map_err(|err| RdmError::other(format!("could not encode settings: {err}")))?;
        self.set_setting(SETTINGS_KEY, &json)
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM application_settings WHERE key=?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(RdmError::from)
        })
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO application_settings (key,value) VALUES (?1,?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )?;
            Ok(())
        })
    }

    pub fn all_settings_raw(&self) -> Result<Vec<(String, String)>> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT key,value FROM application_settings ORDER BY key")?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }
}
