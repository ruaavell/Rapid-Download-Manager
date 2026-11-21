use rdm_common::{
    AppStatus, BrowserInboxEntry, Category, DownloadDetails, DownloadSnapshot, ExportBundle,
    HistoryEntry, ImportResult, MediaDownloadRequest, MediaInfo, MediaJobStatus,
    NewDownloadRequest, Queue, QueueStatus, RdmError, Schedule, Settings,
};
use rdm_engine::EngineEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bumped whenever the wire format changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

/// First line sent by a client. It is never accepted after the handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientHello {
    pub version: u32,
    pub token: String,
    #[serde(default)]
    pub client: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerHello {
    pub version: u32,
    pub pid: u32,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One request envelope. `id` is echoed by the server so pipelined clients can
/// match responses, even though the shipped client is strictly sequential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub command: Command,
}

impl Request {
    pub fn new(id: u64, command: Command) -> Self {
        Self { id, command }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcError>,
}

impl Response {
    pub fn success(id: u64, data: Value) -> Self {
        Self {
            id,
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn failure(id: u64, error: IpcError) -> Self {
        Self {
            id,
            ok: false,
            data: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
}

impl IpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn from_rdm(error: &RdmError) -> Self {
        let code = match error {
            RdmError::NotFound(_) => "not_found",
            RdmError::InvalidState(_) => "invalid_state",
            RdmError::Permission(_) => "permission_denied",
            RdmError::InvalidUrl(_)
            | RdmError::UnsupportedScheme(_)
            | RdmError::EmbeddedCredentials
            | RdmError::InvalidFilename(_)
            | RdmError::PathTraversal(_)
            | RdmError::InvalidHeader(_) => "invalid_request",
            RdmError::Cancelled => "cancelled",
            RdmError::Network(_) | RdmError::Http { .. } => "network",
            RdmError::Database(_) => "database",
            RdmError::DiskFull { .. } => "disk_full",
            _ => "internal",
        };
        Self::new(code, error.to_string())
    }

    /// Rebuild a typed error for callers that want to branch on the kind.
    pub fn into_rdm(self) -> RdmError {
        match self.code.as_str() {
            "not_found" => RdmError::NotFound(self.message),
            "invalid_state" => RdmError::InvalidState(self.message),
            "permission_denied" => RdmError::Permission(self.message),
            "invalid_request" => RdmError::InvalidUrl(self.message),
            "cancelled" => RdmError::Cancelled,
            "network" => RdmError::Network(self.message),
            "database" => RdmError::Database(self.message),
            "disk_full" => RdmError::Other(self.message),
            _ => RdmError::Other(self.message),
        }
    }
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for IpcError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenTarget {
    File,
    Folder,
    Reveal,
}

/// All daemon operations. Field names avoid `id` so that the envelope id stays
/// unambiguous in logs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    Ping,
    Status,

    ListDownloads,
    GetDownload {
        download_id: String,
    },
    AddDownload {
        request: NewDownloadRequest,
    },
    StartDownload {
        download_id: String,
    },
    PauseDownload {
        download_id: String,
    },
    ResumeDownload {
        download_id: String,
    },
    CancelDownload {
        download_id: String,
    },
    RemoveDownload {
        download_id: String,
        #[serde(default)]
        delete_file: bool,
    },
    PauseAll,
    ResumeAll,
    SetSpeedLimit {
        download_id: String,
        #[serde(default)]
        limit_bps: Option<u64>,
    },
    SetConnections {
        download_id: String,
        connections: u32,
    },
    SetDownloadQueue {
        download_id: String,
        #[serde(default)]
        queue_id: Option<String>,
    },
    SetPriority {
        download_id: String,
        priority: i32,
    },

    ListQueues,
    CreateQueue {
        queue: Queue,
    },
    UpdateQueue {
        queue: Queue,
    },
    DeleteQueue {
        queue_id: String,
    },
    SetQueueStatus {
        queue_id: String,
        status: QueueStatus,
    },

    ListCategories,
    CreateCategory {
        category: Category,
    },
    UpdateCategory {
        category: Category,
    },
    DeleteCategory {
        category_id: String,
    },

    ListSchedules,
    CreateSchedule {
        schedule: Schedule,
    },
    UpdateSchedule {
        schedule: Schedule,
    },
    DeleteSchedule {
        schedule_id: String,
    },

    GetSettings,
    SaveSettings {
        settings: Settings,
    },

    ListHistory {
        #[serde(default)]
        limit: usize,
        #[serde(default)]
        offset: usize,
    },
    ClearHistory,
    DeleteHistory {
        entry_id: i64,
    },

    ListBrowserInbox {
        #[serde(default = "default_inbox_limit")]
        limit: usize,
    },
    ApproveBrowserEntry {
        entry_id: i64,
        #[serde(default)]
        destination: Option<String>,
        #[serde(default)]
        filename: Option<String>,
        #[serde(default)]
        connections: Option<u32>,
    },
    DismissBrowserEntry {
        entry_id: i64,
    },
    ClearProcessedBrowserEntries,

    ExportBundle,
    ImportBundle {
        bundle: ExportBundle,
    },

    OpenPath {
        path: String,
        target: OpenTarget,
    },

    ProbeMedia {
        url: String,
    },
    StartMedia {
        request: MediaDownloadRequest,
    },
    MediaStatus {
        job_id: String,
    },
    ListMediaJobs,
    CancelMedia {
        job_id: String,
    },

    /// Transitions the connection into a server-sent event stream.
    Subscribe,
}

fn default_inbox_limit() -> usize {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    Download(EngineEvent),
    Media(MediaJobStatus),
    Queue(Queue),
    Schedule(Schedule),
    Log { level: String, message: String },
    Shutdown,
}

/// Convenience payloads so the client does not need to know every JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingInfo {
    pub version: String,
    pub pid: u32,
    pub protocol: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadList {
    pub downloads: Vec<DownloadSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadDetailsPayload {
    pub details: DownloadDetails,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueList {
    pub queues: Vec<Queue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryList {
    pub categories: Vec<Category>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleList {
    pub schedules: Vec<Schedule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryList {
    pub entries: Vec<HistoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserInboxList {
    pub entries: Vec<BrowserInboxEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfoPayload {
    pub info: MediaInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaJobList {
    pub jobs: Vec<MediaJobStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportPayload {
    pub result: ImportResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusPayload {
    pub status: AppStatus,
}
