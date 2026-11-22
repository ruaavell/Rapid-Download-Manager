use rdm_core::{DownloadManager, DownloadStatus, DownloadTask, NewDownload, TaskStore};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{menu::{Menu, MenuItem}, tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent}, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
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
    let mut size = resp.content_length();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    if size.is_none() {
        if let Ok(r2) = client.get(&url).header(reqwest::header::RANGE, "bytes=0-0").send().await {
            if let Some(cr) = r2.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()) {
                if let Some(total) = cr.rsplit('/').next().and_then(|s| s.trim().parse::<u64>().ok()) {
                    if total > 0 { size = Some(total); }
                }
            } else if let Some(cl) = r2.content_length() {
                if cl > 1 { size = Some(cl); }
            }
        }
    }
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
                let mut size = resp.content_length();
                let ct = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                if size.is_none() {
                    if let Ok(r2) = client.get(url).header(reqwest::header::RANGE, "bytes=0-0").send().await {
                        if let Some(cr) = r2.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()) {
                            if let Some(total) = cr.rsplit('/').next().and_then(|s| s.trim().parse::<u64>().ok()) {
                                if total > 0 { size = Some(total); }
                            }
                        } else if let Some(cl) = r2.content_length() {
                            if cl > 1 { size = Some(cl); }
                        }
                    }
                }
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

async fn find_resource(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|s| s.to_str()) == Some(name) {
                return Some(path);
            }
        }
    }
    None
}

async fn install_runtime_dependencies(
    resource_dir: Option<std::path::PathBuf>,
    manager: std::sync::Arc<rdm_core::DownloadManager>,
) {
    let _ = manager.ensure_ytdlp().await;

    let Some(resource_dir) = resource_dir else { return; };
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };
    let data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("rdm");
    let _ = tokio::fs::create_dir_all(&data_dir).await;

    let crx_src = find_resource(&resource_dir, "browser-extension.crx").await;
    let host_src = find_resource(&resource_dir, "rdm_native_host.py").await;

    let crx_dest = data_dir.join("browser-extension.crx");
    if let Some(src) = crx_src {
        let _ = tokio::fs::copy(src, &crx_dest).await;
    }
    let host_dest = home.join(".local").join("bin").join("rdm_native_host.py");
    if let Some(parent) = host_dest.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Some(src) = host_src {
        let _ = tokio::fs::copy(src, &host_dest).await;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = tokio::fs::metadata(&host_dest).await {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = tokio::fs::set_permissions(&host_dest, perms).await;
            }
        }
    }

    if !crx_dest.exists() || !host_dest.exists() {
        return;
    }

    let ext_id = "ddnikbhjakonmonnfeannoihpnpbcojf";
    for browser in ["chromium", "google-chrome"] {
        let base = home.join(".config").join(browser);
        let ext_dir = base.join("External Extensions");
        let _ = tokio::fs::create_dir_all(&ext_dir).await;
        let ext_json = serde_json::json!({
            "external_crx": crx_dest.to_string_lossy(),
            "external_version": "1.0.1"
        });
        let _ = tokio::fs::write(
            ext_dir.join(format!("{}.json", ext_id)),
            serde_json::to_string_pretty(&ext_json).unwrap_or_default(),
        )
        .await;

        let native_dir = base.join("NativeMessagingHosts");
        let _ = tokio::fs::create_dir_all(&native_dir).await;
        let native_json = serde_json::json!({
            "name": "io.github.ruaavell.rdm",
            "description": "RDM Native Messaging Host",
            "path": host_dest.to_string_lossy(),
            "type": "stdio",
            "allowed_origins": [
                format!("chrome-extension://{}/", ext_id),
                "chrome-extension://cihhnonjpjnnaabnjgkalnoonokkchaa/".to_string()
            ]
        });
        let _ = tokio::fs::write(
            native_dir.join("io.github.ruaavell.rdm.json"),
            serde_json::to_string_pretty(&native_json).unwrap_or_default(),
        )
        .await;
    }

    let ff_dir = home.join(".mozilla").join("native-messaging-hosts");
    let _ = tokio::fs::create_dir_all(&ff_dir).await;
    let ff_json = serde_json::json!({
        "name": "io.github.ruaavell.rdm",
        "description": "RDM Native Messaging Host",
        "path": host_dest.to_string_lossy(),
        "type": "stdio",
        "allowed_extensions": ["rdm-browser@example.org"]
    });
    let _ = tokio::fs::write(
        ff_dir.join("io.github.ruaavell.rdm.json"),
        serde_json::to_string_pretty(&ff_json).unwrap_or_default(),
    )
    .await;
}

fn main() {
    use fs2::FileExt;
    use std::io::Write;
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
        .truncate(false)
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
                    for stream in listener.incoming().flatten() {
                        let mut stream = stream;
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

            // Install runtime dependencies (yt-dlp, browser extension, native host)
            // on startup so end users do not have to set them up manually.
            let setup_manager = app.state::<AppState>().manager.clone();
            let setup_resource_dir = app.path().resource_dir().ok();
            tauri::async_runtime::spawn(async move {
                install_runtime_dependencies(setup_resource_dir, setup_manager).await;
            });

            // Background poller: import browser inbox and surface the window
            // when a download needs confirmation, even if the UI is closed.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut prev_pending = 0usize;
                let mut prev_active = 0usize;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    let state = handle.state::<AppState>();
                    let _ = import_browser_inbox_inner(state.inner()).await;
                    let pending_count = state.pending.lock().await.len();
                    let active_count = state.manager.list().await.iter().filter(|t| matches!(t.status, DownloadStatus::Queued | DownloadStatus::Downloading | DownloadStatus::Paused | DownloadStatus::Merging)).count();
                    // Keep the downloads UI in the main window; do not pop up a separate progress window.
                    let _ = (pending_count, active_count, prev_pending, prev_active);
                    prev_pending = pending_count;
                    prev_active = active_count;
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
