mod support;

use rdm_common::{DownloadSnapshot, DownloadStatus, NewDownloadRequest, RdmError};
use rdm_engine::{DownloadEngine, EngineConfig};
use rdm_storage::Storage;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use support::{MockOptions, MockServer};
use tempfile::TempDir;

struct Harness {
    engine: DownloadEngine,
    dir: TempDir,
    server: MockServer,
}

async fn harness(options: MockOptions) -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(dir.path().join("db.sqlite3")).expect("storage");
    let engine = DownloadEngine::new(
        storage,
        EngineConfig {
            segment_threshold: 1024 * 1024,
            progress_interval: Duration::from_millis(40),
            max_retries: 4,
            ..Default::default()
        },
    )
    .expect("engine");
    let server = MockServer::start(options).await;
    Harness {
        engine,
        dir,
        server,
    }
}

fn body(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index % 251) as u8).collect()
}

fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

async fn add(harness: &Harness, filename: Option<&str>, connections: u32) -> String {
    let destination = harness.dir.path().join("output");
    let task = harness
        .engine
        .add(NewDownloadRequest {
            url: harness.server.url("/file.bin"),
            destination: destination.to_string_lossy().to_string(),
            filename: filename.map(str::to_string),
            connections: Some(connections),
            speed_limit: None,
            checksum_sha256: None,
            referrer: Some(harness.server.url("/page")),
            headers: Vec::new(),
            cookies: None,
            queue_id: None,
            priority: None,
            auto_start: None,
        })
        .await
        .expect("add");
    task.id
}

async fn wait_for(
    engine: &DownloadEngine,
    id: &str,
    predicate: impl Fn(&DownloadSnapshot) -> bool,
) -> DownloadSnapshot {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let snapshot = engine
            .list()
            .await
            .expect("list")
            .into_iter()
            .find(|snapshot| snapshot.task.id == id)
            .expect("task");
        if predicate(&snapshot) {
            return snapshot;
        }
        if Instant::now() > deadline {
            panic!(
                "timed out waiting for task; last status: {:?} error: {:?}",
                snapshot.task.status, snapshot.task.error
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_terminal(engine: &DownloadEngine, id: &str) -> DownloadSnapshot {
    wait_for(engine, id, |snapshot| {
        matches!(
            snapshot.task.status,
            DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Cancelled
        )
    })
    .await
}

#[tokio::test]
async fn single_stream_download_completes() {
    let payload = body(300 * 1024);
    let harness = harness(MockOptions {
        body: payload.clone(),
        support_ranges: false,
        ..Default::default()
    })
    .await;
    let id = add(&harness, Some("single.bin"), 4).await;
    harness.engine.start(&id).await.expect("start");
    let snapshot = wait_terminal(&harness.engine, &id).await;
    assert_eq!(
        snapshot.task.status,
        DownloadStatus::Completed,
        "{:?}",
        snapshot.task.error
    );
    assert_eq!(snapshot.task.total_size, Some(payload.len() as u64));
    assert_eq!(snapshot.segments.len(), 1);
    let data = std::fs::read(&snapshot.task.destination).expect("read output");
    assert_eq!(data, payload);
    assert_eq!(
        snapshot.task.checksum_actual.as_deref(),
        Some(sha256(&payload).as_str())
    );
}

#[tokio::test]
async fn multi_segment_download_uses_multiple_ranges() {
    let payload = body(5 * 1024 * 1024);
    let harness = harness(MockOptions {
        body: payload.clone(),
        support_ranges: true,
        ..Default::default()
    })
    .await;
    let id = add(&harness, Some("multi.bin"), 4).await;
    harness.engine.start(&id).await.expect("start");
    let snapshot = wait_terminal(&harness.engine, &id).await;
    assert_eq!(
        snapshot.task.status,
        DownloadStatus::Completed,
        "{:?}",
        snapshot.task.error
    );
    assert!(snapshot.task.supports_ranges);
    assert!(
        snapshot.segments.len() >= 2,
        "segments: {}",
        snapshot.segments.len()
    );
    assert_eq!(std::fs::read(&snapshot.task.destination).unwrap(), payload);
    assert!(harness.server.request_count() > snapshot.segments.len());
}

#[tokio::test]
async fn no_range_server_uses_one_stream() {
    let payload = body(2 * 1024 * 1024);
    let harness = harness(MockOptions {
        body: payload.clone(),
        support_ranges: false,
        ..Default::default()
    })
    .await;
    let id = add(&harness, Some("norange.bin"), 8).await;
    harness.engine.start(&id).await.expect("start");
    let snapshot = wait_terminal(&harness.engine, &id).await;
    assert_eq!(snapshot.task.status, DownloadStatus::Completed);
    assert!(!snapshot.task.supports_ranges);
    assert_eq!(snapshot.segments.len(), 1);
    assert_eq!(std::fs::read(&snapshot.task.destination).unwrap(), payload);
}

#[tokio::test]
async fn resumes_after_connection_drop_using_range() {
    let payload = body(2 * 1024 * 1024);
    let harness = harness(MockOptions {
        body: payload.clone(),
        support_ranges: true,
        drop_after: Some(100 * 1024),
        ..Default::default()
    })
    .await;
    let id = add(&harness, Some("resume.bin"), 2).await;
    harness.engine.start(&id).await.expect("start");
    let snapshot = wait_terminal(&harness.engine, &id).await;
    assert_eq!(
        snapshot.task.status,
        DownloadStatus::Completed,
        "{:?}",
        snapshot.task.error
    );
    assert_eq!(std::fs::read(&snapshot.task.destination).unwrap(), payload);
    let ranges = harness.server.range_requests();
    assert!(
        ranges
            .iter()
            .flatten()
            .any(|range| range != "bytes=0-0" && !range.starts_with("bytes=0-")),
        "expected a resumed range request, got {ranges:?}"
    );
}

#[tokio::test]
async fn pause_flushes_and_resume_continues() {
    let payload = body(8 * 1024 * 1024);
    let harness = harness(MockOptions {
        body: payload.clone(),
        support_ranges: true,
        chunk_delay: Some(Duration::from_millis(50)),
        ..Default::default()
    })
    .await;
    let id = add(&harness, Some("pause.bin"), 2).await;
    harness.engine.start(&id).await.expect("start");
    wait_for(&harness.engine, &id, |snapshot| {
        snapshot.task.downloaded > 100 * 1024
    })
    .await;
    harness.engine.pause(&id).await.expect("pause");
    let paused = harness
        .engine
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|snapshot| snapshot.task.id == id)
        .unwrap();
    assert_eq!(paused.task.status, DownloadStatus::Paused);
    let requests_after_pause = harness.server.request_count();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        harness.server.request_count(),
        requests_after_pause,
        "no requests should be made while paused"
    );
    harness.engine.resume(&id).await.expect("resume");
    let snapshot = wait_terminal(&harness.engine, &id).await;
    assert_eq!(
        snapshot.task.status,
        DownloadStatus::Completed,
        "{:?}",
        snapshot.task.error
    );
    assert_eq!(std::fs::read(&snapshot.task.destination).unwrap(), payload);
}

#[tokio::test]
async fn cancel_stops_network_and_marks_cancelled() {
    let payload = body(8 * 1024 * 1024);
    let harness = harness(MockOptions {
        body: payload,
        support_ranges: true,
        chunk_delay: Some(Duration::from_millis(50)),
        ..Default::default()
    })
    .await;
    let id = add(&harness, Some("cancel.bin"), 2).await;
    harness.engine.start(&id).await.expect("start");
    wait_for(&harness.engine, &id, |snapshot| {
        snapshot.task.downloaded > 50 * 1024
    })
    .await;
    harness.engine.cancel(&id).await.expect("cancel");
    let snapshot = harness
        .engine
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|snapshot| snapshot.task.id == id)
        .unwrap();
    assert_eq!(snapshot.task.status, DownloadStatus::Cancelled);
    let requests = harness.server.request_count();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(harness.server.request_count(), requests);
}

