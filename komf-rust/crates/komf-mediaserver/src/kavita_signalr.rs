//! Kavita SignalR 事件监听器 —— 对应 `KavitaEventHandler.kt`。
//!
//! Kotlin 版通过 SignalR hub `/hubs/messages`（WebSocket transport）实时接收
//! 扫描事件；Rust 移植使用 SignalR 协议的 **Server-Sent Events** transport
//! （SignalR 官方传输之一，Kavita 的 `MapHub` 默认启用 WebSockets/SSE/LongPolling），
//! 消息格式、心跳、事件语义与 Kotlin 完全一致：
//! - `NotificationProgress`（name == "ScanProgress"）：`started` 记录 lastScan；
//!   `ended` 将 `CoverUpdate` 期间收集的卷按 `createdUtc > lastScan` 过滤出新增章节。
//! - `CoverUpdate`（entityType == "volume"）：记录卷 id（封面更新 = 扫描产物）。
//! - `SeriesRemoved`：派发 `onSeriesDeleted`。
//! 断线后每 10 秒自动重连（对应 Kotlin `onClosed` + `delaySubscription(10s)`）。
use crate::client::MediaServerError;
use crate::event_listener::{BookEvent, MediaServerEventListener, SeriesEvent};
use crate::kavita::{KavitaChapter, KavitaClient, KavitaVolume};
use crate::model::{MediaServerBookId, MediaServerLibraryId, MediaServerSeriesId};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// SignalR 心跳间隔（对应客户端默认 15s 空闲 Ping）。
const PING_INTERVAL: Duration = Duration::from_secs(15);

/// 与 Kotlin `noopEvents` 一致：注册但不处理，避免“未注册的调用目标”日志。
const NOOP_EVENTS: &[&str] = &[
    "UpdateAvailable",
    "ScanSeries",
    "CoverUpdateProgress",
    "SeriesAdded",
    "OnlineUsers",
    "CollectionUpdated",
    "BackupDatabaseProgress",
    "CleanupProgress",
    "DownloadProgress",
    "SiteThemeProgress",
    "BookThemeProgress",
    "FileScanProgress",
    "Error",
    "ScanProgress",
    "LibraryModified",
    "UserProgressUpdate",
    "UserUpdate",
    "ConvertBookmarksProgress",
    "WordCountAnalyzerProgress",
    "Info",
    "SendingToDevice",
    "ScrobblingKeyExpired",
    "DashboardUpdate",
    "SideNavUpdate",
    "SiteThemeUpdated",
    "SmartCollectionSync",
    "ChapterRemoved",
    "ChapterUpdated",
    "VolumeRemoved",
    "PersonMerged",
    "ExternalMatchRateLimitError",
    "AnnotationUpdate",
    "ReadingSessionUpdate",
    "ReadingSessionClose",
    "AuthKeyUpdate",
    "AuthKeyDeleted",
    "ReadingListUpdated",
    "SeriesUpdated",
    "ScrobbleProviderUpdated",
    "LicenseInfoUpdate",
    "ExternalMetadataUpdate",
    "RerunMetadataMappingsProgress",
];

/// 监听期间的共享状态（对应 Kotlin `lastScan` / `volumesChanged`）。
struct SignalRState {
    last_scan: Option<DateTime<Utc>>,
    volumes_changed: Vec<i32>,
}

