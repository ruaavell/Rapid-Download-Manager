# Roadmap

## Delivered in 1.0.0
- Rust workspace with shared common, storage, engine, IPC, scheduler, browser, media crates.
- Segmented HTTP/HTTPS downloads with pause/resume/cancel and crash recovery.
- SQLite persistence with migrations, WAL, integrity recovery.
- Background daemon with authenticated Unix-socket IPC.
- Tauri 2 + React desktop UI.
- Native Messaging host in Rust.
- Queue, scheduler, bandwidth windows.
- Media probing for HLS/DASH/direct.
- Linux packaging (AppImage, DEB, RPM).

## Future work
- Full HLS/DASH capture with FFmpeg muxing.
- Browser extension store packaging.
- Signed release artifacts and CI.