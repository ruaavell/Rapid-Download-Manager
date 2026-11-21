mod store;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use reqwest::{header, Client, StatusCode};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewDownload {
    pub url: String,
    pub destination: String,
    pub filename: Option<String>,
    pub speed_limit: Option<u64>,
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
        })
    }

    pub async fn list(&self) -> Vec<DownloadTask> {
        let mut items: Vec<_> = self.tasks.read().await.values().cloned().collect();
        items.sort_by_key(|t| -t.created_at);
        items
    }

    pub async fn add(&self, input: NewDownload) -> Result<DownloadTask> {
        validate_url(&input.url)?;
        let destination = PathBuf::from(&input.destination);
        let filename = input
            .filename
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| filename_from_url(&input.url));
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
        Ok(())
    }

    async fn run_download(&self, id: &str, control: watch::Receiver<Control>) -> Result<()> {
        let task = self.get(id).await?;
        let destination = PathBuf::from(&task.destination);
        // Probe server capabilities.
        let head = self.client.head(&task.url).send().await.ok();
        let total = head.as_ref().and_then(|r| r.content_length());
        let accept_ranges = head
            .as_ref()
            .and_then(|r| r.headers().get(reqwest::header::ACCEPT_RANGES))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.contains("bytes"))
            .unwrap_or(false);

        let segments = if accept_ranges {
            let t = total.unwrap_or(0);
            if t >= 2 * 1024 * 1024 {
                ((t / (1024 * 1024)) as usize).clamp(2, 8)
            } else {
                1
            }
        } else {
            1
        };

        if segments <= 1 || total.is_none() {
            return self.run_single(id, control).await;
        }
        self.run_segmented(id, total.unwrap(), segments, control, destination)
            .await
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

        let part = part_path(&destination);
        let seg_path = |i: usize| -> PathBuf {
            let mut name = part.as_os_str().to_os_string();
            name.push(format!(".seg{}", i));
            PathBuf::from(name)
        };

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
            if let Ok(meta) = tokio::fs::metadata(seg_path(i)).await {
                existing_sizes[i] = meta.len().min(ranges[i].2 - ranges[i].1 + 1);
            }
        }
        let downloaded_init: u64 = existing_sizes.iter().sum();
        let downloaded = Arc::new(AtomicU64::new(downloaded_init));

        // control is shared; clone for segments
        let control_seg = control.clone();

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Outcome {
            Done,
            Paused,
            Cancelled,
        }

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
            let mut control_rx = control_seg.clone();
            let handle = tokio::spawn(async move {
                // If resume_from > end, done
                if resume_from > end {
                    return Ok(Outcome::Done);
                }
                let resp = manager
                    .client
                    .get(&url)
                    .header(reqwest::header::RANGE, format!("bytes={}-{}", resume_from, end))
                    .send()
                    .await?;
                if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT
                    && !resp.status().is_success()
                {
                    return Err(anyhow::anyhow!("HTTP {}", resp.status()));
                }
                // If server ignored range and returned 200, restart segment
                let append = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
                let mut file = if append {
                    tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&seg_file)
                        .await?
                } else {
                    // restart
                    let _ = tokio::fs::remove_file(&seg_file).await;
                    tokio::fs::File::create(&seg_file).await?
                };

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
        let mut first_err: Option<anyhow::Error> = None;
        for h in handles {
            match h.await {
                Ok(Ok(Outcome::Paused)) => paused = true,
                Ok(Ok(Outcome::Cancelled)) => cancelled = true,
                Ok(Err(e)) if first_err.is_none() => { first_err = Some(e); }
                Err(e) if first_err.is_none() => { first_err = Some(e.into()); }
                _ => {}
            }
        }
        updater_handle.abort();

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

        // merge segments into destination
        let mut out = tokio::fs::File::create(&destination).await?;
        for i in 0..segments {
            let mut f = tokio::fs::File::open(seg_path(i)).await?;
            tokio::io::copy(&mut f, &mut out).await?;
        }
        out.flush().await?;
        drop(out);
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

    async fn run_single(&self, id: &str, mut control: watch::Receiver<Control>) -> Result<()> {
        let task = self.get(id).await?;
        let destination = PathBuf::from(&task.destination);
        let part = part_path(&destination);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).await?;
        }

        let existing = match fs::metadata(&part).await {
            Ok(meta) => meta.len(),
            Err(_) => 0,
        };

        let mut request = self.client.get(&task.url);
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
            response = self.client.get(&task.url).send().await?;
        }
        if !response.status().is_success() {
            bail!("HTTP {}", response.status());
        }

        let content_length = response.content_length();
        let total = if append {
            content_length.map(|n| n.saturating_add(existing))
        } else {
            content_length
        };
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
        let mut speed_window_bytes = 0u64;
        let mut speed_window_start = std::time::Instant::now();

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
                            if let Some(limit) = task.speed_limit.filter(|v| *v > 0) {
                                let expected = std::time::Duration::from_secs_f64(
                                    chunk.len() as f64 / limit as f64
                                );
                                let elapsed = speed_window_start.elapsed();
                                if expected > elapsed {
                                    tokio::time::sleep(expected - elapsed).await;
                                }
                            }
                            file.write_all(&chunk).await?;
                            downloaded = downloaded.saturating_add(chunk.len() as u64);
                            speed_window_bytes = speed_window_bytes.saturating_add(chunk.len() as u64);
                            if speed_window_start.elapsed() >= std::time::Duration::from_millis(500) {
                                let elapsed = speed_window_start.elapsed().as_secs_f64();
                                let speed = (speed_window_bytes as f64 / elapsed) as u64;
                                self.update(id, |t| t.downloaded = downloaded).await?;
                                speed_window_bytes = 0;
                                speed_window_start = std::time::Instant::now();
                                let _ = speed;
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
    url::Url::parse(value)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut s| s.next_back())
                .map(str::to_owned)
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "download.bin".to_string())
}

fn part_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_os_string();
    name.push(".rdm.part");
    PathBuf::from(name)
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
