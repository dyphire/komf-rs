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

/// 滑动窗口突发限流器 —— 对齐 Kotlin `IntervalLimiterImpl`
/// （`allowBurst=true` 时 `HttpRequestRateLimiter` 使用）：
/// 窗口（`interval`）内最多 `events_per_interval` 个许可，窗口内可突发打完配额；
/// 窗口已满时阻塞到下一窗口边界（`acquire`），或立即返回 false（`try_acquire`）。
///
/// 用于 MangaDex / AniList / MangaUpdates / Bangumi 等原版 `intervalLimiter(...)` 配置的 provider。
pub struct IntervalLimiter {
    events_per_interval: u32,
    interval: Duration,
    state: Mutex<IntervalLimiterState>,
}

struct IntervalLimiterState {
    count: u32,
    window_start: Instant,
}

impl IntervalLimiter {
    pub fn new(events_per_interval: u32, interval: Duration) -> Self {
        Self {
            events_per_interval,
            interval,
            state: Mutex::new(IntervalLimiterState {
                count: 0,
                window_start: Instant::now(),
            }),
        }
    }

    /// 获取一个许可：窗口未满立即放行；窗口已满阻塞到下一窗口边界。
    /// 对齐 Kotlin `IntervalLimiterImpl.acquire`（窗口过期后对齐到当前时刻）。
    pub async fn acquire(&self) {
        let wait = {
            let mut state = self.state.lock().unwrap();
            let now = Instant::now();
            let elapsed = now.duration_since(state.window_start);
            if elapsed >= self.interval {
                state.window_start = now;
                state.count = 1;
                None
            } else if state.count < self.events_per_interval {
                state.count += 1;
                None
            } else {
                Some(self.interval - elapsed)
            }
        };
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
            // 醒来后对齐到当前时刻，占用新窗口第一个许可。
            let mut state = self.state.lock().unwrap();
            state.window_start = Instant::now();
            state.count = 1;
        }
    }

    /// 非阻塞尝试：窗口未满立即放行并返回 true；窗口已满返回 false（不排队）。
    pub async fn try_acquire(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        let now = Instant::now();
        if now.duration_since(state.window_start) >= self.interval {
            state.window_start = now;
            state.count = 1;
            return true;
        }
        if state.count < self.events_per_interval {
            state.count += 1;
            return true;
        }
        false
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
    async fn interval_limiter_burst_within_window() {
        // 窗口 1s / 10 许可：窗口内前 10 个立即放行（突发），第 11 个阻塞到下一窗口。
        let limiter = IntervalLimiter::new(10, Duration::from_secs(1));
        let start = Instant::now();
        for _ in 0..10 {
            assert!(limiter.try_acquire().await, "first 10 should pass immediately");
        }
        assert!(!limiter.try_acquire().await, "window exhausted");
        assert!(start.elapsed() < Duration::from_millis(50), "burst should be instant");
        // acquire 阻塞到下一窗口边界
        limiter.acquire().await;
        assert!(start.elapsed() >= Duration::from_millis(900), "waits for next window");
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
