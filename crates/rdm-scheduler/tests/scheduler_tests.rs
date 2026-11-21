#![allow(clippy::field_reassign_with_default)]
use rdm_common::{
    DownloadStatus, NewDownloadRequest, Queue, QueueStatus, RepeatRule, Schedule, ScheduleAction,
    Settings,
};
use rdm_engine::{DownloadEngine, EngineConfig};
use rdm_ipc::protocol::Event;
use rdm_scheduler::{Scheduler, SchedulerConfig};
use rdm_storage::Storage;
use std::{sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::broadcast,
    task::JoinHandle,
};

struct Fixture {
    dir: TempDir,
    storage: Storage,
    engine: DownloadEngine,
    scheduler: Scheduler,
    events: broadcast::Sender<Event>,
    server: HoldServer,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::open(dir.path().join("db.sqlite3")).expect("storage");
        let engine = DownloadEngine::new(
            storage.clone(),
            EngineConfig {
                progress_interval: Duration::from_millis(50),
                max_retries: 0,
                read_timeout: Duration::from_secs(300),
                ..Default::default()
            },
        )
        .expect("engine");
        let scheduler = Scheduler::new(storage.clone(), engine.clone(), SchedulerConfig::default());
        let (events, _) = broadcast::channel(64);
        let server = HoldServer::start().await;
        Self {
            dir,
            storage,
            engine,
            scheduler,
            events,
            server,
        }
    }

    async fn add(&self, filename: &str, queue_id: Option<&str>) -> String {
        let destination = self.dir.path().join("out");
        let task = self
            .engine
            .add(NewDownloadRequest {
                url: self.server.url(),
                destination: destination.to_string_lossy().to_string(),
                filename: Some(filename.into()),
                connections: Some(2),
                speed_limit: None,
                checksum_sha256: None,
                referrer: None,
                headers: Vec::new(),
                cookies: None,
                queue_id: queue_id.map(str::to_string),
                priority: None,
                auto_start: None,
            })
            .await
            .expect("add");
        task.id
    }

    fn save_settings(&self, settings: &Settings) {
        self.storage.save_settings(settings).expect("settings");
    }

    async fn snapshots(&self) -> Vec<rdm_common::DownloadSnapshot> {
        self.engine.list().await.expect("list")
    }

    async fn status(&self, id: &str) -> DownloadStatus {
        self.snapshots()
            .await
            .into_iter()
            .find(|snapshot| snapshot.task.id == id)
            .expect("task")
            .task
            .status
    }

    async fn wait_status(&self, id: &str, status: DownloadStatus) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if self.status(id).await == status {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {status:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn shutdown(&self) {
        let _ = self.engine.pause_all().await;
    }
}

/// Accepts requests and never answers, keeping workers in the connecting state
/// so the scheduler's concurrency accounting is deterministic.
struct HoldServer {
    url: String,
    handle: JoinHandle<()>,
}

impl HoldServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buffer = [0u8; 4096];
                    let _ = stream.read(&mut buffer).await;
                    tokio::time::sleep(Duration::from_secs(300)).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self {
            url: format!("http://{address}/file.bin"),
            handle,
        }
    }

    fn url(&self) -> String {
        self.url.clone()
    }
}

impl Drop for HoldServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn queue(id: &str, status: QueueStatus, priority: i32, max: u32) -> Queue {
    Queue {
        id: id.into(),
        name: id.into(),
        max_concurrent: max,
        default_connections: 4,
        speed_limit: None,
        priority,
        status,
        created_at: rdm_common::time::now_unix(),
    }
}

