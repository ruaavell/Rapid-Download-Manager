use crate::{RdmError, Result};
use serde::{Deserialize, Serialize};

/// Lifecycle state of a download. The database stores the snake_case string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Queued,
    Connecting,
    Downloading,
    Paused,
    Verifying,
    Completed,
    Failed,
    Cancelled,
}

impl DownloadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Connecting => "connecting",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Verifying => "verifying",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "connecting" => Self::Connecting,
            "downloading" => Self::Downloading,
            "paused" => Self::Paused,
            "verifying" => Self::Verifying,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Queued,
        }
    }

    /// A worker may be running in this state.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Connecting | Self::Downloading | Self::Verifying)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentStatus {
    Pending,
    Active,
    Complete,
    Failed,
}

impl SegmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "active" => Self::Active,
            "complete" => Self::Complete,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderEntry {
    pub name: String,
    pub value: String,
}

/// A persisted download task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTask {
    pub id: String,
    pub url: String,
    pub filename: String,
    /// Absolute path of the final file.
    pub destination: String,
    /// Directory selected by the user.
    pub directory: String,
    pub status: DownloadStatus,
    pub total_size: Option<u64>,
    pub downloaded: u64,
    pub connections: u32,
    pub speed_limit: Option<u64>,
    pub resumable: bool,
    pub supports_ranges: bool,
    pub mime_type: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub checksum_expected: Option<String>,
    pub checksum_actual: Option<String>,
    pub error: Option<String>,
    pub queue_id: Option<String>,
    pub priority: i32,
    pub referrer: Option<String>,
    pub filename_explicit: bool,
    pub retry_count: u32,
    pub max_retries: u32,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

