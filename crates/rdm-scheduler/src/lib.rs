//! Queue and time-based orchestration.
//!
//! The scheduler never downloads bytes itself. It observes persisted state and
//! asks the engine to start or pause workers, which keeps the engine free of
//! policy decisions and makes every action observable through the database.

use chrono::{Local, Timelike};
use rdm_common::{QueueStatus, RdmError, RepeatRule, Result, Schedule, ScheduleAction, Settings};
use rdm_engine::DownloadEngine;
use rdm_ipc::protocol::Event;
use rdm_storage::Storage;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub tick: Duration,
    /// Allow `shutdown`/`suspend` schedules to affect the machine. Disabled by
    /// default so tests and sandboxes cannot power off the host.
    pub allow_system_actions: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(1),
            allow_system_actions: false,
        }
    }
}

#[derive(Clone)]
pub struct Scheduler {
    storage: Storage,
    engine: DownloadEngine,
    config: SchedulerConfig,
    retry_cooldowns: Arc<Mutex<HashMap<String, Instant>>>,
}

impl Scheduler {
    pub fn new(storage: Storage, engine: DownloadEngine, config: SchedulerConfig) -> Self {
        Self {
            storage,
            engine,
            config,
            retry_cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn run(
        self,
        events: broadcast::Sender<Event>,
        shutdown: CancellationToken,
    ) -> Result<()> {
        let mut interval = tokio::time::interval(self.config.tick);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                _ = interval.tick() => {
                    if let Err(error) = self.tick(&events).await {
                        tracing::warn!(error = %error, "scheduler tick failed");
                    }
                }
            }
        }
    }

    /// One scheduler pass. Public so the daemon and tests can drive it.
    pub async fn tick(&self, events: &broadcast::Sender<Event>) -> Result<()> {
        let settings = self.storage.load_settings()?;
        self.apply_bandwidth(&settings).await?;
        self.reconcile(&settings, events).await?;
        self.retry_failed(&settings, events).await?;
        self.run_due_schedules(events).await?;
        Ok(())
    }

    async fn apply_bandwidth(&self, settings: &Settings) -> Result<()> {
        let desired =
            effective_speed_limit(settings, Local::now().hour() * 60 + Local::now().minute());
        if self.engine.global_speed_limit() != desired {
            self.engine.set_global_speed_limit(desired);
            tracing::info!(
                limit = ?desired,
                "applied bandwidth schedule"
            );
        }
        Ok(())
    }

    async fn reconcile(
        &self,
        settings: &Settings,
        events: &broadcast::Sender<Event>,
    ) -> Result<()> {
        let queues = self.storage.list_queues()?;
        let queue_map: HashMap<String, rdm_common::Queue> = queues
            .iter()
            .map(|queue| (queue.id.clone(), queue.clone()))
            .collect();

        let snapshots = self.engine.list().await?;

        // A paused or stopped queue must not keep workers running.
        for snapshot in &snapshots {
            if !snapshot.task.status.is_active() {
                continue;
            }
            let Some(queue_id) = &snapshot.task.queue_id else {
                continue;
            };
            let Some(queue) = queue_map.get(queue_id) else {
                continue;
            };
            if queue.status != QueueStatus::Running {
                if let Err(error) = self.engine.pause(&snapshot.task.id).await {
                    tracing::warn!(
                        download = %snapshot.task.id,
                        error = %error,
                        "could not pause a task in an inactive queue"
                    );
                }
            }
        }

        let snapshots = self.engine.list().await?;
        let mut active_total = snapshots
            .iter()
            .filter(|snapshot| snapshot.task.status.is_active())
            .count();
        let mut active_per_queue: HashMap<String, usize> = HashMap::new();
        for snapshot in &snapshots {
            if snapshot.task.status.is_active() {
                if let Some(queue_id) = &snapshot.task.queue_id {
                    *active_per_queue.entry(queue_id.clone()).or_insert(0) += 1;
                }
            }
        }

        let max_total = settings.max_concurrent_downloads.max(1) as usize;
        if active_total >= max_total {
            return Ok(());
        }

        let mut queued: Vec<_> = snapshots
            .iter()
            .filter(|snapshot| snapshot.task.status == rdm_common::DownloadStatus::Queued)
            .collect();
        queued.sort_by(|left, right| {
            let left_queue_priority = left
                .task
                .queue_id
                .as_ref()
                .and_then(|id| queue_map.get(id))
                .map(|queue| queue.priority)
                .unwrap_or(0);
            let right_queue_priority = right
                .task
                .queue_id
                .as_ref()
                .and_then(|id| queue_map.get(id))
                .map(|queue| queue.priority)
                .unwrap_or(0);
            right_queue_priority
                .cmp(&left_queue_priority)
                .then_with(|| right.task.priority.cmp(&left.task.priority))
                .then_with(|| left.task.created_at.cmp(&right.task.created_at))
        });

        for snapshot in queued {
            if active_total >= max_total {
                break;
            }
            if let Some(queue_id) = &snapshot.task.queue_id {
                let Some(queue) = queue_map.get(queue_id) else {
                    // The queue was deleted but the foreign key still points at
                    // it; treat it as an implicit running queue.
                    continue;
                };
                if queue.status != QueueStatus::Running {
                    continue;
                }
                let limit = queue.max_concurrent.max(1) as usize;
                let running = active_per_queue.get(queue_id).copied().unwrap_or(0);
                if running >= limit {
                    continue;
                }
            }
            match self.engine.start(&snapshot.task.id).await {
                Ok(()) => {
                    active_total += 1;
                    if let Some(queue_id) = &snapshot.task.queue_id {
                        *active_per_queue.entry(queue_id.clone()).or_insert(0) += 1;
                    }
                    if let Ok(Some(queue)) = snapshot
                        .task
                        .queue_id
                        .as_ref()
                        .map(|id| self.storage.get_queue(id))
                        .unwrap_or(Ok(None))
                    {
                        let _ = events.send(Event::Queue(queue));
                    }
                }
                Err(RdmError::InvalidState(_)) => {}
                Err(error) => {
                    tracing::warn!(
                        download = %snapshot.task.id,
                        error = %error,
                        "scheduler could not start a queued download"
                    );
                }
            }
        }
        Ok(())
    }

