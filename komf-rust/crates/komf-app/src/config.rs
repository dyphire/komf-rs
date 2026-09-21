//! 应用配置 —— 对应 `AppConfig.kt`、`ConfigLoader.kt`、`ConfigWriter.kt`。
use komf_mediaserver::config::{DatabaseConfig, KavitaConfig, KomgaConfig, StumpConfig};
use komf_notifications::NotificationsConfig;
use komf_core::config::MetadataProvidersConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppConfig {
    pub komga: KomgaConfig,
    pub kavita: KavitaConfig,
    pub stump: StumpConfig,
    pub database: DatabaseConfig,
    pub metadata_providers: MetadataProvidersConfig,
    pub notifications: NotificationsConfig,
    pub server: ServerConfig,
    pub log_level: String,
    pub http_log_level: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            komga: KomgaConfig::default(),
            kavita: KavitaConfig::default(),
            stump: StumpConfig::default(),
            database: DatabaseConfig::default(),
            metadata_providers: MetadataProvidersConfig::default(),
            notifications: NotificationsConfig::default(),
            server: ServerConfig::default(),
            log_level: "INFO".to_string(),
            http_log_level: "BASIC".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerConfig {
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { port: 8085 }
    }
}

/// 配置加载器 —— 对应 `ConfigLoader.kt`。
pub struct ConfigLoader;

impl ConfigLoader {
    /// 加载配置：`configPath` 为目录时读目录下 `application.yml`，
    /// 为文件时读文件；为空时读当前目录 `application.yml`（不存在则用默认值）。
    /// 始终应用环境变量覆盖。
    pub fn load(config_path: Option<&std::path::Path>) -> AppConfig {
        let (config, config_dir) = match config_path {
            Some(path) if path.is_dir() => {
                let file = path.join("application.yml");
                let config = read_config_file(&file).unwrap_or_default();
                (config, Some(path.to_path_buf()))
            }
            Some(path) => {
                let config = read_config_file(path).unwrap_or_default();
                (config, None)
            }
            None => {
                let file = std::path::Path::new(".").join("application.yml");
                if file.exists() {
                    (read_config_file(&file).unwrap_or_default(), None)
                } else {
                    (AppConfig::default(), None)
                }
            }
        };
        let processed = override_with_env(config, config_dir.as_deref());
        warn_about_disabled_providers(&processed);
        processed
    }
}

fn read_config_file(path: &std::path::Path) -> Option<AppConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    match serde_yaml::from_str(&content) {
        Ok(config) => Some(config),
        Err(error) => {
            // 解析失败静默回退默认值会掩盖配置错误（字段名/枚举值不匹配），显式告警。
            tracing::warn!("Failed to parse config file {}: {error}", path.display());
            None
        }
    }
}

/// 环境变量覆盖 —— 对应 `ConfigLoader.overrideConfigDirAndEnvVars`。
fn override_with_env(mut config: AppConfig, config_dir: Option<&std::path::Path>) -> AppConfig {
    let env = |key: &str| -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.trim().is_empty())
    };

    let database_file = config_dir
        .map(|dir| dir.join("database.sqlite").to_string_lossy().to_string())
        .unwrap_or_else(|| config.database.file.clone());

    let templates_directory = config_dir
        .map(|dir| dir.to_string_lossy().to_string())
        .unwrap_or_else(|| config.notifications.templates_directory.clone());

    if let Some(urls) = env("KOMF_APPRISE_URLS") {
        config.notifications.apprise.urls = Some(urls.split(',').map(|s| s.trim().to_string()).collect());
    }
    if let Some(webhooks) = env("KOMF_DISCORD_WEBHOOKS") {
        config.notifications.discord.webhooks = Some(webhooks.split(',').map(|s| s.trim().to_string()).collect());
    }
    if let Some(uri) = env("KOMF_KOMGA_BASE_URI") {
        config.komga.base_uri = uri;
    }
    if let Some(user) = env("KOMF_KOMGA_USER") {
        config.komga.komga_user = user;
    }
    if let Some(password) = env("KOMF_KOMGA_PASSWORD") {
        config.komga.komga_password = password;
    }
    if let Some(api_key) = env("KOMF_KOMGA_API_KEY") {
        config.komga.api_key = api_key;
    }
    if let Some(uri) = env("KOMF_KAVITA_BASE_URI") {
        config.kavita.base_uri = uri;
    }
    if let Some(api_key) = env("KOMF_KAVITA_API_KEY") {
        config.kavita.api_key = api_key;
    }
    if let Some(uri) = env("KOMF_STUMP_BASE_URI") {
        config.stump.base_uri = uri;
    }
    if let Some(user) = env("KOMF_STUMP_USER") {
        config.stump.username = user;
    }
    if let Some(password) = env("KOMF_STUMP_PASSWORD") {
        config.stump.password = password;
    }
    if let Some(api_key) = env("KOMF_STUMP_API_KEY") {
        config.stump.api_key = api_key;
    }
    if let Some(port) = env("KOMF_SERVER_PORT").and_then(|p| p.parse::<u16>().ok()) {
        config.server.port = port;
    }
    if let Some(level) = env("KOMF_LOG_LEVEL") {
        config.log_level = level;
    }
    if let Some(client_id) = env("KOMF_METADATA_PROVIDERS_MAL_CLIENT_ID") {
        config.metadata_providers.mal_client_id = Some(client_id);
    }
    if let Some(api_key) = env("KOMF_METADATA_PROVIDERS_COMIC_VINE_API_KEY") {
        config.metadata_providers.comic_vine_api_key = Some(api_key);
    }
    if let Some(limit) = env("KOMF_METADATA_PROVIDERS_COMIC_VINE_SEARCH_LIMIT").and_then(|l| l.parse().ok()) {
        config.metadata_providers.comic_vine_search_limit = Some(limit);
    }
    if let Some(token) = env("KOMF_METADATA_PROVIDERS_BANGUMI_TOKEN") {
        config.metadata_providers.bangumi_token = Some(token);
    }

    config.database.file = database_file;
    config.notifications.templates_directory = templates_directory;
    config
}

