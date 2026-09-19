//! MangaUpdates provider -- 对应 Kotlin `providers/mangaupdates` 包。

use serde::{Deserialize, Serialize};

use crate::config::ProviderConfig;
use crate::model::{
    Author, AuthorRole, Image, MatchQuery, MediaType, ProviderBookId, ProviderBookMetadata,
    ProviderSeriesId, ProviderSeriesMetadata, Publisher, PublisherType, SeriesMetadata,
    SeriesSearchResult, SeriesStatus, SeriesTitle, TitleType, WebLink,
};
use crate::providers::ProviderError;
use crate::providers::{CoreProviders, MetadataProvider};
use crate::util::NameSimilarityMatcher;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SeriesImage {
    pub url: Option<ImageUrl>,
    pub volume: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ImageUrl {
    pub original: Option<String>,
    pub thumb: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Genre {
    pub genre: String,
}

pub const BASE_URL: &str = "https://api.mangaupdates.com/v1";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SearchResultPage {
    pub total_hits: i64,
    pub page: u32,
    pub per_page: u32,
    pub results: Vec<SearchResultHit>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SearchResultHit {
    pub record: SeriesRecord,
    #[serde(default)]
    pub hit_title: Option<String>,
}

/// Kotlin `SearchResult`：series_id/title/description/image/genres/year/url。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SeriesRecord {
    pub series_id: u64,
    pub title: String,
    pub description: Option<String>,
    pub image: Option<SeriesImage>,
    pub genres: Vec<Genre>,
    pub year: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MangaUpdatesSeries {
    pub series_id: u64,
    pub title: String,
    pub description: Option<String>,
    pub year: Option<String>,
    pub url: Option<String>,
    pub image: Option<SeriesImage>,
    /// 真实 API 中为字符串，如 `"115 Volumes (Ongoing)"`。
    pub status: Option<String>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub licensed: Option<bool>,
    pub completed: Option<bool>,
    pub associated: Vec<Associated>,
    pub genres: Vec<Genre>,
    pub categories: Vec<Category>,
    pub authors: Vec<SeriesAuthor>,
    pub publishers: Vec<SeriesPublisher>,
    pub recommendations: Vec<serde_json::Value>,
    pub bayesian_rating: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Associated {
    pub title: String,
    #[serde(rename = "type")]
    pub type_: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Category {
    pub category: String,
    pub votes: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SeriesAuthor {
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SeriesPublisher {
    /// 真实 API 字段名为 `publisher_name`。
    pub publisher_name: String,
    #[serde(rename = "type")]
    pub r#type: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SeriesType {
    #[serde(rename = "Manga")]
    Manga,
    #[serde(rename = "Novel")]
    Novel,
    #[serde(rename = "Artbook")]
    Artbook,
    #[serde(rename = "Doujinshi")]
    Doujinshi,
    #[serde(rename = "Manhua")]
    Manhua,
    #[serde(rename = "Manhwa")]
    Manhwa,
    #[serde(rename = "OEL")]
    Oel,
    #[serde(rename = "Filipino")]
    Filipino,
    #[serde(rename = "Indonesian")]
    Indonesian,
    #[serde(rename = "Thai")]
    Thai,
    #[serde(rename = "Vietnamese")]
    Vietnamese,
    #[serde(rename = "Malaysian")]
    Malaysian,
    #[serde(rename = "Nordic")]
    Nordic,
    #[serde(rename = "French")]
    French,
    #[serde(rename = "Spanish")]
    Spanish,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchRequest {
    search: String,
    page: u32,
    perpage: u32,
    #[serde(rename = "type")]
    r#type: Vec<SeriesType>,
}

pub struct MangaUpdatesClient {
    http: reqwest::Client,
}

impl MangaUpdatesClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    pub async fn search_series(
        &self,
        name: &str,
        types: &[SeriesType],
        page: u32,
        per_page: u32,
    ) -> Result<SearchResultPage, ProviderError> {
        let request = SearchRequest {
            search: name.chars().take(400).collect(),
            page,
            perpage: per_page,
            r#type: types.to_vec(),
        };
        let response = self
            .http
            .post(format!("{BASE_URL}/series/search"))
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(
                CoreProviders::MangaUpdates,
                status,
                body,
            ));
        }
        let mut page: SearchResultPage = response.json().await?;
        for result in page.results.iter_mut() {
            result.record.title = html_unescape(&result.record.title);
            result.record.year = result.record.year.as_deref().map(take_last_year);
        }
        Ok(page)
    }

    pub async fn get_series(&self, series_id: u64) -> Result<MangaUpdatesSeries, ProviderError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/series/{series_id}"))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(
                CoreProviders::MangaUpdates,
                status,
                body,
            ));
        }
        let mut series: MangaUpdatesSeries = response.json().await?;
        series.title = html_unescape(&series.title)
            .trim_end_matches(" (Novel)")
            .to_string();
        series.description = series.description.as_deref().map(parse_description);
        for assoc in series.associated.iter_mut() {
            assoc.title = html_unescape(&assoc.title);
        }
        for genre in series.genres.iter_mut() {
            genre.genre = html_unescape(&genre.genre);
        }
        for category in series.categories.iter_mut() {
            category.category = html_unescape(&category.category);
        }
        for author in series.authors.iter_mut() {
            author.name = html_unescape(&author.name);
            author.r#type = html_unescape(&author.r#type);
        }
        for publisher in series.publishers.iter_mut() {
            publisher.publisher_name = html_unescape(&publisher.publisher_name);
            publisher.r#type = html_unescape(&publisher.r#type);
        }
        series.year = series.year.as_deref().map(take_last_year);
        Ok(series)
    }

    pub async fn get_thumbnail(
        &self,
        series: &MangaUpdatesSeries,
    ) -> Result<Option<Image>, ProviderError> {
        let Some(url) = series
            .image
            .as_ref()
            .and_then(|i| i.url.as_ref())
            .and_then(|u| u.original.clone())
        else {
            return Ok(None);
        };
        let response = self.http.get(&url).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Ok(None);
        }
        let bytes = response.bytes().await?;
        Ok(Some(Image::new(bytes.to_vec(), detect_mime(&url))))
    }
}

