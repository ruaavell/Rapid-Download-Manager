use crate::{
    http::{backoff_delay, parse_content_range, retry_after, status_error},
    limiter::SpeedLimiter,
};
use futures_util::StreamExt;
use rdm_common::{DownloadSegment, RdmError, Result, SegmentStatus};
use reqwest::{
    header::{HeaderMap, HeaderValue, RANGE},
    Client, StatusCode,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering},
        Arc,
    },
};
use tokio::{
    fs::{File, OpenOptions},
    io::AsyncWriteExt,
};
use tokio_util::sync::CancellationToken;

pub const PROGRESS_PENDING: u8 = 0;
pub const PROGRESS_ACTIVE: u8 = 1;
pub const PROGRESS_COMPLETE: u8 = 2;
pub const PROGRESS_FAILED: u8 = 3;

#[derive(Debug)]
pub struct SegmentProgress {
    pub downloaded: AtomicU64,
    pub status: AtomicU8,
}

impl SegmentProgress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            downloaded: AtomicU64::new(0),
            status: AtomicU8::new(PROGRESS_PENDING),
        })
    }

    pub fn status(&self) -> SegmentStatus {
        match self.status.load(Ordering::Relaxed) {
            PROGRESS_ACTIVE => SegmentStatus::Active,
            PROGRESS_COMPLETE => SegmentStatus::Complete,
            PROGRESS_FAILED => SegmentStatus::Failed,
            _ => SegmentStatus::Pending,
        }
    }
}

#[derive(Clone)]
pub struct SegmentJob {
    pub client: Client,
    pub url: String,
    pub headers: HeaderMap,
    pub segment: DownloadSegment,
    pub part_path: PathBuf,
    pub supports_ranges: bool,
    pub total: Option<u64>,
    pub cancel: CancellationToken,
    pub task_limiter: Arc<SpeedLimiter>,
    pub global_limiter: Arc<SpeedLimiter>,
    pub progress: Arc<SegmentProgress>,
    pub max_retries: u32,
}

/// Download one segment to its own part file, resuming from the existing file
/// length when the server supports ranges.
pub async fn download_segment(job: SegmentJob) -> Result<u64> {
    let expected = job.segment.expected_len();
    let mut file = open_part(&job.part_path).await?;
    let mut offset = file
        .metadata()
        .await
        .map_err(|e| RdmError::from_io_at(job.part_path.clone(), e))?
        .len();

    if let Some(expected) = expected {
        if offset > expected {
            file.set_len(0)
                .await
                .map_err(|e| RdmError::from_io_at(job.part_path.clone(), e))?;
            offset = 0;
        }
        if offset == expected {
            job.progress.downloaded.store(offset, Ordering::Relaxed);
            job.progress
                .status
                .store(PROGRESS_COMPLETE, Ordering::Relaxed);
            return Ok(offset);
        }
    } else if !job.supports_ranges && offset > 0 {
        file.set_len(0)
            .await
            .map_err(|e| RdmError::from_io_at(job.part_path.clone(), e))?;
        offset = 0;
    }

    job.progress.downloaded.store(offset, Ordering::Relaxed);
    job.progress
        .status
        .store(PROGRESS_ACTIVE, Ordering::Relaxed);

    let mut attempt = 0u32;
    let mut last_error: Option<RdmError> = None;

    loop {
        if job.cancel.is_cancelled() {
            flush_file(&mut file, &job.part_path).await?;
            return Err(RdmError::Cancelled);
        }

        let absolute_start = job.segment.start.saturating_add(offset);
        let absolute_end = expected.map(|len| job.segment.start.saturating_add(len - 1));

        let mut request = job.client.get(&job.url).headers(job.headers.clone());
        if job.supports_ranges {
            let range = match absolute_end {
                Some(end) => format!("bytes={absolute_start}-{end}"),
                None => format!("bytes={absolute_start}-"),
            };
            request = request.header(
                RANGE,
                HeaderValue::from_str(&range).unwrap_or(HeaderValue::from_static("bytes=0-")),
            );
        } else if offset > 0 {
            // A server that ignored Range cannot resume. Start this segment over.
            file.set_len(0)
                .await
                .map_err(|e| RdmError::from_io_at(job.part_path.clone(), e))?;
            offset = 0;
            job.progress.downloaded.store(0, Ordering::Relaxed);
        }

        let response = tokio::select! {
            _ = job.cancel.cancelled() => {
                flush_file(&mut file, &job.part_path).await?;
                return Err(RdmError::Cancelled);
            }
            response = request.send() => response,
        };

        let response = match response {
            Ok(response) => response,
            Err(err) => {
                let error = RdmError::network(format!("request failed: {err}"));
                if attempt >= job.max_retries {
                    return break_with(&job, &mut file, error).await;
                }
                last_error = Some(error);
                attempt += 1;
                tokio::time::sleep(backoff_delay(attempt)).await;
                continue;
            }
        };

        let status = response.status();
        match status {
            StatusCode::PARTIAL_CONTENT => {
                let content_range = response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string);
                let Some(content_range) = content_range else {
                    return Err(RdmError::InvalidContentRange(
                        "206 response without Content-Range".into(),
                    ));
                };
                let range = parse_content_range(&content_range)?;
                if range.start != absolute_start {
                    return Err(RdmError::InvalidContentRange(format!(
                        "requested start {absolute_start}, server returned {}",
                        range.start
                    )));
                }
                if let (Some(expected_total), Some(server_total)) = (job.total, range.total) {
                    if expected_total != server_total {
                        return Err(RdmError::SizeMismatch {
                            expected: expected_total,
                            actual: server_total,
                        });
                    }
                }
                let outcome = read_body(&job, &mut file, &mut offset, expected, response).await?;
                match outcome {
                    BodyOutcome::Complete => break,
                    BodyOutcome::Retry(error) => {
                        last_error = Some(error);
                        if attempt >= job.max_retries {
                            break;
                        }
                        attempt += 1;
                        tokio::time::sleep(backoff_delay(attempt)).await;
                    }
                }
            }
            StatusCode::OK => {
                if job.supports_ranges && (job.segment.start > 0 || offset > 0) {
                    return Err(RdmError::RangeUnsupported);
                }
                let outcome = read_body(&job, &mut file, &mut offset, expected, response).await?;
                match outcome {
                    BodyOutcome::Complete => break,
                    BodyOutcome::Retry(error) => {
                        last_error = Some(error);
                        if attempt >= job.max_retries {
                            break;
                        }
                        attempt += 1;
                        tokio::time::sleep(backoff_delay(attempt)).await;
                    }
                }
            }
            StatusCode::RANGE_NOT_SATISFIABLE => {
                if let Some(expected) = expected {
                    if offset == expected {
                        break;
                    }
                }
                if let Some(total) = job.total {
                    if absolute_start >= total {
                        break;
                    }
                }
                let error = status_error(status);
                if attempt >= job.max_retries {
                    return break_with(&job, &mut file, error).await;
                }
                last_error = Some(error);
                attempt += 1;
                tokio::time::sleep(backoff_delay(attempt)).await;
            }
            StatusCode::TOO_MANY_REQUESTS
            | StatusCode::REQUEST_TIMEOUT
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT => {
                let error = status_error(status);
                if attempt >= job.max_retries {
                    return break_with(&job, &mut file, error).await;
                }
                last_error = Some(error);
                attempt += 1;
                let delay =
                    retry_after(response.headers()).unwrap_or_else(|| backoff_delay(attempt));
                tokio::time::sleep(delay).await;
            }
            other if other.as_u16() == 425 => {
                let error = status_error(other);
                if attempt >= job.max_retries {
                    return break_with(&job, &mut file, error).await;
                }
                last_error = Some(error);
                attempt += 1;
                tokio::time::sleep(backoff_delay(attempt)).await;
            }
            other => {
                return Err(status_error(other));
            }
        }
    }

    if let Some(expected) = expected {
        if offset != expected {
            let error = last_error.unwrap_or(RdmError::SizeMismatch {
                expected,
                actual: offset,
            });
            return break_with(&job, &mut file, error).await;
        }
    }

    flush_file(&mut file, &job.part_path).await?;
    job.progress.downloaded.store(offset, Ordering::Relaxed);
    job.progress
        .status
        .store(PROGRESS_COMPLETE, Ordering::Relaxed);
    Ok(offset)
}

