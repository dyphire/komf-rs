//! Kavita REST 客户端 —— 全量对齐 `snd.komf.mediaserver.kavita` 包。
//!
//! 使用 Kavita API v1（`/api/...`），通过 API Key 获取 JWT 后以 Bearer 认证；
//! 封面图片端点以 `apiKey` 查询参数认证（与 Kotlin 一致）。
//! 更新类请求受 120 次/60 秒限流保护（对应 Kotlin `rateLimiter`）。
//! 注：Kotlin 版经 `JwtConsumer` 校验 JWT 过期时间；本移植解析 JWT payload
//! （不验证签名，仅取过期时间）实现相同语义的令牌刷新。
use crate::client::{MediaServerClient, MediaServerError};
use crate::model::*;
use base64::Engine;
use komf_core::model::{Image, SeriesStatus, WebLink};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// 枚举 —— 对应 `KavitaAgeRating.kt` / `KavitaSeries.kt`（KavitaPublicationStatus）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KavitaAgeRating {
    NotApplicable,
    Unknown,
    RatingPending,
    EarlyChildhood,
    Everyone,
    G,
    Everyone10Plus,
    Pg,
    KidsToAdults,
    Teen,
    Mature15Plus,
    Mature17Plus,
    Mature,
    R18Plus,
    AdultsOnly,
    X18Plus,
}

impl KavitaAgeRating {
    fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            -1 => Self::NotApplicable,
            0 => Self::Unknown,
            1 => Self::RatingPending,
            2 => Self::EarlyChildhood,
            3 => Self::Everyone,
            4 => Self::G,
            5 => Self::Everyone10Plus,
            6 => Self::Pg,
            7 => Self::KidsToAdults,
            8 => Self::Teen,
            9 => Self::Mature15Plus,
            10 => Self::Mature17Plus,
            11 => Self::Mature,
            12 => Self::R18Plus,
            13 => Self::AdultsOnly,
            14 => Self::X18Plus,
            _ => return None,
        })
    }

    fn id(self) -> i32 {
        match self {
            Self::NotApplicable => -1,
            Self::Unknown => 0,
            Self::RatingPending => 1,
            Self::EarlyChildhood => 2,
            Self::Everyone => 3,
            Self::G => 4,
            Self::Everyone10Plus => 5,
            Self::Pg => 6,
            Self::KidsToAdults => 7,
            Self::Teen => 8,
            Self::Mature15Plus => 9,
            Self::Mature17Plus => 10,
            Self::Mature => 11,
            Self::R18Plus => 12,
            Self::AdultsOnly => 13,
            Self::X18Plus => 14,
        }
    }

    /// 对应 Kotlin `ageRating.ageRating`（面向读者的评级数值；UNKNOWN/NOT_APPLICABLE 为 None）。
    fn age_rating_value(self) -> Option<i32> {
        match self {
            Self::NotApplicable | Self::Unknown => None,
            Self::RatingPending | Self::Everyone | Self::G => Some(0),
            Self::EarlyChildhood => Some(3),
            Self::KidsToAdults => Some(6),
            Self::Pg => Some(8),
            Self::Everyone10Plus => Some(10),
            Self::Teen => Some(13),
            Self::Mature15Plus => Some(15),
            Self::Mature17Plus | Self::Mature => Some(17),
            Self::R18Plus | Self::AdultsOnly | Self::X18Plus => Some(18),
        }
    }

    /// 对应 Kotlin `toKavitaSeriesMetadataUpdate` 中的 ageRating 升档映射：
    /// 取所有非空 ageRating 中第一个 >= 目标值的评级，否则 ADULTS_ONLY。
    fn from_rating(target: i32) -> Self {
        // 保持与 Kotlin 声明顺序一致（stable sort 保留重复值顺序）
        const CANDIDATES: [(KavitaAgeRating, i32); 14] = [
            (KavitaAgeRating::RatingPending, 0),
            (KavitaAgeRating::Everyone, 0),
            (KavitaAgeRating::G, 0),
            (KavitaAgeRating::EarlyChildhood, 3),
            (KavitaAgeRating::KidsToAdults, 6),
            (KavitaAgeRating::Pg, 8),
            (KavitaAgeRating::Everyone10Plus, 10),
            (KavitaAgeRating::Teen, 13),
            (KavitaAgeRating::Mature15Plus, 15),
            (KavitaAgeRating::Mature17Plus, 17),
            (KavitaAgeRating::Mature, 17),
            (KavitaAgeRating::R18Plus, 18),
            (KavitaAgeRating::AdultsOnly, 18),
            (KavitaAgeRating::X18Plus, 18),
        ];
        CANDIDATES
            .iter()
            .find(|(_, value)| *value >= target)
            .map(|(rating, _)| *rating)
            .unwrap_or(KavitaAgeRating::AdultsOnly)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KavitaPublicationStatus {
    Ongoing,
    Hiatus,
    Completed,
    Cancelled,
    Ended,
}

impl KavitaPublicationStatus {
    fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            0 => Self::Ongoing,
            1 => Self::Hiatus,
            2 => Self::Completed,
            3 => Self::Cancelled,
            4 => Self::Ended,
            _ => return None,
        })
    }

    fn id(self) -> i32 {
        match self {
            Self::Ongoing => 0,
            Self::Hiatus => 1,
            Self::Completed => 2,
            Self::Cancelled => 3,
            Self::Ended => 4,
        }
    }
}

