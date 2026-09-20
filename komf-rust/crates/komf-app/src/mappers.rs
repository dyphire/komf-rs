//! 配置 DTO 映射 —— 对应 `AppConfigMapper.kt` / `AppConfigUpdateMapper.kt`。
use komf_api_models::common::*;
use komf_api_models::config::*;
use komf_core::config::{BangumiConfig, EHentaiConfig, MetadataProvidersConfig, ProviderConfig, ProvidersConfig};
use komf_core::model::{AuthorRole, MediaType, ReadingDirection, UpdateMode};
use komf_core::util::NameSimilarityMatcher;
use komf_mediaserver::config::{
    AlternateTitleLabelsConfig, EventListenerConfig, KavitaConfig, KomgaConfig,
    MetadataProcessingConfig, MetadataPostProcessingConfig, MetadataUpdateConfig,
};
use komf_notifications::NotificationsConfig;
use crate::config::AppConfig;

/// `AppConfig` → `KomfConfig`（/api/config GET）。
///
/// `manga_baka_timestamp` / `manga_baka_checksum` / `book_walker_timestamp` 来自
/// 数据库下载器（对应 Kotlin `AppConfigMapper.toDto` 的 `mangaBakaDbMetadata` /
/// `bookWalkerDbTimestamp` 参数）。
pub fn to_config_dto(
    config: &AppConfig,
    manga_baka_timestamp: Option<&str>,
    manga_baka_checksum: Option<&str>,
    book_walker_timestamp: Option<&str>,
) -> KomfConfig {
    KomfConfig {
        komga: to_komga_dto(&config.komga),
        kavita: to_kavita_dto(&config.kavita),
        notifications: to_notifications_dto(&config.notifications),
        metadata_providers: to_metadata_providers_dto(
            &config.metadata_providers,
            manga_baka_timestamp,
            manga_baka_checksum,
            book_walker_timestamp,
        ),
    }
}

/// 对应 Kotlin AppConfigMapper 的脱敏：长度小于 `threshold` 返回 `"********"`，
/// 否则保留前 4 个字符、其余替换为 `*`（Kotlin 正则 `(?<=.{4}).`）。
/// 用于 malClientId（threshold=32）与 comicVineClientId（threshold=40）。
fn mask_secret(value: &str, threshold: usize) -> String {
    let len = value.chars().count();
    if len < threshold {
        "********".to_string()
    } else {
        let mut chars = value.chars();
        let head: String = chars.by_ref().take(4).collect();
        format!("{head}{}", "*".repeat(len - 4))
    }
}

/// 对应 Kotlin Discord webhook 脱敏：长度 <110 返回 `"********"`，
/// 否则保留前 34 与后 10 个字符、中间替换为 `*`（Kotlin 正则 `(?<=.{34}).(?=.{10})`）。
fn mask_discord_webhook(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let len = chars.len();
    if len < 110 {
        "********".to_string()
    } else {
        chars
            .iter()
            .enumerate()
            .map(|(i, c)| if i >= 34 && i < len - 10 { '*' } else { *c })
            .collect()
    }
}

/// 对应 Kotlin AppConfigMapper.toDto(AppriseConfig)：`take(7) + "*".repeat(50)`。
fn mask_apprise_url(value: &str) -> String {
    let head: String = value.chars().take(7).collect();
    format!("{head}{}", "*".repeat(50))
}

fn to_komga_dto(config: &KomgaConfig) -> KomgaConfigDto {
    KomgaConfigDto {
        base_uri: Some(config.base_uri.clone()),
        komga_user: Some(config.komga_user.clone()),
        // 凭据扩展：GET 不输出（skip_serializing）
        komga_password: None,
        komga_api_key: None,
        event_listener: Some(to_event_listener_dto(&config.event_listener)),
        // komf ≤0.12 兼容：通知库过滤的旧位置（与 eventListener.notificationsLibraryFilter 同值）
        notifications: Some(KomgaNotificationsDto {
            libraries: Some(config.event_listener.notifications_library_filter.clone()),
        }),
        metadata_update: Some(to_metadata_update_dto(&config.metadata_update)),
    }
}

fn to_kavita_dto(config: &KavitaConfig) -> KavitaConfigDto {
    KavitaConfigDto {
        base_uri: Some(config.base_uri.clone()),
        // 凭据扩展：GET 不输出（skip_serializing）
        api_key: None,
        event_listener: Some(to_event_listener_dto(&config.event_listener)),
        // komf ≤0.12 兼容：通知库过滤的旧位置（同 Komga）
        notifications: Some(KomgaNotificationsDto {
            libraries: Some(config.event_listener.notifications_library_filter.clone()),
        }),
        metadata_update: Some(to_metadata_update_dto(&config.metadata_update)),
    }
}

fn to_notifications_dto(config: &NotificationsConfig) -> NotificationConfigDto {
    NotificationConfigDto {
        apprise: Some(AppriseConfigDto {
            // 对齐 Kotlin AppConfigMapper.toDto(AppriseConfig)：每个 url 取前 7 字符 + 50 个 '*'。
            urls: config
                .apprise
                .urls
                .clone()
                .map(|urls| IndexedUrlList::from_vec(urls.iter().map(|u| mask_apprise_url(u)).collect())),
            series_cover: Some(config.apprise.series_cover),
        }),
        discord: Some(DiscordConfigDto {
            // 对齐 Kotlin AppConfigMapper.toDto(DiscordConfig)：webhook 长度 <110 → "********"，
            // 否则保留前 34 与后 10、中间打码。
            webhooks: config
                .discord
                .webhooks
                .clone()
                .map(|webhooks| IndexedUrlList::from_vec(webhooks.iter().map(|w| mask_discord_webhook(w)).collect())),
            series_cover: Some(config.discord.series_cover),
        }),
    }
}

fn to_event_listener_dto(config: &EventListenerConfig) -> EventListenerConfigDto {
    EventListenerConfigDto {
        enabled: Some(config.enabled),
        metadata_library_filter: Some(config.metadata_library_filter.clone()),
        // komf ≤0.12 兼容：metadataLibraryFilter 的旧字段名，输出同值
        libraries: Some(config.metadata_library_filter.clone()),
        metadata_series_exclude_filter: Some(config.metadata_series_exclude_filter.clone()),
        // PATCH 专用字段（Kotlin 名 metadataExcludeSeriesFilter），GET 不输出
        metadata_exclude_series_filter: None,
        notifications_library_filter: Some(config.notifications_library_filter.clone()),
    }
}

fn to_metadata_update_dto(config: &MetadataUpdateConfig) -> MetadataUpdateConfigDto {
    MetadataUpdateConfigDto {
        default: Some(to_processing_dto(&config.default)),
        library: Some(
            config
                .library
                .iter()
                .map(|(k, v)| (k.clone(), Some(to_processing_dto(v))))
                .collect(),
        ),
    }
}

fn to_processing_dto(config: &MetadataProcessingConfig) -> MetadataProcessingConfigDto {
    MetadataProcessingConfigDto {
        library_type: Some(to_media_type_dto(config.library_type)),
        aggregate: Some(config.aggregate),
        merge_tags: Some(config.merge_tags),
        merge_genres: Some(config.merge_genres),
        book_covers: Some(config.book_covers),
        series_covers: Some(config.series_covers),
        override_existing_covers: Some(config.override_existing_covers),
        lock_covers: Some(config.lock_covers),
        update_modes: Some(config.update_modes.iter().map(|m| to_update_mode_dto(*m)).collect()),
        // 对齐 Kotlin GET（AppConfigMapper 不输出 overrideComicInfo）：PATCH 专用，GET 不输出
        override_comic_info: None,
        post_processing: Some(MetadataPostProcessingConfigDto {
            series_title: Some(config.post_processing.series_title),
            series_title_language: config.post_processing.series_title_language.clone().map(Some),
            alternative_series_titles: Some(config.post_processing.alternative_series_titles),
            alternative_series_title_languages: Some(
                config.post_processing.alternative_series_title_languages.clone(),
            ),
            order_books: Some(config.post_processing.order_books),
            reading_direction_value: config
                .post_processing
                .reading_direction_value
                .map(|v| Some(to_reading_direction_dto(v))),
            language_value: config.post_processing.language_value.clone().map(Some),
            fallback_to_alt_title: Some(config.post_processing.fallback_to_alt_title),
            score_tag_name: config.post_processing.score_tag_name.clone().map(Some),
            original_publisher_tag_name: config.post_processing.original_publisher_tag_name.clone().map(Some),
            publisher_tag_names: Some(
                config
                    .post_processing
                    .publisher_tag_names
                    .iter()
                    .map(|p| PublisherTagNameConfigDto {
                        tag_name: p.tag_name.clone(),
                        language: p.language.clone(),
                    })
                    .collect(),
            ),
            alternate_title_labels: Some(AlternateTitleLabelsConfigDto {
                romaji: config
                    .post_processing
                    .alternate_title_labels
                    .romaji
                    .clone()
                    .map(Some),
                native: config
                    .post_processing
                    .alternate_title_labels
                    .native
                    .clone()
                    .map(Some),
                localized: config
                    .post_processing
                    .alternate_title_labels
                    .localized
                    .clone()
                    .map(Some),
            }),
            links_skip_enabled: Some(config.post_processing.links_skip_enabled),
            links_match_enabled: Some(config.post_processing.links_match_enabled),
        }),
        // Rust 扩展：GET 输出失败收藏夹名（Kotlin 无）。
        failed_match_collection_name: config.failed_match_collection_name.clone().map(Some),
        // Rust 扩展：搜索标题提取配置（Kotlin 无）。
        search_title_extraction: Some(to_search_title_extraction_dto(&config.search_title_extraction)),
        // Rust 扩展：简繁转换配置（Kotlin 无）。
        chinese_conversion: Some(to_chinese_conversion_dto(&config.chinese_conversion)),
    }
}

