use rdm_common::{DownloadSegment, RdmError, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};

pub fn parts_dir(destination: &Path) -> PathBuf {
    let mut value = destination.as_os_str().to_os_string();
    value.push(".rdm.parts");
    PathBuf::from(value)
}

pub fn assembling_path(destination: &Path) -> PathBuf {
    let mut value = destination.as_os_str().to_os_string();
    value.push(".rdm.assembling");
    PathBuf::from(value)
}

pub fn segment_path(parts: &Path, index: u32) -> PathBuf {
    parts.join(format!("segment-{index:04}.part"))
}

pub async fn prepare_parts_dir(parts: &Path) -> Result<()> {
    fs::create_dir_all(parts)
        .await
        .map_err(|e| RdmError::from_io_at(parts.to_path_buf(), e))?;
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(parts, std::fs::Permissions::from_mode(0o700)).await;
    Ok(())
}

pub async fn reset_parts(parts: &Path) -> Result<()> {
    match fs::remove_dir_all(parts).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(RdmError::from_io_at(parts.to_path_buf(), err)),
    }
}

/// Check every part file against its planned length and return the total.
pub async fn validate_parts(segments: &[DownloadSegment], parts: &Path) -> Result<u64> {
    let mut total = 0u64;
    for segment in segments {
        let path = segment_path(parts, segment.index);
        let metadata = fs::metadata(&path)
            .await
            .map_err(|e| RdmError::from_io_at(path.clone(), e))?;
        let length = metadata.len();
        if let Some(expected) = segment.expected_len() {
            if length != expected {
                return Err(RdmError::SizeMismatch {
                    expected,
                    actual: length,
                });
            }
        }
        total = total.saturating_add(length);
    }
    Ok(total)
}

pub struct Assembled {
    pub temp_path: PathBuf,
    pub total: u64,
}

/// Concatenate segment files into a temporary file next to the destination.
pub async fn assemble_to_temp(
    destination: &Path,
    parts: &Path,
    segments: &[DownloadSegment],
    expected_total: Option<u64>,
) -> Result<Assembled> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| RdmError::from_io_at(parent.to_path_buf(), e))?;
    }
    let temp_path = assembling_path(destination);
    let mut output = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temp_path)
        .await
        .map_err(|e| RdmError::from_io_at(temp_path.clone(), e))?;

    let mut total = 0u64;
    let mut ordered = segments.to_vec();
    ordered.sort_by_key(|segment| segment.index);
    for segment in &ordered {
        let path = segment_path(parts, segment.index);
        let mut input = File::open(&path)
            .await
            .map_err(|e| RdmError::from_io_at(path.clone(), e))?;
        let copied = tokio::io::copy(&mut input, &mut output)
            .await
            .map_err(|e| RdmError::from_io_at(path.clone(), e))?;
        if let Some(expected) = segment.expected_len() {
            if copied != expected {
                let _ = fs::remove_file(&temp_path).await;
                return Err(RdmError::SizeMismatch {
                    expected,
                    actual: copied,
                });
            }
        }
        total = total.saturating_add(copied);
    }

    output
        .flush()
        .await
        .map_err(|e| RdmError::from_io_at(temp_path.clone(), e))?;
    output
        .sync_all()
        .await
        .map_err(|e| RdmError::from_io_at(temp_path.clone(), e))?;
    drop(output);

    if let Some(expected) = expected_total {
        if total != expected {
            let _ = fs::remove_file(&temp_path).await;
            return Err(RdmError::SizeMismatch {
                expected,
                actual: total,
            });
        }
    }

    Ok(Assembled { temp_path, total })
}

pub async fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .await
        .map_err(|e| RdmError::from_io_at(path.to_path_buf(), e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|e| RdmError::from_io_at(path.to_path_buf(), e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Atomically move the verified temporary file into place.
pub async fn commit(assembled: Assembled, destination: &Path) -> Result<()> {
    if destination.is_dir() {
        return Err(RdmError::InvalidState(format!(
            "destination {} is a directory",
            destination.display()
        )));
    }
    fs::rename(&assembled.temp_path, destination)
        .await
        .map_err(|e| RdmError::from_io_at(destination.to_path_buf(), e))?;
    if let Some(parent) = destination.parent() {
        let parent = parent.to_path_buf();
        tokio::task::spawn_blocking(move || rdm_common::fsutil::sync_dir(&parent))
            .await
            .map_err(|err| RdmError::other(format!("directory sync task failed: {err}")))??;
    }
    Ok(())
}

pub async fn discard(assembled: &Assembled) {
    let _ = fs::remove_file(&assembled.temp_path).await;
}
