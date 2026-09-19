//! MangaBaka Provider —— 对应 `snd.komf.providers.mangabaka` 包。
//!
//! 使用公开 API（`https://api.mangabaka.org`，无需密钥）：
//! - `GET /v1/series/search?q=...&type=...&type_not=...`
//! - `GET /v1/series/{id}`
//!
//! 对应 Kotlin 文件：`MangaBakaSeries.kt`、`MangaBakaMetadataProvider.kt`、
//! `MangaBakaMetadataMapper.kt`、`api/MangaBakaApiClient.kt`、
//! `api/MangaBakaResponse.kt`、`api/MangaBakaSearchResponse.kt`、
//! `db/MangaBakaDbDataSource.kt`、`db/MangaBakaDbDownloader.kt`。
//! 两种模式均实现：`mode: API`（默认，在线 API）与 `mode: DATABASE`（本地 SQLite，
//! 经 `update-mangabaka-db` 下载并建立 FTS5 索引）。
use crate::config::{MangaBakaConfig, SeriesMetadataConfig};
use crate::model::{
    Author, Image, MatchQuery, ProviderBookId, ProviderBookMetadata, ProviderSeriesId,
    ProviderSeriesMetadata, Publisher, PublisherType, ReleaseDate, SeriesBook, SeriesMetadata,
    SeriesSearchResult, SeriesStatus, SeriesTitle, TitleType, WebLink,
};
use crate::providers::{CoreProviders, MetadataProvider, ProviderError};
use crate::util::NameSimilarityMatcher;
use komf_api_models::config::DownloadProgress;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// API 模型（snake_case 与 MangaBaka API 一致）
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct MangaBakaSearchResponse {
    data: Vec<MangaBakaSeriesDto>,
}

