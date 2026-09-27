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
};
use komf_mediaserver::jobs::{MetadataJobEvent, MetadataJobStatus};
use std::convert::Infallible;
use std::time::Duration;
use uuid::Uuid;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/jobs", get(get_jobs))
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
