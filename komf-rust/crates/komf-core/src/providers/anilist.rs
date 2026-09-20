//! AniList provider —— 对应 `providers/anilist` 包。
//!
//! 使用 AniList GraphQL API（`https://graphql.anilist.co`）。
use crate::config::AniListConfig;
use crate::model::{
    Author, AuthorRole, Image, MatchQuery, MediaType, ProviderBookId, ProviderBookMetadata,
    ProviderSeriesId, ProviderSeriesMetadata, SeriesMetadata, SeriesSearchResult, SeriesStatus,
    SeriesTitle, TitleType, WebLink,
};
use crate::providers::{CoreProviders, MetadataProvider, ProviderError};
use crate::util::NameSimilarityMatcher;
use serde::{Deserialize, Serialize};

const GRAPHQL_URL: &str = "https://graphql.anilist.co";

const SEARCH_QUERY: &str = r#"
query ($search: String, $type: MediaType, $perPage: Int, $formats: [MediaFormat!]!) {
  mediaSearch: Page(page: 1, perPage: $perPage) {
    media(search: $search, type: $type, format_in: $formats) {
      id
      title { romaji english native }
      coverImage { large extraLarge }
      startDate { year month day }
      bannerImage
      status
      description(asHtml: false)
      volumes
      chapters
      genres
      meanScore
      siteUrl
      staff { edges { role node { name { full } languageV2 } } }
      studios { edges { node { name } isMain } }
      tags { name rank }
    }
  }
}
"#;

const GET_QUERY: &str = r#"
query ($id: Int) {
  Media(id: $id, type: MANGA) {
    id
    title { romaji english native }
    coverImage { large extraLarge }
    startDate { year month day }
    bannerImage
    status
    description(asHtml: false)
    volumes
    chapters
    genres
    meanScore
    siteUrl
    staff { edges { role node { name { full } languageV2 } } }
    studios { edges { node { name } isMain } }
    tags { name rank }
  }
}
"#;