fn detect_mime(url: &str) -> Option<String> {
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

/// 去除 HTML 实体 —— 对应 Kotlin jsoup.unescapeEntities。
fn html_unescape(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
}

/// 解析 description HTML —— 对应 Kotlin MangaUpdatesClient.parseDescription。
fn parse_description(description: &str) -> String {
    let mut result = String::new();
    let mut rest = description;
    let mut first = true;
    while !rest.is_empty() {
        // 找到标签起点与剩余部分。
        let (text, remainder) = match rest.find('<') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, ""),
        };
        if !text.is_empty() {
            if !first {
                result.push('\n');
            }
            result.push_str(&html_unescape(text));
            first = false;
        }
        if let Some(end) = remainder.find('>') {
            let tag = &remainder[1..end];
            if tag.eq_ignore_ascii_case("br") || tag.eq_ignore_ascii_case("p") {
                first = false;
                result.push('\n');
            }
            rest = &remainder[end + 1..];
        } else {
            break;
        }
    }
    result.trim().to_string()
}

fn take_last_year(year: &str) -> String {
    let regex = regex::Regex::new(r"-[0-9]+$").unwrap();
    regex.replace(year, "").to_string()
}

// ---------------------------------------------------------------------------
// Metadata mapper -- 对应 Kotlin MangaUpdatesMetadataMapper.kt
// ---------------------------------------------------------------------------

pub struct MangaUpdatesMetadataMapper {
    metadata_config: crate::config::SeriesMetadataConfig,
    author_roles: Vec<AuthorRole>,
    artist_roles: Vec<AuthorRole>,
}

impl MangaUpdatesMetadataMapper {
    pub fn new(
        metadata_config: crate::config::SeriesMetadataConfig,
        author_roles: Vec<AuthorRole>,
        artist_roles: Vec<AuthorRole>,
    ) -> Self {
        Self {
            metadata_config,
            author_roles,
            artist_roles,
        }
    }

