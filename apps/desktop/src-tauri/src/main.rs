use rdm_core::{DownloadManager, DownloadTask, NewDownload, TaskStore};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex;

struct PendingDownload {
    id: String,
    url: String,
    referrer: Option<String>,
    filename: String,
    size: Option<u64>,
    content_type: Option<String>,
}

#[derive(Serialize, Clone)]
struct PendingPayload {
    id: String,
    url: String,
    referrer: Option<String>,
    filename: String,
    size: Option<u64>,
    content_type: Option<String>,
}

struct AppState {
    manager: Arc<DownloadManager>,
    pending: Arc<Mutex<Vec<PendingDownload>>>,
}

#[derive(Deserialize)]
struct AddPayload {
    url: String,
    destination: String,
    filename: Option<String>,
    speed_limit: Option<u64>,
}

#[derive(Deserialize)]
struct ConfirmPayload {
    id: String,
    destination: String,
    filename: Option<String>,
}

#[tauri::command]
async fn default_download_dir() -> Result<String, String> {
    let home = dirs::home_dir().ok_or_else(|| "HOME directory is not available".to_string())?;
    let downloads = home.join("Downloads");
    std::fs::create_dir_all(&downloads).map_err(|e| e.to_string())?;
    Ok(downloads.to_string_lossy().to_string())
}

#[tauri::command]
async fn probe_url(url: String) -> Result<(Option<u64>, Option<String>, String), String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("RDM/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.head(&url).send().await.map_err(|e| e.to_string())?;
    let size = resp.content_length();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let filename = url::Url::parse(&url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut s| s.next_back())
                .map(str::to_owned)
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "download.bin".to_string());
    Ok((size, content_type, filename))
}

#[tauri::command]
async fn import_browser_inbox(state: State<'_, AppState>) -> Result<usize, String> {
    let data_home = dirs::data_local_dir().ok_or_else(|| "Could not resolve data dir".to_string())?;
    let inbox = data_home.join("rdm").join("browser-inbox.jsonl");
    let content = match std::fs::read_to_string(&inbox) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.to_string()),
    };

    let mut imported = 0usize;
    let mut remaining = Vec::new();
    let client = reqwest::Client::builder()
        .user_agent(concat!("RDM/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| e.to_string())?;

    for line in content.lines() {
        let parsed: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                remaining.push(line.to_string());
                continue;
            }
        };
        let Some(url) = parsed.get("url").and_then(|v| v.as_str()) else {
            remaining.push(line.to_string());
            continue;
        };
        let referrer = parsed.get("referrer").and_then(|v| v.as_str()).map(|s| s.to_string());

        // probe metadata
        let (size, content_type, filename) = match client.head(url).send().await {
            Ok(resp) => {
                let size = resp.content_length();
                let ct = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                let name = url::Url::parse(url)
                    .ok()
                    .and_then(|u| u.path_segments().and_then(|mut s| s.next_back()).map(str::to_owned))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "download.bin".to_string());
                (size, ct, name)
            }
            Err(_) => {
                let name = url::Url::parse(url)
                    .ok()
                    .and_then(|u| u.path_segments().and_then(|mut s| s.next_back()).map(str::to_owned))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "download.bin".to_string());
                (None, None, name)
            }
        };

        let pending = PendingDownload {
            id: uuid::Uuid::new_v4().to_string(),
            url: url.to_string(),
            referrer,
            filename,
            size,
            content_type,
        };
        state.pending.lock().await.push(pending);
        imported += 1;
    }

    if remaining.is_empty() {
        let _ = std::fs::remove_file(&inbox);
    } else {
        std::fs::write(&inbox, remaining.join("\n") + "\n").map_err(|e| e.to_string())?;
    }
    Ok(imported)
}

#[tauri::command]
async fn list_pending_downloads(state: State<'_, AppState>) -> Result<Vec<PendingPayload>, String> {
    let pending = state.pending.lock().await;
    Ok(pending
        .iter()
        .map(|p| PendingPayload {
            id: p.id.clone(),
            url: p.url.clone(),
            referrer: p.referrer.clone(),
            filename: p.filename.clone(),
            size: p.size,
            content_type: p.content_type.clone(),
        })
        .collect())
}

#[tauri::command]
async fn confirm_pending_download(
    payload: ConfirmPayload,
    state: State<'_, AppState>,
) -> Result<DownloadTask, String> {
    let pending = {
        let mut list = state.pending.lock().await;
        let idx = list.iter().position(|p| p.id == payload.id).ok_or_else(|| "Unknown pending".to_string())?;
        list.remove(idx)
    };
    let task = state
        .manager
        .add(NewDownload {
            url: pending.url,
            destination: payload.destination,
            filename: payload.filename.or(Some(pending.filename)),
            speed_limit: None,
        })
        .await
        .map_err(|e| e.to_string())?;
    state
        .manager
        .start(&task.id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(task)
}

#[tauri::command]
async fn dismiss_pending_download(id: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut list = state.pending.lock().await;
    list.retain(|p| p.id != id);
    Ok(())
}

#[tauri::command]
async fn list_downloads(state: State<'_, AppState>) -> Result<Vec<DownloadTask>, String> {
    Ok(state.manager.list().await)
}

#[tauri::command]
async fn add_download(payload: AddPayload, state: State<'_, AppState>) -> Result<DownloadTask, String> {
    let task = state
        .manager
        .add(NewDownload {
            url: payload.url,
            destination: payload.destination,
            filename: payload.filename,
            speed_limit: payload.speed_limit,
        })
        .await
        .map_err(|e| e.to_string())?;
    state.manager.start(&task.id).await.map_err(|e| e.to_string())?;
    Ok(task)
}

#[tauri::command]
async fn pause_download(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.manager.pause(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn resume_download(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.manager.resume(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn cancel_download(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.manager.cancel(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn remove_download(id: String, delete_file: bool, state: State<'_, AppState>) -> Result<(), String> {
    state
        .manager
        .remove(&id, delete_file)
        .await
        .map_err(|e| e.to_string())
}

fn main() {
    let store = TaskStore::open_default().expect("Could not initialize RDM database");
    let manager = Arc::new(DownloadManager::new(store).expect("Could not initialize download engine"));

    tauri::Builder::default()
        .manage(AppState {
            manager,
            pending: Arc::new(Mutex::new(Vec::new())),
        })
        .invoke_handler(tauri::generate_handler![
            list_downloads,
            default_download_dir,
            probe_url,
            import_browser_inbox,
            list_pending_downloads,
            confirm_pending_download,
            dismiss_pending_download,
            add_download,
            pause_download,
            resume_download,
            cancel_download,
            remove_download
        ])
        .run(tauri::generate_context!())
        .expect("error while running RDM");
}
