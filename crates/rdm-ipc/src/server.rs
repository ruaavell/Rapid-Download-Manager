use crate::{
    protocol::{
        ClientHello, Command, Event, IpcError, Request, Response, ServerHello, PROTOCOL_VERSION,
    },
    wire::{
        current_uid, generate_token, peer_uid, read_json_limited, token_matches, write_json_line,
    },
};
use async_trait::async_trait;
use rdm_common::{fsutil, RdmError, RdmPaths, Result, MAX_IPC_MESSAGE};
use serde_json::Value;
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{broadcast, Semaphore},
};
use tokio_util::sync::CancellationToken;

/// The daemon implements this trait. Handlers are async because some operations
/// wait for pause/cancel acknowledgements from running workers.
#[async_trait]
pub trait Handler: Send + Sync + 'static {
    async fn handle(&self, command: Command) -> Result<Value>;
}

/// Authenticated Unix-socket server.
pub struct IpcServer {
    listener: UnixListener,
    socket: PathBuf,
    token_path: PathBuf,
    pid_path: Option<PathBuf>,
    token: String,
}

impl std::fmt::Debug for IpcServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcServer")
            .field("socket", &self.socket)
            .finish()
    }
}

impl IpcServer {
    pub fn bind(paths: &RdmPaths) -> Result<Self> {
        paths.ensure()?;
        Self::bind_at(&paths.socket, &paths.token, Some(&paths.pid))
    }

    pub fn bind_at(socket: &Path, token_path: &Path, pid_path: Option<&Path>) -> Result<Self> {
        if let Some(parent) = socket.parent() {
            fsutil::ensure_dir(parent)?;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
        if let Some(parent) = token_path.parent() {
            fsutil::ensure_dir(parent)?;
        }

        // Refuse to steal a live socket. A stale one (daemon crashed) is removed.
        if socket.exists() {
            match std::os::unix::net::UnixStream::connect(socket) {
                Ok(_) => {
                    return Err(RdmError::InvalidState(
                        "another RDM daemon is already listening on the IPC socket".into(),
                    ))
                }
                Err(_) => {
                    let _ = std::fs::remove_file(socket);
                }
            }
        }

        let token = generate_token();
        fsutil::atomic_write(token_path, token.as_bytes(), Some(0o600))?;
        let listener = UnixListener::bind(socket)
            .map_err(|error| RdmError::from_io_at(socket.to_path_buf(), error))?;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| RdmError::from_io_at(socket.to_path_buf(), error))?;

        if let Some(pid_path) = pid_path {
            let pid = std::process::id().to_string();
            let _ = fsutil::atomic_write(pid_path, pid.as_bytes(), Some(0o600));
        }

        Ok(Self {
            listener,
            socket: socket.to_path_buf(),
            token_path: token_path.to_path_buf(),
            pid_path: pid_path.map(Path::to_path_buf),
            token,
        })
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Accept connections until `shutdown` is cancelled.
    pub async fn run(
        self,
        handler: Arc<dyn Handler>,
        events: broadcast::Sender<Event>,
        shutdown: CancellationToken,
    ) -> Result<()> {
        let semaphore = Arc::new(Semaphore::new(256));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                accepted = self.listener.accept() => {
                    let (stream, _) = match accepted {
                        Ok(value) => value,
                        Err(error) => {
                            tracing::warn!(error = %error, "IPC accept failed");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            continue;
                        }
                    };
                    let Ok(permit) = semaphore.clone().acquire_owned().await else {
                        break;
                    };
                    let handler = handler.clone();
                    let events = events.clone();
                    let token = self.token.clone();
                    let shutdown = shutdown.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(error) = serve(stream, token, handler, events, shutdown).await {
                            tracing::debug!(error = %error, "IPC connection ended");
                        }
                    });
                }
            }
        }
        Ok(())
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        // Only remove the token if it is still ours; a newer daemon may have
        // replaced it after this process began shutting down.
        if let Ok(contents) = std::fs::read_to_string(&self.token_path) {
            if token_matches(&self.token, contents.trim()) {
                let _ = std::fs::remove_file(&self.token_path);
            }
        }
        let _ = std::fs::remove_file(&self.socket);
        if let Some(pid_path) = &self.pid_path {
            let _ = std::fs::remove_file(pid_path);
        }
    }
}

async fn serve(
    stream: UnixStream,
    expected_token: String,
    handler: Arc<dyn Handler>,
    events: broadcast::Sender<Event>,
    shutdown: CancellationToken,
) -> Result<()> {
    let uid = peer_uid(&stream)
        .map_err(|error| RdmError::network(format!("could not read peer credentials: {error}")))?;
    let mut reader = BufReader::new(stream);

    let hello: ClientHello = match read_json_limited(&mut reader, 8 * 1024).await {
        Ok(Some(hello)) => hello,
        Ok(None) => return Ok(()),
        Err(error) => {
            let _ = write_json_line(
                reader.get_mut(),
                &ServerHello {
                    version: PROTOCOL_VERSION,
                    pid: std::process::id(),
                    ok: false,
                    message: Some(error.to_string()),
                },
            )
            .await;
            return Ok(());
        }
    };

    let authorized = uid == current_uid() && token_matches(&expected_token, hello.token.trim());
    if !authorized || hello.version != PROTOCOL_VERSION {
        let message = if !authorized {
            "IPC authentication failed".to_string()
        } else {
            format!(
                "unsupported protocol version {} (daemon speaks {PROTOCOL_VERSION})",
                hello.version
            )
        };
        let _ = write_json_line(
            reader.get_mut(),
            &ServerHello {
                version: PROTOCOL_VERSION,
                pid: std::process::id(),
                ok: false,
                message: Some(message),
            },
        )
        .await;
        return Ok(());
    }

    write_json_line(
        reader.get_mut(),
        &ServerHello {
            version: PROTOCOL_VERSION,
            pid: std::process::id(),
            ok: true,
            message: None,
        },
    )
    .await?;

    loop {
        let request: Request = match read_json_limited(&mut reader, MAX_IPC_MESSAGE).await {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(error) => {
                tracing::debug!(error = %error, "invalid IPC request");
                return Ok(());
            }
        };

        if matches!(request.command, Command::Subscribe) {
            let mut receiver = events.subscribe();
            write_json_line(
                reader.get_mut(),
                &Response::success(request.id, Value::Null),
            )
            .await?;
            let mut probe = [0u8; 1];
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => return Ok(()),
                    read = reader.read(&mut probe) => {
                        // Any client data on a subscription is a protocol error,
                        // and EOF means the subscriber went away.
                        let _ = read;
                        return Ok(());
                    }
                    event = receiver.recv() => {
                        match event {
                            Ok(event) => {
                                if write_json_line(reader.get_mut(), &event).await.is_err() {
                                    return Ok(());
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                let lagged = Event::Log {
                                    level: "warn".into(),
                                    message: format!("event stream lagged; {skipped} events were dropped"),
                                };
                                if write_json_line(reader.get_mut(), &lagged).await.is_err() {
                                    return Ok(());
                                }
                            }
                            Err(broadcast::error::RecvError::Closed) => return Ok(()),
                        }
                    }
                }
            }
        }

        let response = match handler.handle(request.command).await {
            Ok(data) => Response::success(request.id, data),
            Err(error) => Response::failure(request.id, IpcError::from_rdm(&error)),
        };
        if write_json_line(reader.get_mut(), &response).await.is_err() {
            return Ok(());
        }
    }
}
