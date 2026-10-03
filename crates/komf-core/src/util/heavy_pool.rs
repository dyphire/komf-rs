//! 重 IO 专用阻塞任务池 —— 与 API 请求的 tokio 默认阻塞池隔离。
//!
//! 背景：离线库下载/解压/建索引、ComicInfo 归档重写等重同步 IO 经
//! `tokio::task::spawn_blocking` 执行时，默认进入共享运行时阻塞池（上限 512 线程）。
//! 一旦批量任务（如库级 Auto-Identify 的 ComicInfo 重写）大量占用默认池线程，
//! 会与 API 请求竞争同一池，放大请求延迟。此处维护一个独立的多线程 runtime：
//! - `worker_threads(2)`：仅驱动 IO/timer reactor（任务体都在阻塞线程上跑）；
//! - `max_blocking_threads(8)`：有界，离线源更新已由全局信号量串行化，
//!   8 条阻塞线程足够覆盖"一个更新任务 + 若干惰性迁移/ComicInfo 重写"并发。
//!
//! 池为进程级常驻（`OnceLock`），不对每个任务新建线程。

use std::sync::OnceLock;

use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

static POOL: OnceLock<Runtime> = OnceLock::new();

fn pool() -> &'static Runtime {
    POOL.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(8)
            .thread_name("komf-heavy")
            .enable_all()
            .build()
            .expect("komf heavy pool runtime build failed")
    })
}

/// 在专用重 IO 池上执行同步闭包（等价 `tokio::task::spawn_blocking`，但池独立）。
/// 闭包内可用 `Handle::current().block_on(...)` 驱动异步体（如 `do_update`），
/// 此时 reactor/timer 均落在本池，不占 API 运行时。
pub fn spawn_heavy<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    pool().spawn_blocking(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_heavy_runs_closure() {
        let handle = spawn_heavy(|| 40 + 2);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(rt.block_on(handle).unwrap(), 42);
    }

    #[tokio::test]
    async fn spawn_heavy_supports_block_on_inside() {
        let handle = spawn_heavy(|| {
            tokio::runtime::Handle::current().block_on(async {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                7
            })
        });
        assert_eq!(handle.await.unwrap(), 7);
    }
}
