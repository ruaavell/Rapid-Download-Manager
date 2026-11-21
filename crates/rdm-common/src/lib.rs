//! Shared primitives for RDM.
//!
//! This crate intentionally contains no networking or database code so that the
//! engine, storage, daemon, CLI, desktop application and browser host all agree
//! on the same data model and security rules.

pub mod error;
pub mod format;
pub mod fsutil;
pub mod paths;
pub mod time;
pub mod types;
pub mod url;

pub use error::{RdmError, Result};
pub use paths::RdmPaths;
pub use types::*;
pub use url::{
    content_disposition_filename, expand_tilde, filename_from_url, normalize_sha256,
    parse_http_url, redact_url, resolve_destination, sanitize_filename, unique_path,
    validate_header, validate_headers, validate_http_url, validate_referrer,
};

/// Version string used by the daemon, CLI, desktop app and IPC handshake.
pub const RDM_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Maximum accepted URL length. Prevents pathological inputs in logs and the DB.
pub const MAX_URL_LEN: usize = 16 * 1024;

/// Maximum number of custom headers accepted for one download.
pub const MAX_HEADERS: usize = 64;

/// Maximum size of one custom header value.
pub const MAX_HEADER_VALUE_LEN: usize = 8 * 1024;

/// Maximum number of bytes accepted in a single Native Messaging frame.
pub const MAX_NATIVE_MESSAGE: usize = 1024 * 1024;

/// Maximum number of bytes accepted in a single IPC request line.
pub const MAX_IPC_MESSAGE: usize = 1024 * 1024;
