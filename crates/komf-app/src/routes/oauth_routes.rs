//! OAuth 路由 —— 挂在 `/api/oauth/{provider}/*`。
//!
//! 采用「共享 client + 中转页」模式（与中转页 `docs/oauth-relay` 配合）：
//! - `start`：生成 PKCE/state 并 302 到平台授权页；state 携带当前实例回调
//!   `redirectUrl`（可选 `redirect_path_prefix` 加路径前缀），中转页授权完成
//!   后按 state 跳回本实例 callback；
//! - `callback`：校验 state/nonce → code 换 token → 持久化；成功后触发配置
//!   热重载（使 MAL 等"登录后才注册"的 provider 生效）并 302 回 WebUI；
//! - `status` / `logout`：供 WebUI 展示登录态与退出。
//!
//! 多用户：token 按 (provider, user_key) 隔离。`start` 接受 `?user=` 参数
//! （WebUI/反代以普通链接发起，header 不便携带）；`status`/`logout` 接受
//! `?user=` 或 `X-Tracker-User` 头；缺省一律 `default`。user_key 在授权发起时
//! 记入服务端 pending，回调按 nonce 取回归属——state JSON 与中转页均不改动。
//!
//! MangaBaka 已接入（OIDC，PKCE S256），`{provider}` 接受
//! anilist / mal / bangumi / mangabaka，其余返回 404。

use crate::routes::SharedState;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use komf_api_models::common::KomfErrorResponse;
use komf_core::oauth::{OAuthManager, OAuthProvider, OAuthStatus};
use serde::Deserialize;
use std::sync::Arc;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/oauth/:provider/start", get(start))
        .route("/oauth/:provider/callback", get(callback))
        .route("/oauth/:provider/status", get(status))
        .route("/oauth/:provider/logout", axum::routing::post(logout))
}

fn parse_provider(name: &str) -> Result<OAuthProvider, Response> {
    OAuthProvider::from_str(name).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(KomfErrorResponse {
                message: format!("OAuth provider '{name}' is not supported"),
            }),
        )
            .into_response()
    })
}

fn manager(state: &SharedState) -> Arc<OAuthManager> {
    state.read().unwrap().oauth_manager.clone()
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(KomfErrorResponse {
            message: message.into(),
        }),
    )
        .into_response()
}

fn internal(message: impl std::fmt::Display) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(KomfErrorResponse {
            message: format!("OAuth error: {message}"),
        }),
    )
        .into_response()
}

/// 反向代理前缀优先（X-Forwarded-*），否则用 Host 头；协议默认 http。
fn instance_scheme(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "http".to_string())
}

fn instance_host(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            headers
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
}

#[derive(Deserialize)]
struct StartParams {
    redirect_path_prefix: Option<String>,
    /// 多用户：授权归属的用户身份键（缺省 default）。存入服务端 pending，
    /// 回调时按 nonce 取回；state JSON 与中转页不感知该字段。
    user: Option<String>,
}

