//! 元数据 Provider 框架 —— 对应 `snd.komf.providers` 包。
//!
//! `MetadataProvider` trait 对应 `MetadataProvider.kt`；
//! `ProvidersModule` 对应 `ProvidersModule.kt`。

pub mod anilist;
pub mod bangumi;
pub mod bangumi_archive;
pub mod bookwalker;
pub mod comicvine;
pub mod ehentai;
pub mod ehentai_archive;
pub mod mal;
pub mod mangabaka;
pub mod mangadex;
pub mod mangaupdates;
pub mod viz;
pub mod webtoons;
pub mod yenpress;

use crate::config::{MetadataProvidersConfig, ProvidersConfig};
use crate::model::{
    Image, MatchQuery, MediaType, ProviderBookId, ProviderBookMetadata, ProviderSeriesId,
    ProviderSeriesMetadata, SeriesSearchResult,
};
use crate::util::NameSimilarityMatcher;
use std::fmt;
use std::sync::Arc;

/// 用指定默认头构建带 header 的 HTTP 客户端（reqwest::Client 本身不提供 default_headers）。
pub(crate) fn client_with_default_headers(headers: reqwest::header::HeaderMap) -> reqwest::Client {
    reqwest::Client::builder()
        .default_headers(headers)
        .connect_timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("failed to build http client")
}

/// 检查 HTTP 响应状态：2xx 原样返回，非 2xx 读取 body 构造 `ProviderError::Status`。
/// 合并自各 provider client 中逐字相同的 4 行状态检查样板。
pub(crate) async fn ensure_success(
    provider: CoreProviders,
    response: reqwest::Response,
) -> Result<reqwest::Response, ProviderError> {
    let status = response.status();
    if status.is_success() {
        Ok(response)
    } else {
        Err(ProviderError::Status(
            provider,
            status,
            response.text().await.unwrap_or_default(),
        ))
    }
}

/// 按 URL 扩展名推断图片 MIME —— 对应 Kotlin `getThumbnail` 各 client 的扩展名推断
/// （png/webp/gif，其余兜底 jpeg）。恒返回 Some，Option 为对齐 Kotlin `String?` 的包装。
pub(crate) fn detect_image_mime(url: &str) -> Option<String> {
    let lower = url.to_lowercase();
    if lower.ends_with(".png") {
        Some("image/png".into())
    } else if lower.ends_with(".webp") {
        Some("image/webp".into())
    } else if lower.ends_with(".gif") {
        Some("image/gif".into())
    } else {
        Some("image/jpeg".into())
    }
}

/// 从链接文本按模式提取第一个命中的捕获组（正则按模式串 OnceLock 缓存，
/// 避免 resolve_link_id 热路径重复编译正则）。模式非法时返回 None。
pub(crate) fn capture_link_id(query: &str, pattern: &str) -> Option<String> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, regex::Regex>>,
    > = std::sync::OnceLock::new();
    let re = {
        let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
        cache
            .entry(pattern.to_string())
            .or_insert_with(|| regex::Regex::new(pattern).unwrap())
            .clone()
    };
    re.captures(query)
        .and_then(|c| c.iter().skip(1).flatten().next())
        .map(|m| m.as_str().to_string())
}

/// komf 自有 metadata provider 对应的站点域名白名单（host 小写，含子域尾匹配）。
/// 用于 MangaBaka/MangaDex 等聚合站点的外部链接过滤：只写入 komf 已有 provider 的链接。
pub fn is_komf_provider_domain(host: &str) -> bool {
    const DOMAINS: &[&str] = &[
        "anilist.co",
        "myanimelist.net",
        "bgm.tv",
        "bangumi.tv",
        "bangumi.moe",
        "chii.in",
        "mangadex.org",
        "mangaupdates.com",
        "mangabaka.org",
        "bookwalker.jp",
        "comicvine.gamespot.com",
        "e-hentai.org",
        "exhentai.org",
    ];
    let host = host.trim().to_ascii_lowercase();
    DOMAINS
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// provider 名称 —— 对应 `CoreProviders.kt` 枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoreProviders {
    MangaBaka,
    BookWalker,
    Mangadex,
    MangaUpdates,
    Anilist,
    Mal,
    ComicVine,
    Bangumi,
    EHentai,
    YenPress,
    Viz,
    Webtoons,
    Kodansha,
    Nautiljon,
    Hentag,
}

