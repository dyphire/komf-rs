//! komf-app 入口 —— 对应 `Application.kt`。
//!
//! 用法：`komf-app [config-path]`，其中 config-path 可以是 application.yml
//! 文件或包含 application.yml 的目录；也可通过环境变量 `KOMF_CONFIG_DIR` 指定。
use komf_app::app_context::{init_logging, AppContext, APP_CONTEXT};
use komf_app::server;
use std::sync::Arc;

/// mimalloc 全局分配器：页面重置 + decommit 主动归还 OS。
/// 根治 glibc malloc 释放内存不归还导致的 RSS 只升不降（如 ehentai 翻译库
/// 解析等峰值分配后的"僵尸内存"）。
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
    // 对齐 Kotlin Application.kt：`AppContext(configDir ?: configFile)`
    // —— KOMF_CONFIG_DIR 环境变量优先，其次才是第一个命令行参数。
    let config_path = std::env::var("KOMF_CONFIG_DIR")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::args().nth(1).map(std::path::PathBuf::from));

    let config = komf_app::config::ConfigLoader::load(config_path.as_deref());
    init_logging(&config.log_level, config_path.as_deref());
    tracing::info!("komf-rs starting, log level: {}", config.log_level);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let context = Arc::new(AppContext::new(config_path));
        let _ = APP_CONTEXT.set(context.clone());

        let port = context.state.read().unwrap().config.server.port;
        let router = server::build_router(context.state.clone());

        let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
            .await
            .expect("failed to bind port");
        tracing::info!("komf-rs listening on http://0.0.0.0:{port}");
        axum::serve(listener, router).await.expect("server error");
    });
    Ok(())
}
