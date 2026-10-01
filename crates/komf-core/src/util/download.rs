//! 离线数据库下载重试公共工具 —— BookWalker / MangaBaka / Bangumi Archive / E-Hentai Archive 共用。
//!
//! 统一约定（从四处既有实现收敛，行为不变）：
//! - 指数退避 [`RETRY_DELAYS`]（`[5, 30, 120]`）：首次尝试 + 最多 3 次重试；
//! - 可重试：传输错误 / `429` / `5xx`；其余 `4xx` 直接失败（重试无意义）；
//! - 大文件 `Range` 断点续传：服务端回 `206` 则追加，`200` 则从头写，`416` 视为已完整；
//! - 大文件下载必须用 [`long_download_client`]（整体 3600s），勿用 60s 总超时的共享 client。

use std::fmt::Display;
use std::future::Future;
use std::path::Path;

/// 指数退避延迟（秒）：首次尝试失败后按序睡眠再重试。
pub const RETRY_DELAYS: [u64; 3] = [5, 30, 120];

/// 是否值得重试的状态码：`429` / `5xx`（对齐 Kotlin `HttpRequestRetry` 口径）。
pub fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// 大文件下载专用 client：整体超时 3600s + 连接超时 30s。
/// 背景：共享 client 带 60s 总超时，几百 MB 的离线库必在流式传输中途超时。
pub fn long_download_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HttpRequestException: {e}"))
}

/// 通用指数退避重试：`op` 每次调用为一次尝试（含首次），失败按 `delays` 睡眠后重试。
/// `tag` 仅用于日志（如 `"bangumi archive download"`），`E` 只需可打印。
pub async fn with_retry_delays<T, E, F, Fut>(tag: &str, delays: &[u64], mut op: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: Display,
{
    let mut attempt = 0usize;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt >= delays.len() {
                    return Err(e);
                }
                tracing::warn!(
                    "{tag} attempt {} failed: {e}; retrying in {}s",
                    attempt + 1,
                    delays[attempt]
                );
                tokio::time::sleep(std::time::Duration::from_secs(delays[attempt])).await;
                attempt += 1;
            }
        }
    }
}

/// [`with_retry_delays`] 默认延迟版本（[`RETRY_DELAYS`]）。
pub async fn with_retry<T, E, F, Fut>(tag: &str, op: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: Display,
{
    with_retry_delays(tag, &RETRY_DELAYS, op).await
}

/// GET 文本带重试：传输错误 / `429` / `5xx` 重试；其余 `4xx` 直接失败。
/// 成功返回 `trim()` 后的文本（checksum / JSON API 均适用，调用方再解析）。
pub async fn fetch_text_with_retry(
    client: &reqwest::Client,
    url: &str,
    tag: &str,
) -> Result<String, String> {
    let mut attempt = 0usize;
    loop {
        let result = async {
            let resp = client
                .get(url)
                .send()
                .await
                .map_err(|e| format!("HttpRequestException: {e}"))?;
            let status = resp.status();
            if status.is_success() {
                resp.text()
                    .await
                    .map(|t| t.trim().to_string())
                    .map_err(|e| format!("HttpRequestException: {e}"))
            } else if is_retryable_status(status) {
                let body = resp.text().await.unwrap_or_default();
                Err(format!("ResponseException: {status} {body}"))
            } else {
                // 不可重试的 4xx：直接失败，不进退避（重试无意义）。
                return Err(format!("ResponseException: {status}"));
            }
        }
        .await;
        match result {
            Ok(t) => return Ok(t),
            Err(e) => {
                if attempt >= RETRY_DELAYS.len() {
                    return Err(e);
                }
                tracing::warn!(
                    "{tag} GET {url} attempt {} failed: {e}; retrying in {}s",
                    attempt + 1,
                    RETRY_DELAYS[attempt]
                );
                tokio::time::sleep(std::time::Duration::from_secs(RETRY_DELAYS[attempt])).await;
                attempt += 1;
            }
        }
    }
}

/// 单次下载结果元数据。
#[derive(Debug, Clone, Default)]
pub struct DownloadMeta {
    /// 服务端 `Last-Modified`（续传命中 416 / 无响应头时为 `None`，调用方保留旧值）。
    pub last_modified: Option<String>,
    /// 完整大小（`Content-Length + 已有字节`，未知时为 0）。
    pub total_bytes: i64,
    /// 本次结束时本地总字节。
    pub completed_bytes: i64,
}

/// 单次文件下载（`Range` 断点续传，无重试）：`dest` 已存在部分视为已下载字节。
/// 每次写块后回调 `on_progress(total, completed)`（调用方映射为进度事件）。
pub async fn download_once(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    on_progress: &impl Fn(i64, i64),
) -> Result<DownloadMeta, String> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;
    let existing = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    let mut req = client.get(url);
    if existing > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let response = req
        .send()
        .await
        .map_err(|e| format!("HttpRequestException: {e}"))?;
    let status = response.status();
    // 416（本地已完整）视为成功。
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        let n = existing as i64;
        on_progress(n, n);
        return Ok(DownloadMeta {
            last_modified: None,
            total_bytes: n,
            completed_bytes: n,
        });
    }
    if !status.is_success() {
        return Err(format!("ResponseException: {status}"));
    }
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
    let base = if resumed { existing } else { 0 };
    // response 被 bytes_stream 消费后无法再读响应头，提前记录。
    let last_modified = response
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    // 服务端忽略 Range（200）：从头写，避免拼接坏文件。
    let mut file = if resumed {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(dest)
            .await
            .map_err(|e| format!("FileSystemException: {e}"))?
    } else {
        tokio::fs::File::create(dest)
            .await
            .map_err(|e| format!("FileSystemException: {e}"))?
    };
    let total = response
        .content_length()
        .map(|n| n as i64 + base as i64)
        .unwrap_or(0);
    on_progress(total, base as i64);
    let mut stream = response.bytes_stream();
    let mut completed = base as i64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("HttpRequestException: {e}"))?;
        completed += chunk.len() as i64;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("FileSystemException: {e}"))?;
        on_progress(total, completed);
    }
    file.flush()
        .await
        .map_err(|e| format!("FileSystemException: {e}"))?;
    drop(file);
    Ok(DownloadMeta {
        last_modified,
        total_bytes: total,
        completed_bytes: completed,
    })
}

