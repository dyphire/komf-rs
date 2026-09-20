//! MyAnimeList provider —— 对应 `providers/mal` 包。
//!
//! 使用 MAL API v2（`https://api.myanimelist.net/v2`），需要 `X-MAL-CLIENT-ID`。
use crate::config::ProviderConfig;
use crate::model::{
    Author, AuthorRole, Image, MatchQuery, MediaType, ProviderBookId, ProviderBookMetadata,
    ProviderSeriesId, ProviderSeriesMetadata, SeriesMetadata, SeriesSearchResult, SeriesStatus,
    SeriesTitle, TitleType, WebLink,
};
use crate::providers::{CoreProviders, MetadataProvider, ProviderError};
use crate::util::NameSimilarityMatcher;
use serde::Deserialize;

const BASE_URL: &str = "https://api.myanimelist.net/v2";

// 注意：MAL API v2 返回 snake_case（main_picture / alternative_titles / media_type ...），
// 因此这里不能用 rename_all="camelCase"，字段名直接与 JSON 对齐。
// Kotlin MalClient.searchSeries: 仅请求 alternative_titles,media_type（搜索结果不拿全字段）。
const SEARCH_FIELDS: &str = "alternative_titles,media_type";
// Kotlin MalClient.getSeries: includeFields（mapper 实际用到的子集）。
const GET_FIELDS: &str = "id,title,main_picture,alternative_titles,start_date,end_date,synopsis,mean,rank,media_type,status,num_volumes,num_chapters,genres,authors{first_name,last_name},pictures";

// Kotlin MalMetadataProvider 的系列类型白名单（按 MediaType 选择）
const MANGA_MEDIA_TYPES: &[&str] = &["manga", "one_shot", "doujinshi", "manhwa", "manhua", "oel"];
const NOVEL_MEDIA_TYPES: &[&str] = &["novel", "light_novel"];
const WEBTOON_MEDIA_TYPES: &[&str] = &["manhua", "manhwa"];

/// 按 provider 的 MediaType 返回可接受的 MAL media_type 列表（Kotlin `seriesTypes`）。
fn series_types(media_type: MediaType) -> &'static [&'static str] {
    match media_type {
        MediaType::Novel => NOVEL_MEDIA_TYPES,
        MediaType::Webtoon => WEBTOON_MEDIA_TYPES,
        _ => MANGA_MEDIA_TYPES,
    }
}

/// MAL media_type 字符串 → MediaType（搜索结果显示用；未知类型 → None）。
fn mal_media_type_to_media_type(mt: Option<&str>) -> Option<MediaType> {
    match mt {
        Some("manga" | "one_shot" | "doujinshi" | "oel") => Some(MediaType::Manga),
        Some("novel" | "light_novel") => Some(MediaType::Novel),
        Some("manhua" | "manhwa") => Some(MediaType::Webtoon),
        _ => None,
    }
}

