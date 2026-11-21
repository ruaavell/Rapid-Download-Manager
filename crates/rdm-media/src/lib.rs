use rdm_common::{MediaInfo, MediaJobStatus, MediaKind, MediaStream, RdmError};
use rdm_engine::DownloadEngine;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

static JOBS: OnceLock<Mutex<HashMap<String, MediaJobState>>> = OnceLock::new();

#[derive(Clone)]
struct MediaJobState {
    id: String,
    url: String,
    phase: String,
    downloaded_segments: u64,
    total_segments: Option<u64>,
    downloaded_bytes: u64,
    percent: f64,
    message: String,
    output_path: Option<String>,
    error: Option<String>,
    done: bool,
}

fn jobs() -> &'static Mutex<HashMap<String, MediaJobState>> {
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn probe(url: &str) -> rdm_common::Result<MediaInfo> {
    let client = reqwest::Client::builder()
        .user_agent("RDM/1.0")
        .build()
        .map_err(|e| RdmError::network(e.to_string()))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| RdmError::network(e.to_string()))?;
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = response.text().await.unwrap_or_default();

    let lower_url = url.to_lowercase();
    let is_hls = lower_url.contains(".m3u8") || content_type.contains("mpegurl");
    let is_dash = lower_url.contains(".mpd") || content_type.contains("dash+xml");

    if is_hls {
        let mut segments = Vec::new();
        let mut index = 0u32;
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            segments.push(MediaStream {
                id: index.to_string(),
                kind: "segment".into(),
                codec: None,
                language: None,
                bandwidth: None,
                width: None,
                height: None,
                url: resolve_url(url, line),
            });
            index += 1;
        }
        return Ok(MediaInfo {
            kind: MediaKind::Hls,
            url: final_url,
            title: None,
            duration_seconds: None,
            streams: segments,
            subtitles: vec![],
            drm_protected: body.contains("EXT-X-KEY"),
            notes: vec![
                "HLS playlist parsed; segments are listed but full capture requires FFmpeg".into(),
            ],
        });
    }

    if is_dash {
        return Ok(MediaInfo {
            kind: MediaKind::Dash,
            url: final_url,
            title: None,
            duration_seconds: None,
            streams: vec![],
            subtitles: vec![],
            drm_protected: body.contains("ContentProtection"),
            notes: vec![
                "DASH manifest detected; full capture requires FFmpeg and is not yet implemented"
                    .into(),
            ],
        });
    }

    Ok(MediaInfo {
        kind: MediaKind::Direct,
        url: final_url.clone(),
        title: None,
        duration_seconds: None,
        streams: vec![MediaStream {
            id: "0".into(),
            kind: "direct".into(),
            codec: None,
            language: None,
            bandwidth: None,
            width: None,
            height: None,
            url: final_url.clone(),
        }],
        subtitles: vec![],
        drm_protected: false,
        notes: vec![format!("content-type: {content_type}")],
    })
}

fn resolve_url(base: &str, relative: &str) -> String {
    if relative.starts_with("http://") || relative.starts_with("https://") {
        return relative.to_string();
    }
    if let Ok(base) = url::Url::parse(base) {
        if let Ok(joined) = base.join(relative) {
            return joined.to_string();
        }
    }
    relative.to_string()
}

pub async fn start(
    engine: &DownloadEngine,
    request: rdm_common::MediaDownloadRequest,
) -> rdm_common::Result<MediaJobStatus> {
    let id = uuid::Uuid::new_v4().to_string();
    let state = MediaJobState {
        id: id.clone(),
        url: request.url.clone(),
        phase: "starting".into(),
        downloaded_segments: 0,
        total_segments: None,
        downloaded_bytes: 0,
        percent: 0.0,
        message: "queued".into(),
        output_path: None,
        error: None,
        done: false,
    };
    jobs().lock().unwrap().insert(id.clone(), state);

    // Direct download via engine.
    let engine = engine.clone();
    let job_id = id.clone();
    let destination = request.destination.clone();
    let filename = request.url.rsplit('/').next().map(str::to_string);
    tokio::spawn(async move {
        let task_result = engine
            .add(rdm_common::NewDownloadRequest {
                url: request.url.clone(),
                destination: destination.clone(),
                filename,
                connections: request.connections,
                speed_limit: None,
                checksum_sha256: None,
                referrer: None,
                headers: vec![],
                cookies: None,
                queue_id: None,
                priority: None,
                auto_start: Some(true),
            })
            .await;
        match task_result {
            Ok(task) => {
                let _ = engine.start(&task.id).await;
                // Poll until terminal.
                loop {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    let list = engine.list().await.unwrap_or_default();
                    if let Some(snap) = list.iter().find(|s| s.task.id == task.id) {
                        let mut guard = jobs().lock().unwrap();
                        if let Some(job) = guard.get_mut(&job_id) {
                            job.downloaded_bytes = snap.task.downloaded;
                            job.percent = snap.task.progress_fraction().unwrap_or(0.0) * 100.0;
                            job.phase = format!("{:?}", snap.task.status).to_lowercase();
                            job.message = snap.task.status.as_str().to_string();
                            if snap.task.status == rdm_common::DownloadStatus::Completed {
                                job.done = true;
                                job.output_path = Some(snap.task.destination.clone());
                                job.phase = "completed".into();
                                break;
                            }
                            if snap.task.status == rdm_common::DownloadStatus::Failed {
                                job.done = true;
                                job.error = snap.task.error.clone();
                                job.phase = "failed".into();
                                break;
                            }
                            if snap.task.status == rdm_common::DownloadStatus::Cancelled {
                                job.done = true;
                                job.phase = "cancelled".into();
                                break;
                            }
                        }
                    } else {
                        break;
                    }
                }
            }
            Err(err) => {
                let mut guard = jobs().lock().unwrap();
                if let Some(job) = guard.get_mut(&job_id) {
                    job.done = true;
                    job.error = Some(err.to_string());
                    job.phase = "failed".into();
                }
            }
        }
    });

    let job = jobs()
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .map(state_to_status)
        .unwrap();
    Ok(job)
}

fn state_to_status(s: MediaJobState) -> MediaJobStatus {
    MediaJobStatus {
        id: s.id,
        url: s.url,
        phase: s.phase,
        downloaded_segments: s.downloaded_segments,
        total_segments: s.total_segments,
        downloaded_bytes: s.downloaded_bytes,
        percent: s.percent,
        message: s.message,
        output_path: s.output_path,
        error: s.error,
        done: s.done,
    }
}

pub fn status(job_id: &str) -> Option<MediaJobStatus> {
    jobs()
        .lock()
        .unwrap()
        .get(job_id)
        .cloned()
        .map(state_to_status)
}

pub fn list() -> Vec<MediaJobStatus> {
    jobs()
        .lock()
        .unwrap()
        .values()
        .cloned()
        .map(state_to_status)
        .collect()
}

pub fn cancel(job_id: &str) {
    // Best-effort: mark job cancelled. Underlying engine task cancellation would require id.
    let mut guard = jobs().lock().unwrap();
    if let Some(job) = guard.get_mut(job_id) {
        job.done = true;
        job.phase = "cancelled".into();
        job.message = "cancelled".into();
    }
}