/// 搜索标题提取配置 → DTO。
fn to_search_title_extraction_dto(
    config: &komf_mediaserver::config::SearchTitleExtractionConfig,
) -> komf_api_models::config::SearchTitleExtractionConfigDto {
    komf_api_models::config::SearchTitleExtractionConfigDto {
        enabled: Some(config.enabled),
        bracket_regex: config.bracket_regex.clone().map(Some),
        author_separator: config.author_separator.clone().map(Some),
        title_splitters: Some(config.title_splitters.clone()),
        symbol_normalize_regex: Some(config.symbol_normalize_regex.clone()),
        char_mappings: Some(config.char_mappings.clone()),
        cleanup_regex: Some(config.cleanup_regex.clone()),
    }
}

/// 简繁转换配置 → DTO。
fn to_chinese_conversion_dto(
    config: &komf_mediaserver::config::ChineseConversionConfig,
) -> komf_api_models::config::ChineseConversionConfigDto {
    komf_api_models::config::ChineseConversionConfigDto {
        enabled: Some(config.enabled),
        direction: Some(to_chinese_direction_dto(config.direction)),
        search: Some(config.search),
        matching: Some(config.matching),
        update: Some(komf_api_models::config::ChineseUpdateConfigDto {
            enabled: Some(config.update.enabled),
            fields: Some(config.update.fields.iter().map(|f| to_chinese_field_dto(*f)).collect()),
        }),
    }
}

fn to_chinese_direction_dto(
    d: komf_core::util::ChineseDirection,
) -> komf_api_models::config::KomfChineseDirection {
    match d {
        komf_core::util::ChineseDirection::T2s => komf_api_models::config::KomfChineseDirection::T2s,
        komf_core::util::ChineseDirection::S2t => komf_api_models::config::KomfChineseDirection::S2t,
    }
}

fn from_chinese_direction_dto(
    d: komf_api_models::config::KomfChineseDirection,
) -> komf_core::util::ChineseDirection {
    match d {
        komf_api_models::config::KomfChineseDirection::T2s => komf_core::util::ChineseDirection::T2s,
        komf_api_models::config::KomfChineseDirection::S2t => komf_core::util::ChineseDirection::S2t,
    }
}

fn to_chinese_field_dto(
    f: komf_mediaserver::config::ChineseField,
) -> komf_api_models::config::KomfChineseField {
    match f {
        komf_mediaserver::config::ChineseField::Title => komf_api_models::config::KomfChineseField::Title,
        komf_mediaserver::config::ChineseField::Genres => komf_api_models::config::KomfChineseField::Genres,
        komf_mediaserver::config::ChineseField::Tags => komf_api_models::config::KomfChineseField::Tags,
        komf_mediaserver::config::ChineseField::Summary => komf_api_models::config::KomfChineseField::Summary,
    }
}

fn from_chinese_field_dto(
    f: komf_api_models::config::KomfChineseField,
) -> komf_mediaserver::config::ChineseField {
    match f {
        komf_api_models::config::KomfChineseField::Title => komf_mediaserver::config::ChineseField::Title,
        komf_api_models::config::KomfChineseField::Genres => komf_mediaserver::config::ChineseField::Genres,
        komf_api_models::config::KomfChineseField::Tags => komf_mediaserver::config::ChineseField::Tags,
        komf_api_models::config::KomfChineseField::Summary => komf_mediaserver::config::ChineseField::Summary,
    }
}

fn to_metadata_providers_dto(
    config: &MetadataProvidersConfig,
    manga_baka_timestamp: Option<&str>,
    manga_baka_checksum: Option<&str>,
    book_walker_timestamp: Option<&str>,
) -> MetadataProvidersConfigDto {
    // 对应 Kotlin：`MangaBakaDatabaseDto` 仅当元数据有效（timestamp+checksum 均存在）时给出
    let manga_baka_database = match (manga_baka_timestamp, manga_baka_checksum) {
        (Some(timestamp), Some(checksum)) => Some(MangaBakaDatabaseDto {
            download_timestamp: timestamp.to_string(),
            checksum: checksum.to_string(),
        }),
        _ => None,
    };
    MetadataProvidersConfigDto {
        // 对齐 Kotlin AppConfigMapper.toDto(MetadataProvidersConfig)：
        // malClientId 长度 <32 → "********"，否则保留前 4 字符、其余打码；
        // comicVineClientId 阈值为 40，规则相同。
        // Option<Option<T>>：值为 null 时输出 None（不序列化，Kotlin encodeDefaults=false 同）。
        mal_client_id: config
            .mal_client_id
            .clone()
            .map(|v| Some(mask_secret(&v, 32))),
        comic_vine_client_id: config
            .comic_vine_api_key
            .clone()
            .map(|v| Some(mask_secret(&v, 40))),
        comic_vine_search_limit: config.comic_vine_search_limit.map(Some),
        comic_vine_issue_name: config.comic_vine_issue_name.clone().map(Some),
        comic_vine_id_format: config.comic_vine_id_format.clone().map(Some),
        bangumi_token: config
            .bangumi_token
            .clone()
            .map(|v| Some(mask_secret(&v, 16))),
        name_matching_mode: Some(to_name_matching_mode_dto(config.name_matching_mode)),
        default_providers: Some(to_providers_dto(&config.default_providers)),
        library_providers: Some(
            config
                .library_providers
                .iter()
                .map(|(k, v)| (k.clone(), Some(to_providers_dto(v))))
                .collect(),
        ),
        manga_baka_database,
        book_walker_download_date: book_walker_timestamp.map(str::to_string),
    }
}

fn to_providers_dto(config: &ProvidersConfig) -> ProvidersConfigDto {
    ProvidersConfigDto {
        manga_baka: Some(MangaBakaConfigDto {
            priority: Some(config.manga_baka.priority),
            enabled: Some(config.manga_baka.enabled),
            series_metadata: Some(to_series_metadata_dto(&config.manga_baka.series_metadata)),
            // komf ≤0.12 兼容：输出默认 bookMetadata（脚本对所有 provider 读取，缺失会崩）
            book_metadata: Some(default_book_metadata_config_dto()),
            name_matching_mode: config.manga_baka.name_matching_mode.map(|m| Some(to_name_matching_mode_dto(m))),
            media_type: Some(to_media_type_dto(config.manga_baka.media_type)),
            author_roles: Some(config.manga_baka.author_roles.iter().map(|r| to_author_role_dto(*r)).collect()),
            artist_roles: Some(config.manga_baka.artist_roles.iter().map(|r| to_author_role_dto(*r)).collect()),
            mode: Some(to_manga_baka_mode_dto(config.manga_baka.mode)),
        }),
        book_walker: Some(to_provider_dto(&config.book_walker)),
        manga_dex: Some(MangaDexConfigDto {
            priority: Some(config.manga_dex.priority),
            enabled: Some(config.manga_dex.enabled),
            series_metadata: Some(to_series_metadata_dto(&config.manga_dex.series_metadata)),
            book_metadata: Some(to_book_metadata_dto(&config.manga_dex.book_metadata)),
            name_matching_mode: config.manga_dex.name_matching_mode.map(|m| Some(to_name_matching_mode_dto(m))),
            media_type: Some(to_media_type_dto(config.manga_dex.media_type)),
            author_roles: Some(config.manga_dex.author_roles.iter().map(|r| to_author_role_dto(*r)).collect()),
            artist_roles: Some(config.manga_dex.artist_roles.iter().map(|r| to_author_role_dto(*r)).collect()),
            cover_languages: Some(config.manga_dex.cover_languages.clone()),
            links: Some(config.manga_dex.links.iter().map(|l| to_mangadex_link_dto(*l)).collect()),
        }),
        manga_updates: Some(to_provider_dto(&config.manga_updates)),
        ani_list: Some(AniListConfigDto {
            priority: Some(config.ani_list.priority),
            enabled: Some(config.ani_list.enabled),
            series_metadata: Some(to_series_metadata_dto(&config.ani_list.series_metadata)),
            // komf ≤0.12 兼容：输出默认 bookMetadata（脚本对所有 provider 读取，缺失会崩）
            book_metadata: Some(default_book_metadata_config_dto()),
            name_matching_mode: config.ani_list.name_matching_mode.map(|m| Some(to_name_matching_mode_dto(m))),
            media_type: Some(to_media_type_dto(config.ani_list.media_type)),
            author_roles: Some(config.ani_list.author_roles.iter().map(|r| to_author_role_dto(*r)).collect()),
            artist_roles: Some(config.ani_list.artist_roles.iter().map(|r| to_author_role_dto(*r)).collect()),
            tags_score_threshold: Some(config.ani_list.tags_score_threshold),
            tags_size_limit: Some(config.ani_list.tags_size_limit),
        }),
        mal: Some(to_provider_dto(&config.mal)),
        comic_vine: Some(to_provider_dto(&config.comic_vine)),
        nautiljon: Some(default_provider_config_dto()),
        yen_press: Some(to_provider_dto(&config.yen_press)),
        kodansha: Some(default_provider_config_dto()),
        viz: Some(to_provider_dto(&config.viz)),
        bangumi: Some(BangumiConfigDto {
            priority: Some(config.bangumi.provider.priority),
            enabled: Some(config.bangumi.provider.enabled),
            series_metadata: Some(to_series_metadata_dto(&config.bangumi.provider.series_metadata)),
            book_metadata: Some(to_book_metadata_dto(&config.bangumi.provider.book_metadata)),
            name_matching_mode: config
                .bangumi
                .provider
                .name_matching_mode
                .map(|m| Some(to_name_matching_mode_dto(m))),
            media_type: Some(to_media_type_dto(config.bangumi.provider.media_type)),
            author_roles: Some(
                config
                    .bangumi
                    .provider
                    .author_roles
                    .iter()
                    .map(|r| to_author_role_dto(*r))
                    .collect(),
            ),
            artist_roles: Some(
                config
                    .bangumi
                    .provider
                    .artist_roles
                    .iter()
                    .map(|r| to_author_role_dto(*r))
                    .collect(),
            ),
            tag_whitelist: Some(config.bangumi.provider.tag_whitelist.clone()),
            tag_whitelist_file: config.bangumi.provider.tag_whitelist_file.clone(),
            archive: Some(BangumiArchiveConfigDto {
                enabled: Some(config.bangumi.archive.enabled),
                dir: config.bangumi.archive.dir.clone(),
                update_interval_hours: Some(config.bangumi.archive.update_interval_hours),
                idle_release_secs: config.bangumi.archive.idle_release_secs,
            }),
        }),
        hentag: Some(default_provider_config_dto()),
        webtoons: Some(to_provider_dto(&config.webtoons)),
        e_hentai: Some(to_ehentai_dto(&config.e_hentai)),
    }
}