enum BodyOutcome {
    Complete,
    Retry(RdmError),
}

async fn read_body(
    job: &SegmentJob,
    file: &mut File,
    offset: &mut u64,
    expected: Option<u64>,
    response: reqwest::Response,
) -> Result<BodyOutcome> {
    let mut stream = response.bytes_stream();
    loop {
        tokio::select! {
            _ = job.cancel.cancelled() => {
                flush_file(file, &job.part_path).await?;
                return Err(RdmError::Cancelled);
            }
            item = stream.next() => {
                match item {
                    Some(Ok(chunk)) => {
                        if let Some(expected) = expected {
                            if *offset + chunk.len() as u64 > expected {
                                return Err(RdmError::SizeMismatch {
                                    expected,
                                    actual: *offset + chunk.len() as u64,
                                });
                            }
                        }
                        tokio::select! {
                            _ = job.cancel.cancelled() => {
                                flush_file(file, &job.part_path).await?;
                                return Err(RdmError::Cancelled);
                            }
                            _ = async {
                                job.global_limiter.acquire(chunk.len() as u64).await;
                                job.task_limiter.acquire(chunk.len() as u64).await;
                            } => {}
                        }
                        file.write_all(&chunk)
                            .await
                            .map_err(|e| RdmError::from_io_at(job.part_path.clone(), e))?;
                        *offset = offset.saturating_add(chunk.len() as u64);
                        job.progress.downloaded.store(*offset, Ordering::Relaxed);
                    }
                    Some(Err(err)) => {
                        flush_file(file, &job.part_path).await?;
                        return Ok(BodyOutcome::Retry(RdmError::network(format!(
                            "body read failed: {err}"
                        ))));
                    }
                    None => {
                        flush_file(file, &job.part_path).await?;
                        if let Some(expected) = expected {
                            if *offset < expected {
                                return Ok(BodyOutcome::Retry(RdmError::SizeMismatch {
                                    expected,
                                    actual: *offset,
                                }));
                            }
                        }
                        return Ok(BodyOutcome::Complete);
                    }
                }
            }
        }
    }
}

async fn break_with(job: &SegmentJob, file: &mut File, error: RdmError) -> Result<u64> {
    let _ = flush_file(file, &job.part_path).await;
    job.progress
        .status
        .store(PROGRESS_FAILED, Ordering::Relaxed);
    Err(error)
}

async fn open_part(path: &std::path::Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .await
        .map_err(|e| RdmError::from_io_at(path.to_path_buf(), e))
}

async fn flush_file(file: &mut File, path: &std::path::Path) -> Result<()> {
    file.flush()
        .await
        .map_err(|e| RdmError::from_io_at(path.to_path_buf(), e))?;
    file.sync_all()
        .await
        .map_err(|e| RdmError::from_io_at(path.to_path_buf(), e))
}
