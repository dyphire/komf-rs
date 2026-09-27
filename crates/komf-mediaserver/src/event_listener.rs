//! 事件监听器 —— 对应 `MediaServerEventListener.kt`、`MetadataEventHandler.kt`、
//! `NotificationsEventHandler.kt`、`KomgaEventHandler.kt`、`KavitaEventHandler.kt`。
//!
//! - `MetadataEventHandler`：新书事件 → 过滤 → 按系列触发元数据匹配并等待完成；
//!   删除事件 → 清理缩略图/匹配记录。
//! - `NotificationsEventHandler`：新书事件 → 组装通知上下文 → 发送 Discord/Apprise。
//! - `KomgaEventHandler`：SSE 连接 `/api/v1/events`，累积 BookAdded/BookDeleted/SeriesDeleted，
//!   收到 TaskQueueStatus(count==0) 时批量分发。
//! - Kavita 事件监听（`kavita_signalr.rs`）：Kotlin 版经 SignalR hub `/hubs/messages`
//!   实时接收扫描事件，Rust 移植以 SignalR over SSE transport 实现，语义一致。
use crate::client::MediaServerClient;
use crate::jobs::{MetadataJobEvent, MetadataJobId, KomfJobTracker, KomfJobsRepository};
use crate::komga::KomgaClient;
use crate::metadata_service::MetadataServiceProvider;
use crate::model::{MediaServer, MediaServerBookId, MediaServerLibraryId, MediaServerSeriesId};
use komf_notifications::context::{
    AlternativeTitleContext, AuthorContext, BookContext, BookMetadataContext, LibraryContext,
    NotificationContext, SeriesContext, SeriesMetadataContext, WebLinkContext,
};
use komf_notifications::apprise::AppriseCliService;
use komf_notifications::discord::DiscordWebhookService;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// 事件类型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BookEvent {
    pub library_id: MediaServerLibraryId,
    pub series_id: MediaServerSeriesId,
    pub book_id: MediaServerBookId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SeriesEvent {
    pub library_id: MediaServerLibraryId,
    pub series_id: MediaServerSeriesId,
}

/// 事件监听器 —— 对应 `MediaServerEventListener.kt`。
#[async_trait::async_trait]
pub trait MediaServerEventListener: Send + Sync {
    async fn on_books_added(&self, events: &[BookEvent]);
    async fn on_books_deleted(&self, _events: &[BookEvent]) {}
    async fn on_series_deleted(&self, _events: &[SeriesEvent]) {}
}

// ---------------------------------------------------------------------------
// 过滤辅助
// ---------------------------------------------------------------------------

/// 库过滤：空列表表示全部。
fn library_allowed(filter: &[String], library_id: &str) -> bool {
    filter.is_empty() || filter.iter().any(|f| f == library_id)
}

/// 系列排除过滤。
fn series_allowed(exclude: &[String], series_id: &str) -> bool {
    exclude.iter().all(|f| f != series_id)
}

// ---------------------------------------------------------------------------
// MetadataEventHandler —— 对应 `MetadataEventHandler.kt`
// ---------------------------------------------------------------------------

pub struct MetadataEventHandler {
    services: Arc<MetadataServiceProvider>,
    repository: Arc<KomfJobsRepository>,
    job_tracker: Arc<KomfJobTracker>,
    library_filter: Vec<String>,
    series_exclude_filter: Vec<String>,
    media_server: &'static str,
}

impl MetadataEventHandler {
    pub fn new(
        services: Arc<MetadataServiceProvider>,
        repository: Arc<KomfJobsRepository>,
        job_tracker: Arc<KomfJobTracker>,
        library_filter: Vec<String>,
        series_exclude_filter: Vec<String>,
        media_server: MediaServer,
    ) -> Self {
        Self {
            services,
            repository,
            job_tracker,
            library_filter,
            series_exclude_filter,
            media_server: media_server.as_str(),
        }
    }
}

#[async_trait::async_trait]
impl MediaServerEventListener for MetadataEventHandler {
    async fn on_books_added(&self, events: &[BookEvent]) {
        let unique_series: HashSet<(MediaServerLibraryId, MediaServerSeriesId)> = events
            .iter()
            .filter(|e| {
                library_allowed(&self.library_filter, &e.library_id.0)
                    && series_allowed(&self.series_exclude_filter, &e.series_id.0)
            })
            .map(|e| (e.library_id.clone(), e.series_id.clone()))
            .collect();

        let mut job_ids: Vec<MetadataJobId> = Vec::new();
        for (library_id, series_id) in unique_series {
            // 对齐 Kotlin：matchSeriesMetadata 返回 jobId（错误进 job 状态），不抛错。
            let job_id = self
                .services
                .metadata_service_for(&library_id.0)
                .match_series_metadata_no_links_skip(&series_id)
                .await;
            job_ids.push(job_id);
        }

        // 等待所有匹配任务完成（Kotlin：10 分钟超时）
        for job_id in job_ids {
            if let Some(mut rx) = self.job_tracker.subscribe(&job_id).await {
                let deadline = tokio::time::sleep(Duration::from_secs(600));
                tokio::pin!(deadline);
                loop {
                    tokio::select! {
                        _ = &mut deadline => break,
                        event = rx.recv() => match event {
                            Ok(MetadataJobEvent::Completed) | Err(_) => break,
                            _ => {}
                        },
                    }
                }
            }
        }
    }

