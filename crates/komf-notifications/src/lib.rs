//! 通知模块 —— 对应 `snd.komf.notifications` 包。
//!
//! 提供 Discord webhook 与 Apprise CLI 两种通知渠道，
//! 以及一套轻量 Velocity 模板渲染引擎（支持默认模板所用语法）。
pub mod apprise;
pub mod context;
pub mod discord;
pub mod velocity;

use serde::{Deserialize, Serialize};

/// 通知发送错误 —— 对应 Kotlin 侧非 2xx 抛出的 Ktor `ResponseException`。
///
/// `Upstream` 携带上游（Discord webhook 等）返回的状态码与响应体，
/// 供 HTTP 端点透传（对齐 Kotlin `catch (exception: ResponseException)` → 上游状态码）。
#[derive(Debug, thiserror::Error)]
pub enum NotificationError {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("upstream returned {status}: {body}")]
    Upstream {
        status: reqwest::StatusCode,
        body: String,
    },
}

pub type Result<T> = std::result::Result<T, NotificationError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NotificationsConfig {
    pub apprise: AppriseConfig,
    pub discord: DiscordConfig,
    pub templates_directory: String,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            apprise: AppriseConfig::default(),
            discord: DiscordConfig::default(),
            templates_directory: "./".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiscordConfig {
    pub webhooks: Option<Vec<String>>,
    pub embed_color: String,
    pub series_cover: bool,
}

impl Default for DiscordConfig {
    fn default() -> Self {
        Self {
            webhooks: None,
            embed_color: "1F8B4C".to_string(),
            series_cover: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppriseConfig {
    pub urls: Option<Vec<String>>,
    pub series_cover: bool,
}

impl Default for AppriseConfig {
    fn default() -> Self {
        Self {
            urls: None,
            series_cover: false,
        }
    }
}

/// 通知模块装配 —— 对应 `NotificationsModule.kt`。
pub struct NotificationsModule {
    pub apprise_service: apprise::AppriseCliService,
    pub discord_webhook_service: discord::DiscordWebhookService,
    pub discord_velocity_renderer: discord::DiscordVelocityTemplates,
    pub apprise_velocity_renderer: apprise::AppriseVelocityTemplates,
}

impl NotificationsModule {
    pub fn new(config: &NotificationsConfig, client: reqwest::Client) -> Self {
        let apprise_velocity_renderer =
            apprise::AppriseVelocityTemplates::new(&config.templates_directory);
        let apprise_service = apprise::AppriseCliService::new(
            config.apprise.urls.clone().unwrap_or_default(),
            apprise_velocity_renderer.clone(),
            config.apprise.series_cover,
        );

        let discord_velocity_renderer =
            discord::DiscordVelocityTemplates::new(&config.templates_directory);
        let discord_webhook_service = discord::DiscordWebhookService::new(
            client,
            config.discord.webhooks.clone().unwrap_or_default(),
            config.discord.series_cover,
            config.discord.embed_color.clone(),
            discord_velocity_renderer.clone(),
        );

        Self {
            apprise_service,
            discord_webhook_service,
            discord_velocity_renderer,
            apprise_velocity_renderer,
        }
    }
}
