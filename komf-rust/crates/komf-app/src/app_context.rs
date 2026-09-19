//! 应用上下文 —— 对应 `AppContext.kt`。
use crate::config::{ConfigLoader, ConfigWriter};
use crate::routes::{AppState, SharedState};
use komf_core::providers::bookwalker::BookWalkerDbDownloader;
use komf_core::providers::mangabaka::MangaBakaDbDownloader;
use komf_core::providers::ProvidersModule;
use komf_mediaserver::MediaServerModule;
use komf_notifications::NotificationsModule;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

pub static APP_CONTEXT: OnceLock<Arc<AppContext>> = OnceLock::new();

/// 应用上下文：持有配置、HTTP 客户端与可整体替换的模块状态。
pub struct AppContext {
    pub state: SharedState,
    config_path: Option<PathBuf>,
    http_client: reqwest::Client,
}

impl AppContext {
    pub fn new(config_path: Option<PathBuf>) -> Self {
        let config = ConfigLoader::load(config_path.as_deref());
        init_logging(&config.log_level);

        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            // Rust 扩展：总超时 60s（Kotlin ktor 无总超时），防止单个 provider
            // 请求挂起导致 Auto-Identify Library / 事件监听整条链卡死。
            .timeout(std::time::Duration::from_secs(60))
            // 对应 Kotlin 全局 ktor UserAgent 插件（MangaDex 等 API 强制要求非默认 UA）
            .user_agent("dyphire/komf-rs (https://github.com/dyphire/komf-rs)")
            .build()
            .expect("failed to build http client");

        let state = build_state(&config, &http_client, config_path.as_deref());

        Self {
            state: Arc::new(std::sync::RwLock::new(state)),
            config_path,
            http_client,
        }
    }

    /// 热重载：以新配置重建所有模块并整体替换状态。
    pub fn reload(&self, new_config: crate::config::AppConfig) -> anyhow::Result<()> {
        tracing::info!("Reconfiguring application state");
        let new_state = build_state(&new_config, &self.http_client, self.config_path.as_deref());
        *self.state.write().unwrap() = new_state;
        if let Some(path) = &self.config_path {
            ConfigWriter::write_config(&new_config, path).ok();
        } else {
            ConfigWriter::write_config_to_default_path(&new_config).ok();
        }
        Ok(())
    }
}

fn build_state(
    config: &crate::config::AppConfig,
    http_client: &reqwest::Client,
    config_path: Option<&std::path::Path>,
) -> AppState {
    // 对应 Kotlin `AppContext`：workDir = configDir（数据库下载器工作目录）
    let work_dir: PathBuf = match config_path {
        Some(path) if path.is_dir() => path.to_path_buf(),
        Some(path) => path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".")),
        None => PathBuf::from("."),
    };
    let db_work_dir = work_dir.join("mangabaka");

    let providers_module = ProvidersModule::new(
        &config.metadata_providers,
        http_client.clone(),
        Some(&work_dir),
    );
    let notifications_module = NotificationsModule::new(&config.notifications, http_client.clone());
    let media_server_module = MediaServerModule::new(
        &config.komga,
        &config.kavita,
        &config.database,
        Arc::new(providers_module.metadata_providers),
        http_client.clone(),
        notifications_module.discord_webhook_service.clone(),
        notifications_module.apprise_service.clone(),
    );
    let manga_baka_db_downloader = Arc::new(MangaBakaDbDownloader::new(
        db_work_dir,
        http_client.clone(),
    ));
    let book_walker_db_downloader = Arc::new(BookWalkerDbDownloader::new(
        work_dir.join("bookwalker"),
        http_client.clone(),
    ));
    AppState::from_modules(
        config.clone(),
        media_server_module,
        notifications_module,
        manga_baka_db_downloader,
        book_walker_db_downloader,
    )
}

/// 供路由层调用的热重载入口。
pub async fn reload_with_config(config: crate::config::AppConfig) -> anyhow::Result<()> {
    let context = APP_CONTEXT
        .get()
        .ok_or_else(|| anyhow::anyhow!("application context not initialized"))?;
    context.reload(config)
}

/// 初始化全局日志（重复调用被忽略；热重载改变日志级别需重启生效）。
pub fn init_logging(level: &str) {
    let filter = match level.to_uppercase().as_str() {
        "TRACE" => "trace",
        "DEBUG" => "debug",
        "WARN" => "warn",
        "ERROR" => "error",
        _ => "info",
    };
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .try_init();
}
