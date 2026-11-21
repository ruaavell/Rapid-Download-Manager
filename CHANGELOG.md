# Changelog

## [1.0.0] - 2026-11-21

### Added
- Segmented HTTP/HTTPS download engine with range probing and per-segment persistence.
- SQLite storage with versioned migrations, WAL mode, crash recovery.
- Background daemon with authenticated Unix-socket IPC.
- CLI rebuilt on the new engine.
- Native Messaging host implemented in Rust.
- Media probing module (HLS/DASH/direct) with job tracking.
- Queue and time-based scheduler with bandwidth windows.
- Linux packaging via Tauri (AppImage, DEB, RPM).

### Changed
- Desktop UI upgraded to Tauri 2 bundle targets.
- Documentation updated for installation and security.

### Fixed
- Compilation and clippy warnings across the workspace.
- Scheduler destructive-schedule handling.