    async fn on_books_deleted(&self, events: &[BookEvent]) {
        for event in events {
            let _ = self.repository.delete_book_thumbnail(&event.book_id, self.media_server);
        }
    }

    async fn on_series_deleted(&self, events: &[SeriesEvent]) {
        for event in events {
            let _ = self.repository.delete_series_thumbnail(&event.series_id, self.media_server);
            let _ = self.repository.delete_series_match(&event.series_id, self.media_server);
        }
    }
}

// ---------------------------------------------------------------------------
// NotificationsEventHandler —— 对应 `NotificationsEventHandler.kt`
// ---------------------------------------------------------------------------

pub struct NotificationsEventHandler {
    client: Arc<dyn MediaServerClient>,
    discord: Option<DiscordWebhookService>,
    apprise: Option<AppriseCliService>,
    library_filter: Vec<String>,
    media_server: String,
}

impl NotificationsEventHandler {
    pub fn new(
        client: Arc<dyn MediaServerClient>,
        discord: Option<DiscordWebhookService>,
        apprise: Option<AppriseCliService>,
        library_filter: Vec<String>,
        media_server: MediaServer,
    ) -> Self {
        Self {
            client,
            discord,
            apprise,
            library_filter,
            media_server: media_server.as_str().to_string(),
        }
    }
}

#[async_trait::async_trait]
impl MediaServerEventListener for NotificationsEventHandler {
    async fn on_books_added(&self, events: &[BookEvent]) {
        let mut grouped: HashMap<MediaServerSeriesId, Vec<MediaServerBookId>> = HashMap::new();
        for event in events {
            if library_allowed(&self.library_filter, &event.library_id.0) {
                grouped.entry(event.series_id.clone()).or_default().push(event.book_id.clone());
            }
        }
        for (series_id, book_ids) in grouped {
            let context = match self.webhook_message(&series_id, &book_ids).await {
                Some(context) => context,
                None => continue,
            };
            if let Some(discord) = &self.discord {
                if let Err(error) = discord.send(&context, None).await {
                    tracing::error!("discord notification failed: {error}");
                }
            }
            if let Some(apprise) = &self.apprise {
                if let Err(error) = apprise.send(&context, None) {
                    tracing::error!("apprise notification failed: {error}");
                }
            }
        }
    }
}

impl NotificationsEventHandler {
    async fn webhook_message(
        &self,
        series_id: &MediaServerSeriesId,
        book_ids: &[MediaServerBookId],
    ) -> Option<NotificationContext> {
        let series = self.client.get_series(series_id).await.ok()?;
        let library = self.client.get_library(&series.library_id).await.ok()?;
        if !library_allowed(&self.library_filter, &library.id.0) {
            return None;
        }
        let mut books = Vec::new();
        for book_id in book_ids {
            if let Ok(book) = self.client.get_book(book_id).await {
                books.push(to_book_context(&book));
            }
        }
        let thumbnail = self.client.get_series_thumbnail(series_id).await.ok().flatten();

        Some(NotificationContext {
            library: to_library_context(&library),
            series: to_series_context(&series),
            books,
            series_cover: thumbnail.as_ref().map(|image| image.bytes.clone()),
            series_cover_mime_type: thumbnail.as_ref().and_then(|image| image.mime_type.clone()),
            media_server: self.media_server.clone(),
        })
    }
}

fn to_library_context(library: &crate::model::MediaServerLibrary) -> LibraryContext {
    LibraryContext {
        id: library.id.0.clone(),
        name: library.name.clone(),
    }
}

