# Changelog

## [1.0.12] - 2026-11-22

### Fixed
- Reddit video posts now receive a Download button even when the player is lazy-loaded.
- X age-restricted videos try to recover the direct media URL from browser resource timing; yt-dlp uses the syndication API as fallback.

## [1.0.11] - 2026-11-22

### Fixed
- Reddit video players (shreddit-player) now get a Download button.
- Instagram button is attached to the player container and stays clickable above the overlay.

## [1.0.10] - 2026-11-22

### Changed
- yt-dlp now sends a browser-like user agent.

## [1.0.9] - 2026-11-22

### Fixed
- Video button now scans shadow DOM and rescans periodically for Reddit/X-style players.

## [1.0.8] - 2026-11-22

### Fixed
- yt-dlp requests now use canonical headers for cookies and referer.
- Browser video button resolves the post permalink on X/Twitter and Reddit feeds.

## [1.0.7] - 2026-11-22

### Added
- All-site media fallback: pages that return HTML are handed to yt-dlp instead of being saved as a useless file.
- Manual "Download as media" option in the Add URL dialog for sites not auto-detected.
- Browser session cookies and referer are now forwarded to yt-dlp.

### Fixed
- Reddit, X/Twitter and other extractor-backed sites are handled by yt-dlp with the page URL.

## [1.0.6] - 2026-11-22

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