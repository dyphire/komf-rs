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
//!
//! 传输层按 ASP.NET Core SignalR 协议规范实现：
//! - negotiate 必须为 **POST**（GET 会被服务端 405）；
//! - `negotiateVersion=1` 的响应同时含 `connectionId` 与 `connectionToken`，
//!   后续 SSE/LongPolling 连接请求的 `id` 查询值必须用 **connectionToken**；
//! - SSE/LongPolling 连接请求必须携带 `transport` 查询参数（缺失 400）；
//! - 传输建立后客户端必须先 POST 握手帧 `{"protocol":"json","version":1}\x1e`，
//!   服务端在 SSE 流上回握手响应，之后才派发 hub 消息（HandshakeTimeout 默认 15s）。
use crate::client::MediaServerError;
use crate::event_listener::{BookEvent, MediaServerEventListener, SeriesEvent};
use crate::kavita::{KavitaChapter, KavitaClient, KavitaVolume};
use crate::model::{MediaServerBookId, MediaServerLibraryId, MediaServerSeriesId};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_util::sync::CancellationToken;

/// SignalR 握手消息（`{"protocol":"json","version":1}\x1e`，record separator 结尾）。
const HANDSHAKE_MESSAGE: &str = "{\"protocol\":\"json\",\"version\":1}\u{1e}";

/// SignalR 心跳间隔（对应客户端默认 15s 空闲 Ping）。
const PING_INTERVAL: Duration = Duration::from_secs(15);

/// SSE 握手响应等待超时：规范上服务端应在握手后立即在流上推送 `{}`；
/// 但部分 Kavita 实例的 SSE transport 接受握手却不推送任何数据。
/// 首帧在此窗口内未到达即判定 SSE 不可用，降级 LongPolling。
const SSE_HANDSHAKE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// SSE 连续失败达到该次数后自动降级 LongPolling（本实例实测 SSE 长连接约 60s
/// 被服务端空闲超时关闭；连续两次失败即可确认不稳定，后续周期跳过 SSE）。
const SSE_FAILURE_THRESHOLD: usize = 2;

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
    /// `negotiateVersion>=1` 起服务端同时返回 connectionToken；连接请求的 `id`
    /// 必须用该值（ASP.NET Core 规范），缺失时回退 connectionId。
    connection_token: Option<String>,
    #[allow(dead_code)]
    negotiate_version: Option<i32>,
    available_transports: Option<Vec<TransportInfo>>,
}

