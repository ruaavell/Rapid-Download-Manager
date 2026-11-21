//! SQLite persistence for RDM.
//!
//! The connection is guarded by a mutex. All public methods are synchronous;
//! async callers should use `tokio::task::spawn_blocking` for batches. The
//! engine persists progress at most once per second, so the critical sections
//! stay short.

mod categories;
mod downloads;
mod headers;
mod history;
mod inbox;
mod migrations;
mod queues;
mod schedules;
mod segments;
mod settings;

use rdm_common::{RdmError, RdmPaths, Result};
use rusqlite::{Connection, Transaction};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

pub use migrations::schema_version;

/// Result of opening the database, including whether a corrupt file was moved
/// aside. Callers should surface the backup path to the user.
#[derive(Debug, Clone)]
pub struct OpenReport {
    pub backup: Option<PathBuf>,
}

#[derive(Clone)]
pub struct Storage {
    path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage").field("path", &self.path).finish()
    }
}

impl Storage {
    pub fn open_default() -> Result<(Self, OpenReport)> {
        let paths = RdmPaths::discover()?;
        paths.ensure()?;
        Self::open_with_report(paths.database)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_report(path).map(|(storage, _)| storage)
    }

    pub fn open_with_report(path: impl AsRef<Path>) -> Result<(Self, OpenReport)> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| RdmError::from_io_at(parent.to_path_buf(), e))?;
        }
        let (mut conn, backup) = open_connection(&path)?;
        configure(&conn)?;
        migrations::migrate(&mut conn)?;
        set_private_mode(&path)?;
        Ok((
            Self {
                path,
                conn: Arc::new(Mutex::new(conn)),
            },
            OpenReport { backup },
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| RdmError::Database("database lock poisoned".into()))?;
        f(&guard)
    }

    pub(crate) fn with_tx<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut guard = self
            .conn
            .lock()
            .map_err(|_| RdmError::Database("database lock poisoned".into()))?;
        let tx = guard.transaction().map_err(RdmError::from)?;
        let result = f(&tx)?;
        tx.commit().map_err(RdmError::from)?;
        Ok(result)
    }

    /// Flush the WAL during graceful shutdown.
    pub fn checkpoint(&self) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
    }

    pub fn integrity_check(&self) -> Result<bool> {
        self.with_conn(|conn| {
            let value: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            Ok(value == "ok")
        })
    }
}

fn open_connection(path: &Path) -> Result<(Connection, Option<PathBuf>)> {
    let mut backup = None;
    let conn = match Connection::open(path) {
        Ok(conn) => conn,
        Err(err) => {
            tracing::error!(error = %err, path = %path.display(), "database could not be opened; preserving it and creating a new one");
            backup = Some(backup_corrupt(path)?);
            Connection::open(path).map_err(RdmError::from)?
        }
    };
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(RdmError::from)?;
    let integrity: rusqlite::Result<String> =
        conn.query_row("PRAGMA integrity_check", [], |row| row.get(0));
    match integrity {
        Ok(value) if value == "ok" => Ok((conn, backup)),
        Ok(value) => {
            tracing::error!(result = %value, "database integrity check failed");
            drop(conn);
            backup = Some(backup_corrupt(path)?);
            let conn = Connection::open(path).map_err(RdmError::from)?;
            Ok((conn, backup))
        }
        Err(err) => {
            tracing::error!(error = %err, "database integrity check could not run");
            drop(conn);
            backup = Some(backup_corrupt(path)?);
            let conn = Connection::open(path).map_err(RdmError::from)?;
            Ok((conn, backup))
        }
    }
}

fn configure(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;
         PRAGMA busy_timeout=5000;
         PRAGMA temp_store=MEMORY;
         PRAGMA trusted_schema=OFF;
         PRAGMA secure_delete=ON;",
    )
    .map_err(RdmError::from)?;
    Ok(())
}

fn backup_corrupt(path: &Path) -> Result<PathBuf> {
    let stamp = rdm_common::time::now_unix_millis();
    let backup = path.with_file_name(format!(
        "{}.corrupt-{stamp}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("rdm.sqlite3")
    ));
    if path.exists() {
        std::fs::rename(path, &backup).map_err(|e| RdmError::from_io_at(backup.clone(), e))?;
    }
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
        if sidecar.exists() {
            let target = PathBuf::from(format!("{}{suffix}", backup.display()));
            let _ = std::fs::rename(&sidecar, target);
        }
    }
    Ok(backup)
}

fn set_private_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| RdmError::from_io_at(path.to_path_buf(), e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdm_common::{DownloadStatus, DownloadTask};

    fn sample_task(id: &str) -> DownloadTask {
        DownloadTask {
            id: id.into(),
            url: "https://example.com/file.bin".into(),
            filename: "file.bin".into(),
            destination: format!("/tmp/{id}.bin"),
            directory: "/tmp".into(),
            status: DownloadStatus::Queued,
            total_size: Some(100),
            downloaded: 0,
            connections: 8,
            speed_limit: None,
            resumable: true,
            supports_ranges: true,
            mime_type: None,
            etag: None,
            last_modified: None,
            checksum_expected: None,
            checksum_actual: None,
            error: None,
            queue_id: None,
            priority: 0,
            referrer: None,
            filename_explicit: false,
            retry_count: 0,
            max_retries: 3,
            created_at: 1,
            started_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn migrations_and_crud_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("test.sqlite3")).unwrap();
        assert_eq!(schema_version(&storage).unwrap(), 1);
        storage.insert_download(&sample_task("a"), &[]).unwrap();
        let task = storage.get_download("a").unwrap().unwrap();
        assert_eq!(task.filename, "file.bin");
        assert_eq!(storage.list_downloads().unwrap().len(), 1);
        storage
            .update_status("a", DownloadStatus::Completed, None)
            .unwrap();
        assert_eq!(
            storage.get_download("a").unwrap().unwrap().status,
            DownloadStatus::Completed
        );
        storage.delete_download("a").unwrap();
        assert!(storage.get_download("a").unwrap().is_none());
    }

    #[test]
    fn recovers_interrupted_downloads_as_paused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite3");
        let storage = Storage::open(&path).unwrap();
        let mut task = sample_task("running");
        task.status = DownloadStatus::Downloading;
        storage.insert_download(&task, &[]).unwrap();
        let recovered = storage.recover_running().unwrap();
        assert_eq!(recovered, vec!["running".to_string()]);
        assert_eq!(
            storage.get_download("running").unwrap().unwrap().status,
            DownloadStatus::Paused
        );
    }

    #[test]
    fn corrupt_database_is_preserved_not_silently_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.sqlite3");
        std::fs::write(&path, b"this is not a sqlite database").unwrap();
        let (_storage, report) = Storage::open_with_report(&path).unwrap();
        let backup = report.backup.expect("backup path");
        assert!(backup.exists());
        assert_eq!(
            std::fs::read(backup).unwrap(),
            b"this is not a sqlite database"
        );
    }
}
