//! Provider 配置 —— 对应 `MetadataProvidersConfig.kt`。
use crate::model::{AuthorRole, MediaType};
use crate::util::NameSimilarityMatcher;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataProvidersConfig {
    #[serde(default)]
    pub mal_client_id: Option<String>,
    #[serde(default)]
    pub comic_vine_api_key: Option<String>,
    #[serde(default)]
    pub comic_vine_search_limit: Option<i32>,
    #[serde(default)]
    pub comic_vine_issue_name: Option<String>,
    #[serde(default)]
    pub comic_vine_id_format: Option<String>,
    #[serde(default)]
    pub bangumi_token: Option<String>,
    #[serde(default)]
    pub name_matching_mode: NameSimilarityMatcher,
    #[serde(default)]
    pub default_providers: ProvidersConfig,
    #[serde(default)]
    pub library_providers: std::collections::HashMap<String, ProvidersConfig>,
}

impl Default for MetadataProvidersConfig {
    fn default() -> Self {
        Self {
            mal_client_id: None,
            comic_vine_api_key: None,
            comic_vine_search_limit: None,
            comic_vine_issue_name: None,
            comic_vine_id_format: None,
            bangumi_token: None,
            name_matching_mode: NameSimilarityMatcher::default(),
            default_providers: ProvidersConfig::default(),
            library_providers: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvidersConfig {
    #[serde(default)]
    pub manga_baka: MangaBakaConfig,
    #[serde(default)]
    pub book_walker: ProviderConfig,
    #[serde(default)]
    pub manga_dex: MangaDexConfig,
    #[serde(default)]
    pub manga_updates: ProviderConfig,
    #[serde(default)]
    pub ani_list: AniListConfig,
    #[serde(default)]
    pub mal: ProviderConfig,
    #[serde(default)]
    pub comic_vine: ProviderConfig,
    #[serde(default)]
    pub yen_press: ProviderConfig,
    #[serde(default)]
    pub viz: ProviderConfig,
    #[serde(default)]
    pub bangumi: BangumiConfig,
    #[serde(default)]
    pub webtoons: ProviderConfig,
    #[serde(default)]
    pub e_hentai: EHentaiConfig,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            manga_baka: MangaBakaConfig::default(),
            book_walker: ProviderConfig::default(),
            manga_dex: MangaDexConfig::default(),
            manga_updates: ProviderConfig {
                priority: 10,
                enabled: true,
                ..Default::default()
            },
            ani_list: AniListConfig {
                priority: 40,
                ..Default::default()
            },
            mal: ProviderConfig {
                priority: 20,
                ..Default::default()
            },
            comic_vine: ProviderConfig {
                priority: 110,
                ..Default::default()
            },
            yen_press: ProviderConfig {
                priority: 50,
                ..Default::default()
            },
            viz: ProviderConfig {
                priority: 70,
                ..Default::default()
            },
            bangumi: BangumiConfig {
                provider: ProviderConfig {
                    priority: 100,
                    ..Default::default()
                },
                archive: BangumiArchiveConfig::default(),
            },
            webtoons: ProviderConfig {
                priority: 130,
                ..Default::default()
            },
            e_hentai: EHentaiConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub series_metadata: SeriesMetadataConfig,
    #[serde(default)]
    pub book_metadata: BookMetadataConfig,
    #[serde(default)]
    pub name_matching_mode: Option<NameSimilarityMatcher>,
    #[serde(default)]
    pub media_type: MediaType,
    #[serde(default = "default_writer_roles")]
    pub author_roles: Vec<AuthorRole>,
    #[serde(default = "default_artist_roles")]
    pub artist_roles: Vec<AuthorRole>,
    #[serde(default, deserialize_with = "deserialize_tag_whitelist")]
    pub tag_whitelist: Vec<String>,
    /// bangumi 标签白名单 json 文件路径；缺省用内置资源（bangumi_tag_whitelist.json）。
    #[serde(default)]
    pub tag_whitelist_file: Option<String>,
}

fn deserialize_tag_whitelist<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(s) => Ok(s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect()),
        OneOrMany::Many(v) => Ok(v),
    }
}

/// Bangumi 配置：平铺继承 ProviderConfig 通用字段（兼容既有 yaml），
/// 另含 archive 离线数据源配置段。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiConfig {
    #[serde(flatten)]
    pub provider: ProviderConfig,
    #[serde(default)]
    pub archive: BangumiArchiveConfig,
}

