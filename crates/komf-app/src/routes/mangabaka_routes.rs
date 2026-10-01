//! MangaBaka 管理 API 路由 —— 对应 Kotlin `MangaBakaRoutes.kt`。
//!
//! 全部挂在 `/api/mangabaka/*`（上游一致）。依赖 MangaBaka 数据库文件
//! （`manga_baka_repository` 为 `None` 时所有端点返回 404，客户端表现一致）。
use crate::routes::SharedState;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use komf_api_models::common::KomfErrorResponse;
use komf_api_models::mangabaka::{
    KomfMangaBakaLinkRequest, KomfMangaBakaLinkedSeries, KomfMangaBakaUnlinkRequest,
};
use komf_core::providers::mangabaka::{
    to_api_linked, to_api_series, to_api_tag, MangaBakaDbRepository,
};
use std::sync::{Arc, OnceLock};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/mangabaka/gstatic-favicon", get(get_fav_icon))
        .route("/mangabaka/series/:seriesId/cover", get(get_cover))
        .route("/mangabaka/series/tags", get(get_tags))
        .route("/mangabaka/series/linked/:seriesId", get(get_linked))
        .route("/mangabaka/series/linked/batch", axum::routing::post(batch))
        .route("/mangabaka/series/link", axum::routing::post(link))
        .route("/mangabaka/series/link", axum::routing::delete(unlink))
        .route("/mangabaka/series/link/search", get(search))
        .route(
            "/mangabaka/series/link/match",
            axum::routing::post(link_match),
        )
}

/// 转发用 HTTP 客户端（favicon/封面代理；与上游 Flow<HttpClient> 等价）。
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::new())
}

fn repo(state: &AppStateRef) -> Option<Arc<MangaBakaDbRepository>> {
    state.read().unwrap().manga_baka_repository.clone()
}

type AppStateRef = SharedState;

fn not_matched() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(KomfErrorResponse {
            message: "series is not matched with MangaBaka".to_string(),
        }),
    )
        .into_response()
}

fn internal(error: impl std::fmt::Display) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(KomfErrorResponse {
            message: format!("Exception :{error}"),
        }),
    )
        .into_response()
}

/// `GET /api/mangabaka/series/linked/{seriesId}`：按 Komga 系列 id 查关联。
async fn get_linked(State(state): State<SharedState>, Path(series_id): Path<String>) -> Response {
    let Some(repo) = repo(&state) else {
        return not_matched();
    };
    match repo.find(&series_id) {
        Ok(Some(linked)) => Json(to_api_linked(&linked)).into_response(),
        Ok(None) => not_matched(),
        Err(error) => internal(error),
    }
}

/// `POST /api/mangabaka/series/linked/batch`：批量查询关联（保持输入顺序）。
async fn batch(State(state): State<SharedState>, Json(ids): Json<Vec<String>>) -> Response {
    let Some(repo) = repo(&state) else {
        return not_matched();
    };
    match repo.find_all_linked(&ids) {
        Ok(linked) => Json(
            linked
                .iter()
                .map(to_api_linked)
                .collect::<Vec<KomfMangaBakaLinkedSeries>>(),
        )
        .into_response(),
        Err(error) => internal(error),
    }
}

/// `POST /api/mangabaka/series/link`：建立关联。
async fn link(
    State(state): State<SharedState>,
    Json(request): Json<KomfMangaBakaLinkRequest>,
) -> Response {
    let Some(repo) = repo(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match repo.link(&request.komga_id.0, request.manga_baka_series_id.0) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => internal(error),
    }
}

/// `DELETE /api/mangabaka/series/link`：解除关联。
async fn unlink(
    State(state): State<SharedState>,
    Json(request): Json<KomfMangaBakaUnlinkRequest>,
) -> Response {
    let Some(repo) = repo(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match repo.unlink(&request.komga_id.0) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => internal(error),
    }
}