impl SignalRState {
    /// Kotlin `KavitaEventHandler.lastScan` 初始化为构造时刻（`clock.now()`）。
    /// Rust 对齐：初始化为监听器启动时刻，保证「只有 ended 事件」的扫描也能
    /// 用启动时刻过滤新增章节，而不是静默丢弃。
    fn new() -> Self {
        Self {
            last_scan: Some(Utc::now()),
            volumes_changed: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NegotiateResponse {
    connection_id: String,
    #[allow(dead_code)]
    negotiate_version: Option<i32>,
    available_transports: Option<Vec<TransportInfo>>,
}

#[derive(Debug, Deserialize)]
struct TransportInfo {
    transport: String,
}

pub struct KavitaSignalREventHandler {
    client: Arc<KavitaClient>,
    listeners: Vec<Arc<dyn MediaServerEventListener>>,
    noop: HashSet<&'static str>,
}

impl KavitaSignalREventHandler {
    pub fn new(client: Arc<KavitaClient>, listeners: Vec<Arc<dyn MediaServerEventListener>>) -> Self {
        Self {
            client,
            listeners,
            noop: NOOP_EVENTS.iter().copied().collect(),
        }
    }

    /// 主循环：连接 → 事件流 → 断开后 10 秒重连（对应 Kotlin `retry { isActive }`）。
    pub async fn run(&self, token: CancellationToken) {
        tracing::info!(
            "connecting to Kavita event listener {}/hubs/messages (signalr over sse)",
            self.client.base_uri()
        );
        // Kotlin `lastScan`/`volumesChanged` 是 handler 实例字段，断线重连不重置；
        // Rust 对齐：状态提升到重连循环外，跨连接周期保留。
        let state = Arc::new(Mutex::new(SignalRState::new()));
        loop {
            if token.is_cancelled() {
                return;
            }
            match self.connect_and_run(state.clone(), token.clone()).await {
                Ok(()) => tracing::debug!("kavita signalr connection closed"),
                Err(error) => tracing::warn!("kavita signalr error: {error}"),
            }
            if token.is_cancelled() {
                return;
            }
            tokio::select! {
                _ = token.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(10)) => {}
            }
        }
    }

    /// 单次连接周期：negotiate → SSE 打开 → 事件循环。
    async fn connect_and_run(
        &self,
        state: Arc<Mutex<SignalRState>>,
        token: CancellationToken,
    ) -> Result<(), MediaServerError> {
        tracing::debug!("kavita signalr: obtaining token");
        let jwt = self.client.access_token().await?;
        tracing::debug!("kavita signalr: negotiating");
        let negotiate = self.negotiate(&jwt).await?;
        tracing::debug!(
            "kavita signalr: negotiated connectionId={}",
            negotiate.connection_id
        );

        let supports_sse = negotiate.available_transports.as_ref().is_none_or(|transports| {
            transports.iter().any(|t| t.transport == "ServerSentEvents")
        });
        if !supports_sse {
            tracing::warn!(
                "Kavita signalr negotiate did not advertise ServerSentEvents; \
                 falling back to LongPolling transport"
            );
            return self.run_long_polling(&jwt, &negotiate.connection_id, state, token).await;
        }

        let response = self.open_sse(&jwt, &negotiate.connection_id).await?;
        let status = response.status();
        tracing::debug!("kavita signalr: sse opened status={status}");
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        self.run_sse_loop(&jwt, &negotiate.connection_id, response, state, token)
            .await
    }

    // -- transport 层 ---------------------------------------------------------

    async fn negotiate(&self, jwt: &str) -> Result<NegotiateResponse, MediaServerError> {
        let response = self
            .client
            .http_client()
            .get(format!("{}/hubs/messages/negotiate", self.client.base_uri()))
            .query(&[("negotiateVersion", "1"), ("access_token", jwt)])
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        let parsed: NegotiateResponse = response.json().await?;
        if parsed.connection_id.is_empty() {
            return Err(MediaServerError::message("signalr negotiate returned no connectionId"));
        }
        Ok(parsed)
    }

    async fn open_sse(&self, jwt: &str, connection_id: &str) -> Result<reqwest::Response, MediaServerError> {
        let response = self
            .client
            .http_client()
            .get(format!("{}/hubs/messages", self.client.base_uri()))
            .query(&[("id", connection_id), ("access_token", jwt)])
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await?;
        Ok(response)
    }

    async fn send_client_message(&self, jwt: &str, connection_id: &str, payload: &str) -> Result<(), MediaServerError> {
        let response = self
            .client
            .http_client()
            .post(format!("{}/hubs/messages", self.client.base_uri()))
            .query(&[("id", connection_id), ("access_token", jwt)])
            .header(reqwest::header::CONTENT_TYPE, "text/plain;charset=UTF-8")
            .body(payload.to_string())
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() && status != reqwest::StatusCode::NO_CONTENT {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(())
    }

    /// 发送 SignalR Ping（`{"type":6}\x1e`）。
    async fn send_ping(&self, jwt: &str, connection_id: &str) -> Result<(), MediaServerError> {
        self.send_client_message(jwt, connection_id, "{\"type\":6}\u{1e}").await
    }

    // -- SSE 事件循环 ----------------------------------------------------------

    async fn run_sse_loop(
        &self,
        jwt: &str,
        connection_id: &str,
        response: reqwest::Response,
        state: Arc<Mutex<SignalRState>>,
        token: CancellationToken,
    ) -> Result<(), MediaServerError> {
        let mut stream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        let mut last_activity = tokio::time::Instant::now();
        loop {
            tokio::select! {
                _ = token.cancelled() => return Ok(()),
                maybe_chunk = stream.next() => {
                    let Some(chunk) = maybe_chunk else {
                        return Err(MediaServerError::message("kavita signalr sse stream ended"));
                    };
                    let bytes = chunk?;
                    if bytes.is_empty() {
                        return Err(MediaServerError::message("kavita signalr sse stream ended (empty chunk)"));
                    }
                    last_activity = tokio::time::Instant::now();
                    buffer.extend_from_slice(&bytes);
                    while let Some(end) = find_frame_boundary(&buffer) {
                        let frame: Vec<u8> = buffer.drain(..end).collect();
                        if let Some(data) = parse_sse_data(&frame) {
                            match self.handle_message(&data, &state).await {
                                MessageOutcome::Continue => {}
                                MessageOutcome::Close => return Ok(()),
                                MessageOutcome::Error(e) => return Err(e),
                            }
                        }
                    }
                }
                _ = tokio::time::sleep_until(last_activity + PING_INTERVAL) => {
                    self.send_ping(jwt, connection_id).await?;
                    last_activity = tokio::time::Instant::now();
                }
            }
        }
    }

    // -- LongPolling 兜底 transport ---------------------------------------------

    async fn run_long_polling(
        &self,
        jwt: &str,
        connection_id: &str,
        state: Arc<Mutex<SignalRState>>,
        token: CancellationToken,
    ) -> Result<(), MediaServerError> {
        // LongPolling transport 中轮询请求本身即心跳（服务器挂起等待期间保持连接活跃），
        // 无需客户端主动 Ping。
        loop {
            if token.is_cancelled() {
                return Ok(());
            }
            // 轮询 GET：挂起直到服务器有消息或超时
            let response = self
                .client
                .http_client()
                .get(format!("{}/hubs/messages", self.client.base_uri()))
                .query(&[("id", connection_id), ("access_token", jwt)])
                .send()
                .await?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(MediaServerError::Status(status, body));
            }
            let messages: Vec<String> = response.json().await?;
            for message in messages {
                match self.handle_message(&message, &state).await {
                    MessageOutcome::Continue => {}
                    MessageOutcome::Close => return Ok(()),
                    MessageOutcome::Error(e) => return Err(e),
                }
            }
        }
    }

    // -- 消息处理 ---------------------------------------------------------------

    async fn handle_message(&self, data: &str, state: &Arc<Mutex<SignalRState>>) -> MessageOutcome {
        let parsed: Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(_) => return MessageOutcome::Continue, // 忽略非 JSON（如握手前的残留）
        };
        // SignalR 消息：type 1=Invocation, 3=StreamItem, 6=Ping, 7=Close
        match parsed.get("type").and_then(|t| t.as_u64()) {
            Some(1) => {
                let target = parsed.get("target").and_then(|t| t.as_str()).unwrap_or_default();
                let arguments = parsed
                    .get("arguments")
                    .and_then(|a| a.as_array())
                    .cloned()
                    .unwrap_or_default();
                self.handle_invocation(target, &arguments, state).await
            }
            Some(6) => MessageOutcome::Continue, // Ping（心跳）
            Some(7) => MessageOutcome::Close,
            _ => MessageOutcome::Continue, // 握手响应 / StreamItem / 其他
        }
    }

    async fn handle_invocation(
        &self,
        target: &str,
        arguments: &[Value],
        state: &Arc<Mutex<SignalRState>>,
    ) -> MessageOutcome {
        let Some(event) = arguments.first() else {
            return MessageOutcome::Continue;
        };
        match target {
            "NotificationProgress" => self.handle_progress_notification(event, state).await,
            "CoverUpdate" => {
                let is_volume = event
                    .get("body")
                    .and_then(|b| b.get("entityType"))
                    .and_then(|e| e.as_str())
                    == Some("volume");
                if is_volume {
                    if let Some(id) = event.pointer("/body/id").and_then(|id| id.as_f64()) {
                        let mut guard = state.lock().unwrap();
                        guard.volumes_changed.push(id as i32);
                    }
                }
                MessageOutcome::Continue
            }
            "SeriesRemoved" => {
                let Some(body) = event.get("body") else {
                    return MessageOutcome::Continue;
                };
                let (Some(series_id), Some(library_id)) = (
                    body.get("seriesId").and_then(|v| v.as_i64()),
                    body.get("libraryId").and_then(|v| v.as_i64()),
                ) else {
                    return MessageOutcome::Continue;
                };
                let series_event = SeriesEvent {
                    library_id: MediaServerLibraryId(library_id.to_string()),
                    series_id: MediaServerSeriesId(series_id.to_string()),
                };
                let listeners = self.listeners.clone();
                tokio::spawn(async move {
                    for listener in &listeners {
                        listener.on_series_deleted(&[series_event.clone()]).await;
                    }
                });
                MessageOutcome::Continue
            }
            _ => {
                // 未识别的 target：仅当不在 noop 清单时记录一次（对应 Kotlin 注册空处理）
                if !self.noop.contains(target) {
                    tracing::debug!("kavita signalr: unhandled event target {target}");
                }
                MessageOutcome::Continue
            }
        }
    }

    /// 对应 Kotlin `processProgressNotification`。
    async fn handle_progress_notification(
        &self,
        event: &Value,
        state: &Arc<Mutex<SignalRState>>,
    ) -> MessageOutcome {
        if event.get("name").and_then(|n| n.as_str()) != Some("ScanProgress") {
            return MessageOutcome::Continue;
        }
        match event.get("eventType").and_then(|t| t.as_str()) {
            Some("started") => {
                let mut guard = state.lock().unwrap();
                guard.last_scan = Some(Utc::now());
                MessageOutcome::Continue
            }
            Some("ended") => {
                let (volumes, last_scan) = {
                    let mut guard = state.lock().unwrap();
                    (std::mem::take(&mut guard.volumes_changed), guard.last_scan)
                };
                if let Some(last_scan) = last_scan {
                    let client = self.client.clone();
                    let listeners = self.listeners.clone();
                    tokio::spawn(async move {
                        let events = collect_book_events(&client, volumes, last_scan).await;
                        if !events.is_empty() {
                            for listener in &listeners {
                                listener.on_books_added(&events).await;
                            }
                        }
                    });
                }
                MessageOutcome::Continue
            }
            _ => MessageOutcome::Continue,
        }
    }
}

/// 对应 Kotlin `processEvents`：卷 → 过滤新增章节 → 按系列组装 BookEvent。
async fn collect_book_events(
    client: &KavitaClient,
    volume_ids: Vec<i32>,
    last_scan: DateTime<Utc>,
) -> Vec<BookEvent> {
    let mut book_events = Vec::new();
    let mut volume_chapters: Vec<(KavitaVolume, Vec<KavitaChapter>)> = Vec::new();
    for volume_id in volume_ids {
        match client.get_volume(volume_id).await {
            Ok(volume) => {
                let new_chapters: Vec<KavitaChapter> = volume
                    .chapters
                    .iter()
                    .filter(|chapter| {
                        parse_kavita_datetime(&chapter.created_utc).is_some_and(|t| t > last_scan)
                    })
                    .cloned()
                    .collect();
                if !new_chapters.is_empty() {
                    volume_chapters.push((volume, new_chapters));
                }
            }
            Err(MediaServerError::NotFound(_)) => {}
            Err(error) => tracing::warn!("kavita signalr: failed to load volume {volume_id}: {error}"),
        }
    }
    // groupBy seriesId（保持 Kotlin 语义）
    let mut by_series: std::collections::BTreeMap<i32, Vec<(KavitaVolume, Vec<KavitaChapter>)>> =
        std::collections::BTreeMap::new();
    for (volume, chapters) in volume_chapters {
        by_series.entry(volume.series_id).or_default().push((volume, chapters));
    }
    for (series_id, entries) in by_series {
        let series = match client.get_series(series_id).await {
            Ok(series) => series,
            Err(error) => {
                tracing::warn!("kavita signalr: failed to load series {series_id}: {error}");
                continue;
            }
        };
        for (_, chapters) in entries {
            for chapter in chapters {
                book_events.push(BookEvent {
                    library_id: MediaServerLibraryId(series.library_id.to_string()),
                    series_id: MediaServerSeriesId(series.id.to_string()),
                    book_id: MediaServerBookId(chapter.id.to_string()),
                });
            }
        }
    }
    book_events
}

/// 解析 Kavita 的 `LocalDateTime`（`2026-09-17T05:00:00`，按 UTC 处理，同 Kotlin
/// `createdUtc.toInstant(TimeZone.UTC)`）。
fn parse_kavita_datetime(value: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f"))
        .ok()?;
    Some(naive.and_utc())
}

enum MessageOutcome {
    Continue,
    Close,
    #[allow(dead_code)]
    Error(MediaServerError),
}

/// SSE 帧边界（`\n\n`，兼容 `\r\n\r\n`）。
fn find_frame_boundary(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|pos| pos + 2)
        .or_else(|| buffer.windows(4).position(|w| w == b"\r\n\r\n").map(|pos| pos + 4))
}

