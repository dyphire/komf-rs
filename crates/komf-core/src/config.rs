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
    pub book_walker: BookWalkerConfig,
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
    pub webtoons: WebtoonsConfig,
    #[serde(default)]
    pub e_hentai: EHentaiConfig,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            manga_baka: MangaBakaConfig::default(),
            book_walker: BookWalkerConfig::default(),
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
            webtoons: WebtoonsConfig {
                provider: ProviderConfig {
                    priority: 130,
                    ..Default::default()
                },
                language: WebtoonsLanguage::default(),
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

/// Webtoons 配置：通用 ProviderConfig 字段 + 语言站台（`language`，缺省 en）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebtoonsConfig {
    #[serde(flatten)]
    pub provider: ProviderConfig,
    #[serde(default)]
    pub language: WebtoonsLanguage,
}

/// Webtoons.com 的语言站台。序列化值即网址路径首段（如 `zh-hant`）；
/// 反序列化（YAML）与 API PATCH 共用 [`WebtoonsLanguage::parse`]，接受的写法一致。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize)]
pub enum WebtoonsLanguage {
    #[default]
    #[serde(rename = "en")]
    En,
    #[serde(rename = "zh-hant")]
    ZhHant,
    #[serde(rename = "th")]
    Th,
    #[serde(rename = "id")]
    Id,
    #[serde(rename = "es")]
    Es,
    #[serde(rename = "fr")]
    Fr,
    #[serde(rename = "de")]
    De,
}

impl<'de> Deserialize<'de> for WebtoonsLanguage {
    /// `null` 视同缺省（en）；未知语言码报错并列出可选值。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match Option::<String>::deserialize(deserializer)? {
            None => Ok(Self::default()),
            Some(code) => Self::parse(&code).ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "unknown webtoons language '{code}', expected one of: {}",
                    Self::supported_codes()
                ))
            }),
        }
    }
}

impl WebtoonsLanguage {
    pub const ALL: [WebtoonsLanguage; 7] = [
        Self::En,
        Self::ZhHant,
        Self::Th,
        Self::Id,
        Self::Es,
        Self::Fr,
        Self::De,
    ];

    /// 网址路径首段：`https://www.webtoons.com/{path_code}/...`
    pub fn path_code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhHant => "zh-hant",
            Self::Th => "th",
            Self::Id => "id",
            Self::Es => "es",
            Self::Fr => "fr",
            Self::De => "de",
        }
    }

    /// `locale` cookie：移动版搜索 API 依它决定返回哪个语言站台的作品。
    pub fn cookie_locale(self) -> &'static str {
        match self {
            Self::ZhHant => "zh_TW",
            other => other.path_code(),
        }
    }

    /// 写入 metadata 的 BCP 47 语言标签。
    pub fn language_tag(self) -> &'static str {
        match self {
            Self::ZhHant => "zh-Hant",
            other => other.path_code(),
        }
    }

    /// 唯一的语言码解析入口（YAML / API / 网址路径共用）：大小写不敏感，`_` 视同 `-`；
    /// 除规范路径码外，zh-hant 另接受 `zh-tw`（`zh_TW` 为站点 locale 写法）。
    pub fn parse(code: &str) -> Option<Self> {
        let normalized = code.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "zh-tw" => Some(Self::ZhHant),
            other => Self::ALL.into_iter().find(|l| l.path_code() == other),
        }
    }

    /// 供错误信息使用的可选值列表（`en, zh-hant, ...`）。
    pub fn supported_codes() -> String {
        Self::ALL
            .iter()
            .map(|l| l.path_code())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// 从网址或路径（如 `/zh-hant/fantasy/xxx/list?title_no=1`）取出语言。
    pub fn from_url_path(url_or_path: &str) -> Option<Self> {
        let path = url_or_path
            .split_once("webtoons.com")
            .map(|(_, rest)| rest)
            .unwrap_or(url_or_path);
        let first = path.trim_start_matches('/').split(['/', '?']).next()?;
        Self::parse(first)
    }
}

