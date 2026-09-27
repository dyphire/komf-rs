//! 媒体服务器客户端接口 —— 对应 `MediaServerClient.kt`。
use crate::model::*;
use komf_core::model::Image;

#[derive(Debug, thiserror::Error)]
pub enum MediaServerError {
    #[error("{0}")]
    Message(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("media server returned status {0}: {1}")]
    Status(reqwest::StatusCode, String),
    #[error("resource not found: {0}")]
    NotFound(String),
    /// ComicInfo 写入/移除失败（对应 Kotlin `ComicInfoWriter.ComicInfoException`）。
    #[error("comic info error: {0}")]
    ComicInfo(String),
    /// mylar series.json 写入/封面保存失败。
    #[error("mylar export error: {0}")]
    Mylar(String),
    /// SignalR SSE transport 接受握手但在首帧超时内不推送任何数据
    /// （部分 Kavita 实例/版本行为），触发事件监听降级到 LongPolling。
    #[error("kavita signalr sse transport silent (no data within timeout)")]
    SseSilent,
}

impl MediaServerError {
    pub fn message(msg: impl Into<String>) -> Self {
        MediaServerError::Message(msg.into())
    }
}

#[async_trait::async_trait]
pub trait MediaServerClient: Send + Sync {
    async fn get_series(&self, series_id: &MediaServerSeriesId) -> Result<MediaServerSeries, MediaServerError>;
    async fn get_series_page(
        &self,
        library_id: &MediaServerLibraryId,
        page_number: i32,
    ) -> Result<Page<MediaServerSeries>, MediaServerError>;
    async fn get_series_thumbnail(&self, series_id: &MediaServerSeriesId) -> Result<Option<Image>, MediaServerError>;
    async fn get_series_thumbnails(
        &self,
        series_id: &MediaServerSeriesId,
    ) -> Result<Vec<MediaServerSeriesThumbnail>, MediaServerError>;
    async fn get_book(&self, book_id: &MediaServerBookId) -> Result<MediaServerBook, MediaServerError>;
    async fn get_books(&self, series_id: &MediaServerSeriesId) -> Result<Vec<MediaServerBook>, MediaServerError>;
    async fn get_book_thumbnails(
        &self,
        book_id: &MediaServerBookId,
    ) -> Result<Vec<MediaServerBookThumbnail>, MediaServerError>;
    async fn get_book_thumbnail(&self, book_id: &MediaServerBookId) -> Result<Option<Image>, MediaServerError>;
    async fn get_library(&self, library_id: &MediaServerLibraryId) -> Result<MediaServerLibrary, MediaServerError>;
    async fn get_libraries(&self) -> Result<Vec<MediaServerLibrary>, MediaServerError>;

    /// 收藏夹列表（Rust 扩展，Auto-Identify 失败归集用）。默认不支持。
    async fn get_collections(&self) -> Result<Vec<MediaServerCollection>, MediaServerError> {
        Ok(Vec::new())
    }
    /// 创建收藏夹（Rust 扩展）。默认不支持。
    async fn create_collection(
        &self,
        _name: &str,
        _series_ids: &[String],
    ) -> Result<MediaServerCollection, MediaServerError> {
        Err(MediaServerError::message("collections not supported"))
    }
    /// 全量更新收藏夹 seriesIds（Rust 扩展）。默认不支持。
    async fn update_collection_series(
        &self,
        _collection_id: &str,
        _series_ids: &[String],
    ) -> Result<(), MediaServerError> {
        Err(MediaServerError::message("collections not supported"))
    }

    async fn update_series_metadata(
        &self,
        series_id: &MediaServerSeriesId,
        metadata: &MediaServerSeriesMetadataUpdate,
    ) -> Result<(), MediaServerError>;
    async fn delete_series_thumbnail(
        &self,
        series_id: &MediaServerSeriesId,
        thumbnail_id: &MediaServerThumbnailId,
    ) -> Result<(), MediaServerError>;
    async fn update_book_metadata(
        &self,
        book_id: &MediaServerBookId,
        metadata: &MediaServerBookMetadataUpdate,
    ) -> Result<(), MediaServerError>;
    async fn delete_book_thumbnail(
        &self,
        book_id: &MediaServerBookId,
        thumbnail_id: &MediaServerThumbnailId,
    ) -> Result<(), MediaServerError>;

    async fn reset_book_metadata(
        &self,
        book: &MediaServerBook,
        book_number: Option<i32>,
    ) -> Result<(), MediaServerError>;
    async fn reset_series_metadata(
        &self,
        series: &MediaServerSeries,
    ) -> Result<(), MediaServerError>;

    async fn upload_series_thumbnail(
        &self,
        series_id: &MediaServerSeriesId,
        thumbnail: &Image,
        selected: bool,
        lock: bool,
    ) -> Result<Option<MediaServerSeriesThumbnail>, MediaServerError>;

    async fn upload_book_thumbnail(
        &self,
        book_id: &MediaServerBookId,
        thumbnail: &Image,
        selected: bool,
        lock: bool,
    ) -> Result<Option<MediaServerBookThumbnail>, MediaServerError>;

    async fn refresh_metadata(
        &self,
        library_id: &MediaServerLibraryId,
        series_id: &MediaServerSeriesId,
    ) -> Result<(), MediaServerError>;
}
