use rdm_core::{DownloadManager, DownloadStatus, NewDownload, TaskStore};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TOTAL: usize = 4 * 1024 * 1024;

fn payload() -> Vec<u8> {
    (0..TOTAL).map(|i| (i % 251) as u8).collect()
}

fn handle(mut stream: TcpStream, body: Arc<Vec<u8>>, truncate: bool) {
    let mut request = Vec::new();
    let mut buffer = [0u8; 4096];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&buffer[..n]),
        }
    }
    let text = String::from_utf8_lossy(&request);
    let range = text
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("range:"))
        .and_then(|line| line.split_once('='))
        .and_then(|(_, value)| value.split_once('-'))
        .and_then(|(start, end)| {
            let start = start.trim().parse::<usize>().ok()?;
            let end = end
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .parse::<usize>()
                .unwrap_or(TOTAL - 1);
            Some((start, end.min(TOTAL - 1)))
        });
    let (start, end) = range.unwrap_or((0, TOTAL - 1));
    if start > end {
        return;
    }
    let mut slice = &body[start..=end];
    if truncate && start == TOTAL / 4 {
        // Pretend the transfer ended cleanly while sending only half of part 2.
        slice = &slice[..slice.len() / 2];
    }
    let head = if range.is_some() {
        format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\n\
             Content-Disposition: attachment; filename=\"big.bin\"\r\n\
             Content-Range: bytes {start}-{end}/{TOTAL}\r\nContent-Length: {}\r\n\
             Accept-Ranges: bytes\r\nConnection: close\r\n\r\n",
            slice.len()
        )
    } else {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
             Content-Disposition: attachment; filename=\"big.bin\"\r\n\
             Content-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
            slice.len()
        )
    };
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    // Slow the stream down just enough for the test to observe live parts.
    for chunk in slice.chunks(8 * 1024) {
        if stream.write_all(chunk).is_err() {
            return;
        }
        let _ = stream.flush();
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn serve(body: Arc<Vec<u8>>) -> String {
    serve_with(body, false)
}

fn serve_truncating(body: Arc<Vec<u8>>) -> String {
    serve_with(body, true)
}

fn serve_with(body: Arc<Vec<u8>>, truncate: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let body = body.clone();
            std::thread::spawn(move || handle(stream, body, truncate));
        }
    });
    format!("http://{addr}/big.bin")
}

#[tokio::test]
async fn speed_limit_can_be_applied_and_removed_while_running() {
    let body = Arc::new(payload());
    let url = serve(body.clone());
    let dir = std::env::temp_dir().join(format!("rdm-speed-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Downloads")).unwrap();
    let manager = DownloadManager::new(TaskStore::open(dir.join("db.sqlite3")).unwrap()).unwrap();

    let task = manager
        .add(NewDownload {
            url,
            destination: dir.join("Downloads").to_string_lossy().to_string(),
            filename: None,
            speed_limit: None,
            referrer: None,
            cookies: None,
        })
        .await
        .unwrap();
    manager.start(&task.id).await.unwrap();
    manager
        .set_speed_limit(&task.id, Some(100 * 1024))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(1500)).await;
    let throttled = manager
        .list()
        .await
        .into_iter()
        .find(|item| item.id == task.id)
        .unwrap();
    assert_eq!(throttled.status, DownloadStatus::Downloading);
    assert_eq!(throttled.speed_limit, Some(100 * 1024));
    assert!(
        throttled.downloaded < (TOTAL as u64) / 2,
        "limit did not throttle the running download: {} bytes",
        throttled.downloaded
    );

    manager.set_speed_limit(&task.id, None).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let current = manager
            .list()
            .await
            .into_iter()
            .find(|item| item.id == task.id)
            .unwrap();
        if matches!(
            current.status,
            DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Cancelled
        ) {
            assert_eq!(current.status, DownloadStatus::Completed, "{:?}", current.error);
            break;
        }
        assert!(Instant::now() < deadline, "download did not finish after removing the limit");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let saved = std::fs::read(dir.join("Downloads").join("big.bin")).unwrap();
    assert_eq!(saved, *body);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn short_part_fails_verification_and_keeps_parts_for_resume() {
    let body = Arc::new(payload());
    let url = serve_truncating(body);
    let dir = std::env::temp_dir().join(format!("rdm-verify-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Downloads")).unwrap();
    let manager = DownloadManager::new(TaskStore::open(dir.join("db.sqlite3")).unwrap()).unwrap();

    let task = manager
        .add(NewDownload {
            url,
            destination: dir.join("Downloads").to_string_lossy().to_string(),
            filename: None,
            speed_limit: None,
            referrer: None,
            cookies: None,
        })
        .await
        .unwrap();
    manager.start(&task.id).await.unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    let finished = loop {
        let current = manager
            .list()
            .await
            .into_iter()
            .find(|item| item.id == task.id)
            .unwrap();
        if matches!(
            current.status,
            DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Cancelled
        ) {
            break current;
        }
        assert!(Instant::now() < deadline, "download did not finish in time");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert_eq!(finished.status, DownloadStatus::Failed);
    assert!(
        finished
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Part 2 verification failed"),
        "unexpected error: {:?}",
        finished.error
    );
    assert!(!dir.join("Downloads").join("big.bin").exists());
    assert!(
        dir.join("Downloads").join("big.bin.rdm.part.seg1").exists(),
        "completed parts should be kept for a resume"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn segmented_download_reports_live_part_progress() {
    let body = Arc::new(payload());
    let url = serve(body.clone());
    let dir = std::env::temp_dir().join(format!("rdm-segment-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Downloads")).unwrap();
    let manager = DownloadManager::new(TaskStore::open(dir.join("db.sqlite3")).unwrap()).unwrap();

    let task = manager
        .add(NewDownload {
            url,
            destination: dir.join("Downloads").to_string_lossy().to_string(),
            filename: None,
            speed_limit: None,
            referrer: None,
            cookies: None,
        })
        .await
        .unwrap();
    manager.start(&task.id).await.unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut saw_live_parts = false;
    let finished = loop {
        let segments = manager.segment_progress(&task.id).await;
        if segments.len() == 4 {
            assert_eq!(segments[0].start, 0);
            assert_eq!(segments[3].end as usize, TOTAL - 1);
            assert_eq!(segments.iter().map(|part| part.total).sum::<u64>(), TOTAL as u64);
            if segments
                .iter()
                .any(|part| part.downloaded > 0 && !part.complete)
            {
                saw_live_parts = true;
            }
        }
        let current = manager
            .list()
            .await
            .into_iter()
            .find(|item| item.id == task.id)
            .unwrap();
        if matches!(
            current.status,
            DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Cancelled
        ) {
            break current;
        }
        assert!(Instant::now() < deadline, "download did not finish in time");
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    assert_eq!(finished.status, DownloadStatus::Completed, "{:?}", finished.error);
    assert!(saw_live_parts, "segment progress was never observed mid-download");
    let saved = std::fs::read(dir.join("Downloads").join("big.bin")).unwrap();
    assert_eq!(saved, *body);
    let _ = std::fs::remove_dir_all(&dir);
}
