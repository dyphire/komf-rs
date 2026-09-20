//! ComicVine provider —— 对应 `providers/comicvine` 包。
//!
//! 使用 ComicVine API（`https://comicvine.gamespot.com/api`），需要 API key。
//! 对齐 Kotlin `ComicVineMetadataProvider.kt` / `ComicVineClient.kt` / `ComicVineRateLimiter.kt`
//! / `ComicVineMetadataMapper.kt` / `util/ImageHash.kt`。
use crate::config::ProviderConfig;
use crate::model::{
    Author, AuthorRole, BookMetadata, BookRange, BookStoryArc, Image, MatchQuery, ProviderBookId,
    ProviderBookMetadata, ProviderSeriesId, ProviderSeriesMetadata, Publisher, ReleaseDate,
    SeriesBook, SeriesMetadata, SeriesSearchResult, SeriesTitle, WebLink,
};
use crate::providers::{CoreProviders, MetadataProvider, ProviderError};
use crate::util::NameSimilarityMatcher;
use serde::Deserialize;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const BASE_URL: &str = "https://comicvine.gamespot.com/api";
const SIMILARITY_THRESHOLD: f64 = 0.1;

// ===================== 限流（对应 ComicVineRateLimiter.kt） =====================
//
// Kotlin 每类 API 独立 LimiterInternal：burst = intervalLimiter(50, 60min)（tryAcquire 立即放行），
// 突发窗口满时走 regular = rateLimiter(144, 60min)（每 25s 一个 permit，排队等待）。

/// intervalLimiter(50, 60.minutes)：滑动窗口突发限流。tryAcquire 在窗口未满时立即放行。
struct IntervalLimiter {
    events_per_interval: u32,
    interval: Duration,
    state: Mutex<IntervalLimiterState>,
}

struct IntervalLimiterState {
    count: u32,
    window_start: Instant,
}

impl IntervalLimiter {
    fn new(events_per_interval: u32, interval: Duration) -> Self {
        Self {
            events_per_interval,
            interval,
            state: Mutex::new(IntervalLimiterState {
                count: 0,
                window_start: Instant::now(),
            }),
        }
    }

    async fn try_acquire(&self) -> bool {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        if now.duration_since(state.window_start) >= self.interval {
            // 对齐 Kotlin getWakeUpTime：窗口过期后对齐到当前时刻。
            state.window_start = now;
            state.count = 1;
            return true;
        }
        if state.count < self.events_per_interval {
            state.count += 1;
            return true;
        }
        false
    }
}

/// rateLimiter(144, 60.minutes)：令牌式限流，permit 间隔 = interval / events。
struct RateLimiter {
    permit_duration: Duration,
    state: Mutex<RateLimiterState>,
}

struct RateLimiterState {
    cursor: Instant,
}

impl RateLimiter {
    fn new(events_per_interval: u32, interval: Duration) -> Self {
        Self {
            permit_duration: interval / events_per_interval,
            state: Mutex::new(RateLimiterState {
                cursor: Instant::now(),
            }),
        }
    }

