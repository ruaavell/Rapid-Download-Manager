fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::default().app_manifest(
            tauri_build::AppManifest::new().commands(&[
                "open_folder",
                "set_autostart",
                "default_download_dir",
                "probe_url",
                "import_browser_inbox",
                "list_pending_downloads",
                "confirm_pending_download",
                "dismiss_pending_download",
                "list_downloads",
                "add_download",
                "pause_download",
                "set_speed_limit",
                "enqueue_download",
                "dequeue_download",
                "resume_download",
                "cancel_download",
                "remove_download",
            ]),
        ),
    )
    .expect("failed to run tauri build script");
}