fn default_provider_config_dto() -> ProviderConfigDto {
    ProviderConfigDto {
        priority: Some(10),
        enabled: Some(false),
        series_metadata: Some(default_series_metadata_config_dto()),
        book_metadata: Some(default_book_metadata_config_dto()),
        name_matching_mode: None,
        media_type: Some(KomfMediaType::Manga),
        author_roles: Some(Vec::new()),
        artist_roles: Some(Vec::new()),
        tag_whitelist: Some(Vec::new()),
        tag_whitelist_file: None,
    }
}

fn default_series_metadata_config_dto() -> SeriesMetadataConfigDto {
    SeriesMetadataConfigDto {
        status: Some(true),
        title: Some(true),
        summary: Some(true),
        publisher: Some(true),
        reading_direction: Some(true),
        age_rating: Some(true),
        language: Some(true),
        genres: Some(true),
        tags: Some(true),
        total_book_count: Some(true),
        authors: Some(true),
        release_date: Some(true),
        thumbnail: Some(true),
        links: Some(true),
        books: Some(true),
        use_original_publisher: Some(false),
        original_publisher_tag_name: None,
        english_publisher_tag_name: None,
        french_publisher_tag_name: None,
    }
}

fn default_book_metadata_config_dto() -> BookMetadataConfigDto {
    BookMetadataConfigDto {
        title: Some(true),
        summary: Some(true),
        number: Some(true),
        number_sort: Some(true),
        release_date: Some(true),
        authors: Some(true),
        tags: Some(true),
        isbn: Some(true),
        links: Some(true),
        thumbnail: Some(true),
    }
}

fn to_provider_dto(config: &ProviderConfig) -> ProviderConfigDto {
    ProviderConfigDto {
        priority: Some(config.priority),
        enabled: Some(config.enabled),
        series_metadata: Some(to_series_metadata_dto(&config.series_metadata)),
        book_metadata: Some(to_book_metadata_dto(&config.book_metadata)),
        name_matching_mode: config.name_matching_mode.map(|m| Some(to_name_matching_mode_dto(m))),
        media_type: Some(to_media_type_dto(config.media_type)),
        author_roles: Some(config.author_roles.iter().map(|r| to_author_role_dto(*r)).collect()),
        artist_roles: Some(config.artist_roles.iter().map(|r| to_author_role_dto(*r)).collect()),
        tag_whitelist: Some(config.tag_whitelist.clone()),
        tag_whitelist_file: config.tag_whitelist_file.clone(),
    }
}

/// EHentai 独立 DTO（含 preferredLanguages）；tagWhitelist 输出空数组（脚本兼容）。
fn to_ehentai_dto(config: &EHentaiConfig) -> EHentaiConfigDto {
    EHentaiConfigDto {
        priority: Some(config.priority),
        enabled: Some(config.enabled),
        series_metadata: Some(to_series_metadata_dto(&config.series_metadata)),
        book_metadata: Some(to_book_metadata_dto(&config.book_metadata)),
        name_matching_mode: config.name_matching_mode.map(|m| Some(to_name_matching_mode_dto(m))),
        media_type: Some(to_media_type_dto(config.media_type)),
        author_roles: Some(config.author_roles.iter().map(|r| to_author_role_dto(*r)).collect()),
        artist_roles: Some(config.artist_roles.iter().map(|r| to_author_role_dto(*r)).collect()),
        preferred_languages: Some(config.preferred_languages.clone()),
        tag_whitelist: Some(Vec::new()),
        title_priority: Some(config.title_priority.clone()),
        translator_keywords: Some(config.translator_keywords.clone()),
        male_only_tags_file: config.male_only_tags_file.clone(),
        title_template: Some(config.title_template.clone()),
        tag_translation_enabled: Some(config.tag_translation_enabled),
        tag_translation_url: config.tag_translation_url.clone(),
        search_domain: Some(config.search_domain.clone()),
        gid_only_match: Some(config.gid_only_match),
        ipb_member_id: config.ipb_member_id.clone(),
        ipb_pass_hash: config.ipb_pass_hash.clone(),
    }
}

fn to_series_metadata_dto(config: &komf_core::config::SeriesMetadataConfig) -> SeriesMetadataConfigDto {
    SeriesMetadataConfigDto {
        status: Some(config.status),
        title: Some(config.title),
        summary: Some(config.summary),
        publisher: Some(config.publisher),
        reading_direction: Some(config.reading_direction),
        age_rating: Some(config.age_rating),
        language: Some(config.language),
        genres: Some(config.genres),
        tags: Some(config.tags),
        total_book_count: Some(config.total_book_count),
        authors: Some(config.authors),
        release_date: Some(config.release_date),
        thumbnail: Some(config.thumbnail),
        links: Some(config.links),
        books: Some(config.books),
        use_original_publisher: Some(config.use_original_publisher),
        // 对齐 Kotlin AppConfigMapper：输出空字符串（脚本 UI 空输入框契约），非 null
        original_publisher_tag_name: Some(String::new()),
        english_publisher_tag_name: Some(String::new()),
        french_publisher_tag_name: Some(String::new()),
    }
}

fn to_book_metadata_dto(config: &komf_core::config::BookMetadataConfig) -> BookMetadataConfigDto {
    BookMetadataConfigDto {
        title: Some(config.title),
        summary: Some(config.summary),
        number: Some(config.number),
        number_sort: Some(config.number_sort),
        release_date: Some(config.release_date),
        authors: Some(config.authors),
        tags: Some(config.tags),
        isbn: Some(config.isbn),
        links: Some(config.links),
        thumbnail: Some(config.thumbnail),
    }
}

// ---------------------------------------------------------------------------
// DTO → AppConfig（PATCH 更新；缺省字段保持原值，对应 Kotlin PatchValue）
// ---------------------------------------------------------------------------

