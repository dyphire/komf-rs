//! Tracker 路由 —— 挂在 `/api/tracker/{provider}/*`。
//!
//! 阅读状态同步（以 API 形态暴露）：
//! - `search`：按标题在平台上搜条目（复用已登录 OAuth token 鉴权）；
//! - `state`：读取某条目在平台上的跟踪状态；
//! - `update`：推送状态/进度/评分到平台。
//!
//! `{provider}` 仅接受 anilist / mal / bangumi / mangabaka（与 OAuth 一致），
//! 未登录时返回 401；其余返回 404。
//!
//! 多用户：所有端点按调用方身份隔离（`X-Tracker-User` 请求头，缺省
//! `default`）。komf 不解释 user_key 语义——由可信调用方（如 kmrs 代理，
//! 以服务端会话填充）传入其用户 ID；单用户场景（komf 自带 WebUI）不传头
//! 即落到 default 账户，行为与历史一致。

use crate::routes::SharedState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use komf_api_models::common::KomfErrorResponse;
use komf_core::oauth::{validate_user_key, DEFAULT_USER_KEY};
use komf_core::trackers::{TrackUpdate, TrackerService};
use serde::Deserialize;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/tracker/:provider/search", axum::routing::get(search))
        .route("/tracker/:provider/state", axum::routing::get(get_state))
        .route("/tracker/:provider/update", axum::routing::post(update))
        .route("/tracker/links", axum::routing::get(list_links))
}

/// 提取调用方用户身份（`X-Tracker-User` 头，缺省 default）；非法键 → 400。
fn user_key_from(headers: &HeaderMap) -> Result<String, Response> {
    let key = headers
        .get("x-tracker-user")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(DEFAULT_USER_KEY);
    validate_user_key(key)
        .map(|_| key.to_string())
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(KomfErrorResponse { message: e }),
            )
                .into_response()
        })
}

fn tracker_for<'a>(
    state: &'a SharedState,
    name: &str,
) -> Result<std::sync::Arc<dyn TrackerService>, Response> {
    state
        .read()
        .unwrap()
        .tracker_services
        .get(name)
        .ok_or_else(|| {
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
    headers: HeaderMap,
) -> Response {
    let user_key = match user_key_from(&headers) {
        Ok(k) => k,
        Err(response) => return response,
    };
    let tracker = match tracker_for(&state, &provider) {
        Ok(t) => t,
        Err(response) => return response,
    };
    if !tracker.is_logged_in(&user_key) {
        return unauthorized(format!(
            "{provider} tracker: not logged in (see /api/oauth/{provider}/start)"
        ));
    }
    // Rust 扩展：括号段噪声剔除（仅含多个 [] 时生效；与 metadata 搜索同一规则，
    // authorSeparator 取 komga 默认库配置；平台链接输入无括号 → 原样通过）。
    let author_separator = state
        .read()
        .unwrap()
        .config
        .komga
        .metadata_update
        .default
        .search_title_extraction
        .author_separator
        .clone();
    let name = komf_core::util::bracket_search_term(&query.name, author_separator.as_deref())
        .unwrap_or_else(|| query.name.clone());
    match tracker
        .search(&user_key, &name, query.nsfw.unwrap_or(true))
        .await
    {
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
    headers: HeaderMap,
) -> Response {
    let user_key = match user_key_from(&headers) {
        Ok(k) => k,
        Err(response) => return response,
    };
    let tracker = match tracker_for(&state, &provider) {
        Ok(t) => t,
        Err(response) => return response,
    };
    if !tracker.is_logged_in(&user_key) {
        return unauthorized(format!(
            "{provider} tracker: not logged in (see /api/oauth/{provider}/start)"
        ));
    }
    match tracker.get_state(&user_key, &query.track_id).await {
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
    headers: HeaderMap,
    Json(request): Json<UpdateRequest>,
) -> Response {
    let user_key = match user_key_from(&headers) {
        Ok(k) => k,
        Err(response) => return response,
    };
    let tracker = match tracker_for(&state, &provider) {
        Ok(t) => t,
        Err(response) => return response,
    };
    if !tracker.is_logged_in(&user_key) {
        return unauthorized(format!(
            "{provider} tracker: not logged in (see /api/oauth/{provider}/start)"
        ));
    }
    match tracker
        .update(&user_key, &request.track_id, &request.update)
        .await
    {
        Ok(()) => {
            // 记录"通过 komf API 关联"的条目（本地台账，供 Tracker 页"已关联"查看）。
            let title = request.update.title.as_deref();
            let url = tracker_entry_url(&provider, &request.track_id);
            let cover_url = request.update.cover_url.as_deref();
            state.read().unwrap().oauth_manager.record_tracker_link(
                &provider,
                &user_key,
                &request.track_id,
                title,
                url.as_deref(),
                cover_url,
            );
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
/// 按 `X-Tracker-User` 隔离（缺省 default）。
async fn list_links(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let user_key = match user_key_from(&headers) {
        Ok(k) => k,
        Err(response) => return response,
    };
    let links = state
        .read()
        .unwrap()
        .oauth_manager
        .list_tracker_links(&user_key)
        .into_iter()
        .map(
            |(provider, track_id, title, url, cover_url, updated_at)| TrackerLinkItem {
                provider,
                track_id,
                title,
                url,
                cover_url,
                updated_at,
            },
        )
        .collect::<Vec<_>>();
    Json(links).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn user_key_from_defaults_and_validates() {
        let headers = HeaderMap::new();
        assert_eq!(user_key_from(&headers).unwrap(), DEFAULT_USER_KEY);

        let mut headers = HeaderMap::new();
        headers.insert("x-tracker-user", HeaderValue::from_static("alice"));
        assert_eq!(user_key_from(&headers).unwrap(), "alice");

        let mut headers = HeaderMap::new();
        headers.insert("x-tracker-user", HeaderValue::from_static("a b"));
        assert_eq!(
            user_key_from(&headers).unwrap_err().status(),
            StatusCode::BAD_REQUEST
        );
    }
}