    async fn acquire(&self) {
        let wait = {
            let mut state = self.state.lock().await;
            let now = Instant::now();
            let base = if state.cursor > now {
                state.cursor
            } else {
                now
            };
            state.cursor = base + self.permit_duration;
            base.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

struct LimiterInternal {
    bursting: IntervalLimiter,
    regular: RateLimiter,
}

impl LimiterInternal {
    fn new() -> Self {
        Self {
            bursting: IntervalLimiter::new(50, Duration::from_secs(60 * 60)),
            regular: RateLimiter::new(144, Duration::from_secs(60 * 60)),
        }
    }

    async fn acquire(&self) {
        if !self.bursting.try_acquire().await {
            self.regular.acquire().await;
        }
    }
}

struct ComicVineRateLimiter {
    search: LimiterInternal,
    volume: LimiterInternal,
    issue: LimiterInternal,
    story_arc: LimiterInternal,
    cover: LimiterInternal,
}

impl ComicVineRateLimiter {
    fn new() -> Self {
        Self {
            search: LimiterInternal::new(),
            volume: LimiterInternal::new(),
            issue: LimiterInternal::new(),
            story_arc: LimiterInternal::new(),
            cover: LimiterInternal::new(),
        }
    }

    async fn search_acquire(&self) {
        self.search.acquire().await;
    }
    async fn volume_acquire(&self) {
        self.volume.acquire().await;
    }
    async fn issue_acquire(&self) {
        self.issue.acquire().await;
    }
    async fn story_arc_acquire(&self) {
        self.story_arc.acquire().await;
    }
    async fn cover_acquire(&self) {
        self.cover.acquire().await;
    }
}

// ===================== Model（对齐 ComicVine*.kt，API 键均为 snake_case） =====================

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComicVineResponse<T> {
    pub error: String,
    pub status_code: i32,
    pub results: T,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComicVineVolumeSearchResult {
    pub id: i32,
    pub name: String,
    pub api_detail_url: Option<String>,
    pub site_detail_url: Option<String>,
    pub aliases: Option<String>,
    pub count_of_issues: Option<i32>,
    pub description: Option<String>,
    pub first_issue: Option<ComicVineIssueSlim>,
    pub last_issue: Option<ComicVineIssueSlim>,
    pub image: Option<ComicVineImage>,
    pub publisher: Option<ComicVinePublisher>,
    pub start_year: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComicVineVolume {
    pub id: i32,
    pub name: String,
    pub api_detail_url: Option<String>,
    pub site_detail_url: Option<String>,
    pub aliases: Option<String>,
    pub count_of_issues: Option<i32>,
    pub description: Option<String>,
    pub image: Option<ComicVineImage>,
    pub publisher: Option<ComicVinePublisher>,
    pub start_year: Option<String>,
    pub issues: Option<Vec<ComicVineIssueSlim>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComicVineIssueSlim {
    pub id: i32,
    pub name: Option<String>,
    pub api_detail_url: Option<String>,
    pub site_detail_url: Option<String>,
    pub issue_number: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComicVineIssue {
    pub id: i32,
    pub name: Option<String>,
    pub api_detail_url: Option<String>,
    pub site_detail_url: Option<String>,
    pub aliases: Option<String>,
    pub cover_date: Option<String>,
    pub store_date: Option<String>,
    pub description: Option<String>,
    pub image: Option<ComicVineImage>,
    pub issue_number: Option<String>,
    pub person_credits: Option<Vec<ComicVinePersonCredit>>,
    pub story_arc_credits: Option<Vec<ComicVineCredit>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComicVinePersonCredit {
    pub id: i32,
    pub name: String,
    pub api_detail_url: Option<String>,
    pub site_detail_url: Option<String>,
    pub role: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComicVineCredit {
    pub id: i32,
    pub name: String,
    pub api_detail_url: Option<String>,
    pub site_detail_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComicVineStoryArc {
    pub id: i32,
    pub name: String,
    pub issues: Vec<ComicVineStoryArcIssue>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComicVineStoryArcIssue {
    pub id: i32,
    pub name: Option<String>,
    pub api_detail_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComicVinePublisher {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComicVineImage {
    pub icon_url: Option<String>,
    pub medium_url: Option<String>,
    pub screen_url: Option<String>,
    pub screen_large_url: Option<String>,
    pub small_url: Option<String>,
    pub super_url: Option<String>,
    pub thumb_url: Option<String>,
    pub tiny_url: Option<String>,
    pub original_url: Option<String>,
    pub image_tags: Option<String>,
}

// ===================== Client（对应 ComicVineClient.kt） =====================

pub struct ComicVineClient {
    http: reqwest::Client,
    api_key: String,
    search_limit: i32,
    rate_limiter: ComicVineRateLimiter,
}

impl ComicVineClient {
    pub fn new(http: reqwest::Client, api_key: String, search_limit: i32) -> Self {
        Self {
            http,
            api_key,
            search_limit,
            rate_limiter: ComicVineRateLimiter::new(),
        }
    }

    /// 对应 `handleResult`：error != "OK" 时抛 "Comic Vine returned error response. status code ..."。
    async fn handle_result<T>(&self, response: reqwest::Response) -> Result<T, ProviderError>
    where
        T: serde::de::DeserializeOwned,
    {
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(
                CoreProviders::ComicVine,
                status,
                body,
            ));
        }
        let body: ComicVineResponse<T> = response.json().await?;
        if body.error != "OK" {
            return Err(ProviderError::message(format!(
                "Comic Vine returned error response. status code {}",
                body.status_code
            )));
        }
        Ok(body.results)
    }

    /// 对应 `searchVolume`：GET /search/?query=&format=json&resources=volume&limit=&api_key=。
    pub async fn search_volumes(
        &self,
        name: &str,
    ) -> Result<Vec<ComicVineVolumeSearchResult>, ProviderError> {
        self.rate_limiter.search_acquire().await;
        let limit = self.search_limit.to_string();
        let response = self
            .http
            .get(format!("{BASE_URL}/search/"))
            .query(&[
                ("api_key", self.api_key.as_str()),
                ("format", "json"),
                ("query", name),
                ("resources", "volume"),
                ("limit", limit.as_str()),
            ])
            .send()
            .await?;
        self.handle_result(response).await
    }

    /// 对应 `getVolume`：GET /volume/4050-{id}/（全字段，无 field_list）。
    pub async fn get_volume(&self, id: i32) -> Result<ComicVineVolume, ProviderError> {
        self.rate_limiter.volume_acquire().await;
        let response = self
            .http
            .get(format!("{BASE_URL}/volume/4050-{id}/"))
            .query(&[("api_key", self.api_key.as_str()), ("format", "json")])
            .send()
            .await?;
        self.handle_result(response).await
    }

    /// 对应 `getIssue`：GET /issue/4000-{id}/。
    pub async fn get_issue(&self, id: i32) -> Result<ComicVineIssue, ProviderError> {
        self.rate_limiter.issue_acquire().await;
        let response = self
            .http
            .get(format!("{BASE_URL}/issue/4000-{id}/"))
            .query(&[("api_key", self.api_key.as_str()), ("format", "json")])
            .send()
            .await?;
        self.handle_result(response).await
    }

    /// 对应 `getStoryArc`：GET /story_arc/4045-{id}/。
    pub async fn get_story_arc(&self, id: i32) -> Result<ComicVineStoryArc, ProviderError> {
        self.rate_limiter.story_arc_acquire().await;
        let response = self
            .http
            .get(format!("{BASE_URL}/story_arc/4045-{id}/"))
            .query(&[("api_key", self.api_key.as_str()), ("format", "json")])
            .send()
            .await?;
        self.handle_result(response).await
    }

    /// 对应 `getCover`（限流 + 404 忽略）。
    pub async fn get_cover(&self, url: &str) -> Result<Option<Image>, ProviderError> {
        self.rate_limiter.cover_acquire().await;
        let response = self.http.get(url).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            tracing::error!(
                "Entry has cover url but there was no image. Skipping cover retrieval \"{url}\""
            );
            return Ok(None);
        }
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(
                CoreProviders::ComicVine,
                status,
                body,
            ));
        }
        let bytes = response.bytes().await?;
        Ok(Some(Image::new(bytes.to_vec(), None)))
    }
}

// ===================== ImageHash（对应 util/ImageHash.kt） =====================

/// 32×32 AverageHash，normalized Hamming ≤ 0.1 视为匹配。
fn compare_images(image1: &[u8], image2: &[u8]) -> bool {
    let Ok(img1) = image::load_from_memory(image1) else {
        return false;
    };
    let Ok(img2) = image::load_from_memory(image2) else {
        return false;
    };
    let hash1 = average_hash(&img1);
    let hash2 = average_hash(&img2);
    normalized_hamming_distance(&hash1, &hash2) <= SIMILARITY_THRESHOLD
}

/// 对齐 Kotlin：box 滤波缩放 32×32 → luma（0.299/0.587/0.114）→ 平均亮度 → 1024 bit。
fn average_hash(img: &image::DynamicImage) -> Vec<u8> {
    const SIZE: u32 = 32;
    let rgb = img.to_rgb8();
    let resized = resize_box(&rgb, SIZE, SIZE);
    let mut luma = Vec::with_capacity((SIZE * SIZE) as usize);
    for pixel in resized.pixels() {
        luma.push(
            (pixel[0] as f64) * 0.299 + (pixel[1] as f64) * 0.587 + (pixel[2] as f64) * 0.114,
        );
    }
    let avg = luma.iter().sum::<f64>() / luma.len() as f64;
    let mut bytes = vec![0u8; (luma.len() + 7) / 8];
    for (i, value) in luma.iter().enumerate() {
        // Kotlin: value < avg -> 0（prependZero），否则 1（prependOne）。
        if *value >= avg {
            bytes[i / 8] |= 1 << (i % 8);
        }
    }
    bytes
}

/// box 滤波（ResampleOp FILTER_BOX）等比缩放：目标像素取源区域像素均值。
fn resize_box(img: &image::RgbImage, width: u32, height: u32) -> image::RgbImage {
    let (src_w, src_h) = img.dimensions();
    let mut out = image::RgbImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let x0 = x * src_w / width;
            let x1 = (((x + 1) * src_w) / width).max(x0 + 1).min(src_w);
            let y0 = y * src_h / height;
            let y1 = (((y + 1) * src_h) / height).max(y0 + 1).min(src_h);
            let mut sum = [0u32; 3];
            let mut count = 0u32;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = img.get_pixel(sx, sy);
                    sum[0] += p[0] as u32;
                    sum[1] += p[1] as u32;
                    sum[2] += p[2] as u32;
                    count += 1;
                }
            }
            out.put_pixel(
                x,
                y,
                image::Rgb([
                    (sum[0] / count) as u8,
                    (sum[1] / count) as u8,
                    (sum[2] / count) as u8,
                ]),
            );
        }
    }
    out
}

fn normalized_hamming_distance(hash1: &[u8], hash2: &[u8]) -> f64 {
    let diff: u64 = hash1
        .iter()
        .zip(hash2)
        .map(|(a, b)| (a ^ b).count_ones() as u64)
        .sum();
    diff as f64 / (hash1.len() * 8) as f64
}

// ===================== Mapper（对应 ComicVineMetadataMapper.kt） =====================

pub struct ComicVineMetadataMapper {
    series_metadata_config: crate::config::SeriesMetadataConfig,
    book_metadata_config: crate::config::BookMetadataConfig,
    issue_name_template: Option<String>,
}

impl ComicVineMetadataMapper {
    pub fn new(
        series_metadata_config: crate::config::SeriesMetadataConfig,
        book_metadata_config: crate::config::BookMetadataConfig,
        issue_name_template: Option<String>,
    ) -> Self {
        Self {
            series_metadata_config,
            book_metadata_config,
            issue_name_template,
        }
    }

    /// 对应 `toSeriesMetadata`。title/titles 恒为 (name, null, null)；status/totalBookCount/language 恒 null。
    pub fn to_series_metadata(
        &self,
        volume: &ComicVineVolume,
        thumbnail: Option<Image>,
    ) -> ProviderSeriesMetadata {
        let cfg = &self.series_metadata_config;

        let title = SeriesTitle {
            name: volume.name.clone(),
            r#type: None,
            language: None,
        };

        let metadata = SeriesMetadata {
            status: None,
            title: cfg.title.then_some(title.clone()),
            titles: vec![title],
            summary: cfg
                .summary
                .then(|| volume.description.as_deref().map(parse_description))
                .flatten(),
            publisher: cfg
                .publisher
                .then(|| {
                    volume.publisher.as_ref().map(|p| Publisher {
                        name: p.name.clone(),
                        r#type: None,
                        language_tag: None,
                    })
                })
                .flatten(),
            alternative_publishers: Vec::new(),
            reading_direction: None,
            age_rating: None,
            language: None,
            genres: Vec::new(),
            tags: Vec::new(),
            total_book_count: None,
            authors: Vec::new(),
            release_date: cfg
                .release_date
                .then(|| {
                    volume
                        .start_year
                        .as_deref()
                        .and_then(|y| y.parse::<i32>().ok())
                        .map(|year| ReleaseDate::new(Some(year), None, None))
                })
                .flatten(),
            links: if cfg.links {
                volume
                    .site_detail_url
                    .as_ref()
                    .map(|url| WebLink {
                        label: "ComicVine".to_string(),
                        url: url.clone(),
                    })
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            },
            score: None,
            thumbnail,
        };

        let books = if cfg.books {
            volume
                .issues
                .iter()
                .flatten()
                .map(|issue| SeriesBook {
                    id: ProviderBookId(issue.id.to_string()),
                    number: issue
                        .issue_number
                        .as_deref()
                        .and_then(|n| n.parse::<f64>().ok())
                        .map(BookRange::single),
                    // 对齐 Kotlin：series books 的 name 直接用 issue.name（不套 issueNameTemplate）。
                    name: issue.name.clone(),
                    r#type: None,
                    edition: None,
                })
                .collect()
        } else {
            Vec::new()
        };

        ProviderSeriesMetadata {
            id: ProviderSeriesId(volume.id.to_string()),
            metadata,
            books,
        }
    }

    /// 对应 `toSeriesSearchResult(ComicVineVolumeSearch)`。
    pub fn to_series_search_result(
        &self,
        volume: &ComicVineVolumeSearchResult,
    ) -> SeriesSearchResult {
        SeriesSearchResult {
            url: volume.site_detail_url.clone(),
            image_url: volume.image.as_ref().and_then(|i| i.medium_url.clone()),
            title: series_title(
                &volume.name,
                volume.start_year.as_deref(),
                volume.publisher.as_ref().map(|p| p.name.as_str()),
            ),
            provider: CoreProviders::ComicVine.as_str().to_string(),
            result_id: volume.id.to_string(),
            media_type: None,
            language: None,
            nsfw: None,
        }
    }

    /// 对应 `toSeriesSearchResult(ComicVineVolume)`（idFormat 直查时使用）。
    pub fn to_series_search_result_volume(&self, volume: &ComicVineVolume) -> SeriesSearchResult {
        SeriesSearchResult {
            url: volume.site_detail_url.clone(),
            image_url: volume.image.as_ref().and_then(|i| i.medium_url.clone()),
            title: series_title(
                &volume.name,
                volume.start_year.as_deref(),
                volume.publisher.as_ref().map(|p| p.name.as_str()),
            ),
            provider: CoreProviders::ComicVine.as_str().to_string(),
            result_id: volume.id.to_string(),
            media_type: None,
            language: None,
            nsfw: None,
        }
    }

    /// 对应 `toBookMetadata(issue, storyArcs, cover)`。
    pub fn to_book_metadata(
        &self,
        issue: &ComicVineIssue,
        story_arcs: &[ComicVineStoryArc],
        cover: Option<Image>,
    ) -> ProviderBookMetadata {
        let cfg = &self.book_metadata_config;
        let issue_number = issue
            .issue_number
            .as_deref()
            .and_then(|n| n.parse::<f64>().ok());
        let number = issue_number.map(BookRange::single);

        // Kotlin: issue.name ?: issueNameTemplate?.replace("{number}", issueNumber.toString())
        // issueNumber 是 BookRange?，toString() 在 null 时为 "null"。
        let title = issue.name.clone().or_else(|| {
            self.issue_name_template.as_ref().map(|t| {
                t.replace(
                    "{number}",
                    &number
                        .as_ref()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "null".to_string()),
                )
            })
        });

        let story_arcs = if story_arcs.is_empty() {
            None
        } else {
            Some(
                story_arcs
                    .iter()
                    .map(|arc| {
                        let index = arc
                            .issues
                            .iter()
                            .position(|i| i.id == issue.id)
                            .unwrap_or_else(|| {
                                panic!(
                                    "Story arc does not contain this issue arcs:{:?}; issue {} ",
                                    arc.issues.iter().map(|i| i.id).collect::<Vec<_>>(),
                                    issue.id
                                )
                            });
                        BookStoryArc {
                            name: arc.name.clone(),
                            number: (index + 1) as i32,
                        }
                    })
                    .collect(),
            )
        };

        let metadata = BookMetadata {
            title: cfg.title.then_some(title).flatten(),
            summary: cfg
                .summary
                .then(|| issue.description.as_deref().map(parse_description))
                .flatten(),
            number: cfg.number.then_some(number).flatten(),
            number_sort: cfg.number_sort.then_some(issue_number).flatten(),
            release_date: cfg
                .release_date
                .then(|| {
                    issue
                        .store_date
                        .clone()
                        .or_else(|| issue.cover_date.clone())
                })
                .flatten(),
            authors: if cfg.authors {
                get_authors(issue)
            } else {
                Vec::new()
            },
            tags: Vec::new(),
            isbn: None,
            links: if cfg.links {
                issue
                    .site_detail_url
                    .as_ref()
                    .map(|url| WebLink {
                        label: "ComicVine".to_string(),
                        url: url.clone(),
                    })
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            },
            chapters: Vec::new(),
            story_arcs,
            start_chapter: None,
            end_chapter: None,
            thumbnail: cfg.thumbnail.then_some(cover).flatten(),
        };

        ProviderBookMetadata {
            id: Some(ProviderBookId(issue.id.to_string())),
            series_id: None,
            metadata,
        }
    }
}

/// 对应 `getAuthors`：personCredits 的 role 按 ", " 拆分并映射到 AuthorRole。
fn get_authors(issue: &ComicVineIssue) -> Vec<Author> {
    let mut authors = Vec::new();
    if let Some(credits) = &issue.person_credits {
        for person in credits {
            for role in person.role.split(", ") {
                match role {
                    "writer" | "plotter" | "scripter" => {
                        authors.push(Author {
                            name: person.name.clone(),
                            role: AuthorRole::Writer,
                        });
                    }
                    "penciller" | "penciler" | "breakdowns" => {
                        authors.push(Author {
                            name: person.name.clone(),
                            role: AuthorRole::Penciller,
                        });
                    }
                    "inker" | "finishes" => {
                        authors.push(Author {
                            name: person.name.clone(),
                            role: AuthorRole::Inker,
                        });
                    }
                    "colorist" | "colourist" | "colorer" | "colourer" => {
                        authors.push(Author {
                            name: person.name.clone(),
                            role: AuthorRole::Colorist,
                        });
                    }
                    "letterer" => {
                        authors.push(Author {
                            name: person.name.clone(),
                            role: AuthorRole::Letterer,
                        });
                    }
                    "cover" | "covers" | "coverartist" | "cover artist" => {
                        authors.push(Author {
                            name: person.name.clone(),
                            role: AuthorRole::Cover,
                        });
                    }
                    "editor" => {
                        authors.push(Author {
                            name: person.name.clone(),
                            role: AuthorRole::Editor,
                        });
                    }
                    "artist" => {
                        authors.push(Author {
                            name: person.name.clone(),
                            role: AuthorRole::Penciller,
                        });
                        authors.push(Author {
                            name: person.name.clone(),
                            role: AuthorRole::Inker,
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    authors
}

/// 对应 `seriesTitle`：`${name}${startYear}${publisher}`。
fn series_title(name: &str, start_year: Option<&str>, publisher: Option<&str>) -> String {
    let start_year_string = start_year.map(|y| format!(" ({y})")).unwrap_or_default();
    let publisher_string = publisher.map(|p| format!(" ({p})")).unwrap_or_default();
    format!("{name}{start_year_string}{publisher_string}")
}

// ===================== HTML 描述解析（对应 mapper 的 parseDescription） =====================
//
// Kotlin 用 Ksoup.parse(description).child(0).child(1).children() 取 body 的直接元素子节点，
// 然后递归拼接：br/p/h2/h3/h4 前加 "\n\n"，li 前加 "\n  - "，TextNode 保留原文，最终 trim。

enum HtmlNode {
    Text(String),
    Element {
        tag: String,
        children: Vec<HtmlNode>,
    },
}

fn parse_description(html: &str) -> String {
    let body = body_inner(html);
    let nodes = parse_fragment(body);
    let mut out = String::new();
    for node in &nodes {
        // 对齐 Kotlin body.children()：只保留顶层元素节点（直接文本节点被丢弃）。
        if let HtmlNode::Element { .. } = node {
            render_element(node, &mut out);
        }
    }
    out.trim().to_string()
}

/// 提取 <body ...>...</body> 内部；无 body 时原样返回。
fn body_inner(input: &str) -> &str {
    if let Some(body_start) = input.find("<body") {
        if let Some(gt) = input[body_start..].find('>') {
            let content_start = body_start + gt + 1;
            if let Some(body_end) = input[content_start..].find("</body>") {
                return &input[content_start..content_start + body_end];
            }
        }
    }
    input
}

/// 简易片段解析：返回顶层节点列表（元素 + 文本）。自闭合与 void 标签无 children。
fn parse_fragment(input: &str) -> Vec<HtmlNode> {
    let mut nodes = Vec::new();
    let mut rest = input;
    while let Some(lt) = rest.find('<') {
        if lt > 0 {
            nodes.push(HtmlNode::Text(rest[..lt].to_string()));
        }
        rest = &rest[lt..];
        if rest.starts_with("</") {
            break;
        }
        let Some(gt) = rest.find('>') else {
            break;
        };
        let tag_str = &rest[1..gt];
        let tag_name = tag_str.split_whitespace().next().unwrap_or("").to_string();
        if tag_name.is_empty() {
            rest = &rest[gt + 1..];
            continue;
        }
        let self_closing = tag_str.ends_with('/') || is_void_tag(&tag_name);
        if self_closing {
            nodes.push(HtmlNode::Element {
                tag: tag_name,
                children: Vec::new(),
            });
            rest = &rest[gt + 1..];
            continue;
        }
        let close_tag = format!("</{tag_name}");
        let content_start = gt + 1;
        let (content_end, after) = match_close_tag(rest, &close_tag, content_start);
        let children = parse_fragment(&rest[content_start..content_end]);
        nodes.push(HtmlNode::Element {
            tag: tag_name,
            children,
        });
        rest = after;
    }
    nodes
}

/// 从 start 起寻找与 `close_tag` 匹配的结束标签（嵌套计数），返回 (内容结束偏移, 匹配后的剩余)。
fn match_close_tag<'a>(input: &'a str, close_tag: &str, start: usize) -> (usize, &'a str) {
    let open_tag = format!("<{}", &close_tag[2..]);
    let mut depth = 0usize;
    let mut pos = start;
    while pos < input.len() {
        let remaining = &input[pos..];
        if let Some(idx) = remaining.find('<') {
            let tag_start = pos + idx;
            let candidate = &input[tag_start..];
            if candidate.starts_with(&close_tag) {
                if depth == 0 {
                    return (tag_start, &candidate[close_tag.len()..]);
                }
                depth -= 1;
                pos = tag_start + close_tag.len();
            } else if candidate.starts_with(&open_tag) {
                depth += 1;
                pos = tag_start + open_tag.len();
            } else {
                pos = tag_start + 1;
            }
        } else {
            break;
        }
    }
    (input.len(), "")
}

fn is_void_tag(tag: &str) -> bool {
    matches!(
        tag,
        "br" | "hr"
            | "img"
            | "input"
            | "meta"
            | "link"
            | "area"
            | "base"
            | "col"
            | "embed"
            | "source"
            | "track"
            | "wbr"
    )
}

fn render_element(node: &HtmlNode, out: &mut String) {
    if let HtmlNode::Element { tag, children } = node {
        match tag.as_str() {
            "br" | "p" | "h2" | "h3" | "h4" => out.push_str("\n\n"),
            "li" => out.push_str("\n  - "),
            _ => {}
        }
        for child in children {
            match child {
                HtmlNode::Text(text) => out.push_str(text),
                HtmlNode::Element { .. } => render_element(child, out),
            }
        }
    }
}

// ===================== Provider（对应 ComicVineMetadataProvider.kt） =====================

/// storyArcCache（cache4k expireAfterWrite(30.minutes)）。
struct StoryArcCache {
    inner: Mutex<HashMap<i32, (Instant, ComicVineStoryArc)>>,
}

impl StoryArcCache {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    async fn get_or_fetch<F, Fut>(
        &self,
        id: i32,
        fetch: F,
    ) -> Result<ComicVineStoryArc, ProviderError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<ComicVineStoryArc, ProviderError>>,
    {
        {
            let inner = self.inner.lock().await;
            if let Some((written_at, arc)) = inner.get(&id) {
                if written_at.elapsed() < Duration::from_secs(30 * 60) {
                    return Ok(arc.clone());
                }
            }
        }
        let arc = fetch().await?;
        self.inner
            .lock()
            .await
            .insert(id, (Instant::now(), arc.clone()));
        Ok(arc)
    }
}

pub struct ComicVineMetadataProvider {
    client: ComicVineClient,
    mapper: ComicVineMetadataMapper,
    name_matcher: NameSimilarityMatcher,
    fetch_series_covers: bool,
    fetch_book_covers: bool,
    id_format: Option<String>,
    story_arc_cache: Arc<StoryArcCache>,
}

#[allow(clippy::too_many_arguments)]
pub fn create_provider(
    config: &ProviderConfig,
    api_key: Option<&str>,
    search_limit: Option<i32>,
    issue_name: Option<&str>,
    id_format: Option<&str>,
    default_name_matcher: NameSimilarityMatcher,
    http_client: &reqwest::Client,
) -> Option<ComicVineMetadataProvider> {
    if !config.enabled {
        return None;
    }
    let api_key = api_key?;
    let name_matcher = config.name_matching_mode.unwrap_or(default_name_matcher);
    Some(ComicVineMetadataProvider {
        client: ComicVineClient::new(
            http_client.clone(),
            api_key.to_string(),
            search_limit.unwrap_or(10),
        ),
        mapper: ComicVineMetadataMapper::new(
            config.series_metadata.clone(),
            config.book_metadata.clone(),
            issue_name.map(|s| s.to_string()),
        ),
        name_matcher,
        fetch_series_covers: config.series_metadata.thumbnail,
        fetch_book_covers: config.book_metadata.thumbnail,
        id_format: id_format.map(|s| s.to_string()),
        story_arc_cache: Arc::new(StoryArcCache::new()),
    })
}

/// 对应 Kotlin `extractVolumeId`：idFormat 中 `{id}` 前后缀 Regex.escape + `(?<id>\d+)`。
fn parse_comic_vine_id(name: &str, id_format: &str) -> Option<i32> {
    let parts: Vec<&str> = id_format.splitn(2, "{id}").collect();
    if parts.len() != 2 {
        return None;
    }
    let pattern = format!(
        r"{}(?P<id>\d+){}",
        regex::escape(parts[0]),
        regex::escape(parts[1])
    );
    let regex = regex::Regex::new(&pattern).ok()?;
    regex
        .captures(name)
        .and_then(|c| c.name("id"))
        .and_then(|m| m.as_str().parse().ok())
}

/// 对应 `extractYear`：`\((?<startYear>\d{4})(-\d{4})?\)`。
fn extract_year(series_name: &str) -> Option<i32> {
    let regex = regex::Regex::new(r"\((?P<startYear>\d{4})(-\d{4})?\)").ok()?;
    regex
        .captures(series_name)
        .and_then(|c| c.name("startYear"))
        .and_then(|m| m.as_str().parse().ok())
}

/// 对应 `removeParentheses`：`[({\[]([^)}\]]+)[)}\]]`。
fn remove_parentheses(series_name: &str) -> String {
    let regex = regex::Regex::new(r"[(\[{]([^)}\]]+)[)}\]]").unwrap();
    regex.replace_all(series_name, "").trim().to_string()
}

impl ComicVineMetadataProvider {
    /// 对应 `getCover`：mediumUrl ?: smallUrl ?: originalUrl，404 时忽略返回 null。
    async fn get_cover(&self, image: &ComicVineImage) -> Result<Option<Image>, ProviderError> {
        let Some(url) = image
            .medium_url
            .clone()
            .or_else(|| image.small_url.clone())
            .or_else(|| image.original_url.clone())
        else {
            return Ok(None);
        };
        self.client.get_cover(&url).await
    }

    /// 对应 `resultMatchFilter`。
    fn result_match_filter(
        &self,
        match_query: &MatchQuery,
        result: &ComicVineVolumeSearchResult,
    ) -> bool {
        let start_year = match_query
            .start_year
            .or_else(|| extract_year(&match_query.series_name));
        let series_name =
            match_query.normalize_title(&remove_parentheses(&match_query.series_name));

        if !self
            .name_matcher
            .matches_single(&series_name, &match_query.normalize_title(&result.name))
        {
            return false;
        }
        if start_year.is_none() || result.start_year.is_none() {
            return true;
        }
        let Some(result_year) = result
            .start_year
            .as_deref()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            return false;
        };
        start_year == Some(result_year)
    }

    /// 对应 `matchesBookCover`：firstIssueNumber 匹配 qualifier 后比对 phash。
    async fn matches_book_cover(
        &self,
        volume: &ComicVineVolumeSearchResult,
        qualifier: &crate::model::BookQualifier,
    ) -> Result<bool, ProviderError> {
        let first_issue_number = volume
            .first_issue
            .as_ref()
            .and_then(|issue| issue.issue_number.as_deref())
            .and_then(|n| n.parse::<f64>().ok());
        let Some(first_issue_number) = first_issue_number else {
            return Ok(false);
        };
        let Some(qualifier_image) = &qualifier.cover else {
            return Ok(false);
        };
        let Some(first_issue) = &volume.first_issue else {
            return Ok(false);
        };

        tracing::info!(
            "matching cover of volume \"{}\" {:?}",
            volume.name,
            volume.site_detail_url
        );

        if qualifier.number.start != first_issue_number {
            return Ok(false);
        }

        let issue = self.client.get_issue(first_issue.id).await?;
        let issue_cover = match issue.image.as_ref() {
            Some(image) => self.get_cover(image).await?,
            None => None,
        };
        let Some(issue_cover) = issue_cover else {
            return Ok(false);
        };
        Ok(compare_images(&qualifier_image.bytes, &issue_cover.bytes))
    }
}

#[async_trait::async_trait]
impl MetadataProvider for ComicVineMetadataProvider {

    fn resolve_link_id(&self, query: &str) -> Option<String> {
        let re = regex::Regex::new(r"comicvine\.gamespot\.com/[^/]+/(\d+)-(\d+)").ok()?;
        re.captures(query)
            .map(|c| c.get(2).unwrap().as_str().to_string())
    }
    fn provider_name(&self) -> CoreProviders {
        CoreProviders::ComicVine
    }

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        let id: i32 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid ComicVine series id: {}", series_id.0))
        })?;
        let series = self.client.get_volume(id).await?;
        let cover = if self.fetch_series_covers {
            match series.image.as_ref() {
                Some(image) => self.get_cover(image).await?,
                None => None,
            }
        } else {
            None
        };
        Ok(self.mapper.to_series_metadata(&series, cover))
    }

    async fn get_series_cover(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError> {
        let id: i32 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid ComicVine series id: {}", series_id.0))
        })?;
        let series = self.client.get_volume(id).await?;
        match series.image.as_ref() {
            Some(image) => self.get_cover(image).await,
            None => Ok(None),
        }
    }

    async fn get_book_metadata(
        &self,
        _series_id: &ProviderSeriesId,
        book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError> {
        let id: i32 = book_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid ComicVine issue id: {}", book_id.0))
        })?;
        // 对齐 Kotlin：getIssue 直查（不走 volume.issues 内查找）。
        let issue = self.client.get_issue(id).await?;
        let story_arcs = if let Some(credits) = &issue.story_arc_credits {
            let mut arcs = Vec::new();
            for credit in credits {
                let id = credit.id;
                let arc = self
                    .story_arc_cache
                    .get_or_fetch(id, || async move { self.client.get_story_arc(id).await })
                    .await?;
                arcs.push(arc);
            }
            arcs
        } else {
            Vec::new()
        };
        let cover = if self.fetch_book_covers {
            match issue.image.as_ref() {
                Some(image) => self.get_cover(image).await?,
                None => None,
            }
        } else {
            None
        };
        Ok(self.mapper.to_book_metadata(&issue, &story_arcs, cover))
    }

    async fn search_series(
        &self,
        series_name: &str,
        _limit: usize,
        _media_type: Option<crate::model::MediaType>,
    ) -> Result<Vec<SeriesSearchResult>, ProviderError> {
        // 对齐 Kotlin：idFormat 存在时先从 seriesName 提取 id，直查 volume。
        if let Some(id_format) = &self.id_format {
            if let Some(extracted_id) = parse_comic_vine_id(series_name, id_format) {
                let result = self.client.get_volume(extracted_id).await?;
                return Ok(vec![self.mapper.to_series_search_result_volume(&result)]);
            }
        }
        let cleaned: String = series_name.replace('<', "").chars().take(400).collect();
        let result = self.client.search_volumes(&cleaned).await?;
        Ok(result
            .iter()
            .map(|v| self.mapper.to_series_search_result(v))
            .collect())
    }

    async fn match_series_metadata(
        &self,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        let series_name = remove_parentheses(&match_query.series_name);

        // 对齐 Kotlin：seriesFolder 优先提取 id；folder 有值但未提取到再从 seriesName 提取。
        if let Some(id_format) = &self.id_format {
            let mut extracted_id = match &match_query.series_folder {
                Some(folder) => parse_comic_vine_id(folder, id_format),
                None => parse_comic_vine_id(&match_query.series_name, id_format),
            };
            if match_query.series_folder.is_some() && extracted_id.is_none() {
                extracted_id = parse_comic_vine_id(&match_query.series_name, id_format);
            }
            if let Some(id) = extracted_id {
                let result = self.client.get_volume(id).await?;
                return Ok(Some(self.mapper.to_series_metadata(&result, None)));
            }
        }

        let cleaned: String = series_name.replace('<', "").chars().take(400).collect();
        let search_results = self.client.search_volumes(&cleaned).await?;

        let results: Vec<ComicVineVolumeSearchResult> = search_results
            .into_iter()
            .filter(|result| self.result_match_filter(match_query, result))
            .collect();

        if results.len() > 1 {
            tracing::info!(
                "Multiple series matches: {}",
                results
                    .iter()
                    .map(|r| format!("\"{}\" id={}", r.name, r.id))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let Some(book_qualifier) = &match_query.book_qualifier else {
                return Ok(None);
            };
            tracing::info!(
                "Attempting to match using cover of book number: {} name: \"{}\"",
                book_qualifier.number,
                book_qualifier.name
            );
            for result in results.iter() {
                if self.matches_book_cover(result, book_qualifier).await? {
                    let volume = self.client.get_volume(result.id).await?;
                    return Ok(Some(self.mapper.to_series_metadata(&volume, None)));
                }
            }
            return Ok(None);
        }

        let Some(result) = results.first() else {
            return Ok(None);
        };
        let volume = self.client.get_volume(result.id).await?;
        Ok(Some(self.mapper.to_series_metadata(&volume, None)))
    }
}
