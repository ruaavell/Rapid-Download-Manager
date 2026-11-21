use crate::{
    protocol::{ClientHello, Command, Event, Request, Response, ServerHello, PROTOCOL_VERSION},
    wire::{read_json_limited, write_json_line},
};
use rdm_common::{RdmError, RdmPaths, Result, MAX_IPC_MESSAGE};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{io::BufReader, net::UnixStream, sync::Mutex, time::timeout};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Sequential request/response client. A subscription uses its own connection
/// so that long-running event consumers never block RPC calls.
pub struct IpcClient {
    inner: Mutex<BufReader<UnixStream>>,
    next_id: AtomicU64,
    socket: PathBuf,
    token: String,
    pid: u32,
}

impl std::fmt::Debug for IpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcClient")
            .field("socket", &self.socket)
            .field("pid", &self.pid)
            .finish()
    }
}

impl IpcClient {
    pub async fn connect(paths: &RdmPaths) -> Result<Self> {
        let token = std::fs::read_to_string(&paths.token).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RdmError::InvalidState(
                    "the RDM daemon is not running (IPC token is missing)".into(),
                )
            } else {
                RdmError::from_io_at(paths.token.clone(), error)
            }
        })?;
        Self::connect_at(&paths.socket, token.trim()).await
    }

    pub async fn connect_at(socket: &Path, token: &str) -> Result<Self> {
        let stream = UnixStream::connect(socket).await.map_err(|error| {
            RdmError::network(format!(
                "could not connect to the RDM daemon at {}: {error}",
                socket.display()
            ))
        })?;
        let mut reader = BufReader::new(stream);
        let hello = ClientHello {
            version: PROTOCOL_VERSION,
            token: token.to_string(),
            client: format!("rdm/{}", rdm_common::RDM_VERSION),
        };
        write_json_line(reader.get_mut(), &hello).await?;
        let response = timeout(
            HANDSHAKE_TIMEOUT,
            read_json_limited::<_, ServerHello>(&mut reader, 8 * 1024),
        )
        .await
        .map_err(|_| RdmError::network("timed out during the IPC handshake"))??;
        let response = response.ok_or_else(|| {
            RdmError::network("the RDM daemon closed the connection during the handshake")
        })?;
        if !response.ok {
            return Err(RdmError::Permission(
                response
                    .message
                    .unwrap_or_else(|| "IPC authentication failed".into()),
            ));
        }
        if response.version != PROTOCOL_VERSION {
            return Err(RdmError::InvalidState(format!(
                "protocol mismatch: daemon speaks {}, client speaks {PROTOCOL_VERSION}",
                response.version
            )));
        }
        Ok(Self {
            inner: Mutex::new(reader),
            next_id: AtomicU64::new(1),
            socket: socket.to_path_buf(),
            token: token.to_string(),
            pid: response.pid,
        })
    }

    pub fn daemon_pid(&self) -> u32 {
        self.pid
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub async fn call<T>(&self, command: Command) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let value = self.call_raw(command).await?;
        serde_json::from_value(value)
            .map_err(|error| RdmError::other(format!("unexpected IPC payload: {error}")))
    }

    pub async fn call_raw(&self, command: Command) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = Request::new(id, command);
        let mut guard = self.inner.lock().await;
        write_json_line(guard.get_mut(), &request).await?;
        let response: Response = read_json_limited(&mut *guard, MAX_IPC_MESSAGE)
            .await?
            .ok_or_else(|| RdmError::network("the RDM daemon closed the connection"))?;
        if response.id != id {
            return Err(RdmError::network(format!(
                "IPC response id {} does not match request {id}",
                response.id
            )));
        }
        if !response.ok {
            return Err(response
                .error
                .map(|error| error.into_rdm())
                .unwrap_or_else(|| RdmError::other("the RDM daemon returned an unknown error")));
        }
        Ok(response.data.unwrap_or(Value::Null))
    }

    pub async fn subscribe(&self) -> Result<EventStream> {
        let stream = UnixStream::connect(&self.socket).await.map_err(|error| {
            RdmError::network(format!("could not open the event stream: {error}"))
        })?;
        let mut reader = BufReader::new(stream);
        let hello = ClientHello {
            version: PROTOCOL_VERSION,
            token: self.token.clone(),
            client: format!("rdm-events/{}", rdm_common::RDM_VERSION),
        };
        write_json_line(reader.get_mut(), &hello).await?;
        let response = timeout(
            HANDSHAKE_TIMEOUT,
            read_json_limited::<_, ServerHello>(&mut reader, 8 * 1024),
        )
        .await
        .map_err(|_| RdmError::network("timed out during the event handshake"))??;
        let response =
            response.ok_or_else(|| RdmError::network("daemon closed the event connection"))?;
        if !response.ok {
            return Err(RdmError::Permission(
                response
                    .message
                    .unwrap_or_else(|| "IPC authentication failed".into()),
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        write_json_line(reader.get_mut(), &Request::new(id, Command::Subscribe)).await?;
        let response: Response = read_json_limited(&mut reader, MAX_IPC_MESSAGE)
            .await?
            .ok_or_else(|| RdmError::network("daemon closed the event connection"))?;
        if !response.ok {
            return Err(response
                .error
                .map(|error| error.into_rdm())
                .unwrap_or_else(|| RdmError::other("could not subscribe to events")));
        }
        Ok(EventStream { reader })
    }
}

pub struct EventStream {
    reader: BufReader<UnixStream>,
}

impl EventStream {
    /// `None` means the daemon closed the stream.
    pub async fn recv(&mut self) -> Result<Option<Event>> {
        read_json_limited(&mut self.reader, MAX_IPC_MESSAGE).await
    }
}

/// True when a token and socket exist. This is a cheap UI hint; the only
/// authoritative check is a successful handshake.
pub fn daemon_maybe_running(paths: &RdmPaths) -> bool {
    paths.token.exists() && paths.socket.exists()
}
