//! 媒体服务器模块装配 —— 对应 `MediaServerModule.kt`。
//!
//! 组装 Komga/Kavita/Stump 客户端、任务追踪、各 library 的元数据服务与更新器，
//! 以及事件监听器（Komga SSE / Kavita SignalR / Stump GraphQL WS），
//! `eventListener.enabled` 时自动启动。
use crate::client::MediaServerClient;
use crate::config::{
    DatabaseConfig, KavitaConfig, KomgaConfig, MetadataProcessingConfig, MetadataUpdateConfig, StumpConfig,
};
use crate::event_listener::{
    KomgaEventHandler, MediaServerEventListener, MetadataEventHandler, NotificationsEventHandler,
};
use crate::jobs::{KomfJobTracker, KomfJobsRepository};
use crate::kavita::{KavitaClient, KavitaMediaServerClientAdapter};
use crate::kavita_signalr::KavitaSignalREventHandler;
use crate::komga::KomgaClient;
use crate::metadata_merger::MetadataMerger;
use crate::metadata_post_processor::{MetadataPostProcessor, PublisherTagNameConfig};
use crate::metadata_service::{MetadataService, MetadataServiceProvider};
use crate::metadata_updater::MetadataUpdater;
use crate::model::{MediaServer, MediaServerLibraryId};
use crate::stump::{StumpClient, StumpMediaServerClientAdapter};
use crate::stump_event::StumpEventHandler;
use komf_core::providers::MetadataProviders;
use komf_notifications::apprise::AppriseCliService;
use komf_notifications::discord::DiscordWebhookService;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub struct MediaServerModule {
    pub job_repository: Arc<KomfJobsRepository>,
    pub job_tracker: Arc<KomfJobTracker>,
    pub komga_client: Arc<dyn MediaServerClient>,
    pub komga_metadata_service_provider: Arc<MetadataServiceProvider>,
    pub kavita_client: Arc<dyn MediaServerClient>,
    kavita_client_core: Arc<KavitaClient>,
    pub kavita_metadata_service_provider: Arc<MetadataServiceProvider>,
    pub stump_client: Arc<dyn MediaServerClient>,
    stump_client_core: Arc<StumpClient>,
    pub stump_metadata_service_provider: Arc<MetadataServiceProvider>,
    listener_tokens: Vec<CancellationToken>,
    /// mylar ${configDir} 占位符基准（=配置目录，热重载重建 updater 时复用）。
    mylar_config_dir: Option<std::path::PathBuf>,
}

impl Drop for MediaServerModule {
    fn drop(&mut self) {
        // 热重载重建模块时停止旧的事件监听器
        for token in &self.listener_tokens {
            token.cancel();
        }
    }
}

impl MediaServerModule {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        komga_config: &KomgaConfig,
        kavita_config: &KavitaConfig,
        stump_config: &StumpConfig,
        database_config: &DatabaseConfig,
        metadata_providers: Arc<MetadataProviders>,
        http_client: reqwest::Client,
        discord_service: DiscordWebhookService,
        apprise_service: AppriseCliService,
        mylar_config_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let repository = Arc::new(
            KomfJobsRepository::open(Path::new(&database_config.file))
                .expect("failed to open database"),
        );
        let job_tracker = Arc::new(KomfJobTracker::new(repository.clone(), "KOMGA"));

        let komga_client_impl = Arc::new(
            KomgaClient::new(
                &komga_config.base_uri,
                &komga_config.komga_user,
                &komga_config.komga_password,
                &komga_config.api_key,
                komga_config.thumbnail_size_limit as u64,
                komga_config
                    .metadata_update
                    .default
                    .post_processing
                    .alternate_title_labels
                    .clone(),
            )
            .expect("failed to create komga client"),
        );
        let komga_client: Arc<dyn MediaServerClient> = komga_client_impl.clone();

        let komga_metadata_service_provider = Arc::new(Self::create_metadata_service_provider(
            &komga_config.metadata_update,
            komga_client.clone(),
            metadata_providers.clone(),
            repository.clone(),
            job_tracker.clone(),
            MediaServer::Komga,
            mylar_config_dir.clone(),
        ));

        let kavita_client_core = Arc::new(
            KavitaClient::new(http_client, &kavita_config.base_uri, &kavita_config.api_key)
                .expect("failed to create kavita client"),
        );
        let kavita_client: Arc<dyn MediaServerClient> = Arc::new(
            KavitaMediaServerClientAdapter::new((*kavita_client_core).clone()),
        );
        let kavita_metadata_service_provider = Arc::new(Self::create_metadata_service_provider(
            &kavita_config.metadata_update,
            kavita_client.clone(),
            metadata_providers.clone(),
            repository.clone(),
            job_tracker.clone(),
            MediaServer::Kavita,
            mylar_config_dir.clone(),
        ));