#[derive(Debug, Deserialize)]
struct MangaBakaSeriesResponse {
    data: MangaBakaSeriesDto,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct MangaBakaSeriesDto {
    id: i32,
    artists: Option<Vec<String>>,
    authors: Option<Vec<String>>,
    canonical_url: String,
    cover: MangaBakaCoverDto,
    description: Option<String>,
    final_volume: Option<String>,
    publishers: Option<Vec<MangaBakaPublisherDto>>,
    rating: Option<f64>,
    status: MangaBakaStatusDto,
    #[allow(dead_code)]
    r#type: MangaBakaTypeDto,
    links_v2: Option<Vec<MangaBakaLinkDto>>,
    published: Option<MangaBakaPublishedDateDto>,
    tags_v2: Option<Vec<MangaBakaTagDto>>,
    titles: Option<Vec<MangaBakaTitleDto>>,
    source: MangaBakaSourceDto,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct MangaBakaCoverDto {
    #[allow(dead_code)]
    x150: Option<MangaBakaCoverDpiDto>,
    #[allow(dead_code)]
    x250: Option<MangaBakaCoverDpiDto>,
    x350: Option<MangaBakaCoverDpiDto>,
}

#[derive(Debug, Deserialize)]
struct MangaBakaCoverDpiDto {
    x1: Option<String>,
    #[allow(dead_code)]
    x2: Option<String>,
    #[allow(dead_code)]
    x3: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MangaBakaPublisherDto {
    name: Option<String>,
    #[serde(rename = "type")]
    type_: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MangaBakaLinkDto {
    name_display: String,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct MangaBakaPublishedDateDto {
    start_date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct MangaBakaTagDto {
    name: String,
    #[serde(default)]
    is_genre: bool,
}

#[derive(Debug, PartialEq, Deserialize)]
struct MangaBakaTitleDto {
    language: String,
    title: String,
    traits: Vec<String>,
    #[serde(default)]
    is_primary: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct MangaBakaSourceDto {
    anilist: Option<MangaBakaSourceEntryDto>,
    anime_news_network: Option<MangaBakaSourceEntryDto>,
    anime_planet: Option<MangaBakaSourceEntryDto>,
    kitsu: Option<MangaBakaSourceEntryDto>,
    manga_updates: Option<MangaBakaSourceEntryDto>,
    #[allow(dead_code)]
    my_anime_list: Option<MangaBakaSourceEntryDto>,
    shikimori: Option<MangaBakaSourceEntryDto>,
}

#[derive(Debug, Deserialize)]
struct MangaBakaSourceEntryDto {
    id: Option<serde_json::Value>,
}

impl MangaBakaSourceEntryDto {
    fn id_string(&self) -> Option<String> {
        self.id.as_ref().and_then(|v| match v {
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::String(s) => Some(s.clone()),
            _ => None,
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MangaBakaStatusDto {
    Cancelled,
    Completed,
    Hiatus,
    Releasing,
    Upcoming,
    Unknown,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MangaBakaTypeDto {
    Manga,
    Novel,
    Manhwa,
    Manhua,
    Oel,
    Other,
}

impl MangaBakaTypeDto {
    fn as_str(&self) -> &'static str {
        match self {
            MangaBakaTypeDto::Manga => "manga",
            MangaBakaTypeDto::Novel => "novel",
            MangaBakaTypeDto::Manhwa => "manhwa",
            MangaBakaTypeDto::Manhua => "manhua",
            MangaBakaTypeDto::Oel => "oel",
            MangaBakaTypeDto::Other => "other",
        }
    }
}

// ---------------------------------------------------------------------------
// API 客户端 —— 对应 `MangaBakaApiClient.kt`
// ---------------------------------------------------------------------------

const BASE_URL: &str = "https://api.mangabaka.org";

struct MangaBakaApiClient {
    http: reqwest::Client,
}

impl MangaBakaApiClient {
    fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    async fn search(
        &self,
        title: &str,
        types: &[MangaBakaTypeDto],
        types_not: &[MangaBakaTypeDto],
    ) -> Result<Vec<MangaBakaSeriesDto>, ProviderError> {
        let mut request = self
            .http
            .get(format!("{BASE_URL}/v1/series/search"))
            .query(&[("q", title)]);
        for t in types {
            request = request.query(&[("type", t.as_str())]);
        }
        for t in types_not {
            request = request.query(&[("type_not", t.as_str())]);
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(
                CoreProviders::MangaBaka,
                status,
                body,
            ));
        }
        let parsed: MangaBakaSearchResponse = response.json().await?;
        Ok(parsed.data)
    }

    async fn get_series(&self, id: i32) -> Result<MangaBakaSeriesDto, ProviderError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/v1/series/{id}"))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(
                CoreProviders::MangaBaka,
                status,
                body,
            ));
        }
        let parsed: MangaBakaSeriesResponse = response.json().await?;
        Ok(parsed.data)
    }
}

// ---------------------------------------------------------------------------
// Metadata mapper —— 对应 `MangaBakaMetadataMapper.kt`
// ---------------------------------------------------------------------------

pub struct MangaBakaMetadataMapper {
    metadata_config: SeriesMetadataConfig,
    author_roles: Vec<crate::model::AuthorRole>,
    artist_roles: Vec<crate::model::AuthorRole>,
}

impl MangaBakaMetadataMapper {
    pub fn new(
        metadata_config: SeriesMetadataConfig,
        author_roles: Vec<crate::model::AuthorRole>,
        artist_roles: Vec<crate::model::AuthorRole>,
    ) -> Self {
        Self {
            metadata_config,
            author_roles,
            artist_roles,
        }
    }

    fn to_series_metadata(
        &self,
        series: &MangaBakaSeriesDto,
        thumbnail: Option<Image>,
    ) -> ProviderSeriesMetadata {
        let cfg = &self.metadata_config;

        let status = Some(map_status(&series.status));
        let status_field = cfg.status.then_some(status).flatten();

        // 标题处理：原生（native 且非 -Latn）、罗马音、本地化
        let mut all_titles: Vec<&MangaBakaTitleDto> = series.titles.iter().flatten().collect();
        all_titles.sort_by(|a, b| {
            let pa = a.is_primary.unwrap_or(false);
            let pb = b.is_primary.unwrap_or(false);
            pb.cmp(&pa)
        });
        let native_title = all_titles
            .iter()
            .find(|t| t.traits.iter().any(|x| x == "native") && !t.language.ends_with("-Latn"));
        let titles: Vec<SeriesTitle> = all_titles
            .iter()
            .map(|t| {
                let romanized = t.title.ends_with("-Latn");
                let is_native = native_title == Some(t);
                SeriesTitle {
                    name: t.title.clone(),
                    r#type: Some(if is_native {
                        TitleType::Native
                    } else if romanized {
                        TitleType::Romaji
                    } else {
                        TitleType::Localized
                    }),
                    language: Some(t.language.replace("-Latn", "-ro")),
                }
            })
            .collect();
        let title_field = cfg.title.then(|| {
            titles.first().cloned().unwrap_or_else(|| SeriesTitle {
                name: String::new(),
                r#type: None,
                language: None,
            })
        });
        // Kotlin seriesTitles：title 配置关闭时保留标题列表，仅清空 type/language
        let titles_field = if cfg.title {
            titles.clone()
        } else {
            titles
                .iter()
                .map(|t| SeriesTitle {
                    r#type: None,
                    language: None,
                    ..t.clone()
                })
                .collect()
        };

        // 作者 / 艺术家
        let authors = if cfg.authors {
            let mut authors: Vec<Author> = series
                .authors
                .iter()
                .flatten()
                .flat_map(|name| {
                    self.author_roles.iter().map(|role| Author {
                        name: name.clone(),
                        role: *role,
                    })
                })
                .collect();
            authors.extend(series.artists.iter().flatten().flat_map(|name| {
                self.artist_roles.iter().map(|role| Author {
                    name: name.clone(),
                    role: *role,
                })
            }));
            authors
        } else {
            Vec::new()
        };

        // 出版社（Original / English）
        let mut original_publishers: Vec<Publisher> = series
            .publishers
            .iter()
            .flatten()
            .filter(|p| p.type_.as_deref() == Some("Original"))
            .filter_map(|p| p.name.clone())
            .map(|name| Publisher {
                name,
                r#type: Some(PublisherType::Original),
                language_tag: None,
            })
            .collect();
        dedup_publishers(&mut original_publishers);
        let mut english_publishers: Vec<Publisher> = series
            .publishers
            .iter()
            .flatten()
            .filter(|p| p.type_.as_deref() == Some("English"))
            .filter_map(|p| p.name.clone())
            .map(|name| Publisher {
                name,
                r#type: Some(PublisherType::Localized),
                language_tag: Some("en".to_string()),
            })
            .collect();
        dedup_publishers(&mut english_publishers);
        let publisher = if cfg.publisher {
            if cfg.use_original_publisher {
                original_publishers.first().cloned()
            } else {
                english_publishers
                    .first()
                    .cloned()
                    .or_else(|| original_publishers.first().cloned())
            }
        } else {
            None
        };
        let alternative_publishers: Vec<Publisher> = if cfg.publisher {
            original_publishers
                .iter()
                .chain(english_publishers.iter())
                .filter(|p| Some(*p) != publisher.as_ref())
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        // 链接：官方链接（links_v2，按 label 排序）+ 来源链接
        let official_links: Vec<WebLink> = if cfg.links {
            let mut links: Vec<WebLink> = series
                .links_v2
                .iter()
                .flatten()
                .map(|link| WebLink {
                    label: link.name_display.clone(),
                    url: link.url.clone(),
                })
                .collect();
            links.sort_by(|a, b| a.label.cmp(&b.label));
            links
        } else {
            Vec::new()
        };
        let source_links: Vec<WebLink> = if cfg.links {
            let mut links = Vec::new();
            links.push(WebLink {
                label: "MangaBaka".to_string(),
                url: series.canonical_url.clone(),
            });
            let source = &series.source;
            if let Some(id) = source.anilist.as_ref().and_then(|s| s.id_string()) {
                links.push(WebLink {
                    label: "AniList".to_string(),
                    url: format!("https://anilist.co/manga/{id}"),
                });
            }
            if let Some(id) = source
                .anime_news_network
                .as_ref()
                .and_then(|s| s.id_string())
            {
                links.push(WebLink {
                    label: "AnimeNewsNetwork".to_string(),
                    url: format!("https://www.animenewsnetwork.com/encyclopedia/manga.php?id={id}"),
                });
            }
            if let Some(id) = source.anime_planet.as_ref().and_then(|s| s.id_string()) {
                links.push(WebLink {
                    label: "AnimePlanet".to_string(),
                    url: format!("https://www.anime-planet.com/manga/{id}"),
                });
            }
            if let Some(id) = source.kitsu.as_ref().and_then(|s| s.id_string()) {
                links.push(WebLink {
                    label: "Kitsu".to_string(),
                    url: format!("https://kitsu.app/manga/{id}"),
                });
            }
            if let Some(id) = source.manga_updates.as_ref().and_then(|s| s.id_string()) {
                links.push(WebLink {
                    label: "MangaUpdates".to_string(),
                    url: format!("https://www.mangaupdates.com/series/{id}"),
                });
            }
            if let Some(id) = source.shikimori.as_ref().and_then(|s| s.id_string()) {
                links.push(WebLink {
                    label: "Shikimori".to_string(),
                    url: format!("https://shikimori.one/mangas/{id}"),
                });
            }
            links
        } else {
            Vec::new()
        };

        // 类型 / 标签
        let all_tags: Vec<&MangaBakaTagDto> = series.tags_v2.iter().flatten().collect();
        let genres = if cfg.genres {
            all_tags
                .iter()
                .filter(|t| t.is_genre)
                .map(|t| t.name.clone())
                .collect()
        } else {
            Vec::new()
        };
        let tags = if cfg.tags {
            all_tags
                .iter()
                .filter(|t| !t.is_genre)
                .map(|t| t.name.clone())
                .collect()
        } else {
            Vec::new()
        };

        let total_book_count = cfg
            .total_book_count
            .then(|| {
                series
                    .final_volume
                    .as_deref()
                    .and_then(|v| v.trim().parse::<i32>().ok())
            })
            .flatten();

        let summary = cfg
            .summary
            .then(|| series.description.as_ref().map(|d| html_to_text(d)))
            .flatten();

        let release_date = cfg
            .release_date
            .then(|| {
                series
                    .published
                    .as_ref()
                    .and_then(|p| p.start_date.as_ref())
                    .and_then(|d| parse_date_parts(d))
                    .map(|(year, month, day)| ReleaseDate::new(Some(year), month, day))
            })
            .flatten();

        let score = cfg.score.then_some(series.rating).flatten();

        let metadata = SeriesMetadata {
            status: status_field,
            title: title_field,
            titles: titles_field,
            summary,
            publisher,
            alternative_publishers,
            reading_direction: None,
            age_rating: None,
            language: None,
            genres,
            tags,
            total_book_count,
            authors,
            release_date,
            links: {
                let mut links = official_links;
                links.extend(source_links);
                links
            },
            score,
            thumbnail,
        };

        let books = if cfg.books {
            Vec::<SeriesBook>::new()
        } else {
            Vec::new()
        };

        ProviderSeriesMetadata {
            id: ProviderSeriesId(series.id.to_string()),
            metadata,
            books,
        }
    }

    fn to_series_search_result(&self, series: &MangaBakaSeriesDto) -> SeriesSearchResult {
        SeriesSearchResult {
            url: Some(format!("https://mangabaka.org/{}", series.id)),
            image_url: series.cover.x350.as_ref().and_then(|c| c.x1.clone()),
            title: primary_title(series),
            provider: CoreProviders::MangaBaka.as_str().to_string(),
            result_id: series.id.to_string(),
            media_type: None,
            language: None,
        }
    }
}

/// 主标题 —— 对应 Kotlin `getPrimaryTitle`。
fn primary_title(series: &MangaBakaSeriesDto) -> String {
    let titles = match &series.titles {
        Some(titles) => titles,
        None => return String::new(),
    };
    if let Some(native) = titles
        .iter()
        .find(|t| t.traits.iter().any(|x| x == "native"))
    {
        return native.title.clone();
    }
    if let Some(primary_en) = titles
        .iter()
        .find(|t| t.language == "en" && t.is_primary == Some(true))
    {
        return primary_en.title.clone();
    }
    titles.first().map(|t| t.title.clone()).unwrap_or_default()
}

/// 状态映射 —— 对应 Kotlin `when (series.status)`。
fn map_status(status: &MangaBakaStatusDto) -> SeriesStatus {
    match status {
        MangaBakaStatusDto::Releasing
        | MangaBakaStatusDto::Upcoming
        | MangaBakaStatusDto::Unknown => SeriesStatus::Ongoing,
        MangaBakaStatusDto::Completed => SeriesStatus::Completed,
        MangaBakaStatusDto::Cancelled => SeriesStatus::Abandoned,
        MangaBakaStatusDto::Hiatus => SeriesStatus::Hiatus,
    }
}

/// 解析 `YYYY-MM-DD`。
fn parse_date_parts(value: &str) -> Option<(i32, Option<u32>, Option<u32>)> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next().and_then(|m| m.parse().ok());
    let day = parts.next().and_then(|d| d.parse().ok());
    Some((year, month, day))
}

/// 轻量 HTML → 纯文本（对应 Kotlin `Ksoup.parse(it).wholeText()`）。
fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_entity = false;
    let mut entity = String::new();
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            '&' if !in_tag => {
                in_entity = true;
                entity.clear();
            }
            ';' if in_entity => {
                in_entity = false;
                text.push_str(&decode_entity(&entity));
                entity.clear();
            }
            _ if in_tag => {}
            _ if in_entity => entity.push(ch),
            _ => text.push(ch),
        }
    }
    if !entity.is_empty() {
        text.push_str(&decode_entity(&entity));
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entity(entity: &str) -> String {
    match entity {
        "amp" => "&".to_string(),
        "lt" => "<".to_string(),
        "gt" => ">".to_string(),
        "quot" => "\"".to_string(),
        "apos" => "'".to_string(),
        "nbsp" => " ".to_string(),
        _ => format!("&{entity};"),
    }
}

// ---------------------------------------------------------------------------
// Provider —— 对应 `MangaBakaMetadataProvider.kt`
// ---------------------------------------------------------------------------

pub struct MangaBakaMetadataProvider {
    data_source: MangaBakaDataSource,
    metadata_mapper: MangaBakaMetadataMapper,
    name_matcher: NameSimilarityMatcher,
    cover_fetch_client: Option<reqwest::Client>,
    type_includes: Vec<MangaBakaTypeDto>,
    type_excludes: Vec<MangaBakaTypeDto>,
}

pub fn create_provider(
    config: &MangaBakaConfig,
    default_name_matcher: NameSimilarityMatcher,
    http_client: &reqwest::Client,
    database_file: Option<&std::path::Path>,
) -> Option<MangaBakaMetadataProvider> {
    if !config.enabled {
        return None;
    }
    let data_source = match config.mode {
        crate::config::MangaBakaMode::Api => {
            MangaBakaDataSource::Api(MangaBakaApiClient::new(http_client.clone()))
        }
        crate::config::MangaBakaMode::Database => {
            match database_file {
                Some(path) if path.exists() => {
                    MangaBakaDataSource::Db(MangaBakaDbRepository::new(path.to_path_buf()))
                }
                _ => {
                    // 对应 Kotlin："Failed to find MangaBaka database. Disabling MangaBaka provider"
                    tracing::warn!(
                        "Failed to find MangaBaka database. Disabling MangaBaka provider"
                    );
                    return None;
                }
            }
        }
    };
    let type_excludes: Vec<MangaBakaTypeDto> = match config.media_type {
        crate::model::MediaType::Manga => vec![MangaBakaTypeDto::Novel],
        _ => Vec::new(),
    };
    let type_includes: Vec<MangaBakaTypeDto> = match config.media_type {
        crate::model::MediaType::Manga => Vec::new(),
        crate::model::MediaType::Novel => vec![MangaBakaTypeDto::Novel],
        crate::model::MediaType::Comic => vec![MangaBakaTypeDto::Oel, MangaBakaTypeDto::Other],
        crate::model::MediaType::Webtoon => {
            vec![MangaBakaTypeDto::Manhua, MangaBakaTypeDto::Manhwa]
        }
    };

    let name_matcher = config.name_matching_mode.unwrap_or(default_name_matcher);
    Some(MangaBakaMetadataProvider {
        data_source,
        metadata_mapper: MangaBakaMetadataMapper::new(
            config.series_metadata.clone(),
            config.author_roles.clone(),
            config.artist_roles.clone(),
        ),
        name_matcher,
        cover_fetch_client: config
            .series_metadata
            .thumbnail
            .then(|| http_client.clone()),
        type_includes,
        type_excludes,
    })
}

impl MangaBakaMetadataProvider {
    async fn fetch_cover(&self, series: &MangaBakaSeriesDto) -> Option<Image> {
        let client = self.cover_fetch_client.as_ref()?;
        let url = series.cover.x350.as_ref()?.x1.as_ref()?;
        let response = client.get(url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        // Kotlin：mime 取自响应 Content-Type
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .map(|ct| ct.to_str().unwrap_or("").to_string());
        let bytes = response.bytes().await.ok()?.to_vec();
        Some(Image::new(bytes, mime))
    }
}

/// 对应 Kotlin `.toSet()`：按 (name,type,languageTag) 去重，保留首次出现顺序。
fn dedup_publishers(v: &mut Vec<Publisher>) {
    let mut out: Vec<Publisher> = Vec::with_capacity(v.len());
    for p in v.drain(..) {
        if !out.iter().any(|e| e == &p) {
            out.push(p);
        }
    }
    *v = out;
}

#[async_trait::async_trait]
impl MetadataProvider for MangaBakaMetadataProvider {
    fn provider_name(&self) -> CoreProviders {
        CoreProviders::MangaBaka
    }

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        let id: i32 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid MangaBaka series id: {}", series_id.0))
        })?;
        let series = self.data_source.get_series(id).await?;
        let cover = self.fetch_cover(&series).await;
        Ok(self.metadata_mapper.to_series_metadata(&series, cover))
    }

    async fn get_series_cover(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError> {
        let id: i32 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid MangaBaka series id: {}", series_id.0))
        })?;
        let series = self.data_source.get_series(id).await?;
        Ok(self.fetch_cover(&series).await)
    }

