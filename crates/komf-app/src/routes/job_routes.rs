//! 任务路由 —— 对应 `JobRoutes.kt`（含 SSE 事件流）。
use crate::routes::SharedState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{
    sse::{Event, KeepAlive, Sse},
    IntoResponse, Response,
};
use axum::routing::{delete, get};
use axum::{Json, Router};
use futures::stream::Stream;
use komf_api_models::common::{KomfErrorResponse, KomfPage, KomfServerSeriesId};
use komf_api_models::job::{
    KomfMetadataJob, KomfMetadataJobEvent as KomfEvent, KomfMetadataJobId, KomfMetadataJobStatus,
    JOB_CREATED_EVENT_NAME, JOB_FINISHED_EVENT_NAME,
};
use komf_mediaserver::jobs::{GlobalJobEvent, GlobalJobEventKind, MetadataJobEvent, MetadataJobStatus};
use std::collections::HashSet;
use std::convert::Infallible;
use std::time::Duration;
use uuid::Uuid;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/jobs", get(get_jobs))
        // 静态路由优先于动态 `:job_id`（axum 按字面量优先匹配），但仍把
        // `/jobs/events` 放在 `/jobs/:job_id` 之前，避免 `job_id="events"` 误命中。
        .route("/jobs/events", get(global_job_events))
        .route("/jobs/:job_id", get(get_job))
        .route("/jobs/:job_id/events", get(job_events))
        .route("/jobs/all", delete(delete_all))
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobQuery {
    status: Option<String>,
    #[serde(rename = "pageSize")]
    page_size: Option<i64>,
    page: Option<i64>,
}

async fn get_jobs(
    State(state): State<SharedState>,
    Query(query): Query<JobQuery>,
) -> impl IntoResponse {
    let status = match query.status.as_deref() {
        None => None,
        Some("RUNNING") => Some(MetadataJobStatus::Running),
        Some("FAILED") => Some(MetadataJobStatus::Failed),
        Some("COMPLETED") => Some(MetadataJobStatus::Completed),
        Some(_) => return (StatusCode::BAD_REQUEST, Json("")).into_response(),
    };
    let limit = query.page_size.unwrap_or(1000);
    let page = query.page.unwrap_or(0);

    let state = state.read().unwrap();
    let offset = (page - 1).max(0) * limit;
    let count = state.jobs_repository.count_all(status).unwrap_or(0);
    if limit == 0 {
        // 对齐 Kotlin：`(count / limit)` 除零 → ArithmeticException → 500（无 StatusPages 处理）。
        return (StatusCode::INTERNAL_SERVER_ERROR, Json("")).into_response();
    }
    let jobs = state
        .jobs_repository
        .find_all(status, limit, offset)
        .unwrap_or_default()
        .into_iter()
        .map(|job| to_dto(&job))
        .collect::<Vec<_>>();

    (
        StatusCode::OK,
        Json(KomfPage {
            content: jobs,
            total_pages: (count / limit) as i32,
            current_page: page as i32,
        }),
    )
        .into_response()
}

async fn get_job(
    State(state): State<SharedState>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    let Ok(job_id) = Uuid::parse_str(&job_id) else {
        // 对齐 Kotlin：`UUID.fromString` 抛 IllegalArgumentException → StatusPages → 400。
        return (
            StatusCode::BAD_REQUEST,
            Json(KomfErrorResponse {
                message: format!("IllegalArgumentException :Invalid UUID string: {job_id}"),
            }),
        )
            .into_response();
    };
    let state = state.read().unwrap();
    match state.jobs_repository.get_job(&komf_mediaserver::jobs::MetadataJobId(job_id)) {
        Ok(Some(job)) => (StatusCode::OK, Json(to_dto(&job))).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn delete_all(State(state): State<SharedState>) -> impl IntoResponse {
    let state = state.read().unwrap();
    let _ = state.jobs_repository.delete_all();
    StatusCode::NO_CONTENT
}

async fn job_events(
    State(state): State<SharedState>,
    Path(job_id): Path<String>,
) -> Response {
    // 对齐 Kotlin：jobId 先经 `UUID.fromString`（非 UUID → IllegalArgumentException → 400），
    // UUID 合法但查不到 → 200 SSE 发空 data 的 EventStreamNotFoundEvent。
    let job_id = match Uuid::parse_str(&job_id) {
        Ok(id) => komf_mediaserver::jobs::MetadataJobId(id),
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(KomfErrorResponse {
                    message: format!("IllegalArgumentException :Invalid UUID string: {job_id}"),
                }),
            )
                .into_response();
        }
    };
    let job_tracker = {
        let state = state.read().unwrap();
        state.job_tracker.clone()
    };
    let Some(receiver) = job_tracker.subscribe(&job_id).await else {
        // 对齐 Kotlin：job 不存在时返回 200 SSE 流，发一个空 data 的
        // `EventStreamNotFoundEvent`（eventsStreamNotFoundName）后关闭。
        return event_stream_not_found();
    };
    let stream = event_stream(receiver);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))).into_response()
}

