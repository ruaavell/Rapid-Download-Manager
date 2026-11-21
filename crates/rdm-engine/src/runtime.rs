use crate::limiter::SpeedLimiter;
use rdm_common::{HeaderEntry, Result};
use std::sync::{
    atomic::{AtomicU32, AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{mpsc, oneshot};

/// Commands accepted by a running worker. Pause/cancel carry an acknowledgement
/// so the caller knows buffers were flushed and network activity stopped.
pub enum ControlCommand {
    Pause(oneshot::Sender<Result<()>>),
    Cancel(oneshot::Sender<Result<()>>),
    SetConnections(u32),
}

pub struct TaskRuntime {
    pub id: String,
    pub generation: String,
    pub control: mpsc::Sender<ControlCommand>,
    pub speed: AtomicU64,
    pub active_connections: AtomicU32,
    pub connections: AtomicU32,
    pub limiter: Arc<SpeedLimiter>,
}

impl TaskRuntime {
    pub fn new(
        id: String,
        connections: u32,
        speed_limit: Option<u64>,
    ) -> (Self, mpsc::Receiver<ControlCommand>) {
        let (control, receiver) = mpsc::channel(8);
        (
            Self {
                id,
                generation: uuid::Uuid::new_v4().to_string(),
                control,
                speed: AtomicU64::new(0),
                active_connections: AtomicU32::new(0),
                connections: AtomicU32::new(connections),
                limiter: SpeedLimiter::new(speed_limit.unwrap_or(0)),
            },
            receiver,
        )
    }

    pub fn speed(&self) -> u64 {
        self.speed.load(Ordering::Relaxed)
    }

    pub fn active_connections(&self) -> u32 {
        self.active_connections.load(Ordering::Relaxed)
    }
}

/// Credentials and headers that are intentionally kept only in memory.
#[derive(Clone, Default)]
pub struct TaskSecrets {
    pub headers: Vec<HeaderEntry>,
    pub cookies: Option<String>,
}

impl TaskSecrets {
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty() && self.cookies.is_none()
    }
}
