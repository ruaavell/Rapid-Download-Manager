use crate::{
    assemble,
    http::backoff_delay,
    probe::{probe_server, ProbeResult},
    runtime::{ControlCommand, TaskRuntime, TaskSecrets},
    segment::{download_segment, SegmentJob, SegmentProgress, PROGRESS_COMPLETE, PROGRESS_PENDING},
    DownloadEngine, EngineEventKind,
};
use rdm_common::{
    time::now_unix, DownloadSegment, DownloadStatus, DownloadTask, RdmError, Result, SegmentStatus,
};
use reqwest::header::HeaderMap;
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tokio::{
    sync::mpsc,
    task::JoinSet,
    time::{interval, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

pub async fn run_worker(
    engine: DownloadEngine,
    id: String,
    runtime: Arc<TaskRuntime>,
    mut control_rx: mpsc::Receiver<ControlCommand>,
) -> Result<()> {
    let mut task = engine
        .storage()
        .get_download(&id)?
        .ok_or_else(|| RdmError::NotFound(format!("download {id}")))?;
    let stored_headers = engine.storage().get_headers(&id)?;
    let secrets: TaskSecrets = engine.secrets_for(&id).await;
    let headers = engine.build_headers(&stored_headers, &secrets, task.referrer.as_deref())?;

    let previous_etag = task.etag.clone();
    let previous_last_modified = task.last_modified.clone();

    let probe = tokio::select! {
        command = control_rx.recv() => {
            match command {
                Some(command) => {
                    handle_stop(&engine, &id, command, task.downloaded, task.total_size, &[]).await?;
                }
                None => {
                    engine.storage().update_status(&id, DownloadStatus::Paused, None)?;
                    engine.emit_current(&id, EngineEventKind::StatusChanged).await;
                }
            }
            return Ok(());
        }
        result = probe_with_retry(&engine, &task.url, &headers) => result?,
    };

    // Persist server metadata and (when safe) the Content-Disposition filename.
    let representation_changed = match (&previous_etag, &probe.etag) {
        (Some(old), Some(new)) => old != new,
        _ => match (&previous_last_modified, &probe.last_modified) {
            (Some(old), Some(new)) => old != new,
            _ => false,
        },
    };

    let existing_segments = engine.storage().list_segments(&id)?;
    let destination_before = PathBuf::from(&task.destination);
    let parts_before = assemble::parts_dir(&destination_before);
    let has_parts = tokio::fs::metadata(&parts_before).await.is_ok();

    if !task.filename_explicit
        && existing_segments.is_empty()
        && !has_parts
        && probe.filename.is_some()
        && probe.filename.as_deref() != Some(task.filename.as_str())
    {
        if let Some(filename) = &probe.filename {
            let directory = PathBuf::from(&task.directory);
            let candidate = directory.join(filename);
            let candidate = unique_destination_for(&engine, &candidate)?;
            task.filename = candidate
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(filename)
                .to_string();
            task.destination = candidate.to_string_lossy().to_string();
        }
    }

    task.total_size = probe.total;
    task.supports_ranges = probe.supports_ranges;
    task.resumable = probe.supports_ranges && probe.total.is_some();
    task.etag = probe.etag.clone();
    task.last_modified = probe.last_modified.clone();
    task.mime_type = probe.mime_type.clone();

    let destination = PathBuf::from(&task.destination);
    let parts = assemble::parts_dir(&destination);
    assemble::prepare_parts_dir(&parts).await?;

    let mut segments = existing_segments;
    let must_replan = representation_changed
        || !segments_are_valid(&segments, probe.total, probe.supports_ranges)
        || !parts.exists();

    if must_replan {
        if representation_changed {
            tracing::warn!(download = %id, "server representation changed; restarting from zero");
        }
        assemble::reset_parts(&parts).await?;
        assemble::prepare_parts_dir(&parts).await?;
        let plan = plan_segments(
            probe.total,
            probe.supports_ranges,
            task.connections,
            engine.config().segment_threshold,
        );
        if plan.is_empty() {
            return complete_empty(&engine, task, &destination, &parts).await;
        }
        segments = engine.storage().replace_segments(&id, &plan)?;
        task.downloaded = 0;
    }

    if probe.total == Some(0) && segments.is_empty() {
        return complete_empty(&engine, task, &destination, &parts).await;
    }

    engine.storage().update_download(&task)?;

    // Reconcile the database plan with actual part-file lengths.
    let mut progress: HashMap<u32, Arc<SegmentProgress>> = HashMap::new();
    let mut pending: VecDeque<DownloadSegment> = VecDeque::new();
    let mut actual_downloaded = 0u64;

    for segment in segments.iter_mut() {
        let path = assemble::segment_path(&parts, segment.index);
        let mut length = tokio::fs::metadata(&path)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0);
        if let Some(expected) = segment.expected_len() {
            if length > expected {
                tokio::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .await
                    .map_err(|e| RdmError::from_io_at(path.clone(), e))?
                    .set_len(0)
                    .await
                    .map_err(|e| RdmError::from_io_at(path.clone(), e))?;
                length = 0;
            }
            if length == expected {
                segment.downloaded = length;
                segment.status = SegmentStatus::Complete;
            } else {
                segment.downloaded = length;
                segment.status = SegmentStatus::Pending;
            }
        } else {
            if !probe.supports_ranges && length > 0 {
                let file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .await
                    .map_err(|e| RdmError::from_io_at(path.clone(), e))?;
                file.set_len(0)
                    .await
                    .map_err(|e| RdmError::from_io_at(path.clone(), e))?;
                length = 0;
            }
            segment.downloaded = length;
            segment.status = SegmentStatus::Pending;
        }
        actual_downloaded = actual_downloaded.saturating_add(segment.downloaded);

        let progress_entry = SegmentProgress::new();
        progress_entry
            .downloaded
            .store(segment.downloaded, Ordering::Relaxed);
        progress_entry.status.store(
            if segment.status == SegmentStatus::Complete {
                PROGRESS_COMPLETE
            } else {
                PROGRESS_PENDING
            },
            Ordering::Relaxed,
        );
        if segment.status != SegmentStatus::Complete {
            pending.push_back(segment.clone());
        }
        progress.insert(segment.index, progress_entry);
    }

    task.downloaded = actual_downloaded;
    task.status = DownloadStatus::Downloading;
    task.error = None;
    if task.started_at.is_none() {
        task.started_at = Some(now_unix());
    }
    engine.storage().update_download(&task)?;
    persist_segments(&engine, &id, &segments).await?;
    engine
        .emit_current(&id, EngineEventKind::StatusChanged)
        .await;

    if pending.is_empty() {
        return finalize(
            &engine,
            id,
            task,
            segments,
            parts,
            destination,
            control_rx,
            &runtime,
        )
        .await;
    }

    let mut cancel = CancellationToken::new();
    let mut join_set: JoinSet<Result<(u32, u64)>> = JoinSet::new();
    let mut fallback_used = false;
    let mut use_ranges = probe.supports_ranges;
    let mut ticker = interval(engine.config().progress_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_downloaded = actual_downloaded;
    let mut last_tick = Instant::now();
    let mut last_persist = Instant::now();
    let mut speed = 0u64;

    loop {
        let target = runtime.connections.load(Ordering::Relaxed).clamp(1, 32) as usize;
        while join_set.len() < target {
            let Some(segment) = pending.pop_front() else {
                break;
            };
            let Some(progress_entry) = progress.get(&segment.index).cloned() else {
                continue;
            };
            let index = segment.index;
            let part_path = assemble::segment_path(&parts, index);
            let job = SegmentJob {
                client: engine.client().clone(),
                url: task.url.clone(),
                headers: headers.clone(),
                segment,
                part_path,
                supports_ranges: use_ranges,
                total: probe.total,
                cancel: cancel.clone(),
                task_limiter: runtime.limiter.clone(),
                global_limiter: engine.global_limiter(),
                progress: progress_entry,
                max_retries: task.max_retries.max(1),
            };
            join_set.spawn(async move { download_segment(job).await.map(|bytes| (index, bytes)) });
        }

        if pending.is_empty() && join_set.is_empty() {
            break;
        }

        tokio::select! {
            command = control_rx.recv() => {
                match command {
                    Some(ControlCommand::SetConnections(value)) => {
                        runtime.connections.store(value.clamp(1, 32), Ordering::Relaxed);
                        continue;
                    }
                    Some(command) => {
                        cancel.cancel();
                        drain_join_set(&mut join_set).await;
                        update_segments_from_progress(&mut segments, &progress);
                        let downloaded = sum_downloaded(&segments);
                        task.downloaded = downloaded;
                        persist_progress(&engine, &id, downloaded, task.total_size, &segments).await?;
                        handle_stop(&engine, &id, command, downloaded, task.total_size, &segments).await?;
                        return Ok(());
                    }
                    None => {
                        cancel.cancel();
                        drain_join_set(&mut join_set).await;
                        update_segments_from_progress(&mut segments, &progress);
                        let downloaded = sum_downloaded(&segments);
                        task.downloaded = downloaded;
                        persist_progress(&engine, &id, downloaded, task.total_size, &segments).await?;
                        engine.storage().update_status(&id, DownloadStatus::Paused, None)?;
                        engine.emit_current(&id, EngineEventKind::StatusChanged).await;
                        return Ok(());
                    }
                }
            }
            Some(joined) = join_set.join_next(), if !join_set.is_empty() => {
                match joined {
                    Ok(Ok((index, bytes))) => {
                        if let Some(entry) = progress.get(&index) {
                            entry.downloaded.store(bytes, Ordering::Relaxed);
                            entry.status.store(PROGRESS_COMPLETE, Ordering::Relaxed);
                        }
                        if let Some(segment) = segments.iter_mut().find(|segment| segment.index == index) {
                            segment.downloaded = bytes;
                            segment.status = SegmentStatus::Complete;
                        }
                        engine.storage().update_segment(&id, index, bytes, SegmentStatus::Complete)?;
                    }
                    Ok(Err(RdmError::Cancelled)) => {
                        // A cancellation initiated by pause/fallback; the outer loop owns the state.
                    }
                    Ok(Err(RdmError::RangeUnsupported)) if !fallback_used => {
                        fallback_used = true;
                        cancel.cancel();
                        drain_join_set(&mut join_set).await;
                        assemble::reset_parts(&parts).await?;
                        assemble::prepare_parts_dir(&parts).await?;
                        engine.storage().delete_segments(&id)?;
                        let plan = plan_segments(probe.total, false, 1, engine.config().segment_threshold);
                        segments = engine.storage().replace_segments(&id, &plan)?;
                        progress.clear();
                        pending.clear();
                        for segment in &segments {
                            let entry = SegmentProgress::new();
                            entry.downloaded.store(0, Ordering::Relaxed);
                            progress.insert(segment.index, entry);
                            pending.push_back(segment.clone());
                        }
                        task.downloaded = 0;
                        task.supports_ranges = false;
                        task.resumable = false;
                        use_ranges = false;
                        engine.storage().update_download(&task)?;
                        cancel = CancellationToken::new();
                        last_downloaded = 0;
                        last_tick = Instant::now();
                        continue;
                    }
                    Ok(Err(error)) => {
                        cancel.cancel();
                        drain_join_set(&mut join_set).await;
                        update_segments_from_progress(&mut segments, &progress);
                        let downloaded = sum_downloaded(&segments);
                        task.downloaded = downloaded;
                        persist_progress(&engine, &id, downloaded, task.total_size, &segments).await?;
                        let message = error.to_string();
                        engine.storage().update_status(&id, DownloadStatus::Failed, Some(&message))?;
                        engine.emit_current(&id, EngineEventKind::Failed).await;
                        return Err(error);
                    }
                    Err(join_error) => {
                        cancel.cancel();
                        drain_join_set(&mut join_set).await;
                        let error = RdmError::other(format!("segment worker failed: {join_error}"));
                        let message = error.to_string();
                        engine.storage().update_status(&id, DownloadStatus::Failed, Some(&message))?;
                        engine.emit_current(&id, EngineEventKind::Failed).await;
                        return Err(error);
                    }
                }
            }
            _ = ticker.tick() => {
                update_segments_from_progress(&mut segments, &progress);
                let downloaded = sum_downloaded(&segments);
                task.downloaded = downloaded;
                let elapsed = last_tick.elapsed().as_secs_f64();
                if elapsed > 0.0 {
                    let instant = ((downloaded.saturating_sub(last_downloaded)) as f64 / elapsed) as u64;
                    speed = if speed == 0 { instant } else { (speed * 2 + instant) / 3 };
                }
                last_downloaded = downloaded;
                last_tick = Instant::now();
                runtime.speed.store(speed, Ordering::Relaxed);
                runtime
                    .active_connections
                    .store(join_set.len() as u32, Ordering::Relaxed);
                let snapshot = engine.snapshot_from(
                    task.clone(),
                    segments.clone(),
                    speed,
                    join_set.len() as u32,
                );
                engine.emit_snapshot(EngineEventKind::Progress, snapshot);
                if last_persist.elapsed() >= Duration::from_secs(1) {
                    persist_progress(&engine, &id, downloaded, task.total_size, &segments).await?;
                    last_persist = Instant::now();
                }
            }
        }
    }

    cancel.cancel();
    drain_join_set(&mut join_set).await;
    update_segments_from_progress(&mut segments, &progress);
    let downloaded = sum_downloaded(&segments);
    task.downloaded = downloaded;
    persist_progress(&engine, &id, downloaded, task.total_size, &segments).await?;
    runtime.speed.store(0, Ordering::Relaxed);
    runtime.active_connections.store(0, Ordering::Relaxed);

    finalize(
        &engine,
        id,
        task,
        segments,
        parts,
        destination,
        control_rx,
        &runtime,
    )
    .await
}

async fn probe_with_retry(
    engine: &DownloadEngine,
    url: &str,
    headers: &HeaderMap,
) -> Result<ProbeResult> {
    let mut attempt = 0u32;
    loop {
        match probe_server(engine.client(), url, headers).await {
            Ok(probe) => return Ok(probe),
            Err(error) if error.is_retryable() && attempt < engine.config().max_retries => {
                attempt += 1;
                tokio::time::sleep(backoff_delay(attempt)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn plan_segments(
    total: Option<u64>,
    supports_ranges: bool,
    connections: u32,
    threshold: u64,
) -> Vec<(u32, u64, Option<u64>)> {
    let Some(total) = total else {
        return vec![(0, 0, None)];
    };
    if total == 0 {
        return Vec::new();
    }
    if !supports_ranges || total < threshold {
        return vec![(0, 0, None)];
    }
    let min_segment = 1024 * 1024u64;
    let max_parts = (total / min_segment).clamp(1, 32) as u32;
    let parts = connections.clamp(1, 32).min(max_parts);
    if parts <= 1 {
        return vec![(0, 0, Some(total - 1))];
    }
    let base = total / parts as u64;
    let remainder = total % parts as u64;
    let mut result = Vec::with_capacity(parts as usize);
    let mut start = 0u64;
    for index in 0..parts {
        let length = base + if (index as u64) < remainder { 1 } else { 0 };
        let end = start + length - 1;
        result.push((index, start, Some(end)));
        start = end + 1;
    }
    result
}

fn segments_are_valid(
    segments: &[DownloadSegment],
    total: Option<u64>,
    supports_ranges: bool,
) -> bool {
    if segments.is_empty() {
        return false;
    }
    if !supports_ranges {
        return segments.len() == 1 && segments[0].end.is_none();
    }
    match total {
        None => segments.len() == 1 && segments[0].end.is_none(),
        Some(total) => {
            let mut ordered = segments.to_vec();
            ordered.sort_by_key(|segment| segment.index);
            let mut cursor = 0u64;
            for segment in &ordered {
                if segment.start != cursor {
                    return false;
                }
                let Some(end) = segment.end else {
                    return false;
                };
                if end < segment.start {
                    return false;
                }
                cursor = end.saturating_add(1);
            }
            cursor == total
        }
    }
}

async fn persist_segments(
    engine: &DownloadEngine,
    id: &str,
    segments: &[DownloadSegment],
) -> Result<()> {
    let storage = engine.storage().clone();
    let id = id.to_string();
    let values: Vec<(u32, u64, SegmentStatus)> = segments
        .iter()
        .map(|segment| (segment.index, segment.downloaded, segment.status))
        .collect();
    tokio::task::spawn_blocking(move || storage.update_segments_batch(&id, &values))
        .await
        .map_err(|error| RdmError::other(format!("database task failed: {error}")))??;
    Ok(())
}

async fn persist_progress(
    engine: &DownloadEngine,
    id: &str,
    downloaded: u64,
    total: Option<u64>,
    segments: &[DownloadSegment],
) -> Result<()> {
    let storage = engine.storage().clone();
    let id = id.to_string();
    let values: Vec<(u32, u64, SegmentStatus)> = segments
        .iter()
        .map(|segment| (segment.index, segment.downloaded, segment.status))
        .collect();
    tokio::task::spawn_blocking(move || {
        storage.update_progress(&id, downloaded, total)?;
        storage.update_segments_batch(&id, &values)?;
        Ok::<(), RdmError>(())
    })
    .await
    .map_err(|error| RdmError::other(format!("database task failed: {error}")))??;
    Ok(())
}

fn update_segments_from_progress(
    segments: &mut [DownloadSegment],
    progress: &HashMap<u32, Arc<SegmentProgress>>,
) {
    for segment in segments.iter_mut() {
        if let Some(entry) = progress.get(&segment.index) {
            segment.downloaded = entry.downloaded.load(Ordering::Relaxed);
            segment.status = entry.status();
        }
    }
}

fn sum_downloaded(segments: &[DownloadSegment]) -> u64 {
    segments.iter().fold(0u64, |total, segment| {
        total.saturating_add(segment.downloaded)
    })
}

async fn drain_join_set(join_set: &mut JoinSet<Result<(u32, u64)>>) {
    let deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => {
                join_set.abort_all();
                while join_set.join_next().await.is_some() {}
                return;
            }
            joined = join_set.join_next() => {
                match joined {
                    Some(_) => continue,
                    None => return,
                }
            }
        }
    }
}

async fn handle_stop(
    engine: &DownloadEngine,
    id: &str,
    command: ControlCommand,
    downloaded: u64,
    total: Option<u64>,
    segments: &[DownloadSegment],
) -> Result<()> {
    match command {
        ControlCommand::Pause(ack) => {
            persist_progress(engine, id, downloaded, total, segments).await?;
            engine
                .storage()
                .update_status(id, DownloadStatus::Paused, None)?;
            engine
                .emit_current(id, EngineEventKind::StatusChanged)
                .await;
            let _ = ack.send(Ok(()));
        }
        ControlCommand::Cancel(ack) => {
            persist_progress(engine, id, downloaded, total, segments).await?;
            engine
                .storage()
                .update_status(id, DownloadStatus::Cancelled, None)?;
            engine
                .emit_current(id, EngineEventKind::StatusChanged)
                .await;
            let _ = ack.send(Ok(()));
        }
        ControlCommand::SetConnections(_) => {}
    }
    Ok(())
}

async fn wait_for_stop(control_rx: &mut mpsc::Receiver<ControlCommand>) -> Option<ControlCommand> {
    loop {
        match control_rx.recv().await {
            Some(ControlCommand::SetConnections(_)) => continue,
            other => return other,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn finalize(
    engine: &DownloadEngine,
    id: String,
    mut task: DownloadTask,
    segments: Vec<DownloadSegment>,
    parts: PathBuf,
    destination: PathBuf,
    mut control_rx: mpsc::Receiver<ControlCommand>,
    runtime: &TaskRuntime,
) -> Result<()> {
    let actual = match assemble::validate_parts(&segments, &parts).await {
        Ok(value) => value,
        Err(error) => {
            let message = error.to_string();
            engine
                .storage()
                .update_status(&id, DownloadStatus::Failed, Some(&message))?;
            engine.emit_current(&id, EngineEventKind::Failed).await;
            return Err(error);
        }
    };
    if let Some(expected) = task.total_size {
        if expected != actual {
            let error = RdmError::SizeMismatch { expected, actual };
            let message = error.to_string();
            engine
                .storage()
                .update_status(&id, DownloadStatus::Failed, Some(&message))?;
            engine.emit_current(&id, EngineEventKind::Failed).await;
            return Err(error);
        }
    }

    task.downloaded = actual;
    task.total_size = Some(actual);
    task.status = DownloadStatus::Verifying;
    engine.storage().update_download(&task)?;
    engine
        .emit_current(&id, EngineEventKind::StatusChanged)
        .await;

    let assembled = tokio::select! {
        command = wait_for_stop(&mut control_rx) => {
            match command {
                Some(command) => {
                    handle_stop(engine, &id, command, actual, Some(actual), &segments).await?;
                }
                None => {
                    engine.storage().update_status(&id, DownloadStatus::Paused, None)?;
                    engine.emit_current(&id, EngineEventKind::StatusChanged).await;
                }
            }
            return Ok(());
        }
        result = assemble::assemble_to_temp(&destination, &parts, &segments, Some(actual)) => result?,
    };

    let digest = tokio::select! {
        command = wait_for_stop(&mut control_rx) => {
            assemble::discard(&assembled).await;
            match command {
                Some(command) => {
                    handle_stop(engine, &id, command, actual, Some(actual), &segments).await?;
                }
                None => {
                    engine.storage().update_status(&id, DownloadStatus::Paused, None)?;
                    engine.emit_current(&id, EngineEventKind::StatusChanged).await;
                }
            }
            return Ok(());
        }
        result = assemble::sha256_file(&assembled.temp_path) => result?,
    };

    if let Some(expected) = &task.checksum_expected {
        if !digest.eq_ignore_ascii_case(expected) {
            assemble::discard(&assembled).await;
            let error = RdmError::ChecksumMismatch {
                expected: expected.clone(),
                actual: digest,
            };
            let message = error.to_string();
            engine
                .storage()
                .update_status(&id, DownloadStatus::Failed, Some(&message))?;
            engine.emit_current(&id, EngineEventKind::Failed).await;
            return Err(error);
        }
    }

    while let Ok(command) = control_rx.try_recv() {
        match command {
            ControlCommand::SetConnections(_) => continue,
            command => {
                assemble::discard(&assembled).await;
                handle_stop(engine, &id, command, actual, Some(actual), &segments).await?;
                return Ok(());
            }
        }
    }

    if let Err(error) = assemble::commit(assembled, &destination).await {
        let message = error.to_string();
        engine
            .storage()
            .update_status(&id, DownloadStatus::Failed, Some(&message))?;
        engine.emit_current(&id, EngineEventKind::Failed).await;
        return Err(error);
    }
    let _ = assemble::reset_parts(&parts).await;

    task.downloaded = actual;
    task.total_size = Some(actual);
    task.checksum_actual = Some(digest);
    task.status = DownloadStatus::Completed;
    task.error = None;
    task.completed_at = Some(now_unix());
    task.retry_count = 0;
    engine.storage().update_download(&task)?;
    engine.storage().add_history(&task)?;
    runtime.speed.store(0, Ordering::Relaxed);
    runtime.active_connections.store(0, Ordering::Relaxed);
    engine.emit_current(&id, EngineEventKind::Completed).await;
    Ok(())
}

async fn complete_empty(
    engine: &DownloadEngine,
    mut task: DownloadTask,
    destination: &std::path::Path,
    parts: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| RdmError::from_io_at(parent.to_path_buf(), e))?;
    }
    let temp = assemble::assembling_path(destination);
    let file = tokio::fs::File::create(&temp)
        .await
        .map_err(|e| RdmError::from_io_at(temp.clone(), e))?;
    file.sync_all()
        .await
        .map_err(|e| RdmError::from_io_at(temp.clone(), e))?;
    drop(file);
    tokio::fs::rename(&temp, destination)
        .await
        .map_err(|e| RdmError::from_io_at(destination.to_path_buf(), e))?;
    let digest = assemble::sha256_file(destination).await?;
    let _ = assemble::reset_parts(parts).await;
    task.downloaded = 0;
    task.total_size = Some(0);
    task.checksum_actual = Some(digest);
    task.status = DownloadStatus::Completed;
    task.error = None;
    task.completed_at = Some(now_unix());
    engine.storage().update_download(&task)?;
    engine.storage().add_history(&task)?;
    engine
        .emit_current(&task.id, EngineEventKind::Completed)
        .await;
    Ok(())
}

fn unique_destination_for(engine: &DownloadEngine, candidate: &std::path::Path) -> Result<PathBuf> {
    if !candidate.exists()
        && engine
            .storage()
            .find_active_by_destination(&candidate.to_string_lossy())?
            .is_none()
    {
        return Ok(candidate.to_path_buf());
    }
    let parent = candidate
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let stem = candidate
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = candidate.extension().and_then(|value| value.to_str());
    for index in 1..10_000 {
        let path = match extension {
            Some(ext) => parent.join(format!("{stem} ({index}).{ext}")),
            None => parent.join(format!("{stem} ({index})")),
        };
        if !path.exists()
            && engine
                .storage()
                .find_active_by_destination(&path.to_string_lossy())?
                .is_none()
        {
            return Ok(path);
        }
    }
    Err(RdmError::other(
        "could not find a free destination filename",
    ))
}
