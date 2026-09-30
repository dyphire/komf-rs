//! Tracker 路由 —— 挂在 `/api/tracker/{provider}/*`。
//!
//! 阅读状态同步（以 API 形态暴露）：
//! - `search`：按标题在平台上搜条目（复用已登录 OAuth token 鉴权）；
//! - `state`：读取某条目在平台上的跟踪状态；
//! - `update`：推送状态/进度/评分到平台。
//!
//! `{provider}` 仅接受 anilist / mal / bangumi / mangabaka（与 OAuth 一致），
//! 未登录时返回 401；其余返回 404。

use crate::routes::SharedState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use komf_api_models::common::KomfErrorResponse;
use komf_core::trackers::{TrackerService, TrackUpdate};
use serde::Deserialize;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/tracker/:provider/search", axum::routing::get(search))
        .route("/tracker/:provider/state", axum::routing::get(get_state))
        .route("/tracker/:provider/update", axum::routing::post(update))
        .route("/tracker/links", axum::routing::get(list_links))
}

fn tracker_for<'a>(state: &'a SharedState, name: &str) -> Result<std::sync::Arc<dyn TrackerService>, Response> {
    state.read().unwrap().tracker_services.get(name).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(KomfErrorResponse {
                message: format!("Tracker provider '{name}' is not supported"),
            }),
        )
            .into_response()
    })
}

fn unauthorized(message: String) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(KomfErrorResponse { message }),
    )
        .into_response()
}

/// tracker 执行错误 → 响应：登录态失效类错误（"not logged in"）映射为 401
/// （token 过期且刷新失败 / 无 refresh / 源站吊销等执行期暴露的失效），
/// 其余保持 500。
fn tracker_error(provider: &str, error: String) -> Response {
    if error.contains("not logged in")
        || error.contains("HTTP 401")
        || error.contains("Unauthorized")
        || error.contains("Invalid token")
        || error.contains("Invalid Authentication")
    {
        return unauthorized(format!(
            "{provider} tracker: not logged in (see /api/oauth/{provider}/start)"
        ));
    }
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(KomfErrorResponse { message: error }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    name: String,
    /// 包含成人内容；缺省 true（与现有行为一致）。
    nsfw: Option<bool>,
}

/// `GET /api/tracker/{provider}/search?name=...` → 候选条目数组。
async fn search(
    State(state): State<SharedState>,
    Path(provider): Path<String>,
    Query(query): Query<SearchQuery>,
) -> Response {
    let tracker = match tracker_for(&state, &provider) {
        Ok(t) => t,
        Err(response) => return response,
    };
    if !tracker.is_logged_in() {
        return unauthorized(format!("{provider} tracker: not logged in (see /api/oauth/{provider}/start)"));
    }
    match tracker.search(&query.name, query.nsfw.unwrap_or(true)).await {
        Ok(results) => Json(results).into_response(),
        Err(error) => tracker_error(&provider, error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateQuery {
    track_id: String,
}

/// `GET /api/tracker/{provider}/state?trackId=...` → 平台上的跟踪状态。
async fn get_state(
    State(state): State<SharedState>,
    Path(provider): Path<String>,
    Query(query): Query<StateQuery>,
) -> Response {
    let tracker = match tracker_for(&state, &provider) {
        Ok(t) => t,
        Err(response) => return response,
    };
    if !tracker.is_logged_in() {
        return unauthorized(format!("{provider} tracker: not logged in (see /api/oauth/{provider}/start)"));
    }
    match tracker.get_state(&query.track_id).await {
        Ok(track_state) => Json(track_state).into_response(),
        Err(error) => tracker_error(&provider, error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateRequest {
    track_id: String,
    #[serde(flatten)]
    update: TrackUpdate,
}

/// `POST /api/tracker/{provider}/update`，body：
/// `{"trackId":"...", "score":8, "status":"reading",
///   "lastReadChapter":12, "lastReadVolume":1,
///   "startReadDate":"2026-09-01", "finishReadDate":null}`
async fn update(
    State(state): State<SharedState>,
    Path(provider): Path<String>,
    Json(request): Json<UpdateRequest>,
) -> Response {
    let tracker = match tracker_for(&state, &provider) {
        Ok(t) => t,
        Err(response) => return response,
    };
    if !tracker.is_logged_in() {
        return unauthorized(format!("{provider} tracker: not logged in (see /api/oauth/{provider}/start)"));
    }
    match tracker.update(&request.track_id, &request.update).await {
        Ok(()) => {
            // 记录"通过 komf API 关联"的条目（本地台账，供 Tracker 页"已关联"查看）。
            let title = request.update.title.as_deref();
            let url = tracker_entry_url(&provider, &request.track_id);
            let cover_url = request.update.cover_url.as_deref();
            state
                .read()
                .unwrap()
                .oauth_manager
                .record_tracker_link(&provider, &request.track_id, title, url.as_deref(), cover_url);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => tracker_error(&provider, error),
    }
}

/// 平台条目 URL（台账展示用）。
fn tracker_entry_url(provider: &str, track_id: &str) -> Option<String> {
    match provider {
        "anilist" => Some(format!("https://anilist.co/manga/{track_id}")),
        "mal" => Some(format!("https://myanimelist.net/manga/{track_id}")),
        "bangumi" => Some(format!("https://bgm.tv/subject/{track_id}")),
        "mangabaka" => Some(format!("https://mangabaka.org/{track_id}")),
        _ => None,
    }
}

/// 已关联条目（本地台账，按更新时间倒序）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TrackerLinkItem {
    provider: String,
    track_id: String,
    title: Option<String>,
    url: Option<String>,
    cover_url: Option<String>,
    updated_at: i64,
}

/// `GET /api/tracker/links` → 通过 komf API 关联过的条目列表（无需登录）。
async fn list_links(State(state): State<SharedState>) -> Response {
    let links = state
        .read()
        .unwrap()
        .oauth_manager
        .list_tracker_links()
        .into_iter()
        .map(|(provider, track_id, title, url, cover_url, updated_at)| TrackerLinkItem {
            provider,
            track_id,
            title,
            url,
            cover_url,
            updated_at,
        })
        .collect::<Vec<_>>();
    Json(links).into_response()
}