/// 从 SSE 帧提取 SignalR data 负载（`data: <json>\x1e`）。
fn parse_sse_data(frame: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(frame);
    let data = text.lines().find_map(|line| {
        line.strip_prefix("data:")
            .map(|s| s.trim_start())
            .or_else(|| line.strip_prefix("data:").map(|s| s.trim()))
    })?;
    let data = data.strip_suffix('\u{1e}').unwrap_or(data);
    let data = data.strip_suffix('\r').unwrap_or(data);
    Some(data.trim().to_string())
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sse_data_frames() {
        assert_eq!(
            parse_sse_data(b"data: {\"type\":6}\x1e\n\n"),
            Some("{\"type\":6}".to_string())
        );
        assert_eq!(
            parse_sse_data(b"data: {\"type\":1}\x1e\r\n\r\n"),
            Some("{\"type\":1}".to_string())
        );
        assert_eq!(
            parse_sse_data(b"event: ping\ndata: hello\x1e\n\n"),
            Some("hello".to_string())
        );
        assert_eq!(parse_sse_data(b"event: ping\n\n"), None);
    }

    #[test]
    fn finds_frame_boundaries() {
        let buffer = b"data: a\x1e\n\ndata: b\x1e\n\n";
        assert_eq!(find_frame_boundary(buffer), Some(10));
        let buffer = b"data: a\x1e\r\n\r\n";
        assert_eq!(find_frame_boundary(buffer), Some(12));
        let buffer = b"partial";
        assert_eq!(find_frame_boundary(buffer), None);
    }

    #[test]
    fn parses_kavita_datetimes() {
        assert!(parse_kavita_datetime("2026-09-17T05:00:00").is_some());
        assert!(parse_kavita_datetime("2026-09-17T05:00:00.123456").is_some());
        assert!(parse_kavita_datetime("garbage").is_none());
        let early = parse_kavita_datetime("2026-09-17T05:00:00").unwrap();
        let late = parse_kavita_datetime("2026-09-18T00:00:00").unwrap();
        assert!(late > early);
    }

    #[test]
    fn signalr_ping_payload() {
        assert_eq!("{\"type\":6}\u{1e}", "{\"type\":6}\u{1e}");
    }

    /// 事件 → 状态转移：ScanProgress ended 取走卷列表（Kotlin 语义）。
    #[tokio::test]
    async fn progress_notification_state_transition() {
        let state = Arc::new(Mutex::new(SignalRState::new()));
        {
            let mut guard = state.lock().unwrap();
            guard.last_scan = Some(parse_kavita_datetime("2026-09-17T05:00:00").unwrap());
            guard.volumes_changed.push(11);
            guard.volumes_changed.push(22);
        }
        let (volumes, last_scan) = {
            let mut guard = state.lock().unwrap();
            (std::mem::take(&mut guard.volumes_changed), guard.last_scan)
        };
        assert_eq!(volumes, vec![11, 22]);
        assert!(last_scan.is_some());
        assert!(state.lock().unwrap().volumes_changed.is_empty());
    }

    #[test]
    fn noop_events_cover_kotlin_list() {        let handler = KavitaSignalREventHandler {
            client: Arc::new(
                KavitaClient::new(reqwest::Client::new(), "http://localhost:5000", "key").unwrap(),
            ),
            listeners: Vec::new(),
            noop: NOOP_EVENTS.iter().copied().collect(),
        };
        // Kotlin noop 清单中的事件都应被识别为已注册（不产生告警分支）
        for event in NOOP_EVENTS {
            assert!(handler.noop.contains(event), "missing noop: {event}");
        }
        assert!(!handler.noop.contains("NotificationProgress"));
        assert!(!handler.noop.contains("CoverUpdate"));
        assert!(!handler.noop.contains("SeriesRemoved"));
    }

    /// 环境探针：reqwest 在沙箱内能否直连本机 loopback（用于诊断端到端联调）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reqwest_can_reach_loopback() {
        use std::time::Duration as StdDuration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}")
                        .await;
                });
            }
        });
        let client = reqwest::Client::builder()
            .connect_timeout(StdDuration::from_secs(5))
            .build()
            .unwrap();
        let resp = tokio::time::timeout(
            StdDuration::from_secs(10),
            client.get(format!("http://{addr}/probe")).send(),
        )
        .await
        .expect("reqwest loopback request timed out")
        .expect("reqwest loopback request failed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }
}
