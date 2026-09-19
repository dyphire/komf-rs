//! 配置路由 —— 对应 `ConfigRoutes.kt`。
use crate::mappers;
use crate::routes::SharedState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use komf_api_models::config::{DownloadProgress, KomfConfigUpdateRequest};
use komf_api_models::common::KomfErrorResponse;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/config", get(get_config).patch(update_config))
        .route("/update-manga-baka-db", axum::routing::post(update_manga_baka_db))
        .route("/update-book-walker-db", axum::routing::post(update_book_walker_db))
}

async fn get_config(State(state): State<SharedState>) -> impl IntoResponse {
    let state = state.read().unwrap();
    let manga_baka_timestamp = state.manga_baka_db_downloader.download_timestamp();
    let manga_baka_checksum = std::fs::read_to_string(
        state
            .manga_baka_db_downloader
            .work_dir()
            .join("checksum.sha1"),
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
    let book_walker_timestamp = state.book_walker_db_downloader.download_timestamp();
    Json(mappers::to_config_dto(
        &state.config,
        manga_baka_timestamp.as_deref(),
        manga_baka_checksum.as_deref(),
        book_walker_timestamp.as_deref(),
    ))
}

async fn update_config(
    State(state): State<SharedState>,
    Json(request): Json<KomfConfigUpdateRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<KomfErrorResponse>)> {
    let updated = {
        let state = state.read().unwrap();
        mappers::apply_config_update(state.config.clone(), &request)
    };
    match crate::app_context::reload_with_config(updated).await {
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        Err(error) => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(KomfErrorResponse {
                message: format!("ConfigUpdateException: {error}"),
            }),
        )),
    }
}

/// 对应 Kotlin `updateMangaBakaDB`：触发下载并流式输出 NDJSON 进度事件。
///
/// 事件流语义与 Kotlin `transformWhile { emit(event); event is ProgressEvent }`
/// 一致：逐行写 `ProgressEvent`，遇 `FinishedEvent` / `ErrorEvent` 后结束。
async fn update_manga_baka_db(State(state): State<SharedState>) -> impl IntoResponse {
    let state = state.read().unwrap();
    let downloader = state.manga_baka_db_downloader.clone();
    drop(state);
    download_to_jsonl(move |_| downloader.launch_download())
}

/// 对应 Kotlin `updateBookWalkerDb`。
async fn update_book_walker_db(State(state): State<SharedState>) -> impl IntoResponse {
    let state = state.read().unwrap();
    let downloader = state.book_walker_db_downloader.clone();
    drop(state);
    download_to_jsonl(move |_| downloader.launch_download())
}

/// 将下载事件流转换为 `application/jsonl` 响应体（一行一个事件 JSON）。
fn download_to_jsonl(
    launch: impl FnOnce(()) -> tokio::sync::watch::Receiver<Option<DownloadProgress>>,
) -> impl IntoResponse {
    let receiver = launch(());
    let stream = async_stream::stream! {
        let mut receiver = receiver;
        loop {
            match receiver.changed().await {
                Ok(_) => {}
                Err(_) => break, // sender 已关闭（下载任务结束）
            }
            let Some(event) = receiver.borrow().clone() else { continue };
            let line = serde_json::to_string(&event).unwrap_or_default();
            yield Ok::<_, std::io::Error>(format!("{line}\n"));
            match event {
                DownloadProgress::ProgressEvent { .. } => continue,
                DownloadProgress::FinishedEvent | DownloadProgress::ErrorEvent { .. } => break,
            }
        }
    };
    axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, "application/jsonl")
        .body(axum::body::Body::from_stream(stream))
        .expect("invalid response")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// jsonl 流：逐行输出 ProgressEvent，遇 FinishedEvent 结束。
    #[tokio::test]
    async fn jsonl_stream_emits_ndjson_until_finished() {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        drop(sender); // 无事件 + sender 关闭 → 空流
        let body = download_to_jsonl(move |_| receiver);
        let bytes = axum::body::to_bytes(body.into_response().into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.is_empty()); // 无事件 → 空流

        let (sender, receiver) = tokio::sync::watch::channel(None);
        let task = tokio::spawn(async move {
            let body = download_to_jsonl(move |_| receiver);
            axum::body::to_bytes(body.into_response().into_body(), usize::MAX)
                .await
                .unwrap()
        });
        let _ = sender.send(Some(DownloadProgress::ProgressEvent {
            total: 100,
            completed: 40,
            info: Some("https://example.com/db.zst".to_string()),
        }));
        tokio::task::yield_now().await;
        let _ = sender.send(Some(DownloadProgress::ProgressEvent {
            total: 100,
            completed: 100,
            info: None,
        }));
        tokio::task::yield_now().await;
        let _ = sender.send(Some(DownloadProgress::FinishedEvent));
        let bytes = task.await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();

        // Kotlin kotlinx 多态格式：{"type":"ProgressEvent",...}
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0],
            r#"{"type":"ProgressEvent","total":100,"completed":40,"info":"https://example.com/db.zst"}"#
        );
        assert_eq!(lines[1], r#"{"type":"ProgressEvent","total":100,"completed":100,"info":null}"#);
        assert_eq!(lines[2], r#"{"type":"FinishedEvent"}"#);
    }

    /// ErrorEvent 同样终止流。
    #[tokio::test]
    async fn jsonl_stream_stops_on_error() {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        let task = tokio::spawn(async move {
            let body = download_to_jsonl(move |_| receiver);
            axum::body::to_bytes(body.into_response().into_body(), usize::MAX)
                .await
                .unwrap()
        });
        let _ = sender.send(Some(DownloadProgress::ProgressEvent {
            total: 0,
            completed: 0,
            info: Some("start".to_string()),
        }));
        tokio::task::yield_now().await;
        let _ = sender.send(Some(DownloadProgress::ErrorEvent {
            message: "ResponseException: 500".to_string(),
        }));
        let bytes = task.await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[1],
            r#"{"type":"ErrorEvent","message":"ResponseException: 500"}"#
        );
    }
}