    async fn get_book_metadata(
        &self,
        _series_id: &ProviderSeriesId,
        _book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError> {
        // 对应 Kotlin `TODO("Not yet implemented")`
        Err(ProviderError::NotImplemented(CoreProviders::MangaBaka))
    }

    async fn search_series(
        &self,
        series_name: &str,
        limit: usize,
        _media_type: Option<crate::model::MediaType>,
    ) -> Result<Vec<SeriesSearchResult>, ProviderError> {
        let results = self
            .data_source
            .search(series_name, &self.type_includes, &self.type_excludes)
            .await?;
        Ok(results
            .iter()
            .take(limit)
            .map(|r| self.metadata_mapper.to_series_search_result(r))
            .collect())
    }

    async fn match_series_metadata(
        &self,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        let series_name = match_query.series_name.clone();
        let results = self
            .data_source
            .search(
                &series_name.chars().take(400).collect::<String>(),
                &self.type_includes,
                &self.type_excludes,
            )
            .await?;
        let matched = results.iter().find(|series| {
            let titles: Vec<String> = series
                .titles
                .as_ref()
                .map(|titles| titles.iter().map(|t| t.title.clone()).collect())
                .unwrap_or_default();
            self.name_matcher.matches(
                &match_query.normalized_series_name(),
                &match_query.normalize_titles(&titles),
            )
        });
        match matched {
            Some(series) => {
                let cover = self.fetch_cover(series).await;
                Ok(Some(self.metadata_mapper.to_series_metadata(series, cover)))
            }
            None => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// 数据源抽象 + DATABASE 模式 —— 对应 `MangaBakaDataSource.kt` / `db/*.kt`
// ---------------------------------------------------------------------------

/// 数据源：API（在线）或本地 SQLite 数据库（`MangaBakaMode.DATABASE`）。
enum MangaBakaDataSource {
    Api(MangaBakaApiClient),
    Db(MangaBakaDbRepository),
}

impl MangaBakaDataSource {
    async fn search(
        &self,
        title: &str,
        types: &[MangaBakaTypeDto],
        types_not: &[MangaBakaTypeDto],
    ) -> Result<Vec<MangaBakaSeriesDto>, ProviderError> {
        match self {
            MangaBakaDataSource::Api(client) => client.search(title, types, types_not).await,
            MangaBakaDataSource::Db(repo) => repo.search(title, types, types_not),
        }
    }

    async fn get_series(&self, id: i32) -> Result<MangaBakaSeriesDto, ProviderError> {
        match self {
            MangaBakaDataSource::Api(client) => client.get_series(id).await,
            MangaBakaDataSource::Db(repo) => repo.get_series(id),
        }
    }
}

// ---------------------------------------------------------------------------
// DATABASE 数据访问 —— 对应 `MangaBakaDbDataSource.kt` / `MangaBakaSeriesTable.kt`
// ---------------------------------------------------------------------------

/// 本地 SQLite 只读仓储。连接按查询打开（保证 `Sync`，行为与 Kotlin 每次 transaction 等价）。
pub struct MangaBakaDbRepository {
    database_file: PathBuf,
}

const MANGA_BAKA_DB_URL: &str = "https://api.mangabaka.org/v1/database/series.sqlite.tar.gz";
const MANGA_BAKA_CHECKSUM_URL: &str =
    "https://api.mangabaka.org/v1/database/series.sqlite.tar.gz.sha1";

impl MangaBakaDbRepository {
    pub fn new(database_file: impl Into<PathBuf>) -> Self {
        Self {
            database_file: database_file.into(),
        }
    }

    fn open(&self) -> Result<rusqlite::Connection, ProviderError> {
        rusqlite::Connection::open(&self.database_file)
            .map_err(|e| ProviderError::message(format!("failed to open MangaBaka database: {e}")))
    }

    /// 对应 `MangaBakaDbDataSource.search`：FTS5 titles MATCH + type IN/NOT IN + rank 排序。
    fn search(
        &self,
        title: &str,
        types: &[MangaBakaTypeDto],
        types_not: &[MangaBakaTypeDto],
    ) -> Result<Vec<MangaBakaSeriesDto>, ProviderError> {
        let conn = self.open()?;
        let quoted = format!("\"{title}\"");
        let mut sql = String::from("SELECT id FROM series_fts WHERE titles MATCH ?");
        let mut params: Vec<rusqlite::types::Value> = vec![rusqlite::types::Value::Text(quoted)];
        if !types.is_empty() {
            let list = types.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            sql.push_str(&format!(" AND type IN ({list})"));
            params.extend(
                types
                    .iter()
                    .map(|t| rusqlite::types::Value::Text(t.as_str().to_string())),
            );
        }
        if !types_not.is_empty() {
            let list = types_not.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            sql.push_str(&format!(" AND type NOT IN ({list})"));
            params.extend(
                types_not
                    .iter()
                    .map(|t| rusqlite::types::Value::Text(t.as_str().to_string())),
            );
        }
        sql.push_str(" ORDER BY rank LIMIT 10");

        let ids: Vec<i32> = {
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| ProviderError::message(format!("MangaBaka db search prepare: {e}")))?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params.iter()), |row| row.get(0))
                .map_err(|e| ProviderError::message(format!("MangaBaka db search: {e}")))?;
            let ids: Vec<i32> = rows
                .collect::<Result<_, _>>()
                .map_err(|e| ProviderError::message(format!("MangaBaka db search rows: {e}")))?;
            drop(stmt);
            ids
        };

        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.fetch_series_by_ids(&conn, &ids)
    }

    /// 对应 `MangaBakaDbDataSource.getSeries`：按 id 查询，无结果抛错。
    fn get_series(&self, id: i32) -> Result<MangaBakaSeriesDto, ProviderError> {
        let conn = self.open()?;
        self.fetch_series_by_ids(&conn, &[id])?
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::message(format!("failed to find series with id {id}")))
    }

