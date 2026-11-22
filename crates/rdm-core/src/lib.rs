pub mod net;
mod store;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use reqwest::{header, header::HeaderMap, header::HeaderValue, Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs::{self, File, OpenOptions},
    io::AsyncWriteExt,
    sync::{watch, Mutex, RwLock},
};
use uuid::Uuid;

pub use store::TaskStore;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Paused,
    Completed,
    Failed,
    Cancelled,
    Merging,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTask {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub destination: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub status: DownloadStatus,
    pub speed_limit: Option<u64>,
    pub error: Option<String>,
    pub created_at: i64,
}

/// Live progress of one byte range of a segmented download.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentProgress {
    pub index: usize,
    pub start: u64,
    pub end: u64,
    pub downloaded: u64,
    pub total: u64,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewDownload {
    pub url: String,
    pub destination: String,
    pub filename: Option<String>,
    pub speed_limit: Option<u64>,
    /// Page the link was taken from; sent as the `Referer` header.
    #[serde(default)]
    pub referrer: Option<String>,
    /// `Cookie` header value from the browser. Kept in memory only.
    #[serde(default)]
    pub cookies: Option<String>,
}

/// Shared token bucket so every connection of a task respects one limit.
struct RateGate {
    limit: std::sync::atomic::AtomicU64,
    state: std::sync::Mutex<(f64, std::time::Instant)>,
}

impl RateGate {
    fn new(limit: u64) -> Self {
        Self {
            limit: std::sync::atomic::AtomicU64::new(limit),
            state: std::sync::Mutex::new((0.0, std::time::Instant::now())),
        }
    }

    fn set_limit(&self, limit: u64) {
        self.limit
            .store(limit, std::sync::atomic::Ordering::Relaxed);
    }

    async fn acquire(&self, bytes: usize) {
        loop {
            let limit = self.limit.load(std::sync::atomic::Ordering::Relaxed);
            if limit == 0 {
                return;
            }
            let wait = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let now = std::time::Instant::now();
                let elapsed = now.duration_since(state.1).as_secs_f64();
                state.1 = now;
                // Allow a short burst so small chunks still flow smoothly.
                let capacity = limit as f64 * 0.25;
                state.0 = (state.0 + elapsed * limit as f64).min(capacity);
                let needed = bytes as f64;
                if state.0 >= needed {
                    state.0 -= needed;
                    0.0
                } else {
                    let missing = needed - state.0;
                    state.0 = 0.0;
                    missing / limit as f64
                }
            };
            if wait <= 0.0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait.min(0.5))).await;
        }
    }
}

/// Per-task request data that must never be written to disk.
#[derive(Debug, Clone, Default)]
struct RequestExtras {
    referrer: Option<String>,
    cookies: Option<String>,
    filename_explicit: bool,
}

/// What a one-byte range request told us about the remote file.
#[derive(Debug, Clone, Default)]
struct Probe {
    total: Option<u64>,
    accept_ranges: bool,
    filename: Option<String>,
    content_type: Option<String>,
}

impl Probe {
    fn is_html(&self) -> bool {
        self.content_type
            .as_deref()
            .map(net::is_html_content_type)
            .unwrap_or(false)
    }
}

/// Remote file details shown when the browser hands a link to RDM.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProbeInfo {
    pub size: Option<u64>,
    pub content_type: Option<String>,
    pub filename: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineEvent {
    pub task: DownloadTask,
    pub speed_bps: u64,
}

#[derive(Clone)]
pub struct DownloadManager {
    client: Client,
    store: TaskStore,
    tasks: Arc<RwLock<HashMap<String, DownloadTask>>>,
    controls: Arc<Mutex<HashMap<String, watch::Sender<Control>>>>,
    extras: Arc<RwLock<HashMap<String, RequestExtras>>>,
    gates: Arc<Mutex<HashMap<String, Arc<RateGate>>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Run,
    Pause,
    Cancel,
}

