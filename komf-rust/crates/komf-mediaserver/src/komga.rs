//! Komga REST 客户端 —— 对应 Kotlin 中由 `io.github.sndr:komga-client` 库提供、
//! 经 `KomgaMediaServerClientAdapter` 适配的能力。
//!
//! 本移植直接基于 Komga REST API v1 实现，使用 Basic Auth。
use crate::client::{MediaServerClient, MediaServerError};
use crate::config::AlternateTitleLabelsConfig;
use crate::model::*;
use komf_core::model::{Image, ReadingDirection, SeriesStatus, TitleType, WebLink};
use serde::{Deserialize, Serialize};

const API_PREFIX: &str = "/api/v1";

// ---------------------------------------------------------------------------
// Komga DTO（响应）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaSeriesDto {
    pub id: String,
    pub library_id: String,
    pub name: String,
    pub books_count: i32,
    pub metadata: KomgaSeriesMetadataDto,
    /// komga SeriesDto.booksMetadata：系列聚合书籍元数据（含书籍级 links 并集）
    #[serde(default)]
    pub books_metadata: Option<KomgaBookMetadataAggregationDto>,
    pub url: String,
    #[serde(default)]
    pub deleted: bool,
    /// oneshot 单本系列（mylar 导出文件名区分用）。
    #[serde(default)]
    pub oneshot: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaBookMetadataAggregationDto {
    #[serde(default)]
    pub links: Vec<KomgaWebLinkDto>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaSeriesMetadataDto {
    pub status: Option<String>,
    pub status_lock: bool,
    pub title: String,
    pub title_lock: bool,
    pub title_sort: String,
    pub title_sort_lock: bool,
    #[serde(default)]
    pub alternate_titles: Vec<KomgaAlternateTitleDto>,
    pub alternate_titles_lock: bool,
    pub summary: String,
    pub summary_lock: bool,
    pub reading_direction: Option<String>,
    pub reading_direction_lock: bool,
    pub publisher: Option<String>,
    pub publisher_lock: bool,
    pub age_rating: Option<i32>,
    pub age_rating_lock: bool,
    pub language: Option<String>,
    pub language_lock: bool,
    pub genres: Vec<String>,
    pub genres_lock: bool,
    pub tags: Vec<String>,
    pub tags_lock: bool,
    pub total_book_count: Option<i32>,
    pub total_book_count_lock: bool,
    pub links: Vec<KomgaWebLinkDto>,
    pub links_lock: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaAlternateTitleDto {
    pub label: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaWebLinkDto {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaCollectionDto {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub series_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaPage<T> {
    pub content: Vec<T>,
    pub number: i32,
    pub total_elements: i64,
    pub total_pages: i32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaThumbnailDto {
    pub id: String,
    pub r#type: String,
    pub selected: bool,
    #[serde(default)]
    pub series_id: Option<String>,
    #[serde(default)]
    pub book_id: Option<String>,
    #[serde(default)]
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaBookDto {
    pub id: String,
    pub series_id: String,
    pub library_id: Option<String>,
    pub series_title: String,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub file_name: String,
    pub number: i32,
    pub oneshot: bool,
    pub metadata: KomgaBookMetadataDto,
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaBookMetadataDto {
    pub title: String,
    pub title_lock: bool,
    pub summary: String,
    pub summary_lock: bool,
    pub number: Option<String>,
    pub number_lock: bool,
    /// Komga 返回数值（float）；映射时按 Kotlin `Float.toString()` 语义转字符串。
    pub number_sort: f64,
    pub number_sort_lock: bool,
    pub release_date: Option<String>,
    pub release_date_lock: bool,
    #[serde(default)]
    pub authors: Vec<KomgaAuthorDto>,
    pub authors_lock: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    pub tags_lock: bool,
    pub isbn: Option<String>,
    pub isbn_lock: bool,
    #[serde(default)]
    pub links: Vec<KomgaWebLinkDto>,
    pub links_lock: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaAuthorDto {
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaLibraryDto {
    pub id: String,
    pub name: String,
    pub root: String,
}

// ---------------------------------------------------------------------------
// Komga 更新请求（PatchValue 语义：省略=不修改，null=清空，值=设置）
// ---------------------------------------------------------------------------

/// 对应 Kotlin komga-client 的 `PatchValue`。
/// `None` 表示 Unset（序列化时省略字段）；`Some(None)` 表示显式 null；`Some(Some(v))` 表示值。
pub type PatchValue<T> = Option<Option<T>>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaSeriesMetadataUpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: PatchValue<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: PatchValue<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_sort: PatchValue<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternate_titles: PatchValue<Vec<KomgaAlternateTitleDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: PatchValue<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: PatchValue<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading_direction: PatchValue<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_rating: PatchValue<Option<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: PatchValue<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genres: PatchValue<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: PatchValue<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_book_count: PatchValue<Option<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: PatchValue<Vec<KomgaWebLinkDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_sort_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternate_titles_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading_direction_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_rating_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genres_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_book_count_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links_lock: PatchValue<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KomgaBookMetadataUpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: PatchValue<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: PatchValue<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: PatchValue<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number_sort: PatchValue<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_date: PatchValue<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors: PatchValue<Vec<KomgaAuthorDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: PatchValue<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isbn: PatchValue<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: PatchValue<Vec<KomgaWebLinkDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number_sort_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_date_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isbn_lock: PatchValue<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links_lock: PatchValue<bool>,
}

// ---------------------------------------------------------------------------
// Komga 客户端
// ---------------------------------------------------------------------------

pub struct KomgaClient {
    http: reqwest::Client,
    base_uri: String,
    thumbnail_size_limit: u64,
    alternate_title_labels: AlternateTitleLabelsConfig,
}

impl KomgaClient {
    pub fn new(
        base_uri: &str,
        username: &str,
        password: &str,
        api_key: &str,
        thumbnail_size_limit: u64,
        alternate_title_labels: AlternateTitleLabelsConfig,
    ) -> Result<Self, MediaServerError> {
        let base_uri = base_uri.trim_end_matches('/').to_string();
        let mut headers = reqwest::header::HeaderMap::new();
        if !api_key.is_empty() {
            // Komga API key：`X-API-Key` 请求头，优先于 Basic Auth。
            if let Ok(value) = reqwest::header::HeaderValue::from_str(api_key) {
                headers.insert("X-API-Key", value);
            }
        } else {
            let credentials = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                format!("{username}:{password}"),
            );
            if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Basic {credentials}")) {
                headers.insert(reqwest::header::AUTHORIZATION, value);
            }
        }
        if let Ok(value) = reqwest::header::HeaderValue::from_str("dyphire/komf-rs") {
            headers.insert(reqwest::header::USER_AGENT, value);
        }
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(MediaServerError::Http)?;
        Ok(Self {
            http,
            base_uri,
            thumbnail_size_limit,
            alternate_title_labels,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_uri, path)
    }

    /// 打开 Komga 事件流（SSE，`/sse/v1/events`）。
    pub async fn events_stream(&self) -> Result<reqwest::Response, MediaServerError> {
        self.http
            .get(self.url("/sse/v1/events"))
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(MediaServerError::Http)
    }

    async fn send_json<T: for<'de> Deserialize<'de>>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, MediaServerError> {
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(response.json().await?)
    }

    async fn get_series_dto(&self, series_id: &str) -> Result<KomgaSeriesDto, MediaServerError> {
        self.send_json(self.http.get(self.url(&format!("{API_PREFIX}/series/{series_id}")))).await
    }

    async fn get_book_dto(&self, book_id: &str) -> Result<KomgaBookDto, MediaServerError> {
        self.send_json(self.http.get(self.url(&format!("{API_PREFIX}/books/{book_id}")))).await
    }

    fn to_series(&self, dto: &KomgaSeriesDto) -> MediaServerSeries {
        let metadata = &dto.metadata;
        MediaServerSeries {
            id: MediaServerSeriesId(dto.id.clone()),
            library_id: MediaServerLibraryId(dto.library_id.clone()),
            name: dto.name.clone(),
            books_count: dto.books_count,
            metadata: MediaServerSeriesMetadata {
                status: metadata.status.as_deref().and_then(map_komga_status),
                title: metadata.title.clone(),
                title_sort: metadata.title_sort.clone(),
                alternative_titles: metadata
                    .alternate_titles
                    .iter()
                    .map(|t| MediaServerAlternativeTitle {
                        label: t.label.clone(),
                        title: t.title.clone(),
                    })
                    .collect(),
                summary: metadata.summary.clone(),
                reading_direction: metadata.reading_direction.as_deref().and_then(map_komga_reading_direction),
                publisher: metadata.publisher.clone(),
                alternative_publishers: Vec::new(),
                age_rating: metadata.age_rating,
                language: metadata.language.clone(),
                genres: metadata.genres.clone(),
                tags: metadata.tags.clone(),
                total_book_count: metadata.total_book_count,
                authors: Vec::new(),
                release_year: None,
                links: metadata.links.iter().map(|l| WebLink { label: l.label.clone(), url: l.url.clone() }).collect(),
                status_lock: metadata.status_lock,
                title_lock: metadata.title_lock,
                title_sort_lock: metadata.title_sort_lock,
                alternative_titles_lock: metadata.alternate_titles_lock,
                summary_lock: metadata.summary_lock,
                reading_direction_lock: metadata.reading_direction_lock,
                publisher_lock: metadata.publisher_lock,
                age_rating_lock: metadata.age_rating_lock,
                language_lock: metadata.language_lock,
                genres_lock: metadata.genres_lock,
                tags_lock: metadata.tags_lock,
                total_book_count_lock: metadata.total_book_count_lock,
                authors_lock: false,
                release_year_lock: false,
                links_lock: metadata.links_lock,
            },
            books_metadata_links: dto
                .books_metadata
                .as_ref()
                .map(|agg| {
                    agg.links
                        .iter()
                        .map(|l| WebLink {
                            label: l.label.clone(),
                            url: l.url.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            url: dto.url.clone(),
            deleted: dto.deleted,
            oneshot: dto.oneshot,
        }
    }

    fn to_book(&self, dto: &KomgaBookDto) -> MediaServerBook {
        let metadata = &dto.metadata;
        MediaServerBook {
            id: MediaServerBookId(dto.id.clone()),
            series_id: MediaServerSeriesId(dto.series_id.clone()),
            library_id: dto.library_id.clone().map(MediaServerLibraryId),
            series_title: dto.series_title.clone(),
            name: dto.name.clone(),
            url: dto.url.clone(),
            // komga BookDto.fileName 在部分版本返回空 → 回退从 url（完整文件路径）取 file_stem
            file_name: if dto.file_name.is_empty() {
                std::path::Path::new(&dto.url)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            } else {
                dto.file_name.clone()
            },
            number: dto.number,
            oneshot: dto.oneshot,
            metadata: MediaServerBookMetadata {
                title: metadata.title.clone(),
                summary: metadata.summary.clone(),
                number: metadata.number.clone().unwrap_or_default(),
                number_sort: Some(kotlin_float_string(metadata.number_sort)),
                release_date: metadata.release_date.clone(),
                authors: metadata.authors.iter().map(|a| MediaServerAuthor { name: a.name.clone(), role: a.role.clone() }).collect(),
                tags: metadata.tags.clone(),
                isbn: metadata.isbn.clone(),
                links: metadata.links.iter().map(|l| WebLink { label: l.label.clone(), url: l.url.clone() }).collect(),
                title_lock: metadata.title_lock,
                summary_lock: metadata.summary_lock,
                number_lock: metadata.number_lock,
                number_sort_lock: metadata.number_sort_lock,
                release_date_lock: metadata.release_date_lock,
                authors_lock: metadata.authors_lock,
                tags_lock: metadata.tags_lock,
                isbn_lock: metadata.isbn_lock,
                links_lock: metadata.links_lock,
            },
            deleted: dto.deleted,
        }
    }
}

fn map_komga_status(status: &str) -> Option<SeriesStatus> {
    match status {
        "ENDED" => Some(SeriesStatus::Ended),
        "ONGOING" => Some(SeriesStatus::Ongoing),
        "ABANDONED" => Some(SeriesStatus::Abandoned),
        "HIATUS" => Some(SeriesStatus::Hiatus),
        _ => None,
    }
}

/// 模拟 Kotlin `Float.toString()`（整数加 `.0`，其余最短表示），用于 numberSort。
fn kotlin_float_string(value: f64) -> String {
    if value.is_finite() && value == value.trunc() {
        format!("{:.1}", value)
    } else {
        value.to_string()
    }
}

fn map_komga_reading_direction(direction: &str) -> Option<ReadingDirection> {
    match direction {
        "LEFT_TO_RIGHT" => Some(ReadingDirection::LeftToRight),
        "RIGHT_TO_LEFT" => Some(ReadingDirection::RightToLeft),
        "VERTICAL" => Some(ReadingDirection::Vertical),
        "WEBTOON" => Some(ReadingDirection::Webtoon),
        _ => None,
    }
}

/// 系列元数据重置请求 —— 对应 Kotlin `seriesMetadataResetRequest`。
/// Kotlin 的 `PatchValue.None` 表示“不修改”（序列化时省略该字段），
/// 而非显式 null；只有 `Some("")` / `Some([])` 才真正清空。
/// 重置时若字段已锁定（lock=true）则不发送该字段（保持值 + 保持锁定）；
/// 未锁定字段正常重置。lock 字段本身一律不发送，Komga 现有 lock 状态不变。
/// 注意：Kotlin 原版 reset 请求发 Some(false) 会解锁所有字段，本实现为改进（用户要求尊重 lock）。
fn unless_locked<T>(locked: bool, value: Option<Option<T>>) -> Option<Option<T>> {
    if locked { None } else { value }
}

pub fn series_metadata_reset_request(
    name: &str,
    metadata: &MediaServerSeriesMetadata,
) -> KomgaSeriesMetadataUpdateRequest {
    KomgaSeriesMetadataUpdateRequest {
        status: unless_locked(metadata.status_lock, Some(Some(Some("ONGOING".to_string())))),
        title: unless_locked(metadata.title_lock, Some(Some(name.to_string()))),
        title_sort: unless_locked(metadata.title_sort_lock, Some(Some(name.to_string()))),
        alternate_titles: unless_locked(metadata.alternative_titles_lock, Some(None)),
        summary: unless_locked(metadata.summary_lock, Some(Some(String::new()))),
        publisher: unless_locked(metadata.publisher_lock, Some(Some(String::new()))),
        reading_direction: unless_locked(metadata.reading_direction_lock, Some(None)),
        age_rating: unless_locked(metadata.age_rating_lock, Some(None)),
        language: unless_locked(metadata.language_lock, Some(Some(String::new()))),
        genres: unless_locked(metadata.genres_lock, Some(Some(Vec::new()))),
        tags: unless_locked(metadata.tags_lock, Some(Some(Vec::new()))),
        total_book_count: unless_locked(metadata.total_book_count_lock, Some(None)),
        links: unless_locked(metadata.links_lock, Some(None)),
        status_lock: None,
        title_lock: None,
        title_sort_lock: None,
        alternate_titles_lock: None,
        summary_lock: None,
        publisher_lock: None,
        reading_direction_lock: None,
        age_rating_lock: None,
        language_lock: None,
        genres_lock: None,
        tags_lock: None,
        total_book_count_lock: None,
        links_lock: None,
    }
}

/// 书籍元数据重置请求 —— 对应 Kotlin `bookMetadataResetRequest`。
pub fn book_metadata_reset_request(
    name: &str,
    book_number: Option<i32>,
    metadata: &MediaServerBookMetadata,
) -> KomgaBookMetadataUpdateRequest {
    let number_value = book_number.map(|n| Some(Some(n.to_string()))).unwrap_or(Some(None));
    let number_sort_value = book_number.map(|n| Some(Some(n as f64))).unwrap_or(Some(None));
    KomgaBookMetadataUpdateRequest {
        title: unless_locked(metadata.title_lock, Some(Some(name.to_string()))),
        summary: unless_locked(metadata.summary_lock, Some(Some(String::new()))),
        number: unless_locked(metadata.number_lock, number_value),
        number_sort: unless_locked(metadata.number_sort_lock, number_sort_value),
        release_date: unless_locked(metadata.release_date_lock, Some(None)),
        authors: unless_locked(metadata.authors_lock, Some(Some(Vec::new()))),
        tags: unless_locked(metadata.tags_lock, Some(Some(Vec::new()))),
        isbn: unless_locked(metadata.isbn_lock, Some(None)),
        links: unless_locked(metadata.links_lock, Some(Some(Vec::new()))),
        title_lock: None,
        summary_lock: None,
        number_lock: None,
        number_sort_lock: None,
        release_date_lock: None,
        authors_lock: None,
        tags_lock: None,
        isbn_lock: None,
        links_lock: None,
    }
}

/// 从内部更新模型转 Komga 系列元数据请求 —— 对应 Kotlin `toMetadataUpdateRequest`。
pub fn to_series_update_request(
    update: &MediaServerSeriesMetadataUpdate,
    alternate_title_labels: &AlternateTitleLabelsConfig,
) -> KomgaSeriesMetadataUpdateRequest {
    fn patch<T>(value: Option<T>) -> PatchValue<T> {
        value.map(Some)
    }

    KomgaSeriesMetadataUpdateRequest {
        status: patch(update.status.map(|s| match s {
            SeriesStatus::Completed | SeriesStatus::Ended => Some("ENDED".to_string()),
            SeriesStatus::Ongoing => Some("ONGOING".to_string()),
            SeriesStatus::Abandoned => Some("ABANDONED".to_string()),
            SeriesStatus::Hiatus => Some("HIATUS".to_string()),
        })),
        title: patch(update.title.as_ref().map(|t| t.name.clone())),
        title_sort: patch(update.title_sort.as_ref().map(|t| t.name.clone())),
        alternate_titles: patch(update.alternative_titles.as_ref().map(|titles| {
            // 对应 Kotlin：ROMAJI/NATIVE 用类型标签，LOCALIZED 用语言（缺省回退类型标签），
            // null 类型且无语言 → 过滤；再按 title 去重（distinctBy { it.title }，保留首个）。
            let mut seen = std::collections::HashSet::new();
            titles
                .iter()
                .filter_map(|(name, title_type, language)| {
                    let label = match title_type {
                        Some(TitleType::Romaji) => Some(
                            alternate_title_labels
                                .romaji
                                .clone()
                                .unwrap_or_else(|| TitleType::Romaji.label().to_string()),
                        ),
                        Some(TitleType::Native) => Some(
                            alternate_title_labels
                                .native
                                .clone()
                                .unwrap_or_else(|| TitleType::Native.label().to_string()),
                        ),
                        Some(TitleType::Localized) => Some(
                            alternate_title_labels
                                .localized
                                .clone()
                                .unwrap_or_else(|| {
                                    language
                                        .clone()
                                        .unwrap_or_else(|| TitleType::Localized.label().to_string())
                                }),
                        ),
                        None => language.clone(),
                    }?;
                    let dto = KomgaAlternateTitleDto {
                        label,
                        title: name.clone(),
                    };
                    seen.insert(dto.title.clone()).then_some(dto)
                })
                .collect::<Vec<_>>()
        })),
        summary: patch(update.summary.clone()),
        publisher: patch(update.publisher.clone()),
        reading_direction: patch(update.reading_direction.map(|d| match d {
            ReadingDirection::LeftToRight => Some("LEFT_TO_RIGHT".to_string()),
            ReadingDirection::RightToLeft => Some("RIGHT_TO_LEFT".to_string()),
            ReadingDirection::Vertical => Some("VERTICAL".to_string()),
            ReadingDirection::Webtoon => Some("WEBTOON".to_string()),
        })),
        age_rating: patch(update.age_rating.map(Some)),
        language: patch(update.language.clone()),
        genres: patch(update.genres.clone()),
        tags: patch(update.tags.clone()),
        total_book_count: patch(update.total_book_count.filter(|c| *c > 0).map(Some)),
        links: patch(update.links.as_ref().map(|links| {
            links
                .iter()
                .map(|l| KomgaWebLinkDto { label: l.label.clone(), url: l.url.clone() })
                .collect()
        })),
        status_lock: patch(update.status_lock),
        title_lock: patch(update.title_lock),
        title_sort_lock: patch(update.title_sort_lock),
        alternate_titles_lock: patch(update.alternative_titles_lock),
        summary_lock: patch(update.summary_lock),
        publisher_lock: patch(update.publisher_lock),
        reading_direction_lock: patch(update.reading_direction_lock),
        age_rating_lock: patch(update.age_rating_lock),
        language_lock: patch(update.language_lock),
        genres_lock: patch(update.genres_lock),
        tags_lock: patch(update.tags_lock),
        total_book_count_lock: patch(update.total_book_count_lock),
        links_lock: patch(update.links_lock),
    }
}

/// 从内部更新模型转 Komga 书籍元数据请求 —— 对应 Kotlin `toKomgaMetadataUpdate`。
pub fn to_book_update_request(update: &MediaServerBookMetadataUpdate) -> KomgaBookMetadataUpdateRequest {
    fn patch<T>(value: Option<T>) -> PatchValue<T> {
        value.map(Some)
    }

    KomgaBookMetadataUpdateRequest {
        title: patch(update.title.clone()),
        summary: patch(update.summary.clone()),
        number: patch(update.number.clone()),
        number_sort: patch(update.number_sort),
        release_date: patch(update.release_date.clone().map(Some)),
        authors: patch(update.authors.as_ref().map(|authors| {
            authors
                .iter()
                .map(|a| KomgaAuthorDto { name: a.name.clone(), role: a.role.to_lowercase() })
                .collect()
        })),
        tags: patch(update.tags.clone()),
        isbn: patch(update.isbn.clone().map(Some)),
        links: patch(update.links.as_ref().map(|links| {
            links
                .iter()
                .map(|l| KomgaWebLinkDto { label: l.label.clone(), url: l.url.clone() })
                .collect()
        })),
        title_lock: patch(update.title_lock),
        summary_lock: patch(update.summary_lock),
        number_lock: patch(update.number_lock),
        number_sort_lock: patch(update.number_sort_lock),
        release_date_lock: patch(update.release_date_lock),
        authors_lock: patch(update.authors_lock),
        tags_lock: patch(update.tags_lock),
        isbn_lock: patch(update.isbn_lock),
        links_lock: patch(update.links_lock),
    }
}

#[async_trait::async_trait]
impl MediaServerClient for KomgaClient {
    async fn get_series(&self, series_id: &MediaServerSeriesId) -> Result<MediaServerSeries, MediaServerError> {
        let dto = self.get_series_dto(&series_id.0).await?;
        Ok(self.to_series(&dto))
    }

    async fn get_series_page(
        &self,
        library_id: &MediaServerLibraryId,
        page_number: i32,
    ) -> Result<Page<MediaServerSeries>, MediaServerError> {
        // Komga ≥1.19：GET /api/v1/series 已废弃，改用 POST /api/v1/series/list
        // （Kotlin komga-client 同款）。分页在 query，过滤条件在 body
        // {"condition":{"libraryId":{"operator":"is","value":<id>}}}。
        // Rust 扩展：固定按 lastModified 倒序（分页稳定，最新修改的系列优先处理）。
        let page_index = (page_number - 1).max(0);
        let page_index_str = page_index.to_string();
        let body = serde_json::json!({
            "condition": {
                "libraryId": {
                    "operator": "is",
                    "value": library_id.0
                }
            }
        });
        let response = self
            .http
            .post(self.url(&format!("{API_PREFIX}/series/list")))
            .query(&[
                ("page", page_index_str.as_str()),
                ("size", "500"),
                ("sort", "lastModified,desc"),
            ])
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        let page: KomgaPage<KomgaSeriesDto> = response.json().await?;
        Ok(Page {
            content: page.content.iter().map(|dto| self.to_series(dto)).collect(),
            page_number: page.number,
            total_elements: page.total_elements,
            total_pages: page.total_pages,
        })
    }

    async fn get_series_thumbnail(&self, series_id: &MediaServerSeriesId) -> Result<Option<Image>, MediaServerError> {
        // Kotlin `runCatching { getDefaultThumbnail() }.getOrNull()`：任何失败都返回 null。
        match self
            .http
            .get(self.url(&format!("{API_PREFIX}/series/{}/thumbnail", series_id.0)))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                let bytes = response.bytes().await?;
                Ok(Some(Image::new(bytes.to_vec(), None)))
            }
            _ => Ok(None),
        }
    }

    async fn get_series_thumbnails(
        &self,
        series_id: &MediaServerSeriesId,
    ) -> Result<Vec<MediaServerSeriesThumbnail>, MediaServerError> {
        let thumbnails: Vec<KomgaThumbnailDto> = self
            .send_json(
                self.http
                    .get(self.url(&format!("{API_PREFIX}/series/{}/thumbnails", series_id.0))),
            )
            .await?;
        Ok(thumbnails
            .into_iter()
            .map(|t| MediaServerSeriesThumbnail {
                id: MediaServerThumbnailId(t.id),
                series_id: MediaServerSeriesId(t.series_id.unwrap_or_else(|| series_id.0.clone())),
                r#type: t.r#type,
                selected: t.selected,
            })
            .collect())
    }

    async fn get_book(&self, book_id: &MediaServerBookId) -> Result<MediaServerBook, MediaServerError> {
        let dto = self.get_book_dto(&book_id.0).await?;
        Ok(self.to_book(&dto))
    }

    async fn get_books(&self, series_id: &MediaServerSeriesId) -> Result<Vec<MediaServerBook>, MediaServerError> {
        // Komga ≥1.19：GET /api/v1/books 与 GET /api/v1/series/{id}/books 均已废弃，
        // 改用 POST /api/v1/books/list（Kotlin komga-client 同款）。分页在 query，
        // 过滤条件在 body {"condition":{"seriesId":{"operator":"is","value":<id>}}}。
        let mut all = Vec::new();
        let mut page_index = 0;
        loop {
            let page_index_str = page_index.to_string();
            let body = serde_json::json!({
                "condition": {
                    "seriesId": {
                        "operator": "is",
                        "value": series_id.0
                    }
                }
            });
            let response = self
                .http
                .post(self.url(&format!("{API_PREFIX}/books/list")))
                .query(&[("page", page_index_str.as_str()), ("size", "500")])
                .json(&body)
                .send()
                .await?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(MediaServerError::Status(status, body));
            }
            let page: KomgaPage<KomgaBookDto> = response.json().await?;
            all.extend(page.content.iter().map(|dto| self.to_book(dto)));
            if page.number >= page.total_pages - 1 {
                break;
            }
            page_index += 1;
        }
        Ok(all)
    }

    async fn get_book_thumbnails(
        &self,
        book_id: &MediaServerBookId,
    ) -> Result<Vec<MediaServerBookThumbnail>, MediaServerError> {
        let thumbnails: Vec<KomgaThumbnailDto> = self
            .send_json(
                self.http
                    .get(self.url(&format!("{API_PREFIX}/books/{}/thumbnails", book_id.0))),
            )
            .await?;
        Ok(thumbnails
            .into_iter()
            .map(|t| MediaServerBookThumbnail {
                id: MediaServerThumbnailId(t.id),
                book_id: MediaServerBookId(t.book_id.unwrap_or_else(|| book_id.0.clone())),
                r#type: t.r#type,
                selected: t.selected,
                file_size: t.file_size,
            })
            .collect())
    }

    async fn get_book_thumbnail(&self, book_id: &MediaServerBookId) -> Result<Option<Image>, MediaServerError> {
        // Kotlin `runCatching { getDefaultThumbnail() }.getOrNull()`：任何失败都返回 null。
        match self
            .http
            .get(self.url(&format!("{API_PREFIX}/books/{}/thumbnail", book_id.0)))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                let bytes = response.bytes().await?;
                Ok(Some(Image::new(bytes.to_vec(), None)))
            }
            _ => Ok(None),
        }
    }

    async fn get_library(&self, library_id: &MediaServerLibraryId) -> Result<MediaServerLibrary, MediaServerError> {
        let dto: KomgaLibraryDto = self
            .send_json(self.http.get(self.url(&format!("{API_PREFIX}/libraries/{}", library_id.0))))
            .await?;
        Ok(MediaServerLibrary {
            id: MediaServerLibraryId(dto.id),
            name: dto.name,
            roots: vec![dto.root],
        })
    }

    async fn get_libraries(&self) -> Result<Vec<MediaServerLibrary>, MediaServerError> {
        let dtos: Vec<KomgaLibraryDto> = self
            .send_json(self.http.get(self.url(&format!("{API_PREFIX}/libraries"))))
            .await?;
        Ok(dtos
            .into_iter()
            .map(|dto| MediaServerLibrary {
                id: MediaServerLibraryId(dto.id),
                name: dto.name,
                roots: vec![dto.root],
            })
            .collect())
    }

    async fn get_collections(&self) -> Result<Vec<MediaServerCollection>, MediaServerError> {
        let page: KomgaPage<KomgaCollectionDto> = self
            .send_json(
                self.http
                    .get(self.url(&format!("{API_PREFIX}/collections")))
                    .query(&[("unpaged", "true")]),
            )
            .await?;
        Ok(page
            .content
            .into_iter()
            .map(|dto| MediaServerCollection {
                id: dto.id,
                name: dto.name,
                series_ids: dto.series_ids,
            })
            .collect())
    }

    async fn create_collection(
        &self,
        name: &str,
        series_ids: &[String],
    ) -> Result<MediaServerCollection, MediaServerError> {
        let body = serde_json::json!({
            "name": name,
            "seriesIds": series_ids,
            "ordered": false,
        });
        let dto: KomgaCollectionDto = self
            .send_json(self.http.post(self.url(&format!("{API_PREFIX}/collections"))).json(&body))
            .await?;
        Ok(MediaServerCollection {
            id: dto.id,
            name: dto.name,
            series_ids: dto.series_ids,
        })
    }

    async fn update_collection_series(
        &self,
        collection_id: &str,
        series_ids: &[String],
    ) -> Result<(), MediaServerError> {
        let body = serde_json::json!({ "seriesIds": series_ids });
        let response = self
            .http
            .patch(self.url(&format!("{API_PREFIX}/collections/{collection_id}")))
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(())
    }

    async fn update_series_metadata(
        &self,
        series_id: &MediaServerSeriesId,
        metadata: &MediaServerSeriesMetadataUpdate,
    ) -> Result<(), MediaServerError> {
        let request = to_series_update_request(metadata, &self.alternate_title_labels);
        let response = self
            .http
            .patch(self.url(&format!("{API_PREFIX}/series/{}/metadata", series_id.0)))
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(())
    }

    async fn delete_series_thumbnail(
        &self,
        series_id: &MediaServerSeriesId,
        thumbnail_id: &MediaServerThumbnailId,
    ) -> Result<(), MediaServerError> {
        let response = self
            .http
            .delete(self.url(&format!(
                "{API_PREFIX}/series/{}/thumbnails/{}",
                series_id.0, thumbnail_id.0
            )))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(())
    }

    async fn update_book_metadata(
        &self,
        book_id: &MediaServerBookId,
        metadata: &MediaServerBookMetadataUpdate,
    ) -> Result<(), MediaServerError> {
        let request = to_book_update_request(metadata);
        let response = self
            .http
            .patch(self.url(&format!("{API_PREFIX}/books/{}/metadata", book_id.0)))
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(())
    }

    async fn delete_book_thumbnail(
        &self,
        book_id: &MediaServerBookId,
        thumbnail_id: &MediaServerThumbnailId,
    ) -> Result<(), MediaServerError> {
        let response = self
            .http
            .delete(self.url(&format!(
                "{API_PREFIX}/books/{}/thumbnails/{}",
                book_id.0, thumbnail_id.0
            )))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(())
    }

    async fn reset_book_metadata(
        &self,
        book: &MediaServerBook,
        book_number: Option<i32>,
    ) -> Result<(), MediaServerError> {
        let request = book_metadata_reset_request(&book.name, book_number, &book.metadata);
        let response = self
            .http
            .patch(self.url(&format!("{API_PREFIX}/books/{}/metadata", book.id.0)))
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(())
    }

    async fn reset_series_metadata(
        &self,
        series: &MediaServerSeries,
    ) -> Result<(), MediaServerError> {
        let request = series_metadata_reset_request(&series.name, &series.metadata);
        let response = self
            .http
            .patch(self.url(&format!("{API_PREFIX}/series/{}/metadata", series.id.0)))
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(())
    }

    async fn upload_series_thumbnail(
        &self,
        series_id: &MediaServerSeriesId,
        thumbnail: &Image,
        selected: bool,
        _lock: bool,
    ) -> Result<Option<MediaServerSeriesThumbnail>, MediaServerError> {
        if thumbnail.bytes.len() as u64 > self.thumbnail_size_limit {
            tracing::warn!(
                "Thumbnail size {} bytes is bigger than limit {}. Skipping thumbnail upload",
                thumbnail.bytes.len(),
                self.thumbnail_size_limit
            );
            return Ok(None);
        }
        let mime = thumbnail.mime_type.clone().unwrap_or_else(|| "image/jpeg".to_string());
        let part = reqwest::multipart::Part::bytes(thumbnail.bytes.clone())
            .file_name("thumbnail")
            .mime_str(&mime)
            .map_err(|e| MediaServerError::message(format!("invalid mime: {e}")))?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let response = self
            .http
            .post(self.url(&format!("{API_PREFIX}/series/{}/thumbnails", series_id.0)))
            .query(&[("selected", selected.to_string())])
            .multipart(form)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        let dto: KomgaThumbnailDto = response.json().await?;
        Ok(Some(MediaServerSeriesThumbnail {
            id: MediaServerThumbnailId(dto.id),
            series_id: MediaServerSeriesId(dto.series_id.unwrap_or_else(|| series_id.0.clone())),
            r#type: dto.r#type,
            selected: dto.selected,
        }))
    }

    async fn upload_book_thumbnail(
        &self,
        book_id: &MediaServerBookId,
        thumbnail: &Image,
        selected: bool,
        _lock: bool,
    ) -> Result<Option<MediaServerBookThumbnail>, MediaServerError> {
        if thumbnail.bytes.len() as u64 > self.thumbnail_size_limit {
            tracing::warn!(
                "Thumbnail size {} bytes is bigger than limit {}. Skipping thumbnail upload",
                thumbnail.bytes.len(),
                self.thumbnail_size_limit
            );
            return Ok(None);
        }
        let mime = thumbnail.mime_type.clone().unwrap_or_else(|| "image/jpeg".to_string());
        let part = reqwest::multipart::Part::bytes(thumbnail.bytes.clone())
            .file_name("thumbnail")
            .mime_str(&mime)
            .map_err(|e| MediaServerError::message(format!("invalid mime: {e}")))?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let response = self
            .http
            .post(self.url(&format!("{API_PREFIX}/books/{}/thumbnails", book_id.0)))
            .query(&[("selected", selected.to_string())])
            .multipart(form)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        let dto: KomgaThumbnailDto = response.json().await?;
        Ok(Some(MediaServerBookThumbnail {
            id: MediaServerThumbnailId(dto.id),
            book_id: MediaServerBookId(dto.book_id.unwrap_or_else(|| book_id.0.clone())),
            r#type: dto.r#type,
            selected: dto.selected,
            file_size: dto.file_size,
        }))
    }

    async fn refresh_metadata(
        &self,
        _library_id: &MediaServerLibraryId,
        series_id: &MediaServerSeriesId,
    ) -> Result<(), MediaServerError> {
        let response = self
            .http
            .post(self.url(&format!("{API_PREFIX}/series/{}/analyze", series_id.0)))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Komga 事件（SSE）—— 对应 `KomgaEventHandler.kt`
// ---------------------------------------------------------------------------

/// Komga SSE 事件解析（`/sse/v1/events`）。
#[derive(Debug, Clone)]
pub enum KomgaEvent {
    SeriesAdded { series_id: String, library_id: String },
    SeriesChanged { series_id: String, library_id: String },
    SeriesDeleted { series_id: String, library_id: String },
    BookAdded { book_id: String, series_id: String, library_id: String },
    BookChanged { book_id: String, series_id: String, library_id: String },
    BookDeleted { book_id: String, series_id: String, library_id: String },
    TaskQueueStatus { count: i64 },
    Other { r#type: String, data: String },
}

impl KomgaEvent {
    /// 从 SSE 原始行解析事件（`event: type\ndata: json`）。
    pub fn parse(event_type: &str, data: &str) -> KomgaEvent {
        match event_type {
            "SeriesAdded" | "SeriesChanged" | "SeriesDeleted" => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
                    let series_id = value
                        .pointer("/seriesId")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let library_id = value
                        .pointer("/libraryId")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    return match event_type {
                        "SeriesAdded" => KomgaEvent::SeriesAdded { series_id, library_id },
                        "SeriesChanged" => KomgaEvent::SeriesChanged { series_id, library_id },
                        _ => KomgaEvent::SeriesDeleted { series_id, library_id },
                    };
                }
                KomgaEvent::Other { r#type: event_type.to_string(), data: data.to_string() }
            }
            "BookAdded" | "BookChanged" | "BookDeleted" => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
                    let book_id = value.pointer("/bookId").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let series_id = value.pointer("/seriesId").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let library_id = value.pointer("/libraryId").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    return match event_type {
                        "BookAdded" => KomgaEvent::BookAdded { book_id, series_id, library_id },
                        "BookChanged" => KomgaEvent::BookChanged { book_id, series_id, library_id },
                        _ => KomgaEvent::BookDeleted { book_id, series_id, library_id },
                    };
                }
                KomgaEvent::Other { r#type: event_type.to_string(), data: data.to_string() }
            }
            "TaskQueueStatus" => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
                    let count = value.pointer("/count").and_then(|v| v.as_i64()).unwrap_or(0);
                    return KomgaEvent::TaskQueueStatus { count };
                }
                KomgaEvent::Other { r#type: event_type.to_string(), data: data.to_string() }
            }
            _ => KomgaEvent::Other { r#type: event_type.to_string(), data: data.to_string() },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(value: impl Serialize) -> serde_json::Value {
        serde_json::to_value(value).unwrap()
    }

    fn update_request(update: &MediaServerSeriesMetadataUpdate) -> KomgaSeriesMetadataUpdateRequest {
        to_series_update_request(update, &AlternateTitleLabelsConfig::default())
    }

    fn series_update_base() -> MediaServerSeriesMetadataUpdate {
        MediaServerSeriesMetadataUpdate {
            status: None,
            title: None,
            title_sort: None,
            alternative_titles: None,
            summary: None,
            publisher: None,
            alternative_publishers: None,
            reading_direction: None,
            age_rating: None,
            language: None,
            genres: None,
            tags: None,
            total_book_count: None,
            authors: None,
            release_year: None,
            links: None,
            status_lock: None,
            title_lock: None,
            title_sort_lock: None,
            alternative_titles_lock: None,
            summary_lock: None,
            publisher_lock: None,
            reading_direction_lock: None,
            age_rating_lock: None,
            language_lock: None,
            genres_lock: None,
            tags_lock: None,
            total_book_count_lock: None,
            links_lock: None,
        }
    }

    fn book_update_base() -> MediaServerBookMetadataUpdate {
        MediaServerBookMetadataUpdate {
            title: None,
            summary: None,
            release_date: None,
            authors: None,
            tags: None,
            isbn: None,
            links: None,
            number: None,
            number_sort: None,
            title_lock: None,
            summary_lock: None,
            number_lock: None,
            number_sort_lock: None,
            release_date_lock: None,
            authors_lock: None,
            tags_lock: None,
            isbn_lock: None,
            links_lock: None,
        }
    }

    // ---------- reset 请求：Kotlin PatchValue.None → JSON null → Komga 清空字段 ----------

    #[test]
    fn series_reset_request_omits_none_fields() {
        let req = series_metadata_reset_request("My Series", &MediaServerSeriesMetadata::default());
        let v = json(&req);
        // 显式设置/清空的字段必须存在（全部未锁定时）
        assert_eq!(v["status"], "ONGOING");
        assert_eq!(v["title"], "My Series");
        assert_eq!(v["titleSort"], "My Series");
        assert_eq!(v["summary"], "");
        assert_eq!(v["publisher"], "");
        assert_eq!(v["language"], "");
        assert_eq!(v["genres"], serde_json::json!([]));
        assert_eq!(v["tags"], serde_json::json!([]));
        // lock 字段全部省略：尊重 Komga 现有 lock（锁定字段保持锁定、值不被重置）。
        // 注意：Kotlin 原版发 Some(false) 会解锁所有字段，本实现为改进（用户要求尊重 lock）。
        for key in [
            "statusLock",
            "titleLock",
            "titleSortLock",
            "alternateTitlesLock",
            "summaryLock",
            "publisherLock",
            "readingDirectionLock",
            "ageRatingLock",
            "languageLock",
            "genresLock",
            "tagsLock",
            "totalBookCountLock",
            "linksLock",
        ] {
            assert!(
                v.get(key).is_none(),
                "field {key} should be omitted (respect existing lock), got {:?}",
                v.get(key)
            );
        }
        // PatchValue.None（Kotlin）→ JSON null → Komga 清空字段（对齐 Kotlin seriesMetadataResetRequest）
        for key in [
            "alternateTitles",
            "readingDirection",
            "ageRating",
            "totalBookCount",
            "links",
        ] {
            assert_eq!(
                v.get(key),
                Some(&serde_json::Value::Null),
                "field {key} should be null (clear), got {:?}",
                v.get(key)
            );
        }
    }

    #[test]
    fn series_reset_request_skips_locked_fields() {
        // 锁定 title + summary：对应字段不发送（保持值），其余正常重置
        let mut md = MediaServerSeriesMetadata::default();
        md.title_lock = true;
        md.summary_lock = true;
        let req = series_metadata_reset_request("My Series", &md);
        let v = json(&req);
        assert!(v.get("title").is_none(), "locked title must be omitted");
        assert!(v.get("summary").is_none(), "locked summary must be omitted");
        assert_eq!(v["status"], "ONGOING");
        assert_eq!(v["titleSort"], "My Series");
        assert_eq!(v["publisher"], "");
        assert!(v.get("titleLock").is_none(), "lock fields always omitted");
    }

    #[test]
    fn book_reset_request_skips_locked_fields() {
        let mut md = MediaServerBookMetadata::default();
        md.title_lock = true;
        md.number_lock = true;
        md.number_sort_lock = true;
        let req = book_metadata_reset_request("My Book", Some(3), &md);
        let v = json(&req);
        assert!(v.get("title").is_none(), "locked title must be omitted");
        assert!(v.get("number").is_none(), "locked number must be omitted");
        assert!(v.get("numberSort").is_none(), "locked numberSort must be omitted");
        assert_eq!(v["summary"], "");
        assert_eq!(v["authors"], serde_json::json!([]));
        assert!(v.get("titleLock").is_none(), "lock fields always omitted");
    }

    #[test]
    fn book_reset_request_omits_none_fields() {
        let req = book_metadata_reset_request("Chapter 1", Some(12), &MediaServerBookMetadata::default());
        let v = json(&req);
        assert_eq!(v["title"], "Chapter 1");
        assert_eq!(v["summary"], "");
        assert_eq!(v["number"], "12");
        assert_eq!(v["numberSort"], 12.0);
        assert_eq!(v["authors"], serde_json::json!([]));
        assert_eq!(v["tags"], serde_json::json!([]));
        assert_eq!(v["links"], serde_json::json!([]));
        for key in ["releaseDate", "isbn"] {
            assert_eq!(
                v.get(key),
                Some(&serde_json::Value::Null),
                "field {key} should be null (clear), got {:?}",
                v.get(key)
            );
        }
        // bookNumber 为 null 时 number/numberSort 同样输出 null（Kotlin PatchValue.None）
        let req_none = book_metadata_reset_request("Oneshot", None, &MediaServerBookMetadata::default());
        let v2 = json(&req_none);
        assert_eq!(v2.get("number"), Some(&serde_json::Value::Null));
        assert_eq!(v2.get("numberSort"), Some(&serde_json::Value::Null));
    }

    // ---------- 更新请求 ----------

    #[test]
    fn alternate_title_labels_config_maps_labels() {
        let labels = AlternateTitleLabelsConfig {
            romaji: Some("罗马音".to_string()),
            native: Some("原名".to_string()),
            localized: Some("别名".to_string()),
        };
        let update = MediaServerSeriesMetadataUpdate {
            alternative_titles: Some(vec![
                ("soredemo".to_string(), Some(TitleType::Romaji), None),
                ("それでも".to_string(), Some(TitleType::Native), None),
                ("Soredemo".to_string(), Some(TitleType::Localized), Some("en".to_string())),
            ]),
            ..series_update_base()
        };
        let v = json(&to_series_update_request(&update, &labels));
        let titles = v["alternateTitles"].as_array().unwrap();
        let pairs: Vec<(String, String)> = titles
            .iter()
            .map(|t| (t["label"].as_str().unwrap().to_string(), t["title"].as_str().unwrap().to_string()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("罗马音".to_string(), "soredemo".to_string()),
                ("原名".to_string(), "それでも".to_string()),
                ("别名".to_string(), "Soredemo".to_string()),
            ]
        );
    }

    #[test]
    fn series_update_request_status_mapping() {
        let req = update_request(&MediaServerSeriesMetadataUpdate {
            status: Some(SeriesStatus::Completed),
            ..series_update_base()
        });
        assert_eq!(json(&req)["status"], "ENDED");

        let req2 = update_request(&MediaServerSeriesMetadataUpdate {
            status: Some(SeriesStatus::Ended),
            ..series_update_base()
        });
        assert_eq!(json(&req2)["status"], "ENDED");

        let req3 = update_request(&MediaServerSeriesMetadataUpdate {
            status: Some(SeriesStatus::Hiatus),
            ..series_update_base()
        });
        assert_eq!(json(&req3)["status"], "HIATUS");
    }

    #[test]
    fn series_update_request_omits_null_fields() {
        let req = update_request(&series_update_base());
        let v = json(&req);
        for key in [
            "status",
            "title",
            "titleSort",
            "alternateTitles",
            "summary",
            "readingDirection",
            "ageRating",
            "links",
        ] {
            assert!(
                v.get(key).is_none(),
                "field {key} should be omitted when null, got {:?}",
                v.get(key)
            );
        }
    }

    #[test]
    fn alternate_titles_mapping_matches_kotlin() {
        // ROMAJI/NATIVE → type.label；LOCALIZED → language 或 type.label；null 类型无语言 → 过滤；按 title 去重
        let update = MediaServerSeriesMetadataUpdate {
            alternative_titles: Some(vec![
                ("soredemo".to_string(), Some(TitleType::Romaji), None),
                ("Soredemo".to_string(), Some(TitleType::Localized), Some("en".to_string())),
                ("それでも".to_string(), Some(TitleType::Native), None),
                ("dropped-no-language".to_string(), None, None),
                ("kept-with-language".to_string(), None, Some("fr".to_string())),
                ("soredemo".to_string(), Some(TitleType::Romaji), None), // 与第一条 title 相同 → 去重
            ]),
            ..series_update_base()
        };
        let v = json(&update_request(&update));
        let titles = v["alternateTitles"].as_array().unwrap();
        let pairs: Vec<(String, String)> = titles
            .iter()
            .map(|t| (t["label"].as_str().unwrap().to_string(), t["title"].as_str().unwrap().to_string()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("Romaji".to_string(), "soredemo".to_string()),
                ("en".to_string(), "Soredemo".to_string()),
                ("Native".to_string(), "それでも".to_string()),
                ("fr".to_string(), "kept-with-language".to_string()),
            ]
        );
    }

    #[test]
    fn book_update_request_maps_fields() {
        let req = to_book_update_request(&MediaServerBookMetadataUpdate {
            title: Some("Renamed".to_string()),
            number_sort: Some(12.5),
            authors: Some(vec![MediaServerAuthor {
                name: "Author".to_string(),
                role: "Writer".to_string(),
            }]),
            ..book_update_base()
        });
        let v = json(&req);
        assert_eq!(v["title"], "Renamed");
        assert_eq!(v["numberSort"], 12.5);
        assert_eq!(v["authors"][0]["name"], "Author");
        // Komga 标准 AuthorRole 序列化为小写（writer/penciller/...）；大写枚举名会被存为自定义角色
        assert_eq!(v["authors"][0]["role"], "writer");
        assert!(v.get("releaseDate").is_none());
    }

    // ---------- numberSort 反序列化与 Kotlin Float.toString ----------

    #[test]
    fn book_dto_number_sort_deserializes_number() {
        let dto: KomgaBookMetadataDto = serde_json::from_str(
            r#"{"title":"t","titleLock":false,"summary":"","summaryLock":false,"number":null,"numberLock":false,"numberSort":12.5,"numberSortLock":false,"releaseDate":null,"releaseDateLock":false,"authors":[],"authorsLock":false,"tags":[],"tagsLock":false,"isbn":null,"isbnLock":false,"links":[],"linksLock":false}"#,
        )
        .unwrap();
        assert_eq!(dto.number_sort, 12.5);
    }

    #[test]
    fn kotlin_float_string_matches_kotlin() {
        assert_eq!(kotlin_float_string(12.0), "12.0");
        assert_eq!(kotlin_float_string(12.5), "12.5");
        assert_eq!(kotlin_float_string(0.0), "0.0");
        assert_eq!(kotlin_float_string(100.0), "100.0");
    }

    // ---------- Komga SSE 事件解析 ----------

    #[test]
    fn parse_book_added_event() {
        let event = KomgaEvent::parse(
            "BookAdded",
            r#"{"bookId":"b1","seriesId":"s1","libraryId":"l1","title":"x"}"#,
        );
        match event {
            KomgaEvent::BookAdded { book_id, series_id, library_id } => {
                assert_eq!(book_id, "b1");
                assert_eq!(series_id, "s1");
                assert_eq!(library_id, "l1");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_task_queue_status() {
        let event = KomgaEvent::parse("TaskQueueStatus", r#"{"count":3}"#);
        match event {
            KomgaEvent::TaskQueueStatus { count } => assert_eq!(count, 3),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_series_changed_event() {
        let event = KomgaEvent::parse(
            "SeriesChanged",
            r#"{"seriesId":"s1","libraryId":"l1"}"#,
        );
        match event {
            KomgaEvent::SeriesChanged { series_id, library_id } => {
                assert_eq!(series_id, "s1");
                assert_eq!(library_id, "l1");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_book_changed_event() {
        let event = KomgaEvent::parse(
            "BookChanged",
            r#"{"bookId":"b1","seriesId":"s1","libraryId":"l1"}"#,
        );
        match event {
            KomgaEvent::BookChanged { book_id, series_id, library_id } => {
                assert_eq!(book_id, "b1");
                assert_eq!(series_id, "s1");
                assert_eq!(library_id, "l1");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
