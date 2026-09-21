//! Stump GraphQL/REST 客户端 —— Rust 版新增的 Stump 媒体服务器支持（骨架）。
//!
//! 设计要点（对照 `komga.rs` / `kavita.rs` 的既有约定）：
//! - 数据读写走 GraphQL（`POST {base}/api/graphql`，标准 async-graphql JSON 信封），
//!   查询用字符串常量 + `serde_json` 解析，不引入 graphql 代码生成依赖；
//! - 图片（系列/书籍缩略图）走 REST v2（`/api/v2/series/{id}/thumbnail`、`/api/v2/media/{id}/thumbnail`）；
//! - 认证优先 API Key（`Authorization: Bearer <api-key>`，Stump 中间件按前缀识别 API Key），
//!   未配置时用账号密码登录换取 JWT（access token），401 自动失效重试一次；
//! - 事件监听（WS 订阅 `readEvents`）与配置/路由接线见文件尾部 TODO，不在本文件实现。
//!
//! 已知差距（与 Stump 0.1.x schema 对齐，见 README 可行性分析）：
//! - 系列级字段 titleSort / readingDirection / language / alternativeTitles 等
//!   Stump series_metadata 无对应列，写入时静默丢弃；
//! - 收藏夹（Reading List）为书级且 update 未实现，`get_collections` 等沿用 trait 默认值；
//! - 无 `resetMediaMetadata` mutation，书级重置用 `updateMediaMetadata` 置空变通；
//! - Stump 单一缩略图模型：`delete_*_thumbnail` 为 no-op（上传即整体替换）。

use crate::client::{MediaServerClient, MediaServerError};
use crate::model::*;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use komf_core::model::{Image, SeriesStatus, WebLink};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// GraphQL 文档常量（宏展开为字符串字面量，供 concat! 编译期拼接）
// ---------------------------------------------------------------------------

/// `Series` 对象字段片段。
macro_rules! series_fields {
    () => {
        r#"fragment SeriesFields on Series {
  id
  name
  description
  path
  status
  isOneshot
  thumbnailPath
  libraryId
  mediaCount
  deletedAt
  metadata {
    seriesId
    ageRating
    descriptionFormatted
    genres
    links
    publisher
    status
    summary
    title
    totalIssues
    volume
    writers
    year
  }
}"#
    };
}

/// `Media` 对象字段片段（附带所属系列名，映射 `series_title` 用）。
macro_rules! media_fields {
    () => {
        r#"fragment MediaFields on Media {
  id
  name
  size
  extension
  pages
  isOneshot
  path
  status
  thumbnailPath
  seriesId
  libraryId
  deletedAt
  series {
    id
    name
    libraryId
  }
  metadata {
    mediaId
    ageRating
    day
    identifierIsbn
    language
    month
    number
    pageCount
    publisher
    summary
    title
    titleSort
    volume
    year
    writers
    genres
    links
  }
}"#
    };
}

const SERIES_BY_ID_DOC: &str = concat!(
    series_fields!(),
    "\n",
    "query SeriesById($id: ID!) { seriesById(id: $id) { ...SeriesFields } }"
);

/// 按图书馆分页拉取系列（offset 分页；`page_number` 为 0 基，映射到 zeroBased=false 的 page+1）。
const SERIES_PAGE_DOC: &str = concat!(
    series_fields!(),
    "\n",
    r#"query SeriesPage($filter: SeriesFilterInput!, $pagination: Pagination!) {
  series(filter: $filter, pagination: $pagination) {
    nodes { ...SeriesFields }
    pageInfo {
      ... on OffsetPaginationInfo { totalPages totalItems currentPage pageSize }
    }
  }
}"#
);

const MEDIA_BY_ID_DOC: &str = concat!(
    media_fields!(),
    "\n",
    "query MediaById($id: ID!) { mediaById(id: $id) { ...MediaFields } }"
);

/// 分页拉取系列下全部书籍（顶层 `media(filter: {seriesId})`；`Series.media` 无 skip 参数，
/// 故经顶层查询 + offset 分页，每页 1000 本，循环翻页直到取完）。
const MEDIA_PAGE_DOC: &str = concat!(
    media_fields!(),
    "\n",
    r#"query MediaPage($filter: MediaFilterInput!, $pagination: Pagination!) {
  media(filter: $filter, pagination: $pagination) {
    nodes { ...MediaFields }
    pageInfo {
      ... on OffsetPaginationInfo { totalPages totalItems currentPage pageSize }
    }
  }
}"#
);

const LIBRARIES_DOC: &str = r#"query Libraries($pagination: Pagination!) {
  libraries(pagination: $pagination) {
    nodes { id name description path }
    pageInfo {
      ... on OffsetPaginationInfo { totalPages totalItems currentPage pageSize }
    }
  }
}"#;

const LIBRARY_BY_ID_DOC: &str = r#"query LibraryById($id: ID!) {
  libraryById(id: $id) { id name description path }
}"#;

const UPDATE_SERIES_METADATA_DOC: &str =
    "mutation UpdateSeriesMetadata($id: ID!, $input: SeriesMetadataInput!) { updateSeriesMetadata(id: $id, input: $input) { id } }";

const UPDATE_MEDIA_METADATA_DOC: &str =
    "mutation UpdateMediaMetadata($id: ID!, $input: MediaMetadataInput!) { updateMediaMetadata(id: $id, input: $input) { id } }";

/// 系列 tags（komf `series.tags` 与 genres 分离）走独立 mutation（Stump SeriesMetadataInput 无 tags 列）。
const SET_SERIES_TAGS_DOC: &str =
    "mutation SetSeriesTags($id: ID!, $tags: [String!]!) { setSeriesTags(id: $id, tags: $tags) { id } }";

const RESET_SERIES_METADATA_DOC: &str =
    "mutation ResetSeriesMetadata($id: ID!, $impact: MetadataResetImpact!) { resetSeriesMetadata(id: $id, impact: $impact) { id } }";