impl DownloadManager {
    pub fn new(store: TaskStore) -> Result<Self> {
        let client = Client::builder()
            .user_agent(concat!("RDM/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::limited(10))
            .connect_timeout(std::time::Duration::from_secs(20))
            .build()?;
        let recovered = store.list()?;
        let mut tasks = HashMap::new();
        for mut task in recovered {
            if task.status == DownloadStatus::Downloading {
                task.status = DownloadStatus::Paused;
                store.upsert(&task)?;
            }
            tasks.insert(task.id.clone(), task);
        }
        Ok(Self {
            client,
            store,
            tasks: Arc::new(RwLock::new(tasks)),
            controls: Arc::new(Mutex::new(HashMap::new())),
            extras: Arc::new(RwLock::new(HashMap::new())),
            gates: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn list(&self) -> Vec<DownloadTask> {
        let mut items: Vec<_> = self.tasks.read().await.values().cloned().collect();
        items.sort_by_key(|t| -t.created_at);
        items
    }

    /// Change the limit of a running task immediately. `None` or zero removes it.
    pub async fn set_speed_limit(&self, id: &str, limit: Option<u64>) -> Result<()> {
        let limit = limit.filter(|value| *value > 0);
        self.update(id, |task| task.speed_limit = limit).await?;
        if let Some(gate) = self.gates.lock().await.get(id) {
            gate.set_limit(limit.unwrap_or(0));
        }
        Ok(())
    }

    /// Byte-range progress for a segmented task, read from its `.segN` files.
    /// Returns an empty list for single-stream or finished tasks.
    pub async fn segment_progress(&self, id: &str) -> Vec<SegmentProgress> {
        let Ok(task) = self.get(id).await else {
            return Vec::new();
        };
        let Some(total) = task.total else {
            return Vec::new();
        };
        let segments = segment_count(total);
        if segments < 2 {
            return Vec::new();
        }
        let part = part_path(Path::new(&task.destination));
        if fs::metadata(seg_path(&part, 0)).await.is_err() {
            return Vec::new();
        }
        let base = total / segments as u64;
        let mut progress = Vec::with_capacity(segments);
        for index in 0..segments {
            let start = index as u64 * base;
            let end = if index == segments - 1 {
                total - 1
            } else {
                (index as u64 + 1) * base - 1
            };
            let len = end - start + 1;
            let downloaded = fs::metadata(seg_path(&part, index))
                .await
                .map(|meta| meta.len().min(len))
                .unwrap_or(0);
            progress.push(SegmentProgress {
                index,
                start,
                end,
                downloaded,
                total: len,
                complete: downloaded >= len,
            });
        }
        progress
    }

    async fn gate_for(&self, id: &str) -> Arc<RateGate> {
        if let Some(gate) = self.gates.lock().await.get(id) {
            return gate.clone();
        }
        let limit = self
            .get(id)
            .await
            .ok()
            .and_then(|task| task.speed_limit)
            .unwrap_or(0);
        let gate = Arc::new(RateGate::new(limit));
        self.gates
            .lock()
            .await
            .entry(id.to_string())
            .or_insert_with(|| gate.clone())
            .clone()
    }

    pub async fn add(&self, input: NewDownload) -> Result<DownloadTask> {
        validate_url(&input.url)?;
        let destination = PathBuf::from(&input.destination);
        let explicit_name = input
            .filename
            .as_deref()
            .map(net::sanitize_filename)
            .filter(|s| !s.is_empty());
        let filename_explicit = explicit_name.is_some();
        let filename = explicit_name.unwrap_or_else(|| filename_from_url(&input.url));
        let cookies = input
            .cookies
            .filter(|c| !c.is_empty() && c.len() <= 64 * 1024 && !c.contains(['\r', '\n', '\0']));
        let referrer = input
            .referrer
            .filter(|r| (r.starts_with("http://") || r.starts_with("https://")) && !r.contains(['\r', '\n', '\0']));
        let destination = if destination.is_dir() {
            destination.join(&filename)
        } else {
            destination
        };
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).await?;
        }
        let id = Uuid::new_v4().to_string();
        let task = DownloadTask {
            id: id.clone(),
            url: input.url,
            filename,
            destination: destination.to_string_lossy().to_string(),
            downloaded: 0,
            total: None,
            status: DownloadStatus::Queued,
            speed_limit: input.speed_limit,
            error: None,
            created_at: now(),
        };
        self.store.upsert(&task)?;
        self.extras.write().await.insert(
            id.clone(),
            RequestExtras {
                referrer,
                cookies,
                filename_explicit,
            },
        );
        self.tasks.write().await.insert(id, task.clone());
        Ok(task)
    }

    pub async fn start(&self, id: &str) -> Result<()> {
        let task = self.get(id).await?;
        if matches!(
            task.status,
            DownloadStatus::Completed | DownloadStatus::Cancelled
        ) {
            bail!("This task cannot be started in its current state");
        }
        let (tx, rx) = watch::channel(Control::Run);
        self.controls.lock().await.insert(id.to_string(), tx);
        self.update(id, |t| {
            t.status = DownloadStatus::Downloading;
            t.error = None;
        })
        .await?;
        let manager = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            if let Err(err) = manager.run_download(&id, rx).await {
                let message = format!("{err:#}");
                let _ = manager
                    .update(&id, |t| {
                        t.status = DownloadStatus::Failed;
                        t.error = Some(message);
                    })
                    .await;
            }
            manager.controls.lock().await.remove(&id);
        });
        Ok(())
    }

    pub async fn pause(&self, id: &str) -> Result<()> {
        if let Some(tx) = self.controls.lock().await.get(id) {
            let _ = tx.send(Control::Pause);
        }
        // The worker changes the task to Paused after flushing the partial file.
        // This avoids starting a second worker while the previous stream is still alive.
        Ok(())
    }

    pub async fn cancel(&self, id: &str) -> Result<()> {
        if let Some(tx) = self.controls.lock().await.get(id) {
            let _ = tx.send(Control::Cancel);
        }
        self.update(id, |t| t.status = DownloadStatus::Cancelled)
            .await?;
        Ok(())
    }

    pub async fn resume(&self, id: &str) -> Result<()> {
        self.start(id).await
    }

    pub async fn remove(&self, id: &str, delete_file: bool) -> Result<()> {
        let task = self.get(id).await?;
        if let Some(tx) = self.controls.lock().await.get(id) {
            let _ = tx.send(Control::Cancel);
        }
        if delete_file {
            let part = part_path(&PathBuf::from(&task.destination));
            let _ = fs::remove_file(part).await;
            if task.status == DownloadStatus::Completed {
                let _ = fs::remove_file(&task.destination).await;
            }
        }
        self.store.delete(id)?;
        self.tasks.write().await.remove(id);
        self.extras.write().await.remove(id);
        self.gates.lock().await.remove(id);
        Ok(())
    }

    async fn run_download(&self, id: &str, control: watch::Receiver<Control>) -> Result<()> {
        let task = self.get(id).await?;
        if is_torrent_url(&task.url) {
            return self.run_torrent(id, control).await;
        }
        if is_youtube_url(&task.url) {
            return self.run_ytdlp(id, control).await;
        }

        let headers = self.headers_for(id).await;
        let (resolved_url, probe) = self.probe_resolved(&task.url, &headers).await;
        if resolved_url != task.url {
            let new_url = resolved_url.clone();
            self.update(id, move |t| t.url = new_url).await?;
        }

        if let Some(probe) = &probe {
            // A web page where an archive/media file was expected means the
            // server wants a sign-in or confirmation. Saving it would produce a
            // "corrupt" file, so fail with a clear message instead.
            let current = self.get(id).await?;
            if probe.is_html()
                && (net::is_google_drive_host(&resolved_url)
                    || net::has_non_html_extension(&current.filename))
            {
                bail!(
                    "The server returned a web page instead of the file (sign-in or confirmation required). \
                     Start the download again from the browser while signed in."
                );
            }
            self.apply_server_filename(id, probe).await?;
        }

        let task = self.get(id).await?;
        let destination = PathBuf::from(&task.destination);
        let total = probe.as_ref().and_then(|p| p.total);
        let accept_ranges = probe.as_ref().map(|p| p.accept_ranges).unwrap_or(false);

        let segments = if accept_ranges {
            segment_count(total.unwrap_or(0))
        } else {
            1
        };

        if segments <= 1 || total.is_none() {
            return self.run_single(id, control).await;
        }
        self.run_segmented(id, total.unwrap(), segments, control, destination)
            .await
    }

    /// `Referer` / `Cookie` headers for a task. These live in memory only.
    async fn headers_for(&self, id: &str) -> HeaderMap {
        let extras = self.extras.read().await.get(id).cloned().unwrap_or_default();
        build_request_headers(extras.referrer.as_deref(), extras.cookies.as_deref())
    }

    /// One-byte range request: learns size, range support, name and type
    /// without downloading the body. Returns `None` if the server is
    /// unreachable or refuses; the download itself then reports the error.
    async fn probe(&self, url: &str, headers: &HeaderMap) -> Option<Probe> {
        let response = self
            .client
            .get(url)
            .headers(headers.clone())
            .header(header::RANGE, "bytes=0-0")
            .send()
            .await
            .ok()?;
        let status = response.status();
        let h = response.headers();
        let content_type = h
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string());
        let filename = h
            .get(header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .and_then(net::content_disposition_filename);
        let content_range = h
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let (total, accept_ranges) = match status {
            StatusCode::PARTIAL_CONTENT => (
                content_range
                    .as_deref()
                    .and_then(|v| v.rsplit('/').next())
                    .and_then(|v| v.trim().parse::<u64>().ok()),
                true,
            ),
            // Empty file: "bytes */0".
            StatusCode::RANGE_NOT_SATISFIABLE => (
                content_range
                    .as_deref()
                    .and_then(|v| v.rsplit('/').next())
                    .and_then(|v| v.trim().parse::<u64>().ok()),
                false,
            ),
            // The server ignored Range and answered with the whole body.
            s if s.is_success() => (response.content_length(), false),
            _ => return None,
        };
        Some(Probe {
            total,
            accept_ranges,
            filename,
            content_type,
        })
    }

    /// Probe, following Google Drive's "can't scan for viruses" page to the
    /// real download link when needed.
    async fn probe_resolved(&self, url: &str, headers: &HeaderMap) -> (String, Option<Probe>) {
        let mut url = url.to_string();
        let mut probe = self.probe(&url, headers).await;
        for _ in 0..2 {
            let html = probe.as_ref().map(Probe::is_html).unwrap_or(false);
            if !html || !net::is_google_drive_host(&url) {
                break;
            }
            let Some(next) = self.resolve_drive_confirmation(&url, headers).await else {
                break;
            };
            if next == url {
                break;
            }
            url = next;
            probe = self.probe(&url, headers).await;
        }
        (url, probe)
    }

    async fn resolve_drive_confirmation(&self, page_url: &str, headers: &HeaderMap) -> Option<String> {
        let response = self
            .client
            .get(page_url)
            .headers(headers.clone())
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let is_html = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(net::is_html_content_type)
            .unwrap_or(false);
        if !is_html {
            return None;
        }
        let body = response.text().await.ok()?;
        let found = net::google_drive_confirm_url(&body)?;
        let next = url::Url::parse(page_url).ok()?.join(&found).ok()?;
        Some(next.to_string())
    }

    /// Use the server's `Content-Disposition` name unless the user chose one
    /// or a partial download already exists under the current name.
    async fn apply_server_filename(&self, id: &str, probe: &Probe) -> Result<()> {
        let Some(name) = probe.filename.clone() else {
            return Ok(());
        };
        let explicit = self
            .extras
            .read()
            .await
            .get(id)
            .map(|e| e.filename_explicit)
            .unwrap_or(false);
        if explicit {
            return Ok(());
        }
        let task = self.get(id).await?;
        if task.filename == name {
            return Ok(());
        }
        let old_destination = PathBuf::from(&task.destination);
        let part = part_path(&old_destination);
        let mut seg0 = part.as_os_str().to_os_string();
        seg0.push(".seg0");
        if fs::metadata(&part).await.is_ok() || fs::metadata(PathBuf::from(seg0)).await.is_ok() {
            return Ok(());
        }
        let dir = old_destination
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let new_destination = unique_path(&dir.join(&name));
        let final_name = new_destination
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&name)
            .to_string();
        let final_destination = new_destination.to_string_lossy().to_string();
        self.update(id, move |t| {
            t.filename = final_name;
            t.destination = final_destination;
        })
        .await
    }