    pub fn to_series_metadata(
        &self,
        series: &MangaUpdatesSeries,
        thumbnail: Option<Image>,
    ) -> ProviderSeriesMetadata {
        let cfg = &self.metadata_config;

        // Kotlin: SeriesTitle(series.title, ROMAJI, "ja-ro")
        let title = SeriesTitle {
            name: series.title.clone(),
            r#type: Some(TitleType::Romaji),
            language: Some("ja-ro".to_string()),
        };
        let title_field = cfg.title.then_some(title.clone());

        let status = series.status.as_deref().and_then(map_status);
        let status_field = cfg.status.then_some(status).flatten();

        let summary = cfg.summary.then_some(series.description.clone()).flatten();
        let genres: Vec<String> = if cfg.genres {
            series.genres.iter().map(|g| g.genre.clone()).collect()
        } else {
            Vec::new()
        };
        // Kotlin: categories.sortedByDescending { it.votes }.take(15)
        let tags: Vec<String> = if cfg.tags {
            let mut categories: Vec<&Category> = series.categories.iter().collect();
            categories.sort_by(|a, b| b.votes.cmp(&a.votes));
            categories
                .iter()
                .take(15)
                .map(|c| c.category.clone())
                .collect()
        } else {
            Vec::new()
        };

        let authors = if cfg.authors {
            map_authors(series, &self.author_roles, &self.artist_roles)
        } else {
            Vec::new()
        };

        // Kotlin: Original/English 出版社过滤 + useOriginalPublisher 选择 + alternative 减 publisher
        let mut original_publishers: Vec<Publisher> = series
            .publishers
            .iter()
            .filter(|p| p.r#type == "Original")
            .map(|p| Publisher {
                name: p.publisher_name.clone(),
                r#type: Some(PublisherType::Original),
                language_tag: None,
            })
            .collect();
        dedup_publishers(&mut original_publishers);
        let mut english_publishers: Vec<Publisher> = series
            .publishers
            .iter()
            .filter(|p| p.r#type == "English")
            .map(|p| Publisher {
                name: p.publisher_name.clone(),
                r#type: Some(PublisherType::Localized),
                language_tag: Some("en".to_string()),
            })
            .collect();
        dedup_publishers(&mut english_publishers);
        let (publisher, alternative_publishers): (Option<Publisher>, Vec<Publisher>) =
            if cfg.publisher {
                let selected = if cfg.use_original_publisher {
                    original_publishers.first()
                } else {
                    english_publishers
                        .first()
                        .or_else(|| original_publishers.first())
                };
                let mut all: Vec<Publisher> = original_publishers
                    .iter()
                    .chain(english_publishers.iter())
                    .cloned()
                    .collect();
                if let Some(selected) = selected {
                    all.retain(|candidate| {
                        !(candidate.name == selected.name
                            && candidate.r#type == selected.r#type
                            && candidate.language_tag == selected.language_tag)
                    });
                }
                (selected.cloned(), all)
            } else {
                (None, Vec::new())
            };

        let release_date = cfg
            .release_date
            .then(|| {
                series
                    .year
                    .as_deref()
                    .and_then(|y| y.trim().parse::<i32>().ok())
                    .map(|year| crate::model::ReleaseDate::new(Some(year), None, None))
            })
            .flatten();

        let links = if cfg.links {
            series
                .url
                .as_ref()
                .map(|url| WebLink {
                    label: "MangaUpdates".to_string(),
                    url: url.clone(),
                })
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };

        // Kotlin mangaupdates 不设置 totalBookCount（保持 None）。
        let total_book_count: Option<i32> = None;

        // Kotlin titles: [main] + associated；title 禁用时保留全部仅清空 type/language
        let mut titles = vec![title.clone()];
        titles.extend(series.associated.iter().map(|assoc| SeriesTitle {
            name: assoc.title.clone(),
            r#type: None,
            language: None,
        }));
        if !cfg.title {
            for t in titles.iter_mut() {
                t.r#type = None;
                t.language = None;
            }
        }

        let metadata = SeriesMetadata {
            status: status_field,
            title: title_field,
            titles,
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
            links,
            // Kotlin: score = series.bayesianRating（受 cfg.score 裁剪）
            score: if cfg.score {
                series.bayesian_rating
            } else {
                None
            },
            thumbnail,
        };

        // Kotlin: MangaUpdates 不提供 books（ProviderSeriesMetadata.books 默认空）
        let books = Vec::new();

        ProviderSeriesMetadata {
            id: ProviderSeriesId(series.series_id.to_string()),
            metadata,
            books,
        }
    }

    pub fn to_series_search_result(&self, record: &SeriesRecord) -> SeriesSearchResult {
        SeriesSearchResult {
            url: Some(record.url.clone()),
            image_url: record
                .image
                .as_ref()
                .and_then(|i| i.url.as_ref())
                .and_then(|u| u.original.clone()),
            title: record.title.clone(),
            provider: CoreProviders::MangaUpdates.as_str().to_string(),
            result_id: record.series_id.to_string(),
            media_type: None,
            language: None,
        }
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

fn map_status(status: &str) -> Option<SeriesStatus> {
    // Kotlin parseStatus: 提取所有 "(...)" 组，全部包含第一组时按大写映射
    let groups: Vec<String> = parenthesized_groups(status);
    let first = groups.first()?;
    if !groups.iter().all(|g| g.contains(first.as_str())) {
        return None;
    }
    match first.to_uppercase().as_str() {
        "COMPLETE" => Some(SeriesStatus::Ended),
        "ONGOING" => Some(SeriesStatus::Ongoing),
        "CANCELLED" => Some(SeriesStatus::Abandoned),
        "HIATUS" => Some(SeriesStatus::Hiatus),
        _ => None,
    }
}

fn parenthesized_groups(input: &str) -> Vec<String> {
    let mut groups = Vec::new();
    let mut chars = input.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '(' {
            let rest = &input[i + 1..];
            if let Some(end_rel) = rest.find(')') {
                groups.push(rest[..end_rel].to_string());
            }
        }
    }
    groups
}

fn map_authors(
    series: &MangaUpdatesSeries,
    author_roles: &[AuthorRole],
    artist_roles: &[AuthorRole],
) -> Vec<Author> {
    series
        .authors
        .iter()
        .flat_map(|author| {
            // Kotlin: it.type == "Author" -> authorRoles 全量；else -> artistRoles 全量
            let roles = if author.r#type == "Author" {
                author_roles
            } else {
                artist_roles
            };
            roles
                .iter()
                .map(|role| Author {
                    name: author.name.clone(),
                    role: *role,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Provider -- 对应 Kotlin MangaUpdatesMetadataProvider.kt
// ---------------------------------------------------------------------------

pub struct MangaUpdatesMetadataProvider {
    client: MangaUpdatesClient,
    metadata_mapper: MangaUpdatesMetadataMapper,
    name_matcher: NameSimilarityMatcher,
    fetch_series_covers: bool,
    series_types: Vec<SeriesType>,
}

pub fn create_provider(
    config: &ProviderConfig,
    default_name_matcher: NameSimilarityMatcher,
    http_client: &reqwest::Client,
) -> Option<MangaUpdatesMetadataProvider> {
    if !config.enabled {
        return None;
    }
    let series_types = match config.media_type {
        MediaType::Manga => vec![
            SeriesType::Manga,
            SeriesType::Manhwa,
            SeriesType::Manhua,
            SeriesType::Artbook,
            SeriesType::Doujinshi,
            SeriesType::Filipino,
            SeriesType::Indonesian,
            SeriesType::Thai,
            SeriesType::Vietnamese,
            SeriesType::Malaysian,
            SeriesType::Oel,
            SeriesType::Nordic,
            SeriesType::French,
            SeriesType::Spanish,
        ],
        MediaType::Novel => vec![SeriesType::Novel],
        MediaType::Comic => return None, // Kotlin: throw IllegalStateException
        MediaType::Webtoon => vec![
            SeriesType::Manhwa,
            SeriesType::Manhua,
            SeriesType::Filipino,
            SeriesType::Indonesian,
            SeriesType::Thai,
            SeriesType::Vietnamese,
            SeriesType::Malaysian,
            SeriesType::Oel,
            SeriesType::Nordic,
            SeriesType::French,
            SeriesType::Spanish,
        ],
    };

    let name_matcher = config.name_matching_mode.unwrap_or(default_name_matcher);
    Some(MangaUpdatesMetadataProvider {
        client: MangaUpdatesClient::new(http_client.clone()),
        metadata_mapper: MangaUpdatesMetadataMapper::new(
            config.series_metadata.clone(),
            config.author_roles.clone(),
            config.artist_roles.clone(),
        ),
        name_matcher,
        fetch_series_covers: config.series_metadata.thumbnail,
        series_types,
    })
}

#[async_trait::async_trait]
impl MetadataProvider for MangaUpdatesMetadataProvider {
    fn provider_name(&self) -> CoreProviders {
        CoreProviders::MangaUpdates
    }

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        let id: u64 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid MangaUpdates series id: {}", series_id.0))
        })?;
        let series = self.client.get_series(id).await?;
        let thumbnail = if self.fetch_series_covers {
            self.client.get_thumbnail(&series).await?
        } else {
            None
        };
        Ok(self.metadata_mapper.to_series_metadata(&series, thumbnail))
    }

    async fn get_series_cover(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError> {
        let id: u64 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid MangaUpdates series id: {}", series_id.0))
        })?;
        let series = self.client.get_series(id).await?;
        self.client.get_thumbnail(&series).await
    }

    async fn get_book_metadata(
        &self,
        _series_id: &ProviderSeriesId,
        _book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError> {
        // Kotlin: throw UnsupportedOperationException()
        Err(ProviderError::message(
            "MangaUpdates provider does not support book metadata",
        ))
    }

    async fn search_series(
        &self,
        series_name: &str,
        limit: usize,
        _media_type: Option<crate::model::MediaType>,
    ) -> Result<Vec<SeriesSearchResult>, ProviderError> {
        let page = self
            .client
            .search_series(series_name, &self.series_types, 1, limit as u32)
            .await?;
        Ok(page
            .results
            .into_iter()
            .take(limit)
            .map(|r| self.metadata_mapper.to_series_search_result(&r.record))
            .collect())
    }

    async fn match_series_metadata(
        &self,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        // Kotlin: searchSeries(name, types) 默认 perPage=5（非 10）
        let page = self
            .client
            .search_series(&match_query.series_name, &self.series_types, 1, 5)
            .await?;
        let record = page.results.into_iter().find(|r| {
            let title = r.record.title.trim_end_matches(" (Novel)").to_string();
            self.name_matcher.matches_single(
                &match_query.normalized_series_name(),
                &match_query.normalize_title(&title),
            )
        });
        let Some(record) = record else {
            return Ok(None);
        };

        let series = self.client.get_series(record.record.series_id).await?;
        let thumbnail = if self.fetch_series_covers {
            self.client.get_thumbnail(&series).await?
        } else {
            None
        };
        Ok(Some(
            self.metadata_mapper.to_series_metadata(&series, thumbnail),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_description() {
        let html = "<div><p>Story about pirates.</p><br>Second line</div>";
        let parsed = parse_description(html);
        assert!(parsed.contains("Story about pirates."));
        assert!(parsed.contains("Second line"));
    }

    #[test]
    fn unescapes_entities() {
        assert_eq!(html_unescape("A &amp; B &#39;test&#39;"), "A & B 'test'");
    }

    /// 真实 API 响应解析（依赖 `komf-rust/.smoke-komga/search.json`，联调时用）。
    #[test]
    #[ignore]
    fn debug_parse_real_search_response() {
        let raw =
            std::fs::read_to_string(r"G:\Github\komf\komf-rust\.smoke-komga\search.json").unwrap();
        let page: SearchResultPage = serde_json::from_str(&raw).unwrap();
        assert!(!page.results.is_empty());
        assert!(page.total_hits > 0);
        assert!(page.results[0].record.series_id > 0);
    }

    /// 真实 API 系列详情解析 + Kotlin 映射行为验证（依赖 `komf-rust/.smoke-komga/series.json`，联调时用）。
    #[test]
    #[ignore]
    fn debug_parse_real_series_response() {
        let raw =
            std::fs::read_to_string(r"G:\Github\komf\komf-rust\.smoke-komga\series.json").unwrap();
        let series: MangaUpdatesSeries = serde_json::from_str(&raw).unwrap();
        assert_eq!(series.series_id, 55099564912);
        assert!(series.status.is_some());
        assert!(!series.authors.is_empty());
        assert!(!series.publishers.is_empty());
        // 新 DTO 字段：真实响应均有
        assert_eq!(series.bayesian_rating, Some(8.89));
        assert!(series.categories.iter().any(|c| c.votes > 0));
        // Kotlin parseStatus：真实 status "115 Volumes (Ongoing)" -> Ongoing
        assert_eq!(
            map_status(series.status.as_deref().unwrap()),
            Some(SeriesStatus::Ongoing)
        );
        // 出版社映射：英文优先（useOriginalPublisher=false 默认）
        let meta = test_mapper(crate::config::SeriesMetadataConfig::default())
            .to_series_metadata(&series, None);
        if let Some(publisher) = meta.metadata.publisher.as_ref() {
            assert_eq!(publisher.r#type, Some(PublisherType::Localized));
            assert_eq!(publisher.language_tag.as_deref(), Some("en"));
        }
        // tags: 按 votes 降序取前 15
        assert_eq!(meta.metadata.tags.len(), 15);
        let mut sorted: Vec<&Category> = series.categories.iter().collect();
        sorted.sort_by(|a, b| b.votes.cmp(&a.votes));
        assert_eq!(
            meta.metadata.tags,
            sorted
                .iter()
                .take(15)
                .map(|c| c.category.clone())
                .collect::<Vec<_>>()
        );
        // 主标题 language = ja-ro
        assert_eq!(meta.metadata.titles[0].language.as_deref(), Some("ja-ro"));
        // score 透出 bayesian rating
        assert_eq!(meta.metadata.score, Some(8.89));
    }

    fn test_mapper(cfg: crate::config::SeriesMetadataConfig) -> MangaUpdatesMetadataMapper {
        MangaUpdatesMetadataMapper::new(
            cfg,
            vec![AuthorRole::Writer, AuthorRole::Cover],
            vec![AuthorRole::Penciller, AuthorRole::Colorist],
        )
    }

    #[test]
    fn map_status_matches_kotlin_parse_status() {
        // Kotlin parseStatus: 提取括号词 uppercase 后 Status.valueOf -> ENDED/ONGOING/ABANDONED/HIATUS
        assert_eq!(
            map_status("115 Volumes (Ongoing)"),
            Some(SeriesStatus::Ongoing)
        );
        assert_eq!(
            map_status("30 Volumes (Complete)"),
            Some(SeriesStatus::Ended)
        );
        assert_eq!(map_status("(Cancelled)"), Some(SeriesStatus::Abandoned));
        assert_eq!(map_status("(Hiatus)"), Some(SeriesStatus::Hiatus));
        assert_eq!(map_status("Completed"), None); // 无括号 -> Kotlin null
        assert_eq!(map_status("(Ongoing) (Other)"), None); // 不全含第一组 -> Kotlin null
    }

    #[test]
    fn tags_sorted_by_votes_take_15() {
        let mut series = MangaUpdatesSeries::default();
        for i in (1..=20).rev() {
            series.categories.push(Category {
                category: format!("cat{i}"),
                votes: i as i64,
            });
        }
        let meta = test_mapper(crate::config::SeriesMetadataConfig::default())
            .to_series_metadata(&series, None);
        assert_eq!(meta.metadata.tags.len(), 15);
        assert_eq!(meta.metadata.tags[0], "cat20"); // 最高票在前
        assert_eq!(meta.metadata.tags[14], "cat6");
    }

    #[test]
    fn publisher_selection_matches_kotlin() {
        let mut series = MangaUpdatesSeries::default();
        series.publishers = vec![
            SeriesPublisher {
                publisher_name: "OrigPub".into(),
                r#type: "Original".into(),
            },
            SeriesPublisher {
                publisher_name: "EngPub1".into(),
                r#type: "English".into(),
            },
            SeriesPublisher {
                publisher_name: "EngPub2".into(),
                r#type: "English".into(),
            },
            SeriesPublisher {
                publisher_name: "Other".into(),
                r#type: "Something".into(),
            },
        ];
        // 默认 useOriginalPublisher=false: English 优先
        let meta = test_mapper(crate::config::SeriesMetadataConfig::default())
            .to_series_metadata(&series, None);
        assert_eq!(
            meta.metadata.publisher.as_ref().map(|p| p.name.as_str()),
            Some("EngPub1")
        );
        assert_eq!(
            meta.metadata
                .publisher
                .as_ref()
                .and_then(|p| p.language_tag.as_deref()),
            Some("en")
        );
        let alternatives: Vec<&str> = meta
            .metadata
            .alternative_publishers
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(alternatives, vec!["OrigPub", "EngPub2"]);
        // useOriginalPublisher=true: Original 优先
        let mut cfg = crate::config::SeriesMetadataConfig::default();
        cfg.use_original_publisher = true;
        let meta = test_mapper(cfg).to_series_metadata(&series, None);
        assert_eq!(
            meta.metadata.publisher.as_ref().map(|p| p.name.as_str()),
            Some("OrigPub")
        );
        // 仅 Original: 回退 Original
        let mut series2 = MangaUpdatesSeries::default();
        series2.publishers = vec![SeriesPublisher {
            publisher_name: "OnlyOrig".into(),
            r#type: "Original".into(),
        }];
        let meta = test_mapper(crate::config::SeriesMetadataConfig::default())
            .to_series_metadata(&series2, None);
        assert_eq!(
            meta.metadata.publisher.as_ref().map(|p| p.name.as_str()),
            Some("OnlyOrig")
        );
        // cfg.publisher=false: 全空
        let mut cfg2 = crate::config::SeriesMetadataConfig::default();
        cfg2.publisher = false;
        let meta = test_mapper(cfg2).to_series_metadata(&series, None);
        assert!(meta.metadata.publisher.is_none());
        assert!(meta.metadata.alternative_publishers.is_empty());
    }

    #[test]
    fn authors_mapped_by_exact_type() {
        let mut series = MangaUpdatesSeries::default();
        series.authors = vec![
            SeriesAuthor {
                name: "WriterPerson".into(),
                r#type: "Author".into(),
            },
            SeriesAuthor {
                name: "ArtistPerson".into(),
                r#type: "Artist".into(),
            },
        ];
        let meta = test_mapper(crate::config::SeriesMetadataConfig::default())
            .to_series_metadata(&series, None);
        let pairs: Vec<(String, AuthorRole)> = meta
            .metadata
            .authors
            .iter()
            .map(|a| (a.name.clone(), a.role))
            .collect();
        // WriterPerson -> authorRoles 全量；ArtistPerson -> artistRoles 全量
        assert_eq!(pairs.len(), 4);
        assert_eq!(pairs[0], ("WriterPerson".to_string(), AuthorRole::Writer));
        assert_eq!(pairs[1], ("WriterPerson".to_string(), AuthorRole::Cover));
        assert_eq!(
            pairs[2],
            ("ArtistPerson".to_string(), AuthorRole::Penciller)
        );
        assert_eq!(pairs[3], ("ArtistPerson".to_string(), AuthorRole::Colorist));
    }

    #[test]
    fn titles_match_kotlin_shape() {
        let mut series = MangaUpdatesSeries::default();
        series.title = "Main Series".into();
        series.associated = vec![Associated {
            title: "Alt Title".into(),
            type_: None,
        }];
        let meta = test_mapper(crate::config::SeriesMetadataConfig::default())
            .to_series_metadata(&series, None);
        assert_eq!(meta.metadata.titles.len(), 2);
        assert_eq!(meta.metadata.titles[0].name, "Main Series");
        assert_eq!(meta.metadata.titles[0].r#type, Some(TitleType::Romaji));
        assert_eq!(meta.metadata.titles[0].language.as_deref(), Some("ja-ro"));
        assert_eq!(meta.metadata.titles[1].name, "Alt Title");
        assert_eq!(meta.metadata.titles[1].r#type, None);
        assert_eq!(meta.metadata.titles[1].language, None);
        // title 禁用时保留 titles 仅清空 type/language（Kotlin seriesTitles）
        let mut cfg = crate::config::SeriesMetadataConfig::default();
        cfg.title = false;
        let meta = test_mapper(cfg).to_series_metadata(&series, None);
        assert_eq!(meta.metadata.titles.len(), 2);
        assert!(meta
            .metadata
            .titles
            .iter()
            .all(|t| t.r#type.is_none() && t.language.is_none()));
        assert!(meta.metadata.title.is_none());
    }

    #[test]
    fn search_result_uses_original_image() {
        let mut record = SeriesRecord::default();
        record.image = Some(SeriesImage {
            url: Some(ImageUrl {
                original: Some("orig".into()),
                thumb: Some("thumb".into()),
            }),
            volume: None,
        });
        let result = test_mapper(crate::config::SeriesMetadataConfig::default())
            .to_series_search_result(&record);
        assert_eq!(result.image_url.as_deref(), Some("orig"));
    }

    #[test]
    fn score_comes_from_bayesian_rating() {
        let mut series = MangaUpdatesSeries::default();
        series.bayesian_rating = Some(7.89);
        let meta = test_mapper(crate::config::SeriesMetadataConfig::default())
            .to_series_metadata(&series, None);
        assert_eq!(meta.metadata.score, Some(7.89));
        let mut cfg = crate::config::SeriesMetadataConfig::default();
        cfg.score = false;
        let meta = test_mapper(cfg).to_series_metadata(&series, None);
        assert_eq!(meta.metadata.score, None);
    }
}