pub fn apply_config_update(mut config: AppConfig, request: &KomfConfigUpdateRequest) -> AppConfig {
    if let Some(komga) = &request.komga {
        if let Some(base_uri) = &komga.base_uri {
            config.komga.base_uri = base_uri.clone();
        }
        if let Some(user) = &komga.komga_user {
            config.komga.komga_user = user.clone();
        }
        // 凭据扩展：脚本 v0.12.2 设置 UI 保存 komga 密码时 PATCH 发送 komgaPassword
        // （仅在非空时发送，缺省保持原值）—— 对齐 Kotlin DeprecatedConfigUpdateMapper
        // `komgaPassword = patch.komgaPassword ?: config.komgaPassword`。
        if let Some(password) = &komga.komga_password {
            config.komga.komga_password = password.clone();
        }
        // 凭据扩展：PATCH 显式提供时覆盖 API key（脚本 UI 无入口，curl/API 可写）。
        // 仅在 Some 时覆盖，避免缺省（None）清掉 env/yml 已配置的 key。
        if let Some(api_key) = &komga.komga_api_key {
            config.komga.api_key = api_key.clone();
        }
        if let Some(event_listener) = &komga.event_listener {
            config.komga.event_listener = from_event_listener_dto(event_listener, &config.komga.event_listener);
        }
        // komf ≤0.12 兼容：旧位置 notifications.libraries → eventListener.notificationsLibraryFilter
        if let Some(notifications) = &komga.notifications {
            if let Some(libraries) = &notifications.libraries {
                config.komga.event_listener.notifications_library_filter = libraries.clone();
            }
        }
        if let Some(metadata_update) = &komga.metadata_update {
            config.komga.metadata_update = from_metadata_update_dto(metadata_update, &config.komga.metadata_update);
        }
    }
    if let Some(kavita) = &request.kavita {
        if let Some(base_uri) = &kavita.base_uri {
            config.kavita.base_uri = base_uri.clone();
        }
        // 凭据扩展：脚本 v0.12.2 设置 UI 有 Kavita apiKey 输入框，PATCH 发送 apiKey。
        if let Some(api_key) = &kavita.api_key {
            config.kavita.api_key = api_key.clone();
        }
        if let Some(event_listener) = &kavita.event_listener {
            config.kavita.event_listener = from_event_listener_dto(event_listener, &config.kavita.event_listener);
        }
        // komf ≤0.12 兼容：旧位置 notifications.libraries → eventListener.notificationsLibraryFilter
        if let Some(notifications) = &kavita.notifications {
            if let Some(libraries) = &notifications.libraries {
                config.kavita.event_listener.notifications_library_filter = libraries.clone();
            }
        }
        if let Some(metadata_update) = &kavita.metadata_update {
            config.kavita.metadata_update = from_metadata_update_dto(metadata_update, &config.kavita.metadata_update);
        }
    }
    if let Some(notifications) = &request.notifications {
        if let Some(apprise) = &notifications.apprise {
            if let Some(urls) = &apprise.urls {
                // 索引合并（对齐 Kotlin `old + patch → values.filterNotNull()`）
                let base = config.notifications.apprise.urls.clone().unwrap_or_default();
                config.notifications.apprise.urls = Some(urls.apply_merge(&base));
            }
            if let Some(series_cover) = apprise.series_cover {
                config.notifications.apprise.series_cover = series_cover;
            }
        }
        if let Some(discord) = &notifications.discord {
            if let Some(webhooks) = &discord.webhooks {
                // 索引合并（对齐 Kotlin `old + patch → values.filterNotNull()`）
                let base = config.notifications.discord.webhooks.clone().unwrap_or_default();
                config.notifications.discord.webhooks = Some(webhooks.apply_merge(&base));
            }
            if let Some(series_cover) = discord.series_cover {
                config.notifications.discord.series_cover = series_cover;
            }
        }
    }
    if let Some(providers) = &request.metadata_providers {
        // Option<Option<T>> 三态：Some(None)=清空；Some(Some(v))=设置；None=保持
        if let Some(mal_client_id) = &providers.mal_client_id {
            config.metadata_providers.mal_client_id = mal_client_id.clone();
        }
        if let Some(comic_vine_client_id) = &providers.comic_vine_client_id {
            config.metadata_providers.comic_vine_api_key = comic_vine_client_id.clone();
        }
        if let Some(comic_vine_search_limit) = providers.comic_vine_search_limit {
            config.metadata_providers.comic_vine_search_limit = comic_vine_search_limit;
        }
        if let Some(comic_vine_issue_name) = &providers.comic_vine_issue_name {
            config.metadata_providers.comic_vine_issue_name = comic_vine_issue_name.clone();
        }
        if let Some(comic_vine_id_format) = &providers.comic_vine_id_format {
            config.metadata_providers.comic_vine_id_format = comic_vine_id_format.clone();
        }
        if let Some(bangumi_token) = &providers.bangumi_token {
            config.metadata_providers.bangumi_token = bangumi_token.clone();
        }
        if let Some(name_matching_mode) = providers.name_matching_mode {
            // Kotlin getOrNull()：Some=设置；缺省/None=保持（无清空语义）
            config.metadata_providers.name_matching_mode = from_name_matching_mode_dto(name_matching_mode);
        }
        if let Some(default_providers) = &providers.default_providers {
            config.metadata_providers.default_providers =
                from_providers_dto(default_providers, &config.metadata_providers.default_providers);
        }
        if let Some(library_providers) = &providers.library_providers {
            // Kotlin `Map<String, ProvidersConfigUpdateRequest?>`：value null = 删除该库
            let mut merged = config.metadata_providers.library_providers.clone();
            for (library_id, provider_dto) in library_providers {
                match provider_dto {
                    None => {
                        merged.remove(library_id);
                    }
                    Some(dto) => {
                        let base = merged
                            .get(library_id)
                            .cloned()
                            .unwrap_or_default();
                        merged.insert(library_id.clone(), from_providers_dto(dto, &base));
                    }
                }
            }
            config.metadata_providers.library_providers = merged;
        }
    }
    config
}

fn from_event_listener_dto(dto: &EventListenerConfigDto, base: &EventListenerConfig) -> EventListenerConfig {
    EventListenerConfig {
        enabled: dto.enabled.unwrap_or(base.enabled),
        metadata_library_filter: dto
            .metadata_library_filter
            .clone()
            // komf ≤0.12 兼容：旧字段名 libraries 作为回退
            .or_else(|| dto.libraries.clone())
            .unwrap_or_else(|| base.metadata_library_filter.clone()),
        metadata_series_exclude_filter: dto
            .metadata_exclude_series_filter
            .clone()
            // Kotlin 更新请求字段名 `metadataExcludeSeriesFilter` 优先，回退旧 Rust 字段名
            .or_else(|| dto.metadata_series_exclude_filter.clone())
            .unwrap_or_else(|| base.metadata_series_exclude_filter.clone()),
        notifications_library_filter: dto
            .notifications_library_filter
            .clone()
            .unwrap_or_else(|| base.notifications_library_filter.clone()),
    }
}

fn from_metadata_update_dto(dto: &MetadataUpdateConfigDto, base: &MetadataUpdateConfig) -> MetadataUpdateConfig {
    MetadataUpdateConfig {
        default: dto
            .default
            .as_ref()
            .map(|d| from_processing_dto(d, &base.default))
            .unwrap_or_else(|| base.default.clone()),
        library: dto
            .library
            .as_ref()
            .map(|m| {
                // Kotlin `Map<String, MetadataProcessingConfigUpdateRequest?>`：value null = 删除
                let mut merged = base.library.clone();
                for (library_id, processing_dto) in m {
                    match processing_dto {
                        None => {
                            merged.remove(library_id);
                        }
                        Some(d) => {
                            let base_processing = merged
                                .get(library_id)
                                .cloned()
                                .unwrap_or_else(|| base.default.clone());
                            merged.insert(library_id.clone(), from_processing_dto(d, &base_processing));
                        }
                    }
                }
                merged
            })
            .unwrap_or_else(|| base.library.clone()),
    }
}

fn from_processing_dto(dto: &MetadataProcessingConfigDto, base: &MetadataProcessingConfig) -> MetadataProcessingConfig {
    MetadataProcessingConfig {
        library_type: dto.library_type.map(from_media_type_dto).unwrap_or(base.library_type),
        aggregate: dto.aggregate.unwrap_or(base.aggregate),
        merge_tags: dto.merge_tags.unwrap_or(base.merge_tags),
        merge_genres: dto.merge_genres.unwrap_or(base.merge_genres),
        book_covers: dto.book_covers.unwrap_or(base.book_covers),
        series_covers: dto.series_covers.unwrap_or(base.series_covers),
        override_existing_covers: dto.override_existing_covers.unwrap_or(base.override_existing_covers),
        lock_covers: dto.lock_covers.unwrap_or(base.lock_covers),
        update_modes: dto
            .update_modes
            .as_ref()
            .map(|m| m.iter().map(|m| from_update_mode_dto(*m)).collect())
            .unwrap_or_else(|| base.update_modes.clone()),
        override_comic_info: dto.override_comic_info.unwrap_or(base.override_comic_info),
        post_processing: dto
            .post_processing
            .as_ref()
            .map(|p| from_post_processing_dto(p, &base.post_processing))
            .unwrap_or_else(|| base.post_processing.clone()),
        search_title_extraction: dto
            .search_title_extraction
            .as_ref()
            .map(|d| from_search_title_extraction_dto(d, &base.search_title_extraction))
            .unwrap_or_else(|| base.search_title_extraction.clone()),
        failed_match_collection_name: match &dto.failed_match_collection_name {
            Some(v) => v.clone(),
            None => base.failed_match_collection_name.clone(),
        },
        chinese_conversion: match &dto.chinese_conversion {
            Some(c) => from_chinese_conversion_dto(c, &base.chinese_conversion),
            None => base.chinese_conversion.clone(),
        },
    }
}

/// 搜索标题提取 DTO → 配置（逐字段三态合并：None=保持 base）。
fn from_search_title_extraction_dto(
    dto: &komf_api_models::config::SearchTitleExtractionConfigDto,
    base: &komf_mediaserver::config::SearchTitleExtractionConfig,
) -> komf_mediaserver::config::SearchTitleExtractionConfig {
    komf_mediaserver::config::SearchTitleExtractionConfig {
        enabled: dto.enabled.unwrap_or(base.enabled),
        bracket_regex: match &dto.bracket_regex {
            Some(v) => v.clone(),
            None => base.bracket_regex.clone(),
        },
        author_separator: match &dto.author_separator {
            Some(v) => v.clone(),
            None => base.author_separator.clone(),
        },
        title_splitters: dto
            .title_splitters
            .clone()
            .unwrap_or_else(|| base.title_splitters.clone()),
        symbol_normalize_regex: dto
            .symbol_normalize_regex
            .clone()
            .unwrap_or_else(|| base.symbol_normalize_regex.clone()),
        char_mappings: dto
            .char_mappings
            .clone()
            .unwrap_or_else(|| base.char_mappings.clone()),
        cleanup_regex: dto
            .cleanup_regex
            .clone()
            .unwrap_or_else(|| base.cleanup_regex.clone()),
    }
}