/// 全局 job 事件流（firehose）：`GET /jobs/events`，单连接观察全部 job 活动。
///
/// 每帧 `event:` 沿用 per-job 事件名（另加 `JobCreatedEvent` / `JobFinishedEvent`
/// 生命周期帧），`data` 为扁平 JSON（原事件字段 + `jobId` + `seriesId`），
/// kmrs 可直接透传，浏览器侧 1 连接即可（替代 `?ids=` fan-out 聚合）。
///
/// - 连接时先回放当前 RUNNING 快照（`JobCreatedEvent`），再进入实时流；
/// - `?ids=a,b,c` 可选过滤（逗号分隔 jobId，非法 UUID 忽略；传了但全非法 → 空流）；
/// - 单个 job 终态只发 `JobFinishedEvent`，流本身不断开；慢客户端丢帧追赶。
#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GlobalEventsQuery {
    ids: Option<String>,
}

async fn global_job_events(
    State(state): State<SharedState>,
    Query(query): Query<GlobalEventsQuery>,
) -> Response {
    let filter: Option<HashSet<String>> = query.ids.as_deref().map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            // 仅保留合法 UUID（与 per-job `UUID.fromString` 语义一致，其余忽略）。
            .filter(|s| Uuid::parse_str(s).is_ok())
            .map(ToString::to_string)
            .collect()
    });
    // `?ids=` 传了但全非法 → 空流（不断开，行为与“过滤无命中”一致）。
    let filter = match filter {
        Some(set) if query.ids.as_deref().is_some_and(|s| !s.trim().is_empty()) && set.is_empty() => {
            return empty_global_stream();
        }
        other => other.filter(|set| !set.is_empty()),
    };

    let job_tracker = {
        let state = state.read().unwrap();
        state.job_tracker.clone()
    };
    // 先订阅再取快照：订阅与快照之间的新建 job 会同时出现在 live 缓冲与快照中，
    // 用 seen 集合对 Created 去重；反之若先快照后订阅则会漏 Created。
    let receiver = job_tracker.subscribe_all();
    let snapshot = job_tracker.running_jobs().await;
    let stream = global_event_stream(receiver, snapshot, filter);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))).into_response()
}

/// `?ids=` 全非法时的空流：只保活，不断开。
fn empty_global_stream() -> Response {
    let stream = async_stream::stream! {
        futures::future::pending::<()>().await;
        #[allow(unreachable_code)]
        yield Ok::<Event, Infallible>(Event::default().data(""));
    };
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))).into_response()
}

