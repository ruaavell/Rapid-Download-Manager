use crate::Storage;
use rdm_common::{RdmError, Result};
use rusqlite::Connection;

/// Each entry is applied exactly once, inside a transaction, and recorded in
/// `schema_migrations`. Never edit a released migration; append a new one.
const MIGRATIONS: &[(&str, &str)] = &[(
    "0001_initial",
    r#"
CREATE TABLE IF NOT EXISTS queues (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL UNIQUE,
    max_concurrent      INTEGER NOT NULL DEFAULT 1,
    default_connections INTEGER NOT NULL DEFAULT 8,
    speed_limit         INTEGER,
    priority            INTEGER NOT NULL DEFAULT 0,
    status              TEXT NOT NULL DEFAULT 'running',
    created_at          INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS downloads (
    id                  TEXT PRIMARY KEY,
    url                 TEXT NOT NULL,
    filename            TEXT NOT NULL,
    destination         TEXT NOT NULL,
    directory           TEXT NOT NULL,
    status              TEXT NOT NULL,
    total_size          INTEGER,
    downloaded          INTEGER NOT NULL DEFAULT 0,
    connections         INTEGER NOT NULL DEFAULT 8,
    speed_limit         INTEGER,
    resumable           INTEGER NOT NULL DEFAULT 0,
    supports_ranges     INTEGER NOT NULL DEFAULT 0,
    mime_type           TEXT,
    etag                TEXT,
    last_modified       TEXT,
    checksum_expected   TEXT,
    checksum_actual     TEXT,
    error               TEXT,
    queue_id            TEXT REFERENCES queues(id) ON DELETE SET NULL,
    priority            INTEGER NOT NULL DEFAULT 0,
    referrer            TEXT,
    filename_explicit   INTEGER NOT NULL DEFAULT 0,
    retry_count         INTEGER NOT NULL DEFAULT 0,
    max_retries         INTEGER NOT NULL DEFAULT 3,
    created_at          INTEGER NOT NULL,
    started_at          INTEGER,
    completed_at        INTEGER
);

CREATE TABLE IF NOT EXISTS download_segments (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    download_id TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
    idx         INTEGER NOT NULL,
    start_offset INTEGER NOT NULL,
    end_offset  INTEGER,
    downloaded  INTEGER NOT NULL DEFAULT 0,
    status      TEXT NOT NULL DEFAULT 'pending',
    updated_at  INTEGER NOT NULL,
    UNIQUE(download_id, idx)
);

CREATE TABLE IF NOT EXISTS download_headers (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    download_id TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    value       TEXT NOT NULL,
    UNIQUE(download_id, name)
);

CREATE TABLE IF NOT EXISTS download_categories (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    extensions  TEXT NOT NULL,
    directory   TEXT
);

CREATE TABLE IF NOT EXISTS download_history (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    download_id     TEXT NOT NULL,
    filename        TEXT NOT NULL,
    url             TEXT NOT NULL,
    destination     TEXT NOT NULL,
    total_size      INTEGER,
    status          TEXT NOT NULL,
    checksum_sha256 TEXT,
    completed_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS application_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS browser_inbox (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    url         TEXT NOT NULL,
    referrer    TEXT,
    filename    TEXT,
    headers     TEXT NOT NULL DEFAULT '[]',
    cookies     TEXT,
    created_at  INTEGER NOT NULL,
    processed   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS scheduled_downloads (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    action      TEXT NOT NULL,
    target_id   TEXT,
    run_at      INTEGER NOT NULL,
    repeat_rule TEXT NOT NULL DEFAULT 'once',
    enabled     INTEGER NOT NULL DEFAULT 1,
    confirmed   INTEGER NOT NULL DEFAULT 0,
    last_run    INTEGER,
    created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_downloads_status ON downloads(status);
CREATE INDEX IF NOT EXISTS idx_downloads_queue ON downloads(queue_id);
CREATE INDEX IF NOT EXISTS idx_downloads_created ON downloads(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_downloads_url ON downloads(url);
CREATE INDEX IF NOT EXISTS idx_segments_download ON download_segments(download_id);
CREATE INDEX IF NOT EXISTS idx_history_completed ON download_history(completed_at DESC);
CREATE INDEX IF NOT EXISTS idx_inbox_processed ON browser_inbox(processed);
CREATE INDEX IF NOT EXISTS idx_schedules_run_at ON scheduled_downloads(enabled, run_at);

INSERT OR IGNORE INTO queues (id, name, max_concurrent, default_connections, speed_limit, priority, status, created_at)
VALUES ('default', 'Default', 2, 8, NULL, 0, 'running', strftime('%s','now'));

INSERT OR IGNORE INTO download_categories (id, name, extensions, directory) VALUES
 ('compressed', 'Compressed', '["zip","7z","rar","tar","gz","bz2","xz","zst","iso"]', NULL),
 ('documents', 'Documents', '["pdf","doc","docx","xls","xlsx","ppt","pptx","txt","epub","odt"]', NULL),
 ('music', 'Music', '["mp3","flac","wav","ogg","m4a","aac","opus"]', NULL),
 ('videos', 'Videos', '["mp4","mkv","webm","avi","mov","m4v","ts"]', NULL),
 ('programs', 'Programs', '["deb","rpm","pkg.tar.zst","appimage","run","sh","bin"]', NULL);
"#,
)];

pub fn migrate(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    TEXT PRIMARY KEY,
             applied_at INTEGER NOT NULL
         );",
    )?;
    let applied: Vec<String> = {
        let mut stmt = conn.prepare("SELECT version FROM schema_migrations")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(std::result::Result::ok).collect()
    };

    for (version, sql) in MIGRATIONS {
        if applied.iter().any(|value| value == version) {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, strftime('%s','now'))",
            [version],
        )?;
        tx.commit()?;
        tracing::info!(version, "applied database migration");
    }

    let version = MIGRATIONS.len() as i64;
    conn.pragma_update(None, "user_version", version)
        .map_err(RdmError::from)?;
    Ok(())
}

pub fn schema_version(storage: &Storage) -> Result<i64> {
    storage.with_conn(|conn| {
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .map_err(RdmError::from)
    })
}