fn to_series_context(series: &crate::model::MediaServerSeries) -> SeriesContext {
    let metadata = &series.metadata;
    SeriesContext {
        id: series.id.0.clone(),
        name: series.name.clone(),
        book_count: series.books_count,
        metadata: SeriesMetadataContext {
            status: metadata.status.map(|s| screaming_snake(&format!("{s:?}"))).unwrap_or_default(),
            title: metadata.title.clone(),
            title_sort: metadata.title_sort.clone(),
            alternative_titles: metadata
                .alternative_titles
                .iter()
                .map(|t| AlternativeTitleContext {
                    label: t.label.clone(),
                    title: t.title.clone(),
                })
                .collect(),
            summary: metadata.summary.clone(),
            reading_direction: metadata.reading_direction.map(|d| screaming_snake(&format!("{d:?}"))),
            publisher: metadata.publisher.clone(),
            alternative_publishers: metadata.alternative_publishers.clone(),
            age_rating: metadata.age_rating,
            language: metadata.language.clone(),
            genres: metadata.genres.clone(),
            tags: metadata.tags.clone(),
            total_book_count: metadata.total_book_count,
            authors: metadata
                .authors
                .iter()
                .map(|a| AuthorContext {
                    name: a.name.clone(),
                    role: a.role.clone(),
                })
                .collect(),
            release_year: metadata.release_year,
            links: metadata
                .links
                .iter()
                .map(|l| WebLinkContext {
                    label: l.label.clone(),
                    url: l.url.clone(),
                })
                .collect(),
        },
    }
}

fn to_book_context(book: &crate::model::MediaServerBook) -> BookContext {
    let metadata = &book.metadata;
    BookContext {
        id: book.id.0.clone(),
        name: book.name.clone(),
        number: book.number,
        metadata: BookMetadataContext {
            title: metadata.title.clone(),
            summary: Some(metadata.summary.clone()),
            number: metadata.number.clone(),
            number_sort: metadata.number_sort.clone(),
            release_date: metadata.release_date.clone(),
            authors: metadata
                .authors
                .iter()
                .map(|a| AuthorContext {
                    name: a.name.clone(),
                    role: a.role.clone(),
                })
                .collect(),
            tags: metadata.tags.clone(),
            isbn: metadata.isbn.clone(),
            links: metadata
                .links
                .iter()
                .map(|l| WebLinkContext {
                    label: l.label.clone(),
                    url: l.url.clone(),
                })
                .collect(),
        },
    }
}

// ---------------------------------------------------------------------------
// KomgaEventHandler —— 对应 `KomgaEventHandler.kt`
// ---------------------------------------------------------------------------

pub struct KomgaEventHandler {
    client: Arc<KomgaClient>,
    listeners: Vec<Arc<dyn MediaServerEventListener>>,
    book_added: Mutex<Vec<BookEvent>>,
    book_deleted: Mutex<Vec<BookEvent>>,
    series_deleted: Mutex<Vec<SeriesEvent>>,
}

impl KomgaEventHandler {
    pub fn new(client: Arc<KomgaClient>, listeners: Vec<Arc<dyn MediaServerEventListener>>) -> Self {
        Self {
            client,
            listeners,
            book_added: Mutex::new(Vec::new()),
            book_deleted: Mutex::new(Vec::new()),
            series_deleted: Mutex::new(Vec::new()),
        }
    }

    /// 启动 SSE 监听循环。断开后自动重连（10 秒退避），直到 token 取消。
    pub async fn run(&self, token: CancellationToken) {
        while !token.is_cancelled() {
            match self.client.events_stream().await {
                Ok(response) => {
                    if !response.status().is_success() {
                        tracing::warn!(
                            "komga events stream returned status {}; retrying",
                            response.status()
                        );
                        self.wait_or_cancel(token.clone(), Duration::from_secs(10)).await;
                        continue;
                    }
                    tracing::info!("connected to Komga event stream");
                    if self.consume_stream(response, token.clone()).await {
                        return; // 被取消
                    }
                }
                Err(error) => {
                    tracing::warn!("komga events stream error: {error}");
                    self.wait_or_cancel(token.clone(), Duration::from_secs(10)).await;
                }
            }
        }
    }

    async fn consume_stream(&self, response: reqwest::Response, token: CancellationToken) -> bool {
        let mut stream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        loop {
            tokio::select! {
                _ = token.cancelled() => return true,
                chunk = stream.next() => match chunk {
                    Some(Ok(bytes)) => {
                        buffer.extend_from_slice(&bytes);
                        while let Some(pos) = find_frame_boundary(&buffer) {
                            let frame = buffer.drain(..pos).collect::<Vec<u8>>();
                            if let Some((event_type, data)) = parse_sse_frame(&frame) {
                                let event = crate::komga::KomgaEvent::parse(&event_type, &data);
                                self.handle_event(event).await;
                            }
                        }
                    }
                    Some(Err(error)) => {
                        tracing::warn!("komga events stream read error: {error}");
                        return false;
                    }
                    None => return false,
                },
            }
        }
    }

