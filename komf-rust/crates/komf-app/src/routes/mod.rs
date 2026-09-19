//! HTTP 路由 —— 对应 `snd.komf.app.api` 包。
pub mod config_routes;
pub mod deprecated;
pub mod job_routes;
pub mod media_server_routes;
pub mod metadata_routes;
pub mod notification_routes;

use komf_mediaserver::jobs::{KomfJobTracker, KomfJobsRepository};
use komf_mediaserver::media_server_module::MediaServerModule;
use komf_notifications::apprise::{AppriseCliService, AppriseVelocityTemplates};
use komf_notifications::discord::{DiscordVelocityTemplates, DiscordWebhookService};
use std::sync::{Arc, RwLock};

use crate::config::AppConfig;

/// 全局应用状态（配置热重载时整体替换）。
pub struct AppState {
    pub config: AppConfig,
    pub job_tracker: Arc<KomfJobTracker>,
    pub jobs_repository: Arc<KomfJobsRepository>,
    pub komga_client: Arc<dyn komf_mediaserver::MediaServerClient>,
    pub komga_services: Arc<komf_mediaserver::MetadataServiceProvider>,
    pub kavita_client: Arc<dyn komf_mediaserver::MediaServerClient>,
    pub kavita_services: Arc<komf_mediaserver::MetadataServiceProvider>,
    pub discord_service: DiscordWebhookService,
    pub discord_renderer: DiscordVelocityTemplates,
    pub apprise_service: AppriseCliService,
    pub apprise_renderer: AppriseVelocityTemplates,
    /// 数据库下载器（`/update-*-db` jsonl 流与 `/config` 时间戳）。
    pub manga_baka_db_downloader: Arc<komf_core::providers::mangabaka::MangaBakaDbDownloader>,
    pub book_walker_db_downloader: Arc<komf_core::providers::bookwalker::BookWalkerDbDownloader>,
    /// 持有媒体服务器模块（含事件监听器生命周期管理）。
    _module: Arc<komf_mediaserver::MediaServerModule>,
}

pub type SharedState = Arc<RwLock<AppState>>;

/// 路由所属媒体服务器（Komga / Kavita 各挂一套并行路由）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerKind {
    Komga,
    Kavita,
}

impl ServerKind {
    pub fn label(self) -> &'static str {
        match self {
            ServerKind::Komga => "komga",
            ServerKind::Kavita => "kavita",
        }
    }
}

impl AppState {
    pub fn from_modules(
        config: AppConfig,
        module: MediaServerModule,
        notifications: komf_notifications::NotificationsModule,
        manga_baka_db_downloader: Arc<komf_core::providers::mangabaka::MangaBakaDbDownloader>,
        book_walker_db_downloader: Arc<komf_core::providers::bookwalker::BookWalkerDbDownloader>,
    ) -> Self {
        // 持有模块以保持事件监听器存活（Drop 会取消监听器 token）；
        // 热重载整体替换 AppState 时旧模块 Drop，自动停止旧监听器。
        let module = Arc::new(module);
        Self {
            config,
            job_tracker: module.job_tracker.clone(),
            jobs_repository: module.job_repository.clone(),
            komga_client: module.komga_client.clone(),
            komga_services: module.komga_metadata_service_provider.clone(),
            kavita_client: module.kavita_client.clone(),
            kavita_services: module.kavita_metadata_service_provider.clone(),
            discord_service: notifications.discord_webhook_service,
            discord_renderer: notifications.discord_velocity_renderer,
            apprise_service: notifications.apprise_service,
            apprise_renderer: notifications.apprise_velocity_renderer,
            manga_baka_db_downloader,
            book_walker_db_downloader,
            _module: module,
        }
    }
}