/// `GET /api/mangabaka/series/link/search?title=...`：标题搜索（匹配库内系列）。
async fn search(State(state): State<SharedState>, Query(query): Query<SearchQuery>) -> Response {
    let Some(title) = query.title else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(repo) = repo(&state) else {
        return not_matched();
    };
    match repo.search(&title, &[], &[]) {
        Ok(series) => Json(series.iter().map(to_api_series).collect::<Vec<_>>()).into_response(),
        Err(error) => internal(error),
    }
}

#[derive(Debug, serde::Deserialize)]
struct SearchQuery {
    title: Option<String>,
}

/// `POST /api/mangabaka/series/link/match`：上游为占位实现（NotImplemented）。
async fn link_match() -> Response {
    StatusCode::NOT_IMPLEMENTED.into_response()
}

/// `GET /api/mangabaka/series/{seriesId}/cover`：代理 MangaBaka 封面（x350 第一档）。
async fn get_cover(State(state): State<SharedState>, Path(series_id): Path<String>) -> Response {
    let Some(repo) = repo(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(id) = series_id.parse::<i64>() else {
        return internal("NumberFormatException: Invalid series id");
    };
    let cover_link = match repo.get_series(id) {
        Ok(series) => series.cover.x350.as_ref().and_then(|dpi| dpi.x1.clone()),
        Err(error) => return internal(error),
    };
    let Some(cover_link) = cover_link else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let response = match http_client().get(&cover_link).send().await {
        Ok(response) => response,
        Err(error) => return internal(error),
    };
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => return internal(error),
    };
    if !status.is_success() {
        return StatusCode::from_u16(status.as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            .into_response();
    }
    let mut builder = Response::builder().status(StatusCode::OK);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    builder
        .body(axum::body::Body::from(bytes))
        .map_err(|e| internal(e))
        .unwrap_or_else(|r| r)
}

/// `GET /api/mangabaka/series/tags`：标签目录（CacheControl max-age 900s，与上游一致）。
async fn get_tags(State(state): State<SharedState>) -> Response {
    let Some(repo) = repo(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match repo.get_all_tags() {
        Ok(tags) => {
            let mut response =
                Json(tags.iter().map(to_api_tag).collect::<Vec<_>>()).into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("max-age=900"),
            );
            response
        }
        Err(error) => internal(error),
    }
}

/// `GET /api/mangabaka/gstatic-favicon?url=...`：代理 Google favicon 服务。
async fn get_fav_icon(
    State(state): State<SharedState>,
    Query(query): Query<FavIconQuery>,
) -> Response {
    let _ = &state;
    let Some(url_string) = query.url else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(host) = parse_url_host(&url_string) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let response = match http_client()
        .get("https://t1.gstatic.com/faviconV2")
        .query(&[
            ("client", "SOCIAL"),
            ("type", "FAVICON"),
            ("fallback_opts", "TYPE,SIZE,URL"),
            ("url", host.as_str()),
            ("size", "48"),
        ])
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return internal(error),
    };
    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return StatusCode::NOT_FOUND.into_response();
    }
    let names = [
        header::CONTENT_TYPE,
        header::EXPIRES,
        header::CACHE_CONTROL,
        header::LAST_MODIFIED,
    ];
    let mut passthrough: Vec<(axum::http::HeaderName, String)> = Vec::new();
    for name in names.iter() {
        if let Some(value) = response.headers().get(name) {
            if let Ok(value) = value.to_str() {
                passthrough.push((name.clone(), value.to_string()));
            }
        }
    }
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => return internal(error),
    };
    let mut builder = Response::builder().status(StatusCode::OK);
    for (name, value) in passthrough {
        builder = builder.header(name, value);
    }
    builder
        .body(axum::body::Body::from(bytes))
        .map_err(|e| internal(e))
        .unwrap_or_else(|r| r)
}

#[derive(Debug, serde::Deserialize)]
struct FavIconQuery {
    url: Option<String>,
}

/// 对齐 Kotlin `parseUrl` 语义：只接受 http/https，返回 `scheme://host`。
fn parse_url_host(url_string: &str) -> Option<String> {
    let url = url::Url::parse(url_string).ok()?;
    match url.scheme() {
        "http" | "https" => Some(format!("{}://{}", url.scheme(), url.host_str()?)),
        _ => None,
    }
}