/// 简繁转换 DTO → 配置（逐字段三态合并：None=保持 base）。
fn from_chinese_conversion_dto(
    dto: &komf_api_models::config::ChineseConversionConfigDto,
    base: &komf_mediaserver::config::ChineseConversionConfig,
) -> komf_mediaserver::config::ChineseConversionConfig {
    komf_mediaserver::config::ChineseConversionConfig {
        enabled: dto.enabled.unwrap_or(base.enabled),
        direction: dto
            .direction
            .map(from_chinese_direction_dto)
            .unwrap_or(base.direction),
        search: dto.search.unwrap_or(base.search),
        matching: dto.matching.unwrap_or(base.matching),
        update: match &dto.update {
            Some(u) => komf_mediaserver::config::ChineseUpdateConfig {
                enabled: u.enabled.unwrap_or(base.update.enabled),
                fields: u
                    .fields
                    .as_ref()
                    .map(|fs| fs.iter().map(|f| from_chinese_field_dto(*f)).collect())
                    .unwrap_or_else(|| base.update.fields.clone()),
            },
            None => base.update.clone(),
        },
    }
}

fn from_post_processing_dto(
    dto: &MetadataPostProcessingConfigDto,
    base: &MetadataPostProcessingConfig,
) -> MetadataPostProcessingConfig {
    MetadataPostProcessingConfig {
        series_title: dto.series_title.unwrap_or(base.series_title),
        // Option<Option<T>> 三态：Some(None)=清空；Some(Some(v))=设置；None=保持
        series_title_language: match &dto.series_title_language {
            Some(v) => v.clone(),
            None => base.series_title_language.clone(),
        },
        alternative_series_titles: dto.alternative_series_titles.unwrap_or(base.alternative_series_titles),
        alternative_series_title_languages: dto
            .alternative_series_title_languages
            .clone()
            .unwrap_or_else(|| base.alternative_series_title_languages.clone()),
        order_books: dto.order_books.unwrap_or(base.order_books),
        reading_direction_value: match &dto.reading_direction_value {
            Some(v) => v.map(from_reading_direction_dto),
            None => base.reading_direction_value,
        },
        language_value: match &dto.language_value {
            Some(v) => v.clone(),
            None => base.language_value.clone(),
        },
        fallback_to_alt_title: dto.fallback_to_alt_title.unwrap_or(base.fallback_to_alt_title),
        score_tag_name: match &dto.score_tag_name {
            Some(v) => v.clone(),
            None => base.score_tag_name.clone(),
        },
        original_publisher_tag_name: match &dto.original_publisher_tag_name {
            Some(v) => v.clone(),
            None => base.original_publisher_tag_name.clone(),
        },
        publisher_tag_names: dto
            .publisher_tag_names
            .as_ref()
            .map(|p| {
                p.iter()
                    .map(|p| komf_mediaserver::config::PublisherTagNameConfig {
                        tag_name: p.tag_name.clone(),
                        language: p.language.clone(),
                    })
                    .collect()
            })
            .unwrap_or_else(|| base.publisher_tag_names.clone()),
        alternate_title_labels: match &dto.alternate_title_labels {
            Some(labels) => AlternateTitleLabelsConfig {
                romaji: match &labels.romaji {
                    Some(v) => v.clone(),
                    None => base.alternate_title_labels.romaji.clone(),
                },
                native: match &labels.native {
                    Some(v) => v.clone(),
                    None => base.alternate_title_labels.native.clone(),
                },
                localized: match &labels.localized {
                    Some(v) => v.clone(),
                    None => base.alternate_title_labels.localized.clone(),
                },
            },
            None => base.alternate_title_labels.clone(),
        },
        links_skip_enabled: dto.links_skip_enabled.unwrap_or(base.links_skip_enabled),
        links_match_enabled: dto.links_match_enabled.unwrap_or(base.links_match_enabled),
    }
}

fn from_providers_dto(dto: &ProvidersConfigDto, base: &ProvidersConfig) -> ProvidersConfig {
    ProvidersConfig {
        manga_baka: dto
            .manga_baka
            .as_ref()
            .map(|d| from_manga_baka_dto(d, &base.manga_baka))
            .unwrap_or_else(|| base.manga_baka.clone()),
        book_walker: dto
            .book_walker
            .as_ref()
            .map(|d| from_provider_dto(d, &base.book_walker))
            .unwrap_or_else(|| base.book_walker.clone()),
        manga_dex: dto
            .manga_dex
            .as_ref()
            .map(|d| from_mangadex_dto(d, &base.manga_dex))
            .unwrap_or_else(|| base.manga_dex.clone()),
        manga_updates: dto
            .manga_updates
            .as_ref()
            .map(|d| from_provider_dto(d, &base.manga_updates))
            .unwrap_or_else(|| base.manga_updates.clone()),
        ani_list: dto
            .ani_list
            .as_ref()
            .map(|d| from_anilist_dto(d, &base.ani_list))
            .unwrap_or_else(|| base.ani_list.clone()),
        mal: dto
            .mal
            .as_ref()
            .map(|d| from_provider_dto(d, &base.mal))
            .unwrap_or_else(|| base.mal.clone()),
        comic_vine: dto
            .comic_vine
            .as_ref()
            .map(|d| from_provider_dto(d, &base.comic_vine))
            .unwrap_or_else(|| base.comic_vine.clone()),
        yen_press: dto
            .yen_press
            .as_ref()
            .map(|d| from_provider_dto(d, &base.yen_press))
            .unwrap_or_else(|| base.yen_press.clone()),
        viz: dto
            .viz
            .as_ref()
            .map(|d| from_provider_dto(d, &base.viz))
            .unwrap_or_else(|| base.viz.clone()),
        bangumi: BangumiConfig {
            provider: dto
                .bangumi
                .as_ref()
                .map(|d| {
                    from_provider_dto(
                        &ProviderConfigDto {
                            priority: d.priority,
                            enabled: d.enabled,
                            series_metadata: d.series_metadata.clone(),
                            book_metadata: d.book_metadata.clone(),
                            name_matching_mode: d.name_matching_mode.clone(),
                            media_type: d.media_type,
                            author_roles: d.author_roles.clone(),
                            artist_roles: d.artist_roles.clone(),
                            tag_whitelist: d.tag_whitelist.clone(),
                            tag_whitelist_file: d.tag_whitelist_file.clone(),
                        },
                        &base.bangumi.provider,
                    )
                })
                .unwrap_or_else(|| base.bangumi.provider.clone()),
            archive: dto
                .bangumi
                .as_ref()
                .and_then(|d| d.archive.as_ref())
                .map(|a| komf_core::config::BangumiArchiveConfig {
                    enabled: a.enabled.unwrap_or(base.bangumi.archive.enabled),
                    dir: a.dir.clone().or_else(|| base.bangumi.archive.dir.clone()),
                    update_interval_hours: a.update_interval_hours.unwrap_or(base.bangumi.archive.update_interval_hours),
                    idle_release_secs: a.idle_release_secs.or(base.bangumi.archive.idle_release_secs),
                })
                .unwrap_or_else(|| base.bangumi.archive.clone()),
        },
        webtoons: dto
            .webtoons
            .as_ref()
            .map(|d| from_provider_dto(d, &base.webtoons))
            .unwrap_or_else(|| base.webtoons.clone()),
        e_hentai: dto
            .e_hentai
            .as_ref()
            .map(|d| from_ehentai_dto(d, &base.e_hentai))
            .unwrap_or_else(|| base.e_hentai.clone()),
    }
}

fn from_provider_dto(dto: &ProviderConfigDto, base: &ProviderConfig) -> ProviderConfig {
    ProviderConfig {
        priority: dto.priority.unwrap_or(base.priority),
        enabled: dto.enabled.unwrap_or(base.enabled),
        series_metadata: dto
            .series_metadata
            .as_ref()
            .map(|d| from_series_metadata_dto(d, &base.series_metadata))
            .unwrap_or_else(|| base.series_metadata.clone()),
        book_metadata: dto
            .book_metadata
            .as_ref()
            .map(|d| from_book_metadata_dto(d, &base.book_metadata))
            .unwrap_or_else(|| base.book_metadata.clone()),
        name_matching_mode: match &dto.name_matching_mode {
            Some(v) => v.map(from_name_matching_mode_dto),
            None => base.name_matching_mode,
        },
        media_type: dto.media_type.map(from_media_type_dto).unwrap_or(base.media_type),
        author_roles: dto
            .author_roles
            .as_ref()
            .map(|r| r.iter().map(|r| from_author_role_dto(*r)).collect())
            .unwrap_or_else(|| base.author_roles.clone()),
        artist_roles: dto
            .artist_roles
            .as_ref()
            .map(|r| r.iter().map(|r| from_author_role_dto(*r)).collect())
            .unwrap_or_else(|| base.artist_roles.clone()),
        tag_whitelist: dto.tag_whitelist.clone().unwrap_or_else(|| base.tag_whitelist.clone()),
        tag_whitelist_file: dto.tag_whitelist_file.clone().or_else(|| base.tag_whitelist_file.clone()),
    }
}