impl MalMetadataProvider {
    /// 库配置 mediaType 优先（Rust 扩展）：库类型映射失败时回退 provider 全局配置。
    fn effective_series_types(&self, media_type: Option<MediaType>) -> &'static [&'static str] {
        match media_type {
            Some(mt) => series_types(mt),
            None => series_types(self.media_type),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MalSearchResponse {
    pub data: Vec<MalSearchEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MalSearchEntry {
    pub node: MalManga,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MalManga {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub main_picture: Option<MalPicture>,
    #[serde(default)]
    pub alternative_titles: Option<MalAlternativeTitles>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub synopsis: Option<String>,
    pub mean: Option<f64>,
    pub rank: Option<i32>,
    pub status: Option<String>,
    pub num_volumes: Option<i32>,
    pub num_chapters: Option<i32>,
    #[serde(default)]
    pub genres: Vec<MalGenre>,
    #[serde(default)]
    pub authors: Vec<MalAuthor>,
    #[serde(default)]
    pub pictures: Vec<MalPicture>,
    #[serde(default)]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MalPicture {
    pub large: Option<String>,
    pub medium: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MalAlternativeTitles {
    pub synonyms: Option<Vec<String>>,
    pub en: Option<String>,
    pub ja: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MalGenre {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MalAuthor {
    pub node: MalAuthorNode,
    pub role: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MalAuthorNode {
    pub id: Option<i64>,
    #[serde(rename = "first_name")]
    pub first_name: Option<String>,
    #[serde(rename = "last_name")]
    pub last_name: Option<String>,
}

pub struct MalClient {
    http: reqwest::Client,
}

impl MalClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    pub async fn search(&self, name: &str) -> Result<Vec<MalManga>, ProviderError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/manga"))
            .query(&[
                ("q", name),
                ("fields", SEARCH_FIELDS),
                // Kotlin MalClient: parameter("nsfw", "true") —— 不过滤成人向作品
                // Kotlin searchSeries 不传 limit（MAL 默认 100）。
                ("nsfw", "true"),
            ])
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::Mal, status, body));
        }
        let body: MalSearchResponse = response.json().await?;
        Ok(body.data.into_iter().map(|e| e.node).collect())
    }

    pub async fn get(&self, id: u64) -> Result<MalManga, ProviderError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/manga/{id}"))
            .query(&[("fields", GET_FIELDS)])
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::Mal, status, body));
        }
        Ok(response.json().await?)
    }

    pub async fn get_thumbnail(&self, manga: &MalManga) -> Result<Option<Image>, ProviderError> {
        // Kotlin: series.mainPicture?.medium（不回退 pictures[].large）。
        let Some(url) = manga.main_picture.as_ref().and_then(|p| p.medium.clone()) else {
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

pub struct MalMetadataMapper {
    metadata_config: crate::config::SeriesMetadataConfig,
    author_roles: Vec<AuthorRole>,
    artist_roles: Vec<AuthorRole>,
}

impl MalMetadataMapper {
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
        manga: &MalManga,
        thumbnail: Option<Image>,
    ) -> ProviderSeriesMetadata {
        let cfg = &self.metadata_config;

        let mut titles: Vec<SeriesTitle> = Vec::new();
        if cfg.title {
            titles.push(SeriesTitle {
                name: manga.title.clone(),
                r#type: Some(TitleType::Romaji),
                language: None,
            });
            if let Some(en) = manga.alternative_titles.as_ref().and_then(|a| a.en.clone()) {
                titles.push(SeriesTitle {
                    name: en,
                    r#type: Some(TitleType::Localized),
                    language: Some("en".into()),
                });
            }
            if let Some(ja) = manga.alternative_titles.as_ref().and_then(|a| a.ja.clone()) {
                titles.push(SeriesTitle {
                    name: ja,
                    r#type: Some(TitleType::Native),
                    language: Some("ja".into()),
                });
            }
        }
        let title = titles.first().cloned();

        let status = if cfg.status {
            manga.status.as_deref().and_then(map_status)
        } else {
            None
        };
        let release_date = cfg
            .release_date
            .then(|| manga.start_date.as_deref().and_then(parse_mal_date))
            .flatten();

        let authors = if cfg.authors {
            map_authors(manga, &self.author_roles, &self.artist_roles)
        } else {
            Vec::new()
        };

        let links = if cfg.links {
            vec![WebLink {
                label: "MyAnimeList".to_string(),
                url: format!("https://myanimelist.net/manga/{}", manga.id),
            }]
        } else {
            Vec::new()
        };

        let metadata = SeriesMetadata {
            status,
            title,
            titles,
            summary: cfg.summary.then_some(manga.synopsis.clone()).flatten(),
            publisher: None,
            alternative_publishers: Vec::new(),
            reading_direction: None,
            age_rating: None,
            language: None,
            genres: if cfg.genres {
                manga.genres.iter().map(|g| g.name.clone()).collect()
            } else {
                Vec::new()
            },
            tags: Vec::new(),
            total_book_count: cfg.total_book_count.then_some(manga.num_volumes).flatten(),
            authors,
            release_date,
            links,
            score: cfg.score.then_some(manga.mean).flatten(),
            thumbnail,
        };

        ProviderSeriesMetadata {
            id: ProviderSeriesId(manga.id.to_string()),
            metadata,
            books: Vec::new(),
        }
    }

    pub fn to_series_search_result(&self, manga: &MalManga) -> SeriesSearchResult {
        SeriesSearchResult {
            url: Some(format!("https://myanimelist.net/manga/{}", manga.id)),
            image_url: manga.main_picture.as_ref().and_then(|p| p.medium.clone()),
            title: manga.title.clone(),
            provider: CoreProviders::Mal.as_str().to_string(),
            result_id: manga.id.to_string(),
            media_type: mal_media_type_to_media_type(manga.media_type.as_deref()),
            language: None,
            nsfw: None,
        }
    }
}

fn parse_mal_date(date: &str) -> Option<crate::model::ReleaseDate> {
    // MAL 日期格式: yyyy-mm-dd 或 yyyy-mm
    let parts: Vec<&str> = date.split('-').collect();
    let year = parts.first().and_then(|p| p.parse::<i32>().ok());
    let month = parts.get(1).and_then(|p| p.parse::<u32>().ok());
    let day = parts.get(2).and_then(|p| p.parse::<u32>().ok());
    Some(crate::model::ReleaseDate::new(year, month, day))
}

fn map_status(status: &str) -> Option<SeriesStatus> {
    match status.to_uppercase().as_str() {
        "FINISHED" => Some(SeriesStatus::Completed),
        "CURRENTLY_PUBLISHING" => Some(SeriesStatus::Ongoing),
        "NOT_YET_PUBLISHED" => Some(SeriesStatus::Ongoing),
        "ON_HIATUS" => Some(SeriesStatus::Hiatus),
        "DISCONTINUED" => Some(SeriesStatus::Abandoned),
        _ => None,
    }
}

fn map_authors(
    manga: &MalManga,
    author_roles: &[AuthorRole],
    artist_roles: &[AuthorRole],
) -> Vec<Author> {
    // Kotlin: 角色精确匹配 "Art"/"Story"/"Story & Art"（大小写敏感），展开全部 authorRoles/artistRoles；
    // name = "$firstName $lastName"。
    let mut authors = Vec::new();
    for author in &manga.authors {
        let name = match (&author.node.first_name, &author.node.last_name) {
            (Some(f), Some(l)) => format!("{f} {l}"),
            (Some(f), None) => f.clone(),
            (None, Some(l)) => l.clone(),
            (None, None) => continue,
        };
        match author.role.as_str() {
            "Art" => {
                authors.extend(artist_roles.iter().map(|r| Author {
                    name: name.clone(),
                    role: *r,
                }));
            }
            "Story" => {
                authors.extend(author_roles.iter().map(|r| Author {
                    name: name.clone(),
                    role: *r,
                }));
            }
            "Story & Art" => {
                authors.extend(artist_roles.iter().map(|r| Author {
                    name: name.clone(),
                    role: *r,
                }));
                authors.extend(author_roles.iter().map(|r| Author {
                    name: name.clone(),
                    role: *r,
                }));
            }
            _ => {}
        }
    }
    authors
}

pub struct MalMetadataProvider {
    client: MalClient,
    metadata_mapper: MalMetadataMapper,
    name_matcher: NameSimilarityMatcher,
    fetch_series_covers: bool,
    media_type: MediaType,
}

pub fn create_provider(
    config: &ProviderConfig,
    client_id: Option<&str>,
    default_name_matcher: NameSimilarityMatcher,
    _http_client: &reqwest::Client,
) -> Option<MalMetadataProvider> {
    if !config.enabled {
        return None;
    }
    let client_id = client_id?;
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(value) = reqwest::header::HeaderValue::from_str(client_id) {
        headers.insert("X-MAL-CLIENT-ID", value);
    }
    let client = crate::providers::client_with_default_headers(headers);

    let name_matcher = config.name_matching_mode.unwrap_or(default_name_matcher);
    Some(MalMetadataProvider {
        client: MalClient::new(client),
        metadata_mapper: MalMetadataMapper::new(
            config.series_metadata.clone(),
            config.author_roles.clone(),
            config.artist_roles.clone(),
        ),
        name_matcher,
        fetch_series_covers: config.series_metadata.thumbnail,
        media_type: config.media_type,
    })
}

#[async_trait::async_trait]
impl MetadataProvider for MalMetadataProvider {

    fn resolve_link_id(&self, query: &str) -> Option<String> {
        let re = regex::Regex::new(r"myanimelist\.net/(?:manga|anime)/(\d+)").ok()?;
        re.captures(query)
            .map(|c| c.get(1).unwrap().as_str().to_string())
    }
    fn provider_name(&self) -> CoreProviders {
        CoreProviders::Mal
    }

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        let id: u64 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid MAL series id: {}", series_id.0))
        })?;
        let manga = self.client.get(id).await?;
        let thumbnail = if self.fetch_series_covers {
            self.client.get_thumbnail(&manga).await?
        } else {
            None
        };
        Ok(self.metadata_mapper.to_series_metadata(&manga, thumbnail))
    }

    async fn get_series_cover(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError> {
        let id: u64 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid MAL series id: {}", series_id.0))
        })?;
        let manga = self.client.get(id).await?;
        self.client.get_thumbnail(&manga).await
    }

    async fn get_book_metadata(
        &self,
        _series_id: &ProviderSeriesId,
        _book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError> {
        Err(ProviderError::message(
            "MAL provider does not support book metadata",
        ))
    }

    async fn search_series(
        &self,
        series_name: &str,
        limit: usize,
        media_type: Option<crate::model::MediaType>,
    ) -> Result<Vec<SeriesSearchResult>, ProviderError> {
        // Kotlin MalMetadataProvider.searchSeries: <3 字符拒绝搜索
        if series_name.chars().count() < 3 {
            tracing::warn!("{series_name} is less than 3 characters. Can't perform a search");
            return Ok(Vec::new());
        }
        // Kotlin: seriesName.take(64)；MAL 搜索不传 limit（默认 100），再按 mediaType 过滤后 take(limit)
        let name: String = series_name.chars().take(64).collect();
        let allowed = self.effective_series_types(media_type);
        let results = self.client.search(&name).await?;
        Ok(results
            .iter()
            .filter(|m| {
                m.media_type
                    .as_deref()
                    .is_some_and(|t| allowed.contains(&t))
            })
            .take(limit)
            .map(|m| self.metadata_mapper.to_series_search_result(m))
            .collect())
    }

    async fn match_series_metadata(
        &self,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        // Kotlin MalMetadataProvider.matchSeriesMetadata
        if match_query.series_name.chars().count() < 3 {
            tracing::warn!(
                "{} is less than 3 characters. Can't perform a search",
                match_query.series_name
            );
            return Ok(None);
        }
        let series_name: String = match_query.series_name.chars().take(64).collect();
        let allowed = self.effective_series_types(match_query.media_type);
        // Kotlin: 先用稀疏搜索结果匹配，再 getSeries(id) 取全量。
        let results = self.client.search(&series_name).await?;
        let matched_id = results
            .iter()
            .find(|m| {
                if !m
                    .media_type
                    .as_deref()
                    .is_some_and(|t| allowed.contains(&t))
                {
                    return false;
                }
                let mut titles = vec![m.title.clone()];
                if let Some(alts) = &m.alternative_titles {
                    titles.extend(alts.synonyms.clone().unwrap_or_default());
                    if let Some(en) = &alts.en {
                        titles.push(en.clone());
                    }
                    if let Some(ja) = &alts.ja {
                        titles.push(ja.clone());
                    }
                }
                self.name_matcher.matches(
                    &match_query.normalized_series_name(),
                    &match_query.normalize_titles(&titles),
                )
            })
            .map(|m| m.id);
        let Some(matched_id) = matched_id else {
            return Ok(None);
        };

        let manga = self.client.get(matched_id).await?;
        let thumbnail = if self.fetch_series_covers {
            self.client.get_thumbnail(&manga).await?
        } else {
            None
        };
        Ok(Some(
            self.metadata_mapper.to_series_metadata(&manga, thumbnail),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping_covers_mal_strings() {
        assert_eq!(map_status("FINISHED"), Some(SeriesStatus::Completed));
        assert_eq!(
            map_status("CURRENTLY_PUBLISHING"),
            Some(SeriesStatus::Ongoing)
        );
        assert_eq!(map_status("NOT_YET_PUBLISHED"), Some(SeriesStatus::Ongoing));
        assert_eq!(map_status("ON_HIATUS"), Some(SeriesStatus::Hiatus));
        assert_eq!(map_status("DISCONTINUED"), Some(SeriesStatus::Abandoned));
    }

    #[test]
    fn mal_media_type_mapping() {
        assert_eq!(
            mal_media_type_to_media_type(Some("manga")),
            Some(MediaType::Manga)
        );
        assert_eq!(
            mal_media_type_to_media_type(Some("one_shot")),
            Some(MediaType::Manga)
        );
        assert_eq!(
            mal_media_type_to_media_type(Some("novel")),
            Some(MediaType::Novel)
        );
        assert_eq!(
            mal_media_type_to_media_type(Some("light_novel")),
            Some(MediaType::Novel)
        );
        assert_eq!(
            mal_media_type_to_media_type(Some("manhwa")),
            Some(MediaType::Webtoon)
        );
        assert_eq!(mal_media_type_to_media_type(None), None);
    }

    #[test]
    fn author_node_name_and_exact_role() {
        let json = r#"{"id":1,"title":"x","authors":[{"node":{"id":10,"first_name":"Eiichiro","last_name":"Oda"},"role":"Story & Art"},{"node":{"id":11,"first_name":"Other","last_name":"Guy"},"role":"Story"},{"node":{"id":12,"first_name":"No","last_name":"Match"},"role":"Screenplay"}]}"#;
        let manga: MalManga = serde_json::from_str(json).unwrap();
        let out = map_authors(&manga, &[AuthorRole::Writer], &[AuthorRole::Penciller]);
        // "Story & Art" -> Penciller + Writer；"Story" -> Writer；Screenplay 忽略。
        let names: Vec<_> = out.iter().map(|a| (a.name.as_str(), a.role)).collect();
        assert_eq!(names[0], ("Eiichiro Oda", AuthorRole::Penciller));
        assert_eq!(names[1], ("Eiichiro Oda", AuthorRole::Writer));
        assert_eq!(names[2], ("Other Guy", AuthorRole::Writer));
        assert_eq!(names.len(), 3);
    }
}
