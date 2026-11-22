use rdm_common::{parse_http_url, RdmPaths};
use rdm_storage::Storage;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Deserialize)]
struct IncomingMessage {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    referrer: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    headers: Option<Vec<rdm_common::HeaderEntry>>,
    #[serde(default)]
    cookies: Option<String>,
    #[serde(default)]
    ping: Option<bool>,
}

#[derive(Serialize)]
struct OutgoingMessage {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<i64>,
}

#[tokio::main]
async fn main() {
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let paths = RdmPaths::discover().expect("paths");
    let _ = paths.ensure();
    let storage = Storage::open(paths.database.clone()).ok();

    loop {
        let mut len_buf = [0u8; 4];
        if stdin.read_exact(&mut len_buf).await.is_err() {
            break;
        }
        let len = u32::from_ne_bytes(len_buf) as usize;
        if len == 0 || len > 1024 * 1024 {
            break;
        }
        let mut buf = vec![0u8; len];
        if stdin.read_exact(&mut buf).await.is_err() {
            break;
        }
        let response = handle_message(&buf, &storage, &paths);
        let encoded = serde_json::to_vec(&response).unwrap_or_default();
        let out_len = encoded.len() as u32;
        let _ = stdout.write_all(&out_len.to_ne_bytes()).await;
        let _ = stdout.write_all(&encoded).await;
        let _ = stdout.flush().await;
    }
}

fn handle_message(buf: &[u8], storage: &Option<Storage>, paths: &rdm_common::RdmPaths) -> OutgoingMessage {
    let parsed: IncomingMessage = match serde_json::from_slice(buf) {
        Ok(v) => v,
        Err(err) => {
            return OutgoingMessage {
                ok: false,
                message: Some(format!("invalid json: {err}")),
                id: None,
            }
        }
    };
    if parsed.ping.unwrap_or(false) {
        return OutgoingMessage {
            ok: true,
            message: Some("pong".into()),
            id: None,
        };
    }
    let Some(url) = parsed.url else {
        return OutgoingMessage {
            ok: false,
            message: Some("url is required".into()),
            id: None,
        };
    };
    if parse_http_url(&url).is_err() {
        return OutgoingMessage {
            ok: false,
            message: Some("invalid url".into()),
            id: None,
        };
    }
    // Append to the legacy JSONL inbox so the desktop UI (which reads
    // browser-inbox.jsonl) can import the download.
    //
    // The file name the browser resolved and the cookies for this link are
    // forwarded too; without them servers such as Google Drive answer with a
    // web page and the file ends up misnamed and corrupt. Cookies only ever
    // live in this owner-only file, which the desktop app deletes once read.
    let record = serde_json::json!({
        "url": url,
        "referrer": parsed.referrer,
        "filename": parsed.filename,
        "cookies": parsed.cookies,
    });
    if let Some(parent) = paths.inbox.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let _ = options.open(&paths.inbox).and_then(|mut f| {
        use std::io::Write;
        writeln!(f, "{}", record)
    });

    // Also register in SQLite browser inbox when storage is available.
    let storage = match storage {
        Some(s) => s,
        None => {
            return OutgoingMessage {
                ok: true,
                message: Some("queued".into()),
                id: None,
            }
        }
    };
    let headers = parsed.headers.unwrap_or_default();
    // Cookies are deliberately not copied into the SQLite inbox.
    match storage.add_browser_entry(
        &url,
        parsed.referrer.as_deref(),
        parsed.filename.as_deref(),
        &headers,
        None,
    ) {
        Ok(id) => OutgoingMessage {
            ok: true,
            message: Some("queued".into()),
            id: Some(id),
        },
        Err(err) => OutgoingMessage {
            ok: false,
            message: Some(err.to_string()),
            id: None,
        },
    }
}