impl DownloadTask {
    pub fn progress_fraction(&self) -> Option<f64> {
        match self.total_size {
            Some(total) if total > 0 => Some((self.downloaded as f64 / total as f64).min(1.0)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadSegment {
    pub id: i64,
    pub download_id: String,
    pub index: u32,
    pub start: u64,
    /// Inclusive end offset. `None` means the total size is unknown.
    pub end: Option<u64>,
    pub downloaded: u64,
    pub status: SegmentStatus,
    pub updated_at: i64,
}

impl DownloadSegment {
    pub fn expected_len(&self) -> Option<u64> {
        self.end.map(|end| end.saturating_sub(self.start) + 1)
    }

    pub fn is_complete(&self) -> bool {
        match self.expected_len() {
            Some(len) => self.downloaded >= len,
            None => false,
        }
    }
}

/// Runtime view of a task. Speed/ETA/connection counts are measured by the
/// engine and are never persisted or fabricated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadSnapshot {
    #[serde(flatten)]
    pub task: DownloadTask,
    pub speed_bps: u64,
    pub eta_seconds: Option<u64>,
    pub active_connections: u32,
    pub segments: Vec<DownloadSegment>,
}

impl From<DownloadTask> for DownloadSnapshot {
    fn from(task: DownloadTask) -> Self {
        Self {
            task,
            speed_bps: 0,
            eta_seconds: None,
            active_connections: 0,
            segments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadDetails {
    pub task: DownloadTask,
    pub segments: Vec<DownloadSegment>,
    pub headers: Vec<HeaderEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewDownloadRequest {
    pub url: String,
    /// Directory (preferred) or an explicit file path.
    pub destination: String,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub connections: Option<u32>,
    #[serde(default)]
    pub speed_limit: Option<u64>,
    #[serde(default)]
    pub checksum_sha256: Option<String>,
    #[serde(default)]
    pub referrer: Option<String>,
    #[serde(default)]
    pub headers: Vec<HeaderEntry>,
    /// Cookies are only accepted from an explicit, user-authorized browser flow.
    #[serde(default)]
    pub cookies: Option<String>,
    #[serde(default)]
    pub queue_id: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub auto_start: Option<bool>,
}

impl NewDownloadRequest {
    pub fn auto_start(&self) -> bool {
        self.auto_start.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueStatus {
    Running,
    Paused,
    Stopped,
}

impl QueueStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "paused" => Self::Paused,
            "stopped" => Self::Stopped,
            _ => Self::Running,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queue {
    pub id: String,
    pub name: String,
    pub max_concurrent: u32,
    pub default_connections: u32,
    pub speed_limit: Option<u64>,
    pub priority: i32,
    pub status: QueueStatus,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleAction {
    StartQueue,
    PauseQueue,
    ResumeAll,
    PauseAll,
    SetGlobalSpeedLimit,
    Shutdown,
    Suspend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatRule {
    Once,
    Hourly,
    Daily,
    Weekly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub action: ScheduleAction,
    pub target_id: Option<String>,
    /// Unix timestamp (seconds).
    pub run_at: i64,
    pub repeat_rule: RepeatRule,
    pub enabled: bool,
    /// Destructive actions require this to be true.
    pub confirmed: bool,
    pub last_run: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub id: String,
    pub name: String,
    pub extensions: Vec<String>,
    pub directory: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandwidthWindow {
    /// Minutes since local midnight, inclusive.
    pub start_minute: u32,
    /// Minutes since local midnight, exclusive. May wrap past midnight.
    pub end_minute: u32,
    pub limit_bps: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub default_download_dir: String,
    pub max_concurrent_downloads: u32,
    pub default_connections: u32,
    pub global_speed_limit: Option<u64>,
    pub auto_resume_on_start: bool,
    pub auto_retry: bool,
    pub max_retries: u32,
    pub clipboard_detection: bool,
    pub notifications: bool,
    pub theme: String,
    pub language: String,
    pub log_level: String,
    pub minimize_to_tray: bool,
    pub close_to_tray: bool,
    pub browser_auto_capture: bool,
    pub browser_confirm: bool,
    pub browser_file_types: Vec<String>,
    pub allow_browser_cookies: bool,
    pub bandwidth_schedule: Vec<BandwidthWindow>,
    pub completed_notification: bool,
    pub failed_notification: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_download_dir: String::new(),
            max_concurrent_downloads: 4,
            default_connections: 8,
            global_speed_limit: None,
            auto_resume_on_start: true,
            auto_retry: true,
            max_retries: 3,
            clipboard_detection: false,
            notifications: true,
            theme: "dark".into(),
            language: "en".into(),
            log_level: "info".into(),
            minimize_to_tray: true,
            close_to_tray: true,
            browser_auto_capture: false,
            browser_confirm: true,
            browser_file_types: vec![
                "zip".into(),
                "7z".into(),
                "rar".into(),
                "iso".into(),
                "exe".into(),
                "msi".into(),
                "mp4".into(),
                "mkv".into(),
                "mp3".into(),
                "pdf".into(),
            ],
            allow_browser_cookies: false,
            bandwidth_schedule: Vec::new(),
            completed_notification: true,
            failed_notification: true,
        }
    }
}

impl Settings {
    /// Reject values that would make the engine behave unpredictably.
    pub fn validate(&self) -> Result<()> {
        if !(1..=32).contains(&self.max_concurrent_downloads) {
            return Err(RdmError::InvalidState(
                "max_concurrent_downloads must be between 1 and 32".into(),
            ));
        }
        if !(1..=32).contains(&self.default_connections) {
            return Err(RdmError::InvalidState(
                "default_connections must be between 1 and 32".into(),
            ));
        }
        if self.global_speed_limit == Some(0) {
            return Err(RdmError::InvalidState(
                "global_speed_limit must be positive or null".into(),
            ));
        }
        if !matches!(self.theme.as_str(), "dark" | "light" | "system") {
            return Err(RdmError::InvalidState("invalid theme".into()));
        }
        if !matches!(self.language.as_str(), "en" | "tr") {
            return Err(RdmError::InvalidState("invalid language".into()));
        }
        if !matches!(
            self.log_level.as_str(),
            "trace" | "debug" | "info" | "warn" | "error"
        ) {
            return Err(RdmError::InvalidState("invalid log level".into()));
        }
        for window in &self.bandwidth_schedule {
            if window.start_minute >= 1440 || window.end_minute > 1440 || window.limit_bps == 0 {
                return Err(RdmError::InvalidState(
                    "invalid bandwidth schedule window".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub download_id: String,
    pub filename: String,
    pub url: String,
    pub destination: String,
    pub total_size: Option<u64>,
    pub status: String,
    pub checksum_sha256: Option<String>,
    pub completed_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserInboxEntry {
    pub id: i64,
    pub url: String,
    pub referrer: Option<String>,
    pub filename: Option<String>,
    pub headers: Vec<HeaderEntry>,
    pub cookies: Option<String>,
    pub created_at: i64,
    pub processed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStatus {
    pub version: String,
    pub daemon_pid: u32,
    pub uptime_seconds: u64,
    pub socket_path: String,
    pub database_path: String,
    pub active_downloads: usize,
    pub queued_downloads: usize,
    pub completed_downloads: usize,
    pub failed_downloads: usize,
    pub global_speed_bps: u64,
    pub total_downloaded_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Hls,
    Dash,
    Direct,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaStream {
    pub id: String,
    pub kind: String,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub bandwidth: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub kind: MediaKind,
    pub url: String,
    pub title: Option<String>,
    pub duration_seconds: Option<f64>,
    pub streams: Vec<MediaStream>,
    pub subtitles: Vec<MediaStream>,
    pub drm_protected: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaDownloadRequest {
    pub url: String,
    pub destination: String,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub max_height: Option<u32>,
    #[serde(default)]
    pub audio_language: Option<String>,
    #[serde(default)]
    pub include_subtitles: bool,
    #[serde(default)]
    pub connections: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaJobStatus {
    pub id: String,
    pub url: String,
    pub phase: String,
    pub downloaded_segments: u64,
    pub total_segments: Option<u64>,
    pub downloaded_bytes: u64,
    pub percent: f64,
    pub message: String,
    pub output_path: Option<String>,
    pub error: Option<String>,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportDownload {
    pub url: String,
    pub filename: String,
    pub destination: String,
    pub connections: u32,
    pub speed_limit: Option<u64>,
    pub checksum_sha256: Option<String>,
    pub referrer: Option<String>,
    pub headers: Vec<HeaderEntry>,
    pub queue_id: Option<String>,
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportBundle {
    pub version: String,
    pub exported_at: i64,
    pub downloads: Vec<ExportDownload>,
    pub queues: Vec<Queue>,
    pub categories: Vec<Category>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub imported: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}