#[tokio::test]
async fn respects_max_concurrent_downloads() {
    let fixture = Fixture::new().await;
    let mut settings = Settings::default();
    settings.max_concurrent_downloads = 2;
    fixture.save_settings(&settings);
    let ids = [
        fixture.add("a.bin", None).await,
        fixture.add("b.bin", None).await,
        fixture.add("c.bin", None).await,
    ];
    fixture.scheduler.tick(&fixture.events).await.expect("tick");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let snapshots = fixture.snapshots().await;
    let active = snapshots
        .iter()
        .filter(|snapshot| snapshot.task.status.is_active())
        .count();
    let queued = snapshots
        .iter()
        .filter(|snapshot| snapshot.task.status == DownloadStatus::Queued)
        .count();
    assert_eq!(active, 2, "snapshots: {snapshots:?}");
    assert_eq!(queued, 1);
    assert_eq!(ids.len(), 3);
    fixture.shutdown().await;
}

#[tokio::test]
async fn paused_queue_blocks_queued_downloads() {
    let fixture = Fixture::new().await;
    let mut settings = Settings::default();
    settings.max_concurrent_downloads = 4;
    fixture.save_settings(&settings);
    fixture
        .storage
        .insert_queue(&queue("paused", QueueStatus::Paused, 0, 4))
        .unwrap();
    let id = fixture.add("paused.bin", Some("paused")).await;
    fixture.scheduler.tick(&fixture.events).await.expect("tick");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fixture.status(&id).await, DownloadStatus::Queued);

    let mut running = queue("paused", QueueStatus::Running, 0, 4);
    running.status = QueueStatus::Running;
    fixture.storage.update_queue(&running).unwrap();
    fixture.scheduler.tick(&fixture.events).await.expect("tick");
    fixture.wait_status(&id, DownloadStatus::Connecting).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn queue_concurrency_limit_is_enforced() {
    let fixture = Fixture::new().await;
    let mut settings = Settings::default();
    settings.max_concurrent_downloads = 8;
    fixture.save_settings(&settings);
    fixture
        .storage
        .insert_queue(&queue("small", QueueStatus::Running, 0, 1))
        .unwrap();
    let first = fixture.add("one.bin", Some("small")).await;
    let second = fixture.add("two.bin", Some("small")).await;
    fixture.scheduler.tick(&fixture.events).await.expect("tick");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let statuses = [fixture.status(&first).await, fixture.status(&second).await];
    let active = statuses.iter().filter(|status| status.is_active()).count();
    let queued = statuses
        .iter()
        .filter(|status| **status == DownloadStatus::Queued)
        .count();
    assert_eq!(active, 1, "statuses: {statuses:?}");
    assert_eq!(queued, 1);
    fixture.shutdown().await;
}

#[tokio::test]
async fn due_start_queue_schedule_runs() {
    let fixture = Fixture::new().await;
    let mut settings = Settings::default();
    settings.max_concurrent_downloads = 4;
    fixture.save_settings(&settings);
    fixture
        .storage
        .insert_queue(&queue("night", QueueStatus::Paused, 0, 4))
        .unwrap();
    let id = fixture.add("night.bin", Some("night")).await;
    let now = rdm_common::time::now_unix();
    fixture
        .storage
        .insert_schedule(&Schedule {
            id: "start-night".into(),
            name: "start night".into(),
            action: ScheduleAction::StartQueue,
            target_id: Some("night".into()),
            run_at: now - 5,
            repeat_rule: RepeatRule::Once,
            enabled: true,
            confirmed: false,
            last_run: None,
            created_at: now - 10,
        })
        .unwrap();
    fixture.scheduler.tick(&fixture.events).await.expect("tick");
    let queue = fixture.storage.get_queue("night").unwrap().expect("queue");
    assert_eq!(queue.status, QueueStatus::Running);
    // The first tick ran before the schedule; the next tick starts the task.
    fixture.scheduler.tick(&fixture.events).await.expect("tick");
    fixture.wait_status(&id, DownloadStatus::Connecting).await;
    let schedule = fixture
        .storage
        .get_schedule("start-night")
        .unwrap()
        .expect("schedule");
    assert!(
        !schedule.enabled,
        "one-shot schedules disable after running"
    );
    assert!(schedule.last_run.is_some());
    fixture.shutdown().await;
}