fn from_ehentai_dto(dto: &EHentaiConfigDto, base: &EHentaiConfig) -> EHentaiConfig {
    EHentaiConfig {
        priority: dto.priority.unwrap_or(base.priority),
        enabled: dto.enabled.unwrap_or(base.enabled),
        series_metadata: dto
            .series_metadata
            .as_ref()
            .map(|d| from_series_metadata_dto(d, &base.series_metadata))
            .unwrap_or_else(|| base.series_metadata.clone()),
        book_metadata: dto
            .book_metadata
            .as_ref()
            .map(|d| from_book_metadata_dto(d, &base.book_metadata))
            .unwrap_or_else(|| base.book_metadata.clone()),
        name_matching_mode: match &dto.name_matching_mode {
            Some(v) => v.map(from_name_matching_mode_dto),
            None => base.name_matching_mode,
        },
        media_type: dto.media_type.map(from_media_type_dto).unwrap_or(base.media_type),
        author_roles: dto
            .author_roles
            .as_ref()
            .map(|r| r.iter().map(|r| from_author_role_dto(*r)).collect())
            .unwrap_or_else(|| base.author_roles.clone()),
        artist_roles: dto
            .artist_roles
            .as_ref()
            .map(|r| r.iter().map(|r| from_author_role_dto(*r)).collect())
            .unwrap_or_else(|| base.artist_roles.clone()),
        preferred_languages: dto
            .preferred_languages
            .clone()
            .unwrap_or_else(|| base.preferred_languages.clone()),
        title_priority: dto
            .title_priority
            .clone()
            .unwrap_or_else(|| base.title_priority.clone()),
        translator_keywords: dto
            .translator_keywords
            .clone()
            .unwrap_or_else(|| base.translator_keywords.clone()),
        male_only_tags_file: dto
            .male_only_tags_file
            .clone()
            .or_else(|| base.male_only_tags_file.clone()),
        title_template: dto
            .title_template
            .clone()
            .unwrap_or_else(|| base.title_template.clone()),
        tag_translation_enabled: dto
            .tag_translation_enabled
            .unwrap_or(base.tag_translation_enabled),
        tag_translation_url: dto
            .tag_translation_url
            .clone()
            .or_else(|| base.tag_translation_url.clone()),
        search_domain: dto
            .search_domain
            .clone()
            .unwrap_or_else(|| base.search_domain.clone()),
        gid_only_match: dto.gid_only_match.unwrap_or(base.gid_only_match),
        ipb_member_id: dto.ipb_member_id.clone().or_else(|| base.ipb_member_id.clone()),
        ipb_pass_hash: dto.ipb_pass_hash.clone().or_else(|| base.ipb_pass_hash.clone()),
    }
}

fn from_anilist_dto(dto: &AniListConfigDto, base: &komf_core::config::AniListConfig) -> komf_core::config::AniListConfig {
    komf_core::config::AniListConfig {
        priority: dto.priority.unwrap_or(base.priority),
        enabled: dto.enabled.unwrap_or(base.enabled),
        series_metadata: dto
            .series_metadata
            .as_ref()
            .map(|d| from_series_metadata_dto(d, &base.series_metadata))
            .unwrap_or_else(|| base.series_metadata.clone()),
        name_matching_mode: match &dto.name_matching_mode {
            Some(v) => v.map(from_name_matching_mode_dto),
            None => base.name_matching_mode,
        },
        media_type: dto.media_type.map(from_media_type_dto).unwrap_or(base.media_type),
        author_roles: dto
            .author_roles
            .as_ref()
            .map(|r| r.iter().map(|r| from_author_role_dto(*r)).collect())
            .unwrap_or_else(|| base.author_roles.clone()),
        artist_roles: dto
            .artist_roles
            .as_ref()
            .map(|r| r.iter().map(|r| from_author_role_dto(*r)).collect())
            .unwrap_or_else(|| base.artist_roles.clone()),
        tags_score_threshold: dto.tags_score_threshold.unwrap_or(base.tags_score_threshold),
        tags_size_limit: dto.tags_size_limit.unwrap_or(base.tags_size_limit),
    }
}

fn from_mangadex_dto(dto: &MangaDexConfigDto, base: &komf_core::config::MangaDexConfig) -> komf_core::config::MangaDexConfig {
    komf_core::config::MangaDexConfig {
        priority: dto.priority.unwrap_or(base.priority),
        enabled: dto.enabled.unwrap_or(base.enabled),
        series_metadata: dto
            .series_metadata
            .as_ref()
            .map(|d| from_series_metadata_dto(d, &base.series_metadata))
            .unwrap_or_else(|| base.series_metadata.clone()),
        book_metadata: dto
            .book_metadata
            .as_ref()
            .map(|d| from_book_metadata_dto(d, &base.book_metadata))
            .unwrap_or_else(|| base.book_metadata.clone()),
        name_matching_mode: match &dto.name_matching_mode {
            Some(v) => v.map(from_name_matching_mode_dto),
            None => base.name_matching_mode,
        },
        media_type: dto.media_type.map(from_media_type_dto).unwrap_or(base.media_type),
        author_roles: dto
            .author_roles
            .as_ref()
            .map(|r| r.iter().map(|r| from_author_role_dto(*r)).collect())
            .unwrap_or_else(|| base.author_roles.clone()),
        artist_roles: dto
            .artist_roles
            .as_ref()
            .map(|r| r.iter().map(|r| from_author_role_dto(*r)).collect())
            .unwrap_or_else(|| base.artist_roles.clone()),
        cover_languages: dto.cover_languages.clone().unwrap_or_else(|| base.cover_languages.clone()),
        links: dto
            .links
            .as_ref()
            .map(|l| l.iter().map(|l| from_mangadex_link_dto(*l)).collect())
            .unwrap_or_else(|| base.links.clone()),
    }
}

fn from_manga_baka_dto(dto: &MangaBakaConfigDto, base: &komf_core::config::MangaBakaConfig) -> komf_core::config::MangaBakaConfig {
    komf_core::config::MangaBakaConfig {
        priority: dto.priority.unwrap_or(base.priority),
        enabled: dto.enabled.unwrap_or(base.enabled),
        series_metadata: dto
            .series_metadata
            .as_ref()
            .map(|d| from_series_metadata_dto(d, &base.series_metadata))
            .unwrap_or_else(|| base.series_metadata.clone()),
        name_matching_mode: match &dto.name_matching_mode {
            Some(v) => v.map(from_name_matching_mode_dto),
            None => base.name_matching_mode,
        },
        media_type: dto.media_type.map(from_media_type_dto).unwrap_or(base.media_type),
        author_roles: dto
            .author_roles
            .as_ref()
            .map(|r| r.iter().map(|r| from_author_role_dto(*r)).collect())
            .unwrap_or_else(|| base.author_roles.clone()),
        artist_roles: dto
            .artist_roles
            .as_ref()
            .map(|r| r.iter().map(|r| from_author_role_dto(*r)).collect())
            .unwrap_or_else(|| base.artist_roles.clone()),
        mode: dto.mode.map(from_manga_baka_mode_dto).unwrap_or(base.mode),
    }
}

fn from_series_metadata_dto(
    dto: &SeriesMetadataConfigDto,
    base: &komf_core::config::SeriesMetadataConfig,
) -> komf_core::config::SeriesMetadataConfig {
    komf_core::config::SeriesMetadataConfig {
        status: dto.status.unwrap_or(base.status),
        title: dto.title.unwrap_or(base.title),
        title_sort: dto.title.unwrap_or(base.title_sort),
        summary: dto.summary.unwrap_or(base.summary),
        publisher: dto.publisher.unwrap_or(base.publisher),
        reading_direction: dto.reading_direction.unwrap_or(base.reading_direction),
        age_rating: dto.age_rating.unwrap_or(base.age_rating),
        language: dto.language.unwrap_or(base.language),
        genres: dto.genres.unwrap_or(base.genres),
        tags: dto.tags.unwrap_or(base.tags),
        total_book_count: dto.total_book_count.unwrap_or(base.total_book_count),
        authors: dto.authors.unwrap_or(base.authors),
        release_date: dto.release_date.unwrap_or(base.release_date),
        thumbnail: dto.thumbnail.unwrap_or(base.thumbnail),
        books: dto.books.unwrap_or(base.books),
        links: dto.links.unwrap_or(base.links),
        score: base.score,
        use_original_publisher: dto.use_original_publisher.unwrap_or(base.use_original_publisher),
    }
}

fn from_book_metadata_dto(
    dto: &BookMetadataConfigDto,
    base: &komf_core::config::BookMetadataConfig,
) -> komf_core::config::BookMetadataConfig {
    komf_core::config::BookMetadataConfig {
        title: dto.title.unwrap_or(base.title),
        summary: dto.summary.unwrap_or(base.summary),
        number: dto.number.unwrap_or(base.number),
        number_sort: dto.number_sort.unwrap_or(base.number_sort),
        release_date: dto.release_date.unwrap_or(base.release_date),
        authors: dto.authors.unwrap_or(base.authors),
        tags: dto.tags.unwrap_or(base.tags),
        isbn: dto.isbn.unwrap_or(base.isbn),
        links: dto.links.unwrap_or(base.links),
        thumbnail: dto.thumbnail.unwrap_or(base.thumbnail),
    }
}