impl CoreProviders {
    pub fn as_str(&self) -> &'static str {
        match self {
            CoreProviders::MangaBaka => "MANGA_BAKA",
            CoreProviders::BookWalker => "BOOK_WALKER",
            CoreProviders::Mangadex => "MANGADEX",
            CoreProviders::MangaUpdates => "MANGA_UPDATES",
            CoreProviders::Anilist => "ANILIST",
            CoreProviders::Mal => "MAL",
            CoreProviders::ComicVine => "COMIC_VINE",
            CoreProviders::Bangumi => "BANGUMI",
            CoreProviders::EHentai => "EHENTAI",
            CoreProviders::YenPress => "YEN_PRESS",
            CoreProviders::Viz => "VIZ",
            CoreProviders::Webtoons => "WEBTOONS",
            CoreProviders::Kodansha => "KODANSHA",
            CoreProviders::Nautiljon => "NAUTILJON",
            CoreProviders::Hentag => "HENTAG",
        }
    }

    pub fn from_str(name: &str) -> Option<CoreProviders> {
        match name {
            "MANGA_BAKA" => Some(CoreProviders::MangaBaka),
            "BOOK_WALKER" => Some(CoreProviders::BookWalker),
            "MANGADEX" => Some(CoreProviders::Mangadex),
            "MANGA_UPDATES" => Some(CoreProviders::MangaUpdates),
            "ANILIST" => Some(CoreProviders::Anilist),
            "MAL" => Some(CoreProviders::Mal),
            "COMIC_VINE" => Some(CoreProviders::ComicVine),
            "BANGUMI" => Some(CoreProviders::Bangumi),
            "EHENTAI" => Some(CoreProviders::EHentai),
            "YEN_PRESS" => Some(CoreProviders::YenPress),
            "VIZ" => Some(CoreProviders::Viz),
            "WEBTOONS" => Some(CoreProviders::Webtoons),
            "KODANSHA" => Some(CoreProviders::Kodansha),
            "NAUTILJON" => Some(CoreProviders::Nautiljon),
            "HENTAG" => Some(CoreProviders::Hentag),
            _ => None,
        }
    }
}

impl fmt::Display for CoreProviders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 离线数据源句柄（bangumi/Archive、e-hentai-db）—— 供 app 层查询下载状态 / 手动触发更新。
/// provider 内部启动后台服务后，通过 `MetadataProvider::offline_archive` 暴露。
#[derive(Clone)]
pub enum OfflineArchive {
    Bangumi(Arc<bangumi_archive::BangumiArchiveService>),
    EHentai(Arc<ehentai_archive::EHentaiArchiveService>),
}

/// 元数据提供者接口 —— 对应 `MetadataProvider.kt`。
#[async_trait::async_trait]
pub trait MetadataProvider: Send + Sync {
    fn provider_name(&self) -> CoreProviders;

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError>;

