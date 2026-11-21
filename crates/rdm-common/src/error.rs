use std::path::PathBuf;

/// Typed error used across RDM. Variants carry enough context for the UI and
/// for structured logs without leaking credentials.
#[derive(Debug, thiserror::Error)]
pub enum RdmError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    #[error("unsupported URL scheme '{0}': only http and https are allowed")]
    UnsupportedScheme(String),

    #[error("URLs containing embedded credentials are not accepted")]
    EmbeddedCredentials,

    #[error("invalid filename: {0}")]
    InvalidFilename(String),

    #[error("path '{0}' escapes the selected destination directory")]
    PathTraversal(PathBuf),

    #[error("invalid HTTP header: {0}")]
    InvalidHeader(String),

    #[error("invalid state: {0}")]
    InvalidState(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database error: {0}")]
    Database(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("HTTP {status}: {message}")]
    Http { status: u16, message: String },

    #[error("server does not support byte ranges")]
    RangeUnsupported,

    #[error("invalid Content-Range response: {0}")]
    InvalidContentRange(String),

    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("expected {expected} bytes but received {actual}")]
    SizeMismatch { expected: u64, actual: u64 },

    #[error("operation cancelled")]
    Cancelled,

    #[error("permission denied: {0}")]
    Permission(String),

    #[error("disk full while writing {path}: {source}")]
    DiskFull {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Other(String),
}

impl RdmError {
    pub fn database(err: impl std::fmt::Display) -> Self {
        Self::Database(err.to_string())
    }

    pub fn network(err: impl std::fmt::Display) -> Self {
        Self::Network(err.to_string())
    }

    pub fn other(err: impl std::fmt::Display) -> Self {
        Self::Other(err.to_string())
    }

    /// True when retrying the same request may succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Network(_) | Self::Io(_) => true,
            Self::Http { status, .. } => {
                *status == 408 || *status == 425 || *status == 429 || *status >= 500
            }
            _ => false,
        }
    }

    /// Convert an I/O error into a more actionable variant.
    pub fn from_io_at(path: impl Into<PathBuf>, err: std::io::Error) -> Self {
        let path = path.into();
        match err.kind() {
            std::io::ErrorKind::PermissionDenied => {
                Self::Permission(format!("{}: {err}", path.display()))
            }
            _ if err.raw_os_error() == Some(libc::ENOSPC) => Self::DiskFull { path, source: err },
            _ => Self::Io(err),
        }
    }
}

impl From<rusqlite::Error> for RdmError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Database(value.to_string())
    }
}

impl From<url::ParseError> for RdmError {
    fn from(value: url::ParseError) -> Self {
        Self::InvalidUrl(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, RdmError>;
