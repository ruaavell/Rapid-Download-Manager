use crate::{RdmError, Result};
use std::path::Path;

/// Write a file atomically inside its parent directory.
pub fn atomic_write(path: &Path, contents: &[u8], mode: Option<u32>) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| RdmError::Other("path has no parent".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| RdmError::from_io_at(parent, e))?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("rdm"),
        std::process::id()
    ));
    {
        use std::io::Write;
        let mut file =
            std::fs::File::create(&temp).map_err(|e| RdmError::from_io_at(temp.clone(), e))?;
        file.write_all(contents)
            .map_err(|e| RdmError::from_io_at(temp.clone(), e))?;
        file.sync_all()
            .map_err(|e| RdmError::from_io_at(temp.clone(), e))?;
    }
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(mode))
            .map_err(|e| RdmError::from_io_at(temp.clone(), e))?;
    }
    std::fs::rename(&temp, path).map_err(|e| RdmError::from_io_at(path.to_path_buf(), e))?;
    sync_dir(parent)?;
    Ok(())
}

/// fsync a directory so a rename survives a crash.
pub fn sync_dir(path: &Path) -> Result<()> {
    let dir = std::fs::File::open(path).map_err(|e| RdmError::from_io_at(path.to_path_buf(), e))?;
    dir.sync_all()
        .map_err(|e| RdmError::from_io_at(path.to_path_buf(), e))
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(|e| RdmError::from_io_at(path.to_path_buf(), e))
}

/// Best-effort recursive removal. Missing paths are ignored.
pub fn remove_dir_all(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(RdmError::from_io_at(path.to_path_buf(), err)),
    }
}

/// True when a path is a descendant of a directory after lexical normalization.
pub fn is_within(path: &Path, directory: &Path) -> bool {
    let path = normalize_lexically(path);
    let directory = normalize_lexically(directory);
    path.starts_with(directory)
}

/// Normalize `.` and `..` without touching the filesystem or following symlinks.
pub fn normalize_lexically(path: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut result = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    result.push("..");
                }
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        atomic_write(&path, b"one", Some(0o600)).unwrap();
        atomic_write(&path, b"two", Some(0o600)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
    }

    #[test]
    fn lexical_containment() {
        assert!(is_within(
            Path::new("/home/user/Downloads/a/b.zip"),
            Path::new("/home/user/Downloads")
        ));
        assert!(!is_within(
            Path::new("/home/user/Downloads/../secret"),
            Path::new("/home/user/Downloads")
        ));
    }
}