/// Bangumi 配置：平铺继承 ProviderConfig 通用字段（兼容既有 yaml），
/// 另含 archive 离线数据源配置段。
/// 作者/出版社中文名（archive.staffChineseNames，缺省 false=日文原名）
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
/// enabled 时后台下载 Archive（约 418MB zip）→ 构建 SQLite 单库（FTS5 预分词
/// 分析链 + 主数据 json 列）；搜索/元数据优先离线，未就绪或未命中回退在线 API。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiArchiveConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 数据目录（archive_index.db）；缺省 workDir/bangumi-archive。
    #[serde(default)]
    pub dir: Option<String>,
    /// 更新间隔（小时）；0 = 不检查更新（仅首次构建）。
    #[serde(default = "default_archive_update_interval")]
    pub update_interval_hours: u64,
    /// SQLite 页缓存空闲释放阈值（秒）：距上次释放超过该时长即 `PRAGMA shrink_memory`
    /// 归还页缓存（v8 语义，替代 v7 对 mmap jsonlines 的 MADV_DONTNEED 释放）；
    /// 0 = 禁用。缺省 60。
    #[serde(default = "default_archive_idle_release")]
    pub idle_release_secs: Option<u64>,
    /// 作者/出版社中文名开关：true=离线 person 实体 name_cn 优先（作者/出版社写中文名）；
    /// false（缺省）=用日文原名。仅影响 authors/publisher 显示名
    #[serde(default)]
    pub staff_chinese_names: bool,
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
            staff_chinese_names: false,
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
    /// 自动匹配仅 gid 匹配：true 时 match 只做 gid 精准搜索——提取不到 gid 或
    /// gid 搜索无结果都跳过（不回落普通标题相似度搜索）；links 匹配（linksSkipEnabled/
    /// linksMatchEnabled）不受影响。false（默认）→ gid 优先、失败回落普通搜索。
    #[serde(default)]
    pub gid_only_match: bool,
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
    /// e-hentai-db 离线数据源（URenko nightly SQLite）：enabled 时启动时下载/解压并
    /// 用于 gid 精准查询（getBookOrThrow/gid 匹配/链接搜索离线优先，未命中回退在线）。
    #[serde(default)]
    pub archive: EHentaiArchiveConfig,
}

/// e-hentai-db 离线数据源配置：
/// 数据文件为 URenko fork 的 nightly SQLite dump（gallery/gid_tid/tag/torrent 表，
/// 与官方 gdata JSON 同字段），启动时应用内自动下载 zstd 并解压到本地，定期检查更新。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EHentaiArchiveConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 数据库下载 URL（缺省 URenko nightly release e-hentai.db.zstd）。
    #[serde(default)]
    pub url: Option<String>,
    /// 本地数据库文件路径（缺省 workDir/ehentai/e-hentai.db）。
    #[serde(default)]
    pub db_file: Option<String>,
    /// 更新检查间隔（小时）；0 = 仅启动时加载/下载一次，不自动更新。
    #[serde(default = "default_ehentai_archive_update_interval")]
    pub update_interval_hours: u64,
    /// 空闲释放 SQLite 页面缓存（秒）；0 = 禁用（默认 60，对齐 bangumi archive）。
    #[serde(default = "default_ehentai_archive_idle_release")]
    pub idle_release_secs: Option<u64>,
    /// 搜索结果/匹配候选的 category 白名单（如 ["Doujinshi"]）；空 = 不过滤。
    #[serde(default)]
    pub search_category_filter: Vec<String>,
    /// 搜索结果/匹配候选的 uploader 白名单（精确匹配）；空 = 不过滤。
    #[serde(default)]
    pub search_uploader_filter: Vec<String>,
}

fn default_ehentai_archive_update_interval() -> u64 {
    4
}

fn default_ehentai_archive_idle_release() -> Option<u64> {
    Some(60)
}

impl Default for EHentaiArchiveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: None,
            db_file: None,
            update_interval_hours: default_ehentai_archive_update_interval(),
            idle_release_secs: default_ehentai_archive_idle_release(),
            search_category_filter: Vec::new(),
            search_uploader_filter: Vec::new(),
        }
    }
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
            gid_only_match: false,
            title_priority: default_ehentai_title_priority(),
            translator_keywords: Vec::new(),
            male_only_tags_file: None,
            title_template: String::new(),
            tag_translation_enabled: false,
            tag_translation_url: None,
            search_domain: default_ehentai_search_domain(),
            ipb_member_id: None,
            ipb_pass_hash: None,
            archive: EHentaiArchiveConfig::default(),
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
    #[serde(default)]
    pub book_metadata: BookMetadataConfig,
    /// 书籍封面语言偏好（默认 en/ja，对齐 MangaDex coverLanguages）。
    #[serde(default = "default_cover_languages")]
    pub cover_languages: Vec<String>,
    /// 本地数据库定时更新间隔（小时）；0 = 仅手动下载（默认 24）。
    #[serde(default = "default_db_update_interval")]
    pub update_interval_hours: u64,
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
            book_metadata: BookMetadataConfig::default(),
            cover_languages: default_cover_languages(),
            update_interval_hours: default_db_update_interval(),
        }
    }
}

/// 数据库定时更新间隔默认值（小时）：24，对齐 ehentai archive 节奏（比 bangumi 7 天更频繁）。
fn default_db_update_interval() -> u64 {
    24
}

/// BookWalker 配置（独立结构，含本地数据库定时更新间隔）。
/// 标签白名单为 Bangumi 独有特性，BookWalker 不提供（曾保留 tagWhitelist 字段但
/// provider 从未消费，已移除以免误导配置）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookWalkerConfig {
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
    /// 本地数据库定时更新间隔（小时）；0 = 仅手动下载（默认 24）。
    #[serde(default = "default_db_update_interval")]
    pub update_interval_hours: u64,
}

