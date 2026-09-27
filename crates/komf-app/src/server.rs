//! HTTP 服务模块 —— 对应 `ServerModule.kt`。
use crate::routes::{
    config_routes, deprecated, job_routes, media_server_routes, metadata_routes,
    notification_routes, ServerKind, SharedState,
};
use axum::http::HeaderValue;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower_http::cors::CorsLayer;

/// 编译时注入的版本号：CI 发布时由 `KOMF_VERSION`（git tag / svu 版本）传入，
/// 本地构建时回退到 `Cargo.toml` 的 `CARGO_PKG_VERSION`。
pub const KOMF_VERSION: &str = match option_env!("KOMF_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// `GET /` 健康检查响应体：保留原有 `komf-rs` 前缀，追加版本号。
/// 老的存活探测（判 `200` / 体以 `komf-rs` 开头）不受影响。
pub fn health_body() -> String {
    format!("komf-rs {KOMF_VERSION}")
}

async fn health_handler() -> String {
    health_body()
}

/// `GET /version` 结构化版本信息，供程序化健康检查。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct VersionInfo {
    pub name: &'static str,
    pub version: &'static str,
}

pub fn version_info() -> VersionInfo {
    VersionInfo {
        name: "komf-rs",
        version: KOMF_VERSION,
    }
}

async fn version_handler() -> Json<VersionInfo> {
    Json(version_info())
}

/// `GET /api/health` 结构化健康检查（与 `/version` 同数据源，多一个 `status` 字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HealthInfo {
    pub status: &'static str,
    pub name: &'static str,
    pub version: &'static str,
}

pub fn health_info() -> HealthInfo {
    HealthInfo {
        status: "ok",
        name: "komf-rs",
        version: KOMF_VERSION,
    }
}

async fn api_health_handler() -> Json<HealthInfo> {
    Json(health_info())
}

/// 构建完整的 axum 路由（对应 Kotlin ServerModule 的 routing 配置）。
pub fn build_router(state: SharedState) -> Router {
    // 与 Kotlin 版一致的宽松 CORS：任意方法/任意来源
    let cors = CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any);

    let api = Router::new()
        .route("/health", get(api_health_handler))
        .merge(config_routes::router())
        .merge(job_routes::router())
        .merge(notification_routes::router())
        .nest(
            "/komga",
            Router::new()
                .merge(metadata_routes::router(ServerKind::Komga))
                .merge(media_server_routes::router(ServerKind::Komga)),
        )
        .nest(
            "/kavita",
            Router::new()
                .merge(metadata_routes::router(ServerKind::Kavita))
                .merge(media_server_routes::router(ServerKind::Kavita)),
        )
        .nest(
            "/stump",
            Router::new()
                .merge(metadata_routes::router(ServerKind::Stump))
                .merge(media_server_routes::router(ServerKind::Stump)),
        );

    let deprecated = deprecated::router();

    let health = Router::new()
        .route("/", get(health_handler))
        .route("/version", get(version_handler));

    Router::new()
        .merge(health)
        .merge(deprecated)
        .nest("/api", api)
        .layer(cors)
        .layer(axum::middleware::from_fn(default_headers))
        .with_state(state)
}

/// 对应 Kotlin `DefaultHeaders`：COEP/COOP 头，并附带版本头方便检查健康度。
async fn default_headers(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str("require-corp") {
        response.headers_mut().insert("Cross-Origin-Embedder-Policy", value);
    }
    if let Ok(value) = HeaderValue::from_str("same-origin") {
        response.headers_mut().insert("Cross-Origin-Opener-Policy", value);
    }
    if let Ok(value) = HeaderValue::from_str(KOMF_VERSION) {
        response.headers_mut().insert("X-Komf-Version", value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_body_keeps_komf_rs_prefix_with_version() {
        let body = health_body();
        assert!(body.starts_with("komf-rs"), "unexpected body: {body}");
        assert!(
            body.len() > "komf-rs".len(),
            "version missing in body: {body}"
        );
        assert_eq!(body, format!("komf-rs {KOMF_VERSION}"));
        assert!(!KOMF_VERSION.is_empty());
    }

    #[test]
    fn version_info_returns_name_and_version() {
        let info = version_info();
        assert_eq!(info.name, "komf-rs");
        assert_eq!(info.version, KOMF_VERSION);
        assert!(!info.version.is_empty());

        let json = serde_json::to_value(&info).expect("serialize version info");
        assert_eq!(json, serde_json::json!({"name": "komf-rs", "version": KOMF_VERSION}));
    }

    #[test]
    fn api_health_returns_ok_with_name_and_version() {
        let info = health_info();
        assert_eq!(info.status, "ok");
        assert_eq!(info.name, "komf-rs");
        assert_eq!(info.version, KOMF_VERSION);

        let json = serde_json::to_value(&info).expect("serialize health info");
        assert_eq!(
            json,
            serde_json::json!({"status": "ok", "name": "komf-rs", "version": KOMF_VERSION})
        );
    }
}