    fn fetch_series_by_ids(
        &self,
        conn: &rusqlite::Connection,
        ids: &[i32],
    ) -> Result<Vec<MangaBakaSeriesDto>, ProviderError> {
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT * FROM series WHERE id IN ({placeholders})");
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| ProviderError::message(format!("MangaBaka db prepare: {e}")))?;
        let rows = {
            let mut rows = stmt
                .query(rusqlite::params_from_iter(ids.iter().map(|i| *i)))
                .map_err(|e| ProviderError::message(format!("MangaBaka db query: {e}")))?;
            let mut out = Vec::new();
            while let Some(row) = rows
                .next()
                .map_err(|e| ProviderError::message(format!("MangaBaka db rows: {e}")))?
            {
                out.push(row_to_series_dto(row)?);
            }
            out
        };
        drop(stmt);
        Ok(rows)
    }
}

/// 从 SQLite 行构造 API DTO —— 对应 Kotlin `ResultRow.toModel()`。
///
/// 仅填充映射链路消费的字段，其余保持 None/默认（与 Kotlin 领域模型到 API DTO
/// 的字段投影等价：API 响应多余字段不影响映射）。
fn row_to_series_dto(row: &rusqlite::Row<'_>) -> Result<MangaBakaSeriesDto, ProviderError> {
    let json_vec =
        |row: &rusqlite::Row<'_>, col: &str| -> Result<Option<serde_json::Value>, ProviderError> {
            match row.get::<_, Option<String>>(col)? {
                Some(text) => serde_json::from_str(&text)
                    .map(Some)
                    .map_err(|e| ProviderError::message(format!("MangaBaka db json {col}: {e}"))),
                None => Ok(None),
            }
        };
    let text = |row: &rusqlite::Row<'_>, col: &str| -> Result<Option<String>, ProviderError> {
        row.get(col)
            .map_err(|e| ProviderError::message(format!("MangaBaka db col {col}: {e}")))
    };

    let status = match text(row, "status")?
        .unwrap_or_default()
        .to_uppercase()
        .as_str()
    {
        "CANCELLED" => MangaBakaStatusDto::Cancelled,
        "COMPLETED" => MangaBakaStatusDto::Completed,
        "HIATUS" => MangaBakaStatusDto::Hiatus,
        "RELEASING" => MangaBakaStatusDto::Releasing,
        "UPCOMING" => MangaBakaStatusDto::Upcoming,
        _ => MangaBakaStatusDto::Unknown,
    };
    let type_ = match text(row, "type")?
        .unwrap_or_default()
        .to_uppercase()
        .as_str()
    {
        "MANGA" => MangaBakaTypeDto::Manga,
        "NOVEL" => MangaBakaTypeDto::Novel,
        "MANHWA" => MangaBakaTypeDto::Manhwa,
        "MANHUA" => MangaBakaTypeDto::Manhua,
        "OEL" => MangaBakaTypeDto::Oel,
        _ => MangaBakaTypeDto::Other,
    };

    let cover_x350 = text(row, "cover_x350_x1")?;
    let cover = MangaBakaCoverDto {
        x150: None,
        x250: None,
        x350: cover_x350.map(|url| MangaBakaCoverDpiDto {
            x1: Some(url),
            x2: None,
            x3: None,
        }),
    };

    let source = MangaBakaSourceDto {
        anilist: text(row, "source_anilist_id")?.map(|v| MangaBakaSourceEntryDto {
            id: Some(serde_json::Value::from(v.parse::<i64>().unwrap_or(0))),
        }),
        anime_news_network: text(row, "source_anime_news_network_id")?.map(|v| {
            MangaBakaSourceEntryDto {
                id: Some(serde_json::Value::from(v.parse::<i64>().unwrap_or(0))),
            }
        }),
        anime_planet: text(row, "source_anime_planet_id")?.map(|v| MangaBakaSourceEntryDto {
            id: Some(serde_json::Value::String(v)),
        }),
        kitsu: text(row, "source_kitsu_id")?.map(|v| MangaBakaSourceEntryDto {
            id: Some(serde_json::Value::from(v.parse::<i64>().unwrap_or(0))),
        }),
        manga_updates: text(row, "source_manga_updates_id")?.map(|v| MangaBakaSourceEntryDto {
            id: Some(serde_json::Value::String(v)),
        }),
        my_anime_list: text(row, "source_my_anime_list_id")?.map(|v| MangaBakaSourceEntryDto {
            id: Some(serde_json::Value::from(v.parse::<i64>().unwrap_or(0))),
        }),
        shikimori: text(row, "source_shikimori_id")?.map(|v| MangaBakaSourceEntryDto {
            id: Some(serde_json::Value::from(v.parse::<i64>().unwrap_or(0))),
        }),
    };

    let published = match text(row, "published_start_date")? {
        Some(date) => Some(MangaBakaPublishedDateDto {
            start_date: Some(date),
        }),
        None => None,
    };

    Ok(MangaBakaSeriesDto {
        id: row
            .get("id")
            .map_err(|e| ProviderError::message(format!("MangaBaka db id: {e}")))?,
        artists: json_vec(row, "artists")?
            .map(|v| {
                serde_json::from_value(v)
                    .map_err(|e| ProviderError::message(format!("MangaBaka db artists: {e}")))
            })
            .transpose()?,
        authors: json_vec(row, "authors")?
            .map(|v| {
                serde_json::from_value(v)
                    .map_err(|e| ProviderError::message(format!("MangaBaka db authors: {e}")))
            })
            .transpose()?,
        canonical_url: text(row, "canonical_url")?.unwrap_or_default(),
        cover,
        description: text(row, "description")?,
        final_volume: text(row, "final_volume")?,
        publishers: json_vec(row, "publishers")?
            .map(|v| {
                serde_json::from_value(v)
                    .map_err(|e| ProviderError::message(format!("MangaBaka db publishers: {e}")))
            })
            .transpose()?,
        rating: row
            .get("rating")
            .map_err(|e| ProviderError::message(format!("MangaBaka db rating: {e}")))?,
        status,
        r#type: type_,
        links_v2: json_vec(row, "links_v2")?
            .map(|v| {
                serde_json::from_value(v)
                    .map_err(|e| ProviderError::message(format!("MangaBaka db links: {e}")))
            })
            .transpose()?,
        published,
        tags_v2: json_vec(row, "tags_v2")?
            .map(|v| {
                serde_json::from_value(v)
                    .map_err(|e| ProviderError::message(format!("MangaBaka db tags: {e}")))
            })
            .transpose()?,
        titles: json_vec(row, "titles")?
            .map(|v| {
                serde_json::from_value(v)
                    .map_err(|e| ProviderError::message(format!("MangaBaka db titles: {e}")))
            })
            .transpose()?,
        source,
    })
}