impl Default for BangumiConfig {
    fn default() -> Self {
        Self {
            provider: ProviderConfig::default(),
            archive: BangumiArchiveConfig::default(),
        }
    }
}

/// bangumi/Archive 离线数据源配置（BangumiKomga bangumi_archive 移植）：
/// enabled 时后台下载 Archive（约 418MB zip）→ 构建 SQLite 索引（FTS5 trigram）
/// + mmap 数据源；搜索/元数据优先离线，未就绪或未命中回退在线 API。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiArchiveConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 数据目录（archive_index.db + subject*.jsonlines）；缺省 workDir/bangumi-archive。
    #[serde(default)]
    pub dir: Option<String>,
    /// 更新间隔（小时）；0 = 不检查更新（仅首次构建）。
    #[serde(default = "default_archive_update_interval")]
    pub update_interval_hours: u64,
    /// 空闲释放 mmap 热页（秒）：距上次查询超过该时长后对 subject.jsonlines 的
    /// mmap 执行 MADV_DONTNEED 释放页缓存（下次查询按需重读），降低常驻内存。
    /// 默认 60；0 = 禁用。仅 Linux 生效（unix madvise）。
    #[serde(default = "default_archive_idle_release")]
    pub idle_release_secs: Option<u64>,
}

fn default_archive_update_interval() -> u64 {
    168
}

fn default_archive_idle_release() -> Option<u64> {
    Some(60)
}

impl Default for BangumiArchiveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: None,
            update_interval_hours: default_archive_update_interval(),
            idle_release_secs: default_archive_idle_release(),
        }
    }
}

fn default_priority() -> i32 {
    10
}

fn default_writer_roles() -> Vec<AuthorRole> {
    vec![AuthorRole::Writer]
}