// ---------------------------------------------------------------------------
// 枚举转换（本地函数，避免对外部类型实现 trait）
// ---------------------------------------------------------------------------

pub(crate) fn to_media_type_dto(v: MediaType) -> KomfMediaType {
    match v {
        MediaType::Manga => KomfMediaType::Manga,
        MediaType::Novel => KomfMediaType::Novel,
        MediaType::Comic => KomfMediaType::Comic,
        MediaType::Webtoon => KomfMediaType::Webtoon,
    }
}

fn from_media_type_dto(v: KomfMediaType) -> MediaType {
    match v {
        KomfMediaType::Manga => MediaType::Manga,
        KomfMediaType::Novel => MediaType::Novel,
        KomfMediaType::Comic => MediaType::Comic,
        KomfMediaType::Webtoon => MediaType::Webtoon,
    }
}

fn to_update_mode_dto(v: UpdateMode) -> KomfUpdateMode {
    match v {
        UpdateMode::Api => KomfUpdateMode::Api,
        UpdateMode::ComicInfo => KomfUpdateMode::ComicInfo,
    }
}

fn from_update_mode_dto(v: KomfUpdateMode) -> UpdateMode {
    match v {
        KomfUpdateMode::Api => UpdateMode::Api,
        KomfUpdateMode::ComicInfo => UpdateMode::ComicInfo,
    }
}

fn to_reading_direction_dto(v: ReadingDirection) -> KomfReadingDirection {
    match v {
        ReadingDirection::LeftToRight => KomfReadingDirection::LeftToRight,
        ReadingDirection::RightToLeft => KomfReadingDirection::RightToLeft,
        ReadingDirection::Vertical => KomfReadingDirection::Vertical,
        ReadingDirection::Webtoon => KomfReadingDirection::Webtoon,
    }
}

fn from_reading_direction_dto(v: KomfReadingDirection) -> ReadingDirection {
    match v {
        KomfReadingDirection::LeftToRight => ReadingDirection::LeftToRight,
        KomfReadingDirection::RightToLeft => ReadingDirection::RightToLeft,
        KomfReadingDirection::Vertical => ReadingDirection::Vertical,
        KomfReadingDirection::Webtoon => ReadingDirection::Webtoon,
    }
}

fn to_author_role_dto(v: AuthorRole) -> KomfAuthorRole {
    match v {
        AuthorRole::Writer => KomfAuthorRole::Writer,
        AuthorRole::Penciller => KomfAuthorRole::Penciller,
        AuthorRole::Inker => KomfAuthorRole::Inker,
        AuthorRole::Colorist => KomfAuthorRole::Colorist,
        AuthorRole::Letterer => KomfAuthorRole::Letterer,
        AuthorRole::Cover => KomfAuthorRole::Cover,
        AuthorRole::Editor => KomfAuthorRole::Editor,
        AuthorRole::Translator => KomfAuthorRole::Translator,
    }
}

fn from_author_role_dto(v: KomfAuthorRole) -> AuthorRole {
    match v {
        KomfAuthorRole::Writer => AuthorRole::Writer,
        KomfAuthorRole::Penciller => AuthorRole::Penciller,
        KomfAuthorRole::Inker => AuthorRole::Inker,
        KomfAuthorRole::Colorist => AuthorRole::Colorist,
        KomfAuthorRole::Letterer => AuthorRole::Letterer,
        KomfAuthorRole::Cover => AuthorRole::Cover,
        KomfAuthorRole::Editor => AuthorRole::Editor,
        KomfAuthorRole::Translator => AuthorRole::Translator,
    }
}

fn to_name_matching_mode_dto(v: NameSimilarityMatcher) -> KomfNameMatchingMode {
    match v {
        NameSimilarityMatcher::Exact => KomfNameMatchingMode::Exact,
        NameSimilarityMatcher::ClosestMatch => KomfNameMatchingMode::ClosestMatch,
    }
}

fn from_name_matching_mode_dto(v: KomfNameMatchingMode) -> NameSimilarityMatcher {
    match v {
        KomfNameMatchingMode::Exact => NameSimilarityMatcher::Exact,
        KomfNameMatchingMode::ClosestMatch => NameSimilarityMatcher::ClosestMatch,
    }
}

fn to_mangadex_link_dto(v: komf_core::config::MangaDexLink) -> MangaDexLink {
    use komf_core::config::MangaDexLink as C;
    match v {
        C::MangaDex => MangaDexLink::MangaDex,
        C::Anilist => MangaDexLink::Anilist,
        C::AnimePlanet => MangaDexLink::AnimePlanet,
        C::BookwalkerJp => MangaDexLink::BookwalkerJp,
        C::MangaUpdates => MangaDexLink::MangaUpdates,
        C::NovelUpdates => MangaDexLink::NovelUpdates,
        C::Kitsu => MangaDexLink::Kitsu,
        C::Amazon => MangaDexLink::Amazon,
        C::EbookJapan => MangaDexLink::EbookJapan,
        C::MyAnimeList => MangaDexLink::MyAnimeList,
        C::CdJapan => MangaDexLink::CdJapan,
        C::Raw => MangaDexLink::Raw,
        C::EnglishTl => MangaDexLink::EnglishTl,
    }
}

fn from_mangadex_link_dto(v: MangaDexLink) -> komf_core::config::MangaDexLink {
    use komf_core::config::MangaDexLink as C;
    match v {
        MangaDexLink::MangaDex => C::MangaDex,
        MangaDexLink::Anilist => C::Anilist,
        MangaDexLink::AnimePlanet => C::AnimePlanet,
        MangaDexLink::BookwalkerJp => C::BookwalkerJp,
        MangaDexLink::MangaUpdates => C::MangaUpdates,
        MangaDexLink::NovelUpdates => C::NovelUpdates,
        MangaDexLink::Kitsu => C::Kitsu,
        MangaDexLink::Amazon => C::Amazon,
        MangaDexLink::EbookJapan => C::EbookJapan,
        MangaDexLink::MyAnimeList => C::MyAnimeList,
        MangaDexLink::CdJapan => C::CdJapan,
        MangaDexLink::Raw => C::Raw,
        MangaDexLink::EnglishTl => C::EnglishTl,
    }
}

fn to_manga_baka_mode_dto(v: komf_core::config::MangaBakaMode) -> MangaBakaMode {
    match v {
        komf_core::config::MangaBakaMode::Api => MangaBakaMode::Api,
        komf_core::config::MangaBakaMode::Database => MangaBakaMode::Database,
    }
}

