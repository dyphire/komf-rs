//! 已弃用路由 —— 对应 `DeprecatedConfigRoutes.kt` / `DeprecatedMetadataRoutes.kt`。
//!
//! 保持与 Kotlin 版非 `/api` 前缀的旧接口兼容（`/config` 与 `/{komga,kavita,stump}/...`）。
use crate::mappers;
use crate::routes::{metadata_routes, ServerKind, SharedState};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use komf_api_models::config::KomfConfigUpdateRequest;
use komf_mediaserver::model::{MediaServerLibraryId, MediaServerSeriesId};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/config", get(get_config).patch(update_config))
        .nest("/komga", deprecated_metadata_routes(ServerKind::Komga))
        .nest("/kavita", deprecated_metadata_routes(ServerKind::Kavita))
        .nest("/stump", deprecated_metadata_routes(ServerKind::Stump))
}

fn deprecated_metadata_routes(kind: ServerKind) -> Router<SharedState> {
    Router::new()
        .route("/providers", get(move |s, q| providers(kind, s, q)))
        .route("/search", get(move |s, q| search(kind, s, q)))
        .route("/identify", axum::routing::post(move |s, j| identify(kind, s, j)))
        .route(
            "/match/library/:library_id/series/:series_id",
            axum::routing::post(move |s, p| match_series(kind, s, p)),
        )
        .route("/match/library/:library_id", axum::routing::post(move |s, p| match_library(kind, s, p)))
        .route(
            "/reset/library/:library_id/series/:series_id",
            axum::routing::post(move |s, p, q| reset_series(kind, s, p, q)),
        )
        .route("/reset/library/:library_id", axum::routing::post(move |s, p, q| reset_library(kind, s, p, q)))
}

async fn get_config(State(state): State<SharedState>) -> Response {
    let state = state.read().unwrap();
    Json(mappers::to_config_dto(&state.config, None, None, None)).into_response()
}

async fn update_config(
    State(state): State<SharedState>,
    Json(request): Json<KomfConfigUpdateRequest>,
) -> Response {
    let updated = {
        let state = state.read().unwrap();
        mappers::apply_config_update(state.config.clone(), &request)
    };
    match crate::app_context::reload_with_config(updated).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "message": error.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibraryQuery {
    library_id: Option<String>,
}

async fn providers(kind: ServerKind, State(state): State<SharedState>, Query(query): Query<LibraryQuery>) -> Response {
    let services = metadata_routes::select_services_for(kind, &state);
    let providers = match query.library_id {
        Some(library_id) => services
            .metadata_service_for(&library_id)
            .available_providers(&MediaServerLibraryId(library_id)),
        None => services
            .default_metadata_service()
            .available_providers(&MediaServerLibraryId(String::new())),
    };
    Json(providers.iter().map(|p| p.as_str()).collect::<Vec<_>>()).into_response()
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchQuery {
    name: Option<String>,
    series_id: Option<String>,
    library_id: Option<String>,
}

async fn search(kind: ServerKind, State(state): State<SharedState>, Query(query): Query<SearchQuery>) -> Response {
    let Some(name) = query.name else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (services, client) = metadata_routes::select_all_for(kind, &state);
    let library_id = match &query.library_id {
        Some(library_id) => Some(MediaServerLibraryId(library_id.clone())),
        None => match &query.series_id {
            Some(series_id) => client
                .get_series(&MediaServerSeriesId(series_id.clone()))
                .await
                .ok()
                .map(|s| s.library_id),
            None => None,
        },
    };
    let results = match &library_id {
        Some(library_id) => services
            .metadata_service_for(&library_id.0)
            .search_series_metadata(&name, Some(library_id))
            .await,
        None => services.default_metadata_service().search_series_metadata(&name, None).await,
    };
    Json(results).into_response()
}

/// 已弃用 identify 请求 —— 对应 Kotlin `IdentifySeriesRequest`（含 edition 字段）。
/// 注意：脚本 v0.12.2 发送 camelCase（seriesId/providerSeriesId），必须重命名对齐。
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeprecatedIdentifyRequest {
    library_id: Option<String>,
    series_id: String,
    provider: String,
    provider_series_id: String,
    #[serde(default)]
    edition: Option<String>,
}

async fn identify(kind: ServerKind, State(state): State<SharedState>, Json(request): Json<DeprecatedIdentifyRequest>) -> Response {
    let (services, client) = metadata_routes::select_all_for(kind, &state);
    let library_id = match &request.library_id {
        Some(library_id) => library_id.clone(),
        None => match client.get_series(&MediaServerSeriesId(request.series_id.clone())).await {
            Ok(series) => series.library_id.0,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "message": error.to_string() })),
                )
                    .into_response();
            }
        },
    };
    // 对齐 Kotlin：`CoreProviders.valueOf(request.provider.uppercase())`——
    // 先大写化再匹配枚举名（容忍小写/混合大小写输入）。
    let provider = match komf_core::providers::CoreProviders::from_str(&request.provider.to_uppercase()) {
        Some(provider) => provider,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "message": format!("unknown provider {}", request.provider) })),
            )
                .into_response();
        }
    };
    // 对齐 Kotlin：setSeriesMetadata 返回 jobId（错误进 job），随后阻塞收集事件流
    // 直到 CompletionEvent（takeWhile 语义）才返回恒 204。
    // Rust 扩展：identify 也受 linksMatchEnabled 控制（links 直用，不受 linksSkipEnabled 限制）。
    let job_id = services
        .metadata_service_for(&library_id)
        .identify_series_metadata(
            &MediaServerSeriesId(request.series_id),
            provider,
            &komf_core::model::ProviderSeriesId(request.provider_series_id),
            request.edition.as_deref(),
        )
        .await;
    wait_for_completion(&state, &job_id).await;
    StatusCode::NO_CONTENT.into_response()
}

