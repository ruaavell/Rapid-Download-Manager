# ⚡ RDM - Rapid Download Manager

<p align="center">
  <img width="180" alt="RDM Logo" src="https://github.com/user-attachments/assets/448ef0d1-dab2-42fa-a6a0-b9f5273343f7" />
</p>

<h3 align="center">A fast, native, and open-source download manager for Linux.</h3>

<p align="center">
  Built with Rust. Designed for Linux. Made for speed.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Version-1.0.0-2563EB?style=for-the-badge" />
  <img src="https://img.shields.io/badge/Language-Rust-orange?style=for-the-badge&logo=rust" />
  <img src="https://img.shields.io/badge/Desktop-Tauri%202-blue?style=for-the-badge&logo=tauri" />
  <img src="https://img.shields.io/badge/Platform-Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black" />
  <img src="https://img.shields.io/badge/License-GPL--3.0-green?style=for-the-badge" />
</p>

---

## 🚀 Overview
<img width="1172" height="809" alt="Ekran Görüntüsü 2026-11-22 18-10-24" src="https://github.com/user-attachments/assets/b21bd668-8a79-497d-aa11-108ac9e70c44" />

**RDM (Rapid Download Manager)** is an open-source, Linux-native download manager written in Rust.

Designed to provide a modern download management experience, RDM combines a high-performance Rust download engine with a lightweight desktop interface and persistent task management.

Unlike solutions that rely on compatibility layers or heavyweight runtimes, RDM is built specifically for Linux.

The project uses:

- **Rust** for the download engine and core functionality.
- **Tauri 2** for the native desktop application.
- **React + TypeScript** for the user interface.
- **SQLite** for persistent download management.
- **Native Messaging** for browser integration.

RDM aims to bring a reliable, extensible, and native download management experience to the Linux desktop.

## ✨ Features

### 📥 Download Management

- HTTP and HTTPS download support.
- Pause, resume, and cancel downloads.
- HTTP Range-based download resumption when supported by the server.
- Returned byte-offset validation during resume.
- Persistent download tasks using SQLite.
- Download queue and task status tracking.
- Per-download speed limits.
- Automatic filename detection.
- Support for custom download destinations.
- Partial download file management.

### 🌐 Browser Integration

- Native Messaging architecture.
- Chromium and Firefox extension support.
- Send download links directly from your browser.
- Page referrer forwarding.
- No unnecessary browser credential collection.

### ⚙️ Performance & Architecture

- Rust-powered download engine.
- Asynchronous HTTP networking.
- Lightweight Tauri desktop runtime.
- SQLite-backed task persistence.
- Native Linux integration.
- CLI download support.

### 🔒 Privacy & Security

- No telemetry.
- No automatic execution of downloaded files.
- No TCP listener.
- Browser communication through Native Messaging.
- HTTP and HTTPS URL scheme validation.
- Native host message-size validation.

## 🖥️ Technology Stack

| Component | Technology |
|---|---|
| Core Engine | Rust |
| Desktop Framework | Tauri 2 |
| Frontend | React + TypeScript |
| Database | SQLite |
| HTTP Client | Reqwest |
| Browser Integration | Native Messaging |
| Build System | Cargo + npm |

## 📦 Requirements

Before building RDM, make sure you have:

- Rust stable
- Cargo
- Node.js 20+
- npm
- GTK3
- WebKitGTK 4.1
- Linux build tools

### Arch Linux

Install the required dependencies:

```bash
sudo pacman -Syu

sudo pacman -S --needed \
  base-devel \
  curl \
  wget \
  file \
  openssl \
  gtk3 \
  webkit2gtk-4.1 \
  librsvg \
  libayatana-appindicator \
  nodejs \
  npm \
  rust
```

If `libayatana-appindicator` is unavailable in your repository configuration, check the current Tauri Linux prerequisites for the equivalent dependency.

## 🛠️ Installation & Build

### 1. Clone the repository

```bash
git clone https://github.com/ruaavell/RDM.git
cd RDM
```

### 2. Install frontend dependencies

```bash
npm install
```

### 3. Run in development mode

```bash
npm run tauri dev
```

### 4. Build the desktop application

```bash
npm run tauri build
```

The generated application bundles will be available in the Tauri build output directory.