fn global_event_stream(
    mut receiver: tokio::sync::broadcast::Receiver<GlobalJobEvent>,
    snapshot: Vec<komf_mediaserver::jobs::KomfJobRecord>,
    filter: Option<HashSet<String>>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        let mut seen: HashSet<String> = HashSet::new();
        // 快照回放：当前 RUNNING job 各补一帧 JobCreatedEvent。
        for record in &snapshot {
            let job_id = record.id.0.to_string();
            if filter.as_ref().is_some_and(|set| !set.contains(&job_id)) {
                continue;
            }
            seen.insert(job_id.clone());
            let (name, data) = global_frame_created(record);
            yield Ok(Event::default().event(name).data(data));
        }
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    let job_id = event.job_id.0.to_string();
                    if filter.as_ref().is_some_and(|set| !set.contains(&job_id)) {
                        continue;
                    }
                    // 订阅与快照竞态产生的重复 Created 去重。
                    if matches!(event.kind, GlobalJobEventKind::Created { .. }) && !seen.insert(job_id.clone()) {
                        continue;
                    }
                    let Some((name, data)) = global_frame(event) else { continue };
                    yield Ok(Event::default().event(name).data(data));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

/// 全局帧 → (SSE 事件名, data JSON)。扁平结构：原事件字段 + jobId + seriesId。
fn global_frame(event: GlobalJobEvent) -> Option<(String, String)> {
    let job_id = event.job_id.0.to_string();
    let series_id = event.series_id.0.clone();
    match event.kind {
        GlobalJobEventKind::Created { started_at } => {
            let data = komf_api_models::job::job_created_json(
                &job_id,
                &series_id,
                &started_at.to_rfc3339(),
            );
            (
                JOB_CREATED_EVENT_NAME.to_string(),
                serde_json::to_string(&data).unwrap_or_default(),
            )
                .into()
        }
        GlobalJobEventKind::Event(inner) => {
            let dto = to_event_dto(inner);
            // per-job 语义里 Completed 不发送直接断流；全局流中 Completed 永不出现
            //（JobEventSender 已过滤），此处防御性跳过。
            if matches!(dto, KomfEvent::Completed) {
                return None;
            }
            let name = dto.event_name().to_string();
            let mut data = dto.to_json();
            if let Some(obj) = data.as_object_mut() {
                obj.insert("jobId".to_string(), serde_json::json!(job_id));
                obj.insert("seriesId".to_string(), serde_json::json!(series_id));
            }
            (name, serde_json::to_string(&data).unwrap_or_default()).into()
        }
        GlobalJobEventKind::Finished { status, message, finished_at } => {
            let dto_status = match status {
                MetadataJobStatus::Running => KomfMetadataJobStatus::Running,
                MetadataJobStatus::Failed => KomfMetadataJobStatus::Failed,
                MetadataJobStatus::Completed => KomfMetadataJobStatus::Completed,
            };
            let data = komf_api_models::job::job_finished_json(
                &job_id,
                &series_id,
                dto_status,
                message.as_deref(),
                &finished_at.to_rfc3339(),
            );
            (
                JOB_FINISHED_EVENT_NAME.to_string(),
                serde_json::to_string(&data).unwrap_or_default(),
            )
                .into()
        }
    }
}

fn global_frame_created(record: &komf_mediaserver::jobs::KomfJobRecord) -> (String, String) {
    let data = komf_api_models::job::job_created_json(
        &record.id.0.to_string(),
        &record.series_id.0,
        &record.started_at.to_rfc3339(),
    );
    (
        JOB_CREATED_EVENT_NAME.to_string(),
        serde_json::to_string(&data).unwrap_or_default(),
    )
}

/// 对齐 Kotlin：job 不存在 → 200 SSE，发空 data 的 `EventStreamNotFoundEvent` 后关闭。
fn event_stream_not_found() -> Response {
    let stream = async_stream::stream! {
        let event = Event::default()
            .event("EventStreamNotFoundEvent")
            .data("");
        yield Ok::<Event, Infallible>(event);
    };
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))).into_response()
}

