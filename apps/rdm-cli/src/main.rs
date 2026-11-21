use anyhow::Result;
use clap::Parser;
use rdm_common::NewDownloadRequest;
use rdm_engine::{DownloadEngine, EngineConfig};
use rdm_storage::Storage;
use std::time::Duration;
use tokio::time::sleep;

#[derive(Parser)]
#[command(name = "rdm", about = "RDM Linux-native download manager CLI")]
struct Args {
    url: String,
    #[arg(short, long, default_value = ".")]
    output: String,
    #[arg(long)]
    filename: Option<String>,
    #[arg(long)]
    speed_limit: Option<u64>,
    #[arg(long, default_value_t = 8)]
    connections: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let (storage, _) = Storage::open_default().map_err(|e| anyhow::anyhow!(e))?;
    let engine =
        DownloadEngine::new(storage, EngineConfig::default()).map_err(|e| anyhow::anyhow!(e))?;
    if let Some(limit) = args.speed_limit {
        engine.set_global_speed_limit(Some(limit));
    }
    let destination = args.output.clone();
    let task = engine
        .add(NewDownloadRequest {
            url: args.url,
            destination,
            filename: args.filename,
            connections: Some(args.connections.clamp(1, 32)),
            speed_limit: args.speed_limit,
            checksum_sha256: None,
            referrer: None,
            headers: vec![],
            cookies: None,
            queue_id: None,
            priority: None,
            auto_start: Some(true),
        })
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    engine
        .start(&task.id)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("Downloading {} ({})", task.filename, task.id);

    loop {
        sleep(Duration::from_millis(500)).await;
        let current = engine
            .list()
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|t| t.task.id == task.id);
        if let Some(current) = current {
            let total = current
                .task
                .total_size
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into());
            eprint!(
                "\r{:?}: {} / {} bytes",
                current.task.status, current.task.downloaded, total
            );
            match current.task.status {
                rdm_common::DownloadStatus::Completed => {
                    println!("\nSaved to {}", current.task.destination);
                    break;
                }
                rdm_common::DownloadStatus::Failed => {
                    anyhow::bail!(
                        "{}",
                        current
                            .task
                            .error
                            .unwrap_or_else(|| "Download failed".into())
                    );
                }
                rdm_common::DownloadStatus::Cancelled => break,
                _ => {}
            }
        }
    }
    let _ = engine.shutdown().await;
    Ok(())
}
