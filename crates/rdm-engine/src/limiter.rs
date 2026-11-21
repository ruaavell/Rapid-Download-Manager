use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// A token bucket shared by all connections of a task (or by the whole engine).
///
/// A rate of `0` means unlimited. The bucket allows at most one second of burst
/// so interactive downloads do not feel artificially bursty.
#[derive(Debug)]
pub struct SpeedLimiter {
    rate: AtomicU64,
    bucket: Mutex<Bucket>,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl SpeedLimiter {
    pub fn new(rate: u64) -> Arc<Self> {
        Arc::new(Self {
            rate: AtomicU64::new(rate),
            bucket: Mutex::new(Bucket {
                tokens: rate as f64,
                last: Instant::now(),
            }),
        })
    }

    pub fn set_rate(&self, rate: u64) {
        self.rate.store(rate, Ordering::Relaxed);
    }

    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Wait until `bytes` may be transferred. Cancellation is the caller's job:
    /// keep the acquired chunks reasonably small so cancellation is responsive.
    pub async fn acquire(&self, bytes: u64) {
        let mut bytes = bytes;
        loop {
            let rate = self.rate.load(Ordering::Relaxed);
            if rate == 0 {
                return;
            }
            if bytes == 0 {
                return;
            }
            // Never wait for more than one second in a single step.
            let step = bytes.min(rate).max(1);
            let wait = {
                let mut bucket = self.bucket.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(bucket.last).as_secs_f64();
                bucket.last = now;
                let capacity = (rate as f64).max(64.0 * 1024.0);
                bucket.tokens = (bucket.tokens + elapsed * rate as f64).min(capacity);
                if bucket.tokens >= step as f64 {
                    bucket.tokens -= step as f64;
                    None
                } else {
                    let missing = step as f64 - bucket.tokens;
                    bucket.tokens = 0.0;
                    Some(Duration::from_secs_f64(
                        (missing / rate as f64).clamp(0.001, 5.0),
                    ))
                }
            };
            match wait {
                Some(duration) => tokio::time::sleep(duration).await,
                None => {
                    bytes -= step;
                    if bytes == 0 {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unlimited_is_immediate() {
        let limiter = SpeedLimiter::new(0);
        let start = Instant::now();
        limiter.acquire(10 * 1024 * 1024).await;
        assert!(start.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn limited_waits_roughly_the_expected_time() {
        let limiter = SpeedLimiter::new(100_000);
        let start = Instant::now();
        // Consume the initial one-second burst, then request another 100 KiB.
        limiter.acquire(100_000).await;
        limiter.acquire(100_000).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(700),
            "elapsed: {elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(3), "elapsed: {elapsed:?}");
    }
}