fn event_stream(
    mut receiver: tokio::sync::broadcast::Receiver<MetadataJobEvent>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    let dto = to_event_dto(event);
                    // 对齐 Kotlin `takeWhile { it !is CompletionEvent }`：Completed 不发送，
                    // 直接关闭连接（Kotlin 端 cancel()）。
                    if matches!(dto, KomfEvent::Completed) {
                        break;
                    }
                    let name = dto.event_name().to_string();
                    let data = serde_json::to_string(&dto.to_json()).unwrap_or_default();
                    let mut sse_event = Event::default().data(data);
                    sse_event = sse_event.event(name);
                    yield Ok(sse_event);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

fn to_event_dto(event: MetadataJobEvent) -> KomfEvent {
    match event {
        MetadataJobEvent::ProviderSeries { provider } => KomfEvent::ProviderSeries {
            provider: provider.as_str().to_string(),
        },
        MetadataJobEvent::ProviderBook {
            provider,
            total_books,
            book_progress,
        } => KomfEvent::ProviderBook {
            provider: provider.as_str().to_string(),
            total_books,
            book_progress,
        },
        MetadataJobEvent::ProviderError { provider, message } => KomfEvent::ProviderError {
            provider: provider.as_str().to_string(),
            message,
        },
        MetadataJobEvent::ProviderCompleted { provider } => KomfEvent::ProviderCompleted {
            provider: provider.as_str().to_string(),
        },
        MetadataJobEvent::PostProcessingStart => KomfEvent::PostProcessingStart,
        MetadataJobEvent::ProcessingError { message } => KomfEvent::ProcessingError { message },
        MetadataJobEvent::Completed => KomfEvent::Completed,
    }
}

fn to_dto(job: &komf_mediaserver::jobs::KomfJobRecord) -> KomfMetadataJob {
    KomfMetadataJob {
        series_id: KomfServerSeriesId(job.series_id.0.clone()),
        id: KomfMetadataJobId(job.id.0.to_string()),
        status: match job.status {
            MetadataJobStatus::Running => KomfMetadataJobStatus::Running,
            MetadataJobStatus::Failed => KomfMetadataJobStatus::Failed,
            MetadataJobStatus::Completed => KomfMetadataJobStatus::Completed,
        },
        message: job.message.clone(),
        started_at: job.started_at.to_rfc3339(),
        finished_at: job.finished_at.map(|t| t.to_rfc3339()),
    }
}

#[cfg(test)]
mod global_stream_tests {
    use super::*;

    #[test]
    fn global_progress_frame_is_flat_with_job_id() {
        let job_id = Uuid::new_v4();
        let event = GlobalJobEvent {
            job_id: komf_mediaserver::jobs::MetadataJobId(job_id),
            series_id: komf_mediaserver::model::MediaServerSeriesId("series-1".into()),
            kind: GlobalJobEventKind::Event(MetadataJobEvent::ProviderBook {
                provider: komf_core::providers::CoreProviders::MangaUpdates,
                total_books: 10,
                book_progress: 3,
            }),
        };
        let (name, data) = global_frame(event).expect("frame");
        assert_eq!(name, "ProviderBookEvent");
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(v["type"], "ProviderBookEvent");
        assert_eq!(v["jobId"], job_id.to_string());
        assert_eq!(v["seriesId"], "series-1");
        assert_eq!(v["totalBooks"], 10);
        assert_eq!(v["bookProgress"], 3);
    }

    #[test]
    fn global_completed_is_skipped_in_favor_of_finished() {
        let event = GlobalJobEvent {
            job_id: komf_mediaserver::jobs::MetadataJobId(Uuid::new_v4()),
            series_id: komf_mediaserver::model::MediaServerSeriesId("s".into()),
            kind: GlobalJobEventKind::Event(MetadataJobEvent::Completed),
        };
        assert!(global_frame(event).is_none());
    }

    #[test]
    fn global_finished_frame_carries_status() {
        let job_id = Uuid::new_v4();
        let event = GlobalJobEvent {
            job_id: komf_mediaserver::jobs::MetadataJobId(job_id),
            series_id: komf_mediaserver::model::MediaServerSeriesId("s".into()),
            kind: GlobalJobEventKind::Finished {
                status: MetadataJobStatus::Failed,
                message: Some("boom".into()),
                finished_at: chrono::Utc::now(),
            },
        };
        let (name, data) = global_frame(event).expect("frame");
        assert_eq!(name, JOB_FINISHED_EVENT_NAME);
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(v["jobId"], job_id.to_string());
        assert_eq!(v["status"], "FAILED");
        assert_eq!(v["message"], "boom");
    }

    /// `/jobs/events` 静态路由不得被 `/jobs/:job_id` 当成 `job_id="events"` 吞掉。
    #[tokio::test]
    async fn static_events_route_wins_over_job_id_param() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let app = Router::new()
            .route("/jobs/events", get(|| async { "global" }))
            .route("/jobs/:job_id", get(|| async { "single" }));
        let res = app
            .oneshot(Request::builder().uri("/jobs/events").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"global");
    }
}
