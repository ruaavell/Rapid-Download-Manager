//! Authenticated IPC between the RDM daemon and its clients.
//!
//! Transport: newline-delimited JSON over a Unix domain socket. Every
//! connection starts with a token handshake; the server additionally requires
//! `SO_PEERCRED` to report the same effective uid. The socket and the token
//! file are both mode 0600 inside a 0700 runtime directory.

pub mod client;
pub mod protocol;
pub mod server;
pub mod wire;

pub use client::{daemon_maybe_running, EventStream, IpcClient};
pub use protocol::*;
pub use server::{Handler, IpcServer};
pub use wire::generate_token;
