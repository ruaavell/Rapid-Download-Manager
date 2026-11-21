use rdm_core::{DownloadManager, DownloadStatus, DownloadTask, NewDownload, TaskStore};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{menu::{Menu, MenuItem}, tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent}, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
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
    speed_limit: Option<u64>,
}

#[tauri::command]
async fn set_autostart(enabled: bool) -> Result<(), String> {
    let home = dirs::home_dir().ok_or_else(|| "HOME directory is not available".to_string())?;
    let path = home.join(".config").join("autostart").join("rdm.desktop");
    if enabled {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let content = format!(
            "[Desktop Entry]\nType=Application\nName=RDM\nComment=Rapid Download Manager\nExec={}\nIcon=rdm\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        std::fs::write(&path, content).map_err(|e| e.to_string())?;
    } else {
        let _ = std::fs::remove_file(&path);
    }
    Ok(())
}

#[tauri::command]
async fn open_folder(path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    let folder = if p.is_dir() { p } else { p.parent().map(|x| x.to_path_buf()).unwrap_or(p) };
    std::process::Command::new("xdg-open")
        .arg(&folder)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
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

async fn import_browser_inbox_inner(state: &AppState) -> Result<usize, String> {
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
async fn import_browser_inbox(state: State<'_, AppState>) -> Result<usize, String> {
    import_browser_inbox_inner(state.inner()).await
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
    app: tauri::AppHandle,
) -> Result<DownloadTask, String> {
    let pending = {
        let mut list = state.pending.lock().await;
        let idx = list.iter().position(|p| p.id == payload.id).ok_or_else(|| "Unknown pending".to_string())?;
        list.remove(idx)
    };
    let dest = std::path::PathBuf::from(&payload.destination);
    if dest.extension().is_none() {
        let _ = std::fs::create_dir_all(&dest);
    }
    let task = state
        .manager
        .add(NewDownload {
            url: pending.url,
            destination: payload.destination,
            filename: payload.filename.or(Some(pending.filename)),
            speed_limit: payload.speed_limit,
        })
        .await
        .map_err(|e| e.to_string())?;
    state
        .manager
        .start(&task.id)
        .await
        .map_err(|e| e.to_string())?;
    show_progress_window(&app);
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
async fn add_download(payload: AddPayload, state: State<'_, AppState>, app: tauri::AppHandle) -> Result<DownloadTask, String> {
    let dest = std::path::PathBuf::from(&payload.destination);
    if dest.extension().is_none() {
        let _ = std::fs::create_dir_all(&dest);
    }
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
    show_progress_window(&app);
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

fn show_progress_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("progress") {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, "progress", WebviewUrl::App("index.html?window=progress".into()))
        .title("RDM Downloads")
        .inner_size(440.0, 360.0)
        .always_on_top(true)
        .resizable(true)
        .build();
}

fn main() {
    use fs2::FileExt;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};

    let data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("rdm");
    let _ = std::fs::create_dir_all(&data_dir);
    let lock_path = data_dir.join("rdm.lock");
    let socket_path = data_dir.join("rdm.sock");

    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .ok();
    if let Some(file) = &lock_file {
        if file.try_lock_exclusive().is_err() {
            // Another instance is already running: ask it to show its window.
            if let Ok(mut stream) = UnixStream::connect(&socket_path) {
                let _ = stream.write_all(b"show");
            }
            std::process::exit(0);
        }
    }
    // Keep lock_file alive for process lifetime.
    let _lock_file = lock_file;

    // Remove stale socket from a previous run.
    let _ = std::fs::remove_file(&socket_path);
    let socket_path_for_thread = socket_path.clone();
    let store = TaskStore::open_default().expect("Could not initialize RDM database");
    let manager = Arc::new(DownloadManager::new(store).expect("Could not initialize download engine"));

    tauri::Builder::default()
        .manage(AppState {
            manager,
            pending: Arc::new(Mutex::new(Vec::new())),
        })
        .setup(move |app| {
            // Single-instance socket: show window when another launch signals us.
            let handle = app.handle().clone();
            let sock = socket_path_for_thread.clone();
            std::thread::spawn(move || {
                use std::io::Read;
                if let Ok(listener) = UnixListener::bind(&sock) {
                    for incoming in listener.incoming() {
                        if let Ok(mut stream) = incoming {
                            let mut buf = [0u8; 16];
                            let _ = stream.read(&mut buf);
                            let app = handle.clone();
                            let app2 = app.clone();
                            let _ = app.run_on_main_thread(move || {
                                if let Some(window) = app2.get_webview_window("main") {
                                    let _ = window.show();
                                    let _ = window.set_focus();
                                }
                            });
                        }
                    }
                }
            });
            // System tray so the app can stay alive in the background.
            let quit_item = MenuItem::with_id(app, "quit", "Quit RDM", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit_item])?;
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().unwrap())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    if event.id().as_ref() == "quit" {
                        app.exit(0);
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            // Background poller: import browser inbox and surface the window
            // when a download needs confirmation, even if the UI is closed.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    let state = handle.state::<AppState>();
                    let _ = import_browser_inbox_inner(state.inner()).await;
                    let has_pending = !state.pending.lock().await.is_empty();
                    let has_active = state.manager.list().await.iter().any(|t| matches!(t.status, DownloadStatus::Queued | DownloadStatus::Downloading | DownloadStatus::Paused | DownloadStatus::Merging));
                    if has_active {
                        show_progress_window(&handle);
                    }
                    if has_pending {
                        if let Some(window) = handle.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                            let _ = window.emit("pending-downloads", ());
                        }
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Keep running in the tray instead of quitting.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            open_folder,
            set_autostart,
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