#[tokio::test]
async fn http_404_marks_task_failed() {
    let harness = harness(MockOptions {
        status_override: Some(404),
        ..Default::default()
    })
    .await;
    let id = add(&harness, Some("missing.bin"), 4).await;
    harness.engine.start(&id).await.expect("start");
    let snapshot = wait_terminal(&harness.engine, &id).await;
    assert_eq!(snapshot.task.status, DownloadStatus::Failed);
    assert!(snapshot.task.error.unwrap_or_default().contains("404"));
}

#[tokio::test]
async fn retries_server_errors_with_backoff() {
    let payload = body(512 * 1024);
    let harness = harness(MockOptions {
        body: payload.clone(),
        fail_first_status: Some(500),
        ..Default::default()
    })
    .await;
    let id = add(&harness, Some("retry.bin"), 1).await;
    harness.engine.start(&id).await.expect("start");
    let snapshot = wait_terminal(&harness.engine, &id).await;
    assert_eq!(
        snapshot.task.status,
        DownloadStatus::Completed,
        "{:?}",
        snapshot.task.error
    );
    assert_eq!(std::fs::read(&snapshot.task.destination).unwrap(), payload);
}

#[tokio::test]
async fn invalid_content_range_is_rejected() {
    let harness = harness(MockOptions {
        body: body(256 * 1024),
        support_ranges: true,
        invalid_content_range: true,
        ..Default::default()
    })
    .await;
    let id = add(&harness, Some("invalid.bin"), 2).await;
    harness.engine.start(&id).await.expect("start");
    let snapshot = wait_terminal(&harness.engine, &id).await;
    assert_eq!(snapshot.task.status, DownloadStatus::Failed);
    let error = snapshot.task.error.unwrap_or_default();
    assert!(
        error.to_lowercase().contains("content-range"),
        "error: {error}"
    );
}