### 5. Build and test the Rust workspace

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo build --workspace
```

## 💻 CLI Usage

RDM also provides a command-line interface.

Download a file directly:

```bash
cargo run -p rdm-cli -- \
  "https://example.com/archive.zip" \
  --output "$HOME/Downloads/archive.zip"
```

This allows RDM's download functionality to be used without opening the desktop interface.

## 🌍 Browser Extension

RDM includes a browser integration based on the Native Messaging API.

### Setup

1. Build or install the RDM desktop application.
2. Build or configure the Native Messaging host.
3. Open `browser-extension/native-host/rdm_native_host.py`.
4. Configure the absolute path to the installed `rdm-native-host` executable.
5. Register the Native Messaging manifest for your browser.
6. Load the `browser-extension/` directory as an unpacked extension.

Supported browser families:

- Chromium-based browsers
- Mozilla Firefox

Once configured, you can send links directly to RDM through the browser context menu.

**Privacy note:** The current implementation forwards the download URL and page referrer. It does not collect all browser cookies or credentials.

## 🏗️ Architecture

RDM follows a modular architecture that separates the desktop interface, download engine, database, and browser integration.

```text
                  ┌──────────────────────┐
                  │    React / TypeScript│
                  │       Frontend       │
                  └──────────┬───────────┘
                             │
                  ┌──────────▼───────────┐
                  │       Tauri 2        │
                  │    Desktop Shell     │
                  └──────────┬───────────┘
                             │
                  ┌──────────▼───────────┐
                  │      RDM Core        │
                  │        Rust          │
                  └──────────┬───────────┘
                             │
             ┌───────────────┼────────────────┐
             │               │                │
      ┌──────▼─────┐  ┌──────▼──────┐  ┌──────▼──────┐
      │   SQLite   │  │   Reqwest   │  │ File System │
      │ Task Store │  │ HTTP Engine │  │ .rdm.part   │
      └────────────┘  └─────────────┘  └─────────────┘

                  Browser Integration
                             │
                  ┌──────────▼───────────┐
                  │  Browser Extension   │
                  └──────────┬───────────┘
                             │
                  ┌──────────▼───────────┐
                  │  Native Messaging    │
                  │        Host          │
                  └──────────┬───────────┘
                             │
                  ┌──────────▼───────────┐
                  │    Local Handoff     │
                  └──────────────────────┘
```

## 📁 Data Storage

RDM stores its application data in standard Linux user directories.

| Data | Location |
|---|---|
| SQLite Database | `$XDG_DATA_HOME/rdm/rdm.sqlite3` |
| Fallback Database | `~/.local/share/rdm/rdm.sqlite3` |
| Downloads | User-selected directory |
| Temporary Files | `filename.rdm.part` |

RDM follows the XDG directory conventions for Linux desktop applications.

## 🔐 Security

Security and user privacy are important parts of RDM's design.

Current security characteristics:

- No TCP listener.
- Native Messaging communication through stdin/stdout.
- Native host message-size validation.
- HTTP and HTTPS schemes only.
- No automatic execution of downloaded files.
- No telemetry or background data collection.
- Browser credentials are not harvested.

## 🗺️ Roadmap

RDM is designed to grow into a complete Linux download management platform.

Potential development directions:

- [ ] Multi-connection downloads.
- [ ] Advanced download scheduling.
- [ ] Download categories and filters.
- [ ] Global download speed limits.
- [ ] Browser integration improvements.
- [ ] Download history and statistics.
- [ ] Checksum verification.
- [ ] Torrent support.
- [ ] Media download integration.
- [ ] FFmpeg-based media processing.
- [ ] Desktop notifications.
- [ ] More Linux package formats.

*Roadmap items are planned development directions, not features currently included in version 1.0.0.*

## 🤝 Contributing

Contributions, bug reports, feature requests, and suggestions are welcome!

If you want to contribute:

1. Fork the repository.
2. Create a feature branch.
3. Make your changes.
4. Run the Rust tests.
5. Submit a pull request.

Please keep contributions focused, documented, and compatible with the Linux-first philosophy of RDM.

## 📜 License

RDM is licensed under the **GNU General Public License v3.0 or later (GPL-3.0-or-later)**.

See the [LICENSE](LICENSE) file for details.

---

<p align="center">
  <b>RDM - Rapid Download Manager</b>
  <br />
  Built with ❤️ and Rust for Linux. By Ruavell
</p>
