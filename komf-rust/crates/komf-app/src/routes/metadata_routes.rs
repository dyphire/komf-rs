//! 元数据路由 —— 对应 `MetadataRoutes.kt`（Komga / Kavita 各挂一套）。
use crate::routes::{ServerKind, SharedState};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use komf_api_models::common::{KomfErrorResponse, KomfProviderSeriesId, KomfProviders};
use komf_api_models::metadata::{
    KomfIdentifyRequest, KomfMetadataJobIdResponse, KomfMetadataJobResponse, KomfMetadataSeriesSearchResult,
};
use komf_core::model::ProviderSeriesId;
use std::sync::Arc;
use komf_core::providers::CoreProviders;
use komf_mediaserver::model::{MediaServerLibraryId, MediaServerSeriesId};
use komf_mediaserver::MediaServerError;

pub fn router(kind: ServerKind) -> Router<SharedState> {
    Router::new()
        .route("/metadata/providers", get(move |s, q| get_providers(kind, s, q)))
        .route("/metadata/search", get(move |s, q| search_series(kind, s, q)))
        .route("/metadata/series-cover", get(move |s, q| get_series_cover(kind, s, q)))
        .route(
            "/metadata/identify",
            axum::routing::post(move |s, j| identify_series(kind, s, j)),
        )
        .route(
            "/metadata/match/library/:library_id/series/:series_id",
            axum::routing::post(move |s, p| match_series(kind, s, p)),
        )
        .route(
            "/metadata/match/library/:library_id",
            axum::routing::post(move |s, p| match_library(kind, s, p)),
        )
        .route(
            "/metadata/reset/library/:library_id/series/:series_id",
            axum::routing::post(move |s, p, q| reset_series(kind, s, p, q)),
        )
        .route(
            "/metadata/reset/library/:library_id",
            axum::routing::post(move |s, p, q| reset_library(kind, s, p, q)),
        )
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProvidersQuery {
    library_id: Option<String>,
}

async fn get_providers(
    kind: ServerKind,
    State(state): State<SharedState>,
    Query(query): Query<ProvidersQuery>,
) -> Response {
    let services = select_services_for(kind, &state);
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

async fn search_series(
    kind: ServerKind,
    State(state): State<SharedState>,
    Query(query): Query<SearchQuery>,
) -> Response {
    let Some(name) = query.name else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (services, client) = select_all_for(kind, &state);
    let library_id = match &query.library_id {
        Some(library_id) => Some(MediaServerLibraryId(library_id.clone())),
        None => match &query.series_id {
            Some(series_id) => {
                // 对齐 Kotlin：getSeries 失败 → 500（Kotlin catch Exception 分支）。
                let series = match client
                    .get_series(&MediaServerSeriesId(series_id.clone()))
                    .await
                {
                    Ok(series) => series,
                    Err(error) => {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(KomfErrorResponse {
                                message: format!("Exception :{error}"),
                            }),
                        )
                            .into_response();
                    }
                };
                Some(series.library_id)
            }
            None => None,
        },
    };
    let results = match &library_id {
        Some(library_id) => services
            .metadata_service_for(&library_id.0)
            .search_series_metadata(&name, Some(library_id))
            .await,
        None => services
            .default_metadata_service()
            .search_series_metadata(&name, None)
            .await,
    };
    let dto: Vec<KomfMetadataSeriesSearchResult> = results
        .into_iter()
        .map(|result| KomfMetadataSeriesSearchResult {
            url: result.url,
            image_url: result.image_url,
            title: result.title,
            provider: provider_from_str(&result.provider),
            result_id: KomfProviderSeriesId(result.result_id),
            media_type: result.media_type.map(crate::mappers::to_media_type_dto),
            language: result.language,
        })
        .collect();
    (StatusCode::OK, Json(dto)).into_response()
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeriesCoverQuery {
    library_id: String,
    provider: String,
    provider_series_id: String,
}

async fn get_series_cover(
    kind: ServerKind,
    State(state): State<SharedState>,
    Query(query): Query<SeriesCoverQuery>,
) -> Response {
    let services = select_services_for(kind, &state);
    let library_id = MediaServerLibraryId(query.library_id);
    let Some(provider) = CoreProviders::from_str(&query.provider) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let image = services
        .metadata_service_for(&library_id.0)
        .get_series_cover(&library_id, provider, &ProviderSeriesId(query.provider_series_id))
        .await;
    match image {
        Ok(Some(image)) => {
            let mime = image.mime_type.clone().unwrap_or_else(|| "image/jpeg".to_string());
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, mime.as_str())],
                image.bytes,
            )
                .into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KomfErrorResponse { message: error.to_string() }),
        )
            .into_response(),
    }
}

