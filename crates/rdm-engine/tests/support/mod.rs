#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

#[derive(Clone, Default)]
pub struct MockOptions {
    pub body: Vec<u8>,
    pub support_ranges: bool,
    pub drop_after: Option<usize>,
    pub fail_first_status: Option<u16>,
    pub status_override: Option<u16>,
    pub invalid_content_range: bool,
    pub chunk_delay: Option<Duration>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_disposition: Option<String>,
    pub redirect: bool,
}

#[derive(Debug, Clone)]
pub struct RequestRecord {
    pub method: String,
    pub path: String,
    pub range: Option<String>,
}

#[derive(Default)]
struct State {
    drop_used: bool,
    fail_used: bool,
    requests: Vec<RequestRecord>,
}

pub struct MockServer {
    pub base_url: String,
    pub requests: Arc<Mutex<Vec<RequestRecord>>>,
    handle: JoinHandle<()>,
}

impl MockServer {
    pub async fn start(options: MockOptions) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let address = listener.local_addr().expect("local addr");
        let state = Arc::new(Mutex::new(State::default()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let request_sink = requests.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let options = options.clone();
                let state = state.clone();
                let requests = request_sink.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, options, state, requests).await;
                });
            }
        });
        Self {
            base_url: format!("http://{address}"),
            requests,
            handle,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    pub fn range_requests(&self) -> Vec<Option<String>> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.range.clone())
            .collect()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    options: MockOptions,
    state: Arc<Mutex<State>>,
    requests: Arc<Mutex<Vec<RequestRecord>>>,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 64 * 1024 {
            return Ok(());
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let range = headers.get("range").cloned();
    let is_probe = range.as_deref() == Some("bytes=0-0");
    let record = RequestRecord {
        method: method.clone(),
        path: path.clone(),
        range: range.clone(),
    };
    requests.lock().unwrap().push(record.clone());
    state.lock().unwrap().requests.push(record);

    if options.redirect && path != "/final" {
        let response = "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(response.as_bytes()).await?;
        return stream.shutdown().await;
    }

    if let Some(status) = options.status_override {
        return send_simple(&mut stream, status, b"mock status").await;
    }

    if let Some(status) = options.fail_first_status {
        let should_fail = {
            let mut state = state.lock().unwrap();
            if !is_probe && !state.fail_used {
                state.fail_used = true;
                true
            } else {
                false
            }
        };
        if should_fail {
            return send_simple(&mut stream, status, b"temporary failure").await;
        }
    }

    let total = options.body.len() as u64;
    let parsed_range = range.as_deref().and_then(parse_range);
    let use_range = options.support_ranges && parsed_range.is_some();

    let (status, start, end, body) = if options.invalid_content_range && use_range {
        let start = 5u64;
        let end = 10u64.min(total.saturating_sub(1));
        let start_index = (start as usize).min(options.body.len());
        let end_index = ((end as usize) + 1).min(options.body.len());
        let slice = if start_index < end_index {
            options.body[start_index..end_index].to_vec()
        } else {
            Vec::new()
        };
        (206, start, end, slice)
    } else if use_range {
        let (start, requested_end) = parsed_range.unwrap();
        if total == 0 || start >= total {
            let head = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).await?;
            return stream.shutdown().await;
        }
        let end = requested_end.unwrap_or(total - 1).min(total - 1);
        if end < start {
            let head = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).await?;
            return stream.shutdown().await;
        }
        (
            206,
            start,
            end,
            options.body[start as usize..=end as usize].to_vec(),
        )
    } else {
        (200, 0, total.saturating_sub(1), options.body.clone())
    };

    let mut response_headers = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    response_headers.push_str(&format!("Content-Length: {}\r\n", body.len()));
    response_headers.push_str("Content-Type: application/octet-stream\r\n");
    response_headers.push_str("Connection: close\r\n");
    if options.support_ranges {
        response_headers.push_str("Accept-Ranges: bytes\r\n");
    }
    if status == 206 {
        response_headers.push_str(&format!("Content-Range: bytes {start}-{end}/{total}\r\n"));
    }
    if let Some(etag) = &options.etag {
        response_headers.push_str(&format!("ETag: {etag}\r\n"));
    }
    if let Some(last_modified) = &options.last_modified {
        response_headers.push_str(&format!("Last-Modified: {last_modified}\r\n"));
    }
    if let Some(disposition) = &options.content_disposition {
        response_headers.push_str(&format!("Content-Disposition: {disposition}\r\n"));
    }
    response_headers.push_str("\r\n");
    stream.write_all(response_headers.as_bytes()).await?;

    if method == "HEAD" {
        return stream.shutdown().await;
    }

    let should_drop = {
        let mut state = state.lock().unwrap();
        if !is_probe && options.drop_after.is_some() && !state.drop_used {
            state.drop_used = true;
            true
        } else {
            false
        }
    };
    let limit = if should_drop {
        options.drop_after.unwrap_or(0).min(body.len())
    } else {
        body.len()
    };

    let mut written = 0usize;
    while written < limit {
        let chunk_end = (written + 64 * 1024).min(limit);
        stream.write_all(&body[written..chunk_end]).await?;
        written = chunk_end;
        if let Some(delay) = options.chunk_delay {
            tokio::time::sleep(delay).await;
        }
    }
    if should_drop {
        stream.shutdown().await?;
    } else {
        let _ = stream.shutdown().await;
    }
    Ok(())
}

async fn send_simple(stream: &mut TcpStream, status: u16, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}

fn parse_range(value: &str) -> Option<(u64, Option<u64>)> {
    let value = value.trim().strip_prefix("bytes=")?;
    let (start, end) = value.split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = end.trim();
    let end = if end.is_empty() {
        None
    } else {
        Some(end.parse::<u64>().ok()?)
    };
    Some((start, end))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        302 => "Found",
        403 => "Forbidden",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Mock",
    }
}
