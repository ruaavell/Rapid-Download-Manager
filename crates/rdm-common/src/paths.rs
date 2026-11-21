use crate::{RdmError, Result};
use std::path::{Path, PathBuf};

/// XDG-compliant locations used by every RDM component.
///
/// `RDM_DATA_DIR` overrides the whole tree, which is used by integration tests
/// and by portable installations. Runtime files (socket/token/lock) always live
/// in a private directory with mode 0700.
#[derive(Debug, Clone)]
pub struct RdmPaths {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub database: PathBuf,
    pub socket: PathBuf,
    pub token: PathBuf,
    pub lock: PathBuf,
    pub pid: PathBuf,
    pub inbox: PathBuf,
    pub log_file: PathBuf,
}

impl RdmPaths {
    pub fn discover() -> Result<Self> {
        let home = dirs::home_dir()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .ok_or_else(|| RdmError::Other("could not resolve the home directory".into()))?;

        if let Some(root) = std::env::var_os("RDM_DATA_DIR") {
            let root = PathBuf::from(root);
            let config = root.join("config");
            let cache = root.join("cache");
            let state = root.join("state");
            let runtime = root.join("runtime");
            return Ok(Self::from_root(root, config, cache, state, runtime));
        }

        let data_dir = env_dir("XDG_DATA_HOME", &home.join(".local/share")).join("rdm");
        let config_dir = env_dir("XDG_CONFIG_HOME", &home.join(".config")).join("rdm");
        let cache_dir = env_dir("XDG_CACHE_HOME", &home.join(".cache")).join("rdm");
        let state_dir = env_dir("XDG_STATE_HOME", &home.join(".local/state")).join("rdm");

        let uid = unsafe { libc::geteuid() };
        let runtime_dir = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("rdm"),
            _ => PathBuf::from(format!("/tmp/rdm-{uid}")),
        };

        let mut socket = runtime_dir.join("rdm.sock");
        // sockaddr_un.sun_path is 108 bytes on Linux including the NUL byte.
        if socket.as_os_str().len() > 100 {
            let fallback = PathBuf::from(format!("/tmp/rdm-{uid}"));
            socket = fallback.join("rdm.sock");
        }

        Ok(Self {
            database: data_dir.join("rdm.sqlite3"),
            inbox: data_dir.join("browser-inbox.jsonl"),
            log_file: state_dir.join("rdm-daemon.log"),
            token: runtime_dir.join("ipc.token"),
            lock: runtime_dir.join("daemon.lock"),
            pid: runtime_dir.join("daemon.pid"),
            data_dir,
            config_dir,
            cache_dir,
            state_dir,
            runtime_dir,
            socket,
        })
    }

    fn from_root(
        data_dir: PathBuf,
        config_dir: PathBuf,
        cache_dir: PathBuf,
        state_dir: PathBuf,
        runtime_dir: PathBuf,
    ) -> Self {
        Self {
            database: data_dir.join("rdm.sqlite3"),
            inbox: data_dir.join("browser-inbox.jsonl"),
            log_file: state_dir.join("rdm-daemon.log"),
            socket: runtime_dir.join("rdm.sock"),
            token: runtime_dir.join("ipc.token"),
            lock: runtime_dir.join("daemon.lock"),
            pid: runtime_dir.join("daemon.pid"),
            data_dir,
            config_dir,
            cache_dir,
            state_dir,
            runtime_dir,
        }
    }

    /// Create all directories and enforce private permissions on runtime state.
    pub fn ensure(&self) -> Result<()> {
        for dir in [
            &self.data_dir,
            &self.config_dir,
            &self.cache_dir,
            &self.state_dir,
            &self.runtime_dir,
        ] {
            std::fs::create_dir_all(dir).map_err(|e| RdmError::from_io_at(dir.clone(), e))?;
        }
        set_mode(&self.runtime_dir, 0o700)?;
        set_mode(&self.data_dir, 0o700)?;
        set_mode(&self.state_dir, 0o700)?;
        Ok(())
    }

    /// Default download directory following `user-dirs.dirs`, then `~/Downloads`.
    pub fn default_download_dir(&self) -> PathBuf {
        if let Some(dir) = xdg_download_dir() {
            return dir;
        }
        dirs::home_dir()
            .map(|home| home.join("Downloads"))
            .unwrap_or_else(|| self.data_dir.join("Downloads"))
    }

    /// True when the path is inside the runtime directory (used by IPC tests).
    pub fn socket_is_private(&self) -> bool {
        self.socket.starts_with(&self.runtime_dir)
    }
}

fn env_dir(key: &str, fallback: &Path) -> PathBuf {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback.to_path_buf())
}

fn xdg_download_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let config = env_dir("XDG_CONFIG_HOME", &home.join(".config")).join("user-dirs.dirs");
    let contents = std::fs::read_to_string(config).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("XDG_DOWNLOAD_DIR=") else {
            continue;
        };
        let value = rest.trim().trim_matches('"');
        if value.is_empty() {
            continue;
        }
        let expanded = value.replace("$HOME", &home.to_string_lossy());
        let path = PathBuf::from(expanded);
        if path.is_absolute() {
            return Some(path);
        }
    }
    None
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| RdmError::from_io_at(path.to_path_buf(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_absolute_and_private() {
        let paths = RdmPaths::discover().expect("paths");
        assert!(paths.database.is_absolute());
        assert!(paths.socket_is_private());
    }

    #[test]
    fn default_download_dir_is_absolute() {
        let paths = RdmPaths::discover().expect("paths");
        assert!(paths.default_download_dir().is_absolute());
    }
}