/// 文件下载带重试 + 断点续传：首次 + [`RETRY_DELAYS`]；`on_retry(attempt_1based, delay_secs, err)`
/// 供调用方发射“即将重试”进度事件（无 UI 的后台任务可传空闭包）。
pub async fn download_with_retry(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    tag: &str,
    on_progress: &impl Fn(i64, i64),
    on_retry: &impl Fn(usize, u64, &str),
) -> Result<DownloadMeta, String> {
    let mut attempt = 0usize;
    loop {
        match download_once(client, url, dest, on_progress).await {
            Ok(meta) => return Ok(meta),
            Err(e) => {
                if attempt >= RETRY_DELAYS.len() {
                    return Err(e);
                }
                tracing::warn!(
                    "{tag} attempt {} failed: {e}; retrying in {}s",
                    attempt + 1,
                    RETRY_DELAYS[attempt]
                );
                on_retry(attempt + 1, RETRY_DELAYS[attempt], &e);
                tokio::time::sleep(std::time::Duration::from_secs(RETRY_DELAYS[attempt])).await;
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn retryable_status_classification() {
        assert!(is_retryable_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(is_retryable_status(reqwest::StatusCode::BAD_GATEWAY));
        assert!(!is_retryable_status(reqwest::StatusCode::OK));
        assert!(!is_retryable_status(reqwest::StatusCode::NOT_FOUND));
        assert!(!is_retryable_status(reqwest::StatusCode::BAD_REQUEST));
    }

    /// 两次失败后成功 → 返回成功且共尝试 3 次（零延迟，不拖慢单测）。
    #[tokio::test]
    async fn with_retry_succeeds_after_transient_failures() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let r = with_retry_delays("test", &[0, 0, 0], move || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err::<u32, String>("boom".to_string())
                } else {
                    Ok(n as u32)
                }
            }
        })
        .await;
        assert_eq!(r, Ok(2));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    /// 一直失败 → 首次 + 3 次重试共 4 次调用后返回错误。
    #[tokio::test]
    async fn with_retry_gives_up_after_delays_exhausted() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let r = with_retry_delays("test", &[0, 0, 0], move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<u32, String>("down".to_string())
            }
        })
        .await;
        assert_eq!(r, Err("down".to_string()));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }
}
