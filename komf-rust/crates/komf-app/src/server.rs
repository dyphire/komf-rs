//! HTTP 服务模块 —— 对应 `ServerModule.kt`。
use crate::routes::{
    config_routes, deprecated, job_routes, media_server_routes, metadata_routes,
    notification_routes, ServerKind, SharedState,
};
use axum::http::HeaderValue;
use axum::routing::get;
use axum::Router;
use tower_http::cors::CorsLayer;

/// 构建完整的 axum 路由（对应 Kotlin ServerModule 的 routing 配置）。
pub fn build_router(state: SharedState) -> Router {
    // 与 Kotlin 版一致的宽松 CORS：任意方法/任意来源
    let cors = CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any);

    let api = Router::new()
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

    let health = Router::new().route("/", get(|| async { "komf-rs" }));

    Router::new()
        .merge(health)
        .merge(deprecated)
        .nest("/api", api)
        .layer(cors)
        .layer(axum::middleware::from_fn(default_headers))
        .with_state(state)
}

/// 对应 Kotlin `DefaultHeaders`：COEP/COOP 头。
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
    response
}