#[derive(Debug, Serialize)]
struct GraphQlRequest {
    query: String,
    variables: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct GraphQlResponse {
    data: Option<serde_json::Value>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AniListMediaSearchResponse {
    pub media_search: AniListMediaSearchMedia,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AniListMediaSearchMedia {
    pub media: Vec<AniListMedia>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AniListMedia {
    pub id: u64,
    pub title: AniListTitle,
    #[serde(default)]
    pub cover_image: Option<AniListCoverImage>,
    #[serde(default)]
    pub start_date: AniListDate,
    pub banner_image: Option<String>,
    pub status: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub volumes: Option<i32>,
    #[serde(default)]
    pub chapters: Option<i32>,
    #[serde(default)]
    pub genres: Vec<String>,
    pub mean_score: Option<i32>,
    pub site_url: Option<String>,
    #[serde(default)]
    pub staff: AniListStaffConnection,
    #[serde(default)]
    pub studios: AniListStudioConnection,
    #[serde(default)]
    pub tags: Vec<AniListTag>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AniListTitle {
    pub romaji: Option<String>,
    pub english: Option<String>,
    pub native: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AniListCoverImage {
    pub large: Option<String>,
    pub extra_large: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AniListDate {
    pub year: Option<i32>,
    pub month: Option<i32>,
    pub day: Option<i32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AniListStaffConnection {
    #[serde(default)]
    pub edges: Vec<AniListStaffEdge>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AniListStaffEdge {
    #[serde(default)]
    pub role: Option<String>,
    pub node: AniListStaff,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AniListStaff {
    #[serde(default)]
    pub name: AniListStaffName,
    #[serde(default)]
    pub language_v2: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AniListStaffName {
    #[serde(default)]
    pub full: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AniListStudioConnection {
    #[serde(default)]
    pub edges: Vec<AniListStudioEdge>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AniListStudioEdge {
    pub node: AniListStudio,
    pub is_main: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AniListStudio {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AniListTag {
    pub name: String,
    #[serde(default)]
    pub rank: Option<i32>,
}

pub struct AniListClient {
    http: reqwest::Client,
}

impl AniListClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    async fn execute(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value, ProviderError> {
        let response = self
            .http
            .post(GRAPHQL_URL)
            .json(&GraphQlRequest {
                query: query.to_string(),
                variables,
            })
            .send()
            .await?;
        let status = response.status();
        let body: GraphQlResponse = response.json().await?;
        if !status.is_success() {
            let message = body
                .errors
                .map(|e| {
                    e.iter()
                        .map(|err| err.message.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            return Err(ProviderError::Status(
                CoreProviders::Anilist,
                status,
                message,
            ));
        }
        body.data
            .ok_or_else(|| ProviderError::message("AniList returned no data"))
    }

    /// Kotlin AniListMetadataProvider.seriesFormats：MANGA/WEBTOON → [MANGA, ONE_SHOT]；
    /// NOVEL → [NOVEL]；COMIC → Kotlin 构造时抛 IllegalStateException（provider 不可用）。
    /// Rust 用空列表模拟"不可用"（搜索返回空，不崩溃）。
    pub async fn search_series(
        &self,
        name: &str,
        limit: u32,
        formats: &[String],
    ) -> Result<Vec<AniListMedia>, ProviderError> {
        let data = self
            .execute(
                SEARCH_QUERY,
                serde_json::json!({
                    "search": name,
                    "type": "MANGA",
                    "perPage": limit,
                    "formats": formats,
                }),
            )
            .await?;
        let page: AniListMediaSearchResponse = serde_json::from_value(data)?;
        Ok(page.media_search.media)
    }

    pub async fn get_series(&self, id: u64) -> Result<AniListMedia, ProviderError> {
        let data = self
            .execute(GET_QUERY, serde_json::json!({ "id": id }))
            .await?;
        let media: AniListMedia = serde_json::from_value(
            data.get("Media")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )?;
        Ok(media)
    }

    pub async fn get_thumbnail(
        &self,
        media: &AniListMedia,
    ) -> Result<Option<Image>, ProviderError> {
        let Some(url) = media
            .cover_image
            .as_ref()
            .and_then(|c| c.extra_large.clone().or_else(|| c.large.clone()))
        else {
            return Ok(None);
        };
        let response = self.http.get(&url).send().await?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let bytes = response.bytes().await?;
        Ok(Some(Image::new(bytes.to_vec(), None)))
    }
}

pub struct AniListMetadataMapper {
    metadata_config: crate::config::SeriesMetadataConfig,
    author_roles: Vec<AuthorRole>,
    artist_roles: Vec<AuthorRole>,
    tags_size_limit: usize,
    tags_score_threshold: i32,
}

impl AniListMetadataMapper {
    pub fn new(
        metadata_config: crate::config::SeriesMetadataConfig,
        author_roles: Vec<AuthorRole>,
        artist_roles: Vec<AuthorRole>,
        tags_size_limit: i32,
        tags_score_threshold: i32,
    ) -> Self {
        Self {
            metadata_config,
            author_roles,
            artist_roles,
            tags_size_limit: tags_size_limit.max(0) as usize,
            tags_score_threshold,
        }
    }

    pub fn to_series_metadata(
        &self,
        media: &AniListMedia,
        thumbnail: Option<Image>,
    ) -> ProviderSeriesMetadata {
        let cfg = &self.metadata_config;

        // Kotlin: english(LOCALIZED/en) 在前，romaji(ROMAJI/ja-ro) 次之，native(NATIVE/ja) 最后。
        let mut titles: Vec<SeriesTitle> = Vec::new();
        if let Some(english) = media.title.english.clone() {
            titles.push(SeriesTitle {
                name: english,
                r#type: Some(TitleType::Localized),
                language: Some("en".into()),
            });
        }
        if let Some(romaji) = media.title.romaji.clone() {
            titles.push(SeriesTitle {
                name: romaji,
                r#type: Some(TitleType::Romaji),
                language: Some("ja-ro".into()),
            });
        }
        if let Some(native) = media.title.native.clone() {
            titles.push(SeriesTitle {
                name: native,
                r#type: Some(TitleType::Native),
                language: Some("ja".into()),
            });
        }
        // Kotlin MetadataConfigApplier.seriesTitles：title 关闭时保留标题名但清空 type/language。
        if !cfg.title {
            for t in titles.iter_mut() {
                t.r#type = None;
                t.language = None;
            }
        }
        let title = cfg.title.then(|| titles.first().cloned()).flatten();

        let status = if cfg.status {
            media.status.as_deref().and_then(map_status)
        } else {
            None
        };

        let tags = if cfg.tags {
            // Kotlin: filter rank >= threshold → sortedByDescending(rank) → take(tagsSizeLimit) → name
            let mut ranked: Vec<(i32, String)> = media
                .tags
                .iter()
                .filter_map(|t| t.rank.map(|rank| (rank, t.name.clone())))
                .filter(|(rank, _)| *rank >= self.tags_score_threshold)
                .collect();
            ranked.sort_by(|a, b| b.0.cmp(&a.0));
            ranked.truncate(self.tags_size_limit);
            ranked.into_iter().map(|(_, name)| name).collect()
        } else {
            Vec::new()
        };

        let authors = if cfg.authors {
            map_authors(media, &self.author_roles, &self.artist_roles)
        } else {
            Vec::new()
        };

        let release_date = cfg.release_date.then(|| {
            crate::model::ReleaseDate::new(
                media.start_date.year,
                media.start_date.month.map(|m| m as u32),
                media.start_date.day.map(|d| d as u32),
            )
        });

        let links = if cfg.links {
            media
                .site_url
                .as_ref()
                .map(|url| WebLink {
                    label: "AniList".to_string(),
                    url: url.clone(),
                })
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };

        let total_book_count = cfg.total_book_count.then_some(media.volumes).flatten();
        let score = cfg
            .score
            .then(|| media.mean_score.map(|s| s as f64 / 10.0))
            .flatten();

        let metadata = SeriesMetadata {
            status,
            title,
            titles,
            summary: cfg
                .summary
                .then(|| media.description.as_deref().map(strip_html))
                .flatten(),
            publisher: None,
            alternative_publishers: Vec::new(),
            reading_direction: None,
            age_rating: None,
            language: None,
            genres: if cfg.genres {
                media.genres.clone()
            } else {
                Vec::new()
            },
            tags,
            total_book_count,
            authors,
            release_date,
            links,
            score,
            thumbnail,
        };

        ProviderSeriesMetadata {
            id: ProviderSeriesId(media.id.to_string()),
            metadata,
            books: Vec::new(),
        }
    }

    pub fn to_series_search_result(&self, media: &AniListMedia) -> SeriesSearchResult {
        // Kotlin: title = english ?: romaji ?: native；imageUrl = extraLarge；url = 构造 anilist.co/manga/{id}
        SeriesSearchResult {
            url: Some(format!("https://anilist.co/manga/{}", media.id)),
            image_url: media
                .cover_image
                .as_ref()
                .and_then(|c| c.extra_large.clone()),
            title: media
                .title
                .english
                .clone()
                .or_else(|| media.title.romaji.clone())
                .or_else(|| media.title.native.clone())
                .unwrap_or_default(),
            provider: CoreProviders::Anilist.as_str().to_string(),
            result_id: media.id.to_string(),
            media_type: None,
            language: None,
            nsfw: None,
        }
    }
}

/// Kotlin allowedRoles：角色去括号后必须命中白名单才收录。
const ALLOWED_ROLES: [&str; 6] = [
    "Story & Art",
    "Story",
    "Original Story",
    "Original Creator",
    "Art",
    "Illustration",
];

fn strip_parenthesized(role: &str) -> String {
    // Kotlin extractNameAndRole: role.replace("\\([^)]*\\)".toRegex(), "")
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\([^)]*\)").unwrap());
    re.replace_all(role, "").into_owned()
}

/// 对应 Kotlin `Ksoup.parse(description).wholeText()`：去除 HTML 标签并解码常见实体。
fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for c in input.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

fn map_status(status: &str) -> Option<SeriesStatus> {
    match status.to_uppercase().as_str() {
        "FINISHED" => Some(SeriesStatus::Completed),
        "RELEASING" => Some(SeriesStatus::Ongoing),
        "NOT_YET_RELEASED" => Some(SeriesStatus::Ongoing),
        "HIATUS" => Some(SeriesStatus::Hiatus),
        "CANCELLED" => Some(SeriesStatus::Abandoned),
        _ => None,
    }
}

/// Kotlin AniListMetadataMapper.toSeriesMetadata 的作者展开：
/// 角色去括号后必须在 allowedRoles 白名单；"Story & Art" → artistRoles + authorRoles 各映射一个；
/// "Story"/"Original Story"/"Original Creator" → authorRoles；"Art"/"Illustration" → artistRoles。
fn map_authors(
    media: &AniListMedia,
    author_roles: &[AuthorRole],
    artist_roles: &[AuthorRole],
) -> Vec<Author> {
    media
        .staff
        .edges
        .iter()
        .filter_map(|edge| {
            let role = edge.role.as_deref()?;
            let role = strip_parenthesized(role).trim().to_string();
            if !ALLOWED_ROLES.contains(&role.as_str()) {
                return None;
            }
            let name = edge.node.name.full.clone();
            let authors: Vec<Author> = match role.as_str() {
                "Story & Art" => artist_roles
                    .iter()
                    .map(|r| Author {
                        name: name.clone(),
                        role: *r,
                    })
                    .chain(author_roles.iter().map(|r| Author {
                        name: name.clone(),
                        role: *r,
                    }))
                    .collect(),
                "Story" | "Original Story" | "Original Creator" => author_roles
                    .iter()
                    .map(|r| Author {
                        name: name.clone(),
                        role: *r,
                    })
                    .collect(),
                "Art" | "Illustration" => artist_roles
                    .iter()
                    .map(|r| Author {
                        name: name.clone(),
                        role: *r,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Some(authors)
        })
        .flatten()
        .collect()
}

pub struct AniListMetadataProvider {
    client: AniListClient,
    metadata_mapper: AniListMetadataMapper,
    name_matcher: NameSimilarityMatcher,
    fetch_series_covers: bool,
    media_type: MediaType,
    /// Kotlin `seriesFormats`：MANGA/WEBTOON → [MANGA, ONE_SHOT]；NOVEL → [NOVEL]；COMIC → 空（不可用）
    series_formats: Vec<String>,
}

/// Kotlin `mangaMediaFormats` / `novelMediaFormats`。
fn series_formats_for(media_type: MediaType) -> Vec<String> {
    match media_type {
        MediaType::Novel => vec!["NOVEL".to_string()],
        MediaType::Comic => Vec::new(),
        _ => vec!["MANGA".to_string(), "ONE_SHOT".to_string()],
    }
}

impl AniListMetadataProvider {
    /// 库配置 mediaType 优先（Rust 扩展）：库类型映射为空（COMIC）时回退 provider 全局配置。
    fn effective_formats(&self, media_type: Option<MediaType>) -> Vec<String> {
        match media_type {
            Some(mt) => {
                let formats = series_formats_for(mt);
                if formats.is_empty() {
                    self.series_formats.clone()
                } else {
                    formats
                }
            }
            None => self.series_formats.clone(),
        }
    }
}

pub fn create_provider(
    config: &AniListConfig,
    default_name_matcher: NameSimilarityMatcher,
    http_client: &reqwest::Client,
) -> Option<AniListMetadataProvider> {
    if !config.enabled {
        return None;
    }
    let name_matcher = config.name_matching_mode.unwrap_or(default_name_matcher);
    Some(AniListMetadataProvider {
        client: AniListClient::new(http_client.clone()),
        metadata_mapper: AniListMetadataMapper::new(
            config.series_metadata.clone(),
            config.author_roles.clone(),
            config.artist_roles.clone(),
            config.tags_size_limit,
            config.tags_score_threshold,
        ),
        name_matcher,
        fetch_series_covers: config.series_metadata.thumbnail,
        media_type: config.media_type,
        series_formats: series_formats_for(config.media_type),
    })
}

#[async_trait::async_trait]
impl MetadataProvider for AniListMetadataProvider {

    fn resolve_link_id(&self, query: &str) -> Option<String> {
        let re = regex::Regex::new(r"anilist\.co/(?:manga|anime)/(\d+)").ok()?;
        re.captures(query)
            .map(|c| c.get(1).unwrap().as_str().to_string())
    }
    fn provider_name(&self) -> CoreProviders {
        CoreProviders::Anilist
    }

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        let id: u64 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid AniList series id: {}", series_id.0))
        })?;
        let media = self.client.get_series(id).await?;
        let thumbnail = if self.fetch_series_covers {
            self.client.get_thumbnail(&media).await?
        } else {
            None
        };
        Ok(self.metadata_mapper.to_series_metadata(&media, thumbnail))
    }

    async fn get_series_cover(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError> {
        let id: u64 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid AniList series id: {}", series_id.0))
        })?;
        let media = self.client.get_series(id).await?;
        self.client.get_thumbnail(&media).await
    }

    async fn get_book_metadata(
        &self,
        _series_id: &ProviderSeriesId,
        _book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError> {
        // AniList 不提供独立卷元数据（对应 Kotlin 中该 provider 不实现书籍元数据）
        Err(ProviderError::message(
            "AniList provider does not support book metadata",
        ))
    }

    async fn search_series(
        &self,
        series_name: &str,
        limit: usize,
        media_type: Option<crate::model::MediaType>,
    ) -> Result<Vec<SeriesSearchResult>, ProviderError> {
        // Kotlin: seriesName.take(400)
        let name: String = series_name.chars().take(400).collect();
        let media = self
            .client
            .search_series(&name, limit as u32, &self.effective_formats(media_type))
            .await?;
        Ok(media
            .iter()
            .map(|m| self.metadata_mapper.to_series_search_result(m))
            .collect())
    }

    async fn match_series_metadata(
        &self,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        // Kotlin COMIC 在构造期抛 IllegalStateException（provider 不可用）→ Rust 用 None 近似
        if matches!(self.media_type, MediaType::Comic) || self.series_formats.is_empty() {
            return Ok(None);
        }
        let name: String = match_query.series_name.chars().take(400).collect();
        let media = self
            .client
            .search_series(&name, 10, &self.effective_formats(match_query.media_type))
            .await?;
        let matched = media.into_iter().find(|m| {
            let titles: Vec<String> = vec![
                m.title.english.clone(),
                m.title.romaji.clone(),
                m.title.native.clone(),
            ]
            .into_iter()
            .flatten()
            .collect();
            self.name_matcher.matches(
                &match_query.normalized_series_name(),
                &match_query.normalize_titles(&titles),
            )
        });
        let Some(media) = matched else {
            return Ok(None);
        };

        let thumbnail = if self.fetch_series_covers {
            self.client.get_thumbnail(&media).await?
        } else {
            None
        };
        Ok(Some(
            self.metadata_mapper.to_series_metadata(&media, thumbnail),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实 AniList 搜索响应（2026-09 抓取）：`data.mediaSearch.media` 别名 + staff name 嵌套。
    #[test]
    fn search_response_parses_media_search_and_staff_name() {
        let json = r#"{"data":{"mediaSearch":{"media":[{"id":30013,"title":{"romaji":"ONE PIECE"},"staff":{"edges":[{"role":"Story & Art","node":{"name":{"full":"Eiichirou Oda"},"languageV2":"Japanese"}}]}}]}}}"#;
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let response: AniListMediaSearchResponse =
            serde_json::from_value(value.get("data").cloned().unwrap()).unwrap();
        let media = &response.media_search.media[0];
        assert_eq!(media.id, 30013);
        let edge = &media.staff.edges[0];
        assert_eq!(edge.role.as_deref(), Some("Story & Art"));
        assert_eq!(edge.node.name.full, "Eiichirou Oda");
        assert_eq!(edge.node.language_v2.as_deref(), Some("Japanese"));
    }

    /// 单个 Media 响应（GET 查询）：`data.Media`。
    #[test]
    fn get_response_parses_media() {
        let json = r#"{"data":{"Media":{"id":30013,"title":{"romaji":"ONE PIECE"},"coverImage":{"extraLarge":"https://s4.anilist.co/file/anilistcdn/media/anime/cover/large/bx30013-Iv1t71hTX5wC.png"},"staff":{"edges":[]}}}}"#;
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let media: AniListMedia =
            serde_json::from_value(value.get("data").unwrap().get("Media").cloned().unwrap())
                .unwrap();
        assert_eq!(media.id, 30013);
        assert!(media.staff.edges.is_empty());
    }

    fn mapper() -> AniListMetadataMapper {
        AniListMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            15,
            60,
        )
    }

    #[test]
    fn titles_order_english_first_and_romaji_language() {
        let json = r#"{"id":1,"title":{"romaji":"ROMAJI","english":"English Title","native":"ネイティブ"}}"#;
        let media: AniListMedia = serde_json::from_str(json).unwrap();
        let out = mapper().to_series_metadata(&media, None);
        let t: Vec<_> = out
            .metadata
            .titles
            .iter()
            .map(|t| (t.name.as_str(), t.r#type, t.language.as_deref()))
            .collect();
        assert_eq!(
            t[0],
            ("English Title", Some(TitleType::Localized), Some("en"))
        );
        assert_eq!(t[1], ("ROMAJI", Some(TitleType::Romaji), Some("ja-ro")));
        assert_eq!(t[2], ("ネイティブ", Some(TitleType::Native), Some("ja")));
        assert_eq!(out.metadata.title.as_ref().unwrap().name, "English Title");
    }

    #[test]
    fn score_uses_mean_score_divided_by_ten() {
        let json = r#"{"id":1,"title":{"romaji":"x"},"meanScore":75}"#;
        let media: AniListMedia = serde_json::from_str(json).unwrap();
        let out = mapper().to_series_metadata(&media, None);
        assert_eq!(out.metadata.score, Some(7.5));
    }

    #[test]
    fn status_not_yet_released_maps_to_ongoing() {
        assert_eq!(map_status("NOT_YET_RELEASED"), Some(SeriesStatus::Ongoing));
        assert_eq!(map_status("FINISHED"), Some(SeriesStatus::Completed));
    }

    #[test]
    fn tags_without_rank_are_skipped_and_threshold_applied() {
        let json = r#"{"id":1,"title":{"romaji":"x"},"tags":[{"name":"no_rank"},{"name":"low","rank":10},{"name":"ok","rank":90}]}"#;
        let media: AniListMedia = serde_json::from_str(json).unwrap();
        let out = mapper().to_series_metadata(&media, None);
        assert_eq!(out.metadata.tags, vec!["ok".to_string()]);
    }

    #[test]
    fn summary_html_is_stripped() {
        assert_eq!(strip_html("<p>Hello <b>world</b></p>"), "Hello world");
        assert_eq!(strip_html("&lt;escaped&gt; &amp; more"), "<escaped> & more");
    }

    #[test]
    fn staff_edge_with_null_role_is_ignored() {
        let json = r#"{"id":1,"title":{"romaji":"x"},"staff":{"edges":[{"role":null,"node":{"name":{"full":"Nobody"}}},{"role":"Story","node":{"name":{"full":"Writer Guy"}}}]}}"#;
        let media: AniListMedia = serde_json::from_str(json).unwrap();
        let out = mapper().to_series_metadata(&media, None);
        let names: Vec<_> = out
            .metadata
            .authors
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, vec!["Writer Guy"]);
    }
}