async fn identify_series(
    kind: ServerKind,
    State(state): State<SharedState>,
    Json(request): Json<KomfIdentifyRequest>,
) -> Response {
    let (services, client) = select_all_for(kind, &state);
    let library_id = match &request.library_id {
        Some(library_id) => library_id.0.clone(),
        None => {
            let series = client
                .get_series(&MediaServerSeriesId(request.series_id.0.clone()))
                .await;
            match series {
                Ok(series) => series.library_id.0,
                Err(error) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(KomfErrorResponse { message: error.to_string() }),
                    )
                        .into_response();
                }
            }
        }
    };
    let provider = match core_provider_from_dto(&request.provider) {
        Some(provider) => provider,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(KomfErrorResponse {
                    message: format!("unknown provider {}", request.provider),
                }),
            )
                .into_response();
        }
    };
    // 对齐 Kotlin：setSeriesMetadata 返回 jobId（错误进 job 状态/事件流），恒 200。
    // Rust 扩展：identify 也受 linksMatchEnabled 控制（links 有可解析链接 → 直用，
    // 不受 linksSkipEnabled 限制）；无链接时回退请求指定的 provider。
    let job_id = services
        .metadata_service_for(&library_id)
        .identify_series_metadata(
            &MediaServerSeriesId(request.series_id.0.clone()),
            provider,
            &ProviderSeriesId(request.provider_series_id.0.clone()),
            None,
        )
        .await;
    (
        StatusCode::OK,
        Json(KomfMetadataJobResponse {
            id: KomfMetadataJobIdResponse(job_id.0.to_string()),
        }),
    )
        .into_response()
}

async fn match_series(
    kind: ServerKind,
    State(state): State<SharedState>,
    Path((library_id, series_id)): Path<(String, String)>,
) -> Response {
    let services = select_services_for(kind, &state);
    // 对齐 Kotlin：matchSeriesMetadata 返回 jobId（错误进 job），恒 200。
    let job_id = services
        .metadata_service_for(&library_id)
        .match_series_metadata(&MediaServerSeriesId(series_id))
        .await;
    (
        StatusCode::OK,
        Json(KomfMetadataJobResponse {
            id: KomfMetadataJobIdResponse(job_id.0.to_string()),
        }),
    )
        .into_response()
}

async fn match_library(
    kind: ServerKind,
    State(state): State<SharedState>,
    Path(library_id): Path<String>,
) -> Response {
    let services = select_services_for(kind, &state);
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
        }
    };
    match update_service
        .reset_series_metadata(&MediaServerSeriesId(series_id), remove_comic_info)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        // 对齐 Kotlin：仅 ComicInfoException → 422；其余错误（media server 等）→ 500。
        Err(MediaServerError::ComicInfo(message)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(KomfErrorResponse { message }),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KomfErrorResponse {
                message: format!("Exception :{error}"),
            }),
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
        }
    };
    match update_service
        .reset_library_metadata(&MediaServerLibraryId(library_id), remove_comic_info)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KomfErrorResponse { message: error.to_string() }),
        )
            .into_response(),
    }
}

pub(crate) fn select_services_for(
    kind: ServerKind,
    state: &SharedState,
) -> Arc<komf_mediaserver::MetadataServiceProvider> {
    let state = state.read().unwrap();
    match kind {
        ServerKind::Komga => state.komga_services.clone(),
        ServerKind::Kavita => state.kavita_services.clone(),
    }
}

pub(crate) fn select_all_for(
    kind: ServerKind,
    state: &SharedState,
) -> (
    Arc<komf_mediaserver::MetadataServiceProvider>,
    Arc<dyn komf_mediaserver::MediaServerClient>,
) {
    let state = state.read().unwrap();
    match kind {
        ServerKind::Komga => (state.komga_services.clone(), state.komga_client.clone()),
        ServerKind::Kavita => (state.kavita_services.clone(), state.kavita_client.clone()),
    }
}

pub(crate) fn provider_from_str(name: &str) -> KomfProviders {
    CoreProviders::from_str(name)
        .map(provider_to_dto)
        .unwrap_or(KomfProviders::Unknown(name.to_string()))
}

pub(crate) fn provider_to_dto(provider: CoreProviders) -> KomfProviders {
    match provider {
        CoreProviders::MangaBaka => KomfProviders::MangaBaka,
        CoreProviders::BookWalker => KomfProviders::BookWalker,
        CoreProviders::Mangadex => KomfProviders::Mangadex,
        CoreProviders::MangaUpdates => KomfProviders::MangaUpdates,
        CoreProviders::Anilist => KomfProviders::Anilist,
        CoreProviders::Mal => KomfProviders::Mal,
        CoreProviders::ComicVine => KomfProviders::ComicVine,
        CoreProviders::Bangumi => KomfProviders::Bangumi,
        CoreProviders::EHentai => KomfProviders::EHentai,
        CoreProviders::YenPress => KomfProviders::YenPress,
        CoreProviders::Viz => KomfProviders::Viz,
        CoreProviders::Webtoons => KomfProviders::Webtoons,
        CoreProviders::Kodansha => KomfProviders::Kodansha,
        CoreProviders::Nautiljon => KomfProviders::Nautiljon,
        CoreProviders::Hentag => KomfProviders::Hentag,
    }
}

pub(crate) fn core_provider_from_dto(provider: &KomfProviders) -> Option<CoreProviders> {
    CoreProviders::from_str(provider.as_str())
}
