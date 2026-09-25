//! 配置相关 API DTO —— 对应 `snd.komf.api.config` 包。
//!
//! 全部字段为 `Option`（反序列化缺省即 `None`），使 `PATCH /api/config` 具备
//! Kotlin `PatchValue` 语义：缺省字段保持原值；`GET` 由 mapper 填充 `Some`。
use super::common::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfConfig {
    pub komga: KomgaConfigDto,
    pub kavita: KavitaConfigDto,
    pub stump: StumpConfigDto,
    pub notifications: NotificationConfigDto,
    pub metadata_providers: MetadataProvidersConfigDto,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KomgaConfigDto {
    pub base_uri: Option<String>,
    pub komga_user: Option<String>,
    /// 凭据扩展（脚本 v0.12.2 设置 UI 输入 komga 密码，PATCH 发送 `komgaPassword`；
    /// Kotlin deprecated `KomgaConfigUpdateDto.komgaPassword` 同语义）：
    /// GET 不输出（保持与 Kotlin 契约一致，脚本 passwordDisabled 判定不变）；
    /// PATCH 接收后覆盖 `KomgaConfig.komga_password`（Basic 认证）。
    #[serde(skip_serializing)]
    pub komga_password: Option<String>,
    /// 凭据扩展（非 Kotlin DTO 字段，Kotlin 走 env/yml）：
    /// GET 不输出（保持与 Kotlin 契约一致，脚本 passwordDisabled 判定不变）；
    /// PATCH 接收后覆盖 `KomgaConfig.api_key`（X-API-Key 认证）。
    #[serde(skip_serializing)]
    pub komga_api_key: Option<String>,
    pub event_listener: Option<EventListenerConfigDto>,
    /// komf ≤0.12 旧字段：通知库过滤（`eventListener.notificationsLibraryFilter` 的旧位置）。
    /// GET 输出与 `event_listener.notifications_library_filter` 同值；PATCH 接收时映射过去。
    pub notifications: Option<KomgaNotificationsDto>,
    pub metadata_update: Option<MetadataUpdateConfigDto>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KavitaConfigDto {
    pub base_uri: Option<String>,
    /// 凭据扩展（非 Kotlin DTO 字段，Kotlin 走 env/yml）：
    /// GET 不输出；PATCH 接收后覆盖 `KavitaConfig.api_key`。
    /// 脚本 v0.12.2 设置 UI 有 Kavita apiKey 输入框（PATCH 发送 `apiKey`）。
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    pub event_listener: Option<EventListenerConfigDto>,
    /// komf ≤0.12 旧字段：通知库过滤（同 `KomgaConfigDto.notifications` 语义）。
    pub notifications: Option<KomgaNotificationsDto>,
    pub metadata_update: Option<MetadataUpdateConfigDto>,
}

/// Stump 媒体服务器配置（Rust 扩展；Kotlin komf 无此 DTO）。
///
/// 凭据同 Komga/Kavita 契约：GET 不输出 password/apiKey（skip_serializing），
/// PATCH 仅在显式提供时覆盖，缺省保持原值。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StumpConfigDto {
    pub base_uri: Option<String>,
    pub username: Option<String>,
    /// 凭据扩展：GET 不输出；PATCH 接收后覆盖 `StumpConfig.password`（登录换 JWT 用）。
    #[serde(skip_serializing)]
    pub password: Option<String>,
    /// 凭据扩展：GET 不输出；PATCH 接收后覆盖 `StumpConfig.api_key`（推荐认证方式）。
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    pub event_listener: Option<EventListenerConfigDto>,
    pub metadata_update: Option<MetadataUpdateConfigDto>,
}

/// komf ≤0.12 的通知库过滤结构：`{ "libraries": [...] }`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KomgaNotificationsDto {
    pub libraries: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MetadataUpdateConfigDto {
    pub default: Option<MetadataProcessingConfigDto>,
    /// PATCH：value 为 null 时删除该库配置（对应 Kotlin `Map<String, MetadataProcessingConfigUpdateRequest?>`）。
    pub library: Option<std::collections::HashMap<String, Option<MetadataProcessingConfigDto>>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MetadataProcessingConfigDto {
    pub library_type: Option<KomfMediaType>,
    pub aggregate: Option<bool>,
    pub merge_tags: Option<bool>,
    pub merge_genres: Option<bool>,
    pub book_covers: Option<bool>,
    pub series_covers: Option<bool>,
    pub override_existing_covers: Option<bool>,
    pub lock_covers: Option<bool>,
    pub update_modes: Option<Vec<KomfUpdateMode>>,
    /// 对应 Kotlin `MetadataProcessingConfigUpdateRequest.overrideComicInfo`（PATCH 可更新）。
    pub override_comic_info: Option<bool>,
    /// Rust 扩展：mylar series.json 导出时同时下载系列封面（默认 false）。
    pub mylar_covers: Option<bool>,
    /// Rust 扩展：mylar 导出根目录（对齐 py --output）。三态：缺省=保持；null=回系列原目录；有值=设置。
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub mylar_output_dir: Option<Option<String>>,
    pub post_processing: Option<MetadataPostProcessingConfigDto>,
    /// 搜索标题提取配置（Rust 扩展，Kotlin 无）。GET 输出；PATCH 接收时合并。
    pub search_title_extraction: Option<SearchTitleExtractionConfigDto>,
    /// Auto-Identify Library 匹配失败的系列加入的收藏夹名称（Rust 扩展）。
    /// Option 三态：缺省=保持；null=清空；有值=设置。
    pub failed_match_collection_name: Option<Option<String>>,
    /// Rust 扩展：简繁转换配置（Kotlin 无）。缺省=保持；null=重置为默认。
    pub chinese_conversion: Option<ChineseConversionConfigDto>,
}

/// 搜索标题提取配置（DTO；子字段：None=保持原值，Some=覆盖）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchTitleExtractionConfigDto {
    pub enabled: Option<bool>,
    pub bracket_regex: Option<Option<String>>,
    pub author_separator: Option<Option<String>>,
    pub title_splitters: Option<Vec<String>>,
    pub symbol_normalize_regex: Option<String>,
    /// 单字符映射表（`[["／","/"]]`）。None=保持；Some=整体覆盖。
    pub char_mappings: Option<Vec<(String, String)>>,
    pub cleanup_regex: Option<Vec<String>>,
}

/// 简繁转换配置（DTO；三态子字段：None=保持原值，Some=覆盖）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChineseConversionConfigDto {
    pub enabled: Option<bool>,
    /// 转换方向：`t2s`（繁体→简体）/ `s2t`（简体→繁体）。
    pub direction: Option<KomfChineseDirection>,
    /// 是否应用于搜索关键词。
    pub search: Option<bool>,
    /// 是否应用于自动匹配。
    pub matching: Option<bool>,
    /// 元数据更新应用配置。
    pub update: Option<ChineseUpdateConfigDto>,
}