        // Stump：GraphQL 客户端；API Key 为空时退回账号密码登录 JWT（login 在首次请求时惰性执行）
        let stump_client_core = Arc::new(
            StumpClient::new(
                &stump_config.base_uri,
                &stump_config.username,
                &stump_config.password,
                &stump_config.api_key,
            )
            .expect("failed to create stump client"),
        );
        let stump_client: Arc<dyn MediaServerClient> = Arc::new(
            StumpMediaServerClientAdapter::new((*stump_client_core).clone()),
        );
        let stump_metadata_service_provider = Arc::new(Self::create_metadata_service_provider(
            &stump_config.metadata_update,
            stump_client.clone(),
            metadata_providers.clone(),
            repository.clone(),
            job_tracker.clone(),
            MediaServer::Stump,
            mylar_config_dir.clone(),
        ));

        let mut module = Self {
            job_repository: repository,
            job_tracker,
            komga_client,
            komga_metadata_service_provider,
            kavita_client,
            kavita_client_core,
            kavita_metadata_service_provider,
            stump_client,
            stump_client_core,
            stump_metadata_service_provider,
            listener_tokens: Vec::new(),
            mylar_config_dir,
        };

        if komga_config.event_listener.enabled {
            module.start_komga_listener(
                komga_client_impl,
                &komga_config,
                discord_service.clone(),
                apprise_service.clone(),
            );
        }
        if kavita_config.event_listener.enabled {
            module.start_kavita_listener(
                &kavita_config,
                discord_service.clone(),
                apprise_service.clone(),
            );
        }
        if stump_config.event_listener.enabled {
            module.start_stump_listener(
                &stump_config,
                discord_service,
                apprise_service,
            );
        }