// ---------------------------------------------------------------------------
// 响应 DTO（camelCase，与 Kavita API / Kotlin 模型一致）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaAuthResponse {
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaSeries {
    pub id: i32,
    pub name: String,
    pub library_id: i32,
    pub library_name: String,
    pub original_name: String,
    pub localized_name: Option<String>,
    pub sort_name: String,
    pub pages: i32,
    pub format: i32,
    pub created: String,
    pub folder_path: String,
    pub cover_image_locked: bool,
    pub localized_name_locked: bool,
    pub sort_name_locked: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaSeriesDetails {
    pub total_count: i32,
    pub chapters: Option<Vec<KavitaChapter>>,
    pub storyline_chapters: Option<Vec<KavitaChapter>>,
    pub unread_count: Option<i32>,
    pub volumes: Option<Vec<KavitaVolume>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaSeriesMetadata {
    pub id: i32,
    pub series_id: i32,
    pub summary: Option<String>,
    #[serde(default)]
    pub genres: Vec<KavitaGenre>,
    #[serde(default)]
    pub tags: Vec<KavitaTag>,
    #[serde(default)]
    pub writers: Vec<KavitaAuthor>,
    #[serde(default)]
    pub cover_artists: Vec<KavitaAuthor>,
    #[serde(default)]
    pub publishers: Vec<KavitaAuthor>,
    #[serde(default)]
    pub characters: Vec<KavitaAuthor>,
    #[serde(default)]
    pub pencillers: Vec<KavitaAuthor>,
    #[serde(default)]
    pub inkers: Vec<KavitaAuthor>,
    #[serde(default)]
    pub imprints: Vec<KavitaAuthor>,
    #[serde(default)]
    pub colorists: Vec<KavitaAuthor>,
    #[serde(default)]
    pub letterers: Vec<KavitaAuthor>,
    #[serde(default)]
    pub editors: Vec<KavitaAuthor>,
    #[serde(default)]
    pub translators: Vec<KavitaAuthor>,
    #[serde(default)]
    pub teams: Vec<KavitaAuthor>,
    #[serde(default)]
    pub locations: Vec<KavitaAuthor>,
    pub age_rating: i32,
    pub release_year: i32,
    pub language: Option<String>,
    pub max_count: i32,
    pub total_count: i32,
    pub publication_status: i32,
    pub web_links: Option<String>,

    #[serde(default)]
    pub language_locked: bool,
    #[serde(default)]
    pub summary_locked: bool,
    #[serde(default)]
    pub age_rating_locked: bool,
    #[serde(default)]
    pub publication_status_locked: bool,
    #[serde(default)]
    pub genres_locked: bool,
    #[serde(default)]
    pub tags_locked: bool,
    #[serde(default)]
    pub writer_locked: bool,
    #[serde(default)]
    pub character_locked: bool,
    #[serde(default)]
    pub colorist_locked: bool,
    #[serde(default)]
    pub editor_locked: bool,
    #[serde(default)]
    pub inker_locked: bool,
    #[serde(default)]
    pub imprint_locked: bool,
    #[serde(default)]
    pub letterer_locked: bool,
    #[serde(default)]
    pub penciller_locked: bool,
    #[serde(default)]
    pub publisher_locked: bool,
    #[serde(default)]
    pub translator_locked: bool,
    #[serde(default)]
    pub team_locked: bool,
    #[serde(default)]
    pub location_locked: bool,
    #[serde(default)]
    pub cover_artist_locked: bool,
    #[serde(default)]
    pub release_year_locked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaAuthor {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaGenre {
    pub id: i32,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaTag {
    pub id: i32,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaVolume {
    pub id: i32,
    pub min_number: f32,
    pub max_number: f32,
    pub name: String,
    pub pages: i32,
    pub series_id: i32,
    #[serde(default)]
    pub chapters: Vec<KavitaChapter>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaChapter {
    pub id: i32,
    pub range: Option<String>,
    pub number: Option<String>,
    pub pages: i32,
    pub is_special: bool,
    pub title: String,
    #[serde(default)]
    pub files: Vec<KavitaChapterFile>,
    pub pages_read: i32,
    pub cover_image_locked: bool,
    pub volume_id: i32,
    pub created_utc: String,
    pub count: i32,
    pub total_count: i32,

    pub summary: Option<String>,
    #[serde(default)]
    pub genres: Vec<KavitaGenre>,
    #[serde(default)]
    pub tags: Vec<KavitaTag>,
    pub age_rating: i32,
    pub language: Option<String>,
    pub web_links: String,
    pub isbn: String,
    pub release_date: String,
    pub title_name: String,
    pub sort_order: f64,

    #[serde(default)]
    pub writers: Vec<KavitaAuthor>,
    #[serde(default)]
    pub cover_artists: Vec<KavitaAuthor>,
    #[serde(default)]
    pub publishers: Vec<KavitaAuthor>,
    #[serde(default)]
    pub characters: Vec<KavitaAuthor>,
    #[serde(default)]
    pub pencillers: Vec<KavitaAuthor>,
    #[serde(default)]
    pub inkers: Vec<KavitaAuthor>,
    #[serde(default)]
    pub imprints: Vec<KavitaAuthor>,
    #[serde(default)]
    pub colorists: Vec<KavitaAuthor>,
    #[serde(default)]
    pub letterers: Vec<KavitaAuthor>,
    #[serde(default)]
    pub editors: Vec<KavitaAuthor>,
    #[serde(default)]
    pub translators: Vec<KavitaAuthor>,
    #[serde(default)]
    pub teams: Vec<KavitaAuthor>,
    #[serde(default)]
    pub locations: Vec<KavitaAuthor>,

    #[serde(default)]
    pub age_rating_locked: bool,
    #[serde(default)]
    pub genres_locked: bool,
    #[serde(default)]
    pub tags_locked: bool,
    #[serde(default)]
    pub writer_locked: bool,
    #[serde(default)]
    pub character_locked: bool,
    #[serde(default)]
    pub colorist_locked: bool,
    #[serde(default)]
    pub editor_locked: bool,
    #[serde(default)]
    pub inker_locked: bool,
    #[serde(default)]
    pub imprint_locked: bool,
    #[serde(default)]
    pub letterer_locked: bool,
    #[serde(default)]
    pub penciller_locked: bool,
    #[serde(default)]
    pub publisher_locked: bool,
    #[serde(default)]
    pub translator_locked: bool,
    #[serde(default)]
    pub team_locked: bool,
    #[serde(default)]
    pub location_locked: bool,
    #[serde(default)]
    pub cover_artist_locked: bool,
    #[serde(default)]
    pub language_locked: bool,
    #[serde(default)]
    pub summary_locked: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaChapterFile {
    pub id: i32,
    pub file_path: String,
    pub pages: i32,
    pub format: i32,
    pub created: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaLibrary {
    pub id: i32,
    pub name: String,
    pub last_scanned: String,
    pub r#type: i32,
    #[serde(default)]
    pub folders: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaPagination {
    pub current_page: i32,
    pub items_per_page: i32,
    pub total_items: i32,
    pub total_pages: i32,
}

// ---------------------------------------------------------------------------
// 请求体（camelCase，对应 `kavita/model/request/*`）
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaSeriesUpdateRequest {
    pub id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub localized_name: Option<String>,
    pub sort_name: String,
    pub cover_image_locked: bool,
    pub sort_name_locked: bool,
    pub localized_name_locked: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaSeriesMetadataUpdateRequest {
    pub series_metadata: KavitaSeriesMetadata,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaChapterMetadataUpdateRequest {
    pub id: i32,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub genres: Vec<KavitaGenre>,
    pub tags: Vec<KavitaTag>,
    pub age_rating: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub weblinks: String,
    pub isbn: String,
    pub release_date: String,
    pub title_name: String,
    pub sort_order: f64,

    pub writers: Vec<KavitaAuthor>,
    pub cover_artists: Vec<KavitaAuthor>,
    pub publishers: Vec<KavitaAuthor>,
    pub characters: Vec<KavitaAuthor>,
    pub pencillers: Vec<KavitaAuthor>,
    pub inkers: Vec<KavitaAuthor>,
    pub imprints: Vec<KavitaAuthor>,
    pub colorists: Vec<KavitaAuthor>,
    pub letterers: Vec<KavitaAuthor>,
    pub editors: Vec<KavitaAuthor>,
    pub translators: Vec<KavitaAuthor>,
    pub teams: Vec<KavitaAuthor>,
    pub locations: Vec<KavitaAuthor>,

    pub age_rating_locked: bool,
    pub title_name_locked: bool,
    pub genres_locked: bool,
    pub tags_locked: bool,
    pub writer_locked: bool,
    pub character_locked: bool,
    pub colorist_locked: bool,
    pub editor_locked: bool,
    pub inker_locked: bool,
    pub imprint_locked: bool,
    pub letterer_locked: bool,
    pub penciller_locked: bool,
    pub publisher_locked: bool,
    pub translator_locked: bool,
    pub team_locked: bool,
    pub location_locked: bool,
    pub cover_artist_locked: bool,
    pub language_locked: bool,
    pub summary_locked: bool,
    pub isbn_locked: bool,
    pub release_date_locked: bool,
    pub sort_order_locked: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaCoverUploadRequest {
    pub id: i32,
    pub url: String,
    pub lock_cover: bool,
}

// ---------------------------------------------------------------------------
// 简单滑动窗口限流器 —— 对应 Kotlin `rateLimiter(120, 60s)`
// ---------------------------------------------------------------------------

struct RateLimiter {
    max: usize,
    window: Duration,
    timestamps: tokio::sync::Mutex<VecDeque<Instant>>,
}

impl RateLimiter {
    fn new(max: usize, window: Duration) -> Self {
        Self {
            max,
            window,
            timestamps: tokio::sync::Mutex::new(VecDeque::new()),
        }
    }

    async fn acquire(&self) {
        loop {
            {
                let mut queue = self.timestamps.lock().await;
                let now = Instant::now();
                while queue.front().is_some_and(|t| now.duration_since(*t) >= self.window) {
                    queue.pop_front();
                }
                if queue.len() < self.max {
                    queue.push_back(now);
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// JWT 令牌提供者 —— 对应 `KavitaTokenProvider.kt`（简化版，不验证签名）
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct KavitaTokenProvider {
    client: KavitaAuthClient,
    api_key: String,
    token: std::sync::Mutex<Option<(String, Instant)>>,
}

impl KavitaTokenProvider {
    pub fn new(client: KavitaAuthClient, api_key: String) -> Self {
        Self {
            client,
            api_key,
            token: std::sync::Mutex::new(None),
        }
    }

    /// 返回有效 JWT；过期（或缺失）时重新认证。
    pub async fn get_token(&self) -> Result<String, MediaServerError> {
        let expires_at = {
            let guard = self.token.lock().unwrap();
            guard.as_ref().map(|(_, expires_at)| *expires_at)
        };
        if let Some(expires_at) = expires_at {
            if Instant::now() < expires_at {
                if let Ok(guard) = self.token.lock() {
                    if let Some((token, _)) = guard.as_ref() {
                        return Ok(token.clone());
                    }
                }
            }
        }
        let token = self.client.authenticate(&self.api_key).await?;
        let expires_at = parse_jwt_expiry(&token)
            .map(|expiry| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let ttl = (expiry - now - 12 * 3600).max(0) as u64;
                Instant::now() + Duration::from_secs(ttl)
            })
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        *self.token.lock().unwrap() = Some((token.clone(), expires_at));
        Ok(token)
    }
}

/// 解析 JWT payload 的 `exp`（不验证签名，仅取过期时间）。
fn parse_jwt_expiry(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    json.get("exp")?.as_i64()
}

// ---------------------------------------------------------------------------
// 认证客户端 —— 对应 `KavitaAuthClient.kt`
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct KavitaAuthClient {
    http: reqwest::Client,
    base_uri: String,
}

impl KavitaAuthClient {
    pub fn new(http: reqwest::Client, base_uri: &str) -> Self {
        Self {
            http,
            base_uri: base_uri.trim_end_matches('/').to_string(),
        }
    }

    pub async fn authenticate(&self, api_key: &str) -> Result<String, MediaServerError> {
        // Kotlin: POST api/plugin/authenticate?apiKey=<key>&pluginName=Komf（查询参数）
        let response = self
            .http
            .post(format!("{}/api/plugin/authenticate", self.base_uri))
            .query(&[("apiKey", api_key), ("pluginName", "Komf")])
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        let parsed: KavitaAuthResponse = response.json().await?;
        Ok(parsed.token)
    }
}

// ---------------------------------------------------------------------------
// KavitaClient —— 对应 `KavitaClient.kt`（全方法）
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct KavitaClient {
    http: reqwest::Client,
    base_uri: String,
    api_key: String,
    token_provider: std::sync::Arc<KavitaTokenProvider>,
    updates_rate_limiter: std::sync::Arc<RateLimiter>,
}

impl KavitaClient {
    pub fn new(http: reqwest::Client, base_uri: &str, api_key: &str) -> Result<Self, MediaServerError> {
        let base_uri = base_uri.trim_end_matches('/').to_string();
        let auth_client = KavitaAuthClient::new(http.clone(), &base_uri);
        let token_provider = std::sync::Arc::new(KavitaTokenProvider::new(auth_client, api_key.to_string()));
        Ok(Self {
            http,
            base_uri,
            api_key: api_key.to_string(),
            token_provider,
            updates_rate_limiter: std::sync::Arc::new(RateLimiter::new(120, Duration::from_secs(60))),
        })
    }

    async fn authed_request(&self, method: reqwest::Method, path: &str) -> Result<reqwest::RequestBuilder, MediaServerError> {
        let token = self.token_provider.get_token().await?;
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        Ok(self
            .http
            .request(method, format!("{}/{}", self.base_uri, path.trim_start_matches('/')))
            .headers(headers))
    }

    /// 当前有效 JWT（供 SignalR 事件连接的 `access_token` 认证）。
    pub async fn access_token(&self) -> Result<String, MediaServerError> {
        self.token_provider.get_token().await
    }

    /// 底层 HTTP 客户端（供事件连接复用）。
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn base_uri(&self) -> &str {
        &self.base_uri
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

    /// 写操作（POST）：Kavita 成功响应体可能为空或非 JSON 文本（实测
    /// `api/series/metadata` 返回 "更新成功"、`api/series/update` 返回空 body），
    /// 只检查 HTTP 状态，不解析 body（对齐 Kotlin 只查 status 的行为）
    async fn send_write(&self, request: reqwest::RequestBuilder) -> Result<(), MediaServerError> {
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(())
    }

    // -- 读取 ----------------------------------------------------------------

    pub async fn get_series(&self, series_id: i32) -> Result<KavitaSeries, MediaServerError> {
        self.send_json(
            self.authed_request(reqwest::Method::GET, &format!("api/series/{series_id}"))
                .await?,
        )
        .await
    }

    pub async fn get_series_page(
        &self,
        library_id: i32,
        page: i32,
    ) -> Result<(Vec<KavitaSeries>, Option<KavitaPagination>), MediaServerError> {
        let response = self
            .authed_request(reqwest::Method::POST, "api/series/v2")
            .await?
            .query(&[("pageNumber", page.to_string()), ("pageSize", "500".to_string())])
            .json(&serde_json::json!({
                "statements": [{
                    "field": 19,
                    "value": library_id.to_string(),
                    "comparison": 0
                }]
            }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        let pagination_header = response
            .headers()
            .get("Pagination")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string());
        let content: Vec<KavitaSeries> = response.json().await?;
        let pagination = pagination_header.as_deref().and_then(parse_pagination_header);
        Ok((content, pagination))
    }

    pub async fn get_series_metadata(&self, series_id: i32) -> Result<KavitaSeriesMetadata, MediaServerError> {
        self.send_json(
            self.authed_request(reqwest::Method::GET, "api/series/metadata")
                .await?
                .query(&[("seriesId", series_id.to_string())]),
        )
        .await
    }

    pub async fn get_series_details(&self, series_id: i32) -> Result<KavitaSeriesDetails, MediaServerError> {
        self.send_json(
            self.authed_request(reqwest::Method::GET, "api/series/series-detail")
                .await?
                .query(&[("seriesId", series_id.to_string())]),
        )
        .await
    }

    pub async fn get_volumes(&self, series_id: i32) -> Result<Vec<KavitaVolume>, MediaServerError> {
        self.send_json(
            self.authed_request(reqwest::Method::GET, "api/series/volumes")
                .await?
                .query(&[("seriesId", series_id.to_string())]),
        )
        .await
    }

    pub async fn get_volume(&self, volume_id: i32) -> Result<KavitaVolume, MediaServerError> {
        let response = self
            .authed_request(reqwest::Method::GET, "api/series/volume")
            .await?
            .query(&[("volumeId", volume_id.to_string())])
            .send()
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT || status == reqwest::StatusCode::NOT_FOUND {
            return Err(MediaServerError::NotFound(volume_id.to_string()));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(response.json().await?)
    }

    pub async fn get_chapter(&self, chapter_id: i32) -> Result<KavitaChapter, MediaServerError> {
        let response = self
            .authed_request(reqwest::Method::GET, "api/series/chapter")
            .await?
            .query(&[("chapterId", chapter_id.to_string())])
            .send()
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT || status == reqwest::StatusCode::NOT_FOUND {
            return Err(MediaServerError::NotFound(chapter_id.to_string()));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        Ok(response.json().await?)
    }

    pub async fn get_series_cover(&self, series_id: i32) -> Result<Image, MediaServerError> {
        let response = self
            .http
            .get(format!("{}/api/image/series-cover", self.base_uri))
            .query(&[("seriesId", series_id.to_string()), ("apiKey", self.api_key.clone())])
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bytes = response.bytes().await?;
        Ok(Image::new(bytes.to_vec(), mime))
    }

    pub async fn get_chapter_cover(&self, chapter_id: i32) -> Result<Image, MediaServerError> {
        let response = self
            .http
            .get(format!("{}/api/image/chapter-cover", self.base_uri))
            .query(&[("chapterId", chapter_id.to_string()), ("apiKey", self.api_key.clone())])
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bytes = response.bytes().await?;
        Ok(Image::new(bytes.to_vec(), mime))
    }

    pub async fn get_libraries(&self) -> Result<Vec<KavitaLibrary>, MediaServerError> {
        self.send_json(
            self.authed_request(reqwest::Method::GET, "api/library/libraries")
                .await?,
        )
        .await
    }

    // -- 写入（受限流保护） ----------------------------------------------------

    pub async fn update_series(&self, series_update: &KavitaSeriesUpdateRequest) -> Result<(), MediaServerError> {
        self.updates_rate_limiter.acquire().await;
        self.send_write(
            self.authed_request(reqwest::Method::POST, "api/series/update")
                .await?
                .json(series_update),
        )
        .await?;
        Ok(())
    }

    pub async fn update_series_metadata(
        &self,
        metadata: &KavitaSeriesMetadataUpdateRequest,
    ) -> Result<(), MediaServerError> {
        self.updates_rate_limiter.acquire().await;
        self.send_write(
            self.authed_request(reqwest::Method::POST, "api/series/metadata")
                .await?
                .json(metadata),
        )
        .await?;
        Ok(())
    }

    pub async fn update_chapter_metadata(
        &self,
        metadata: &KavitaChapterMetadataUpdateRequest,
    ) -> Result<(), MediaServerError> {
        self.updates_rate_limiter.acquire().await;
        self.send_write(
            self.authed_request(reqwest::Method::POST, "api/chapter/update")
                .await?
                .json(metadata),
        )
        .await?;
        Ok(())
    }

    pub async fn upload_series_cover(
        &self,
        series_id: i32,
        cover: &Image,
        lock_cover: bool,
    ) -> Result<(), MediaServerError> {
        self.updates_rate_limiter.acquire().await;
        let base64_image = base64::engine::general_purpose::STANDARD.encode(&cover.bytes);
        self.send_write(
            self.authed_request(reqwest::Method::POST, "api/upload/series")
                .await?
                .json(&KavitaCoverUploadRequest {
                    id: series_id,
                    url: base64_image,
                    lock_cover,
                }),
        )
        .await?;
        Ok(())
    }

    pub async fn upload_volume_cover(
        &self,
        volume_id: i32,
        cover: &Image,
        lock_cover: bool,
    ) -> Result<(), MediaServerError> {
        self.updates_rate_limiter.acquire().await;
        let base64_image = base64::engine::general_purpose::STANDARD.encode(&cover.bytes);
        self.send_write(
            self.authed_request(reqwest::Method::POST, "api/upload/volume")
                .await?
                .json(&KavitaCoverUploadRequest {
                    id: volume_id,
                    url: base64_image,
                    lock_cover,
                }),
        )
        .await?;
        Ok(())
    }

    pub async fn reset_chapter_lock(&self, chapter_id: i32) -> Result<(), MediaServerError> {
        self.updates_rate_limiter.acquire().await;
        self.send_write(
            self.authed_request(reqwest::Method::POST, "api/upload/chapter")
                .await?
                .json(&KavitaCoverUploadRequest {
                    id: chapter_id,
                    url: String::new(),
                    lock_cover: false,
                }),
        )
        .await?;
        Ok(())
    }

    pub async fn scan_series(&self, library_id: i32, series_id: i32) -> Result<(), MediaServerError> {
        self.send_write(
            self.authed_request(reqwest::Method::POST, "api/series/scan")
                .await?
                .json(&serde_json::json!({
                    "libraryId": library_id,
                    "seriesId": series_id
                })),
        )
        .await?;
        Ok(())
    }

    pub async fn scan_library(&self, library_id: i32) -> Result<(), MediaServerError> {
        self.send_write(
            self.authed_request(reqwest::Method::POST, "api/library/scan")
                .await?
                .query(&[("libraryId", library_id.to_string())]),
        )
        .await?;
        Ok(())
    }
}

/// 解析 Kavita 的 `Pagination` 响应头：优先 JSON，其次 query-string 风格。
fn parse_pagination_header(value: &str) -> Option<KavitaPagination> {
    if let Ok(parsed) = serde_json::from_str::<KavitaPagination>(value) {
        return Some(parsed);
    }
    let mut current_page = None;
    let mut items_per_page = None;
    let mut total_items = None;
    let mut total_pages = None;
    for pair in value.split('&') {
        let mut parts = pair.splitn(2, '=');
        let (key, val) = (parts.next()?, parts.next()?);
        match key.trim() {
            "currentPage" | "current_page" => current_page = val.trim().parse().ok(),
            "itemsPerPage" | "items_per_page" => items_per_page = val.trim().parse().ok(),
            "totalItems" | "total_items" => total_items = val.trim().parse().ok(),
            "totalPages" | "total_pages" => total_pages = val.trim().parse().ok(),
            _ => {}
        }
    }
    Some(KavitaPagination {
        current_page: current_page?,
        items_per_page: items_per_page?,
        total_items: total_items?,
        total_pages: total_pages?,
    })
}

// ---------------------------------------------------------------------------
// 模型映射（与 Kotlin `KavitaMediaServerClientAdapter.kt` 对齐）
// ---------------------------------------------------------------------------

fn map_status(status: KavitaPublicationStatus) -> SeriesStatus {
    match status {
        KavitaPublicationStatus::Ongoing => SeriesStatus::Ongoing,
        KavitaPublicationStatus::Hiatus => SeriesStatus::Hiatus,
        KavitaPublicationStatus::Completed => SeriesStatus::Completed,
        KavitaPublicationStatus::Cancelled => SeriesStatus::Abandoned,
        KavitaPublicationStatus::Ended => SeriesStatus::Ended,
    }
}

fn authors_by_role(role: &str, authors: &[KavitaAuthor]) -> Vec<MediaServerAuthor> {
    authors
        .iter()
        .map(|a| MediaServerAuthor {
            name: a.name.clone(),
            role: role.to_string(),
        })
        .collect()
}

fn all_authors(metadata: &KavitaSeriesMetadata) -> Vec<MediaServerAuthor> {
    let mut authors = Vec::new();
    authors.extend(authors_by_role("WRITER", &metadata.writers));
    authors.extend(authors_by_role("COVER", &metadata.cover_artists));
    authors.extend(authors_by_role("PENCILLER", &metadata.pencillers));
    authors.extend(authors_by_role("LETTERER", &metadata.letterers));
    authors.extend(authors_by_role("INKER", &metadata.inkers));
    authors.extend(authors_by_role("COLORIST", &metadata.colorists));
    authors.extend(authors_by_role("EDITOR", &metadata.editors));
    authors.extend(authors_by_role("TRANSLATOR", &metadata.translators));
    authors
}

fn chapter_all_authors(chapter: &KavitaChapter) -> Vec<MediaServerAuthor> {
    let mut authors = Vec::new();
    authors.extend(authors_by_role("WRITER", &chapter.writers));
    authors.extend(authors_by_role("COVER", &chapter.cover_artists));
    authors.extend(authors_by_role("PENCILLER", &chapter.pencillers));
    authors.extend(authors_by_role("LETTERER", &chapter.letterers));
    authors.extend(authors_by_role("INKER", &chapter.inkers));
    authors.extend(authors_by_role("COLORIST", &chapter.colorists));
    authors.extend(authors_by_role("EDITOR", &chapter.editors));
    authors.extend(authors_by_role("TRANSLATOR", &chapter.translators));
    authors
}

fn any_series_lock(metadata: &KavitaSeriesMetadata) -> bool {
    metadata.writer_locked
        || metadata.cover_artist_locked
        || metadata.penciller_locked
        || metadata.letterer_locked
        || metadata.inker_locked
        || metadata.colorist_locked
        || metadata.editor_locked
        || metadata.translator_locked
}

fn any_chapter_lock(chapter: &KavitaChapter) -> bool {
    chapter.writer_locked
        || chapter.cover_artist_locked
        || chapter.penciller_locked
        || chapter.letterer_locked
        || chapter.inker_locked
        || chapter.colorist_locked
        || chapter.editor_locked
        || chapter.translator_locked
}

fn to_media_server_series(
    series: &KavitaSeries,
    metadata: &KavitaSeriesMetadata,
    book_count: i32,
) -> MediaServerSeries {
    let status = KavitaPublicationStatus::from_id(metadata.publication_status).map(map_status);
    let alternative_titles = series
        .localized_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(|name| vec![MediaServerAlternativeTitle {
            label: "Localized".to_string(),
            title: name.to_string(),
        }])
        .unwrap_or_default();
    MediaServerSeries {
        id: MediaServerSeriesId(series.id.to_string()),
        library_id: MediaServerLibraryId(series.library_id.to_string()),
        name: series.original_name.clone(),
        books_count: book_count,
        books_metadata_links: Vec::new(),
        metadata: MediaServerSeriesMetadata {
            status,
            title: series.name.clone(),
            title_sort: series.sort_name.clone(),
            alternative_titles,
            summary: metadata.summary.clone().unwrap_or_default(),
            reading_direction: None,
            publisher: None,
            alternative_publishers: {
                // Kotlin: publishers.map { it.name }.toSet() —— 保序去重
                let mut seen = std::collections::HashSet::new();
                metadata
                    .publishers
                    .iter()
                    .map(|p| p.name.clone())
                    .filter(|name| seen.insert(name.clone()))
                    .collect()
            },
            age_rating: KavitaAgeRating::from_id(metadata.age_rating)
                .and_then(|r| r.age_rating_value()),
            language: metadata.language.clone(),
            genres: metadata.genres.iter().map(|g| g.title.clone()).collect(),
            tags: metadata.tags.iter().map(|t| t.title.clone()).collect(),
            total_book_count: if metadata.total_count == 0 {
                None
            } else {
                Some(metadata.total_count)
            },
            authors: all_authors(metadata),
            release_year: Some(metadata.release_year),
            links: metadata
                .web_links
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|links| {
                    // Kotlin: webLinks?.split(",")?.map { WebLink(it, it) } —— 原样不 trim 不过滤
                    links
                        .split(',')
                        .map(|l| WebLink {
                            label: l.to_string(),
                            url: l.to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default(),

            status_lock: metadata.publication_status_locked,
            title_lock: false,
            title_sort_lock: series.sort_name_locked,
            alternative_titles_lock: series.localized_name_locked,
            summary_lock: metadata.summary_locked,
            reading_direction_lock: false,
            publisher_lock: metadata.publisher_locked,
            age_rating_lock: metadata.age_rating_locked,
            language_lock: metadata.language_locked,
            genres_lock: metadata.genres_locked,
            tags_lock: metadata.tags_locked,
            total_book_count_lock: false,
            authors_lock: any_series_lock(metadata),
            release_year_lock: metadata.release_year_locked,
            links_lock: false,
        },
        url: series.folder_path.clone(),
        deleted: false,
        // Kavita 无 oneshot 概念：mylar 导出按非 oneshot 命名（series.json）。
        oneshot: false,
    }
}

fn to_media_server_book(chapter: &KavitaChapter, volume: &KavitaVolume) -> MediaServerBook {
    let file_path = chapter.files.first().map(|f| f.file_path.clone()).unwrap_or_default();
    let file_name = std::path::Path::new(&file_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let number = chapter
        .number
        .as_deref()
        .and_then(|n| n.parse::<i32>().ok())
        .unwrap_or(0);
    MediaServerBook {
        id: MediaServerBookId(chapter.id.to_string()),
        series_id: MediaServerSeriesId(volume.series_id.to_string()),
        library_id: None,
        series_title: chapter.title.clone(),
        name: file_name.clone(),
        url: file_path,
        file_name,
        number,
        oneshot: false,
        metadata: MediaServerBookMetadata {
            title: chapter.title.clone(),
            summary: chapter.summary.clone().unwrap_or_default(),
            number: chapter.number.clone().unwrap_or_else(|| "0".to_string()),
            number_sort: chapter.number.clone(),
            release_date: chapter
                .release_date
                .split_once('T')
                .map(|(date, _)| date.to_string()),
            authors: chapter_all_authors(chapter),
            tags: chapter.tags.iter().map(|t| t.title.clone()).collect(),
            isbn: (!chapter.isbn.trim().is_empty()).then(|| chapter.isbn.clone()),
            links: Vec::new(),
            title_lock: false,
            summary_lock: chapter.summary_locked,
            number_lock: false,
            number_sort_lock: false,
            release_date_lock: false,
            authors_lock: any_chapter_lock(chapter),
            tags_lock: chapter.tags_locked,
            isbn_lock: false,
            links_lock: false,
        },
        deleted: false,
    }
}

fn to_media_server_library(library: &KavitaLibrary) -> MediaServerLibrary {
    MediaServerLibrary {
        id: MediaServerLibraryId(library.id.to_string()),
        name: library.name.clone(),
        roots: library.folders.clone(),
    }
}

/// 对应 Kotlin `normalizeRegex` + `deduplicate`：按归一化结果去重并保留原值。
fn deduplicate(values: &[String]) -> Vec<String> {
    let regex = regex::Regex::new(r"[^\p{L}0-9+!]").expect("valid normalize regex");
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for value in values {
        let normalized = regex.replace_all(value.trim(), "").to_lowercase();
        if seen.insert(normalized) {
            result.push(value.clone());
        }
    }
    result
}

/// 对应 Kotlin `toKavitaSeriesMetadataUpdate`。
fn to_kavita_series_metadata_update(
    metadata: &MediaServerSeriesMetadataUpdate,
    current: &KavitaSeriesMetadata,
    series_id: i32,
) -> KavitaSeriesMetadataUpdateRequest {
    let status = metadata.status.map(|s| match s {
        SeriesStatus::Ended => KavitaPublicationStatus::Ended,
        SeriesStatus::Ongoing => KavitaPublicationStatus::Ongoing,
        SeriesStatus::Abandoned => KavitaPublicationStatus::Cancelled,
        SeriesStatus::Hiatus => KavitaPublicationStatus::Hiatus,
        SeriesStatus::Completed => KavitaPublicationStatus::Completed,
    });
    // 对应字段 locked=true 时保留 current 值，不写入 provider 新值。
    let publishers = if current.publisher_locked {
        current.publishers.clone()
    } else if metadata.publisher.is_none() && metadata.alternative_publishers.is_none() {
        current.publishers.clone()
    } else {
        let mut names = metadata.alternative_publishers.clone().unwrap_or_default();
        if let Some(publisher) = &metadata.publisher {
            names.push(publisher.clone());
        }
        // Kotlin: (alternativePublishers + publisher).map{...}.toSet() —— 去重
        let mut seen = std::collections::HashSet::new();
        names
            .into_iter()
            .filter(|name| seen.insert(name.clone()))
            .map(|name| KavitaAuthor { id: 0, name })
            .collect()
    };
    let authors = metadata.authors.as_ref().map(|authors| {
        let mut grouped: std::collections::HashMap<String, Vec<String>> = Default::default();
        for author in authors {
            grouped
                .entry(author.role.to_lowercase())
                .or_default()
                .push(author.name.clone());
        }
        grouped
    });
    let authors_for = |role: &str,
                       authors: &Option<std::collections::HashMap<String, Vec<String>>>,
                       current_set: &Vec<KavitaAuthor>|
     -> Vec<KavitaAuthor> {
        match authors {
            Some(grouped) => {
                let names = grouped.get(&role.to_lowercase()).cloned().unwrap_or_default();
                if names.is_empty() {
                    current_set.clone()
                } else {
                    names
                        .into_iter()
                        .map(|name| KavitaAuthor { id: 0, name })
                        .collect()
                }
            }
            None => current_set.clone(),
        }
    };
    let age_rating = metadata
        .age_rating
        .map(KavitaAgeRating::from_rating)
        .unwrap_or_else(|| KavitaAgeRating::from_id(current.age_rating).unwrap_or(KavitaAgeRating::Unknown));
    let kavita_metadata = KavitaSeriesMetadata {
        id: current.id,
        series_id,
        summary: if current.summary_locked {
            current.summary.clone()
        } else {
            metadata.summary.clone().or_else(|| current.summary.clone())
        },
        genres: if current.genres_locked {
            current.genres.clone()
        } else {
            metadata
                .genres
                .as_ref()
                .map(|values| {
                    deduplicate(values)
                        .into_iter()
                        .map(|title| KavitaGenre { id: 0, title })
                        .collect()
                })
                .unwrap_or_else(|| current.genres.clone())
        },
        tags: if current.tags_locked {
            current.tags.clone()
        } else {
            metadata
                .tags
                .as_ref()
                .map(|values| {
                    deduplicate(values)
                        .into_iter()
                        .map(|title| KavitaTag { id: 0, title })
                        .collect()
                })
                .unwrap_or_else(|| current.tags.clone())
        },
        writers: if current.writer_locked {
            current.writers.clone()
        } else {
            authors_for("WRITER", &authors, &current.writers)
        },
        cover_artists: if current.cover_artist_locked {
            current.cover_artists.clone()
        } else {
            authors_for("COVER", &authors, &current.cover_artists)
        },
        publishers,
        characters: current.characters.clone(),
        pencillers: if current.penciller_locked {
            current.pencillers.clone()
        } else {
            authors_for("PENCILLER", &authors, &current.pencillers)
        },
        inkers: if current.inker_locked {
            current.inkers.clone()
        } else {
            authors_for("INKER", &authors, &current.inkers)
        },
        imprints: current.imprints.clone(),
        colorists: if current.colorist_locked {
            current.colorists.clone()
        } else {
            authors_for("COLORIST", &authors, &current.colorists)
        },
        letterers: if current.letterer_locked {
            current.letterers.clone()
        } else {
            authors_for("LETTERER", &authors, &current.letterers)
        },
        editors: if current.editor_locked {
            current.editors.clone()
        } else {
            authors_for("EDITOR", &authors, &current.editors)
        },
        translators: if current.translator_locked {
            current.translators.clone()
        } else {
            authors_for("TRANSLATOR", &authors, &current.translators)
        },
        teams: current.teams.clone(),
        locations: current.locations.clone(),
        age_rating: if current.age_rating_locked {
            current.age_rating
        } else {
            age_rating.id()
        },
        release_year: if current.release_year_locked {
            current.release_year
        } else {
            metadata.release_year.unwrap_or(current.release_year)
        },
        language: if current.language_locked {
            current.language.clone()
        } else {
            metadata.language.clone().or_else(|| current.language.clone())
        },
        max_count: current.max_count,
        total_count: current.total_count,
        publication_status: if current.publication_status_locked {
            current.publication_status
        } else {
            status
                .unwrap_or_else(|| KavitaPublicationStatus::from_id(current.publication_status).unwrap_or(KavitaPublicationStatus::Ongoing))
                .id()
        },
        web_links: metadata
            .links
            .as_ref()
            .map(|links| links.iter().map(|l| l.url.clone()).collect::<Vec<_>>().join(","))
            .or_else(|| current.web_links.clone()),
        language_locked: current.language_locked,
        summary_locked: current.summary_locked,
        age_rating_locked: current.age_rating_locked,
        publication_status_locked: current.publication_status_locked,
        genres_locked: current.genres_locked,
        tags_locked: current.tags_locked,
        writer_locked: current.writer_locked,
        character_locked: current.character_locked,
        colorist_locked: current.colorist_locked,
        editor_locked: current.editor_locked,
        inker_locked: current.inker_locked,
        imprint_locked: current.imprint_locked,
        letterer_locked: current.letterer_locked,
        penciller_locked: current.penciller_locked,
        publisher_locked: current.publisher_locked,
        translator_locked: current.translator_locked,
        team_locked: current.team_locked,
        location_locked: current.location_locked,
        cover_artist_locked: current.cover_artist_locked,
        release_year_locked: current.release_year_locked,
    };
    KavitaSeriesMetadataUpdateRequest {
        series_metadata: kavita_metadata,
    }
}

/// 对应 Kotlin `kavitaSeriesResetRequest`。
/// 实测（2026-09-26，Kavita 服务端不拦 locked，客户端须自己尊重）：
/// 对应字段 locked=true 时保留 current 值 + 保持 lock 状态，未锁定字段正常重置。
fn kavita_series_reset_request(
    series_id: i32,
    current: &KavitaSeriesMetadata,
) -> KavitaSeriesMetadataUpdateRequest {
    let metadata = KavitaSeriesMetadata {
        id: 0,
        series_id,
        summary: if current.summary_locked {
            current.summary.clone()
        } else {
            Some(String::new())
        },
        genres: if current.genres_locked {
            current.genres.clone()
        } else {
            Vec::new()
        },
        tags: if current.tags_locked {
            current.tags.clone()
        } else {
            Vec::new()
        },
        writers: if current.writer_locked {
            current.writers.clone()
        } else {
            Vec::new()
        },
        cover_artists: if current.cover_artist_locked {
            current.cover_artists.clone()
        } else {
            Vec::new()
        },
        publishers: if current.publisher_locked {
            current.publishers.clone()
        } else {
            Vec::new()
        },
        characters: if current.character_locked {
            current.characters.clone()
        } else {
            Vec::new()
        },
        pencillers: if current.penciller_locked {
            current.pencillers.clone()
        } else {
            Vec::new()
        },
        inkers: if current.inker_locked {
            current.inkers.clone()
        } else {
            Vec::new()
        },
        imprints: if current.imprint_locked {
            current.imprints.clone()
        } else {
            Vec::new()
        },
        colorists: if current.colorist_locked {
            current.colorists.clone()
        } else {
            Vec::new()
        },
        letterers: if current.letterer_locked {
            current.letterers.clone()
        } else {
            Vec::new()
        },
        editors: if current.editor_locked {
            current.editors.clone()
        } else {
            Vec::new()
        },
        translators: if current.translator_locked {
            current.translators.clone()
        } else {
            Vec::new()
        },
        teams: if current.team_locked {
            current.teams.clone()
        } else {
            Vec::new()
        },
        locations: if current.location_locked {
            current.locations.clone()
        } else {
            Vec::new()
        },
        age_rating: if current.age_rating_locked {
            current.age_rating
        } else {
            KavitaAgeRating::Unknown.id()
        },
        release_year: if current.release_year_locked {
            current.release_year
        } else {
            0
        },
        language: if current.language_locked {
            current.language.clone()
        } else {
            Some(String::new())
        },
        max_count: 0,
        total_count: 0,
        publication_status: if current.publication_status_locked {
            current.publication_status
        } else {
            KavitaPublicationStatus::Ongoing.id()
        },
        web_links: Some(String::new()),
        language_locked: current.language_locked,
        summary_locked: current.summary_locked,
        age_rating_locked: current.age_rating_locked,
        publication_status_locked: current.publication_status_locked,
        genres_locked: current.genres_locked,
        tags_locked: current.tags_locked,
        writer_locked: current.writer_locked,
        character_locked: current.character_locked,
        colorist_locked: current.colorist_locked,
        editor_locked: current.editor_locked,
        inker_locked: current.inker_locked,
        imprint_locked: current.imprint_locked,
        letterer_locked: current.letterer_locked,
        penciller_locked: current.penciller_locked,
        publisher_locked: current.publisher_locked,
        translator_locked: current.translator_locked,
        team_locked: current.team_locked,
        location_locked: current.location_locked,
        cover_artist_locked: current.cover_artist_locked,
        release_year_locked: current.release_year_locked,
    };
    KavitaSeriesMetadataUpdateRequest {
        series_metadata: metadata,
    }
}

/// 对应 Kotlin `toKavitaChapterMetadataUpdate`。
fn to_kavita_chapter_metadata_update(
    metadata: &MediaServerBookMetadataUpdate,
    current: &KavitaChapter,
) -> KavitaChapterMetadataUpdateRequest {
    let authors = metadata.authors.as_ref().map(|authors| {
        let mut grouped: std::collections::HashMap<String, Vec<String>> = Default::default();
        for author in authors {
            grouped
                .entry(author.role.to_lowercase())
                .or_default()
                .push(author.name.clone());
        }
        grouped
    });
    let authors_for = |role: &str,
                       authors: &Option<std::collections::HashMap<String, Vec<String>>>,
                       current_set: &Vec<KavitaAuthor>|
     -> Vec<KavitaAuthor> {
        match authors {
            Some(grouped) => {
                let names = grouped.get(&role.to_lowercase()).cloned().unwrap_or_default();
                if names.is_empty() {
                    current_set.clone()
                } else {
                    names
                        .into_iter()
                        .map(|name| KavitaAuthor { id: 0, name })
                        .collect()
                }
            }
            None => current_set.clone(),
        }
    };
    let release_date = metadata
        .release_date
        .as_ref()
        .map(|date| format!("{date}T00:00:00"))
        .unwrap_or_else(|| current.release_date.clone());
    KavitaChapterMetadataUpdateRequest {
        id: current.id,
        summary: metadata.summary.clone().or_else(|| current.summary.clone()),
        genres: current.genres.clone(),
        tags: metadata
            .tags
            .as_ref()
            .map(|values| {
                deduplicate(values)
                    .into_iter()
                    .map(|title| KavitaTag { id: 0, title })
                    .collect()
            })
            .unwrap_or_else(|| current.tags.clone()),
        age_rating: current.age_rating,
        language: current.language.clone(),
        weblinks: metadata
            .links
            .as_ref()
            .map(|links| links.iter().map(|l| l.url.clone()).collect::<Vec<_>>().join(","))
            .unwrap_or_else(|| current.web_links.clone()),
        isbn: metadata.isbn.clone().unwrap_or_else(|| current.isbn.clone()),
        release_date,
        title_name: metadata.title.clone().unwrap_or_else(|| current.title_name.clone()),
        sort_order: metadata.number_sort.unwrap_or(current.sort_order),
        writers: authors_for("WRITER", &authors, &current.writers),
        cover_artists: authors_for("COVER", &authors, &current.cover_artists),
        publishers: current.publishers.clone(),
        characters: current.characters.clone(),
        pencillers: authors_for("PENCILLER", &authors, &current.pencillers),
        inkers: authors_for("INKER", &authors, &current.inkers),
        imprints: current.imprints.clone(),
        colorists: authors_for("COLORIST", &authors, &current.colorists),
        letterers: authors_for("LETTERER", &authors, &current.letterers),
        editors: authors_for("EDITOR", &authors, &current.editors),
        translators: authors_for("TRANSLATOR", &authors, &current.translators),
        teams: current.teams.clone(),
        locations: current.locations.clone(),
        age_rating_locked: current.age_rating_locked,
        title_name_locked: false,
        genres_locked: current.genres_locked,
        tags_locked: current.tags_locked,
        writer_locked: current.writer_locked,
        character_locked: current.character_locked,
        colorist_locked: current.colorist_locked,
        editor_locked: current.editor_locked,
        inker_locked: current.inker_locked,
        imprint_locked: current.imprint_locked,
        letterer_locked: current.letterer_locked,
        penciller_locked: current.penciller_locked,
        publisher_locked: current.publisher_locked,
        translator_locked: current.translator_locked,
        team_locked: current.team_locked,
        location_locked: current.location_locked,
        cover_artist_locked: current.cover_artist_locked,
        language_locked: current.language_locked,
        summary_locked: current.summary_locked,
        isbn_locked: false,
        release_date_locked: false,
        sort_order_locked: false,
    }
}

// ---------------------------------------------------------------------------
// MediaServerClient 实现 —— 对应 `KavitaMediaServerClientAdapter.kt`
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct KavitaMediaServerClientAdapter {
    client: KavitaClient,
}

impl KavitaMediaServerClientAdapter {
    pub fn new(client: KavitaClient) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl MediaServerClient for KavitaMediaServerClientAdapter {
    async fn get_series(&self, series_id: &MediaServerSeriesId) -> Result<MediaServerSeries, MediaServerError> {
        let id: i32 = series_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita series id: {}", series_id.0)))?;
        let series = self.client.get_series(id).await?;
        let metadata = self.client.get_series_metadata(id).await?;
        let details = self.client.get_series_details(id).await?;
        Ok(to_media_server_series(&series, &metadata, details.total_count))
    }

    async fn get_series_page(
        &self,
        library_id: &MediaServerLibraryId,
        page_number: i32,
    ) -> Result<Page<MediaServerSeries>, MediaServerError> {
        let library_id_i32: i32 = library_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita library id: {}", library_id.0)))?;
        let (content, pagination) = self.client.get_series_page(library_id_i32, page_number).await?;
        let mut series_list = Vec::with_capacity(content.len());
        for series in content {
            let metadata = match self.client.get_series_metadata(series.id).await {
                Ok(metadata) => metadata,
                Err(_) => {
                    tracing::warn!("kavita: failed to load metadata for series {}, skipping", series.id);
                    continue;
                }
            };
            let details = self.client.get_series_details(series.id).await?;
            series_list.push(to_media_server_series(&series, &metadata, details.total_count));
        }
        let total_items = pagination.as_ref().map(|p| p.total_items as i64).unwrap_or(series_list.len() as i64);
        let total_pages = pagination.as_ref().map(|p| p.total_pages).unwrap_or(1);
        Ok(Page {
            content: series_list,
            page_number: pagination.as_ref().map(|p| p.current_page).unwrap_or(page_number),
            total_elements: total_items,
            total_pages,
        })
    }

    async fn get_series_thumbnail(&self, series_id: &MediaServerSeriesId) -> Result<Option<Image>, MediaServerError> {
        let id: i32 = series_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita series id: {}", series_id.0)))?;
        match self.client.get_series_cover(id).await {
            Ok(image) => Ok(Some(image)),
            Err(_) => Ok(None),
        }
    }

    async fn get_series_thumbnails(
        &self,
        _series_id: &MediaServerSeriesId,
    ) -> Result<Vec<MediaServerSeriesThumbnail>, MediaServerError> {
        // Kavita 每个系列/卷/章节只有一张封面，无法枚举多个缩略图
        Ok(Vec::new())
    }

    async fn get_book(&self, book_id: &MediaServerBookId) -> Result<MediaServerBook, MediaServerError> {
        let id: i32 = book_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita chapter id: {}", book_id.0)))?;
        let chapter = self.client.get_chapter(id).await?;
        let volume = self.client.get_volume(chapter.volume_id).await?;
        Ok(to_media_server_book(&chapter, &volume))
    }

    async fn get_books(&self, series_id: &MediaServerSeriesId) -> Result<Vec<MediaServerBook>, MediaServerError> {
        let id: i32 = series_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita series id: {}", series_id.0)))?;
        let volumes = self.client.get_volumes(id).await?;
        let mut books = Vec::new();
        for volume in volumes.iter() {
            // 对齐 Kotlin：仅消费 volume 端点嵌套的 chapters；空卷直接跳过
            for chapter in volume.chapters.iter() {
                books.push(to_media_server_book(chapter, volume));
            }
        }
        Ok(books)
    }

    async fn get_book_thumbnails(
        &self,
        _book_id: &MediaServerBookId,
    ) -> Result<Vec<MediaServerBookThumbnail>, MediaServerError> {
        Ok(Vec::new())
    }

    async fn get_book_thumbnail(&self, book_id: &MediaServerBookId) -> Result<Option<Image>, MediaServerError> {
        let id: i32 = book_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita chapter id: {}", book_id.0)))?;
        match self.client.get_chapter_cover(id).await {
            Ok(image) => Ok(Some(image)),
            Err(_) => Ok(None),
        }
    }

    async fn get_library(&self, library_id: &MediaServerLibraryId) -> Result<MediaServerLibrary, MediaServerError> {
        let libraries = self.get_libraries().await?;
        libraries
            .into_iter()
            .find(|l| l.id == *library_id)
            .ok_or_else(|| MediaServerError::NotFound(library_id.0.clone()))
    }

    async fn get_libraries(&self) -> Result<Vec<MediaServerLibrary>, MediaServerError> {
        let libraries = self.client.get_libraries().await?;
        Ok(libraries
            .into_iter()
            .map(|library| to_media_server_library(&library))
            .collect())
    }

    async fn update_series_metadata(
        &self,
        series_id: &MediaServerSeriesId,
        metadata: &MediaServerSeriesMetadataUpdate,
    ) -> Result<(), MediaServerError> {
        let id: i32 = series_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita series id: {}", series_id.0)))?;
        let localized_name = metadata
            .alternative_titles
            .as_ref()
            .and_then(|titles| titles.iter().find(|(_, _, language)| language.is_some()))
            .map(|(name, _, _)| name.clone());
        if metadata.title_sort.is_some() || localized_name.is_some() {
            let series = self.client.get_series(id).await?;
            self.client
                .update_series(&KavitaSeriesUpdateRequest {
                    id,
                    localized_name: localized_name
                        .map(|n| n.trim().to_string())
                        .or_else(|| series.localized_name.clone()),
                    sort_name: metadata
                        .title_sort
                        .as_ref()
                        .map(|t| t.name.trim().to_string())
                        .unwrap_or_else(|| series.sort_name.clone()),
                    sort_name_locked: series.sort_name_locked,
                    localized_name_locked: series.localized_name_locked,
                    cover_image_locked: series.cover_image_locked,
                })
                .await?;
        }
        let old_metadata = self.client.get_series_metadata(id).await?;
        let request = to_kavita_series_metadata_update(metadata, &old_metadata, id);
        self.client.update_series_metadata(&request).await
    }

    async fn delete_series_thumbnail(
        &self,
        series_id: &MediaServerSeriesId,
        _thumbnail_id: &MediaServerThumbnailId,
    ) -> Result<(), MediaServerError> {
        let id: i32 = series_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita series id: {}", series_id.0)))?;
        let series = self.client.get_series(id).await?;
        self.client
            .update_series(&KavitaSeriesUpdateRequest {
                id,
                localized_name: series.localized_name.clone(),
                sort_name: series.sort_name.clone(),
                sort_name_locked: false,
                localized_name_locked: false,
                cover_image_locked: false,
            })
            .await
    }

    async fn update_book_metadata(
        &self,
        book_id: &MediaServerBookId,
        metadata: &MediaServerBookMetadataUpdate,
    ) -> Result<(), MediaServerError> {
        let id: i32 = book_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita chapter id: {}", book_id.0)))?;
        let current_chapter = self.client.get_chapter(id).await?;
        let request = to_kavita_chapter_metadata_update(metadata, &current_chapter);
        self.client.update_chapter_metadata(&request).await
    }

    async fn delete_book_thumbnail(
        &self,
        _book_id: &MediaServerBookId,
        _thumbnail_id: &MediaServerThumbnailId,
    ) -> Result<(), MediaServerError> {
        // Kotlin 空实现
        Ok(())
    }

    async fn reset_book_metadata(
        &self,
        book: &MediaServerBook,
        _book_number: Option<i32>,
    ) -> Result<(), MediaServerError> {
        let id: i32 = book
            .id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita chapter id: {}", book.id.0)))?;
        self.client.reset_chapter_lock(id).await
    }

    async fn reset_series_metadata(
        &self,
        series: &MediaServerSeries,
    ) -> Result<(), MediaServerError> {
        let id: i32 = series
            .id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita series id: {}", series.id.0)))?;
        let series = self.client.get_series(id).await?;
        self.client
            .update_series(&KavitaSeriesUpdateRequest {
                id,
                localized_name: series.localized_name.clone(),
                sort_name: series.sort_name.clone(),
                sort_name_locked: false,
                localized_name_locked: false,
                cover_image_locked: false,
            })
            .await?;
        let current_metadata = self.client.get_series_metadata(id).await?;
        let request = kavita_series_reset_request(id, &current_metadata);
        self.client.update_series_metadata(&request).await
    }

    async fn upload_series_thumbnail(
        &self,
        series_id: &MediaServerSeriesId,
        thumbnail: &Image,
        _selected: bool,
        lock: bool,
    ) -> Result<Option<MediaServerSeriesThumbnail>, MediaServerError> {
        let id: i32 = series_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita series id: {}", series_id.0)))?;
        self.client.upload_series_cover(id, thumbnail, lock).await?;
        Ok(None)
    }

    async fn upload_book_thumbnail(
        &self,
        book_id: &MediaServerBookId,
        thumbnail: &Image,
        _selected: bool,
        lock: bool,
    ) -> Result<Option<MediaServerBookThumbnail>, MediaServerError> {
        let id: i32 = book_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita chapter id: {}", book_id.0)))?;
        let chapter = self.client.get_chapter(id).await?;
        self.client.upload_volume_cover(chapter.volume_id, thumbnail, lock).await?;
        Ok(None)
    }

    async fn refresh_metadata(
        &self,
        library_id: &MediaServerLibraryId,
        series_id: &MediaServerSeriesId,
    ) -> Result<(), MediaServerError> {
        let library_id_i32: i32 = library_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita library id: {}", library_id.0)))?;
        let series_id_i32: i32 = series_id
            .0
            .parse()
            .map_err(|_| MediaServerError::message(format!("invalid Kavita series id: {}", series_id.0)))?;
        self.client.scan_library(library_id_i32).await?;
        self.client.scan_series(library_id_i32, series_id_i32).await
    }
}


// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 写操作成功响应为非 JSON（Kavita 实测返回 "更新成功" 文本）时，
    /// send_write 只检查状态、不解析 body（对齐 Kotlin 只查 status）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_write_accepts_non_json_success_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain;charset=UTF-8\r\nContent-Length: 12\r\n\r\n\xe6\x9b\xb4\xe6\x96\xb0\xe6\x88\x90\xe5\x8a\x9f",
                )
                .await;
        });

        let client = KavitaClient::new(
            reqwest::Client::builder().build().unwrap(),
            &format!("http://{addr}"),
            "key",
        )
        .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.send_write(client.http.post(format!("http://{addr}/write"))),
        )
        .await
        .expect("send_write timed out")
        .expect("send_write failed on non-json 200 body");
        assert_eq!(result, ());
    }

    /// 写操作错误状态（500）必须返回 Status 错误。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_write_rejects_error_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nNo",
                )
                .await;
        });

        let client = KavitaClient::new(
            reqwest::Client::builder().build().unwrap(),
            &format!("http://{addr}"),
            "key",
        )
        .unwrap();
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.send_write(client.http.post(format!("http://{addr}/write"))),
        )
        .await
        .expect("send_write timed out")
        .expect_err("send_write should fail on 500");
        match err {
            MediaServerError::Status(status, _) => {
                assert_eq!(status, reqwest::StatusCode::INTERNAL_SERVER_ERROR)
            }
            other => panic!("expected Status error, got {other:?}"),
        }
    }

    #[test]
    fn age_rating_roundtrip() {
        for id in -1..=14 {
            let rating = KavitaAgeRating::from_id(id).unwrap();
            assert_eq!(rating.id(), id);
        }
        assert_eq!(KavitaAgeRating::from_id(99), None);
        assert_eq!(KavitaAgeRating::Everyone.age_rating_value(), Some(0));
        assert_eq!(KavitaAgeRating::Unknown.age_rating_value(), None);
        assert_eq!(KavitaAgeRating::AdultsOnly.age_rating_value(), Some(18));
    }

    #[test]
    fn age_rating_upgrade_mapping() {
        assert_eq!(KavitaAgeRating::from_rating(0), KavitaAgeRating::RatingPending);
        assert_eq!(KavitaAgeRating::from_rating(10), KavitaAgeRating::Everyone10Plus);
        assert_eq!(KavitaAgeRating::from_rating(17), KavitaAgeRating::Mature17Plus);
        assert_eq!(KavitaAgeRating::from_rating(18), KavitaAgeRating::R18Plus);
        assert_eq!(KavitaAgeRating::from_rating(99), KavitaAgeRating::AdultsOnly);
    }

    #[test]
    fn publication_status_roundtrip() {
        assert_eq!(KavitaPublicationStatus::from_id(0), Some(KavitaPublicationStatus::Ongoing));
        assert_eq!(KavitaPublicationStatus::from_id(4), Some(KavitaPublicationStatus::Ended));
        assert_eq!(KavitaPublicationStatus::from_id(5), None);
        assert_eq!(KavitaPublicationStatus::Completed.id(), 2);
    }

    #[test]
    fn pagination_header_parsing() {
        let parsed = parse_pagination_header(
            r#"{"currentPage":3,"itemsPerPage":500,"totalItems":1234,"totalPages":3}"#,
        )
        .unwrap();
        assert_eq!(parsed.current_page, 3);
        assert_eq!(parsed.total_items, 1234);
        let parsed = parse_pagination_header(
            "currentPage=2&itemsPerPage=500&totalItems=999&totalPages=2",
        )
        .unwrap();
        assert_eq!(parsed.current_page, 2);
        assert_eq!(parsed.total_items, 999);
        assert_eq!(parse_pagination_header("garbage"), None);
    }

    #[test]
    fn deduplicate_normalizes() {
        // Kotlin `normalizeRegex = [^\p{L}0-9+!]`：`+`/`!` 属于保留字符
        let values = vec![
            "One Piece".to_string(),
            "one piece".to_string(),
            "On+E!".to_string(),
            "Naruto".to_string(),
        ];
        let result = deduplicate(&values);
        assert_eq!(result, vec!["One Piece".to_string(), "On+E!".to_string(), "Naruto".to_string()]);
        // 大小写与连字符/空格归一化后去重
        let values = vec![
            "One-Piece!".to_string(),
            "one-piece!".to_string(),
            "Naruto".to_string(),
        ];
        let result = deduplicate(&values);
        assert_eq!(result, vec!["One-Piece!".to_string(), "Naruto".to_string()]);
    }

    #[test]
    fn chapter_number_parsing() {
        let chapter = KavitaChapter {
            id: 1,
            range: None,
            number: Some("12".to_string()),
            pages: 0,
            is_special: false,
            title: "Chapter".to_string(),
            files: vec![KavitaChapterFile {
                id: 1,
                file_path: "manga/series/chapter 12.cbz".to_string(),
                pages: 0,
                format: 0,
                created: String::new(),
            }],
            pages_read: 0,
            cover_image_locked: false,
            volume_id: 2,
            created_utc: String::new(),
            count: 0,
            total_count: 0,
            summary: None,
            genres: Vec::new(),
            tags: Vec::new(),
            age_rating: 0,
            language: None,
            web_links: String::new(),
            isbn: String::new(),
            release_date: "2024-05-01T00:00:00".to_string(),
            title_name: "Chapter 12.5".to_string(),
            sort_order: 12.0,
            writers: Vec::new(),
            cover_artists: Vec::new(),
            publishers: Vec::new(),
            characters: Vec::new(),
            pencillers: Vec::new(),
            inkers: Vec::new(),
            imprints: Vec::new(),
            colorists: Vec::new(),
            letterers: Vec::new(),
            editors: Vec::new(),
            translators: Vec::new(),
            teams: Vec::new(),
            locations: Vec::new(),
            age_rating_locked: false,
            genres_locked: false,
            tags_locked: false,
            writer_locked: false,
            character_locked: false,
            colorist_locked: false,
            editor_locked: false,
            inker_locked: false,
            imprint_locked: false,
            letterer_locked: false,
            penciller_locked: false,
            publisher_locked: false,
            translator_locked: false,
            team_locked: false,
            location_locked: false,
            cover_artist_locked: false,
            language_locked: false,
            summary_locked: false,
        };
        let volume = KavitaVolume {
            id: 2,
            min_number: 12.0,
            max_number: 13.0,
            name: "Vol. 12".to_string(),
            pages: 0,
            series_id: 3,
            chapters: Vec::new(),
        };
        let book = to_media_server_book(&chapter, &volume);
        assert_eq!(book.number, 12);
        assert_eq!(book.name, "chapter 12");
        assert_eq!(book.metadata.release_date.as_deref(), Some("2024-05-01"));
        assert_eq!(book.metadata.number, "12");
        assert_eq!(book.metadata.number_sort.as_deref(), Some("12"));
    }

    #[test]
    fn series_metadata_update_request_serializes_kotlin_shape() {
        // Kotlin: KavitaSeriesMetadataUpdateRequest(seriesMetadata) → { "seriesMetadata": {...} }
        let request = kavita_series_reset_request(7, &KavitaSeriesMetadata::default());
        let json = serde_json::to_value(&request).unwrap();
        let metadata = json.get("seriesMetadata").expect("wrapped under seriesMetadata");
        assert_eq!(metadata["seriesId"], 7);
        assert_eq!(metadata["publicationStatus"], 0);
        assert_eq!(metadata["ageRating"], 0);
        assert_eq!(metadata["releaseYear"], 0);
        assert!(metadata["writers"].as_array().unwrap().is_empty());
    }

    /// locked 语义（实测 2026-09-26）：Kavita 服务端不拦 locked，客户端须保留
    /// locked=true 字段的 current 值（更新与重置一致）。
    #[test]
    fn series_metadata_update_respects_locked_fields() {
        let current = KavitaSeriesMetadata {
            id: 7,
            series_id: 7,
            summary: Some("locked summary".to_string()),
            genres: vec![KavitaGenre { id: 1, title: "Locked Genre".to_string() }],
            tags: vec![KavitaTag { id: 1, title: "Locked Tag".to_string() }],
            age_rating: KavitaAgeRating::Teen.id(),
            publication_status: KavitaPublicationStatus::Completed.id(),
            release_year: 2019,
            language: Some("ja".to_string()),
            writers: vec![KavitaAuthor { id: 1, name: "Locked Writer".to_string() }],
            genres_locked: true,
            summary_locked: true,
            tags_locked: true,
            age_rating_locked: true,
            publication_status_locked: true,
            release_year_locked: true,
            language_locked: true,
            writer_locked: true,
            ..Default::default()
        };
        // 重置：locked 字段保留 current 值
        let request = kavita_series_reset_request(7, &current);
        let json = serde_json::to_value(&request).unwrap();
        let metadata = &json["seriesMetadata"];
        assert_eq!(metadata["summary"], "locked summary");
        assert_eq!(metadata["genres"][0]["title"], "Locked Genre");
        assert_eq!(metadata["tags"][0]["title"], "Locked Tag");
        assert_eq!(metadata["ageRating"], KavitaAgeRating::Teen.id());
        assert_eq!(metadata["publicationStatus"], KavitaPublicationStatus::Completed.id());
        assert_eq!(metadata["releaseYear"], 2019);
        assert_eq!(metadata["language"], "ja");
        assert_eq!(metadata["writers"][0]["name"], "Locked Writer");
        assert!(metadata["genresLocked"].as_bool().unwrap());
        // 未锁定字段仍重置
        let unlocked = KavitaSeriesMetadata {
            id: 7,
            series_id: 7,
            genres_locked: false,
            ..Default::default()
        };
        let request = kavita_series_reset_request(7, &unlocked);
        let metadata = &serde_json::to_value(&request).unwrap()["seriesMetadata"];
        assert!(metadata["genres"].as_array().unwrap().is_empty());
    }

    #[test]
    fn series_update_request_serializes_kotlin_shape() {
        let request = KavitaSeriesUpdateRequest {
            id: 3,
            localized_name: Some("ローカル名".to_string()),
            sort_name: "Sort Name".to_string(),
            cover_image_locked: true,
            sort_name_locked: false,
            localized_name_locked: true,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["id"], 3);
        assert_eq!(json["localizedName"], "ローカル名");
        assert_eq!(json["sortName"], "Sort Name");
        assert_eq!(json["coverImageLocked"], true);
        assert_eq!(json["localizedNameLocked"], true);
    }

    #[test]
    fn chapter_metadata_update_request_serializes_kotlin_shape() {
        let request = KavitaChapterMetadataUpdateRequest {
            id: 42,
            summary: Some("summary".to_string()),
            genres: vec![KavitaGenre { id: 0, title: "Action".to_string() }],
            tags: vec![],
            age_rating: 13,
            language: None,
            weblinks: "https://example.com".to_string(),
            isbn: "978-1".to_string(),
            release_date: "2024-05-01T00:00:00".to_string(),
            title_name: "Chapter 42".to_string(),
            sort_order: 42.0,
            writers: vec![KavitaAuthor { id: 0, name: "Author A".to_string() }],
            cover_artists: vec![],
            publishers: vec![],
            characters: vec![],
            pencillers: vec![],
            inkers: vec![],
            imprints: vec![],
            colorists: vec![],
            letterers: vec![],
            editors: vec![],
            translators: vec![],
            teams: vec![],
            locations: vec![],
            age_rating_locked: false,
            title_name_locked: false,
            genres_locked: false,
            tags_locked: false,
            writer_locked: false,
            character_locked: false,
            colorist_locked: false,
            editor_locked: false,
            inker_locked: false,
            imprint_locked: false,
            letterer_locked: false,
            penciller_locked: false,
            publisher_locked: false,
            translator_locked: false,
            team_locked: false,
            location_locked: false,
            cover_artist_locked: false,
            language_locked: false,
            summary_locked: false,
            isbn_locked: false,
            release_date_locked: false,
            sort_order_locked: false,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["id"], 42);
        assert_eq!(json["titleName"], "Chapter 42");
        assert_eq!(json["sortOrder"], 42.0);
        assert_eq!(json["weblinks"], "https://example.com");
        assert_eq!(json["ageRating"], 13);
        assert_eq!(json["writers"][0]["name"], "Author A");
        assert!(json.get("seriesId").is_none(), "chapter update must not carry seriesId");
    }

    #[test]
    fn cover_upload_request_serializes_kotlin_shape() {
        let request = KavitaCoverUploadRequest {
            id: 9,
            url: "aGVsbG8=".to_string(),
            lock_cover: true,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["id"], 9);
        assert_eq!(json["url"], "aGVsbG8=");
        assert_eq!(json["lockCover"], true);
    }
}