#[tokio::test]
async fn checksum_mismatch_leaves_no_final_file() {
    let harness = harness(MockOptions {
        body: body(256 * 1024),
        support_ranges: false,
        ..Default::default()
    })
    .await;
    let destination = harness.dir.path().join("output");
    let task = harness
        .engine
        .add(NewDownloadRequest {
            url: harness.server.url("/file.bin"),
            destination: destination.to_string_lossy().to_string(),
            filename: Some("checksum.bin".into()),
            connections: Some(2),
            speed_limit: None,
            checksum_sha256: Some("00".repeat(32)),
            referrer: None,
            headers: Vec::new(),
            cookies: None,
            queue_id: None,
            priority: None,
            auto_start: None,
        })
        .await
        .expect("add");
    harness.engine.start(&task.id).await.expect("start");
    let snapshot = wait_terminal(&harness.engine, &task.id).await;
    assert_eq!(snapshot.task.status, DownloadStatus::Failed);
    assert!(snapshot
        .task
        .error
        .unwrap_or_default()
        .to_lowercase()
        .contains("checksum"));
    assert!(!std::path::Path::new(&snapshot.task.destination).exists());
}

#[tokio::test]
async fn checksum_match_completes() {
    let payload = body(128 * 1024);
    let harness = harness(MockOptions {
        body: payload.clone(),
        support_ranges: false,
        ..Default::default()
    })
    .await;
    let destination = harness.dir.path().join("output");
    let task = harness
        .engine
        .add(NewDownloadRequest {
            url: harness.server.url("/file.bin"),
            destination: destination.to_string_lossy().to_string(),
            filename: Some("checksum-ok.bin".into()),
            connections: Some(2),
            speed_limit: None,
            checksum_sha256: Some(sha256(&payload)),
            referrer: None,
            headers: Vec::new(),
            cookies: None,
            queue_id: None,
            priority: None,
            auto_start: None,
        })
        .await
        .expect("add");
    harness.engine.start(&task.id).await.expect("start");
    let snapshot = wait_terminal(&harness.engine, &task.id).await;
    assert_eq!(
        snapshot.task.status,
        DownloadStatus::Completed,
        "{:?}",
        snapshot.task.error
    );
    assert_eq!(
        snapshot.task.checksum_actual.as_deref(),
        Some(sha256(&payload).as_str())
    );
}

#[tokio::test]
async fn empty_file_completes() {
    let harness = harness(MockOptions {
        body: Vec::new(),
        support_ranges: true,
        ..Default::default()
    })
    .await;
    let id = add(&harness, Some("empty.bin"), 4).await;
    harness.engine.start(&id).await.expect("start");
    let snapshot = wait_terminal(&harness.engine, &id).await;
    assert_eq!(
        snapshot.task.status,
        DownloadStatus::Completed,
        "{:?}",
        snapshot.task.error
    );
    assert_eq!(snapshot.task.total_size, Some(0));
    assert_eq!(
        std::fs::metadata(&snapshot.task.destination).unwrap().len(),
        0
    );
}

#[tokio::test]
async fn content_disposition_sets_filename_when_not_explicit() {
    let payload = body(64 * 1024);
    let harness = harness(MockOptions {
        body: payload.clone(),
        content_disposition: Some("attachment; filename=\"server-name.bin\"".into()),
        ..Default::default()
    })
    .await;
    let id = add(&harness, None, 2).await;
    harness.engine.start(&id).await.expect("start");
    let snapshot = wait_terminal(&harness.engine, &id).await;
    assert_eq!(
        snapshot.task.status,
        DownloadStatus::Completed,
        "{:?}",
        snapshot.task.error
    );
    assert_eq!(snapshot.task.filename, "server-name.bin");
    assert_eq!(std::fs::read(&snapshot.task.destination).unwrap(), payload);
}

#[tokio::test]
async fn remove_deletes_task_and_file() {
    let payload = body(32 * 1024);
    let harness = harness(MockOptions {
        body: payload,
        ..Default::default()
    })
    .await;
    let id = add(&harness, Some("remove.bin"), 2).await;
    harness.engine.start(&id).await.expect("start");
    let snapshot = wait_terminal(&harness.engine, &id).await;
    assert_eq!(snapshot.task.status, DownloadStatus::Completed);
    harness.engine.remove(&id, true).await.expect("remove");
    assert!(harness.engine.list().await.unwrap().is_empty());
    assert!(!std::path::Path::new(&snapshot.task.destination).exists());
}

#[tokio::test]
async fn rejects_invalid_schemes() {
    let harness = harness(MockOptions::default()).await;
    let error = harness
        .engine
        .add(NewDownloadRequest {
            url: "file:///etc/passwd".into(),
            destination: harness.dir.path().to_string_lossy().to_string(),
            filename: None,
            connections: None,
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
        .unwrap_err();
    assert!(matches!(error, RdmError::UnsupportedScheme(_)));
}

#[tokio::test]
async fn prevents_two_workers_for_one_task() {
    let payload = body(8 * 1024 * 1024);
    let harness = harness(MockOptions {
        body: payload,
        support_ranges: true,
        chunk_delay: Some(Duration::from_millis(50)),
        ..Default::default()
    })
    .await;
    let id = add(&harness, Some("double.bin"), 2).await;
    harness.engine.start(&id).await.expect("start");
    let second = harness.engine.start(&id).await;
    assert!(second.is_err(), "second start should be rejected");
    harness.engine.cancel(&id).await.expect("cancel");
}