async fn match_series(
    kind: ServerKind,
    State(state): State<SharedState>,
    Path((library_id, series_id)): Path<(String, String)>,
) -> Response {
    let services = metadata_routes::select_services_for(kind, &state);
    // 对齐 Kotlin：matchSeriesMetadata 返回 jobId，随后阻塞收集事件流
    // 直到 CompletionEvent 才返回恒 204。
    let job_id = services
        .metadata_service_for(&library_id)
        .match_series_metadata(&MediaServerSeriesId(series_id))
        .await;
    wait_for_completion(&state, &job_id).await;
    StatusCode::NO_CONTENT.into_response()
}

/// 对齐 Kotlin deprecated 路由的 `getMetadataJobEvents(jobId)?.takeWhile { it != CompletionEvent }?.collect {}`：
/// 订阅 job 事件流，遇 CompletionEvent（或流关闭）结束；job 已终态（无事件流）则不阻塞。
async fn wait_for_completion(state: &SharedState, job_id: &komf_mediaserver::jobs::MetadataJobId) {
    let tracker = {
        let state = state.read().unwrap();
        state.job_tracker.clone()
    };
    let Some(mut receiver) = tracker.subscribe(job_id).await else {
        return;
    };
    while let Ok(event) = receiver.recv().await {
        if matches!(event, komf_mediaserver::jobs::MetadataJobEvent::Completed) {
            break;
        }
    }
}

async fn match_library(kind: ServerKind, State(state): State<SharedState>, Path(library_id): Path<String>) -> Response {
    let services = metadata_routes::select_services_for(kind, &state);
    // 对齐 Kotlin：matchLibraryMetadata 后台执行（Unit），恒 202。
    services
        .metadata_service_for(&library_id)
        .match_library_metadata(&MediaServerLibraryId(library_id))
        .await;
    StatusCode::ACCEPTED.into_response()
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetQuery {
    remove_comic_info: Option<String>,
}

async fn reset_series(
    kind: ServerKind,
    State(state): State<SharedState>,
    Path((library_id, series_id)): Path<(String, String)>,
    Query(query): Query<ResetQuery>,
) -> Response {
    let remove_comic_info = query.remove_comic_info.unwrap_or_default() == "true";
    let update_service = {
        let state = state.read().unwrap();
        match kind {
            ServerKind::Komga => state.komga_services.update_service_for(&library_id),
            ServerKind::Kavita => state.kavita_services.update_service_for(&library_id),
            ServerKind::Stump => state.stump_services.update_service_for(&library_id),
        }
    };
    match update_service
        .reset_series_metadata(&MediaServerSeriesId(series_id), remove_comic_info)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "message": error.to_string() })),
        )
            .into_response(),
    }
}

async fn reset_library(
    kind: ServerKind,
    State(state): State<SharedState>,
    Path(library_id): Path<String>,
    Query(query): Query<ResetQuery>,
) -> Response {
    let remove_comic_info = query.remove_comic_info.unwrap_or_default() == "true";
    let update_service = {
        let state = state.read().unwrap();
        match kind {
            ServerKind::Komga => state.komga_services.update_service_for(&library_id),
            ServerKind::Kavita => state.kavita_services.update_service_for(&library_id),
            ServerKind::Stump => state.stump_services.update_service_for(&library_id),
        }
    };
    match update_service
        .reset_library_metadata(&MediaServerLibraryId(library_id), remove_comic_info)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "message": error.to_string() })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 官方 komf userscript v0.12.2 发送 camelCase 请求体（seriesId/providerSeriesId/edition）。
    /// 缺少 `rename_all = "camelCase"` 时必填 series_id 解析失败 → axum Json 提取 422。
    #[test]
    fn identify_request_accepts_camelcase() {
        // 脚本 editMetadata 形态：libraryId/edition 为 undefined 时被 JSON 序列化省略
        let req: DeprecatedIdentifyRequest =
            serde_json::from_str(r#"{"seriesId":"123","provider":"MANGA_BAKA","providerSeriesId":"456"}"#)
                .unwrap();
        assert_eq!(req.series_id, "123");
        assert_eq!(req.provider, "MANGA_BAKA");
        assert_eq!(req.provider_series_id, "456");
        assert_eq!(req.library_id, None);
        assert_eq!(req.edition, None);
        // 全字段形态（含 edition）
        let req: DeprecatedIdentifyRequest = serde_json::from_str(
            r#"{"libraryId":"lib1","seriesId":"123","provider":"MangaUpdates","providerSeriesId":"456","edition":"Vol 1"}"#,
        )
        .unwrap();
        assert_eq!(req.library_id.as_deref(), Some("lib1"));
        assert_eq!(req.edition.as_deref(), Some("Vol 1"));
        // provider 大小写不敏感（Kotlin `uppercase()` 语义）
        let req: DeprecatedIdentifyRequest =
            serde_json::from_str(r#"{"seriesId":"1","provider":"manga_baka","providerSeriesId":"2"}"#).unwrap();
        assert_eq!(req.provider, "manga_baka");
        // 非法字段（无 seriesId）→ 解析失败（422 场景正确保留）
        assert!(serde_json::from_str::<DeprecatedIdentifyRequest>(r#"{"provider":"BANGUMI"}"#).is_err());
    }
}