    async fn retry_failed(
        &self,
        settings: &Settings,
        events: &broadcast::Sender<Event>,
    ) -> Result<()> {
        if !settings.auto_retry {
            return Ok(());
        }
        let failed = self
            .storage
            .list_downloads_by_status(&[rdm_common::DownloadStatus::Failed])?;
        {
            let mut cooldowns = self
                .retry_cooldowns
                .lock()
                .map_err(|_| RdmError::other("retry cooldown lock poisoned"))?;
            cooldowns.retain(|id, _| failed.iter().any(|task| &task.id == id));
        }
        for task in failed {
            if !task_is_retryable(&task) {
                continue;
            }
            let retries = task.retry_count;
            if retries >= task.max_retries.max(1) {
                continue;
            }
            let delay = Duration::from_secs(5u64.saturating_mul(1 << retries.min(6)).min(300));
            let ready = {
                let mut cooldowns = self
                    .retry_cooldowns
                    .lock()
                    .map_err(|_| RdmError::other("retry cooldown lock poisoned"))?;
                match cooldowns.get(&task.id) {
                    Some(last) if last.elapsed() < delay => false,
                    _ => {
                        cooldowns.insert(task.id.clone(), Instant::now());
                        true
                    }
                }
            };
            if !ready {
                continue;
            }
            let _ = self.storage.increment_retry(&task.id)?;
            match self.engine.start(&task.id).await {
                Ok(()) => {
                    tracing::info!(download = %task.id, attempt = retries + 1, "retrying failed download");
                }
                Err(RdmError::InvalidState(_)) => {}
                Err(error) => {
                    tracing::warn!(download = %task.id, error = %error, "retry start failed");
                }
            }
            let _ = events.send(Event::Log {
                level: "info".into(),
                message: format!("retrying {} (attempt {})", task.filename, retries + 1),
            });
        }
        Ok(())
    }

    async fn run_due_schedules(&self, events: &broadcast::Sender<Event>) -> Result<()> {
        let now = rdm_common::time::now_unix();
        for schedule in self.storage.due_schedules(now)? {
            if let Err(error) = self.execute_schedule(&schedule, events).await {
                tracing::warn!(
                    schedule = %schedule.id,
                    error = %error,
                    "scheduled action failed"
                );
                let _ = events.send(Event::Log {
                    level: "error".into(),
                    message: format!("scheduled action '{}' failed: {error}", schedule.name),
                });
            }
            let next = next_run(&schedule, now);
            self.storage.mark_schedule_run(&schedule.id, now, next)?;
            if let Ok(Some(updated)) = self.storage.get_schedule(&schedule.id) {
                if !(matches!(
                    schedule.action,
                    ScheduleAction::Shutdown | ScheduleAction::Suspend
                ) && !schedule.confirmed)
                {
                    let _ = events.send(Event::Schedule(updated));
                }
            }
        }
        Ok(())
    }