    async fn get_series_cover(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError>;

    async fn get_book_metadata(
        &self,
        series_id: &ProviderSeriesId,
        book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError>;

    async fn search_series(
        &self,
        series_name: &str,
        limit: usize,
        media_type: Option<MediaType>,
    ) -> Result<Vec<SeriesSearchResult>, ProviderError>;

    async fn match_series_metadata(
        &self,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError>;

    /// 从搜索/匹配输入中解析本 provider 的网页链接 → 返回 series_id
    /// （Rust 扩展：输入为 provider 站点链接时直接按 id 获取，跳过站点搜索）。
    /// 默认不支持；各 provider 覆盖实现。
    fn resolve_link_id(&self, _query: &str) -> Option<String> {
        None
    }

    /// 本 provider 是否写入 alternativeTitles（`seriesMetadata.alternativeTitles`）。
    /// false 时最终写入仅保留主标题（备选不写入）；匹配与主标题语言选择仍用全量标题，不受影响。
    /// 默认 true（保持既有行为）；各 provider 返回自身 `SeriesMetadataConfig.alternative_titles`。
    fn alternative_titles_enabled(&self) -> bool {
        true
    }

    /// 本 provider 的离线数据源（bangumi/Archive、e-hentai-db 等；下载状态/手动更新用）。
    /// 默认无；Bangumi / EHentai 返回其 archive 服务句柄。
    fn offline_archive(&self) -> Option<OfflineArchive> {
        None
    }

    /// 链接命中时构造搜索结果（Rust 扩展：搜索框提交 provider 链接时显示用）。
    /// 默认实现：resolve_link_id → get_series_metadata → 通用模板（titles.first，
    /// 无封面 URL / mediaType）。各 provider 可重写复用其搜索结果显示逻辑
    /// （封面 URL、mediaType、标题选择、语言、nsfw）。
    async fn resolve_link_search_result(&self, query: &str) -> Option<SeriesSearchResult> {
        let id = self.resolve_link_id(query)?;
        let meta = self
            .get_series_metadata(&ProviderSeriesId(id.clone()))
            .await
            .ok()?;
        let title = meta
            .metadata
            .titles
            .first()
            .map(|t| t.name.clone())
            .or_else(|| meta.metadata.title.as_ref().map(|t| t.name.clone()))
            .unwrap_or_else(|| query.to_string());
        Some(SeriesSearchResult {
            url: Some(query.trim().to_string()),
            image_url: None,
            title,
            provider: self.provider_name().as_str().to_string(),
            result_id: id,
            media_type: None,
            language: meta.metadata.language.clone(),
            nsfw: None,
        })
    }
}

/// provider 错误。
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{0}")]
    Message(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("provider {0} returned status {1}: {2}")]
    Status(CoreProviders, reqwest::StatusCode, String),
    #[error("provider {0} is not implemented in the Rust port")]
    NotImplemented(CoreProviders),
}

impl ProviderError {
    pub fn message(msg: impl Into<String>) -> Self {
        ProviderError::Message(msg.into())
    }
}

/// 注册表中单个 provider 及其优先级。
pub struct RegisteredProvider {
    pub provider: Arc<dyn MetadataProvider>,
    pub priority: i32,
}

/// 一组（默认或某个 library 的）provider 容器 —— 对应 `MetadataProvidersContainer`。
pub struct MetadataProvidersContainer {
    providers: Vec<RegisteredProvider>,
    /// 离线数据源侧通道（bangumi/Archive、e-hentai-db）：provider 禁用但 archive 启用时，
    /// 下载状态 / 手动更新功能仍可用（匹配不参与）。provider 启用时服务由 provider 持有，此处为空。
    archives: Vec<OfflineArchive>,
}

impl MetadataProvidersContainer {
    pub fn new(providers: Vec<RegisteredProvider>) -> Self {
        let mut providers = providers;
        providers.sort_by_key(|p| p.priority);
        Self {
            providers,
            archives: Vec::new(),
        }
    }

    /// 注册离线数据源（provider 禁用但 archive 启用的场景）。
    pub fn add_archive(&mut self, archive: OfflineArchive) {
        self.archives.push(archive);
    }

    /// 离线数据源列表（provider 禁用场景注册；provider 启用时经 `provider.offline_archive()` 获取）。
    pub fn archives(&self) -> &[OfflineArchive] {
        &self.archives
    }

    pub fn providers(&self) -> &[RegisteredProvider] {
        &self.providers
    }

    pub fn provider(&self, name: CoreProviders) -> Option<Arc<dyn MetadataProvider>> {
        self.providers
            .iter()
            .find(|p| p.provider.provider_name() == name)
            .map(|p| p.provider.clone())
    }
}

/// 默认 + 按 library 区分的 provider 集合 —— 对应 `MetadataProviders`。
pub struct MetadataProviders {
    default: MetadataProvidersContainer,
    library: std::collections::HashMap<String, MetadataProvidersContainer>,
}

impl MetadataProviders {
    pub fn new(default: MetadataProvidersContainer) -> Self {
        Self {
            default,
            library: Default::default(),
        }
    }

    pub fn with_library(
        mut self,
        library: std::collections::HashMap<String, MetadataProvidersContainer>,
    ) -> Self {
        self.library = library;
        self
    }

    pub fn default_providers_list(&self) -> Vec<Arc<dyn MetadataProvider>> {
        self.default
            .providers()
            .iter()
            .map(|p| p.provider.clone())
            .collect()
    }

    pub fn providers(&self, library_id: &str) -> Vec<Arc<dyn MetadataProvider>> {
        self.library
            .get(library_id)
            .unwrap_or(&self.default)
            .providers()
            .iter()
            .map(|p| p.provider.clone())
            .collect()
    }

    pub fn provider(
        &self,
        library_id: &str,
        name: CoreProviders,
    ) -> Option<Arc<dyn MetadataProvider>> {
        self.library
            .get(library_id)
            .unwrap_or(&self.default)
            .provider(name)
    }

    /// 默认容器中 bangumi 的离线数据源（archive 未启用/未注册 → None）。
    /// 侧通道（provider 禁用但 archive 启用）优先，其次经 provider 暴露。
    pub fn bangumi_archive(&self) -> Option<Arc<bangumi_archive::BangumiArchiveService>> {
        self.default
            .archives()
            .iter()
            .find_map(|a| match a {
                OfflineArchive::Bangumi(svc) => Some(svc.clone()),
                _ => None,
            })
            .or_else(|| {
                match self
                    .default
                    .provider(CoreProviders::Bangumi)?
                    .offline_archive()?
                {
                    OfflineArchive::Bangumi(svc) => Some(svc),
                    _ => None,
                }
            })
    }

    /// 默认容器中 ehentai 的离线数据源（archive 未启用/未注册 → None）。
    /// 侧通道（provider 禁用但 archive 启用）优先，其次经 provider 暴露。
    pub fn ehentai_archive(&self) -> Option<Arc<ehentai_archive::EHentaiArchiveService>> {
        self.default
            .archives()
            .iter()
            .find_map(|a| match a {
                OfflineArchive::EHentai(svc) => Some(svc.clone()),
                _ => None,
            })
            .or_else(|| {
                match self
                    .default
                    .provider(CoreProviders::EHentai)?
                    .offline_archive()?
                {
                    OfflineArchive::EHentai(svc) => Some(svc),
                    _ => None,
                }
            })
    }
}

/// Provider 模块装配 —— 对应 `ProvidersModule.kt`。
///
/// `database_work_dir` 对应 Kotlin `CoreModule` 的 workDir：其下 `mangabaka/`、
/// `bookwalker/` 子目录存放 SQLite 数据库（由下载器维护）。数据库缺失时
/// BookWalker 不注册、MangaBaka DATABASE 模式禁用（与 Kotlin 行为一致）。
pub struct ProvidersModule {
    pub metadata_providers: MetadataProviders,
    /// OAuth 管理器（AniList/MAL/Bangumi 登录态；None = 未初始化）。
    pub oauth_manager: Option<Arc<crate::oauth::OAuthManager>>,
}

/// 各容器（default + 库级）的主标题语言（`postProcessing.seriesTitleLanguage`）：
/// 作用于 MangaDex 标题语言、MangaBaka 主标题选择、bangumi 作者/出版社中文名
/// （use_chinese_names）。库级允许覆盖全局值。
#[derive(Debug, Clone, Default)]
pub struct SeriesTitleLanguages {
    pub default: Option<String>,
    /// 库 id → 该库 postProcessing.seriesTitleLanguage（YAML 深合并后的生效值）。
    pub libraries: std::collections::HashMap<String, Option<String>>,
}

impl SeriesTitleLanguages {
    /// 容器生效值：库级 Some 优先，否则（库级缺失/None/未配置条目）回退 default。
    pub fn for_library(&self, library_id: Option<&str>) -> Option<String> {
        library_id
            .and_then(|id| self.libraries.get(id))
            .and_then(|v| v.clone())
            .or_else(|| self.default.clone())
    }
}

/// 全局封面写入开关生效值（`metadataProcessing.seriesCovers` / `bookCovers`）。
/// provider 的 `seriesMetadata.thumbnail` / `bookMetadata.thumbnail` 需与其取「与」：
/// 全局关闭时 provider 不再下载封面字节（对齐 Kotlin ProvidersModule 构造时行为），
/// 避免「下载完被 updater 丢弃」的浪费。上传侧仍由 MetadataUpdater 独立把关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverFetchSwitches {
    pub series: bool,
    pub books: bool,
}

/// default + 库级覆盖（与 `SeriesTitleLanguages` 同源：取 komga.metadataUpdate 生效值；
/// 库级未单独配置 libraryProviders 容器时，其 provider 实例共享 default 容器，
/// 此时库级封面开关不影响下载——与上游 Kotlin 行为一致）。
/// 不提供 Default：开关语义必须显式给出，避免「隐式默认全关」误伤封面下载。
#[derive(Debug, Clone)]
pub struct CoverFetchConfig {
    pub default: CoverFetchSwitches,
    pub libraries: std::collections::HashMap<String, CoverFetchSwitches>,
}

impl CoverFetchConfig {
    /// 容器生效值：库级存在即用库级，否则回退 default。
    pub fn for_library(&self, library_id: Option<&str>) -> CoverFetchSwitches {
        library_id
            .and_then(|id| self.libraries.get(id))
            .copied()
            .unwrap_or(self.default)
    }
}

impl ProvidersModule {
    pub fn new(
        config: &MetadataProvidersConfig,
        http_client: reqwest::Client,
        database_work_dir: Option<&std::path::Path>,
    ) -> Self {
        // new() 不带封面开关：不做下载侧限制（保持 provider 配置原样）。
        Self::with_oauth(config, http_client, database_work_dir, None, None, None)
    }

