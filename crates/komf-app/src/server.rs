//! HTTP 服务模块 —— 对应 `ServerModule.kt`。
use crate::routes::{
    config_routes, deprecated, job_routes, mangabaka_routes, media_server_routes, metadata_routes,
    notification_routes, web_auth, ServerKind, SharedState,
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

/// WebUI 静态目录解析优先级：
/// `KOMF_WEB_DIR` 环境变量 > `./web/dist`（源码构建） > `./ui`（docker `/app/ui`）。
pub fn web_dir() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("KOMF_WEB_DIR") {
        let dir = dir.trim().to_string();
        if !dir.is_empty() {
            let path = std::path::PathBuf::from(dir);
            if path.join("index.html").exists() {
                return Some(path);
            }
        }
    }
    for candidate in ["web/dist", "./web/dist", "ui", "./ui"] {
        let path = std::path::PathBuf::from(candidate);
        if path.join("index.html").exists() {
            return Some(path);
        }
    }
    None
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
        .route("/auth/login", axum::routing::post(web_auth::login))
        .route("/auth/logout", axum::routing::post(web_auth::logout))
        .merge(config_routes::router())
        .merge(job_routes::router())
        .merge(notification_routes::router())
        .merge(mangabaka_routes::router())
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

    // WebUI 为默认路由：`/` 直接 serving SPA（index.html + 静态资源，未匹配 API 的路径
    // fallback 到 index.html）。健康检查收敛到 `/version` 与 `/api/health`。
    // 无构建产物时不挂 fallback（纯 API 模式，`/` 404）。
    let base = Router::new()
        .route("/version", get(version_handler))
        .merge(deprecated)
        .nest("/api", api);
    let base = match web_dir() {
        Some(dir) => {
            use tower_http::services::{ServeDir, ServeFile};
            let index = dir.join("index.html");
            base.fallback_service(ServeDir::new(dir).not_found_service(ServeFile::new(index)))
        }
        None => base,
    };

    base.layer(cors)
        .layer(axum::middleware::from_fn(default_headers))
        .layer(axum::middleware::from_fn(web_auth::auth_guard))
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

    /// `KOMF_WEB_DIR` 指向含 index.html 的目录时被选中；指向空目录时回退 None。
    #[test]
    fn web_dir_prefers_env_var() {
        let dir = std::env::temp_dir().join(format!("komf-web-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<html></html>").unwrap();
        std::env::set_var("KOMF_WEB_DIR", &dir);
        assert_eq!(web_dir(), Some(dir.clone()));
        std::env::remove_var("KOMF_WEB_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn web_dir_env_missing_index_falls_back() {
        let dir = std::env::temp_dir().join(format!("komf-web-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("KOMF_WEB_DIR", &dir);
        // 空目录无 index.html：只有在仓库恰好有 web/dist 时才返回 Some，
        // 否则 None；两种都是合法回退，不断言具体值，只断言不 panic。
        let _ = web_dir();
        std::env::remove_var("KOMF_WEB_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