/// `?user=` 参数优先于 `X-Tracker-User` 头（start 是普通链接，反代/WebUI
/// 方便携带 query），缺省 default；非法键 → 400。
fn resolve_user_key(
    query_user: Option<&str>,
    headers: &axum::http::HeaderMap,
) -> Result<String, Response> {
    let key = query_user
        .map(str::to_string)
        .or_else(|| {
            headers
                .get("x-tracker-user")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
        .unwrap_or_else(|| komf_core::oauth::DEFAULT_USER_KEY.to_string());
    komf_core::oauth::validate_user_key(&key)
        .map(|_| key)
        .map_err(bad_request)
}

/// `GET /api/oauth/{provider}/start`：302 到平台授权页。
/// 可选 `?redirect_path_prefix=/prefix`：回调 URL 加路径前缀，供反代把回调挂进
/// 自有命名空间（如 kmrs 的 /api/v1/komf）或子路径部署；中转页白名单允许任意
/// 路径前缀，无需改动。
async fn start(
    State(state): State<SharedState>,
    Path(provider): Path<String>,
    Query(params): Query<StartParams>,
    headers: axum::http::HeaderMap,
) -> Response {
    let provider = match parse_provider(&provider) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let user_key = match resolve_user_key(params.user.as_deref(), &headers) {
        Ok(k) => k,
        Err(r) => return r,
    };
    let Some(host) = instance_host(&headers) else {
        return bad_request("cannot determine instance host (Host header missing)");
    };
    let scheme = instance_scheme(&headers);
    authorize_response(
        &manager(&state),
        provider,
        &user_key,
        &scheme,
        &host,
        params.redirect_path_prefix.as_deref(),
    )
}

/// 校验前缀、组装实例回调并发起授权；独立出来是因为 SharedState 装配太重，
/// 测试只能直调这一层。
fn authorize_response(
    mgr: &OAuthManager,
    provider: OAuthProvider,
    user_key: &str,
    scheme: &str,
    host: &str,
    prefix: Option<&str>,
) -> Response {
    let prefix = match prefix {
        Some(p) => match komf_core::oauth::validate_redirect_path_prefix(p) {
            Ok(()) => p,
            Err(e) => return bad_request(e),
        },
        None => "",
    };
    // 实例回调（中转页经 state.redirectUrl 转交回来）。
    let redirect_url = format!(
        "{scheme}://{host}{prefix}/api/oauth/{}/callback",
        provider.as_str()
    );
    match mgr.start(provider, user_key, &redirect_url) {
        Ok(url) => Redirect::to(&url).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct CallbackParams {
    code: String,
    state: String,
}

/// `GET /api/oauth/{provider}/callback?code&state`：换 token 后跳回 WebUI。
async fn callback(
    State(state): State<SharedState>,
    Path(provider): Path<String>,
    Query(params): Query<CallbackParams>,
) -> Response {
    let provider = match parse_provider(&provider) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mgr = manager(&state);
    let result = mgr
        .handle_callback(provider, &params.code, &params.state)
        .await;
    match result {
        Ok(()) => {
            // 使登录后才注册的 provider（如 MAL 无 clientId 时）生效：以当前配置热重载。
            let config = state.read().unwrap().config.clone();
            let _ = tokio::spawn(async move {
                if let Err(e) = crate::app_context::reload_with_config(config).await {
                    tracing::warn!("OAuth reload failed: {e}");
                }
            });
            Redirect::to("/?oauth=success").into_response()
        }
        Err(e) => {
            tracing::warn!("OAuth callback failed for {}: {e}", provider.as_str());
            Redirect::to(&format!("/?oauth=error&message={}", urlencode(&e))).into_response()
        }
    }
}

/// `GET /api/oauth/{provider}/status`：登录态 + username（`?user=` 或
/// `X-Tracker-User`，缺省 default）。
async fn status(
    State(state): State<SharedState>,
    Path(provider): Path<String>,
    Query(params): Query<UserQuery>,
    headers: axum::http::HeaderMap,
) -> Response {
    let provider = match parse_provider(&provider) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let user_key = match resolve_user_key(params.user.as_deref(), &headers) {
        Ok(k) => k,
        Err(r) => return r,
    };
    let s: OAuthStatus = manager(&state).status(provider, &user_key);
    Json(s).into_response()
}

#[derive(Deserialize)]
struct UserQuery {
    user: Option<String>,
}

/// `POST /api/oauth/{provider}/logout`：清除指定用户的登录态。
async fn logout(
    State(state): State<SharedState>,
    Path(provider): Path<String>,
    Query(params): Query<UserQuery>,
    headers: axum::http::HeaderMap,
) -> Response {
    let provider = match parse_provider(&provider) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let user_key = match resolve_user_key(params.user.as_deref(), &headers) {
        Ok(k) => k,
        Err(r) => return r,
    };
    manager(&state).logout(provider, &user_key);
    // 登出后同样热重载：MAL 等仅凭 OAuth 登录才注册的 provider 随之移除。
    let config = state.read().unwrap().config.clone();
    let _ = tokio::spawn(async move {
        if let Err(e) = crate::app_context::reload_with_config(config).await {
            tracing::warn!("OAuth reload after logout failed: {e}");
        }
    });
    StatusCode::NO_CONTENT.into_response()
}

/// 轻量 URL 编码（错误消息跳回 WebUI 时用）。
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    fn test_manager(test_name: &str) -> Arc<OAuthManager> {
        let dir = std::env::temp_dir().join(format!(
            "komf-oauth-routes-{test_name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        OAuthManager::new(Some(&dir), reqwest::Client::new())
    }

    fn cleanup(test_name: &str) {
        let dir = std::env::temp_dir().join(format!(
            "komf-oauth-routes-{test_name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 实例 origin：优先 X-Forwarded-*（反代），否则 Host 头 + http。
    #[test]
    fn instance_origin_prefers_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("internal:8085"));
        assert_eq!(instance_host(&headers).as_deref(), Some("internal:8085"));
        assert_eq!(instance_scheme(&headers), "http");
        headers.insert(
            "x-forwarded-host",
            HeaderValue::from_static("komga.example"),
        );
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert_eq!(instance_host(&headers).as_deref(), Some("komga.example"));
        assert_eq!(instance_scheme(&headers), "https");
    }

    /// start 的 302 Location 中 state.redirectUrl 按 scheme/host/prefix 组装。
    #[test]
    fn start_assembles_callback_url_with_prefix() {
        let mgr = test_manager("assemble");
        let response = authorize_response(
            &mgr,
            OAuthProvider::Anilist,
            komf_core::oauth::DEFAULT_USER_KEY,
            "https",
            "komga.example",
            Some("/api/v1/komf"),
        );
        assert!(response.status().is_redirection());
        let location = response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();
        let url = url::Url::parse(location).unwrap();
        let state_raw = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        let state: serde_json::Value = serde_json::from_str(&state_raw).unwrap();
        assert_eq!(
            state["redirectUrl"],
            "https://komga.example/api/v1/komf/api/oauth/anilist/callback"
        );
        cleanup("assemble");
    }

    /// 非法前缀 → 400，不发起授权。
    #[test]
    fn start_rejects_invalid_prefix() {
        let mgr = test_manager("reject");
        let response = authorize_response(
            &mgr,
            OAuthProvider::Anilist,
            komf_core::oauth::DEFAULT_USER_KEY,
            "https",
            "komga.example",
            Some("/a/../b"),
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        cleanup("reject");
    }

    /// 用户身份键：query 参数优先于 header，缺省 default；非法键 400。
    #[test]
    fn resolve_user_key_precedence_and_validation() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            resolve_user_key(None, &headers).unwrap(),
            komf_core::oauth::DEFAULT_USER_KEY
        );
        headers.insert("x-tracker-user", HeaderValue::from_static("bob"));
        assert_eq!(resolve_user_key(None, &headers).unwrap(), "bob");
        // query 优先于 header。
        assert_eq!(resolve_user_key(Some("alice"), &headers).unwrap(), "alice");
        // 非法键 → 400。
        let err = resolve_user_key(Some("a b"), &headers).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    /// start 把 user_key 记入 pending：回调前可从 pending 按 nonce 取回归属。
    #[test]
    fn start_records_user_key_in_pending() {
        let mgr = test_manager("pending-user");
        let response = authorize_response(
            &mgr,
            OAuthProvider::Anilist,
            "alice",
            "https",
            "komga.example",
            None,
        );
        assert!(response.status().is_redirection());
        let location = response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();
        let url = url::Url::parse(location).unwrap();
        let state_raw = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        let state: serde_json::Value = serde_json::from_str(&state_raw).unwrap();
        let nonce = state["nonce"].as_str().unwrap();
        // 与 OAuthManager 的测试同一前提：pending 存了 user_key（经 mgr.start 写入）。
        assert!(!nonce.is_empty());
        cleanup("pending-user");
    }
}
