//! 任务（Job）相关 API DTO —— 对应 `snd.komf.api.job` 包。
use super::common::KomfServerSeriesId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KomfMetadataJobId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfMetadataJob {
    pub series_id: KomfServerSeriesId,
    pub id: KomfMetadataJobId,
    pub status: KomfMetadataJobStatus,
    pub message: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum KomfMetadataJobStatus {
    Running,
    Failed,
    Completed,
}

/// 任务事件流（SSE）—— 对应 `KomfMetadataJobEvents.kt`。
#[derive(Debug, Clone)]
pub enum KomfMetadataJobEvent {
    ProviderSeries {
        provider: String,
    },
    ProviderBook {
        provider: String,
        total_books: i32,
        book_progress: i32,
    },
    ProviderError {
        provider: String,
        message: String,
    },
    ProviderCompleted {
        provider: String,
    },
    PostProcessingStart,
    ProcessingError {
        message: String,
    },
    Completed,
}

impl KomfMetadataJobEvent {
    /// 转为 SSE 事件 JSON —— 对齐 Kotlin kotlinx.serialization 对 sealed 接口的
    /// 序列化：默认 classDiscriminator = "type"，值为 Kotlin 类名（PascalCase），
    /// 其余字段用 camelCase。
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            KomfMetadataJobEvent::ProviderSeries { provider } => {
                serde_json::json!({ "type": "ProviderSeriesEvent", "provider": provider })
            }
            KomfMetadataJobEvent::ProviderBook {
                provider,
                total_books,
                book_progress,
            } => {
                serde_json::json!({
                    "type": "ProviderBookEvent",
                    "provider": provider,
                    "totalBooks": total_books,
                    "bookProgress": book_progress,
                })
            }
            KomfMetadataJobEvent::ProviderError { provider, message } => {
                serde_json::json!({ "type": "ProviderErrorEvent", "provider": provider, "message": message })
            }
            KomfMetadataJobEvent::ProviderCompleted { provider } => {
                serde_json::json!({ "type": "ProviderCompletedEvent", "provider": provider })
            }
            KomfMetadataJobEvent::PostProcessingStart => {
                serde_json::json!({ "type": "PostProcessingStartEvent" })
            }
            KomfMetadataJobEvent::ProcessingError { message } => {
                serde_json::json!({ "type": "ProcessingErrorEvent", "message": message })
            }
            KomfMetadataJobEvent::Completed => serde_json::json!({ "type": "Completed" }),
        }
    }

    /// SSE 事件名 —— 对齐 Kotlin `JobRoutes.kt` 中各 `*EventName` 常量。
    pub fn event_name(&self) -> &'static str {
        match self {
            KomfMetadataJobEvent::ProviderSeries { .. } => "ProviderSeriesEvent",
            KomfMetadataJobEvent::ProviderBook { .. } => "ProviderBookEvent",
            KomfMetadataJobEvent::ProviderError { .. } => "ProviderErrorEvent",
            KomfMetadataJobEvent::ProviderCompleted { .. } => "ProviderCompletedEvent",
            KomfMetadataJobEvent::PostProcessingStart => "PostProcessingStartEvent",
            KomfMetadataJobEvent::ProcessingError { .. } => "ProcessingErrorEvent",
            KomfMetadataJobEvent::Completed => "CompletionEvent",
        }
    }
}

/// 全局 job 事件流（`GET /jobs/events` firehose）生命周期帧的事件名。
pub const JOB_CREATED_EVENT_NAME: &str = "JobCreatedEvent";
pub const JOB_FINISHED_EVENT_NAME: &str = "JobFinishedEvent";

/// 构建 `JobCreatedEvent` 的 SSE data（扁平 JSON，含 jobId/seriesId/type）。
pub fn job_created_json(job_id: &str, series_id: &str, started_at: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "JobCreatedEvent",
        "jobId": job_id,
        "seriesId": series_id,
        "startedAt": started_at,
    })
}

/// 构建 `JobFinishedEvent` 的 SSE data（扁平 JSON，含 jobId/seriesId/status）。
pub fn job_finished_json(
    job_id: &str,
    series_id: &str,
    status: KomfMetadataJobStatus,
    message: Option<&str>,
    finished_at: &str,
) -> serde_json::Value {
    let status_str = match status {
        KomfMetadataJobStatus::Running => "RUNNING",
        KomfMetadataJobStatus::Failed => "FAILED",
        KomfMetadataJobStatus::Completed => "COMPLETED",
    };
    serde_json::json!({
        "type": "JobFinishedEvent",
        "jobId": job_id,
        "seriesId": series_id,
        "status": status_str,
        "message": message,
        "finishedAt": finished_at,
    })
}
