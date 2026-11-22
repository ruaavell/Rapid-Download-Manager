# Changelog

## [Unreleased]

### Fixed
- Downloads sent from the browser are now shown in the main window for confirmation (the dialog previously only existed in a window that was never opened).
- Files from Google Drive and similar hosts keep their real name (`Content-Disposition`, browser-provided name) instead of `download`.
- Google Drive's "can't scan for viruses" page is followed to the real file; any other web page served in place of an archive/media file now fails with a clear error instead of being saved as a corrupt file.
- Browser `Referer` and cookies are sent with the download requests (kept in memory only; the inbox file is owner-only and removed after import).
- Segmented downloads verify the server returned the requested byte range before appending it.
- File names supplied by the browser or UI are sanitized (no path traversal).
- The per-download speed limit entered in the UI is no longer ignored.
- Browser inbox import no longer loses entries appended while it is running, and drops malformed lines instead of retrying them forever.

### Changed
- Browser extension 1.0.6 sends the browser-resolved file name and cookies (new `cookies` permission). The prebuilt `browser-extension.crx` must be rebuilt for this to take effect.

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