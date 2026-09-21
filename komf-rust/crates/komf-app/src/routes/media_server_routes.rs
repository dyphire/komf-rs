//! 媒体服务器路由 —— 对应 `MediaServerRoutes.kt`（Komga / Kavita / Stump 各挂一套）。
use crate::routes::{ServerKind, SharedState};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use std::sync::Arc;
use komf_api_models::mediaserver::{
    KomfMediaServerConnectionResponse, KomfMediaServerLibrary, KomfMediaServerLibraryId,
};

pub fn router(kind: ServerKind) -> Router<SharedState> {
    Router::new()
        .route("/media-server/connected", get(move |s| check_connection(kind, s)))
        .route("/media-server/libraries", get(move |s| get_libraries(kind, s)))
}

async fn check_connection(kind: ServerKind, State(state): State<SharedState>) -> impl IntoResponse {
    let client = select_client(kind, &state);
    match client.get_libraries().await {
        Ok(_) => Json(KomfMediaServerConnectionResponse {
            success: true,
            http_status_code: Some(200),
            error_message: None,
        })
        .into_response(),
        // 对齐 Kotlin ResponseException：HTTP 错误 → status + description
        Err(komf_mediaserver::client::MediaServerError::Status(status, _)) => {
            Json(KomfMediaServerConnectionResponse {
                success: false,
                http_status_code: Some(status.as_u16()),
                error_message: Some(status.canonical_reason().unwrap_or_default().to_string()),
            })
            .into_response()
        }
        // 对齐 Kotlin 普通 Exception：status null + message
        Err(error) => Json(KomfMediaServerConnectionResponse {
            success: false,
            http_status_code: None,
            error_message: Some(error.to_string()),
        })
        .into_response(),
    }
}

async fn get_libraries(kind: ServerKind, State(state): State<SharedState>) -> impl IntoResponse {
    let client = select_client(kind, &state);
    match client.get_libraries().await {
        Ok(libraries) => {
            let dto: Vec<KomfMediaServerLibrary> = libraries
                .into_iter()
                .map(|library| KomfMediaServerLibrary {
                    id: KomfMediaServerLibraryId(library.id.0),
                    name: library.name,
                    roots: library.roots,
                })
                .collect();
            (StatusCode::OK, Json(dto)).into_response()
        }
        // 对齐 Kotlin：getLibraries 无 try/catch → 异常直接 500（空 body）。
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn select_client(
    kind: ServerKind,
    state: &SharedState,
) -> Arc<dyn komf_mediaserver::MediaServerClient> {
    let state = state.read().unwrap();
    match kind {
        ServerKind::Komga => state.komga_client.clone(),
        ServerKind::Kavita => state.kavita_client.clone(),
        ServerKind::Stump => state.stump_client.clone(),
    }
}
