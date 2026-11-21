use anyhow::Result;
use clap::Parser;
use rdm_common::{RdmError, RdmPaths};
use rdm_engine::DownloadEngine;
use rdm_ipc::protocol::{Command, Event, PROTOCOL_VERSION};
use rdm_ipc::server::{Handler, IpcServer};
use rdm_scheduler::{Scheduler, SchedulerConfig};
use rdm_storage::Storage;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(name = "rdm-daemon", about = "RDM background download daemon")]
struct Args {
    #[arg(long)]
    foreground: bool,
}

#[allow(dead_code)]
struct Daemon {
    storage: Storage,
    engine: DownloadEngine,
    scheduler: Scheduler,
    events: broadcast::Sender<Event>,
}

#[async_trait::async_trait]
impl Handler for Daemon {
    async fn handle(&self, command: Command) -> Result<Value, RdmError> {
        match command {
            Command::Ping => {
                let value = serde_json::json!({
                    "version": rdm_common::RDM_VERSION,
                    "pid": std::process::id(),
                    "protocol": PROTOCOL_VERSION,
                });
                Ok(value)
            }
            Command::Status => {
                let tasks = self.engine.list().await?;
                let active = tasks.iter().filter(|t| t.task.status.is_active()).count();
                let queued = tasks
                    .iter()
                    .filter(|t| t.task.status == rdm_common::DownloadStatus::Queued)
                    .count();
                let completed = tasks
                    .iter()
                    .filter(|t| t.task.status == rdm_common::DownloadStatus::Completed)
                    .count();
                let failed = tasks
                    .iter()
                    .filter(|t| t.task.status == rdm_common::DownloadStatus::Failed)
                    .count();
                let total_bytes: u64 = tasks.iter().map(|t| t.task.downloaded).sum();
                let status = rdm_common::AppStatus {
                    version: rdm_common::RDM_VERSION.into(),
                    daemon_pid: std::process::id(),
                    uptime_seconds: 0,
                    socket_path: String::new(),
                    database_path: self.storage.path().to_string_lossy().to_string(),
                    active_downloads: active,
                    queued_downloads: queued,
                    completed_downloads: completed,
                    failed_downloads: failed,
                    global_speed_bps: self.engine.global_speed_limit().unwrap_or(0),
                    total_downloaded_bytes: total_bytes,
                };
                Ok(serde_json::to_value(rdm_ipc::protocol::StatusPayload { status }).unwrap())
            }
            Command::ListDownloads => {
                let downloads = self.engine.list().await?;
                Ok(serde_json::to_value(rdm_ipc::protocol::DownloadList { downloads }).unwrap())
            }
            Command::GetDownload { download_id } => {
                let details = self.engine.get_details(&download_id).await?;
                Ok(
                    serde_json::to_value(rdm_ipc::protocol::DownloadDetailsPayload { details })
                        .unwrap(),
                )
            }
            Command::AddDownload { request } => {
                let task = self.engine.add(request).await?;
                Ok(serde_json::to_value(task).unwrap())
            }
            Command::StartDownload { download_id } => {
                self.engine.start(&download_id).await?;
                Ok(Value::Null)
            }
            Command::PauseDownload { download_id } => {
                self.engine.pause(&download_id).await?;
                Ok(Value::Null)
            }
            Command::ResumeDownload { download_id } => {
                self.engine.resume(&download_id).await?;
                Ok(Value::Null)
            }
            Command::CancelDownload { download_id } => {
                self.engine.cancel(&download_id).await?;
                Ok(Value::Null)
            }
            Command::RemoveDownload {
                download_id,
                delete_file,
            } => {
                self.engine.remove(&download_id, delete_file).await?;
                Ok(Value::Null)
            }
            Command::PauseAll => {
                let _ = self.engine.pause_all().await;
                Ok(Value::Null)
            }
            Command::ResumeAll => {
                let _ = self.engine.resume_all().await;
                Ok(Value::Null)
            }
            Command::SetSpeedLimit {
                download_id,
                limit_bps,
            } => {
                self.engine
                    .update_speed_limit(&download_id, limit_bps)
                    .await?;
                Ok(Value::Null)
            }
            Command::SetConnections {
                download_id,
                connections,
            } => {
                self.engine
                    .update_connections(&download_id, connections)
                    .await?;
                Ok(Value::Null)
            }
            Command::SetDownloadQueue {
                download_id,
                queue_id,
            } => {
                self.engine.set_queue(&download_id, queue_id.as_deref())?;
                Ok(Value::Null)
            }
            Command::SetPriority {
                download_id,
                priority,
            } => {
                self.engine.set_priority(&download_id, priority)?;
                Ok(Value::Null)
            }
            Command::ListQueues => {
                let queues = self.storage.list_queues()?;
                Ok(serde_json::to_value(rdm_ipc::protocol::QueueList { queues }).unwrap())
            }
            Command::CreateQueue { queue } => {
                self.storage.insert_queue(&queue)?;
                Ok(Value::Null)
            }
            Command::UpdateQueue { queue } => {
                self.storage.update_queue(&queue)?;
                Ok(Value::Null)
            }
            Command::DeleteQueue { queue_id } => {
                let _ = self.storage.delete_queue(&queue_id)?;
                Ok(Value::Null)
            }
            Command::SetQueueStatus { queue_id, status } => {
                self.storage.set_queue_status(&queue_id, status)?;
                Ok(Value::Null)
            }
            Command::ListCategories => {
                let categories = self.storage.list_categories()?;
                Ok(serde_json::to_value(rdm_ipc::protocol::CategoryList { categories }).unwrap())
            }
            Command::CreateCategory { category } => {
                self.storage.insert_category(&category)?;
                Ok(Value::Null)
            }
            Command::UpdateCategory { category } => {
                self.storage.update_category(&category)?;
                Ok(Value::Null)
            }
            Command::DeleteCategory { category_id } => {
                let _ = self.storage.delete_category(&category_id)?;
                Ok(Value::Null)
            }
            Command::ListSchedules => {
                let schedules = self.storage.list_schedules()?;
                Ok(serde_json::to_value(rdm_ipc::protocol::ScheduleList { schedules }).unwrap())
            }
            Command::CreateSchedule { schedule } => {
                self.storage.insert_schedule(&schedule)?;
                Ok(Value::Null)
            }
            Command::UpdateSchedule { schedule } => {
                self.storage.update_schedule(&schedule)?;
                Ok(Value::Null)
            }
            Command::DeleteSchedule { schedule_id } => {
                let _ = self.storage.delete_schedule(&schedule_id)?;
                Ok(Value::Null)
            }
            Command::GetSettings => {
                let settings = self.storage.load_settings()?;
                Ok(serde_json::to_value(settings).unwrap())
            }
            Command::SaveSettings { settings } => {
                settings
                    .validate()
                    .map_err(|e| RdmError::InvalidState(e.to_string()))?;
                self.storage.save_settings(&settings)?;
                self.engine
                    .set_global_speed_limit(settings.global_speed_limit);
                Ok(Value::Null)
            }
            Command::ListHistory { limit, offset } => {
                let entries = self.storage.list_history(limit, offset)?;
                Ok(serde_json::to_value(rdm_ipc::protocol::HistoryList { entries }).unwrap())
            }
            Command::ClearHistory => {
                self.storage.clear_history()?;
                Ok(Value::Null)
            }
            Command::DeleteHistory { entry_id } => {
                let _ = self.storage.delete_history(entry_id)?;
                Ok(Value::Null)
            }
            Command::ListBrowserInbox { limit } => {
                let entries = self.storage.list_pending_browser_entries(limit)?;
                Ok(serde_json::to_value(rdm_ipc::protocol::BrowserInboxList { entries }).unwrap())
            }
            Command::ApproveBrowserEntry {
                entry_id,
                destination,
                filename,
                connections,
            } => {
                let entry = self
                    .storage
                    .list_pending_browser_entries(10_000)?
                    .into_iter()
                    .find(|e| e.id == entry_id)
                    .ok_or_else(|| RdmError::NotFound(format!("browser inbox {entry_id}")))?;
                let dest = destination.unwrap_or_default();
                let task = self
                    .engine
                    .add(rdm_common::NewDownloadRequest {
                        url: entry.url,
                        destination: dest,
                        filename,
                        connections,
                        speed_limit: None,
                        checksum_sha256: None,
                        referrer: entry.referrer,
                        headers: entry.headers,
                        cookies: entry.cookies,
                        queue_id: None,
                        priority: None,
                        auto_start: Some(true),
                    })
                    .await?;
                self.storage.mark_browser_entry_processed(entry_id)?;
                Ok(serde_json::to_value(task).unwrap())
            }
            Command::DismissBrowserEntry { entry_id } => {
                self.storage.mark_browser_entry_processed(entry_id)?;
                Ok(Value::Null)
            }
            Command::ClearProcessedBrowserEntries => {
                let _ = self.storage.delete_processed_browser_entries()?;
                Ok(Value::Null)
            }
            Command::ExportBundle => {
                let downloads = self.storage.list_all_for_export()?;
                let queues = self.storage.list_queues()?;
                let categories = self.storage.list_categories()?;
                let bundle = rdm_common::ExportBundle {
                    version: rdm_common::RDM_VERSION.into(),
                    exported_at: rdm_common::time::now_unix(),
                    downloads: downloads
                        .into_iter()
                        .map(|t| rdm_common::ExportDownload {
                            url: t.url,
                            filename: t.filename,
                            destination: t.destination,
                            connections: t.connections,
                            speed_limit: t.speed_limit,
                            checksum_sha256: t.checksum_expected,
                            referrer: t.referrer,
                            headers: vec![],
                            queue_id: t.queue_id,
                            priority: t.priority,
                        })
                        .collect(),
                    queues,
                    categories,
                };
                Ok(serde_json::to_value(bundle).unwrap())
            }
            Command::ImportBundle { bundle } => {
                let mut imported = 0usize;
                let mut skipped = 0usize;
                let mut errors = Vec::new();
                for queue in &bundle.queues {
                    if self.storage.get_queue(&queue.id).ok().flatten().is_none() {
                        let _ = self.storage.insert_queue(queue);
                    }
                }
                for category in &bundle.categories {
                    if self
                        .storage
                        .get_category(&category.id)
                        .ok()
                        .flatten()
                        .is_none()
                    {
                        let _ = self.storage.insert_category(category);
                    }
                }
                for item in &bundle.downloads {
                    match self
                        .engine
                        .add(rdm_common::NewDownloadRequest {
                            url: item.url.clone(),
                            destination: item.destination.clone(),
                            filename: Some(item.filename.clone()),
                            connections: Some(item.connections),
                            speed_limit: item.speed_limit,
                            checksum_sha256: item.checksum_sha256.clone(),
                            referrer: item.referrer.clone(),
                            headers: item.headers.clone(),
                            cookies: None,
                            queue_id: item.queue_id.clone(),
                            priority: Some(item.priority),
                            auto_start: Some(false),
                        })
                        .await
                    {
                        Ok(_) => imported += 1,
                        Err(err) => {
                            skipped += 1;
                            errors.push(err.to_string());
                        }
                    }
                }
                let result = rdm_common::ImportResult {
                    imported,
                    skipped,
                    errors,
                };
                Ok(serde_json::to_value(rdm_ipc::protocol::ImportPayload { result }).unwrap())
            }
            Command::OpenPath { path, target } => {
                let program = match target {
                    rdm_ipc::protocol::OpenTarget::File
                    | rdm_ipc::protocol::OpenTarget::Folder
                    | rdm_ipc::protocol::OpenTarget::Reveal => "xdg-open",
                };
                let _ = std::process::Command::new(program).arg(&path).spawn();
                Ok(Value::Null)
            }
            Command::ProbeMedia { url } => {
                let info = rdm_media::probe(&url)
                    .await
                    .unwrap_or_else(|e| rdm_common::MediaInfo {
                        kind: rdm_common::MediaKind::Direct,
                        url: url.clone(),
                        title: None,
                        duration_seconds: None,
                        streams: vec![],
                        subtitles: vec![],
                        drm_protected: false,
                        notes: vec![e.to_string()],
                    });
                Ok(serde_json::to_value(rdm_ipc::protocol::MediaInfoPayload { info }).unwrap())
            }
            Command::StartMedia { request } => {
                let job = rdm_media::start(&self.engine, request).await?;
                Ok(serde_json::to_value(job).unwrap())
            }
            Command::MediaStatus { job_id } => {
                let status =
                    rdm_media::status(&job_id).unwrap_or_else(|| rdm_common::MediaJobStatus {
                        id: job_id,
                        url: String::new(),
                        phase: "unknown".into(),
                        downloaded_segments: 0,
                        total_segments: None,
                        downloaded_bytes: 0,
                        percent: 0.0,
                        message: "not found".into(),
                        output_path: None,
                        error: Some("not found".into()),
                        done: true,
                    });
                Ok(serde_json::to_value(status).unwrap())
            }
            Command::ListMediaJobs => {
                let jobs = rdm_media::list();
                Ok(serde_json::to_value(rdm_ipc::protocol::MediaJobList { jobs }).unwrap())
            }
            Command::CancelMedia { job_id } => {
                rdm_media::cancel(&job_id);
                Ok(Value::Null)
            }
            Command::Subscribe => {
                // Handled by IPC server.
                Ok(Value::Null)
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let paths = RdmPaths::discover()?;
    paths.ensure()?;
    let (storage, _report) = Storage::open_with_report(&paths.database)?;
    let settings = storage.load_settings().unwrap_or_default();
    let engine = DownloadEngine::new(storage.clone(), rdm_engine::EngineConfig::default())
        .map_err(|e| anyhow::anyhow!(e))?;
    engine.set_global_speed_limit(settings.global_speed_limit);
    let scheduler = Scheduler::new(storage.clone(), engine.clone(), SchedulerConfig::default());

    let (events, _) = broadcast::channel(1024);
    let daemon = Arc::new(Daemon {
        storage: storage.clone(),
        engine: engine.clone(),
        scheduler: scheduler.clone(),
        events: events.clone(),
    });
    let handler: Arc<dyn Handler> = daemon.clone();

    let server = IpcServer::bind(&paths)?;
    let _token = server.token().to_string();
    // write token already done by server
    let shutdown = CancellationToken::new();
    let sched_shutdown = shutdown.clone();
    let sched_handle = tokio::spawn({
        let scheduler = scheduler.clone();
        let events = events.clone();
        async move {
            let _ = scheduler.run(events, sched_shutdown).await;
        }
    });

    let server_handle = tokio::spawn({
        let server = server;
        let handler = handler.clone();
        let events = events.clone();
        let shutdown = shutdown.clone();
        async move {
            let _ = server.run(handler, events, shutdown).await;
        }
    });

    // signal handling
    let shutdown_sig = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        shutdown_sig.cancel();
    });

    let _ = tokio::join!(server_handle, sched_handle);
    engine.shutdown().await;
    let _ = storage.checkpoint();
    let _ = args;
    Ok(())
}