    async fn execute_schedule(
        &self,
        schedule: &Schedule,
        events: &broadcast::Sender<Event>,
    ) -> Result<()> {
        match schedule.action {
            ScheduleAction::StartQueue => {
                let queue_id = schedule.target_id.as_deref().ok_or_else(|| {
                    RdmError::InvalidState("start_queue requires a target queue".into())
                })?;
                self.storage
                    .set_queue_status(queue_id, QueueStatus::Running)?;
                if let Some(queue) = self.storage.get_queue(queue_id)? {
                    let _ = events.send(Event::Queue(queue));
                }
            }
            ScheduleAction::PauseQueue => {
                let queue_id = schedule.target_id.as_deref().ok_or_else(|| {
                    RdmError::InvalidState("pause_queue requires a target queue".into())
                })?;
                self.storage
                    .set_queue_status(queue_id, QueueStatus::Paused)?;
                for task in self.storage.list_downloads()? {
                    if task.queue_id.as_deref() == Some(queue_id) && task.status.is_active() {
                        let _ = self.engine.pause(&task.id).await;
                    }
                }
                if let Some(queue) = self.storage.get_queue(queue_id)? {
                    let _ = events.send(Event::Queue(queue));
                }
            }
            ScheduleAction::ResumeAll => {
                for (id, result) in self.engine.resume_all().await {
                    if let Err(error) = result {
                        tracing::warn!(download = %id, error = %error, "resume_all failed for a task");
                    }
                }
            }
            ScheduleAction::PauseAll => {
                for (id, result) in self.engine.pause_all().await {
                    if let Err(error) = result {
                        tracing::warn!(download = %id, error = %error, "pause_all failed for a task");
                    }
                }
            }
            ScheduleAction::SetGlobalSpeedLimit => {
                let limit = schedule
                    .target_id
                    .as_deref()
                    .and_then(|value| value.trim().parse::<u64>().ok())
                    .unwrap_or(0);
                self.engine
                    .set_global_speed_limit(if limit == 0 { None } else { Some(limit) });
            }
            ScheduleAction::Shutdown => {
                if !schedule.confirmed {
                    // Unconfirmed destructive schedules are silently ignored.
                    return Ok(());
                }
                let _ = events.send(Event::Shutdown);
            }
            ScheduleAction::Suspend => {
                if !schedule.confirmed {
                    // Unconfirmed destructive schedules are silently ignored.
                    return Ok(());
                }
                if !self.config.allow_system_actions {
                    tracing::info!("suspend schedule skipped because system actions are disabled");
                    return Ok(());
                }
                tokio::process::Command::new("systemctl")
                    .arg("suspend")
                    .spawn()
                    .map_err(|error| {
                        RdmError::other(format!("could not suspend the system: {error}"))
                    })?;
            }
        }
        Ok(())
    }
}

/// The most restrictive active bandwidth window wins. Outside every window the
/// user's normal global limit applies.
pub fn effective_speed_limit(settings: &Settings, minute: u32) -> Option<u64> {
    let mut active: Option<u64> = None;
    for window in &settings.bandwidth_schedule {
        if window.limit_bps == 0 {
            continue;
        }
        if window_contains(window.start_minute, window.end_minute, minute) {
            active = Some(match active {
                Some(current) => current.min(window.limit_bps),
                None => window.limit_bps,
            });
        }
    }
    active
        .or(settings.global_speed_limit)
        .filter(|value| *value > 0)
}

fn window_contains(start: u32, end: u32, minute: u32) -> bool {
    let start = start.min(1439);
    let end = end.min(1440);
    if start < end {
        minute >= start && minute < end
    } else if start > end {
        minute >= start || minute < end
    } else {
        false
    }
}

fn next_run(schedule: &Schedule, now: i64) -> Option<i64> {
    match schedule.repeat_rule {
        RepeatRule::Once => None,
        RepeatRule::Hourly => Some(now + 3600),
        RepeatRule::Daily => Some(now + 86_400),
        RepeatRule::Weekly => Some(now + 7 * 86_400),
    }
}

fn task_is_retryable(task: &rdm_common::DownloadTask) -> bool {
    let Some(error) = task.error.as_deref() else {
        return false;
    };
    let error = error.to_ascii_lowercase();
    !(error.contains("checksum") || error.contains("http 4"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)]
    use super::*;
    use rdm_common::BandwidthWindow;

    #[test]
    fn bandwidth_windows_wrap_midnight() {
        assert!(window_contains(23 * 60, 6 * 60, 23 * 60 + 30));
        assert!(window_contains(23 * 60, 6 * 60, 5 * 60));
        assert!(!window_contains(23 * 60, 6 * 60, 12 * 60));
        assert!(!window_contains(60, 60, 60));
    }

    #[test]
    fn active_window_overrides_global_limit() {
        let mut settings = Settings::default();
        settings.global_speed_limit = Some(1_000_000);
        settings.bandwidth_schedule = vec![BandwidthWindow {
            start_minute: 0,
            end_minute: 1440,
            limit_bps: 50_000,
        }];
        assert_eq!(effective_speed_limit(&settings, 600), Some(50_000));
    }

    #[test]
    fn no_window_uses_global_limit() {
        let mut settings = Settings::default();
        settings.global_speed_limit = Some(2_000_000);
        assert_eq!(effective_speed_limit(&settings, 600), Some(2_000_000));
    }

    #[test]
    fn repeat_rules_advance() {
        let mut schedule = Schedule {
            id: "s".into(),
            name: "n".into(),
            action: ScheduleAction::PauseAll,
            target_id: None,
            run_at: 0,
            repeat_rule: RepeatRule::Daily,
            enabled: true,
            confirmed: false,
            last_run: None,
            created_at: 0,
        };
        assert_eq!(next_run(&schedule, 100), Some(100 + 86_400));
        schedule.repeat_rule = RepeatRule::Once;
        assert_eq!(next_run(&schedule, 100), None);
    }
}
