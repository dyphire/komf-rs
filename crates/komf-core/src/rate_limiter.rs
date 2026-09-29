//! 平滑节流限速器 —— 对齐 Kotlin Ktor `HttpRequestRateLimiter`
//! （`interval` / `eventsPerInterval` / `allowBurst=false`，即 Ktor `RateLimiterImpl`）：
//! 每个许可占用 `interval / events` 的槽位，超出时阻塞到下一个可用槽位，无突发。
//!
//! 最初用于 MangaBaka API 客户端（对齐 komf 原版 Kotlin 的
//! `install(HttpRequestRateLimiter) { interval = 2.seconds; eventsPerInterval = 1 }`），
//! 其他 provider / 通知客户端也可复用同一实现。

use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct ThroughputLimiter {
    permit_duration: Duration,
    cursor: Mutex<Instant>,
}

impl ThroughputLimiter {
    /// `events_per_interval` 个许可在 `interval` 内均匀发放（每许可 `interval / events`）。
    pub fn new(events_per_interval: u32, interval: Duration) -> Self {
        let permit_duration = interval.div_f32(events_per_interval as f32);
        Self {
            permit_duration,
            cursor: Mutex::new(Instant::now()),
        }
    }

    /// 获取一个许可；需要等待时阻塞到下一个可用槽位（对齐 Ktor `RateLimiterImpl.acquire`）。
    pub async fn acquire(&self) {
        let now = Instant::now();
        let sleep = {
            let mut cursor = self.cursor.lock().unwrap();
            let base = if *cursor > now { *cursor } else { now };
            *cursor = base + self.permit_duration;
            base.saturating_duration_since(now)
        };
        if !sleep.is_zero() {
            tokio::time::sleep(sleep).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[tokio::test]
    async fn smooths_bursts() {
        // 对齐 Kotlin RateLimiterImpl：第 1 许可立即，后续许可按 permit 槽位平滑发放。
        let limiter = ThroughputLimiter::new(2, Duration::from_millis(100));
        let start = Instant::now();
        limiter.acquire().await;
        assert!(start.elapsed() < Duration::from_millis(30), "first permit should be immediate");
        limiter.acquire().await;
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(40), "second permit waits for its slot, got {elapsed:?}");
        assert!(elapsed < Duration::from_millis(130), "second permit within interval, got {elapsed:?}");
    }

    #[tokio::test]
    async fn single_event_per_interval() {
        // MangaBaka 配置：interval=2s、eventsPerInterval=1 → 相邻许可间隔约 2s。
        let limiter = ThroughputLimiter::new(1, Duration::from_secs(2));
        let start = Instant::now();
        limiter.acquire().await;
        assert!(start.elapsed() < Duration::from_millis(30), "first permit should be immediate");
        limiter.acquire().await;
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(1900), "2s/1 slot, got {elapsed:?}");
    }
}
