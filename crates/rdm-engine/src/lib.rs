//! Segmented HTTP/HTTPS download engine.
//!
//! The engine owns no GUI code and no daemon code. It can be embedded by the
//! daemon, CLI or tests. Each download runs in a worker that persists segment
//! state, exposes pause/cancel acknowledgements, and emits measured progress.

pub mod assemble;
pub mod http;
pub mod limiter;
pub mod probe;
pub mod runtime;
pub mod segment;
mod worker;

use crate::{
    assemble::{parts_dir, reset_parts},
    http::{build_headers, is_sensitive_header},
    limiter::SpeedLimiter,
    runtime::{ControlCommand, TaskRuntime, TaskSecrets},
};
use rdm_common::{
    filename_from_url, fsutil::is_within, normalize_sha256, parse_http_url, resolve_destination,
    sanitize_filename, time::now_unix, validate_headers, validate_referrer, DownloadDetails,
    DownloadSegment, DownloadSnapshot, DownloadStatus, DownloadTask, HeaderEntry,
    NewDownloadRequest, RdmError, RdmPaths, Result,
};
use rdm_storage::Storage;
use reqwest::{redirect::Policy, Client};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{broadcast, oneshot, Mutex};

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub user_agent: String,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub max_retries: u32,
    /// Files smaller than this are downloaded with a single connection.
    pub segment_threshold: u64,
    pub progress_interval: Duration,
    pub default_connections: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            user_agent: format!(
                "RDM/{} (Linux; +https://github.com/ruaavell/rdm)",
                rdm_common::RDM_VERSION
            ),
            connect_timeout: Duration::from_secs(20),
            read_timeout: Duration::from_secs(60),
            max_retries: 5,
            segment_threshold: 2 * 1024 * 1024,
            progress_interval: Duration::from_millis(500),
            default_connections: 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineEventKind {
    Added,
    Progress,
    StatusChanged,
    Completed,
    Failed,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineEvent {
    pub kind: EngineEventKind,
    pub snapshot: DownloadSnapshot,
}

#[derive(Clone)]
pub struct DownloadEngine {
    storage: Storage,
    client: Client,
    config: EngineConfig,
    runtimes: Arc<Mutex<HashMap<String, Arc<TaskRuntime>>>>,
    secrets: Arc<Mutex<HashMap<String, TaskSecrets>>>,
    events: broadcast::Sender<EngineEvent>,
    global_limiter: Arc<SpeedLimiter>,
}

impl DownloadEngine {
    pub fn new(storage: Storage, config: EngineConfig) -> Result<Self> {
        let client = Client::builder()
            .user_agent(config.user_agent.clone())
            .redirect(Policy::limited(10))
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.read_timeout)
            .pool_max_idle_per_host(32)
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .map_err(|err| RdmError::network(format!("could not build HTTP client: {err}")))?;
        let (events, _) = broadcast::channel(1024);
        Ok(Self {
            storage,
            client,
            config,
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            secrets: Arc::new(Mutex::new(HashMap::new())),
            events,
            global_limiter: SpeedLimiter::new(0),
        })
    }

    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    pub(crate) fn global_limiter(&self) -> Arc<SpeedLimiter> {
        self.global_limiter.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.events.subscribe()
    }

    pub fn set_global_speed_limit(&self, limit: Option<u64>) {
        self.global_limiter.set_rate(limit.unwrap_or(0));
    }

    pub fn global_speed_limit(&self) -> Option<u64> {
        match self.global_limiter.rate() {
            0 => None,
            value => Some(value),
        }
    }

    /// Add a task. It is not started automatically; callers decide whether the
    /// scheduler or an explicit user action should start it.
    pub async fn add(&self, request: NewDownloadRequest) -> Result<DownloadTask> {
        let NewDownloadRequest {
            url,
            destination,
            filename,
            connections,
            speed_limit,
            checksum_sha256,
            referrer,
            headers,
            cookies,
            queue_id,
            priority,
            auto_start: _,
        } = request;

        let parsed = parse_http_url(&url)?;
        let url = parsed.to_string();
        validate_headers(&headers)?;
        let referrer = validate_referrer(referrer.as_deref())?;
        let checksum_expected = checksum_sha256
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(normalize_sha256)
            .transpose()?;
        let cookies = cookies.filter(|value| !value.is_empty());
        if let Some(value) = &cookies {
            if value.len() > 64 * 1024 || value.contains(['\r', '\n', '\0']) {
                return Err(RdmError::InvalidHeader("Cookie".into()));
            }
        }
        if let Some(queue_id) = &queue_id {
            if self.storage.get_queue(queue_id)?.is_none() {
                return Err(RdmError::NotFound(format!("queue {queue_id}")));
            }
        }

        let filename_explicit = filename
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
        let requested_name = match filename {
            Some(value) if filename_explicit => sanitize_filename(&value),
            _ => filename_from_url(&url),
        };

        let paths = RdmPaths::discover()?;
        let destination_input = if destination.trim().is_empty() {
            paths.default_download_dir().to_string_lossy().to_string()
        } else {
            destination
        };
        let resolved = absolute_path(&resolve_destination(&destination_input, &requested_name))?;
        let directory = resolved
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| paths.default_download_dir());
        std::fs::create_dir_all(&directory)
            .map_err(|e| RdmError::from_io_at(directory.clone(), e))?;

        let destination_path = self.unique_destination(&directory, &resolved)?;
        let filename = destination_path
            .file_name()
            .and_then(|value| value.to_str())
            .map(sanitize_filename)
            .unwrap_or_else(|| requested_name.clone());

        // Sensitive headers and cookies stay in memory only. This avoids writing
        // credentials to SQLite in plaintext.
        let (persisted_headers, sensitive_headers): (Vec<HeaderEntry>, Vec<HeaderEntry>) = headers
            .into_iter()
            .partition(|header| !is_sensitive_header(&header.name));

        let task = DownloadTask {
            id: uuid::Uuid::new_v4().to_string(),
            url,
            filename,
            destination: destination_path.to_string_lossy().to_string(),
            directory: directory.to_string_lossy().to_string(),
            status: DownloadStatus::Queued,
            total_size: None,
            downloaded: 0,
            connections: connections
                .unwrap_or(self.config.default_connections)
                .clamp(1, 32),
            speed_limit: speed_limit.filter(|value| *value > 0),
            resumable: false,
            supports_ranges: false,
            mime_type: None,
            etag: None,
            last_modified: None,
            checksum_expected,
            checksum_actual: None,
            error: None,
            queue_id,
            priority: priority.unwrap_or(0),
            referrer,
            filename_explicit,
            retry_count: 0,
            max_retries: self.config.max_retries,
            created_at: now_unix(),
            started_at: None,
            completed_at: None,
        };

        self.storage.insert_download(&task, &persisted_headers)?;
        let secrets = TaskSecrets {
            headers: sensitive_headers,
            cookies,
        };
        if !secrets.is_empty() {
            self.secrets.lock().await.insert(task.id.clone(), secrets);
        }
        self.emit_current(&task.id, EngineEventKind::Added).await;
        Ok(task)
    }

    pub async fn start(&self, id: &str) -> Result<()> {
        let mut task = self
            .storage
            .get_download(id)?
            .ok_or_else(|| RdmError::NotFound(format!("download {id}")))?;

        if task.status == DownloadStatus::Completed {
            return Err(RdmError::InvalidState(
                "download is already completed".into(),
            ));
        }
        if task.status == DownloadStatus::Cancelled {
            self.reset_for_restart(&task).await?;
            task = self
                .storage
                .get_download(id)?
                .ok_or_else(|| RdmError::NotFound(format!("download {id}")))?;
        }

        let (runtime, receiver) =
            TaskRuntime::new(id.to_string(), task.connections, task.speed_limit);
        let runtime = Arc::new(runtime);
        {
            let mut runtimes = self.runtimes.lock().await;
            if let Some(existing) = runtimes.get(id) {
                if existing.control.is_closed() {
                    runtimes.remove(id);
                } else {
                    return Err(RdmError::InvalidState(
                        "a worker is already running for this download".into(),
                    ));
                }
            }
            runtimes.insert(id.to_string(), runtime.clone());
        }

        self.storage
            .update_status(id, DownloadStatus::Connecting, None)?;
        self.emit_current(id, EngineEventKind::StatusChanged).await;

        let engine = self.clone();
        let worker_id = id.to_string();
        let worker_runtime = runtime.clone();
        tokio::spawn(async move {
            let result = worker::run_worker(
                engine.clone(),
                worker_id.clone(),
                worker_runtime.clone(),
                receiver,
            )
            .await;
            engine
                .finish_worker(&worker_id, &worker_runtime.generation, result)
                .await;
        });
        Ok(())
    }

    pub async fn resume(&self, id: &str) -> Result<()> {
        self.start(id).await
    }

    pub async fn pause(&self, id: &str) -> Result<()> {
        let runtime = self.runtimes.lock().await.get(id).cloned();
        match runtime {
            Some(runtime) => {
                let (tx, rx) = oneshot::channel();
                runtime
                    .control
                    .send(ControlCommand::Pause(tx))
                    .await
                    .map_err(|_| {
                        RdmError::InvalidState("download worker is not available".into())
                    })?;
                match tokio::time::timeout(Duration::from_secs(60), rx).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => Err(RdmError::other("download worker dropped the pause request")),
                    Err(_) => Err(RdmError::other(
                        "timed out waiting for the download to pause",
                    )),
                }
            }
            None => {
                let task = self
                    .storage
                    .get_download(id)?
                    .ok_or_else(|| RdmError::NotFound(format!("download {id}")))?;
                if task.status.is_active() {
                    self.storage
                        .update_status(id, DownloadStatus::Paused, None)?;
                    self.emit_current(id, EngineEventKind::StatusChanged).await;
                }
                Ok(())
            }
        }
    }

    pub async fn cancel(&self, id: &str) -> Result<()> {
        let runtime = self.runtimes.lock().await.get(id).cloned();
        match runtime {
            Some(runtime) => {
                let (tx, rx) = oneshot::channel();
                runtime
                    .control
                    .send(ControlCommand::Cancel(tx))
                    .await
                    .map_err(|_| {
                        RdmError::InvalidState("download worker is not available".into())
                    })?;
                match tokio::time::timeout(Duration::from_secs(60), rx).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => Err(RdmError::other(
                        "download worker dropped the cancel request",
                    )),
                    Err(_) => Err(RdmError::other(
                        "timed out waiting for the download to cancel",
                    )),
                }
            }
            None => {
                let task = self
                    .storage
                    .get_download(id)?
                    .ok_or_else(|| RdmError::NotFound(format!("download {id}")))?;
                if !task.status.is_terminal() {
                    self.storage
                        .update_status(id, DownloadStatus::Cancelled, None)?;
                    self.emit_current(id, EngineEventKind::StatusChanged).await;
                }
                Ok(())
            }
        }
    }

    pub async fn remove(&self, id: &str, delete_file: bool) -> Result<()> {
        let task = self
            .storage
            .get_download(id)?
            .ok_or_else(|| RdmError::NotFound(format!("download {id}")))?;
        let _ = self.cancel(id).await;
        let destination = PathBuf::from(&task.destination);
        reset_parts(&parts_dir(&destination)).await?;
        let assembling = assemble::assembling_path(&destination);
        let _ = tokio::fs::remove_file(&assembling).await;
        if delete_file && task.status == DownloadStatus::Completed {
            let _ = tokio::fs::remove_file(&destination).await;
        }
        self.storage.delete_download(id)?;
        self.secrets.lock().await.remove(id);
        self.emit_current(&task.id, EngineEventKind::Removed).await;
        Ok(())
    }

    pub async fn pause_all(&self) -> Vec<(String, Result<()>)> {
        let ids: Vec<String> = self
            .storage
            .list_downloads()
            .unwrap_or_default()
            .into_iter()
            .filter(|task| task.status.is_active())
            .map(|task| task.id)
            .collect();
        let futures = ids
            .iter()
            .map(|id| async move { (id.to_string(), self.pause(id).await) });
        futures_util::future::join_all(futures).await
    }

    pub async fn resume_all(&self) -> Vec<(String, Result<()>)> {
        let ids: Vec<String> = self
            .storage
            .list_downloads()
            .unwrap_or_default()
            .into_iter()
            .filter(|task| task.status == DownloadStatus::Paused)
            .map(|task| task.id)
            .collect();
        let futures = ids
            .iter()
            .map(|id| async move { (id.to_string(), self.start(id).await) });
        futures_util::future::join_all(futures).await
    }

    /// Pause every worker, wait for buffers to flush, then checkpoint the WAL.
    pub async fn shutdown(&self) {
        let _ = self.pause_all().await;
        let _ = self.storage.checkpoint();
    }

    pub async fn list(&self) -> Result<Vec<DownloadSnapshot>> {
        let tasks = self.storage.list_downloads()?;
        let runtime_info: HashMap<String, (u64, u32)> = {
            let runtimes = self.runtimes.lock().await;
            runtimes
                .iter()
                .map(|(id, runtime)| (id.clone(), (runtime.speed(), runtime.active_connections())))
                .collect()
        };
        let mut snapshots = Vec::with_capacity(tasks.len());
        for task in tasks {
            let segments = self.storage.list_segments(&task.id)?;
            let (speed, active) = runtime_info.get(&task.id).copied().unwrap_or((0, 0));
            snapshots.push(self.snapshot_from(task, segments, speed, active));
        }
        Ok(snapshots)
    }

    pub async fn get_details(&self, id: &str) -> Result<DownloadDetails> {
        let task = self
            .storage
            .get_download(id)?
            .ok_or_else(|| RdmError::NotFound(format!("download {id}")))?;
        let segments = self.storage.list_segments(id)?;
        let headers = self.storage.get_headers(id)?;
        Ok(DownloadDetails {
            task,
            segments,
            headers,
        })
    }

    pub async fn update_speed_limit(&self, id: &str, limit: Option<u64>) -> Result<()> {
        let limit = limit.filter(|value| *value > 0);
        self.storage.set_speed_limit(id, limit)?;
        if let Some(runtime) = self.runtimes.lock().await.get(id) {
            runtime.limiter.set_rate(limit.unwrap_or(0));
        }
        self.emit_current(id, EngineEventKind::StatusChanged).await;
        Ok(())
    }

    pub async fn update_connections(&self, id: &str, connections: u32) -> Result<()> {
        let connections = connections.clamp(1, 32);
        self.storage.set_connections(id, connections)?;
        if let Some(runtime) = self.runtimes.lock().await.get(id) {
            runtime
                .connections
                .store(connections, std::sync::atomic::Ordering::Relaxed);
            let _ = runtime
                .control
                .send(ControlCommand::SetConnections(connections))
                .await;
        }
        self.emit_current(id, EngineEventKind::StatusChanged).await;
        Ok(())
    }

    pub fn set_queue(&self, id: &str, queue_id: Option<&str>) -> Result<()> {
        self.storage.set_queue(id, queue_id)
    }

    pub fn set_priority(&self, id: &str, priority: i32) -> Result<()> {
        self.storage.set_priority(id, priority)
    }

    pub async fn secrets_for(&self, id: &str) -> TaskSecrets {
        self.secrets
            .lock()
            .await
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn build_headers(
        &self,
        stored: &[HeaderEntry],
        secrets: &TaskSecrets,
        referrer: Option<&str>,
    ) -> Result<reqwest::header::HeaderMap> {
        build_headers(
            stored,
            &secrets.headers,
            referrer,
            secrets.cookies.as_deref(),
        )
    }

    pub(crate) fn snapshot_from(
        &self,
        task: DownloadTask,
        segments: Vec<DownloadSegment>,
        speed_bps: u64,
        active_connections: u32,
    ) -> DownloadSnapshot {
        let eta_seconds = match task.total_size {
            Some(total) if speed_bps > 0 && total > task.downloaded => {
                Some((total - task.downloaded) / speed_bps)
            }
            Some(_) if task.status == DownloadStatus::Completed => Some(0),
            _ => None,
        };
        DownloadSnapshot {
            task,
            speed_bps,
            eta_seconds,
            active_connections,
            segments,
        }
    }

    pub(crate) async fn snapshot_for(
        &self,
        id: &str,
        speed: u64,
        active_connections: u32,
    ) -> Result<DownloadSnapshot> {
        let task = self
            .storage
            .get_download(id)?
            .ok_or_else(|| RdmError::NotFound(format!("download {id}")))?;
        let segments = self.storage.list_segments(id)?;
        Ok(self.snapshot_from(task, segments, speed, active_connections))
    }

    pub(crate) async fn emit_current(&self, id: &str, kind: EngineEventKind) {
        let speed = self
            .runtimes
            .lock()
            .await
            .get(id)
            .map(|runtime| runtime.speed())
            .unwrap_or(0);
        let active = self
            .runtimes
            .lock()
            .await
            .get(id)
            .map(|runtime| runtime.active_connections())
            .unwrap_or(0);
        if let Ok(snapshot) = self.snapshot_for(id, speed, active).await {
            let _ = self.events.send(EngineEvent { kind, snapshot });
        }
    }

    pub(crate) fn emit_snapshot(&self, kind: EngineEventKind, snapshot: DownloadSnapshot) {
        let _ = self.events.send(EngineEvent { kind, snapshot });
    }

    async fn finish_worker(&self, id: &str, generation: &str, result: Result<()>) {
        let removed = {
            let mut runtimes = self.runtimes.lock().await;
            match runtimes.get(id) {
                Some(runtime) if runtime.generation == generation => {
                    runtimes.remove(id);
                    true
                }
                _ => false,
            }
        };
        if !removed {
            return;
        }

        match result {
            Ok(()) | Err(RdmError::Cancelled) => {}
            Err(error) => {
                let task = match self.storage.get_download(id) {
                    Ok(Some(task)) => task,
                    _ => return,
                };
                if !matches!(
                    task.status,
                    DownloadStatus::Completed
                        | DownloadStatus::Cancelled
                        | DownloadStatus::Paused
                        | DownloadStatus::Failed
                ) {
                    let message = error.to_string();
                    let _ = self
                        .storage
                        .update_status(id, DownloadStatus::Failed, Some(&message));
                    self.emit_current(id, EngineEventKind::Failed).await;
                }
            }
        }

        if let Ok(Some(task)) = self.storage.get_download(id) {
            if matches!(
                task.status,
                DownloadStatus::Completed | DownloadStatus::Cancelled
            ) {
                self.secrets.lock().await.remove(id);
            }
        }
    }

    async fn reset_for_restart(&self, task: &DownloadTask) -> Result<()> {
        let destination = PathBuf::from(&task.destination);
        reset_parts(&parts_dir(&destination)).await?;
        let _ = tokio::fs::remove_file(assemble::assembling_path(&destination)).await;
        self.storage.delete_segments(&task.id)?;
        let mut reset = task.clone();
        reset.status = DownloadStatus::Queued;
        reset.downloaded = 0;
        reset.total_size = None;
        reset.error = None;
        reset.checksum_actual = None;
        reset.retry_count = 0;
        reset.started_at = None;
        reset.completed_at = None;
        self.storage.update_download(&reset)?;
        Ok(())
    }

    fn unique_destination(&self, directory: &Path, resolved: &Path) -> Result<PathBuf> {
        let mut candidate = resolved.to_path_buf();
        for _ in 0..10_000 {
            let candidate_string = candidate.to_string_lossy().to_string();
            let occupied = candidate.exists()
                || self
                    .storage
                    .find_active_by_destination(&candidate_string)?
                    .is_some();
            if !occupied {
                return Ok(candidate);
            }
            candidate = next_available_path(&candidate);
        }
        Err(RdmError::Other(format!(
            "could not find a free filename in {}",
            directory.display()
        )))
    }

    /// Validate that a path is a normal file inside the selected directory.
    pub fn validate_output_path(&self, path: &Path, directory: &Path) -> Result<()> {
        if !is_within(path, directory) {
            return Err(RdmError::PathTraversal(path.to_path_buf()));
        }
        Ok(())
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let current =
        std::env::current_dir().map_err(|e| RdmError::from_io_at(PathBuf::from("."), e))?;
    Ok(current.join(path))
}

fn next_available_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 1..10_000 {
        let candidate = match extension {
            Some(ext) => parent.join(format!("{stem} ({index}).{ext}")),
            None => parent.join(format!("{stem} ({index})")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem}-{}", now_unix()))
}
