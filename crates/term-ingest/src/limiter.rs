//! A spacing rate limiter: callers are let through one at a time with a
//! fixed gap between them. It never bursts, which is what EDGAR wants.

use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::{sleep_until, Instant};

pub struct RateLimiter {
    gap: Duration,
    next_slot: Mutex<Instant>,
}

impl RateLimiter {
    /// At most `max` acquisitions per second. `max` must be at least 1.
    pub fn per_second(max: u32) -> Self {
        assert!(max >= 1, "rate must be at least one per second");
        Self { gap: Duration::from_secs(1) / max, next_slot: Mutex::new(Instant::now()) }
    }

    /// Wait for the next free slot.
    pub async fn acquire(&self) {
        let slot = {
            let mut next = self.next_slot.lock().await;
            let slot = (*next).max(Instant::now());
            *next = slot + self.gap;
            slot
        };
        sleep_until(slot).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test(start_paused = true)]
    async fn sequential_calls_are_spaced() {
        let limiter = RateLimiter::per_second(10);
        let start = Instant::now();
        for _ in 0..25 {
            limiter.acquire().await;
        }
        // 25 calls need 24 gaps of 100 ms.
        assert!(start.elapsed() >= Duration::from_millis(2400), "took {:?}", start.elapsed());
        assert!(start.elapsed() < Duration::from_millis(2600), "took {:?}", start.elapsed());
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_callers_never_exceed_the_rate() {
        let limiter = Arc::new(RateLimiter::per_second(10));
        let start = Instant::now();
        let mut handles = Vec::new();
        for _ in 0..40 {
            let limiter = Arc::clone(&limiter);
            handles.push(tokio::spawn(async move {
                limiter.acquire().await;
                start.elapsed()
            }));
        }
        let mut times = Vec::new();
        for handle in handles {
            times.push(handle.await.unwrap());
        }
        times.sort();
        // Any eleven consecutive passes must span at least one second.
        for window in times.windows(11) {
            assert!(window[10] - window[0] >= Duration::from_secs(1), "window {window:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn idle_time_is_not_saved_up() {
        let limiter = RateLimiter::per_second(10);
        limiter.acquire().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        let start = Instant::now();
        for _ in 0..5 {
            limiter.acquire().await;
        }
        assert!(start.elapsed() >= Duration::from_millis(400), "took {:?}", start.elapsed());
    }
}