// ---------------------------------------------------------------------------
// DATABASE 下载器 —— 对应 `MangaBakaDbDownloader.kt` / `MangaBakaDbDownloadTimestamp.kt`
// ---------------------------------------------------------------------------

/// MangaBaka 本地数据库下载器：sha1 校验 → 下载 tar.gz → 解压首个条目 → 建 FTS5 索引。
///
/// 元数据（timestamp / checksum）存于 `mangabaka/` 目录下，对应 Kotlin
/// `MangaBakaDbMetadata`（timestamp、checksum.sha1 文件）。
pub struct MangaBakaDbDownloader {
    work_dir: PathBuf,
    database_archive: PathBuf,
    database_file: PathBuf,
    http: reqwest::Client,
    download_in_progress: Arc<std::sync::atomic::AtomicBool>,
    progress: Arc<std::sync::Mutex<Option<tokio::sync::watch::Sender<Option<DownloadProgress>>>>>,
}

impl MangaBakaDbDownloader {
    pub fn new(work_dir: impl Into<PathBuf>, http: reqwest::Client) -> Self {
        let work_dir = work_dir.into();
        Self {
            database_file: work_dir.join("mangabaka.sqlite"),
            database_archive: work_dir.join("mangabaka.tar.gz"),
            work_dir,
            http,
            download_in_progress: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            progress: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn database_file(&self) -> PathBuf {
        self.database_file.clone()
    }

    /// 工作目录（timestamp / checksum 文件所在目录）。
    pub fn work_dir(&self) -> &PathBuf {
        &self.work_dir
    }

    /// 对应 Kotlin `MangaBakaDbMetadata.timestamp`（`/config` 返回用）。
    pub fn download_timestamp(&self) -> Option<String> {
        std::fs::read_to_string(self.work_dir.join("timestamp"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 对应 Kotlin `MangaBakaDbMetadata.isValid()`：timestamp + checksum 文件均存在。
    fn metadata_valid(&self) -> bool {
        self.work_dir.join("timestamp").exists() && self.work_dir.join("checksum.sha1").exists()
    }

    /// 启动（或复用进行中的）下载，返回进度事件流。
    pub fn launch_download(&self) -> tokio::sync::watch::Receiver<Option<DownloadProgress>> {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        if self
            .download_in_progress
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            {
                let mut guard = self.progress.lock().unwrap();
                *guard = Some(sender.clone());
            }
            let this = self.clone();
            tokio::spawn(async move {
                let result = this.do_download(&sender).await;
                this.download_in_progress
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                let _ = result;
            });
        } else {
            let guard = self.progress.lock().unwrap();
            if let Some(current) = guard.as_ref() {
                return current.subscribe();
            }
        }
        receiver
    }

    async fn do_download(
        &self,
        sender: &tokio::sync::watch::Sender<Option<DownloadProgress>>,
    ) -> Result<(), String> {
        let emit = |sender: &tokio::sync::watch::Sender<Option<DownloadProgress>>,
                    event: DownloadProgress| {
            let _ = sender.send(Some(event));
        };

        // 1. 校验和检查：db 有效且校验一致 → 直接 Finished 跳过下载
        emit(
            sender,
            DownloadProgress::ProgressEvent {
                total: 0,
                completed: 0,
                info: Some(MANGA_BAKA_CHECKSUM_URL.to_string()),
            },
        );
        let new_checksum = self
            .http
            .get(MANGA_BAKA_CHECKSUM_URL)
            .send()
            .await
            .map_err(|e| format!("HttpRequestException: {e}"))?
            .text()
            .await
            .map_err(|e| format!("HttpRequestException: {e}"))?
            .trim()
            .to_string();
        if self.database_file.exists() && self.metadata_valid() {
            let stored = std::fs::read_to_string(self.work_dir.join("checksum.sha1"))
                .unwrap_or_default()
                .trim()
                .to_string();
            if stored == new_checksum {
                emit(sender, DownloadProgress::FinishedEvent);
                return Ok(());
            }
        }
        // 元数据/旧库清理
        let _ = std::fs::remove_file(self.work_dir.join("timestamp"));
        let _ = std::fs::remove_file(self.work_dir.join("checksum.sha1"));
        let _ = std::fs::remove_file(&self.database_file);
        let _ = std::fs::remove_file(&self.database_archive);
        let _ = std::fs::create_dir_all(&self.work_dir);

        // 2. 下载压缩包
        emit(
            sender,
            DownloadProgress::ProgressEvent {
                total: 0,
                completed: 0,
                info: Some(MANGA_BAKA_DB_URL.to_string()),
            },
        );
        let response = self
            .http
            .get(MANGA_BAKA_DB_URL)
            .send()
            .await
            .map_err(|e| format!("HttpRequestException: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("ResponseException: {}", response.status()));
        }
        let total = response.content_length().unwrap_or(0) as i64;
        emit(
            sender,
            DownloadProgress::ProgressEvent {
                total,
                completed: 0,
                info: Some(MANGA_BAKA_DB_URL.to_string()),
            },
        );
        let mut stream = response.bytes_stream();
        let mut file = tokio::fs::File::create(&self.database_archive)
            .await
            .map_err(|e| format!("FileSystemException: {e}"))?;
        let mut completed: i64 = 0;
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("HttpRequestException: {e}"))?;
            completed += chunk.len() as i64;
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("FileSystemException: {e}"))?;
            emit(
                sender,
                DownloadProgress::ProgressEvent {
                    total,
                    completed,
                    info: Some(MANGA_BAKA_DB_URL.to_string()),
                },
            );
        }
        file.flush()
            .await
            .map_err(|e| format!("FileSystemException: {e}"))?;
        drop(file);

        // 3. 解压（tar + gzip，取首个条目）——对应 Kotlin `extractDatabaseFile`
        emit(
            sender,
            DownloadProgress::ProgressEvent {
                total: 0,
                completed: 0,
                info: Some(format!("extracting {:?}", self.database_archive)),
            },
        );
        {
            let input = std::fs::File::open(&self.database_archive)
                .map_err(|e| format!("FileSystemException: {e}"))?;
            let gz = flate2::read::GzDecoder::new(input);
            let mut archive = tar::Archive::new(gz);
            let mut entries = archive
                .entries()
                .map_err(|e| format!("TarException: {e}"))?;
            let first = entries
                .next()
                .ok_or_else(|| "TarException: empty archive".to_string())?
                .map_err(|e| format!("TarException: {e}"))?;
            let output = std::fs::File::create(&self.database_file)
                .map_err(|e| format!("FileSystemException: {e}"))?;
            let mut output = std::io::BufWriter::new(output);
            use std::io::Read;
            use std::io::Write;
            std::io::copy(&mut first.take(u64::MAX), &mut output)
                .map_err(|e| format!("FileSystemException: {e}"))?;
            output
                .flush()
                .map_err(|e| format!("FileSystemException: {e}"))?;
        }

        // 4. FTS5 索引
        emit(
            sender,
            DownloadProgress::ProgressEvent {
                total: 0,
                completed: 0,
                info: Some("creating search index".to_string()),
            },
        );
        {
            let conn = rusqlite::Connection::open(&self.database_file)
                .map_err(|e| format!("SQLiteException: {e}"))?;
            conn.execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS series_fts USING fts5 \
                 (id, titles, type, tokenize = 'trigram');",
            )
            .map_err(|e| format!("SQLiteException: {e}"))?;
            conn.execute_batch(
                "INSERT INTO series_fts \
                 SELECT s.id, GROUP_CONCAT(json_extract(json_each.value, '$.title'), ', '), s.type \
                 FROM series s, json_each(titles) \
                 WHERE state = 'active' \
                 GROUP BY s.id;",
            )
            .map_err(|e| format!("SQLiteException: {e}"))?;
        }

        // 5. 写元数据 + 清理压缩包
        let now = chrono::Utc::now();
        let ts = now.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        std::fs::write(self.work_dir.join("timestamp"), ts)
            .map_err(|e| format!("FileSystemException: {e}"))?;
        std::fs::write(self.work_dir.join("checksum.sha1"), &new_checksum)
            .map_err(|e| format!("FileSystemException: {e}"))?;
        let _ = std::fs::remove_file(&self.database_archive);

        emit(sender, DownloadProgress::FinishedEvent);
        Ok(())
    }
}

impl Clone for MangaBakaDbDownloader {
    fn clone(&self) -> Self {
        Self {
            work_dir: self.work_dir.clone(),
            database_archive: self.database_archive.clone(),
            database_file: self.database_file.clone(),
            http: self.http.clone(),
            download_in_progress: self.download_in_progress.clone(),
            progress: self.progress.clone(),
        }
    }
}
// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert_eq!(
            map_status(&MangaBakaStatusDto::Releasing),
            SeriesStatus::Ongoing
        );
        assert_eq!(
            map_status(&MangaBakaStatusDto::Completed),
            SeriesStatus::Completed
        );
        assert_eq!(
            map_status(&MangaBakaStatusDto::Cancelled),
            SeriesStatus::Abandoned
        );
        assert_eq!(
            map_status(&MangaBakaStatusDto::Hiatus),
            SeriesStatus::Hiatus
        );
        assert_eq!(
            map_status(&MangaBakaStatusDto::Unknown),
            SeriesStatus::Ongoing
        );
    }

    #[test]
    fn html_to_text_strips_tags_and_entities() {
        assert_eq!(
            html_to_text("<p>Hello <b>world</b> &amp; more</p>"),
            "Hello world & more"
        );
        assert_eq!(html_to_text("plain text"), "plain text");
    }

    #[test]
    fn date_parsing() {
        assert_eq!(
            parse_date_parts("2020-05-12"),
            Some((2020, Some(5), Some(12)))
        );
        assert_eq!(parse_date_parts("garbage"), None);
    }

    #[test]
    fn primary_title_selection() {
        let series = MangaBakaSeriesDto {
            id: 1,
            artists: None,
            authors: None,
            canonical_url: "https://mangabaka.org/1".to_string(),
            cover: MangaBakaCoverDto {
                x150: None,
                x250: None,
                x350: None,
            },
            description: None,
            final_volume: None,
            publishers: None,
            rating: None,
            status: MangaBakaStatusDto::Releasing,
            r#type: MangaBakaTypeDto::Manga,
            links_v2: None,
            published: None,
            tags_v2: None,
            titles: Some(vec![
                MangaBakaTitleDto {
                    language: "en".to_string(),
                    title: "English Title".to_string(),
                    traits: vec!["romanized".to_string()],
                    is_primary: Some(true),
                },
                MangaBakaTitleDto {
                    language: "ja".to_string(),
                    title: "日本語".to_string(),
                    traits: vec!["native".to_string()],
                    is_primary: None,
                },
            ]),
            source: MangaBakaSourceDto {
                anilist: None,
                anime_news_network: None,
                anime_planet: None,
                kitsu: None,
                manga_updates: None,
                my_anime_list: None,
                shikimori: None,
            },
        };
        assert_eq!(primary_title(&series), "日本語");
    }

    #[test]
    fn deserializes_series_dto() {
        let json = r#"{
            "id": 123,
            "canonical_url": "https://mangabaka.org/123",
            "cover": {"x350": {"x1": "https://example.com/c.jpg"}},
            "status": "releasing",
            "type": "manga",
            "description": "<p>Summary</p>",
            "titles": [{"language": "en", "title": "Test", "traits": ["romanized"], "is_primary": true}],
            "source": {"anilist": {"id": 42}, "manga_updates": {"id": "abc"}},
            "tags_v2": [{"name": "Action", "is_genre": true}, {"name": "Nudity", "is_genre": false}],
            "published": {"start_date": "2018-01-02"}
        }"#;
        let series: MangaBakaSeriesDto = serde_json::from_str(json).unwrap();
        assert_eq!(series.id, 123);
        assert_eq!(series.status as i32, MangaBakaStatusDto::Releasing as i32);
        assert_eq!(
            series.cover.x350.as_ref().unwrap().x1.as_deref(),
            Some("https://example.com/c.jpg")
        );
        assert_eq!(
            series
                .source
                .anilist
                .as_ref()
                .unwrap()
                .id_string()
                .as_deref(),
            Some("42")
        );
        assert_eq!(
            series
                .source
                .manga_updates
                .as_ref()
                .unwrap()
                .id_string()
                .as_deref(),
            Some("abc")
        );
        assert_eq!(primary_title(&series), "Test");
    }
}