fn warn_about_disabled_providers(config: &AppConfig) {
    let providers = &config.metadata_providers.default_providers;
    let any_enabled = providers.manga_updates.enabled
        || providers.mal.enabled
        || providers.ani_list.enabled
        || providers.yen_press.enabled
        || providers.viz.enabled
        || providers.book_walker.enabled
        || providers.manga_dex.enabled
        || providers.bangumi.provider.enabled
        || providers.comic_vine.enabled
        || providers.manga_baka.enabled
        || providers.webtoons.enabled;
    if !any_enabled && config.metadata_providers.library_providers.is_empty() {
        tracing::warn!("No metadata providers enabled. You will not be able to get new metadata");
    }
}

/// 配置写入器 —— 对应 `ConfigWriter.kt`。
pub struct ConfigWriter;

impl ConfigWriter {
    pub fn write_config(config: &AppConfig, path: &std::path::Path) -> anyhow::Result<()> {
        let file = if path.is_dir() {
            path.join("application.yml")
        } else {
            path.to_path_buf()
        };
        let yaml = serde_yaml::to_string(config)?;
        std::fs::write(file, yaml)?;
        Ok(())
    }

    pub fn write_config_to_default_path(config: &AppConfig) -> anyhow::Result<()> {
        Self::write_config(config, std::path::Path::new("application.yml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kotlin 原版 application.yml 为 camelCase 字段名；Rust 侧必须兼容解析。
    #[test]
    fn parses_kotlin_style_camel_case_yaml() {
        let yml = r#"
komga:
  baseUri: http://127.0.0.1:25600
  komgaUser: user@test.org
  komgaPassword: secret
  apiKey: komga-key-123
  thumbnailSizeLimit: 1048575
kavita:
  baseUri: http://127.0.0.1:5000
  apiKey: kavita-key-456
metadataProviders:
  nameMatchingMode: EXACT
  defaultProviders:
    mangaUpdates:
      enabled: true
      mediaType: NOVEL
      authorRoles: [WRITER]
server:
  port: 8080
database:
  file: ./db.sqlite
logLevel: DEBUG
"#;
        let config: AppConfig = serde_yaml::from_str(yml).unwrap();
        assert_eq!(config.komga.base_uri, "http://127.0.0.1:25600");
        assert_eq!(config.komga.komga_user, "user@test.org");
        assert_eq!(config.komga.komga_password, "secret");
        assert_eq!(config.komga.api_key, "komga-key-123");
        assert_eq!(config.komga.thumbnail_size_limit, 1048575);
        assert_eq!(config.kavita.base_uri, "http://127.0.0.1:5000");
        assert_eq!(config.kavita.api_key, "kavita-key-456");
        assert_eq!(
            config.metadata_providers.name_matching_mode,
            komf_core::util::NameSimilarityMatcher::Exact
        );
        let manga_updates = &config.metadata_providers.default_providers.manga_updates;
        assert!(manga_updates.enabled);
        assert_eq!(manga_updates.media_type, komf_core::model::MediaType::Novel);
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.database.file, "./db.sqlite");
        assert_eq!(config.log_level, "DEBUG");
    }

    /// 反向：写入配置也应输出 camelCase（Kotlin 兼容）。
    #[test]
    fn serializes_to_kotlin_style_camel_case_yaml() {
        let mut config = AppConfig::default();
        config.kavita.base_uri = "http://127.0.0.1:5000".to_string();
        config.kavita.api_key = "kavita-key-456".to_string();
        let yaml = serde_yaml::to_string(&config).unwrap();
        assert!(yaml.contains("baseUri: http://127.0.0.1:5000"));
        assert!(yaml.contains("apiKey: kavita-key-456"));
        assert!(yaml.contains("metadataProviders:"));
        assert!(!yaml.contains("base_uri:"));
        assert!(!yaml.contains("api_key:"));
    }

    /// bangumi.tagWhitelist 支持英文逗号分隔字符串形式
    #[test]
    fn parses_tag_whitelist_comma_string() {
        let yml = r#"
metadataProviders:
  defaultProviders:
    bangumi:
      enabled: true
      tagWhitelist: "热血,搞笑, 自定义标签"
"#;
        let config: AppConfig = serde_yaml::from_str(yml).unwrap();
        let bangumi = &config.metadata_providers.default_providers.bangumi.provider;
        assert_eq!(
            bangumi.tag_whitelist,
            vec!["热血", "搞笑", "自定义标签"]
        );
    }

    /// bangumi.tagWhitelist 同时兼容数组形式
    #[test]
    fn parses_tag_whitelist_array() {
        let yml = r#"
metadataProviders:
  defaultProviders:
    bangumi:
      enabled: true
      tagWhitelist: ["热血", "搞笑"]
"#;
        let config: AppConfig = serde_yaml::from_str(yml).unwrap();
        let bangumi = &config.metadata_providers.default_providers.bangumi.provider;
        assert_eq!(bangumi.tag_whitelist, vec!["热血", "搞笑"]);
    }
}