const UPLOAD_SERIES_THUMBNAIL_DOC: &str =
    "mutation UploadSeriesThumbnail($id: ID!, $image: String!) { uploadSeriesThumbnailBase64(id: $id, image: $image) { id } }";

const UPLOAD_MEDIA_THUMBNAIL_DOC: &str =
    "mutation UploadMediaThumbnail($id: ID!, $image: String!) { uploadMediaThumbnailBase64(id: $id, image: $image) { id } }";

// ---------------------------------------------------------------------------
// DTO —— 与 Stump GraphQL 返回（camelCase）对齐；字段全部容错（default）
// ---------------------------------------------------------------------------

/// async-graphql 响应信封。
#[derive(Debug, Deserialize)]
struct StumpGraphQLResponse {
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    errors: Option<Vec<StumpGraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct StumpGraphQLError {
    message: Option<String>,
}

/// 分页信息（union `PaginationInfo` 的 offset 形态；仅请求 offset 分页，字段可为空）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StumpPageInfo {
    #[serde(default)]
    pub total_pages: Option<i32>,
    #[serde(default)]
    pub total_items: Option<i64>,
    #[serde(default)]
    pub current_page: Option<i32>,
    #[serde(default)]
    pub page_size: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct StumpPageResponse<T> {
    pub nodes: Vec<T>,
    pub page_info: StumpPageInfo,
}

// serde derive 在 `#[serde(default)]` 字段上会给 T 附加不必要的 `Default` 约束，
// 手动实现以支持任意 T: Deserialize。
impl<'de, T: Deserialize<'de>> Deserialize<'de> for StumpPageResponse<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Inner<T> {
            nodes: Option<Vec<T>>,
            page_info: Option<StumpPageInfo>,
        }
        let inner = Inner::deserialize(deserializer)?;
        Ok(Self {
            nodes: inner.nodes.unwrap_or_default(),
            page_info: inner.page_info.unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StumpSeriesMetadataDto {
    #[serde(default)]
    pub series_id: Option<String>,
    #[serde(default)]
    pub age_rating: Option<i32>,
    #[serde(default)]
    pub description_formatted: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    /// "Continuing" / "Ended" / null
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub total_issues: Option<i32>,
    #[serde(default)]
    pub volume: Option<i32>,
    #[serde(default)]
    pub writers: Vec<String>,
    #[serde(default)]
    pub year: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StumpSeriesDto {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub is_oneshot: bool,
    #[serde(default)]
    pub thumbnail_path: Option<String>,
    #[serde(default)]
    pub library_id: Option<String>,
    #[serde(default)]
    pub media_count: Option<i32>,
    #[serde(default)]
    pub deleted_at: Option<String>,
    #[serde(default)]
    pub metadata: Option<StumpSeriesMetadataDto>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StumpMediaMetadataDto {
    #[serde(default)]
    pub media_id: Option<String>,
    #[serde(default)]
    pub age_rating: Option<i32>,
    #[serde(default)]
    pub day: Option<i32>,
    #[serde(default)]
    pub identifier_isbn: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub month: Option<i32>,
    /// Decimal 标量在 JSON 中以字符串传输（async-graphql rust_decimal 序列化）。
    #[serde(default)]
    pub number: Option<String>,
    #[serde(default)]
    pub page_count: Option<i32>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub title_sort: Option<String>,
    #[serde(default)]
    pub volume: Option<i32>,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub writers: Vec<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub links: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StumpMediaDto {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub extension: Option<String>,
    #[serde(default)]
    pub pages: Option<i32>,
    #[serde(default)]
    pub is_oneshot: bool,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub thumbnail_path: Option<String>,
    #[serde(default)]
    pub series_id: Option<String>,
    #[serde(default)]
    pub library_id: Option<String>,
    #[serde(default)]
    pub deleted_at: Option<String>,
    #[serde(default)]
    pub series: Option<StumpSeriesRefDto>,
    #[serde(default)]
    pub metadata: Option<StumpMediaMetadataDto>,
}

/// Media.series 只取映射 series_title 需要的字段。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StumpSeriesRefDto {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub library_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StumpLibraryDto {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub path: String,
}

/// 登录响应（`generate_token=true` 时返回 `AccessToken` 变体），只取 accessToken。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StumpLoginResponse {
    token: StumpTokenPair,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StumpTokenPair {
    access_token: String,
    /// 预留：JWT 过期时优先用 refresh token 续期（当前实现为失效后重新登录）。
    #[serde(default)]
    #[allow(dead_code)]
    refresh_token: Option<String>,
}

// ---------------------------------------------------------------------------
// 令牌提供者：API Key 优先，否则登录换 JWT
// ---------------------------------------------------------------------------

struct StumpTokenProvider {
    http: reqwest::Client,
    base_uri: String,
    username: String,
    password: String,
    api_key: Option<String>,
    /// 缓存的 JWT access token（API Key 模式不使用）。
    token: Mutex<Option<String>>,
}

impl StumpTokenProvider {
    fn new(http: reqwest::Client, base_uri: &str, username: &str, password: &str, api_key: &str) -> Self {
        Self {
            http,
            base_uri: base_uri.to_string(),
            username: username.to_string(),
            password: password.to_string(),
            api_key: (!api_key.is_empty()).then(|| api_key.to_string()),
            token: Mutex::new(None),
        }
    }

    fn uses_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    /// 当前 Bearer 令牌：API Key 直用；JWT 模式先取缓存，未缓存则登录。
    async fn access_token(&self) -> Result<String, MediaServerError> {
        if let Some(api_key) = &self.api_key {
            return Ok(api_key.clone());
        }
        let mut guard = self.token.lock().await;
        if let Some(token) = guard.as_ref() {
            return Ok(token.clone());
        }
        let token = self.login().await?;
        *guard = Some(token.clone());
        Ok(token)
    }

    /// 401 后失效缓存（仅 JWT 模式有意义），下次请求重新登录。
    async fn invalidate(&self) {
        *self.token.lock().await = None;
    }

    /// `POST {base}/api/v2/auth/login?generate_token=true`
    async fn login(&self) -> Result<String, MediaServerError> {
        let response = self
            .http
            .post(format!("{}/api/v2/auth/login", self.base_uri))
            .query(&[("generate_token", "true")])
            .json(&json!({ "username": self.username, "password": self.password }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        let login: StumpLoginResponse = response.json().await?;
        Ok(login.token.access_token)
    }
}

// ---------------------------------------------------------------------------
// Stump 客户端
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct StumpClient {
    http: reqwest::Client,
    base_uri: String,
    token_provider: Arc<StumpTokenProvider>,
}

impl StumpClient {
    pub fn new(base_uri: &str, username: &str, password: &str, api_key: &str) -> Result<Self, MediaServerError> {
        let base_uri = base_uri.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(MediaServerError::Http)?;
        let token_provider = Arc::new(StumpTokenProvider::new(http.clone(), &base_uri, username, password, api_key));
        Ok(Self {
            http,
            base_uri: base_uri.clone(),
            token_provider,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_uri, path)
    }

    /// 当前 Bearer 令牌（供 WS 事件连接复用）。
    pub async fn access_token(&self) -> Result<String, MediaServerError> {
        self.token_provider.access_token().await
    }

    pub fn base_uri(&self) -> &str {
        &self.base_uri
    }

    /// 底层 HTTP 客户端（供 WS 事件连接复用）。
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    /// 执行 GraphQL 文档，返回 `data` 整体（Value）。
    /// JWT 模式遇 401 会失效缓存并重试一次；API Key 模式不重试。
    async fn gql_value(&self, document: &str, variables: Value) -> Result<Value, MediaServerError> {
        let mut retried = false;
        loop {
            let token = self.token_provider.access_token().await?;
            let response = self
                .http
                .post(self.url("/api/graphql"))
                .bearer_auth(token)
                .json(&json!({ "query": document, "variables": variables }))
                .send()
                .await?;
            let status = response.status();
            if status == reqwest::StatusCode::UNAUTHORIZED && !retried && !self.token_provider.uses_api_key() {
                // access token 过期：清缓存重新登录后重试一次
                self.token_provider.invalidate().await;
                retried = true;
                continue;
            }
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(MediaServerError::Status(status, body));
            }
            let envelope: StumpGraphQLResponse = response.json().await?;
            if let Some(errors) = envelope.errors {
                if !errors.is_empty() {
                    let messages = errors
                        .iter()
                        .filter_map(|e| e.message.clone())
                        .collect::<Vec<_>>()
                        .join("; ");
                    return Err(MediaServerError::message(format!("stump graphql error: {messages}")));
                }
            }
            return Ok(envelope.data.unwrap_or(Value::Null));
        }
    }

    /// 执行 GraphQL 文档并从 `data.<data_field>` 反序列化。
    async fn gql_data<T: DeserializeOwned>(
        &self,
        document: &str,
        variables: Value,
        data_field: &str,
    ) -> Result<T, MediaServerError> {
        let data = self.gql_value(document, variables).await?;
        let field = data.get(data_field).cloned().unwrap_or(Value::Null);
        Ok(serde_json::from_value(field)?)
    }

    // -- 读取 ----------------------------------------------------------------

    pub async fn get_series_dto(&self, series_id: &str) -> Result<StumpSeriesDto, MediaServerError> {
        self.gql_data(SERIES_BY_ID_DOC, json!({ "id": series_id }), "seriesById")
            .await
    }

    /// 分页拉取某图书馆的系列。`page_number` 为 0 基（对齐 komf/Komga 约定）。
    pub async fn get_series_page_dto(
        &self,
        library_id: &str,
        page_number: i32,
    ) -> Result<StumpPageResponse<StumpSeriesDto>, MediaServerError> {
        let variables = json!({
            "filter": { "libraryId": { "eq": library_id } },
            // zeroBased=false 时 page 从 1 起，故 +1；pageSize 100 与 Komga 默认一致
            "pagination": { "offset": { "page": page_number + 1, "pageSize": 100, "zeroBased": false } }
        });
        self.gql_data(SERIES_PAGE_DOC, variables, "series").await
    }

    pub async fn get_media_dto(&self, media_id: &str) -> Result<StumpMediaDto, MediaServerError> {
        self.gql_data(MEDIA_BY_ID_DOC, json!({ "id": media_id }), "mediaById")
            .await
    }

    /// 拉取系列下全部书籍（顶层 media 按 seriesId 过滤 + offset 分页，每页 1000，翻页直到取完）。
    pub async fn get_media_of_series_dto(&self, series_id: &str) -> Result<Vec<StumpMediaDto>, MediaServerError> {
        let filter = json!({ "seriesId": { "eq": series_id } });
        let mut all = Vec::new();
        let mut page = 1;
        loop {
            let variables = json!({
                "filter": filter,
                "pagination": { "offset": { "page": page, "pageSize": 1000, "zeroBased": false } }
            });
            let page_data: StumpPageResponse<StumpMediaDto> =
                self.gql_data(MEDIA_PAGE_DOC, variables, "media").await?;
            let count = page_data.nodes.len();
            all.extend(page_data.nodes);
            let total_pages = page_data.page_info.total_pages.unwrap_or(0);
            if page >= total_pages || count == 0 {
                break;
            }
            page += 1;
        }
        Ok(all)
    }

    pub async fn get_libraries_dto(&self) -> Result<Vec<StumpLibraryDto>, MediaServerError> {
        let variables = json!({
            "pagination": { "offset": { "page": 1, "pageSize": 100, "zeroBased": false } }
        });
        self.gql_data(LIBRARIES_DOC, variables, "libraries")
            .await
            .map(|page: StumpPageResponse<StumpLibraryDto>| page.nodes)
    }

    pub async fn get_library_dto(&self, library_id: &str) -> Result<StumpLibraryDto, MediaServerError> {
        self.gql_data(LIBRARY_BY_ID_DOC, json!({ "id": library_id }), "libraryById")
            .await
    }

    /// REST 缩略图（GET /api/v2/series/{id}/thumbnail），404 视为无图。
    pub async fn get_series_thumbnail_image(&self, series_id: &str) -> Result<Option<Image>, MediaServerError> {
        self.get_thumbnail_image(&format!("/api/v2/series/{series_id}/thumbnail")).await
    }

    /// REST 缩略图（GET /api/v2/media/{id}/thumbnail），404 视为无图。
    pub async fn get_book_thumbnail_image(&self, media_id: &str) -> Result<Option<Image>, MediaServerError> {
        self.get_thumbnail_image(&format!("/api/v2/media/{media_id}/thumbnail")).await
    }

    async fn get_thumbnail_image(&self, path: &str) -> Result<Option<Image>, MediaServerError> {
        let token = self.token_provider.access_token().await?;
        let response = self.http.get(self.url(path)).bearer_auth(token).send().await?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaServerError::Status(status, body));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bytes = response.bytes().await?;
        Ok(Some(Image::new(bytes.to_vec(), content_type)))
    }

    // -- 写入 ----------------------------------------------------------------

    pub async fn update_series_metadata(&self, series_id: &str, input: &Value) -> Result<(), MediaServerError> {
        self.gql_value(
            UPDATE_SERIES_METADATA_DOC,
            json!({ "id": series_id, "input": input }),
        )
        .await
        .map(|_| ())
    }

    pub async fn update_media_metadata(&self, media_id: &str, input: &Value) -> Result<(), MediaServerError> {
        self.gql_value(
            UPDATE_MEDIA_METADATA_DOC,
            json!({ "id": media_id, "input": input }),
        )
        .await
        .map(|_| ())
    }

    /// 设置系列 tags（整表替换；仅系列级，书级 tags 走 MediaMetadataInput.genres）。
    pub async fn set_series_tags(&self, series_id: &str, tags: &[String]) -> Result<(), MediaServerError> {
        self.gql_value(
            SET_SERIES_TAGS_DOC,
            json!({ "id": series_id, "tags": tags }),
        )
        .await
        .map(|_| ())
    }

    /// 重置系列元数据（impact=SERIES，仅清系列自身；BOOKS/EVERYTHING 语义见 Stump 文档）。
    pub async fn reset_series_metadata(&self, series_id: &str) -> Result<(), MediaServerError> {
        self.gql_value(
            RESET_SERIES_METADATA_DOC,
            json!({ "id": series_id, "impact": "SERIES" }),
        )
        .await
        .map(|_| ())
    }

    /// base64 上传系列封面（Stump 专为 Komf 添加的 mutation；服务端按魔数校验
    /// PNG/JPEG/WebP/GIF/HEIF/JXL/AVIF 且受 max_file_upload_size 限制）。
    pub async fn upload_series_thumbnail_base64(&self, series_id: &str, image: &Image) -> Result<(), MediaServerError> {
        let encoded = BASE64.encode(&image.bytes);
        self.gql_value(
            UPLOAD_SERIES_THUMBNAIL_DOC,
            json!({ "id": series_id, "image": encoded }),
        )
        .await
        .map(|_| ())
    }

    pub async fn upload_book_thumbnail_base64(&self, media_id: &str, image: &Image) -> Result<(), MediaServerError> {
        let encoded = BASE64.encode(&image.bytes);
        self.gql_value(
            UPLOAD_MEDIA_THUMBNAIL_DOC,
            json!({ "id": media_id, "image": encoded }),
        )
        .await
        .map(|_| ())
    }
}

// ---------------------------------------------------------------------------
// 映射：Stump DTO -> MediaServer 领域模型
// ---------------------------------------------------------------------------

/// Stump 系列状态（series_metadata.status，Continuing/Ended）-> komf SeriesStatus。
fn from_stump_status(status: &Option<String>) -> Option<SeriesStatus> {
    match status.as_deref() {
        Some("Ended") => Some(SeriesStatus::Ended),
        Some("Continuing") => Some(SeriesStatus::Ongoing),
        _ => None,
    }
}

/// komf SeriesStatus -> Stump 状态字符串（与 mylar 映射一致：结束类 -> Ended，其余 -> Continuing）。
fn to_stump_status(status: SeriesStatus) -> &'static str {
    match status {
        SeriesStatus::Ended | SeriesStatus::Completed => "Ended",
        SeriesStatus::Ongoing | SeriesStatus::Hiatus | SeriesStatus::Abandoned => "Continuing",
    }
}

/// 作者角色是否为"作者"（Stump 系列级只有 writers 单列表，无角色；仅收纳作家类角色）。
fn is_writer_role(role: &str) -> bool {
    matches!(
        role.to_ascii_lowercase().as_str(),
        "writer" | "story" | "scenario" | "script"
    )
}

/// Stump links（纯 URL 字符串）-> WebLink（label 近似取 URL 本身，文档注明）。
fn to_web_links(links: &[String]) -> Vec<WebLink> {
    links
        .iter()
        .map(|url| WebLink {
            label: url.clone(),
            url: url.clone(),
        })
        .collect()
}

/// 解析 "YYYY-MM-DD" / "YYYY-MM" / "YYYY" 为 (year, month, day)。
fn parse_release_date(value: &str) -> (Option<i32>, Option<u32>, Option<u32>) {
    let parts: Vec<&str> = value.split('-').collect();
    let year = parts.first().and_then(|s| s.parse::<i32>().ok());
    let month = parts.get(1).and_then(|s| s.parse::<u32>().ok());
    let day = parts.get(2).and_then(|s| s.parse::<u32>().ok());
    (year, month, day)
}

fn to_media_server_series(dto: &StumpSeriesDto, base_uri: &str) -> MediaServerSeries {
    let metadata = dto.metadata.as_ref();
    let title = metadata
        .and_then(|m| m.title.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| dto.name.clone());
    let authors = metadata
        .map(|m| {
            m.writers
                .iter()
                .map(|name| MediaServerAuthor {
                    name: name.clone(),
                    role: "Writer".to_string(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let links = metadata.map(|m| to_web_links(&m.links)).unwrap_or_default();

    MediaServerSeries {
        id: MediaServerSeriesId(dto.id.clone()),
        library_id: MediaServerLibraryId(dto.library_id.clone().unwrap_or_default()),
        name: dto.name.clone(),
        books_count: dto.media_count.unwrap_or(0),
        metadata: MediaServerSeriesMetadata {
            status: from_stump_status(&metadata.and_then(|m| m.status.clone())),
            title,
            // Stump 无 title_sort 列，保持空（mylar/识别逻辑不依赖）
            title_sort: String::new(),
            alternative_titles: Vec::new(),
            summary: metadata.and_then(|m| m.summary.clone()).unwrap_or_default(),
            reading_direction: None,
            publisher: metadata.and_then(|m| m.publisher.clone()),
            alternative_publishers: Vec::new(),
            age_rating: metadata.and_then(|m| m.age_rating),
            language: None,
            genres: metadata.map(|m| m.genres.clone()).unwrap_or_default(),
            tags: Vec::new(),
            total_book_count: metadata.and_then(|m| m.total_issues),
            authors,
            release_year: metadata.and_then(|m| m.year),
            links: links.clone(),
            ..Default::default()
        },
        books_metadata_links: links,
        url: format!("{}/series/{}", base_uri, dto.id),
        deleted: dto.deleted_at.is_some(),
        oneshot: dto.is_oneshot,
    }
}

fn to_media_server_book(dto: &StumpMediaDto, series_name: &str, base_uri: &str) -> MediaServerBook {
    let metadata = dto.metadata.as_ref();
    let title = metadata
        .and_then(|m| m.title.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| dto.name.clone());
    let number = metadata
        .and_then(|m| m.number.clone())
        .and_then(|n| n.parse::<f64>().ok())
        .map(|n| n as i32)
        .unwrap_or(0);
    let number_str = metadata.and_then(|m| m.number.clone()).unwrap_or_default();
    let authors = metadata
        .map(|m| {
            m.writers
                .iter()
                .map(|name| MediaServerAuthor {
                    name: name.clone(),
                    role: "Writer".to_string(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let release_date = metadata.and_then(|m| {
        let (year, month, day) = (m.year, m.month, m.day);
        match (year, month, day) {
            (Some(y), Some(mo), Some(d)) => Some(format!("{y:04}-{mo:02}-{d:02}")),
            (Some(y), Some(mo), None) => Some(format!("{y:04}-{mo:02}")),
            (Some(y), None, None) => Some(format!("{y:04}")),
            _ => None,
        }
    });

    MediaServerBook {
        id: MediaServerBookId(dto.id.clone()),
        series_id: MediaServerSeriesId(dto.series_id.clone().or_else(|| dto.series.as_ref().map(|s| s.id.clone())).unwrap_or_default()),
        library_id: dto
            .library_id
            .clone()
            .or_else(|| dto.series.as_ref().and_then(|s| s.library_id.clone()))
            .map(MediaServerLibraryId),
        series_title: series_name.to_string(),
        name: dto.name.clone(),
        url: format!("{}/books/{}", base_uri, dto.id),
        file_name: dto
            .path
            .as_deref()
            .map(Path::new)
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or(&dto.name)
            .to_string(),
        number,
        oneshot: dto.is_oneshot,
        metadata: MediaServerBookMetadata {
            title,
            summary: metadata.and_then(|m| m.summary.clone()).unwrap_or_default(),
            number: number_str,
            number_sort: None,
            release_date,
            authors,
            tags: Vec::new(),
            isbn: metadata.and_then(|m| m.identifier_isbn.clone()),
            links: metadata.map(|m| to_web_links(&m.links)).unwrap_or_default(),
            ..Default::default()
        },
        deleted: dto.deleted_at.is_some(),
    }
}

fn to_media_server_library(dto: &StumpLibraryDto) -> MediaServerLibrary {
    MediaServerLibrary {
        id: MediaServerLibraryId(dto.id.clone()),
        name: dto.name.clone(),
        // Stump 图书馆为单根路径；Komga roots 语义取单元素列表
        roots: vec![dto.path.clone()],
    }
}

// ---------------------------------------------------------------------------
// 更新输入构造：MediaServer*MetadataUpdate -> Stump GraphQL Input（camelCase）
// ---------------------------------------------------------------------------

/// 构造 `SeriesMetadataInput`。Stump 无对应列的字段（titleSort/readingDirection/
/// language/alternativeTitles/alternativePublishers）静默丢弃；tags 经独立
/// `setSeriesTags` 写入（见适配器 `update_series_metadata`）。
fn build_series_metadata_input(metadata: &MediaServerSeriesMetadataUpdate) -> Value {
    let mut input = serde_json::Map::new();
    if let Some(status) = metadata.status {
        input.insert("status".to_string(), json!(to_stump_status(status)));
    }
    if let Some(title) = &metadata.title {
        input.insert("title".to_string(), json!(title.name));
    }
    if let Some(summary) = &metadata.summary {
        input.insert("summary".to_string(), json!(summary));
    }
    if let Some(publisher) = &metadata.publisher {
        input.insert("publisher".to_string(), json!(publisher));
    }
    if let Some(age_rating) = metadata.age_rating {
        input.insert("ageRating".to_string(), json!(age_rating));
    }
    if let Some(genres) = &metadata.genres {
        if !genres.is_empty() {
            input.insert("genres".to_string(), json!(genres));
        }
    }
    if let Some(links) = &metadata.links {
        let urls: Vec<String> = links.iter().map(|l| l.url.clone()).collect();
        if !urls.is_empty() {
            input.insert("links".to_string(), json!(urls));
        }
    }
    if let Some(total_book_count) = metadata.total_book_count {
        input.insert("totalIssues".to_string(), json!(total_book_count));
    }
    if let Some(authors) = &metadata.authors {
        let writers: Vec<String> = authors
            .iter()
            .filter(|a| is_writer_role(&a.role))
            .map(|a| a.name.clone())
            .collect();
        if !writers.is_empty() {
            input.insert("writers".to_string(), json!(writers));
        }
    }
    if let Some(release_year) = metadata.release_year {
        input.insert("year".to_string(), json!(release_year));
    }
    Value::Object(input)
}

/// 构造 `MediaMetadataInput`。字段全部可写；None = 不改（Stump ActiveValue 语义）。
fn build_media_metadata_input(metadata: &MediaServerBookMetadataUpdate) -> Value {
    let mut input = serde_json::Map::new();
    if let Some(title) = &metadata.title {
        input.insert("title".to_string(), json!(title));
    }
    if let Some(summary) = &metadata.summary {
        input.insert("summary".to_string(), json!(summary));
    }
    if let Some(number) = &metadata.number {
        // Decimal 标量以字符串传输
        input.insert("number".to_string(), json!(number));
    }
    if let Some(authors) = &metadata.authors {
        let writers: Vec<String> = authors
            .iter()
            .filter(|a| is_writer_role(&a.role))
            .map(|a| a.name.clone())
            .collect();
        if !writers.is_empty() {
            input.insert("writers".to_string(), json!(writers));
        }
    }
    if let Some(tags) = &metadata.tags {
        if !tags.is_empty() {
            input.insert("genres".to_string(), json!(tags));
        }
    }
    if let Some(isbn) = &metadata.isbn {
        input.insert("identifierIsbn".to_string(), json!(isbn));
    }
    if let Some(links) = &metadata.links {
        let urls: Vec<String> = links.iter().map(|l| l.url.clone()).collect();
        if !urls.is_empty() {
            input.insert("links".to_string(), json!(urls));
        }
    }
    if let Some(release_date) = &metadata.release_date {
        let (year, month, day) = parse_release_date(release_date);
        if let Some(year) = year {
            input.insert("year".to_string(), json!(year));
        }
        if let Some(month) = month {
            input.insert("month".to_string(), json!(month));
        }
        if let Some(day) = day {
            input.insert("day".to_string(), json!(day));
        }
    }
    Value::Object(input)
}

/// 书级重置（Stump 无 resetMediaMetadata）：用 updateMediaMetadata 置空可清字段。
/// 注意 Stump 对 None 输入为"不改"，故清空用空串/空列表；number/identifiers 等
/// 无法用空值表达，需 Stump 侧补 resetMediaMetadata 才能完全等价（TODO）。
fn build_media_reset_input(book: &MediaServerBook) -> Value {
    json!({
        "title": book.metadata.title,
        "summary": "",
        "writers": [],
        "genres": [],
        "links": [],
        "publisher": ""
    })
}

// ---------------------------------------------------------------------------
// MediaServerClient 适配器
// ---------------------------------------------------------------------------

pub struct StumpMediaServerClientAdapter {
    client: StumpClient,
}

impl StumpMediaServerClientAdapter {
    pub fn new(client: StumpClient) -> Self {
        Self { client }
    }

    /// 暴露底层客户端（事件监听/测试用）。
    pub fn client(&self) -> &StumpClient {
        &self.client
    }
}

#[async_trait::async_trait]
impl MediaServerClient for StumpMediaServerClientAdapter {
    async fn get_series(&self, series_id: &MediaServerSeriesId) -> Result<MediaServerSeries, MediaServerError> {
        let dto = self.client.get_series_dto(&series_id.0).await?;
        Ok(to_media_server_series(&dto, self.client.base_uri()))
    }

    async fn get_series_page(
        &self,
        library_id: &MediaServerLibraryId,
        page_number: i32,
    ) -> Result<Page<MediaServerSeries>, MediaServerError> {
        let page = self.client.get_series_page_dto(&library_id.0, page_number).await?;
        let content = page
            .nodes
            .iter()
            .map(|dto| to_media_server_series(dto, self.client.base_uri()))
            .collect::<Vec<_>>();
        Ok(Page {
            content,
            page_number: page.page_info.current_page.unwrap_or(page_number + 1) - 1,
            total_elements: page.page_info.total_items.unwrap_or(0),
            total_pages: page.page_info.total_pages.unwrap_or(1),
        })
    }

    async fn get_series_thumbnail(&self, series_id: &MediaServerSeriesId) -> Result<Option<Image>, MediaServerError> {
        self.client.get_series_thumbnail_image(&series_id.0).await
    }

    async fn get_series_thumbnails(
        &self,
        series_id: &MediaServerSeriesId,
    ) -> Result<Vec<MediaServerSeriesThumbnail>, MediaServerError> {
        // Stump 单一缩略图模型：有 thumbnailPath 则合成一条记录，否则为空
        let dto = self.client.get_series_dto(&series_id.0).await?;
        Ok(if dto.thumbnail_path.is_some() {
            vec![MediaServerSeriesThumbnail {
                id: MediaServerThumbnailId(format!("{}-thumbnail", series_id.0)),
                series_id: series_id.clone(),
                r#type: "thumbnail".to_string(),
                selected: true,
            }]
        } else {
            Vec::new()
        })
    }

    async fn get_book(&self, book_id: &MediaServerBookId) -> Result<MediaServerBook, MediaServerError> {
        let dto = self.client.get_media_dto(&book_id.0).await?;
        let series_name = dto
            .series
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_default();
        Ok(to_media_server_book(&dto, &series_name, self.client.base_uri()))
    }

    async fn get_books(&self, series_id: &MediaServerSeriesId) -> Result<Vec<MediaServerBook>, MediaServerError> {
        let dtos = self.client.get_media_of_series_dto(&series_id.0).await?;
        Ok(dtos
            .iter()
            .map(|dto| {
                let series_name = dto
                    .series
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                to_media_server_book(dto, &series_name, self.client.base_uri())
            })
            .collect())
    }

    async fn get_book_thumbnails(
        &self,
        book_id: &MediaServerBookId,
    ) -> Result<Vec<MediaServerBookThumbnail>, MediaServerError> {
        // 同系列：单缩略图模型，有 thumbnailPath 则合成一条
        let dto = self.client.get_media_dto(&book_id.0).await?;
        Ok(if dto.thumbnail_path.is_some() {
            vec![MediaServerBookThumbnail {
                id: MediaServerThumbnailId(format!("{}-thumbnail", book_id.0)),
                book_id: book_id.clone(),
                r#type: "thumbnail".to_string(),
                selected: true,
                file_size: None,
            }]
        } else {
            Vec::new()
        })
    }

    async fn get_book_thumbnail(&self, book_id: &MediaServerBookId) -> Result<Option<Image>, MediaServerError> {
        self.client.get_book_thumbnail_image(&book_id.0).await
    }

    async fn get_library(&self, library_id: &MediaServerLibraryId) -> Result<MediaServerLibrary, MediaServerError> {
        let dto = self.client.get_library_dto(&library_id.0).await?;
        Ok(to_media_server_library(&dto))
    }

    async fn get_libraries(&self) -> Result<Vec<MediaServerLibrary>, MediaServerError> {
        let dtos = self.client.get_libraries_dto().await?;
        Ok(dtos.iter().map(to_media_server_library).collect())
    }

    // 收藏夹：Stump Reading List 为书级且 update 未实现，沿用 trait 默认（不支持）。

    async fn update_series_metadata(
        &self,
        series_id: &MediaServerSeriesId,
        metadata: &MediaServerSeriesMetadataUpdate,
    ) -> Result<(), MediaServerError> {
        let input = build_series_metadata_input(metadata);
        self.client.update_series_metadata(&series_id.0, &input).await?;
        // 系列 tags（与 genres 分离）经独立 setSeriesTags 写入
        if let Some(tags) = &metadata.tags {
            if !tags.is_empty() {
                self.client.set_series_tags(&series_id.0, tags).await?;
            }
        }
        Ok(())
    }

    async fn delete_series_thumbnail(
        &self,
        _series_id: &MediaServerSeriesId,
        _thumbnail_id: &MediaServerThumbnailId,
    ) -> Result<(), MediaServerError> {
        // Stump 无删除缩略图 API；上传即整体替换，删除为 no-op
        Ok(())
    }

    async fn update_book_metadata(
        &self,
        book_id: &MediaServerBookId,
        metadata: &MediaServerBookMetadataUpdate,
    ) -> Result<(), MediaServerError> {
        let input = build_media_metadata_input(metadata);
        self.client.update_media_metadata(&book_id.0, &input).await
    }

    async fn delete_book_thumbnail(
        &self,
        _book_id: &MediaServerBookId,
        _thumbnail_id: &MediaServerThumbnailId,
    ) -> Result<(), MediaServerError> {
        // 同 delete_series_thumbnail：no-op
        Ok(())
    }

    async fn reset_book_metadata(
        &self,
        book: &MediaServerBook,
        _book_number: Option<i32>,
    ) -> Result<(), MediaServerError> {
        // Stump 无 resetMediaMetadata，用 updateMediaMetadata 置空可清字段
        let input = build_media_reset_input(book);
        self.client.update_media_metadata(&book.id.0, &input).await
    }

    async fn reset_series_metadata(&self, series: &MediaServerSeries) -> Result<(), MediaServerError> {
        // Stump 的 resetSeriesMetadata(impact=SERIES) 只清 series_metadata 表；
        // series_tags 是独立关联表（setSeriesTags 写入），需显式清空——
        // 对齐 Komga reset（series_metadata_reset_request 中 tags 置空 Vec）语义。
        self.client.reset_series_metadata(&series.id.0).await?;
        self.client.set_series_tags(&series.id.0, &[]).await
    }

    async fn upload_series_thumbnail(
        &self,
        series_id: &MediaServerSeriesId,
        thumbnail: &Image,
        _selected: bool,
        _lock: bool,
    ) -> Result<Option<MediaServerSeriesThumbnail>, MediaServerError> {
        // Stump 单一缩略图：selected/lock 无意义，上传即替换
        self.client.upload_series_thumbnail_base64(&series_id.0, thumbnail).await?;
        Ok(Some(MediaServerSeriesThumbnail {
            id: MediaServerThumbnailId(format!("{}-thumbnail", series_id.0)),
            series_id: series_id.clone(),
            r#type: "thumbnail".to_string(),
            selected: true,
        }))
    }

    async fn upload_book_thumbnail(
        &self,
        book_id: &MediaServerBookId,
        thumbnail: &Image,
        _selected: bool,
        _lock: bool,
    ) -> Result<Option<MediaServerBookThumbnail>, MediaServerError> {
        self.client.upload_book_thumbnail_base64(&book_id.0, thumbnail).await?;
        Ok(Some(MediaServerBookThumbnail {
            id: MediaServerThumbnailId(format!("{}-thumbnail", book_id.0)),
            book_id: book_id.clone(),
            r#type: "thumbnail".to_string(),
            selected: true,
            file_size: None,
        }))
    }

    async fn refresh_metadata(
        &self,
        _library_id: &MediaServerLibraryId,
        _series_id: &MediaServerSeriesId,
    ) -> Result<(), MediaServerError> {
        // Stump 无对应端点（analyze_series 语义不同）；元数据变更即生效，no-op
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 接线状态（对应可行性分析方案，已实施并通过本地 Stump 端到端测试）：
// 1. `StumpConfig`（baseUri/username/password/apiKey/eventListener/metadataUpdate）
//    已在 `crate::config`，环境变量 `KOMF_STUMP_*` 与 application.example.yml 就绪；
// 2. `MediaServerModule::new` 增加 stump_config 参数并注册 StumpClient 与每库
//    MetadataService；`MediaServer::Stump` + `as_str()`；
// 3. komf-app：AppState 持 stump_client/stump_services，`ServerKind::Stump`，
//    `/api/stump/*` 与废弃 `/stump/*` 路由已接通；config DTO 已暴露 stump 段；
// 4. `stump_event.rs`：WS 订阅 `readEvents`（graphql-transport-ws，tokio-tungstenite），
//    批量聚合（JobStarted/JobUpdate 窗口）+ CreatedMedia / CreatedOrUpdatedManyMedia
//    → on_books_added 触发元数据更新（派发时按 seriesId 拉书籍补真实 book id）；
// 5. `failedMatchCollectionName` 对 Stump 关闭（Reading List 书级不支持，get_collections 默认空）。
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn series_dto_sample() -> Value {
        json!({
            "id": "series-1",
            "name": "Berserk",
            "description": "desc",
            "path": "/l/berserk",
            "status": "READY",
            "isOneshot": false,
            "thumbnailPath": "/thumb.png",
            "libraryId": "lib-1",
            "mediaCount": 41,
            "deletedAt": null,
            "metadata": {
                "seriesId": "series-1",
                "ageRating": 17,
                "genres": ["Action", "Dark Fantasy"],
                "links": ["https://example.com/series/berserk"],
                "publisher": "Hakusensha",
                "status": "Continuing",
                "summary": "A dark fantasy.",
                "title": "Berserk",
                "totalIssues": 41,
                "volume": 1,
                "writers": ["Kentaro Miura"],
                "year": 1989
            }
        })
    }

    #[test]
    fn deserialize_series_dto() {
        let dto: StumpSeriesDto = serde_json::from_value(series_dto_sample()).unwrap();
        assert_eq!(dto.id, "series-1");
        assert_eq!(dto.metadata.as_ref().unwrap().writers, vec!["Kentaro Miura"]);
        assert_eq!(dto.media_count, Some(41));
    }

    #[test]
    fn deserialize_page_response() {
        let value = json!({
            "nodes": [series_dto_sample()],
            "pageInfo": { "totalPages": 2, "totalItems": 101, "currentPage": 1, "pageSize": 100 }
        });
        let page: StumpPageResponse<StumpSeriesDto> = serde_json::from_value(value).unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert_eq!(page.page_info.total_items, Some(101));
    }

    #[test]
    fn status_mapping_roundtrip() {
        assert_eq!(from_stump_status(&Some("Ended".into())), Some(SeriesStatus::Ended));
        assert_eq!(from_stump_status(&Some("Continuing".into())), Some(SeriesStatus::Ongoing));
        assert_eq!(from_stump_status(&None), None);
        assert_eq!(to_stump_status(SeriesStatus::Ongoing), "Continuing");
        assert_eq!(to_stump_status(SeriesStatus::Hiatus), "Continuing");
        assert_eq!(to_stump_status(SeriesStatus::Ended), "Ended");
        assert_eq!(to_stump_status(SeriesStatus::Completed), "Ended");
    }

    #[test]
    fn release_date_parsing() {
        assert_eq!(parse_release_date("2016-09-02"), (Some(2016), Some(9), Some(2)));
        assert_eq!(parse_release_date("2016-09"), (Some(2016), Some(9), None));
        assert_eq!(parse_release_date("2016"), (Some(2016), None, None));
    }

    #[test]
    fn series_metadata_input_builder() {
        let update = MediaServerSeriesMetadataUpdate {
            status: Some(SeriesStatus::Ended),
            title: Some(komf_core::model::SeriesTitle {
                name: "Berserk".into(),
                r#type: None,
                language: None,
            }),
            summary: Some("summary".into()),
            publisher: Some("Hakusensha".into()),
            age_rating: Some(17),
            genres: Some(vec!["Action".into()]),
            links: Some(vec![WebLink { label: "x".into(), url: "https://x".into() }]),
            total_book_count: Some(41),
            authors: Some(vec![
                MediaServerAuthor { name: "Kentaro Miura".into(), role: "Writer".into() },
                MediaServerAuthor { name: "Someone".into(), role: "Penciller".into() },
            ]),
            release_year: Some(1989),
            ..Default::default()
        };
        let input = build_series_metadata_input(&update);
        assert_eq!(input["status"], "Ended");
        assert_eq!(input["title"], "Berserk");
        assert_eq!(input["totalIssues"], 41);
        // 仅收纳 Writer 角色
        assert_eq!(input["writers"], json!(["Kentaro Miura"]));
        // 无对应列字段不出现
        assert!(input.get("titleSort").is_none());
    }

    #[test]
    fn media_metadata_input_builder() {
        let update = MediaServerBookMetadataUpdate {
            title: Some("Vol 1".into()),
            summary: Some("summary".into()),
            number: Some("1.5".into()),
            release_date: Some("2016-09-02".into()),
            authors: Some(vec![MediaServerAuthor { name: "Kentaro Miura".into(), role: "Writer".into() }]),
            isbn: Some("978-4-00-000000-0".into()),
            links: Some(vec![WebLink { label: "x".into(), url: "https://x".into() }]),
            ..Default::default()
        };
        let input = build_media_metadata_input(&update);
        assert_eq!(input["number"], "1.5");
        assert_eq!(input["year"], 2016);
        assert_eq!(input["month"], 9);
        assert_eq!(input["day"], 2);
        assert_eq!(input["identifierIsbn"], "978-4-00-000000-0");
    }
}