/// 元数据更新简繁转换配置（DTO）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChineseUpdateConfigDto {
    pub enabled: Option<bool>,
    /// 应用字段：title / genres / tags / summary。
    pub fields: Option<Vec<KomfChineseField>>,
}

/// 简繁转换方向（DTO）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KomfChineseDirection {
    T2s,
    S2t,
}

/// 简繁转换应用字段（DTO）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KomfChineseField {
    Title,
    Genres,
    Tags,
    Summary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MetadataPostProcessingConfigDto {
    pub series_title: Option<bool>,
    /// `Option<Option<T>>` 三态（对应 Kotlin PatchValue）：缺省=保持；null=清空；有值=设置。
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub series_title_language: Option<Option<String>>,
    pub alternative_series_titles: Option<bool>,
    pub alternative_series_title_languages: Option<Vec<String>>,
    pub order_books: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub reading_direction_value: Option<Option<KomfReadingDirection>>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub language_value: Option<Option<String>>,
    pub fallback_to_alt_title: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub score_tag_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub original_publisher_tag_name: Option<Option<String>>,
    pub publisher_tag_names: Option<Vec<PublisherTagNameConfigDto>>,
    pub alternate_title_labels: Option<AlternateTitleLabelsConfigDto>,
    pub links_skip_enabled: Option<bool>,
    pub links_match_enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublisherTagNameConfigDto {
    pub tag_name: String,
    pub language: String,
}

/// alternate titles label 本地化映射 DTO（Rust 扩展，Kotlin 无此配置）。
/// 每个字段为 `Option<Option<String>>` 三态：缺省=保持；null=清空回默认；有值=自定义。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AlternateTitleLabelsConfigDto {
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub romaji: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub native: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub localized: Option<Option<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EventListenerConfigDto {
    pub enabled: Option<bool>,
    pub metadata_library_filter: Option<Vec<String>>,
    /// komf ≤0.12 旧字段名（`metadataLibraryFilter` 旧名）。GET 输出同值；PATCH 接收时映射。
    pub libraries: Option<Vec<String>>,
    pub metadata_series_exclude_filter: Option<Vec<String>>,
    /// Kotlin 更新请求字段名 `metadataExcludeSeriesFilter`（GET 输出仍用 `metadataSeriesExcludeFilter`）。
    /// 仅 PATCH 接收，GET 不输出（skip_serializing）。
    #[serde(skip_serializing)]
    pub metadata_exclude_series_filter: Option<Vec<String>>,
    pub notifications_library_filter: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MetadataProvidersConfigDto {
    /// `Option<Option<T>>` 三态（对应 Kotlin PatchValue）：缺省=保持；null=清空；有值=设置。
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub mal_client_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub comic_vine_client_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub comic_vine_search_limit: Option<Option<i32>>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub comic_vine_issue_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub comic_vine_id_format: Option<Option<String>>,
    /// Bangumi API Token（Rust 扩展，Kotlin 无顶层字段）。三态：缺省=保持；null=清空；有值=设置。
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub bangumi_token: Option<Option<String>>,
    /// Kotlin `patch.nameMatchingMode.getOrNull()`：二态（Some=设置、缺省/None=保持，无清空语义）。
    pub name_matching_mode: Option<KomfNameMatchingMode>,
    pub default_providers: Option<ProvidersConfigDto>,
    /// PATCH：value 为 null 时删除该库配置（对应 Kotlin `Map<String, ProvidersConfigUpdateRequest?>`）。
    pub library_providers: Option<std::collections::HashMap<String, Option<ProvidersConfigDto>>>,
    pub manga_baka_database: Option<MangaBakaDatabaseDto>,
    pub book_walker_download_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaBakaDatabaseDto {
    pub download_timestamp: String,
    pub checksum: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProvidersConfigDto {
    pub manga_baka: Option<MangaBakaConfigDto>,
    pub book_walker: Option<ProviderConfigDto>,
    pub manga_dex: Option<MangaDexConfigDto>,
    pub manga_updates: Option<ProviderConfigDto>,
    pub ani_list: Option<AniListConfigDto>,
    pub mal: Option<ProviderConfigDto>,
    pub comic_vine: Option<ProviderConfigDto>,
    pub nautiljon: Option<ProviderConfigDto>,
    pub yen_press: Option<ProviderConfigDto>,
    pub kodansha: Option<ProviderConfigDto>,
    pub viz: Option<ProviderConfigDto>,
    pub bangumi: Option<BangumiConfigDto>,
    pub hentag: Option<ProviderConfigDto>,
    pub webtoons: Option<ProviderConfigDto>,
    pub e_hentai: Option<EHentaiConfigDto>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderConfigDto {
    pub priority: Option<i32>,
    pub enabled: Option<bool>,
    pub series_metadata: Option<SeriesMetadataConfigDto>,
    pub book_metadata: Option<BookMetadataConfigDto>,
    /// `Option<Option<T>>` 三态（对应 Kotlin PatchValue）：缺省=保持；null=清空；有值=设置。
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub name_matching_mode: Option<Option<KomfNameMatchingMode>>,
    pub media_type: Option<KomfMediaType>,
    pub author_roles: Option<Vec<KomfAuthorRole>>,
    pub artist_roles: Option<Vec<KomfAuthorRole>>,
    pub tag_whitelist: Option<Vec<String>>,
    pub tag_whitelist_file: Option<String>,
}

/// 对应 Kotlin `EHentaiConfigDto`（ProviderConf 独立实现，含 preferredLanguages）。
/// `tag_whitelist` 为脚本兼容字段（脚本对每个 provider 读取/回写，核心配置无对应项）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EHentaiConfigDto {
    pub priority: Option<i32>,
    pub enabled: Option<bool>,
    pub series_metadata: Option<SeriesMetadataConfigDto>,
    pub book_metadata: Option<BookMetadataConfigDto>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub name_matching_mode: Option<Option<KomfNameMatchingMode>>,
    pub media_type: Option<KomfMediaType>,
    pub author_roles: Option<Vec<KomfAuthorRole>>,
    pub artist_roles: Option<Vec<KomfAuthorRole>>,
    pub preferred_languages: Option<Vec<String>>,
    /// 自动匹配仅 gid 匹配（Rust 扩展）：true 时 match 只做 gid 精准搜索，无 gid / gid
    /// 无结果都跳过；links 匹配不受影响。false/未配置 → gid 优先、失败回落普通搜索。
    #[serde(default)]
    pub gid_only_match: Option<bool>,
    #[serde(default)]
    pub tag_whitelist: Option<Vec<String>>,
    pub title_priority: Option<String>,
    pub translator_keywords: Option<Vec<String>>,
    pub male_only_tags_file: Option<String>,
    pub title_template: Option<String>,
    /// EhTagTranslation 标签翻译（hentai-assistant 对齐）：false/未配置 → 不翻译；
    /// url 指定翻译库下载地址（缺省官方 release；应用内部定期更新，缓存 workDir/ehentai/db.text.json）。
    #[serde(default)]
    pub tag_translation_enabled: Option<bool>,
    #[serde(default)]
    pub tag_translation_url: Option<String>,
    pub search_domain: Option<String>,
    pub ipb_member_id: Option<String>,
    pub ipb_pass_hash: Option<String>,
    /// e-hentai-db 离线数据源（Rust 扩展）：与 bangumi archive 同构。
    pub archive: Option<EHentaiArchiveConfigDto>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EHentaiArchiveConfigDto {
    pub enabled: Option<bool>,
    pub url: Option<String>,
    pub db_file: Option<String>,
    pub update_interval_hours: Option<u64>,
    pub idle_release_secs: Option<u64>,
    pub search_category_filter: Option<Vec<String>>,
    pub search_uploader_filter: Option<Vec<String>>,
}

/// Bangumi 配置 DTO：通用 ProviderConfigDto 字段 + archive 离线数据源段。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BangumiConfigDto {
    pub priority: Option<i32>,
    pub enabled: Option<bool>,
    pub series_metadata: Option<SeriesMetadataConfigDto>,
    pub book_metadata: Option<BookMetadataConfigDto>,
    /// `Option<Option<T>>` 三态（对应 Kotlin PatchValue）：缺省=保持；null=清空；有值=设置。
    /// 注：不用 `#[serde(flatten)]` 展开 ProviderConfigDto —— flatten 下 `Option<Option<T>>`
    /// 的 JSON null 会反序列化失败（serde 限制），显式展开后三态 null 正常。
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub name_matching_mode: Option<Option<KomfNameMatchingMode>>,
    pub media_type: Option<KomfMediaType>,
    pub author_roles: Option<Vec<KomfAuthorRole>>,
    pub artist_roles: Option<Vec<KomfAuthorRole>>,
    pub tag_whitelist: Option<Vec<String>>,
    pub tag_whitelist_file: Option<String>,
    pub archive: Option<BangumiArchiveConfigDto>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BangumiArchiveConfigDto {
    pub enabled: Option<bool>,
    pub dir: Option<String>,
    pub update_interval_hours: Option<u64>,
    pub idle_release_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AniListConfigDto {
    pub priority: Option<i32>,
    pub enabled: Option<bool>,
    pub series_metadata: Option<SeriesMetadataConfigDto>,
    /// komf ≤0.12 兼容：官方前端/脚本对所有 provider 读取 bookMetadata（即使 UI 不编辑）。
    /// Kotlin main 该字段为 null（GET 不输出）→ 脚本保存时 `updated.bookMetadata.title` 崩溃。
    /// GET 输出默认结构；PATCH 接收后忽略（core 无对应位置）。
    pub book_metadata: Option<BookMetadataConfigDto>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub name_matching_mode: Option<Option<KomfNameMatchingMode>>,
    pub media_type: Option<KomfMediaType>,
    pub author_roles: Option<Vec<KomfAuthorRole>>,
    pub artist_roles: Option<Vec<KomfAuthorRole>>,
    pub tags_score_threshold: Option<i32>,
    pub tags_size_limit: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MangaDexConfigDto {
    pub priority: Option<i32>,
    pub enabled: Option<bool>,
    pub series_metadata: Option<SeriesMetadataConfigDto>,
    pub book_metadata: Option<BookMetadataConfigDto>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub name_matching_mode: Option<Option<KomfNameMatchingMode>>,
    pub media_type: Option<KomfMediaType>,
    pub author_roles: Option<Vec<KomfAuthorRole>>,
    pub artist_roles: Option<Vec<KomfAuthorRole>>,
    pub cover_languages: Option<Vec<String>>,
    pub links: Option<Vec<MangaDexLink>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MangaBakaConfigDto {
    pub priority: Option<i32>,
    pub enabled: Option<bool>,
    pub series_metadata: Option<SeriesMetadataConfigDto>,
    /// komf ≤0.12 兼容：同 `AniListConfigDto.book_metadata`（脚本对每个 provider 读取）。
    pub book_metadata: Option<BookMetadataConfigDto>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub name_matching_mode: Option<Option<KomfNameMatchingMode>>,
    pub media_type: Option<KomfMediaType>,
    pub author_roles: Option<Vec<KomfAuthorRole>>,
    pub artist_roles: Option<Vec<KomfAuthorRole>>,
    pub mode: Option<MangaBakaMode>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SeriesMetadataConfigDto {
    pub status: Option<bool>,
    pub title: Option<bool>,
    pub summary: Option<bool>,
    pub publisher: Option<bool>,
    pub reading_direction: Option<bool>,
    pub age_rating: Option<bool>,
    pub language: Option<bool>,
    pub genres: Option<bool>,
    pub tags: Option<bool>,
    pub total_book_count: Option<bool>,
    pub authors: Option<bool>,
    pub release_date: Option<bool>,
    pub thumbnail: Option<bool>,
    pub links: Option<bool>,
    pub books: Option<bool>,
    pub score: Option<bool>,
    pub use_original_publisher: Option<bool>,
    pub original_publisher_tag_name: Option<String>,
    pub english_publisher_tag_name: Option<String>,
    pub french_publisher_tag_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BookMetadataConfigDto {
    pub title: Option<bool>,
    pub summary: Option<bool>,
    pub number: Option<bool>,
    pub number_sort: Option<bool>,
    pub release_date: Option<bool>,
    pub authors: Option<bool>,
    pub tags: Option<bool>,
    pub isbn: Option<bool>,
    pub links: Option<bool>,
    pub thumbnail: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NotificationConfigDto {
    pub apprise: Option<AppriseConfigDto>,
    pub discord: Option<DiscordConfigDto>,
}

/// 三态反序列化（对应 Kotlin `PatchValue<T>` 的 None=清空 / Some=设置 / Unset=保持）：
/// - 字段缺失 → `None`（保持）
/// - JSON `null` → `Some(None)`（显式清空）
/// - JSON 值 → `Some(Some(v))`（设置）
///
/// 注意：serde 原生 `Option<Option<T>>` 会把 JSON `null` 解析为外层 `None`（=缺省），
/// 无法区分「缺省」与「显式清空」，必须走 `deserialize_option` 的自定义 Visitor。
fn deserialize_tri_state<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    use serde::de::Visitor;
    struct TriVisitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: serde::de::DeserializeOwned> Visitor<'de> for TriVisitor<T> {
        type Value = Option<Option<T>>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a tri-state value (absent=keep, null=clear, value=set)")
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(Some(None)) // null → 清空
        }
        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            T::deserialize(deserializer).map(|v| Some(Some(v))) // 值 → 设置
        }
    }
    deserializer.deserialize_option(TriVisitor(std::marker::PhantomData))
}

/// Discord/Apprise 通知地址列表 —— 对齐 Kotlin `PatchValue<Map<Int, String?>>` 的
/// **索引合并**语义：PATCH 发送 `{"0":"url1","1":null,...}`（或数组）时按索引合并，
/// 同索引覆盖、新索引追加、null 删除；GET 输出仍为数组（按 key 升序取 value）。
#[derive(Debug, Clone, Default)]
pub struct IndexedUrlList(pub std::collections::BTreeMap<usize, Option<String>>);

impl IndexedUrlList {
    pub fn from_vec(values: Vec<String>) -> Self {
        IndexedUrlList(values.into_iter().enumerate().map(|(i, v)| (i, Some(v))).collect())
    }

    /// 应用 Kotlin `old + patch → values.filterNotNull()`：同索引覆盖、新索引追加、null 删除。
    pub fn apply_merge(&self, base: &[String]) -> Vec<String> {
        let mut merged: std::collections::BTreeMap<usize, Option<String>> =
            base.iter().enumerate().map(|(i, v)| (i, Some(v.clone()))).collect();
        for (i, v) in &self.0 {
            merged.insert(*i, v.clone());
        }
        merged.into_iter().filter_map(|(_, v)| v).collect()
    }
}

fn deserialize_indexed_urls<'de, D>(deserializer: D) -> Result<Option<IndexedUrlList>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum UrlsInput {
        List(Vec<String>),
        Map(std::collections::BTreeMap<String, Option<String>>),
        Null,
    }
    Ok(match UrlsInput::deserialize(deserializer)? {
        UrlsInput::List(list) => Some(IndexedUrlList(
            list.into_iter().enumerate().map(|(i, v)| (i, Some(v))).collect(),
        )),
        UrlsInput::Map(map) => Some(IndexedUrlList(
            map.into_iter()
                .filter_map(|(k, v)| k.parse::<usize>().ok().map(|i| (i, v)))
                .collect(),
        )),
        UrlsInput::Null => None,
    })
}

fn serialize_indexed_urls<S>(value: &Option<IndexedUrlList>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    match value {
        Some(list) => {
            let mut seq = serializer.serialize_seq(Some(list.0.len()))?;
            for (_, v) in &list.0 {
                if let Some(v) = v {
                    seq.serialize_element(v)?;
                }
            }
            seq.end()
        }
        None => serializer.serialize_none(),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiscordConfigDto {
    #[serde(default, deserialize_with = "deserialize_indexed_urls", serialize_with = "serialize_indexed_urls")]
    pub webhooks: Option<IndexedUrlList>,
    pub series_cover: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppriseConfigDto {
    #[serde(default, deserialize_with = "deserialize_indexed_urls", serialize_with = "serialize_indexed_urls")]
    pub urls: Option<IndexedUrlList>,
    pub series_cover: Option<bool>,
}

/// 对应 `KomfConfigUpdateRequest.kt`（配置更新请求）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfConfigUpdateRequest {
    #[serde(default)]
    pub komga: Option<KomgaConfigDto>,
    #[serde(default)]
    pub kavita: Option<KavitaConfigDto>,
    #[serde(default)]
    pub stump: Option<StumpConfigDto>,
    #[serde(default)]
    pub notifications: Option<NotificationConfigDto>,
    #[serde(default)]
    pub metadata_providers: Option<MetadataProvidersConfigDto>,
}

/// 对应 `DownloadProgress.kt`（sealed interface，kotlinx 多态序列化 `type` 判别字段）。
///
/// jsonl 流中的每一行即为一个事件 JSON：`{"type":"ProgressEvent",...}` / `{"type":"FinishedEvent"}`
/// / `{"type":"ErrorEvent","message":...}`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum DownloadProgress {
    ProgressEvent {
        total: i64,
        completed: i64,
        #[serde(default)]
        info: Option<String>,
    },
    FinishedEvent,
    ErrorEvent {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_webhooks_accepts_array_and_index_object() {
        // Kotlin main 契约：数组
        let dto: DiscordConfigDto =
            serde_json::from_str(r#"{"webhooks":["https://a","https://b"],"seriesCover":true}"#).unwrap();
        let list = dto.webhooks.unwrap();
        assert_eq!(
            list.0,
            vec![
                (0usize, Some("https://a".to_string())),
                (1, Some("https://b".to_string()))
            ]
            .into_iter()
            .collect()
        );
        // komf userscript v0.12.2 PATCH 形态：索引对象（Object.fromEntries），按数字键升序取 value
        let dto: DiscordConfigDto = serde_json::from_str(r#"{"webhooks":{"0":"https://a","1":"https://b"}}"#).unwrap();
        let list = dto.webhooks.unwrap();
        assert_eq!(
            list.0,
            vec![
                (0usize, Some("https://a".to_string())),
                (1, Some("https://b".to_string()))
            ]
            .into_iter()
            .collect()
        );
        // 乱序键也按 key 排序（脚本索引语义）
        let dto: DiscordConfigDto = serde_json::from_str(r#"{"webhooks":{"1":"b","0":"a"}}"#).unwrap();
        let list = dto.webhooks.unwrap();
        assert_eq!(
            list.0,
            vec![(0usize, Some("a".to_string())), (1, Some("b".to_string()))]
                .into_iter()
                .collect()
        );
        // null → None
        let dto: DiscordConfigDto = serde_json::from_str(r#"{"webhooks":null}"#).unwrap();
        assert!(dto.webhooks.is_none());
        // 缺省 → None（PATCH 增量语义）
        let dto: DiscordConfigDto = serde_json::from_str(r#"{}"#).unwrap();
        assert!(dto.webhooks.is_none());
        // 索引合并（Kotlin `old + patch → values.filterNotNull()`）：同索引覆盖、追加、null 删除
        let patch: DiscordConfigDto =
            serde_json::from_str(r#"{"webhooks":{"0":"new","2":"add","1":null}}"#).unwrap();
        let base = vec!["old0".to_string(), "old1".to_string(), "old2".to_string()];
        let merged = patch.webhooks.unwrap().apply_merge(&base);
        assert_eq!(merged, vec!["new".to_string(), "add".to_string()]);
    }

    /// Stump DTO 凭据契约：GET 不输出 password/apiKey；PATCH 可接收。
    #[test]
    fn stump_config_dto_credential_contract() {
        let dto = StumpConfigDto {
            base_uri: Some("http://127.0.0.1:10801".into()),
            username: Some("alice".into()),
            password: Some("s3cret".into()),
            api_key: Some("key-123".into()),
            event_listener: None,
            metadata_update: None,
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("\"baseUri\""), "GET must output baseUri, got {json}");
        assert!(!json.contains("password"));
        assert!(!json.contains("apiKey"));
        assert!(!json.contains("s3cret"));
        assert!(!json.contains("key-123"));

        // PATCH 形态：接收凭据
        let parsed: StumpConfigDto = serde_json::from_str(
            r#"{"baseUri":"http://127.0.0.1:10801","username":"alice","password":"s3cret","apiKey":"key-123"}"#,
        )
        .unwrap();
        assert_eq!(parsed.base_uri.as_deref(), Some("http://127.0.0.1:10801"));
        assert_eq!(parsed.username.as_deref(), Some("alice"));
        assert_eq!(parsed.password.as_deref(), Some("s3cret"));
        assert_eq!(parsed.api_key.as_deref(), Some("key-123"));

        // 缺省字段 → None（PATCH 增量语义）
        let empty: StumpConfigDto = serde_json::from_str("{}").unwrap();
        assert!(empty.base_uri.is_none());
        assert!(empty.api_key.is_none());
    }
}