    /// Remote details for a link handed over by the browser extension.
    pub async fn probe_metadata(
        &self,
        url: &str,
        referrer: Option<&str>,
        cookies: Option<&str>,
    ) -> ProbeInfo {
        let headers = build_request_headers(referrer, cookies);
        let (_, probe) = self.probe_resolved(url, &headers).await;
        match probe {
            Some(p) if !p.is_html() => ProbeInfo {
                size: p.total,
                content_type: p.content_type,
                filename: p.filename,
            },
            _ => ProbeInfo::default(),
        }
    }

    async fn run_segmented(
        &self,
        id: &str,
        total: u64,
        segments: usize,
        control: watch::Receiver<Control>,
        destination: PathBuf,
    ) -> Result<()> {
        use futures_util::stream::StreamExt;
        use std::sync::atomic::{AtomicU64, Ordering};
        use tokio::io::AsyncWriteExt;

        if let Some(parent) = destination.parent() {
            let _ = fs::create_dir_all(parent).await;
        }
        let part = part_path(&destination);
        let seg_path = |i: usize| -> PathBuf {
            let mut name = part.as_os_str().to_os_string();
            name.push(format!(".seg{}", i));
            PathBuf::from(name)
        };

        let gate = self.gate_for(id).await;
        let base = total / segments as u64;
        let mut ranges: Vec<(usize, u64, u64)> = Vec::new();
        for i in 0..segments {
            let start = i as u64 * base;
            let end = if i == segments - 1 {
                total - 1
            } else {
                (i as u64 + 1) * base - 1
            };
            ranges.push((i, start, end));
        }

        // existing sizes
        let mut existing_sizes = vec![0u64; segments];
        for i in 0..segments {
            let seg_len = ranges[i].2 - ranges[i].1 + 1;
            if let Ok(meta) = tokio::fs::metadata(seg_path(i)).await {
                let len = meta.len();
                if len > seg_len {
                    // Corrupt/oversized partial file: discard and restart this segment.
                    let _ = tokio::fs::remove_file(seg_path(i)).await;
                    existing_sizes[i] = 0;
                } else {
                    existing_sizes[i] = len;
                }
            }
        }
        let downloaded_init: u64 = existing_sizes.iter().sum();
        // The probe result is not persisted by `run_download`; record it here so
        // the UI can show the total and per-part progress while parts arrive.
        self.update(id, |task| {
            task.total = Some(total);
            task.downloaded = downloaded_init;
            task.status = DownloadStatus::Downloading;
        })
        .await?;
        let downloaded = Arc::new(AtomicU64::new(downloaded_init));

        // control is shared; clone for segments
        let control_seg = control.clone();

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Outcome {
            Done,
            Paused,
            Cancelled,
            Fallback,
        }

        let request_headers = self.headers_for(id).await;
        let mut handles = Vec::new();
        for (i, start, end) in ranges {
            let seg_len = end - start + 1;
            if existing_sizes[i] >= seg_len {
                // already complete
                handles.push(tokio::spawn(async { Ok(Outcome::Done) }));
                continue;
            }
            let url = self.get(id).await?.url.clone();
            let seg_file = seg_path(i);
            let resume_from = start + existing_sizes[i];
            let manager = self.clone();
            let downloaded = downloaded.clone();
            let gate = gate.clone();
            let mut control_rx = control_seg.clone();
            let request_headers = request_headers.clone();
            let handle = tokio::spawn(async move {
                // If resume_from > end, done
                if resume_from > end {
                    return Ok(Outcome::Done);
                }
                let resp = manager
                    .client
                    .get(&url)
                    .headers(request_headers)
                    .header(reqwest::header::RANGE, format!("bytes={}-{}", resume_from, end))
                    .send()
                    .await?;
                if resp.status() == reqwest::StatusCode::OK {
                    // Server did not honor the range request; fall back to single stream.
                    return Ok(Outcome::Fallback);
                }
                if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                    return Err(anyhow::anyhow!("HTTP {}", resp.status()));
                }
                // Appending bytes from a different offset would silently corrupt the file.
                let served_start = resp
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("bytes "))
                    .and_then(|v| v.split_once('-'))
                    .and_then(|(start, _)| start.trim().parse::<u64>().ok());
                if served_start != Some(resume_from) {
                    return Err(anyhow::anyhow!("The server returned an unexpected byte range"));
                }
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&seg_file)
                    .await?;

                let mut stream = resp.bytes_stream();
                loop {
                    tokio::select! {
                        changed = control_rx.changed() => {
                            if changed.is_ok() {
                                let state = *control_rx.borrow();
                                match state {
                                    Control::Pause => {
                                        file.flush().await.ok();
                                        return Ok(Outcome::Paused);
                                    }
                                    Control::Cancel => {
                                        file.flush().await.ok();
                                        let _ = tokio::fs::remove_file(&seg_file).await;
                                        return Ok(Outcome::Cancelled);
                                    }
                                    Control::Run => {}
                                }
                            }
                        }
                        item = stream.next() => {
                            match item {
                                Some(Ok(chunk)) => {
                                    gate.acquire(chunk.len()).await;
                                    file.write_all(&chunk).await?;
                                    downloaded.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                                }
                                Some(Err(e)) => return Err(e.into()),
                                None => break,
                            }
                        }
                    }
                }
                file.flush().await?;
                Ok(Outcome::Done)
            });
            handles.push(handle);
        }

        // updater task to persist progress
        let updater_manager = self.clone();
        let updater_id = id.to_string();
        let updater_downloaded = downloaded.clone();
        let mut updater_control = control.clone();
        let updater_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
                    changed = updater_control.changed() => {
                        if changed.is_err() { break; }
                        let st = *updater_control.borrow();
                        if st == Control::Pause || st == Control::Cancel { break; }
                    }
                }
                let d = updater_downloaded.load(Ordering::Relaxed);
                let _ = updater_manager.update(&updater_id, |t| { t.downloaded = d; }).await;
                // check if all done? We'll break when handles complete (handled below)
            }
        });

        // await all segments
        let mut paused = false;
        let mut cancelled = false;
        let mut fallback = false;
        let mut first_err: Option<anyhow::Error> = None;
        for h in handles {
            match h.await {
                Ok(Ok(Outcome::Paused)) => paused = true,
                Ok(Ok(Outcome::Cancelled)) => cancelled = true,
                Ok(Ok(Outcome::Fallback)) => fallback = true,
                Ok(Err(e)) if first_err.is_none() => { first_err = Some(e); }
                Err(e) if first_err.is_none() => { first_err = Some(e.into()); }
                _ => {}
            }
        }
        updater_handle.abort();

        if fallback {
            for i in 0..segments {
                let _ = tokio::fs::remove_file(seg_path(i)).await;
            }
            return self.run_single(id, control).await;
        }
        if cancelled {
            for i in 0..segments {
                let _ = tokio::fs::remove_file(seg_path(i)).await;
            }
            self.update(id, |t| t.status = DownloadStatus::Cancelled)
                .await?;
            return Ok(());
        }
        if paused {
            let d = downloaded.load(Ordering::Relaxed);
            self.update(id, |t| {
                t.downloaded = d;
                t.status = DownloadStatus::Paused;
            })
            .await?;
            return Ok(());
        }
        if let Some(e) = first_err {
            let d = downloaded.load(Ordering::Relaxed);
            let _ = self.update(id, |t| t.downloaded = d).await;
            return Err(e);
        }

        self.update(id, |t| t.status = DownloadStatus::Merging).await.ok();
        // merge segments into destination
        let mut out = tokio::fs::File::create(&destination).await?;
        for i in 0..segments {
            let mut f = tokio::fs::File::open(seg_path(i)).await?;
            tokio::io::copy(&mut f, &mut out).await?;
        }
        out.flush().await?;
        drop(out);
        // Verify merged file size (IDM-like final check).
        match tokio::fs::metadata(&destination).await {
            Ok(meta) => {
                if meta.len() != total {
                    let _ = tokio::fs::remove_file(&destination).await;
                    for i in 0..segments {
                        let _ = tokio::fs::remove_file(seg_path(i)).await;
                    }
                    return Err(anyhow!("Incomplete merge: expected {total} bytes, got {}", meta.len()));
                }
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&destination).await;
                for i in 0..segments {
                    let _ = tokio::fs::remove_file(seg_path(i)).await;
                }
                return Err(e.into());
            }
        }
        for i in 0..segments {
            let _ = tokio::fs::remove_file(seg_path(i)).await;
        }

        self.update(id, |t| {
            t.downloaded = total;
            t.total = Some(total);
            t.status = DownloadStatus::Completed;
            t.error = None;
        })
        .await?;
        Ok(())
    }

    async fn run_torrent(&self, id: &str, mut control: watch::Receiver<Control>) -> Result<()> {
        use librqbit::{AddTorrent, Session};
        let task = self.get(id).await?;
        let destination = PathBuf::from(&task.destination);
        let out_dir = if destination.is_dir() {
            destination.clone()
        } else if let Some(parent) = destination.parent() {
            parent.to_path_buf()
        } else {
            PathBuf::from(".")
        };
        fs::create_dir_all(&out_dir).await?;

        let session = Session::new(out_dir.clone())
            .await
            .map_err(|e| anyhow!("{e}"))?;
        let add = AddTorrent::from_url(&task.url);
        let resp = session
            .add_torrent(add, None)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        let handle = resp.into_handle().ok_or_else(|| anyhow!("could not get torrent handle"))?;

        // Wait for metadata then download.
        let mut paused = false;
        loop {
            // control handling
            let ctrl_changed = tokio::select! {
                r = control.changed() => r.is_ok(),
                _ = tokio::time::sleep(std::time::Duration::from_millis(800)) => false,
            };
            if ctrl_changed {
                let current = *control.borrow();
                match current {
                    Control::Pause => {
                        if !paused {
                            let _ = session.pause(&handle).await;
                            paused = true;
                            self.update(id, |t| t.status = DownloadStatus::Paused).await.ok();
                        }
                    }
                    Control::Cancel => {
                        self.update(id, |t| t.status = DownloadStatus::Cancelled).await.ok();
                        return Ok(());
                    }
                    Control::Run => {
                        if paused {
                            let _ = session.unpause(&handle).await;
                            paused = false;
                            self.update(id, |t| t.status = DownloadStatus::Downloading).await.ok();
                        }
                    }
                }
            }
            if paused {
                continue;
            }
            let stats = handle.stats();
            let downloaded = stats.progress_bytes;
            let total = if stats.total_bytes > 0 {
                Some(stats.total_bytes)
            } else {
                None
            };
            let finished = stats.finished;
            self.update(id, |t| {
                t.downloaded = downloaded;
                if let Some(tot) = total {
                    t.total = Some(tot);
                }
                t.status = DownloadStatus::Downloading;
            })
            .await
            .ok();
            if finished {
                break;
            }
        }

        let stats = handle.stats();
        self.update(id, |t| {
            t.downloaded = stats.progress_bytes;
            if stats.total_bytes > 0 {
                t.total = Some(stats.total_bytes);
            }
            t.status = DownloadStatus::Completed;
            t.error = None;
        })
        .await?;
        Ok(())
    }

    pub async fn ensure_ytdlp(&self) -> Result<PathBuf> {
        // Prefer a binary already available on PATH.
        if let Ok(output) = tokio::process::Command::new("yt-dlp")
            .arg("--version")
            .output()
            .await
        {
            if output.status.success() {
                return Ok(PathBuf::from("yt-dlp"));
            }
        }

        // Common user-local install location.
        if let Some(home) = dirs::home_dir() {
            let local = home.join(".local").join("bin").join("yt-dlp");
            if local.exists() {
                return Ok(local);
            }
        }

        // App-managed install location.
        let base = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("rdm")
            .join("bin");
        let target = base.join("yt-dlp");
        if target.exists() {
            return Ok(target);
        }

        tokio::fs::create_dir_all(&base).await?;
        let url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp";
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .context("Could not download yt-dlp")?;
        if !resp.status().is_success() {
            bail!("Could not download yt-dlp: HTTP {}", resp.status());
        }
        let bytes = resp.bytes().await.context("Could not read yt-dlp download")?;
        tokio::fs::write(&target, &bytes).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = tokio::fs::metadata(&target).await?.permissions();
            perms.set_mode(0o755);
            tokio::fs::set_permissions(&target, perms).await?;
        }
        Ok(target)
    }

    async fn run_ytdlp(&self, id: &str, mut control: watch::Receiver<Control>) -> Result<()> {
        use tokio::io::AsyncBufReadExt;
        let task = self.get(id).await?;
        let destination = std::path::PathBuf::from(&task.destination);
        let out_dir = if destination.is_dir() {
            destination.clone()
        } else if let Some(parent) = destination.parent() {
            parent.to_path_buf()
        } else {
            std::path::PathBuf::from(".")
        };
        tokio::fs::create_dir_all(&out_dir).await?;

        // Resolve (and auto-install if necessary) the yt-dlp binary.
        let ytdlp = self.ensure_ytdlp().await?;

        self.update(id, |t| {
            t.status = DownloadStatus::Downloading;
            t.error = None;
        })
        .await?;

        let mut child = tokio::process::Command::new(&ytdlp)
            .arg("--no-playlist")
            .arg("-f")
            .arg("bestvideo[ext=mp4]+bestaudio[ext=m4a]/bestvideo+bestaudio/best")
            .arg("--merge-output-format")
            .arg("mp4")
            .arg("--newline")
            .arg("-o")
            .arg(out_dir.join("%(title).200s.%(ext)s"))
            .arg(&task.url)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        if let Some(stdout) = child.stdout.take() {
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            loop {
                tokio::select! {
                    changed = control.changed() => {
                        if changed.is_ok() {
                            let st = *control.borrow();
                            if st == Control::Cancel {
                                let _ = child.kill().await;
                                self.update(id, |t| t.status = DownloadStatus::Cancelled).await.ok();
                                return Ok(());
                            }
                        } else {
                            break;
                        }
                    }
                    line = lines.next_line() => {
                        match line {
                            Ok(Some(_line)) => {}
                            Ok(None) => break,
                            Err(_) => break,
                        }
                    }
                }
            }
        }

        let status = child.wait().await?;
        if !status.success() {
            let msg = if let Some(stderr) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let mut buf = Vec::new();
                let mut r = stderr;
                let _ = r.read_to_end(&mut buf).await;
                String::from_utf8_lossy(&buf).to_string()
            } else {
                "yt-dlp failed".to_string()
            };
            return Err(anyhow!("{}", msg.trim()));
        }

        // Find the produced file (best effort: newest file in out_dir)
        let mut newest: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
        if let Ok(mut entries) = tokio::fs::read_dir(&out_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if let Ok(meta) = entry.metadata().await {
                    if meta.is_file() {
                        if let Ok(modified) = meta.modified() {
                            if newest.as_ref().map(|(_, t)| modified > *t).unwrap_or(true) {
                                newest = Some((entry.path(), modified));
                            }
                        }
                    }
                }
            }
        }
        if let Some((file, _)) = newest {
            let size = tokio::fs::metadata(&file).await.map(|m| m.len()).unwrap_or(0);
            self.update(id, |t| {
                t.filename = file.file_name().unwrap_or_default().to_string_lossy().to_string();
                t.destination = file.to_string_lossy().to_string();
                t.downloaded = size;
                t.total = Some(size);
                t.status = DownloadStatus::Completed;
                t.error = None;
            })
            .await?;
        } else {
            self.update(id, |t| {
                t.status = DownloadStatus::Completed;
                t.error = None;
            })
            .await?;
        }
        Ok(())
    }

    async fn run_single(&self, id: &str, mut control: watch::Receiver<Control>) -> Result<()> {
        let task = self.get(id).await?;
        let gate = self.gate_for(id).await;
        let destination = PathBuf::from(&task.destination);
        let part = part_path(&destination);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).await?;
        }

        let existing = match fs::metadata(&part).await {
            Ok(meta) => meta.len(),
            Err(_) => 0,
        };

        let request_headers = self.headers_for(id).await;
        let mut request = self.client.get(&task.url).headers(request_headers.clone());
        if existing > 0 {
            request = request.header(header::RANGE, format!("bytes={existing}-"));
        }
        let mut response = request.send().await.context("HTTP request failed")?;

        let mut append = existing > 0 && response.status() == StatusCode::PARTIAL_CONTENT;
        if append {
            let range_start = response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("bytes "))
                .and_then(|v| v.split_once('-'))
                .and_then(|(start, _)| start.parse::<u64>().ok());
            if range_start != Some(existing) {
                append = false;
            }
        }
        if existing > 0 && !append {
            // Server ignored Range or representation changed. Restart safely.
            let _ = fs::remove_file(&part).await;
            response = self
                .client
                .get(&task.url)
                .headers(request_headers.clone())
                .send()
                .await?;
        }
        if !response.status().is_success() {
            bail!("HTTP {}", response.status());
        }
        let served_html = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(net::is_html_content_type)
            .unwrap_or(false);
        if served_html
            && (net::is_google_drive_host(&task.url) || net::has_non_html_extension(&task.filename))
        {
            bail!(
                "The server returned a web page instead of the file (sign-in or confirmation required). \
                 Start the download again from the browser while signed in."
            );
        }

        let content_length = response.content_length();
        let mut total = if append {
            content_length.map(|n| n.saturating_add(existing))
        } else {
            content_length
        };
        if total.is_none() {
            if let Ok(head) = self
                .client
                .head(&task.url)
                .headers(request_headers.clone())
                .send()
                .await
            {
                total = head.content_length();
            }
        }
        if total.is_none() {
            if let Ok(r) = self
                .client
                .get(&task.url)
                .headers(request_headers.clone())
                .header(reqwest::header::RANGE, "bytes=0-0")
                .send()
                .await
            {
                if let Some(cr) = r.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()) {
                    if let Some(t) = cr.rsplit('/').next().and_then(|s| s.trim().parse::<u64>().ok()) {
                        if t > 0 { total = Some(t); }
                    }
                }
            }
        }
        self.update(id, |t| {
            t.downloaded = if append { existing } else { 0 };
            t.total = total;
            t.status = DownloadStatus::Downloading;
            t.error = None;
        })
        .await?;

        let mut file = if append {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&part)
                .await?
        } else {
            File::create(&part).await?
        };

        let mut stream = response.bytes_stream();
        let mut downloaded = if append { existing } else { 0 };
        let mut last_progress = std::time::Instant::now();

        loop {
            tokio::select! {
                changed = control.changed() => {
                    if changed.is_err() { break; }
                    let control_state = *control.borrow();
                    match control_state {
                        Control::Pause => {
                            file.flush().await?;
                            self.update(id, |t| {
                                t.downloaded = downloaded;
                                t.status = DownloadStatus::Paused;
                            }).await?;
                            return Ok(());
                        }
                        Control::Cancel => {
                            file.flush().await?;
                            self.update(id, |t| {
                                t.downloaded = downloaded;
                                t.status = DownloadStatus::Cancelled;
                            }).await?;
                            return Ok(());
                        }
                        Control::Run => {}
                    }
                }
                item = stream.next() => {
                    match item {
                        Some(Ok(chunk)) => {
                            gate.acquire(chunk.len()).await;
                            file.write_all(&chunk).await?;
                            downloaded = downloaded.saturating_add(chunk.len() as u64);
                            if last_progress.elapsed() >= std::time::Duration::from_millis(500) {
                                self.update(id, |t| t.downloaded = downloaded).await?;
                                last_progress = std::time::Instant::now();
                            }
                        }
                        Some(Err(err)) => return Err(err.into()),
                        None => break,
                    }
                }
            }
        }
        file.flush().await?;
        drop(file);

        if let Some(expected) = total {
            if downloaded != expected {
                let _ = fs::remove_file(&part).await;
                bail!("Incomplete response: expected {expected} bytes, received {downloaded}");
            }
        }
        fs::rename(&part, &destination)
            .await
            .context("Could not move completed file into destination")?;
        self.update(id, |t| {
            t.downloaded = downloaded;
            t.total = Some(downloaded);
            t.status = DownloadStatus::Completed;
            t.error = None;
        })
        .await?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<DownloadTask> {
        self.tasks
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("Unknown task: {id}"))
    }

    async fn update<F>(&self, id: &str, f: F) -> Result<()>
    where
        F: FnOnce(&mut DownloadTask),
    {
        let mut tasks = self.tasks.write().await;
        let task = tasks
            .get_mut(id)
            .ok_or_else(|| anyhow!("Unknown task: {id}"))?;
        f(task);
        self.store.upsert(task)?;
        Ok(())
    }
}