    async fn handle_event(&self, event: crate::komga::KomgaEvent) {
        // 对齐 Kotlin KomgaEventHandler `logger.debug { event }`：每个 SSE 事件入日志
        tracing::debug!("komga event: {event:?}");
        match event {
            crate::komga::KomgaEvent::BookAdded { book_id, series_id, library_id } => {
                self.book_added.lock().await.push(BookEvent {
                    library_id: MediaServerLibraryId(library_id),
                    series_id: MediaServerSeriesId(series_id),
                    book_id: MediaServerBookId(book_id),
                });
            }
            crate::komga::KomgaEvent::BookDeleted { book_id, series_id, library_id } => {
                self.book_deleted.lock().await.push(BookEvent {
                    library_id: MediaServerLibraryId(library_id),
                    series_id: MediaServerSeriesId(series_id),
                    book_id: MediaServerBookId(book_id),
                });
            }
            crate::komga::KomgaEvent::SeriesDeleted { series_id, library_id } => {
                self.series_deleted.lock().await.push(SeriesEvent {
                    library_id: MediaServerLibraryId(library_id),
                    series_id: MediaServerSeriesId(series_id),
                });
            }
            crate::komga::KomgaEvent::TaskQueueStatus { count } => {
                if count != 0 {
                    return;
                }
                let added = std::mem::take(&mut *self.book_added.lock().await);
                let deleted = std::mem::take(&mut *self.book_deleted.lock().await);
                let series_deleted = std::mem::take(&mut *self.series_deleted.lock().await);
                self.dispatch(added, deleted, series_deleted).await;
            }
            _ => {}
        }
    }

    async fn dispatch(
        &self,
        added: Vec<BookEvent>,
        deleted: Vec<BookEvent>,
        series_deleted: Vec<SeriesEvent>,
    ) {
        for listener in &self.listeners {
            if !added.is_empty() {
                listener.on_books_added(&added).await;
            }
            if !deleted.is_empty() {
                listener.on_books_deleted(&deleted).await;
            }
            if !series_deleted.is_empty() {
                listener.on_series_deleted(&series_deleted).await;
            }
        }
    }

    async fn wait_or_cancel(&self, token: CancellationToken, duration: Duration) {
        tokio::select! {
            _ = token.cancelled() => {}
            _ = tokio::time::sleep(duration) => {}
        }
    }
}


// ---------------------------------------------------------------------------
// SSE 帧解析
// ---------------------------------------------------------------------------

/// `Ended` → `ENDED`，`LeftToRight` → `LEFT_TO_RIGHT`（对应 Kotlin 枚举 `name`）。
fn screaming_snake(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_uppercase());
    }
    out
}

/// 查找一帧 SSE 的边界（`\n\n`，兼容 `\r\n\r\n`）。
fn find_frame_boundary(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|pos| pos + 2)
        .or_else(|| buffer.windows(4).position(|w| w == b"\r\n\r\n").map(|pos| pos + 4))
}

/// 解析一帧 SSE：提取 `event:` 与 `data:` 行。
fn parse_sse_frame(frame: &[u8]) -> Option<(String, String)> {
    let text = String::from_utf8_lossy(frame);
    let mut event_type: Option<String> = None;
    let mut data: Option<String> = None;
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            data = Some(rest.trim_start().to_string());
        }
    }
    match (event_type, data) {
        (Some(event_type), Some(data)) => Some((event_type, data)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::komga::KomgaEvent;

    #[test]
    fn parses_komga_book_added_event() {
        let event = KomgaEvent::parse(
            "BookAdded",
            r#"{"bookId":"5f","seriesId":"1f","libraryId":"2f"}"#,
        );
        match event {
            KomgaEvent::BookAdded { book_id, series_id, library_id } => {
                assert_eq!(book_id, "5f");
                assert_eq!(series_id, "1f");
                assert_eq!(library_id, "2f");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn parses_komga_series_deleted_event() {
        let event = KomgaEvent::parse("SeriesDeleted", r#"{"seriesId":"7","libraryId":"3"}"#);
        match event {
            KomgaEvent::SeriesDeleted { series_id, library_id } => {
                assert_eq!(series_id, "7");
                assert_eq!(library_id, "3");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn parses_task_queue_status_zero() {
        let event = KomgaEvent::parse("TaskQueueStatus", r#"{"count":0,"taskType":"METADATA"}"#);
        match event {
            KomgaEvent::TaskQueueStatus { count } => assert_eq!(count, 0),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn sse_frame_parsing() {
        let frame = b"event: TaskQueueStatus\ndata: {\"count\":1}\n\n";
        let (event_type, data) = parse_sse_frame(frame).unwrap();
        assert_eq!(event_type, "TaskQueueStatus");
        assert!(data.contains("\"count\":1"));
    }

    #[test]
    fn filters_work() {
        assert!(library_allowed(&[], "any"));
        assert!(library_allowed(&["a".to_string()], "a"));
        assert!(!library_allowed(&["a".to_string()], "b"));
        assert!(series_allowed(&[], "s"));
        assert!(!series_allowed(&["s".to_string()], "s"));
    }
}