fn default_artist_roles() -> Vec<AuthorRole> {
    vec![
        AuthorRole::Penciller,
        AuthorRole::Inker,
        AuthorRole::Colorist,
        AuthorRole::Letterer,
        AuthorRole::Cover,
    ]
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            priority: default_priority(),
            enabled: false,
            series_metadata: SeriesMetadataConfig::default(),
            book_metadata: BookMetadataConfig::default(),
            name_matching_mode: None,
            media_type: MediaType::Manga,
            author_roles: default_writer_roles(),
            artist_roles: default_artist_roles(),
            tag_whitelist: Vec::new(),
            tag_whitelist_file: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EHentaiConfig {
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub series_metadata: SeriesMetadataConfig,
    #[serde(default)]
    pub book_metadata: BookMetadataConfig,
    #[serde(default)]
    pub name_matching_mode: Option<NameSimilarityMatcher>,
    #[serde(default)]
    pub media_type: MediaType,
    #[serde(default = "default_ehentai_languages")]
    pub preferred_languages: Vec<String>,
    #[serde(default = "default_ehentai_writer_roles")]
    pub author_roles: Vec<AuthorRole>,
    #[serde(default = "default_artist_roles")]
    pub artist_roles: Vec<AuthorRole>,
    /// 标题优先："jpn" → title_jpn 优先（默认）；"title" → 英文 title 优先。
    /// 作用于搜索结果显示与元数据标题。
    #[serde(default = "default_ehentai_title_priority")]
    pub title_priority: String,
    /// 汉化组关键词（自定义；空时用内置 9 个）。用于 forced-language 检测与 Translator 作者解析。
    #[serde(default)]
    pub translator_keywords: Vec<String>,
    /// male-only 标签 json 文件路径（一致的 {"content": [...]} 格式）；
    /// 缺省用 workDir/ehentai/male_only_taglist.json，文件缺失时从 ehwiki 下载保存。
    #[serde(default)]
    pub male_only_tags_file: Option<String>,
    /// 元数据标题模板（ComicInfo title 模板风格，空=不启用走 titlePriority 逻辑）。
    /// 支持 `{{var}}` 变量替换与 `{% if var %}...{% endif %}` 条件块（变量：title/title_jpn/translator/writer/penciller）。
    /// 示例：`{{title}}{% if translator %} [{{ translator }}]{% endif %}`
    #[serde(default)]
    pub title_template: String,
    /// EhTagTranslation 标签翻译（hentai-assistant ehtranslator.py 对齐）：
    /// false（默认）→ 不翻译；true → 由应用内部统一管理翻译库更新
    /// （缓存到 workDir/ehentai/db.text.json，启动加载 + 每 24h 检查下载）并在标签处理时应用翻译。
    #[serde(default)]
    pub tag_translation_enabled: bool,
    /// 标签翻译库下载 URL（缺省 EhTagTranslation 官方 release：
    /// https://github.com/EhTagTranslation/Database/releases/latest/download/db.text.json）。
    #[serde(default)]
    pub tag_translation_url: Option<String>,
    /// 搜索域名："e-hentai"（默认）| "exhentai"（仅用于搜索；需 ipb cookie）。
    #[serde(default = "default_ehentai_search_domain")]
    pub search_domain: String,
    /// E-Hentai/ExHentai 登录 cookie（浏览器登录后从 Cookie 中获取；exhentai 必需）。
    #[serde(default)]
    pub ipb_member_id: Option<String>,
    #[serde(default)]
    pub ipb_pass_hash: Option<String>,
}

fn default_ehentai_search_domain() -> String {
    "e-hentai".to_string()
}

fn default_ehentai_title_priority() -> String {
    "jpn".to_string()
}

fn default_ehentai_languages() -> Vec<String> {
    vec!["en".to_string(), "ja".to_string()]
}

fn default_ehentai_writer_roles() -> Vec<AuthorRole> {
    vec![AuthorRole::Writer]
}

impl Default for EHentaiConfig {
    fn default() -> Self {
        Self {
            priority: default_priority(),
            enabled: false,
            series_metadata: SeriesMetadataConfig::default(),
            book_metadata: BookMetadataConfig::default(),
            name_matching_mode: None,
            media_type: MediaType::Manga,
            preferred_languages: default_ehentai_languages(),
            author_roles: default_ehentai_writer_roles(),
            artist_roles: default_artist_roles(),
            title_priority: default_ehentai_title_priority(),
            translator_keywords: Vec::new(),
            male_only_tags_file: None,
            title_template: String::new(),
            tag_translation_enabled: false,
            tag_translation_url: None,
            search_domain: default_ehentai_search_domain(),
            ipb_member_id: None,
            ipb_pass_hash: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaBakaConfig {
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub series_metadata: SeriesMetadataConfig,
    #[serde(default)]
    pub name_matching_mode: Option<NameSimilarityMatcher>,
    #[serde(default)]
    pub media_type: MediaType,
    #[serde(default = "default_writer_roles")]
    pub author_roles: Vec<AuthorRole>,
    #[serde(default = "default_artist_roles")]
    pub artist_roles: Vec<AuthorRole>,
    #[serde(default)]
    pub mode: MangaBakaMode,
}

impl Default for MangaBakaConfig {
    fn default() -> Self {
        Self {
            priority: default_priority(),
            enabled: false,
            series_metadata: SeriesMetadataConfig::default(),
            name_matching_mode: None,
            media_type: MediaType::Manga,
            author_roles: default_writer_roles(),
            artist_roles: default_artist_roles(),
            mode: MangaBakaMode::Api,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaBakaMode {
    Api,
    Database,
}

impl Default for MangaBakaMode {
    fn default() -> Self {
        Self::Api
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AniListConfig {
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub series_metadata: SeriesMetadataConfig,
    #[serde(default)]
    pub name_matching_mode: Option<NameSimilarityMatcher>,
    #[serde(default)]
    pub media_type: MediaType,
    #[serde(default = "default_writer_roles")]
    pub author_roles: Vec<AuthorRole>,
    #[serde(default = "default_artist_roles")]
    pub artist_roles: Vec<AuthorRole>,
    #[serde(default = "default_tags_score_threshold")]
    pub tags_score_threshold: i32,
    #[serde(default = "default_tags_size_limit")]
    pub tags_size_limit: i32,
}

fn default_tags_score_threshold() -> i32 {
    60
}

fn default_tags_size_limit() -> i32 {
    15
}

impl Default for AniListConfig {
    fn default() -> Self {
        Self {
            priority: default_priority(),
            enabled: false,
            series_metadata: SeriesMetadataConfig::default(),
            name_matching_mode: None,
            media_type: MediaType::Manga,
            author_roles: default_writer_roles(),
            artist_roles: default_artist_roles(),
            tags_score_threshold: default_tags_score_threshold(),
            tags_size_limit: default_tags_size_limit(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaDexConfig {
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub series_metadata: SeriesMetadataConfig,
    #[serde(default)]
    pub book_metadata: BookMetadataConfig,
    #[serde(default)]
    pub name_matching_mode: Option<NameSimilarityMatcher>,
    #[serde(default)]
    pub media_type: MediaType,
    #[serde(default = "default_writer_roles")]
    pub author_roles: Vec<AuthorRole>,
    #[serde(default = "default_artist_roles")]
    pub artist_roles: Vec<AuthorRole>,
    #[serde(default = "default_cover_languages")]
    pub cover_languages: Vec<String>,
    #[serde(default)]
    pub links: Vec<MangaDexLink>,
}

fn default_cover_languages() -> Vec<String> {
    vec!["en".to_string(), "ja".to_string()]
}

impl Default for MangaDexConfig {
    fn default() -> Self {
        Self {
            priority: default_priority(),
            enabled: false,
            series_metadata: SeriesMetadataConfig::default(),
            book_metadata: BookMetadataConfig::default(),
            name_matching_mode: None,
            media_type: MediaType::Manga,
            author_roles: default_writer_roles(),
            artist_roles: default_artist_roles(),
            cover_languages: default_cover_languages(),
            links: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaDexLink {
    MangaDex,
    Anilist,
    AnimePlanet,
    BookwalkerJp,
    MangaUpdates,
    NovelUpdates,
    Kitsu,
    Amazon,
    EbookJapan,
    MyAnimeList,
    CdJapan,
    Raw,
    EnglishTl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesMetadataConfig {
    #[serde(default = "default_true")]
    pub status: bool,
    #[serde(default = "default_true")]
    pub title: bool,
    #[serde(default = "default_true")]
    pub title_sort: bool,
    #[serde(default = "default_true")]
    pub summary: bool,
    #[serde(default = "default_true")]
    pub publisher: bool,
    #[serde(default = "default_true")]
    pub reading_direction: bool,
    #[serde(default = "default_true")]
    pub age_rating: bool,
    #[serde(default = "default_true")]
    pub language: bool,
    #[serde(default = "default_true")]
    pub genres: bool,
    #[serde(default = "default_true")]
    pub tags: bool,
    #[serde(default = "default_true")]
    pub total_book_count: bool,
    #[serde(default = "default_true")]
    pub authors: bool,
    #[serde(default = "default_true")]
    pub release_date: bool,
    #[serde(default = "default_true")]
    pub thumbnail: bool,
    #[serde(default = "default_true")]
    pub books: bool,
    #[serde(default = "default_true")]
    pub links: bool,
    #[serde(default = "default_true")]
    pub score: bool,
    #[serde(default)]
    pub use_original_publisher: bool,
}

fn default_true() -> bool {
    true
}

impl Default for SeriesMetadataConfig {
    fn default() -> Self {
        Self {
            status: true,
            title: true,
            title_sort: true,
            summary: true,
            publisher: true,
            reading_direction: true,
            age_rating: true,
            language: true,
            genres: true,
            tags: true,
            total_book_count: true,
            authors: true,
            release_date: true,
            thumbnail: true,
            books: true,
            links: true,
            score: true,
            use_original_publisher: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookMetadataConfig {
    #[serde(default = "default_true")]
    pub title: bool,
    #[serde(default = "default_true")]
    pub summary: bool,
    #[serde(default = "default_true")]
    pub number: bool,
    #[serde(default = "default_true")]
    pub number_sort: bool,
    #[serde(default = "default_true")]
    pub release_date: bool,
    #[serde(default = "default_true")]
    pub authors: bool,
    #[serde(default = "default_true")]
    pub tags: bool,
    #[serde(default = "default_true")]
    pub isbn: bool,
    #[serde(default = "default_true")]
    pub links: bool,
    #[serde(default = "default_true")]
    pub thumbnail: bool,
}

impl Default for BookMetadataConfig {
    fn default() -> Self {
        Self {
            title: true,
            summary: true,
            number: true,
            number_sort: true,
            release_date: true,
            authors: true,
            tags: true,
            isbn: true,
            links: true,
            thumbnail: true,
        }
    }
}