        module
    }

    fn start_komga_listener(
        &mut self,
        komga_client: Arc<KomgaClient>,
        config: &KomgaConfig,
        discord_service: DiscordWebhookService,
        apprise_service: AppriseCliService,
    ) {
        let listeners: Vec<Arc<dyn MediaServerEventListener>> = vec![
            Arc::new(MetadataEventHandler::new(
                self.komga_metadata_service_provider.clone(),
                self.job_repository.clone(),
                self.job_tracker.clone(),
                config.event_listener.metadata_library_filter.clone(),
                config.event_listener.metadata_series_exclude_filter.clone(),
                MediaServer::Komga,
            )),
            Arc::new(NotificationsEventHandler::new(
                self.komga_client.clone(),
                Some(discord_service),
                Some(apprise_service),
                config.event_listener.notifications_library_filter.clone(),
                MediaServer::Komga,
            )),
        ];
        let handler = Arc::new(KomgaEventHandler::new(komga_client, listeners));
        let token = CancellationToken::new();
        let task_token = token.clone();
        let task_handler = handler.clone();
        tokio::spawn(async move {
            task_handler.run(task_token).await;
        });
        self.listener_tokens.push(token);
        tracing::info!("Komga event listener started (SSE)");
    }

    fn start_kavita_listener(
        &mut self,
        config: &KavitaConfig,
        discord_service: DiscordWebhookService,
        apprise_service: AppriseCliService,
    ) {
        let listeners: Vec<Arc<dyn MediaServerEventListener>> = vec![
            Arc::new(MetadataEventHandler::new(
                self.kavita_metadata_service_provider.clone(),
                self.job_repository.clone(),
                self.job_tracker.clone(),
                config.event_listener.metadata_library_filter.clone(),
                config.event_listener.metadata_series_exclude_filter.clone(),
                MediaServer::Kavita,
            )),
            Arc::new(NotificationsEventHandler::new(
                self.kavita_client.clone(),
                Some(discord_service),
                Some(apprise_service),
                config.event_listener.notifications_library_filter.clone(),
                MediaServer::Kavita,
            )),
        ];
        let handler = Arc::new(KavitaSignalREventHandler::new(
            self.kavita_client_core.clone(),
            listeners,
        ));
        let token = CancellationToken::new();
        let task_token = token.clone();
        let task_handler = handler.clone();
        tokio::spawn(async move {
            task_handler.run(task_token).await;
        });
        self.listener_tokens.push(token);
        tracing::info!("Kavita event listener started (signalr)");
    }

    fn start_stump_listener(
        &mut self,
        config: &StumpConfig,
        discord_service: DiscordWebhookService,
        apprise_service: AppriseCliService,
    ) {
        let listeners: Vec<Arc<dyn MediaServerEventListener>> = vec![
            Arc::new(MetadataEventHandler::new(
                self.stump_metadata_service_provider.clone(),
                self.job_repository.clone(),
                self.job_tracker.clone(),
                config.event_listener.metadata_library_filter.clone(),
                config.event_listener.metadata_series_exclude_filter.clone(),
                MediaServer::Stump,
            )),
            Arc::new(NotificationsEventHandler::new(
                self.stump_client.clone(),
                Some(discord_service),
                Some(apprise_service),
                config.event_listener.notifications_library_filter.clone(),
                MediaServer::Stump,
            )),
        ];
        let handler = Arc::new(StumpEventHandler::new(self.stump_client_core.clone(), listeners));
        let token = CancellationToken::new();
        let task_token = token.clone();
        let task_handler = handler.clone();
        tokio::spawn(async move {
            task_handler.run(task_token).await;
        });
        self.listener_tokens.push(token);
        tracing::info!("Stump event listener started (graphql ws)");
    }

    #[allow(clippy::too_many_arguments)]
    fn create_metadata_service_provider(
        config: &MetadataUpdateConfig,
        media_server_client: Arc<dyn MediaServerClient>,
        metadata_providers: Arc<MetadataProviders>,
        repository: Arc<KomfJobsRepository>,
        job_tracker: Arc<KomfJobTracker>,
        media_server: MediaServer,
        mylar_config_dir: Option<std::path::PathBuf>,
    ) -> MetadataServiceProvider {
        let default_updater = Self::create_metadata_update_service(
            &config.default,
            media_server_client.clone(),
            repository.clone(),
            media_server,
            mylar_config_dir.clone(),
        );

        let library_updaters: HashMap<String, Arc<MetadataUpdater>> = config
            .library
            .iter()
            .map(|(library_id, config)| {
                (
                    library_id.clone(),
                    Self::create_metadata_update_service(
                        config,
                        media_server_client.clone(),
                        repository.clone(),
                        media_server,
                        mylar_config_dir.clone(),
                    ),
                )
            })
            .collect();

        let default_service = Self::create_metadata_service(
            &config.default,
            media_server_client.clone(),
            metadata_providers.clone(),
            default_updater.clone(),
            repository.clone(),
            job_tracker.clone(),
            media_server,
            None,
        );

        let library_services: HashMap<String, Arc<MetadataService>> = config
            .library
            .iter()
            .map(|(library_id, config)| {
                (
                    library_id.clone(),
                    Self::create_metadata_service(
                        config,
                        media_server_client.clone(),
                        metadata_providers.clone(),
                        library_updaters.get(library_id).cloned().unwrap_or_else(|| default_updater.clone()),
                        repository.clone(),
                        job_tracker.clone(),
                        media_server,
                        Some(MediaServerLibraryId(library_id.clone())),
                    ),
                )
            })
            .collect();

        MetadataServiceProvider::new(
            default_service,
            library_services,
            default_updater,
            library_updaters,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create_metadata_service(
        config: &MetadataProcessingConfig,
        media_server_client: Arc<dyn MediaServerClient>,
        metadata_providers: Arc<MetadataProviders>,
        metadata_update_service: Arc<MetadataUpdater>,
        repository: Arc<KomfJobsRepository>,
        job_tracker: Arc<KomfJobTracker>,
        media_server: MediaServer,
        library_id: Option<MediaServerLibraryId>,
    ) -> Arc<MetadataService> {
        Arc::new(MetadataService::new(
            media_server_client,
            metadata_providers,
            config.aggregate,
            MetadataMerger::new(config.merge_tags, config.merge_genres),
            metadata_update_service,
            repository,
            media_server.as_str(),
            config.library_type,
            job_tracker,
            config.search_title_extraction.clone(),
            config.post_processing.links_skip_enabled,
            config.post_processing.links_match_enabled,
            config.failed_match_collection_name.clone(),
            library_id,
            config.chinese_conversion.clone(),
        ))
    }

    fn create_metadata_update_service(
        config: &MetadataProcessingConfig,
        media_server_client: Arc<dyn MediaServerClient>,
        repository: Arc<KomfJobsRepository>,
        media_server: MediaServer,
        mylar_config_dir: Option<std::path::PathBuf>,
    ) -> Arc<MetadataUpdater> {
        let post_processor = MetadataPostProcessor::new(
            config.library_type,
            config.post_processing.series_title,
            config.post_processing.series_title_language.clone(),
            config.post_processing.alternative_series_titles,
            config.post_processing.alternative_series_title_languages.clone(),
            config.post_processing.order_books,
            config.post_processing.reading_direction_value,
            config.post_processing.language_value.clone(),
            config.post_processing.fallback_to_alt_title,
            config.post_processing.score_tag_name.clone(),
            config.post_processing.original_publisher_tag_name.clone(),
            config
                .post_processing
                .publisher_tag_names
                .iter()
                .map(|p| PublisherTagNameConfig {
                    tag_name: p.tag_name.clone(),
                    language: p.language.clone(),
                })
                .collect(),
        );

        Arc::new(MetadataUpdater::new(
            media_server_client,
            repository,
            media_server.as_str(),
            post_processor,
            config.update_modes.clone(),
            config.override_existing_covers,
            config.book_covers,
            config.series_covers,
            config.lock_covers,
            config.override_comic_info,
            config.mylar_covers,
            config.mylar_output_dir.clone(),
            mylar_config_dir,
        ))
    }
}