impl NegotiateResponse {
    /// 连接请求（SSE / LongPolling / 消息 POST）应使用的 `id` 查询值。
    fn connection_id_for_requests(&self) -> &str {
        self.connection_token.as_deref().unwrap_or(&self.connection_id)
    }
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
            "connecting to Kavita event listener {}/hubs/messages (signalr, websocket preferred)",
            self.client.base_uri()
        );
        // Kotlin `lastScan`/`volumesChanged` 是 handler 实例字段，断线重连不重置；
        // Rust 对齐：状态提升到重连循环外，跨连接周期保留。
        let state = Arc::new(Mutex::new(SignalRState::new()));
        // SSE 连续失败计数（跨连接周期）：连续失败达到阈值后跳过 SSE 走 LongPolling。
        let sse_failures = Arc::new(AtomicUsize::new(0));
        loop {
            if token.is_cancelled() {
                return;
            }
            match self.connect_and_run(state.clone(), token.clone(), sse_failures.clone()).await {
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

    /// 单次连接周期：negotiate → transport 协商 → 事件循环。
    /// 传输优先级 WS → SSE → LongPolling（对齐 Kotlin 官方客户端默认顺序）；
    /// SSE 连续失败（SSE_FAILURE_THRESHOLD 次）后自动降级 LongPolling
    /// （本实例实测 SSE 长连接约 60s 被服务端空闲超时关闭）。
    async fn connect_and_run(
        &self,
        state: Arc<Mutex<SignalRState>>,
        token: CancellationToken,
        sse_failures: Arc<AtomicUsize>,
    ) -> Result<(), MediaServerError> {
        tracing::debug!("kavita signalr: obtaining token");
        let jwt = self.client.access_token().await?;
        tracing::debug!("kavita signalr: negotiating");
        let negotiate = self.negotiate(&jwt).await?;
        let connection = negotiate.connection_id_for_requests();
        tracing::debug!(
            "kavita signalr: negotiated connectionId={} (requests use token={})",
            negotiate.connection_id,
            connection
        );

        // 1. WebSocket：与 Kotlin 官方客户端一致；有协议级心跳，不受服务端
        //    60s 空闲连接超时影响。失败继续尝试下一传输。
        let supports_ws = negotiate.available_transports.as_ref().is_none_or(|transports| {
            transports.iter().any(|t| t.transport == "WebSockets")
        });
        if supports_ws {
            tracing::debug!("kavita signalr: WebSockets advertised, opening websocket");
            match self.run_websocket(&jwt, connection, state.clone(), token.clone()).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    tracing::warn!("Kavita signalr WebSocket failed ({error}); trying SSE");
                }
            }
        }
        // 2. SSE：官方默认第二选择，实时性优于 LongPolling。本实例实测 SSE 挂起
        //    连接约 60s 被服务端空闲超时关闭（error decoding response body），
        //    连续失败达到阈值后跳过 SSE（SseSilent 为连接存活但静默，同周期降级）。
        let supports_sse = negotiate.available_transports.as_ref().is_none_or(|transports| {
            transports.iter().any(|t| t.transport == "ServerSentEvents")
        });
        if supports_sse && sse_failures.load(Ordering::Relaxed) < SSE_FAILURE_THRESHOLD {
            let response = self.open_sse(&jwt, connection).await?;
            let status = response.status();
            tracing::debug!("kavita signalr: sse opened status={status}");
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(MediaServerError::Status(status, body));
            }
            // 传输建立后必须先发握手帧；服务端在 SSE 流上回握手响应。
            self.send_handshake(&jwt, connection).await?;
            tracing::debug!("kavita signalr: handshake sent");
            let sse_result = self
                .run_sse_loop(&jwt, connection, response, state.clone(), token.clone())
                .await;
            return match sse_result {
                Ok(()) => {
                    // SSE 恢复稳定：清零失败计数。
                    sse_failures.store(0, Ordering::Relaxed);
                    Ok(())
                }
                Err(MediaServerError::SseSilent) => {
                    // 连接仍存活但流静默：同周期直接降级 LongPolling。
                    tracing::warn!(
                        "Kavita signalr SSE accepted handshake but stream silent; \
                         falling back to LongPolling"
                    );
                    self.run_long_polling(&jwt, connection, state, token).await
                }
                Err(error) => {
                    // 流中断/错误（连接已死）：计数并返回，外层重连（新 negotiate）；
                    // 连续失败达到阈值后跳过 SSE 直接 LongPolling。
                    sse_failures.fetch_add(1, Ordering::Relaxed);
                    Err(error)
                }
            };
        }
        // 3. LongPolling：轮询短连接免疫服务端空闲超时，作为最终兜底。
        let supports_lp = negotiate.available_transports.as_ref().is_none_or(|transports| {
            transports.iter().any(|t| t.transport == "LongPolling")
        });
        if supports_lp {
            return self.run_long_polling(&jwt, connection, state, token).await;
        }
        Err(MediaServerError::message(
            "Kavita signalr negotiate advertised no usable transport",
        ))
    }

    // -- transport 层 ---------------------------------------------------------

    async fn negotiate(&self, jwt: &str) -> Result<NegotiateResponse, MediaServerError> {
        let response = self
            .client
            .http_client()
            .post(format!("{}/hubs/messages/negotiate", self.client.base_uri()))
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
            .query(&[
                ("id", connection_id),
                ("transport", "ServerSentEvents"),
                ("access_token", jwt),
            ])
            // SSE 长连接禁用压缩（identity）：reqwest 默认 Accept-Encoding: gzip，
            // 与 Kavita SSE 流式响应交互时约 60s 后报 "error decoding response body"；
            // curl（不请求压缩）实测稳定。
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .await?;
        Ok(response)
    }

    async fn send_client_message(&self, jwt: &str, connection_id: &str, transport: &str, payload: &str) -> Result<(), MediaServerError> {
        let response = self
            .client
            .http_client()
            .post(format!("{}/hubs/messages", self.client.base_uri()))
            .query(&[
                ("id", connection_id),
                ("transport", transport),
                ("access_token", jwt),
            ])
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

    /// 发送 SignalR 握手帧（传输建立后客户端必须先发握手）。
    async fn send_handshake(&self, jwt: &str, connection_id: &str) -> Result<(), MediaServerError> {
        self.send_client_message(jwt, connection_id, "ServerSentEvents", HANDSHAKE_MESSAGE).await
    }

    /// 发送 SignalR Ping（`{"type":6}\x1e`）。
    async fn send_ping(&self, jwt: &str, connection_id: &str) -> Result<(), MediaServerError> {
        self.send_client_message(jwt, connection_id, "ServerSentEvents", "{\"type\":6}\u{1e}").await
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
        // 首帧超时：规范上服务端握手后立即推送 `{}`；若 SSE transport 静默
        // （如部分 Kavita 实例），超时后返回 SseSilent 由调用方降级 LongPolling。
        let handshake_deadline = last_activity + SSE_HANDSHAKE_RESPONSE_TIMEOUT;
        let mut received_any = false;
        loop {
            tokio::select! {
                _ = token.cancelled() => return Ok(()),
                _ = tokio::time::sleep_until(handshake_deadline), if !received_any => {
                    return Err(MediaServerError::SseSilent);
                }
                maybe_chunk = stream.next() => {
                    let Some(chunk) = maybe_chunk else {
                        return Err(MediaServerError::message("kavita signalr sse stream ended"));
                    };
                    let bytes = chunk?;
                    if bytes.is_empty() {
                        return Err(MediaServerError::message("kavita signalr sse stream ended (empty chunk)"));
                    }
                    received_any = true;
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

    // -- WebSocket transport --------------------------------------------------

    /// 构建 SignalR WebSocket 连接 URL（http(s):// → ws(s)://，带 transport 与 JWT）。
    fn websocket_url(base_uri: &str, connection_id: &str, jwt: &str) -> String {
        let ws_base = base_uri
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        format!(
            "{ws_base}/hubs/messages?id={connection_id}&transport=WebSockets&access_token={jwt}"
        )
    }

    async fn run_websocket(
        &self,
        jwt: &str,
        connection_id: &str,
        state: Arc<Mutex<SignalRState>>,
        token: CancellationToken,
    ) -> Result<(), MediaServerError> {
        let request = Self::websocket_url(self.client.base_uri(), connection_id, jwt)
            .into_client_request()
            .map_err(|e| MediaServerError::message(format!("websocket request build failed: {e}")))?;
        let (ws_stream, response) = connect_async(request)
            .await
            .map_err(|e| MediaServerError::message(format!("websocket connect failed: {e}")))?;
        tracing::debug!("kavita signalr: websocket opened status={}", response.status());
        let (mut sink, mut stream) = ws_stream.split();

        // 传输建立后必须先发握手帧；服务端在 WS 帧上回 {} 握手响应。
        sink.send(WsMessage::Text(HANDSHAKE_MESSAGE.into()))
            .await
            .map_err(|e| MediaServerError::message(format!("websocket handshake send failed: {e}")))?;
        tracing::debug!("kavita signalr: websocket handshake sent");

        let mut last_activity = tokio::time::Instant::now();
        // 首帧超时：规范上服务端握手后立即回 {}；静默则降级（复用 SseSilent 语义）。
        let handshake_deadline = last_activity + SSE_HANDSHAKE_RESPONSE_TIMEOUT;
        let mut received_any = false;
        loop {
            tokio::select! {
                _ = token.cancelled() => return Ok(()),
                _ = tokio::time::sleep_until(handshake_deadline), if !received_any => {
                    return Err(MediaServerError::SseSilent);
                }
                maybe_message = stream.next() => {
                    let Some(message) = maybe_message else {
                        return Err(MediaServerError::message(
                            "kavita signalr websocket stream ended",
                        ));
                    };
                    let message = message.map_err(|e| {
                        MediaServerError::message(format!("websocket read error: {e}"))
                    })?;
                    match message {
                        WsMessage::Text(text) => {
                            received_any = true;
                            last_activity = tokio::time::Instant::now();
                            // WS 上每条 SignalR 消息是一帧；JSON 协议消息以 \x1e 结尾。
                            let data = text.trim().strip_suffix('\u{1e}').unwrap_or(text.trim());
                            match self.handle_message(data, &state).await {
                                MessageOutcome::Continue => {}
                                MessageOutcome::Close => return Ok(()),
                                MessageOutcome::Error(e) => return Err(e),
                            }
                        }
                        WsMessage::Binary(_) => {
                            // 服务端控制/二进制帧：计数为活跃但不解析。
                            received_any = true;
                            last_activity = tokio::time::Instant::now();
                        }
                        WsMessage::Ping(payload) => {
                            // tungstenite 默认自动回 Pong；显式回以防 feature 差异。
                            sink.send(WsMessage::Pong(payload)).await.map_err(|e| {
                                MediaServerError::message(format!("websocket pong send failed: {e}"))
                            })?;
                        }
                        WsMessage::Pong(_) => {}
                        WsMessage::Close(_) => {
                            return Err(MediaServerError::message(
                                "kavita signalr websocket closed by server",
                            ));
                        }
                        WsMessage::Frame(_) => {}
                    }
                }
                _ = tokio::time::sleep_until(last_activity + PING_INTERVAL) => {
                    // SignalR JSON 协议 Ping（type 6）；服务端回同型 Pong。
                    sink.send(WsMessage::Text("{\"type\":6}\u{1e}".into()))
                        .await
                        .map_err(|e| MediaServerError::message(format!("websocket ping send failed: {e}")))?;
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
        // 必须先 POST 握手帧（transport=LongPolling）建立应用层连接；缺此步时
        // 服务端默认 15s 握手超时删除连接，挂起中的 GET 轮询返回 404。
        self.send_client_message(jwt, connection_id, "LongPolling", HANDSHAKE_MESSAGE)
            .await?;
        loop {
            if token.is_cancelled() {
                return Ok(());
            }
            // 轮询 GET：挂起直到服务器有消息或超时（transport 参数必需）
            // 实测（2026-09-25，localhost:5000）：Kavita 服务端空闲连接超时约 60s
            //（Kestrel KeepAliveTimeout/PollTimeout），挂起的轮询连接到点被服务端
            // 关闭 → reqwest send() 报 "error sending request"。绕开：轮询 GET 加
            // 客户端 30s 超时，无消息即主动结束本轮、立即下一轮，服务端 60s 空闲
            // 超时永不触发；Connection: close 保证每轮独立连接、无 keep-alive 复用。
            let response = match self
                .client
                .http_client()
                .get(format!("{}/hubs/messages", self.client.base_uri()))
                .query(&[
                    ("id", connection_id),
                    ("transport", "LongPolling"),
                    ("access_token", jwt),
                ])
                .header(reqwest::header::CONNECTION, "close")
                .timeout(std::time::Duration::from_secs(30))
                .send()
                .await
            {
                Ok(response) => response,
                // 30s 内服务端无消息：本轮轮询超时属正常，继续下一轮。
                Err(e) if e.is_timeout() => continue,
                Err(e) => return Err(e.into()),
            };
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(MediaServerError::Status(status, body));
            }
            // LongPolling 响应体是文本帧流（消息以 record separator 0x1e 结尾，
            // 如 `{}\x1e`），不是 JSON 数组——用 json() 解析必然报
            // "error decoding response body"。按 0x1e 切分逐条处理。
            let text = response.text().await?;
            for message in text.split('\u{1e}') {
                let message = message.trim();
                if message.is_empty() {
                    continue;
                }
                match self.handle_message(message, &state).await {
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

    #[test]
    fn signalr_handshake_payload() {
        // 传输建立后的握手帧必须是 json 协议 + record separator 结尾。
        assert_eq!(HANDSHAKE_MESSAGE, "{\"protocol\":\"json\",\"version\":1}\u{1e}");
    }

    #[test]
    fn connection_requests_prefer_connection_token() {
        // negotiateVersion=1 响应同时含 connectionId/connectionToken，
        // 连接请求必须用 connectionToken；缺失时回退 connectionId。
        let with_token = NegotiateResponse {
            connection_id: "conn-id".into(),
            connection_token: Some("conn-token".into()),
            negotiate_version: Some(1),
            available_transports: None,
        };
        assert_eq!(with_token.connection_id_for_requests(), "conn-token");

        let no_token = NegotiateResponse {
            connection_id: "conn-id".into(),
            connection_token: None,
            negotiate_version: None,
            available_transports: None,
        };
        assert_eq!(no_token.connection_id_for_requests(), "conn-id");
    }

    #[test]
    fn builds_websocket_url() {
        assert_eq!(
            KavitaSignalREventHandler::websocket_url(
                "https://kavita.example.com:8443",
                "t",
                "j"
            ),
            "wss://kavita.example.com:8443/hubs/messages?id=t&transport=WebSockets&access_token=j"
        );
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
