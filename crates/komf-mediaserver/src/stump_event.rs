//! Stump 事件监听器 —— WS 订阅 GraphQL `readEvents`（Rust 扩展）。
//!
//! Stump 无 Komga 式 SSE；事件经 GraphQL 订阅（graphql-transport-ws 协议）推送，
//! 端点 `ws(s)://{base}/api/graphql/ws`。认证在 HTTP 升级请求头完成
//! （`Authorization: Bearer <api-key|jwt>`，Stump 的 `auth_middleware` 对 /ws 同样生效）。
//!
//! 事件语义映射（近似 Komga `BookAdded`）：
//! - `CreatedMedia { id, seriesId, libraryId }` → `on_books_added`（单本，含真实 book id）；
//! - `CreatedOrUpdatedManyMedia { count, seriesId, libraryId }` → `on_books_added`
//!   （扫描中某系列有新增/更新书籍；事件无 book id 列表，派发时按 seriesId 拉书籍补真实 id）；
//! - `CreatedManySeries { count, libraryId }` → 无系列 id 列表，无法定位系列，忽略（记日志）；
//! - `JobStarted` / `JobUpdate` 仅用于批量聚合窗口（见 `EventBatcher`）。
//!
//! 批量聚合（对齐 Komga `TaskQueueStatus count==0`）：`JobStarted` 打开窗口，
//! Created* 事件入缓冲；最后一个活跃任务以终态（COMPLETED/FAILED/CANCELLED）
//! 结束时统一派发 —— 一次扫描只产生一批匹配任务，避免逐事件小 job 风暴。
//! 无活跃任务时的事件（异常路径）立即派发。
//!
//! 断线后每 10 秒自动重连（缓冲随断线丢弃，重连后重新开窗），直到 token 取消。
use crate::client::MediaServerError;
use crate::event_listener::{BookEvent, MediaServerEventListener};
use crate::model::{MediaServerBookId, MediaServerLibraryId, MediaServerSeriesId};
use crate::stump::{StumpClient, StumpMediaDto};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

/// 断线重连退避（对齐 Komga/Kavita 10 秒）。
const RECONNECT_DELAY: Duration = Duration::from_secs(10);

/// 缓冲上限：任务未正常终结时的安全阀（超过即强制派发，防事件堆积）。
const MAX_BUFFER: usize = 500;

/// readEvents 订阅文档（union 用内联片段取所需字段；Job* 字段仅用于批量窗口）。
const READ_EVENTS_DOC: &str = r#"subscription ReadEvents {
  readEvents {
    __typename
    ... on JobStarted { id }
    ... on JobUpdate { id status }
    ... on CreatedMedia { id seriesId libraryId }
    ... on CreatedOrUpdatedManyMedia { count seriesId libraryId }
    ... on CreatedManySeries { count libraryId }
  }
}"#;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CoreEventDto {
    #[serde(rename = "__typename")]
    typename: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    count: Option<i32>,
    #[serde(default)]
    series_id: Option<String>,
    #[serde(default)]
    library_id: Option<String>,
}

/// 批量聚合状态机（纯逻辑，可单测）。
struct EventBatcher {
    /// 进行中的任务（JobStarted 未终结）。Stump 事件不携带 jobId，无法把 Created*
    /// 归到具体任务，故用"窗口期"整体缓冲（Komga TaskQueueStatus 同语义）。
    active_jobs: HashSet<String>,
    /// 窗口期内的 Created* 事件。
    buffer: Vec<CoreEventDto>,
}

impl EventBatcher {
    fn new() -> Self {
        Self {
            active_jobs: HashSet::new(),
            buffer: Vec::new(),
        }
    }

    /// `JobStarted` → 开窗。
    fn on_job_started(&mut self, id: &str) {
        if self.active_jobs.insert(id.to_string()) {
            tracing::debug!("stump job started {id}; opening event batch window");
        }
    }

