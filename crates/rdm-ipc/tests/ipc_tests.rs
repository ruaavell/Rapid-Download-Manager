use async_trait::async_trait;
use rdm_common::{RdmError, Result};
use rdm_ipc::{
    protocol::{Command, Event, PingInfo},
    Handler, IpcClient, IpcServer,
};
use serde_json::{json, Value};
use std::{os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

struct TestHandler;

#[async_trait]
impl Handler for TestHandler {
    async fn handle(&self, command: Command) -> Result<Value> {
        match command {
            Command::Ping => serde_json::to_value(PingInfo {
                version: rdm_common::RDM_VERSION.to_string(),
                pid: std::process::id(),
                protocol: rdm_ipc::PROTOCOL_VERSION,
            })
            .map_err(|error| RdmError::other(error.to_string())),
            Command::GetDownload { download_id } => {
                if download_id == "missing" {
                    Err(RdmError::NotFound(format!("download {download_id}")))
                } else {
                    Ok(json!({ "download_id": download_id }))
                }
            }
            Command::Status => Ok(json!({ "status": "ok" })),
            other => Err(RdmError::InvalidState(format!("unsupported {other:?}"))),
        }
    }
}

struct Fixture {
    dir: TempDir,
    socket: std::path::PathBuf,
    token_path: std::path::PathBuf,
    token: String,
    events: broadcast::Sender<Event>,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<Result<()>>,
}

impl Fixture {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("runtime").join("rdm.sock");
        let token_path = dir.path().join("runtime").join("ipc.token");
        let server = IpcServer::bind_at(&socket, &token_path, None).expect("bind");
        let token = server.token().to_string();
        let (events, _) = broadcast::channel(32);
        let shutdown = CancellationToken::new();
        let task =
            tokio::spawn(server.run(Arc::new(TestHandler), events.clone(), shutdown.clone()));
        Self {
            dir,
            socket,
            token_path,
            token,
            events,
            shutdown,
            task,
        }
    }

    async fn client(&self) -> IpcClient {
        IpcClient::connect_at(&self.socket, &self.token)
            .await
            .expect("connect")
    }

    async fn stop(self) {
        self.shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
        drop(self.dir);
    }
}

#[tokio::test]
async fn rpc_roundtrip_and_typed_payload() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let info: PingInfo = client.call(Command::Ping).await.expect("ping");
    assert_eq!(info.protocol, rdm_ipc::PROTOCOL_VERSION);
    assert_eq!(info.version, rdm_common::RDM_VERSION);
    let value = client
        .call_raw(Command::GetDownload {
            download_id: "abc".into(),
        })
        .await
        .expect("get");
    assert_eq!(value["download_id"], "abc");
    fixture.stop().await;
}

#[tokio::test]
async fn typed_errors_survive_the_wire() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let error = client
        .call_raw(Command::GetDownload {
            download_id: "missing".into(),
        })
        .await
        .expect_err("must fail");
    assert!(matches!(error, RdmError::NotFound(_)), "got {error:?}");
    fixture.stop().await;
}

#[tokio::test]
async fn wrong_token_is_rejected() {
    let fixture = Fixture::start().await;
    let error = IpcClient::connect_at(&fixture.socket, "not-the-token")
        .await
        .expect_err("must reject");
    assert!(matches!(error, RdmError::Permission(_)), "got {error:?}");
    fixture.stop().await;
}

#[tokio::test]
async fn socket_and_token_are_private() {
    let fixture = Fixture::start().await;
    let socket_mode = std::fs::metadata(&fixture.socket)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    let token_mode = std::fs::metadata(&fixture.token_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(socket_mode, 0o600);
    assert_eq!(token_mode, 0o600);
    fixture.stop().await;
}

#[tokio::test]
async fn event_subscription_receives_broadcasts() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let mut events = client.subscribe().await.expect("subscribe");
    fixture
        .events
        .send(Event::Log {
            level: "info".into(),
            message: "hello".into(),
        })
        .expect("send");
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("timeout")
        .expect("recv")
        .expect("event");
    match event {
        Event::Log { message, .. } => assert_eq!(message, "hello"),
        other => panic!("unexpected event {other:?}"),
    }
    fixture.stop().await;
}

#[tokio::test]
async fn second_server_on_same_socket_is_refused() {
    let fixture = Fixture::start().await;
    let error = IpcServer::bind_at(
        &fixture.socket,
        &fixture.dir.path().join("other.token"),
        None,
    )
    .expect_err("must refuse");
    assert!(matches!(error, RdmError::InvalidState(_)), "got {error:?}");
    fixture.stop().await;
}

#[tokio::test]
async fn malformed_request_closes_only_that_connection() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let fixture = Fixture::start().await;
    let mut stream = tokio::net::UnixStream::connect(&fixture.socket)
        .await
        .expect("connect");
    let hello = serde_json::to_string(&rdm_ipc::ClientHello {
        version: rdm_ipc::PROTOCOL_VERSION,
        token: fixture.token.clone(),
        client: "test".into(),
    })
    .unwrap();
    stream
        .write_all(format!("{hello}\n").as_bytes())
        .await
        .unwrap();
    let mut reader = tokio::io::BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert!(line.contains("\"ok\":true"));
    // Send garbage, then verify a fresh client still works.
    let stream = reader.get_mut();
    stream.write_all(b"this is not json\n").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let client = fixture.client().await;
    client.call_raw(Command::Ping).await.expect("ping");
    fixture.stop().await;
}