#[tokio::test]
async fn bandwidth_window_sets_global_limit() {
    let fixture = Fixture::new().await;
    let mut settings = Settings::default();
    settings.bandwidth_schedule = vec![rdm_common::BandwidthWindow {
        start_minute: 0,
        end_minute: 1440,
        limit_bps: 777_000,
    }];
    settings.global_speed_limit = Some(9_000_000);
    fixture.save_settings(&settings);
    fixture.scheduler.tick(&fixture.events).await.expect("tick");
    assert_eq!(fixture.engine.global_speed_limit(), Some(777_000));

    settings.bandwidth_schedule.clear();
    fixture.save_settings(&settings);
    fixture.scheduler.tick(&fixture.events).await.expect("tick");
    assert_eq!(fixture.engine.global_speed_limit(), Some(9_000_000));

    settings.global_speed_limit = None;
    fixture.save_settings(&settings);
    fixture.scheduler.tick(&fixture.events).await.expect("tick");
    assert_eq!(fixture.engine.global_speed_limit(), None);
}

#[tokio::test]
async fn auto_retry_stops_at_max_retries() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let response = b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(response).await;
                let _ = stream.shutdown().await;
            });
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("db.sqlite3")).unwrap();
    let engine = DownloadEngine::new(
        storage.clone(),
        EngineConfig {
            max_retries: 0,
            progress_interval: Duration::from_millis(50),
            ..Default::default()
        },
    )
    .unwrap();
    let scheduler = Scheduler::new(storage.clone(), engine.clone(), SchedulerConfig::default());
    let (events, _) = broadcast::channel(64);
    let mut settings = Settings::default();
    settings.auto_retry = true;
    settings.max_concurrent_downloads = 2;
    storage.save_settings(&settings).unwrap();
    let task = engine
        .add(NewDownloadRequest {
            url: format!("http://{address}/fail.bin"),
            destination: dir.path().join("out").to_string_lossy().to_string(),
            filename: Some("fail.bin".into()),
            connections: Some(1),
            speed_limit: None,
            checksum_sha256: None,
            referrer: None,
            headers: Vec::new(),
            cookies: None,
            queue_id: None,
            priority: None,
            auto_start: None,
        })
        .await
        .unwrap();
    engine.start(&task.id).await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let task = storage.get_download(&task.id).unwrap().unwrap();
        if task.status == DownloadStatus::Failed {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    scheduler.tick(&events).await.unwrap();
    let after_first = storage.get_download(&task.id).unwrap().unwrap();
    assert_eq!(after_first.retry_count, 1);
    assert!(after_first.status.is_active() || after_first.status == DownloadStatus::Failed);
    // The cooldown prevents a second retry within the same second.
    scheduler.tick(&events).await.unwrap();
    let after_second = storage.get_download(&task.id).unwrap().unwrap();
    assert_eq!(after_second.retry_count, 1);
    let _ = engine.pause_all().await;
    server.abort();
}

#[tokio::test]
async fn unconfirmed_destructive_schedule_is_ignored() {
    let fixture = Fixture::new().await;
    let mut receiver = fixture.events.subscribe();
    let now = rdm_common::time::now_unix();
    fixture
        .storage
        .insert_schedule(&Schedule {
            id: "shutdown".into(),
            name: "shutdown".into(),
            action: ScheduleAction::Shutdown,
            target_id: None,
            run_at: now - 1,
            repeat_rule: RepeatRule::Once,
            enabled: true,
            confirmed: false,
            last_run: None,
            created_at: now - 10,
        })
        .unwrap();
    fixture.scheduler.tick(&fixture.events).await.expect("tick");
    assert!(matches!(
        receiver.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    let schedule = fixture
        .storage
        .get_schedule("shutdown")
        .unwrap()
        .expect("schedule");
    assert!(!schedule.enabled, "ignored one-shot schedule is consumed");
    let _ = Arc::new(());
}