    /// `JobUpdate` 终态 → 关窗。返回 true 表示所有任务已结束、应派发缓冲。
    fn on_job_update(&mut self, id: &str, status: Option<&str>) -> bool {
        if matches!(status, Some("COMPLETED") | Some("FAILED") | Some("CANCELLED")) {
            self.active_jobs.remove(id);
            tracing::debug!(
                "stump job ended {id} ({status:?}); {} job(s) remaining",
                self.active_jobs.len()
            );
        }
        self.active_jobs.is_empty() && !self.buffer.is_empty()
    }

    /// Created* 事件。返回 `Some(事件)` 表示应立刻派发（无开窗时的单条，
    /// 或缓冲超上限的安全阀）；`None` 表示已入缓冲。
    fn on_created(&mut self, event: CoreEventDto) -> Option<Vec<CoreEventDto>> {
        if self.active_jobs.is_empty() {
            return Some(vec![event]);
        }
        self.buffer.push(event);
        if self.buffer.len() >= MAX_BUFFER {
            Some(std::mem::take(&mut self.buffer))
        } else {
            None
        }
    }

    fn drain(&mut self) -> Vec<CoreEventDto> {
        std::mem::take(&mut self.buffer)
    }
}

pub struct StumpEventHandler {
    client: Arc<StumpClient>,
    listeners: Vec<Arc<dyn MediaServerEventListener>>,
    batcher: Mutex<EventBatcher>,
}

impl StumpEventHandler {
    pub fn new(client: Arc<StumpClient>, listeners: Vec<Arc<dyn MediaServerEventListener>>) -> Self {
        Self {
            client,
            listeners,
            batcher: Mutex::new(EventBatcher::new()),
        }
    }

    /// 主循环：连接 → 订阅 → 事件流；断线后 10 秒重连，直到 token 取消。
    pub async fn run(&self, token: CancellationToken) {
        tracing::info!(
            "connecting to Stump event listener {}/api/graphql/ws (graphql-transport-ws)",
            self.client.base_uri()
        );
        while !token.is_cancelled() {
            match self.connect_and_listen(token.clone()).await {
                Ok(cancelled) => {
                    if cancelled {
                        return;
                    }
                }
                Err(error) => {
                    tracing::warn!("stump events stream error: {error}");
                }
            }
            // 退避重连（取消时立即退出）
            tokio::select! {
                _ = token.cancelled() => return,
                _ = tokio::time::sleep(RECONNECT_DELAY) => {}
            }
        }
    }