    /// 带 OAuth 的构造：`oauth_manager` 由 app 层创建（共享 http client 与 work_dir）。
    /// `cover_fetch`：全局封面开关生效值（None = 不做下载侧限制，保持 provider 配置原样）。
    pub fn with_oauth(
        config: &MetadataProvidersConfig,
        http_client: reqwest::Client,
        database_work_dir: Option<&std::path::Path>,
        oauth_manager: Option<Arc<crate::oauth::OAuthManager>>,
        series_title_languages: Option<SeriesTitleLanguages>,
        cover_fetch: Option<CoverFetchConfig>,
    ) -> Self {
        let default_name_matcher = config.name_matching_mode;
        let series_title_languages = series_title_languages.unwrap_or_default();

        // 离线数据源（bangumi/Archive、e-hentai-db）全局唯一实例：数据文件全局一份
        // （缺省 workDir/bangumi-archive、workDir/ehentai/e-hentai.db），default 与所有
        // library 容器共享同一服务，避免每容器重复实例导致重复下载/并发冲突。
        // - 服务恒创建：状态徽标 / update-*-db 手动端点不受 `archive.enabled` 限制
        //   （未下载显示「未下载」，点击即触发下载）；`archive.enabled` 只控制 provider
        //   是否用离线数据参与匹配（false → provider 走在线）
        // - 周期自动更新由 app_context 统一编排（`start_auto_update`，provider 启用才调用）
        // - 服务用 default 配置启动（库级 dir/dbFile 覆盖属极端场景，不拆分数据文件）
        let bangumi_archive_dir = config
            .default_providers
            .bangumi
            .archive
            .dir
            .as_ref()
            .map(std::path::PathBuf::from)
            .or_else(|| database_work_dir.map(|d| d.join("bangumi-archive")))
            .unwrap_or_else(|| std::path::PathBuf::from("bangumi-archive"));
        let bangumi_archive = Some(bangumi_archive::BangumiArchiveService::start(
            &config.default_providers.bangumi.archive,
            http_client.clone(),
            bangumi_archive_dir,
        ));
        let ehentai_archive = Some(ehentai_archive::EHentaiArchiveService::start(
            &config.default_providers.e_hentai.archive,
            http_client.clone(),
            database_work_dir,
        ));

        let default_providers = create_metadata_providers(
            &config.default_providers,
            default_name_matcher,
            config,
            &http_client,
            database_work_dir,
            oauth_manager.clone(),
            series_title_languages.for_library(None),
            cover_fetch.as_ref().map(|c| c.for_library(None)),
            bangumi_archive.clone(),
            ehentai_archive.clone(),
        );
        let library_providers = config
            .library_providers
            .iter()
            .map(|(library_id, library_config)| {
                (
                    library_id.clone(),
                    create_metadata_providers(
                        library_config,
                        default_name_matcher,
                        config,
                        &http_client,
                        database_work_dir,
                        oauth_manager.clone(),
                        series_title_languages.for_library(Some(library_id)),
                        cover_fetch
                            .as_ref()
                            .map(|c| c.for_library(Some(library_id))),
                        bangumi_archive.clone(),
                        ehentai_archive.clone(),
                    ),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();

        Self {
            metadata_providers: MetadataProviders::new(default_providers)
                .with_library(library_providers),
            oauth_manager,
        }
    }
}

/// 全局封面开关与 provider 自身 thumbnail 开关取「与」：
/// 全局 `seriesCovers`/`bookCovers` 关闭时对应下载直接禁用（series-only 的
/// provider 只受 series 开关影响；无 bookMetadata 字段的不动）。
fn apply_cover_switches(config: &mut ProvidersConfig, covers: CoverFetchSwitches) {
    let series = covers.series;
    let books = covers.books;
    config.manga_updates.series_metadata.thumbnail &= series;
    config.manga_updates.book_metadata.thumbnail &= books;
    config.mal.series_metadata.thumbnail &= series;
    config.mal.book_metadata.thumbnail &= books;
    config.ani_list.series_metadata.thumbnail &= series;
    config.manga_dex.series_metadata.thumbnail &= series;
    config.manga_dex.book_metadata.thumbnail &= books;
    config.bangumi.provider.series_metadata.thumbnail &= series;
    config.bangumi.provider.book_metadata.thumbnail &= books;
    config.comic_vine.series_metadata.thumbnail &= series;
    config.comic_vine.book_metadata.thumbnail &= books;
    config.manga_baka.series_metadata.thumbnail &= series;
    config.book_walker.series_metadata.thumbnail &= series;
    config.book_walker.book_metadata.thumbnail &= books;
    config.yen_press.series_metadata.thumbnail &= series;
    config.yen_press.book_metadata.thumbnail &= books;
    config.viz.series_metadata.thumbnail &= series;
    config.viz.book_metadata.thumbnail &= books;
    config.webtoons.series_metadata.thumbnail &= series;
    config.webtoons.book_metadata.thumbnail &= books;
    config.e_hentai.series_metadata.thumbnail &= series;
    config.e_hentai.book_metadata.thumbnail &= books;
}

fn create_metadata_providers(
    config: &ProvidersConfig,
    default_name_matcher: NameSimilarityMatcher,
    global: &MetadataProvidersConfig,
    http_client: &reqwest::Client,
    database_work_dir: Option<&std::path::Path>,
    oauth_manager: Option<Arc<crate::oauth::OAuthManager>>,
    series_title_language: Option<String>,
    covers: Option<CoverFetchSwitches>,
    bangumi_archive: Option<Arc<bangumi_archive::BangumiArchiveService>>,
    ehentai_archive: Option<Arc<ehentai_archive::EHentaiArchiveService>>,
) -> MetadataProvidersContainer {
    let mut providers: Vec<RegisteredProvider> = Vec::new();
    // 离线数据源侧通道：provider 禁用但 archive 启用时仍启动（下载状态/手动更新独立于匹配）。
    let mut archives: Vec<OfflineArchive> = Vec::new();

    // 全局封面开关（metadataProcessing.seriesCovers/bookCovers）关闭时不再下载封面：
    // 与 provider 自身 thumbnail 开关取「与」。仅影响下载侧；上传侧由 updater 把关。
    let mut effective = config.clone();
    if let Some(covers) = covers {
        apply_cover_switches(&mut effective, covers);
    }
    let config = &effective;

    // 对应 Kotlin CoreModule：mangabaka/mangabaka.sqlite、bookwalker/bkwk-db.sqlite
    let manga_baka_db = database_work_dir.map(|d| d.join("mangabaka").join("mangabaka.sqlite"));
    let book_walker_db = database_work_dir.map(|d| d.join("bookwalker").join("bkwk-db.sqlite"));

    if let Some(p) =
        mangaupdates::create_provider(&config.manga_updates, default_name_matcher, http_client)
    {
        providers.push(RegisteredProvider {
            provider: Arc::new(p),
            priority: config.manga_updates.priority,
        });
    }
    if let Some(p) = mal::create_provider(
        &config.mal,
        global.mal_client_id.as_deref(),
        default_name_matcher,
        http_client,
        oauth_manager.clone(),
    ) {
        providers.push(RegisteredProvider {
            provider: Arc::new(p),
            priority: config.mal.priority,
        });
    }
    if let Some(p) = anilist::create_provider(
        &config.ani_list,
        default_name_matcher,
        http_client,
        oauth_manager.clone(),
    ) {
        providers.push(RegisteredProvider {
            provider: Arc::new(p),
            priority: config.ani_list.priority,
        });
    }
    if let Some(p) = mangadex::create_provider(
        &config.manga_dex,
        default_name_matcher,
        http_client,
        series_title_language.clone(),
    ) {
        providers.push(RegisteredProvider {
            provider: Arc::new(p),
            priority: config.manga_dex.priority,
        });
    }
    // bangumi/Archive：全局服务恒存在（状态徽标 / update-bangumi-db 手动端点不受
    // archive.enabled 限制）；`archive.enabled` 只决定本容器 provider 是否用离线数据匹配
    // （false → provider 走在线，服务仍经侧通道注册供状态/手动更新）。
    let bangumi_provider_archive = if config.bangumi.archive.enabled {
        bangumi_archive.clone()
    } else {
        None
    };
    let mut bangumi_got_archive = false;
    if let Some(p) = bangumi::create_provider(
        &config.bangumi,
        default_name_matcher,
        global.bangumi_token.as_deref(),
        http_client,
        database_work_dir,
        oauth_manager.clone(),
        bangumi_provider_archive,
        series_title_language.clone(),
    ) {
        bangumi_got_archive = config.bangumi.archive.enabled;
        providers.push(RegisteredProvider {
            provider: Arc::new(p),
            priority: config.bangumi.provider.priority,
        });
    }
    // 服务未被本容器 provider 持有（provider 禁用，或 archive.enabled=false 不参与匹配）：
    // 经侧通道注册（状态徽标 / update-bangumi-db 可用，匹配不参与）。
    if !bangumi_got_archive {
        if let Some(svc) = bangumi_archive {
            archives.push(OfflineArchive::Bangumi(svc));
        }
    }
    if let Some(p) = comicvine::create_provider(
        &config.comic_vine,
        global.comic_vine_api_key.as_deref(),
        global.comic_vine_search_limit,
        global.comic_vine_issue_name.as_deref(),
        global.comic_vine_id_format.as_deref(),
        default_name_matcher,
        http_client,
    ) {
        providers.push(RegisteredProvider {
            provider: Arc::new(p),
            priority: config.comic_vine.priority,
        });
    }

    // MangaBaka：API 或 DATABASE（本地 SQLite）数据源；BookWalker：本地 SQLite 数据库。
    // OAuth 登录后 API 请求带用户态（401 走强制刷新重试，与 tracker 统一）。
    if let Some(p) = mangabaka::create_provider(
        &config.manga_baka,
        default_name_matcher,
        http_client,
        manga_baka_db.as_deref(),
        series_title_language.clone(),
        config.manga_baka.cover_languages.clone(),
        oauth_manager.clone(),
    ) {
        providers.push(RegisteredProvider {
            provider: Arc::new(p),
            priority: config.manga_baka.priority,
        });
    }
    if let Some(p) = bookwalker::create_provider(
        &config.book_walker,
        book_walker_db.as_deref(),
        default_name_matcher,
        http_client,
    ) {
        providers.push(RegisteredProvider {
            provider: Arc::new(p),
            priority: config.book_walker.priority,
        });
    }
    if let Some(p) = yenpress::create_provider(&config.yen_press, default_name_matcher, http_client)
    {
        providers.push(RegisteredProvider {
            provider: Arc::new(p),
            priority: config.yen_press.priority,
        });
    }
    if let Some(p) = viz::create_provider(&config.viz, default_name_matcher, http_client) {
        providers.push(RegisteredProvider {
            provider: Arc::new(p),
            priority: config.viz.priority,
        });
    }
    if let Some(p) = webtoons::create_provider(&config.webtoons, default_name_matcher, http_client)
    {
        providers.push(RegisteredProvider {
            provider: Arc::new(p),
            priority: config.webtoons.priority,
        });
    }
    // EHentai —— 对应 Kotlin PR #284 createEHentaiMetadataProvider
    // 同 bangumi：全局服务恒存在，`archive.enabled` 只决定 provider 是否用离线数据匹配。
    let ehentai_provider_archive = if config.e_hentai.archive.enabled {
        ehentai_archive.clone()
    } else {
        None
    };
    let mut ehentai_got_archive = false;
    if let Some(p) = ehentai::create_provider(
        &config.e_hentai,
        default_name_matcher,
        http_client,
        database_work_dir,
        ehentai_provider_archive,
    ) {
        ehentai_got_archive = config.e_hentai.archive.enabled;
        providers.push(RegisteredProvider {
            provider: Arc::new(p),
            priority: config.e_hentai.priority,
        });
    }
    // 服务未被本容器 provider 持有（provider 禁用，或 archive.enabled=false 不参与匹配）：
    // 经侧通道注册（状态徽标 / update-ehentai-db 可用，匹配不参与）。
    if !ehentai_got_archive {
        if let Some(svc) = ehentai_archive {
            archives.push(OfflineArchive::EHentai(svc));
        }
    }

    let mut container = MetadataProvidersContainer::new(providers);
    for archive in archives {
        container.add_archive(archive);
    }
    container
}

/// 未实现的 provider 桩。
pub struct UnimplementedProvider {
    name: CoreProviders,
    enabled: bool,
}

#[async_trait::async_trait]
impl MetadataProvider for UnimplementedProvider {
    fn provider_name(&self) -> CoreProviders {
        self.name
    }

    async fn get_series_metadata(
        &self,
        _id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        Err(ProviderError::NotImplemented(self.name))
    }

    async fn get_series_cover(
        &self,
        _id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError> {
        Err(ProviderError::NotImplemented(self.name))
    }

    async fn get_book_metadata(
        &self,
        _series_id: &ProviderSeriesId,
        _book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError> {
        Err(ProviderError::NotImplemented(self.name))
    }

    async fn search_series(
        &self,
        _series_name: &str,
        _limit: usize,
        _media_type: Option<crate::model::MediaType>,
    ) -> Result<Vec<SeriesSearchResult>, ProviderError> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        Err(ProviderError::NotImplemented(self.name))
    }

    async fn match_series_metadata(
        &self,
        _match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        Err(ProviderError::NotImplemented(self.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 库级主标题语言覆盖：库级 Some 优先；库级缺失/None 或未配置条目回退 default。
    #[test]
    fn series_title_languages_library_override() {
        let mut libs = std::collections::HashMap::new();
        libs.insert("lib-zh".to_string(), Some("zh".to_string()));
        libs.insert("lib-none".to_string(), None);
        let langs = SeriesTitleLanguages {
            default: Some("en".to_string()),
            libraries: libs,
        };
        // default 容器
        assert_eq!(langs.for_library(None).as_deref(), Some("en"));
        // 库级覆盖
        assert_eq!(langs.for_library(Some("lib-zh")).as_deref(), Some("zh"));
        // 库级显式 None / 未配置条目 → 回退 default
        assert_eq!(langs.for_library(Some("lib-none")).as_deref(), Some("en"));
        assert_eq!(
            langs.for_library(Some("lib-missing")).as_deref(),
            Some("en")
        );
        // default 为 None（komga 未配置）：库级 Some 仍生效，其余为 None
        let langs = SeriesTitleLanguages {
            default: None,
            libraries: [("lib-zh".to_string(), Some("zh".to_string()))]
                .into_iter()
                .collect(),
        };
        assert_eq!(langs.for_library(Some("lib-zh")).as_deref(), Some("zh"));
        assert_eq!(langs.for_library(None), None);
        assert_eq!(langs.for_library(Some("lib-missing")), None);
    }

    #[test]
    fn cover_fetch_config_for_library_falls_back_to_default() {
        let mut libs = std::collections::HashMap::new();
        libs.insert(
            "lib-a".to_string(),
            CoverFetchSwitches {
                series: true,
                books: false,
            },
        );
        let cfg = CoverFetchConfig {
            default: CoverFetchSwitches {
                series: false,
                books: true,
            },
            libraries: libs,
        };
        assert_eq!(
            cfg.for_library(None),
            CoverFetchSwitches {
                series: false,
                books: true
            }
        );
        assert_eq!(
            cfg.for_library(Some("lib-a")),
            CoverFetchSwitches {
                series: true,
                books: false
            }
        );
        // 未配置条目回退 default
        assert_eq!(
            cfg.for_library(Some("lib-missing")),
            CoverFetchSwitches {
                series: false,
                books: true
            }
        );
    }

    #[test]
    fn apply_cover_switches_ands_provider_thumbnails() {
        use crate::config::{BookMetadataConfig, ProvidersConfig, SeriesMetadataConfig};
        // thumbnail 默认 false（新默认值）；本用例验证「与」语义，构造全部置 true 的 fixture。
        fn config_with_thumbnails_on() -> ProvidersConfig {
            let mut config = ProvidersConfig::default();
            let series_on = {
                let mut c = SeriesMetadataConfig::default();
                c.thumbnail = true;
                c
            };
            let books_on = {
                let mut c = BookMetadataConfig::default();
                c.thumbnail = true;
                c
            };
            macro_rules! all_thumbnails_on {
                ($($field:ident),*) => {$(
                    config.$field.series_metadata = series_on.clone();
                    config.$field.book_metadata = books_on.clone();
                )*};
            }
            all_thumbnails_on!(
                manga_updates,
                mal,
                manga_dex,
                comic_vine,
                book_walker,
                yen_press,
                viz,
                webtoons
            );
            config.ani_list.series_metadata = series_on.clone();
            config.manga_baka.series_metadata = series_on.clone();
            config.bangumi.provider.series_metadata = series_on.clone();
            config.bangumi.provider.book_metadata = books_on.clone();
            config.e_hentai.series_metadata = series_on.clone();
            config.e_hentai.book_metadata = books_on.clone();
            config
        }

        // 全开：保持原样
        let mut config = config_with_thumbnails_on();
        apply_cover_switches(
            &mut config,
            CoverFetchSwitches {
                series: true,
                books: true,
            },
        );
        assert!(config.manga_updates.series_metadata.thumbnail);
        assert!(config.manga_updates.book_metadata.thumbnail);
        // 全关：全部禁用（含 series-only 的 ani_list/manga_baka）
        apply_cover_switches(
            &mut config,
            CoverFetchSwitches {
                series: false,
                books: false,
            },
        );
        assert!(!config.manga_updates.series_metadata.thumbnail);
        assert!(!config.manga_updates.book_metadata.thumbnail);
        assert!(!config.ani_list.series_metadata.thumbnail);
        assert!(!config.manga_baka.series_metadata.thumbnail);
        assert!(!config.e_hentai.series_metadata.thumbnail);
        // 只开 series：series 保留、book 关闭（「与」单向，需全新 fixture）
        let mut config = config_with_thumbnails_on();
        apply_cover_switches(
            &mut config,
            CoverFetchSwitches {
                series: true,
                books: false,
            },
        );
        assert!(config.manga_dex.series_metadata.thumbnail);
        assert!(!config.manga_dex.book_metadata.thumbnail);
        assert!(config.yen_press.series_metadata.thumbnail);
        assert!(!config.yen_press.book_metadata.thumbnail);
    }
}
