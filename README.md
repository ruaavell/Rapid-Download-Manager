# RDM - Rapid Download Manager
<img width="1402" height="1122" alt="icon" src="https://github.com/user-attachments/assets/448ef0d1-dab2-42fa-a6a0-b9f5273343f7" />


RDM is an open-source, Linux-native download manager written in Rust. The project
uses a Rust download engine, SQLite persistence, a Tauri 2 desktop shell, a
React/TypeScript interface, and a browser Native Messaging bridge.

**RDM 1.0.0** is a Linux-native download manager with a Rust download engine,
SQLite persistence, a Tauri 2 desktop shell, and a browser Native Messaging bridge.

## Features in this release

- Native Linux desktop application (Tauri 2)
- HTTP and HTTPS downloads
- Resume using HTTP Range when supported, with returned byte-offset validation
- Pause, resume, cancel
- SQLite task persistence
- Queue list and task status
- Per-task speed limit
- Automatic filename extraction from URL / Content-Disposition
- Browser extension using Native Messaging
- CLI mode
- No Wine, Proton, or Electron

## Requirements

- Rust stable and Cargo
- Node.js 20+ and npm
- Tauri 2 Linux prerequisites: WebKitGTK 4.1, GTK3, build tools
- SQLite is bundled through rusqlite's `bundled` feature
- Optional: FFmpeg (future media milestone)

### Arch Linux

```bash
sudo pacman -Syu
sudo pacman -S --needed base-devel curl wget file openssl \
  gtk3 webkit2gtk-4.1 librsvg libayatana-appindicator \
  nodejs npm rust
```

If `libayatana-appindicator` is unavailable in your repository setup, consult
the current Tauri Linux prerequisites for the equivalent package.

## Build and run

```bash
npm install
npm run tauri dev
```

Build desktop bundle:

```bash
npm run tauri build
```

Build/test Rust workspace:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo build --workspace
```

CLI:

```bash
cargo run -p rdm-cli -- \
  "https://example.com/archive.zip" \
  --output "$HOME/Downloads/archive.zip"
```

## Browser extension

1. Build/install the desktop app or run its Native Messaging host manually.
2. Edit `browser-extension/native-host/rdm_native_host.py` and set the absolute
   path to the installed `rdm-native-host` executable.
3. Register the Native Messaging manifest for the browser (examples are in
   `browser-extension/native-host/manifests/`).
4. Load `browser-extension/` as an unpacked extension in Chromium or Firefox.
5. Use the extension context menu to send a link to RDM.

The extension only sends a URL and page referrer in this MVP. It deliberately
does not harvest all browser cookies or credentials.

## Data locations

- SQLite database: `$XDG_DATA_HOME/rdm/rdm.sqlite3` (fallback `~/.local/share/rdm`)
- Downloads: selected by the user
- Partial files: destination filename plus `.rdm.part`

## Security notes

- The app binds no TCP listener.
- Browser communication uses Native Messaging over stdin/stdout.
- The native host validates message size and URL scheme.
- Only HTTP and HTTPS URLs are accepted.
- No telemetry is included.
- Downloaded files are never executed automatically.

## Architecture

```text
React / Tauri UI
      |
 Tauri commands
      |
 RDM Core (Rust)
      |---------------- SQLite task store
      |---------------- Reqwest HTTP client
      |---------------- .rdm.part files
      |
 Native Messaging Host <--- Browser Extension
      |
      +---- launches/communicates with the desktop app through
            a local handoff file in the user's XDG runtime/data directory
```

## License

GPL-3.0-or-later. See `LICENSE`.