fn validate_url(value: &str) -> Result<()> {
    let parsed = url::Url::parse(value).context("Invalid URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("Only HTTP and HTTPS URLs are supported");
    }
    Ok(())
}

fn filename_from_url(value: &str) -> String {
    net::filename_from_url(value)
}

fn build_request_headers(referrer: Option<&str>, cookies: Option<&str>) -> HeaderMap {
    let mut map = HeaderMap::new();
    if let Some(value) = referrer.and_then(|r| HeaderValue::from_str(r).ok()) {
        map.insert(header::REFERER, value);
    }
    if let Some(mut value) = cookies
        .filter(|c| !c.contains(['\r', '\n', '\0']))
        .and_then(|c| HeaderValue::from_str(c).ok())
    {
        value.set_sensitive(true);
        map.insert(header::COOKIE, value);
    }
    map
}

/// `name`, or `name (1).ext`, `name (2).ext`... if it already exists.
fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    for n in 1..10_000 {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    path.to_path_buf()
}

fn part_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_os_string();
    name.push(".rdm.part");
    PathBuf::from(name)
}

fn seg_path(part: &Path, index: usize) -> PathBuf {
    let mut name = part.as_os_str().to_os_string();
    name.push(format!(".seg{index}"));
    PathBuf::from(name)
}

