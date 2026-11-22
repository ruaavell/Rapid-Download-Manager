use rdm_core::{DownloadManager, DownloadStatus, NewDownload, TaskStore};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

const BODY: &[u8] = b"Rar!\x1a\x07\x00fake-rar-bytes-for-rdm-test";

fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let signed_in = request
                .lines()
                .any(|line| line.to_ascii_lowercase().starts_with("cookie:") && line.contains("SID=abc"));
            let (content_type, disposition, body): (&str, Option<&str>, &[u8]) = if signed_in {
                (
                    "application/x-rar-compressed",
                    Some("attachment; filename=\"gta3.rar\""),
                    BODY,
                )
            } else {
                (
                    "text/html; charset=utf-8",
                    None,
                    b"<html><body>Sign in to continue</body></html>",
                )
            };
            let mut head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            if let Some(d) = disposition {
                head.push_str(&format!("Content-Disposition: {d}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }
    });
    format!("http://{addr}/download?id=1AbC&export=download")
}

#[tokio::test]
async fn cookies_resolve_real_name_and_bytes() {
    let url = serve();
    let dir = std::env::temp_dir().join(format!("rdm-cookie-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Downloads")).unwrap();
    let manager = DownloadManager::new(TaskStore::open(dir.join("db.sqlite3")).unwrap()).unwrap();

    // Without the browser session the server only offers a web page.
    let anonymous = manager.probe_metadata(&url, None, None).await;
    assert_eq!(anonymous.filename, None);

    // With the forwarded cookies it offers the real archive name.
    let signed_in = manager
        .probe_metadata(&url, Some("https://drive.google.com/"), Some("SID=abc"))
        .await;
    assert_eq!(signed_in.filename.as_deref(), Some("gta3.rar"));

    let task = manager
        .add(NewDownload {
            url: url.clone(),
            destination: dir.join("Downloads").to_string_lossy().to_string(),
            filename: None,
            speed_limit: None,
            referrer: Some("https://drive.google.com/".into()),
            cookies: Some("SID=abc".into()),
            media_kind: None,
            quality: None,
            media_page: None,
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
            .find(|t| t.id == task.id)
            .unwrap();
        if matches!(
            current.status,
            DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Cancelled
        ) {
            break current;
        }
        assert!(Instant::now() < deadline, "download did not finish in time");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    assert_eq!(finished.status, DownloadStatus::Completed, "{:?}", finished.error);
    assert_eq!(finished.filename, "gta3.rar");
    let saved = std::fs::read(dir.join("Downloads").join("gta3.rar")).unwrap();
    assert_eq!(saved, BODY);
    let _ = std::fs::remove_dir_all(&dir);
}