fn from_manga_baka_mode_dto(v: MangaBakaMode) -> komf_core::config::MangaBakaMode {
    match v {
        MangaBakaMode::Api => komf_core::config::MangaBakaMode::Api,
        MangaBakaMode::Database => komf_core::config::MangaBakaMode::Database,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 脚本 v0.12.2 PATCH /config 保存 komga 密码链路：
    /// 发送 `komgaPassword` 覆盖运行配置；缺省保持原值；GET 不输出密码。
    #[test]
    fn patch_saves_komga_password_and_get_hides_it() {
        // 脚本 32089-32092：user 变更时发 komgaUser；密码框启用且非空时发 komgaPassword
        let request: KomfConfigUpdateRequest =
            serde_json::from_str(r#"{"komga":{"komgaUser":"alice","komgaPassword":"s3cret"}}"#).unwrap();
        let config = apply_config_update(AppConfig::default(), &request);
        assert_eq!(config.komga.komga_user, "alice");
        assert_eq!(config.komga.komga_password, "s3cret");

        // 仅改 user、不携带 password → password 保持原值（Kotlin `?: config.komgaPassword`）
        let request: KomfConfigUpdateRequest =
            serde_json::from_str(r#"{"komga":{"komgaUser":"bob"}}"#).unwrap();
        let config = apply_config_update(config, &request);
        assert_eq!(config.komga.komga_user, "bob");
        assert_eq!(config.komga.komga_password, "s3cret");

        // GET DTO 序列化不得输出密码（脚本 passwordDisabled 判定契约）
        let dto = to_config_dto(&config, None, None, None);
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("komgaPassword"));
        assert!(!json.contains("s3cret"));
    }

    /// PATCH 三态（对齐 Kotlin PatchValue）：
    /// 缺省=保持、null=清空、有值=设置 —— malClientId / comicVine* / provider nameMatchingMode / postProcessing。
    #[test]
    fn patch_three_state_clear_and_set() {
        // 初始 mal_client_id = "secret-mal"
        let mut config = AppConfig::default();
        config.metadata_providers.mal_client_id = Some("secret-mal".to_string());
        config.metadata_providers.comic_vine_api_key = Some("secret-cv".to_string());
        config.metadata_providers.default_providers.bangumi.provider.name_matching_mode =
            Some(NameSimilarityMatcher::ClosestMatch);
        config.metadata_providers.default_providers.bangumi.provider.media_type = MediaType::Manga;
        config.metadata_providers.default_providers.bangumi.provider.priority = 10;
        config.komga.metadata_update.default.post_processing.series_title_language =
            Some("en".to_string());

        // null → 清空
        let request: KomfConfigUpdateRequest = serde_json::from_str(
            r#"{"metadataProviders":{"malClientId":null,"comicVineClientId":null,"defaultProviders":{"bangumi":{"nameMatchingMode":null}}},"komga":{"metadataUpdate":{"default":{"postProcessing":{"seriesTitleLanguage":null}}}}}"#,
        )
        .unwrap();
        let updated = apply_config_update(config.clone(), &request);
        assert_eq!(updated.metadata_providers.mal_client_id, None);
        assert_eq!(updated.metadata_providers.comic_vine_api_key, None);
        assert_eq!(updated.metadata_providers.default_providers.bangumi.provider.name_matching_mode, None);
        assert_eq!(updated.komga.metadata_update.default.post_processing.series_title_language, None);

        // 有值 → 设置
        let request: KomfConfigUpdateRequest = serde_json::from_str(
            r#"{"metadataProviders":{"malClientId":"new-mal","defaultProviders":{"bangumi":{"nameMatchingMode":"EXACT"}}},"komga":{"metadataUpdate":{"default":{"postProcessing":{"seriesTitleLanguage":"fr"}}}}}"#,
        )
        .unwrap();
        let updated = apply_config_update(config.clone(), &request);
        assert_eq!(updated.metadata_providers.mal_client_id.as_deref(), Some("new-mal"));
        assert_eq!(
            updated.metadata_providers.default_providers.bangumi.provider.name_matching_mode,
            Some(NameSimilarityMatcher::Exact)
        );
        assert_eq!(
            updated.komga.metadata_update.default.post_processing.series_title_language.as_deref(),
            Some("fr")
        );

        // 缺省 → 保持（PATCH 增量语义）
        let request: KomfConfigUpdateRequest = serde_json::from_str(r#"{"metadataProviders":{}}"#).unwrap();
        let updated = apply_config_update(config.clone(), &request);
        assert_eq!(updated.metadata_providers.mal_client_id.as_deref(), Some("secret-mal"));
        assert_eq!(
            updated.metadata_providers.default_providers.bangumi.provider.name_matching_mode,
            Some(NameSimilarityMatcher::ClosestMatch)
        );
        assert_eq!(
            updated.komga.metadata_update.default.post_processing.series_title_language.as_deref(),
            Some("en")
        );
        // 缺省不触碰无关字段
        assert_eq!(updated.metadata_providers.default_providers.bangumi.provider.media_type, MediaType::Manga);
        assert_eq!(updated.metadata_providers.default_providers.bangumi.provider.priority, 10);

        // 顶层 nameMatchingMode：Kotlin getOrNull() 语义 —— null 不生效（保持原值）
        let request: KomfConfigUpdateRequest =
            serde_json::from_str(r#"{"metadataProviders":{"nameMatchingMode":null}}"#).unwrap();
        let updated = apply_config_update(config.clone(), &request);
        assert_eq!(updated.metadata_providers.name_matching_mode, config.metadata_providers.name_matching_mode);
    }

    /// PATCH 库级配置删除：libraryProviders / metadataUpdate.library 的 value 为 null → 删除该库。
    #[test]
    fn patch_library_map_null_deletes() {
        let mut config = AppConfig::default();
        let mut libs = std::collections::HashMap::new();
        libs.insert("lib-a".to_string(), ProvidersConfig::default());
        libs.insert("lib-b".to_string(), ProvidersConfig::default());
        config.metadata_providers.library_providers = libs;
        let mut libs = std::collections::HashMap::new();
        libs.insert("lib-a".to_string(), MetadataProcessingConfig::default());
        config.komga.metadata_update.library = libs;

        // 删除 lib-a、更新 lib-b（设置 priority）、新增 lib-c
        let request: KomfConfigUpdateRequest = serde_json::from_str(
            r#"{"metadataProviders":{"libraryProviders":{"lib-a":null,"lib-b":{"bangumi":{"priority":7}},"lib-c":{"bangumi":{"priority":3}}}},"komga":{"metadataUpdate":{"library":{"lib-a":null,"lib-b":{"libraryType":"MANGA"}}}}}"#,
        )
        .unwrap();
        let updated = apply_config_update(config, &request);
        assert!(!updated.metadata_providers.library_providers.contains_key("lib-a"));
        assert_eq!(updated.metadata_providers.library_providers["lib-b"].bangumi.provider.priority, 7);
        assert_eq!(updated.metadata_providers.library_providers["lib-c"].bangumi.provider.priority, 3);
        assert!(!updated.komga.metadata_update.library.contains_key("lib-a"));
        assert_eq!(
            updated.komga.metadata_update.library["lib-b"].library_type,
            MediaType::Manga
        );
    }

    /// PATCH webhooks/urls 索引合并（对齐 Kotlin `old + patch → values.filterNotNull()`）。
    #[test]
    fn patch_webhooks_index_merge() {
        let mut config = AppConfig::default();
        config.notifications.discord.webhooks = Some(vec!["a".to_string(), "b".to_string()]);
        config.notifications.apprise.urls = Some(vec!["u0".to_string(), "u1".to_string()]);

        let request: KomfConfigUpdateRequest = serde_json::from_str(
            r#"{"notifications":{"discord":{"webhooks":{"0":"a2","2":"c"}},"apprise":{"urls":{"1":null,"3":"u3"}}}}"#,
        )
        .unwrap();
        let updated = apply_config_update(config, &request);
        // discord: a→a2、b 保持、追加 c
        assert_eq!(
            updated.notifications.discord.webhooks,
            Some(vec!["a2".to_string(), "b".to_string(), "c".to_string()])
        );
        // apprise: u1 被 null 删除、u0 保持、追加 u3
        assert_eq!(
            updated.notifications.apprise.urls,
            Some(vec!["u0".to_string(), "u3".to_string()])
        );
    }

    /// PATCH overrideComicInfo 与 eventListener 的 Kotlin 字段名 metadataExcludeSeriesFilter。
    #[test]
    fn patch_override_comic_info_and_event_listener_field_name() {
        let mut config = AppConfig::default();
        config.komga.metadata_update.default.override_comic_info = false;

        // Kotlin 字段名 metadataExcludeSeriesFilter（GET 输出仍为 metadataSeriesExcludeFilter）
        let request: KomfConfigUpdateRequest = serde_json::from_str(
            r#"{"komga":{"metadataUpdate":{"default":{"overrideComicInfo":true}},"eventListener":{"metadataExcludeSeriesFilter":["excluded-series"]}}}"#,
        )
        .unwrap();
        let updated = apply_config_update(config, &request);
        assert!(updated.komga.metadata_update.default.override_comic_info);
        assert_eq!(updated.komga.event_listener.metadata_series_exclude_filter, vec!["excluded-series"]);

        // GET 输出仍用 metadataSeriesExcludeFilter，且不带 PATCH 专用字段
        let dto = to_config_dto(&updated, None, None, None);
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("metadataSeriesExcludeFilter"));
        assert!(!json.contains("metadataExcludeSeriesFilter"));
    }

    /// 复现 HTTP 实测：PATCH body `{"komga":{"metadataUpdate":{...}}}` 应把
    /// seriesTitleLanguage 设置/清空，且写回 DTO 后 GET 输出一致。
    #[test]
    fn http_body_patch_komga_metadata_update_roundtrip() {
        // 设置
        let request: KomfConfigUpdateRequest = serde_json::from_str(
            r#"{"komga":{"metadataUpdate":{"default":{"postProcessing":{"seriesTitleLanguage":"en"}}}}}"#,
        )
        .unwrap();
        assert!(
            request
                .komga
                .as_ref()
                .unwrap()
                .metadata_update
                .as_ref()
                .unwrap()
                .default
                .as_ref()
                .unwrap()
                .post_processing
                .as_ref()
                .unwrap()
                .series_title_language
                .is_some(),
            "seriesTitleLanguage must deserialize from HTTP body"
        );
        let config = AppConfig::default();
        let updated = apply_config_update(config.clone(), &request);
        assert_eq!(
            updated.komga.metadata_update.default.post_processing.series_title_language.as_deref(),
            Some("en")
        );
        // GET DTO 输出 en（序列化链路）
        let dto = to_config_dto(&updated, None, None, None);
        let json = serde_json::to_string(&dto).unwrap();
        assert!(
            json.contains("\"seriesTitleLanguage\":\"en\""),
            "GET must output en, got {json}"
        );
        // 清空
        let request: KomfConfigUpdateRequest = serde_json::from_str(
            r#"{"komga":{"metadataUpdate":{"default":{"postProcessing":{"seriesTitleLanguage":null}}}}}"#,
        )
        .unwrap();
        let updated = apply_config_update(updated, &request);
        assert_eq!(updated.komga.metadata_update.default.post_processing.series_title_language, None);
        let dto = to_config_dto(&updated, None, None, None);
        let value = serde_json::to_value(&dto).unwrap();
        let komga_lang = value["komga"]["metadataUpdate"]["default"]["postProcessing"]["seriesTitleLanguage"]
            .as_str()
            .map(|s| s.to_string());
        assert_eq!(komga_lang, None, "komga seriesTitleLanguage must be cleared");
    }
}