/// Same split rule as `run_download`; kept in one place so reported parts
/// always match the ranges the engine actually uses.
fn segment_count(total: u64) -> usize {
    if total >= 2 * 1024 * 1024 {
        ((total / (1024 * 1024)) as usize).clamp(2, 8)
    } else {
        1
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_plan_is_stable() {
        assert_eq!(segment_count(0), 1);
        assert_eq!(segment_count(2 * 1024 * 1024 - 1), 1);
        assert_eq!(segment_count(2 * 1024 * 1024), 2);
        assert_eq!(segment_count(5 * 1024 * 1024), 5);
        assert_eq!(segment_count(64 * 1024 * 1024), 8);
    }

    #[test]
    fn rejects_non_http_protocols() {
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("ftp://example.com/file").is_err());
        assert!(validate_url("https://example.com/file").is_ok());
    }

    #[test]
    fn extracts_filename() {
        assert_eq!(
            filename_from_url("https://example.com/a/b.iso?token=x"),
            "b.iso"
        );
        assert_eq!(filename_from_url("https://example.com/"), "download.bin");
    }
}

fn is_torrent_url(url: &str) -> bool {
    url.starts_with("magnet:") || url.to_lowercase().ends_with(".torrent")
}

fn is_youtube_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.contains("youtube.com") || lower.contains("youtu.be")
}