impl Default for BookWalkerConfig {
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
            update_interval_hours: default_db_update_interval(),
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
    /// 标题语言优先级（english/romaji/native，越靠前越优先）。非空时系列主标题、
    /// 搜索显示与 staff 姓名均按该优先级取语言版本，不再受登录态 userPreferred 限制；
    /// 空（缺省）=保持原行为（登录时 userPreferred 生效，匿名 english 优先）。
    /// 仅影响显示/写入，不参与匹配（staff 姓名映射：native→name.native，
    /// romaji/english→name.full）。
    #[serde(default)]
    pub title_language_priority: Vec<String>,
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
            title_language_priority: Vec::new(),
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
    /// 控制更新元数据时是否写入 provider 提供的 alternativeTitles（主标题选定后的剩余部分）。
    /// false 时最终写入仅保留主标题（备选不写入）；匹配仍用全量标题，不受影响；
    /// 主标题语言选择（postProcessing.seriesTitleLanguage）仍基于全量候选，不受影响。
    /// 各 provider 可独立配置。默认 true（保持既有行为）。
    #[serde(default = "default_true")]
    pub alternative_titles: bool,
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
    /// Rust 扩展：默认 false（与全局 seriesCovers/bookCovers 默认一致）；
    /// 需显式 `thumbnail: true` 才从 provider 下载封面字节。
    #[serde(default)]
    pub thumbnail: bool,
    #[serde(default = "default_true")]
    pub books: bool,
    #[serde(default = "default_true")]
    pub links: bool,
    #[serde(default)]
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
            alternative_titles: true,
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
            thumbnail: false,
            books: true,
            links: true,
            score: false,
            use_original_publisher: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookMetadataConfig {
    /// Rust 扩展：默认 false（对齐「不主动改书籍标题/卷号」的保守更新策略）；
    /// 需显式 `title: true` 才从 provider 写入书籍标题。
    #[serde(default)]
    pub title: bool,
    #[serde(default = "default_true")]
    pub summary: bool,
    /// Rust 扩展：默认 false；需显式 `number: true` 才从 provider 写入卷号。
    #[serde(default)]
    pub number: bool,
    /// Rust 扩展：默认 false；需显式 `numberSort: true` 才从 provider 写入卷排序。
    #[serde(default)]
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
    /// Rust 扩展：默认 false；需显式 `thumbnail: true` 才从 provider 下载卷封面字节。
    #[serde(default)]
    pub thumbnail: bool,
}

impl Default for BookMetadataConfig {
    fn default() -> Self {
        Self {
            title: false,
            summary: true,
            number: false,
            number_sort: false,
            release_date: true,
            authors: true,
            tags: true,
            isbn: true,
            links: true,
            thumbnail: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webtoons_language_from_yaml() {
        let cfg: ProvidersConfig = serde_yaml::from_str(
            "webtoons:\n  priority: 5\n  enabled: true\n  language: zh-hant\n",
        )
        .unwrap();
        assert_eq!(cfg.webtoons.language, WebtoonsLanguage::ZhHant);
        assert_eq!(cfg.webtoons.provider.priority, 5);
        assert!(cfg.webtoons.provider.enabled);

        // 未写 language → 缺省 en；旧配置可直接沿用
        let cfg: ProvidersConfig = serde_yaml::from_str("webtoons:\n  enabled: true\n").unwrap();
        assert_eq!(cfg.webtoons.language, WebtoonsLanguage::En);

        // YAML 与 API 共用解析器：别名可用，null 视同缺省，未知值报错
        let cfg: ProvidersConfig = serde_yaml::from_str("webtoons:\n  language: zh_TW\n").unwrap();
        assert_eq!(cfg.webtoons.language, WebtoonsLanguage::ZhHant);
        let cfg: ProvidersConfig = serde_yaml::from_str("webtoons:\n  language: null\n").unwrap();
        assert_eq!(cfg.webtoons.language, WebtoonsLanguage::En);
        let err = serde_yaml::from_str::<ProvidersConfig>("webtoons:\n  language: ja\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown webtoons language 'ja'"), "{err}");

        // 整段缺省 → 保持原默认 priority 130
        let cfg = ProvidersConfig::default();
        assert_eq!(cfg.webtoons.provider.priority, 130);
        assert_eq!(cfg.webtoons.language, WebtoonsLanguage::En);
    }

    #[test]
    fn webtoons_language_codes() {
        assert_eq!(WebtoonsLanguage::ZhHant.cookie_locale(), "zh_TW");
        assert_eq!(WebtoonsLanguage::ZhHant.language_tag(), "zh-Hant");
        for code in [
            "zh-hant",
            "ZH-HANT",
            "ZH_HANT",
            "zh-Hant",
            "zh-TW",
            "zh_TW",
            " zh-hant ",
        ] {
            assert_eq!(
                WebtoonsLanguage::parse(code),
                Some(WebtoonsLanguage::ZhHant),
                "{code}"
            );
        }
        assert_eq!(WebtoonsLanguage::parse("EN"), Some(WebtoonsLanguage::En));
        assert_eq!(WebtoonsLanguage::parse("ja"), None);
        assert_eq!(
            WebtoonsLanguage::supported_codes(),
            "en, zh-hant, th, id, es, fr, de"
        );
        assert_eq!(
            serde_yaml::to_string(&WebtoonsLanguage::ZhHant)
                .unwrap()
                .trim(),
            "zh-hant"
        );
    }
}