    /// 单次连接生命周期。返回 Ok(true) 表示被取消。
    async fn connect_and_listen(&self, token: CancellationToken) -> Result<bool, MediaServerError> {
        let token_value = self.client.access_token().await?;
        let ws_url = to_ws_url(self.client.base_uri(), "/api/graphql/ws");

        let mut request = ws_url
            .into_client_request()
            .map_err(|e| MediaServerError::message(format!("invalid stump ws url: {e}")))?;
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "graphql-transport-ws".parse().unwrap());
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {token_value}").parse().unwrap(),
        );

        let (ws_stream, _response) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| MediaServerError::message(format!("stump ws connect failed: {e}")))?;
        tracing::info!("connected to Stump event stream");

        let (mut sink, mut stream) = ws_stream.split();

        // graphql-transport-ws 握手：connection_init → connection_ack → subscribe
        sink.send(Message::Text(
            json!({ "type": "connection_init", "payload": {} }).to_string(),
        ))
        .await
        .map_err(|e| MediaServerError::message(format!("stump ws init failed: {e}")))?;

        let mut subscribed = false;
        loop {
            tokio::select! {
                _ = token.cancelled() => return Ok(true),
                message = stream.next() => {
                    let Some(message) = message else {
                        return Ok(false); // 连接关闭
                    };
                    let message = message.map_err(|e| MediaServerError::message(format!("stump ws read error: {e}")))?;
                    match message {
                        Message::Text(text) => {
                            match self.handle_text(&text, &mut subscribed).await {
                                // 订阅被服务端关闭 → 重连
                                WsAction::End => return Ok(false),
                                WsAction::Subscribe => {
                                    let payload = json!({
                                        "type": "subscribe",
                                        "id": "1",
                                        "payload": { "query": READ_EVENTS_DOC }
                                    });
                                    if sink.send(Message::Text(payload.to_string())).await.is_err() {
                                        return Ok(false);
                                    }
                                    subscribed = true;
                                    tracing::info!("Stump readEvents subscription established");
                                }
                                WsAction::Pong => {
                                    let _ = sink.send(Message::Text(json!({ "type": "pong" }).to_string())).await;
                                }
                                WsAction::None => {}
                            }
                        }
                        // tungstenite 协议级 Ping → Pong
                        Message::Ping(payload) => {
                            let _ = sink.send(Message::Pong(payload)).await;
                        }
                        Message::Close(_) => return Ok(false),
                        Message::Pong(_) => {}
                        _ => {}
                    }
                }
            }
        }
    }

    /// 处理一条文本消息（graphql-transport-ws 控制帧 + next 数据帧）的结果动作。
    async fn handle_text(&self, text: &str, subscribed: &mut bool) -> WsAction {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return WsAction::None;
        };
        let Some(msg_type) = value.get("type").and_then(|t| t.as_str()) else {
            return WsAction::None;
        };
        match msg_type {
            "connection_ack" => WsAction::Subscribe,
            "ping" => WsAction::Pong,
            "next" => {
                if let Some(data) = value.pointer("/payload/data/readEvents") {
                    if let Ok(event) = serde_json::from_value::<CoreEventDto>(data.clone()) {
                        self.handle_core_event(event).await;
                    }
                }
                WsAction::None
            }
            "complete" | "error" => {
                if *subscribed {
                    tracing::warn!("stump readEvents subscription ended ({msg_type})");
                }
                WsAction::End
            }
            _ => WsAction::None,
        }
    }

    /// CoreEvent 路由：Job* 维护批量窗口，Created* 缓冲或立即派发。
    async fn handle_core_event(&self, event: CoreEventDto) {
        let Some(typename) = event.typename.as_deref() else {
            return;
        };
        match typename {
            "JobStarted" => {
                if let Some(id) = &event.id {
                    self.batcher.lock().unwrap().on_job_started(id);
                }
            }
            "JobUpdate" => {
                let id = event.id.clone().unwrap_or_default();
                let status = event.status.as_deref();
                let flush = self.batcher.lock().unwrap().on_job_update(&id, status);
                if flush {
                    tracing::info!("stump scan window closed; dispatching {} batched event(s)", {
                        self.batcher.lock().unwrap().buffer.len()
                    });
                    self.flush().await;
                }
            }
            "CreatedMedia" | "CreatedOrUpdatedManyMedia" => {
                let flush_now = self.batcher.lock().unwrap().on_created(event);
                if let Some(events) = flush_now {
                    self.dispatch_raw(events).await;
                }
            }
            "CreatedManySeries" => {
                tracing::debug!(
                    "stump created {} series in library {}; no series ids in event, skipping",
                    event.count.unwrap_or(0),
                    event.library_id.as_deref().unwrap_or("")
                );
            }
            _ => {}
        }
    }

    /// 派发窗口内缓冲的全部事件（取走缓冲后执行，锁不跨 await）。
    async fn flush(&self) {
        let events = self.batcher.lock().unwrap().drain();
        self.dispatch_raw(events).await;
    }

    /// 展开并派发：`CreatedOrUpdatedManyMedia` 无 book id，按 seriesId 拉书籍补真实 id
    /// （缓存避免同系列多次请求）；拉取失败退回占位事件（元数据监听器仍可用）。
    async fn dispatch_raw(&self, events: Vec<CoreEventDto>) {
        let mut expanded: Vec<BookEvent> = Vec::new();
        let mut books_cache: HashMap<String, Vec<StumpMediaDto>> = HashMap::new();

        for event in events {
            let Some(typename) = event.typename.as_deref() else {
                continue;
            };
            match typename {
                "CreatedMedia" => {
                    let (Some(series_id), Some(library_id), Some(book_id)) = (
                        event.series_id.as_deref(),
                        event.library_id.as_deref(),
                        event.id.as_deref(),
                    ) else {
                        continue;
                    };
                    expanded.push(BookEvent {
                        library_id: MediaServerLibraryId(library_id.to_string()),
                        series_id: MediaServerSeriesId(series_id.to_string()),
                        book_id: MediaServerBookId(book_id.to_string()),
                    });
                }
                "CreatedOrUpdatedManyMedia" => {
                    let (Some(series_id), Some(library_id)) =
                        (event.series_id.as_deref(), event.library_id.as_deref())
                    else {
                        continue;
                    };
                    let book_ids: Vec<String> = match books_cache.entry(series_id.to_string()) {
                        std::collections::hash_map::Entry::Occupied(entry) => entry
                            .get()
                            .iter()
                            .map(|b| b.id.clone())
                            .collect(),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            let books = self
                                .client
                                .get_media_of_series_dto(series_id)
                                .await
                                .unwrap_or_default();
                            entry.insert(books).iter().map(|b| b.id.clone()).collect()
                        }
                    };
                    if book_ids.is_empty() {
                        // 拉取失败/系列暂无书籍：占位（元数据监听器仅用 library/series）
                        expanded.push(BookEvent {
                            library_id: MediaServerLibraryId(library_id.to_string()),
                            series_id: MediaServerSeriesId(series_id.to_string()),
                            book_id: MediaServerBookId(String::new()),
                        });
                    } else {
                        for book_id in book_ids {
                            expanded.push(BookEvent {
                                library_id: MediaServerLibraryId(library_id.to_string()),
                                series_id: MediaServerSeriesId(series_id.to_string()),
                                book_id: MediaServerBookId(book_id),
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        for listener in &self.listeners {
            listener.on_books_added(&expanded).await;
        }
    }
}

/// 处理一条文本消息（graphql-transport-ws 控制帧 + next 数据帧）的结果动作。
enum WsAction {
    /// 握手完成，需发送 subscribe。
    Subscribe,
    /// 收到 ping，需回复 pong。
    Pong,
    /// 订阅结束（complete/error），应重连。
    End,
    None,
}

/// `http(s)://host` → `ws(s)://host` + path（与 Kotlin WS 地址构造一致）。
fn to_ws_url(base_uri: &str, path: &str) -> String {
    let ws_base = if base_uri.starts_with("https://") {
        format!("wss://{}", &base_uri["https://".len()..])
    } else if base_uri.starts_with("http://") {
        format!("ws://{}", &base_uri["http://".len()..])
    } else {
        base_uri.to_string()
    };
    format!("{}{}", ws_base.trim_end_matches('/'), path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_conversion() {
        assert_eq!(
            to_ws_url("http://127.0.0.1:10801", "/api/graphql/ws"),
            "ws://127.0.0.1:10801/api/graphql/ws"
        );
        assert_eq!(
            to_ws_url("https://stump.example.com", "/api/graphql/ws"),
            "wss://stump.example.com/api/graphql/ws"
        );
        assert_eq!(
            to_ws_url("http://127.0.0.1:10801/", "/api/graphql/ws"),
            "ws://127.0.0.1:10801/api/graphql/ws"
        );
    }

    #[test]
    fn parses_created_media_event() {
        let value = json!({
            "__typename": "CreatedMedia",
            "id": "media-1",
            "seriesId": "series-1",
            "libraryId": "lib-1"
        });
        let event: CoreEventDto = serde_json::from_value(value).unwrap();
        assert_eq!(event.typename.as_deref(), Some("CreatedMedia"));
        assert_eq!(event.id.as_deref(), Some("media-1"));
        assert_eq!(event.series_id.as_deref(), Some("series-1"));
        assert_eq!(event.library_id.as_deref(), Some("lib-1"));
    }

    #[test]
    fn parses_created_or_updated_many_media_event() {
        let value = json!({
            "__typename": "CreatedOrUpdatedManyMedia",
            "count": 3,
            "seriesId": "series-1",
            "libraryId": "lib-1"
        });
        let event: CoreEventDto = serde_json::from_value(value).unwrap();
        assert_eq!(event.typename.as_deref(), Some("CreatedOrUpdatedManyMedia"));
        assert_eq!(event.count, Some(3));
        assert!(event.id.is_none());
    }

    #[test]
    fn parses_job_update_with_status() {
        let value = json!({
            "__typename": "JobUpdate",
            "id": "job-1",
            "status": "COMPLETED"
        });
        let event: CoreEventDto = serde_json::from_value(value).unwrap();
        assert_eq!(event.typename.as_deref(), Some("JobUpdate"));
        assert_eq!(event.id.as_deref(), Some("job-1"));
        assert_eq!(event.status.as_deref(), Some("COMPLETED"));
    }

    fn created_event(series: &str) -> CoreEventDto {
        CoreEventDto {
            typename: Some("CreatedOrUpdatedManyMedia".to_string()),
            id: None,
            status: None,
            count: Some(1),
            series_id: Some(series.to_string()),
            library_id: Some("lib-1".to_string()),
        }
    }

    #[test]
    fn batcher_buffers_until_all_jobs_complete() {
        let mut batcher = EventBatcher::new();
        // 无任务：立即派发
        let immediate = batcher.on_created(created_event("s1")).unwrap();
        assert_eq!(immediate.len(), 1);

        // 任务开始 → 缓冲
        batcher.on_job_started("job-1");
        assert!(batcher.on_created(created_event("s1")).is_none());
        assert!(batcher.on_created(created_event("s2")).is_none());

        // 任务未完成 → 不派发
        assert!(!batcher.on_job_update("job-1", Some("RUNNING")));
        assert!(batcher.on_created(created_event("s3")).is_none());

        // 完成 → 全部派发
        assert!(batcher.on_job_update("job-1", Some("COMPLETED")));
        assert_eq!(batcher.drain().len(), 3);
    }

    #[test]
    fn batcher_waits_for_all_concurrent_jobs() {
        let mut batcher = EventBatcher::new();
        batcher.on_job_started("job-1");
        batcher.on_job_started("job-2");
        assert!(batcher.on_created(created_event("s1")).is_none());
        // 一个完成另一个仍在跑 → 不派发
        assert!(!batcher.on_job_update("job-1", Some("COMPLETED")));
        assert!(batcher.on_created(created_event("s2")).is_none());
        // 全部完成 → 派发
        assert!(batcher.on_job_update("job-2", Some("FAILED")));
        assert_eq!(batcher.drain().len(), 2);
    }

    #[test]
    fn batcher_ignores_non_terminal_job_update_and_other_jobs() {
        let mut batcher = EventBatcher::new();
        batcher.on_job_started("job-1");
        batcher.on_job_started("job-2");
        assert!(!batcher.on_job_update("job-1", Some("RUNNING")));
        assert!(!batcher.on_job_update("job-2", Some("PAUSED")));
        assert!(batcher.on_created(created_event("s1")).is_none());
        // 一个任务完成但另一个仍在跑 → 窗口保持打开（不派发）
        assert!(!batcher.on_job_update("job-1", Some("COMPLETED")));
        assert!(batcher.on_created(created_event("s2")).is_none());
        // 最后一个活跃任务完成 → 关闭窗口并派发缓冲
        assert!(batcher.on_job_update("job-2", Some("COMPLETED")));
        assert_eq!(batcher.drain().len(), 2);
    }

    #[test]
    fn batcher_flushes_on_buffer_cap() {
        let mut batcher = EventBatcher::new();
        batcher.on_job_started("job-1");
        // MAX_BUFFER - 1 条入缓冲
        for i in 0..(MAX_BUFFER - 1) {
            assert!(batcher.on_created(created_event(&format!("s{i}"))).is_none());
        }
        // 第 MAX_BUFFER 条触发安全阀派发
        let flushed = batcher.on_created(created_event("s-cap")).unwrap();
        assert_eq!(flushed.len(), MAX_BUFFER);
        assert!(batcher.buffer.is_empty());
    }

    #[test]
    fn batcher_cancel_or_other_job_keeps_window_open() {
        let mut batcher = EventBatcher::new();
        batcher.on_job_started("job-1");
        // 无关任务完结（非本窗口活跃任务）不影响
        assert!(!batcher.on_job_update("ghost-job", Some("COMPLETED")));
        assert!(batcher.on_created(created_event("s1")).is_none());
        assert!(batcher.on_job_update("job-1", Some("CANCELLED")));
        assert_eq!(batcher.drain().len(), 1);
    }
}
