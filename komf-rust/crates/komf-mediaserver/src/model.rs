//! 媒体服务器领域模型 —— 对应 `snd.komf.mediaserver.model` 包。
use komf_core::model::{ReadingDirection, SeriesStatus, WebLink};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaServerSeriesId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaServerBookId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaServerLibraryId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaServerThumbnailId(pub String);

#[derive(Debug, Clone)]
pub struct MediaServerSeries {
    pub id: MediaServerSeriesId,
    pub library_id: MediaServerLibraryId,
    pub name: String,
    pub books_count: i32,
    pub metadata: MediaServerSeriesMetadata,
    /// 系列聚合书籍元数据（komga SeriesDto.booksMetadata）中的 links：
    /// oneshot 单本系列 provider 链接常写在书籍级，聚合后直接可用（无需遍历/额外请求）
    pub books_metadata_links: Vec<WebLink>,
    pub url: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Default)]
pub struct MediaServerSeriesMetadata {
    pub status: Option<SeriesStatus>,
    pub title: String,
    pub title_sort: String,
    pub alternative_titles: Vec<MediaServerAlternativeTitle>,
    pub summary: String,
    pub reading_direction: Option<ReadingDirection>,
    pub publisher: Option<String>,
    pub alternative_publishers: Vec<String>,
    pub age_rating: Option<i32>,
    pub language: Option<String>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub total_book_count: Option<i32>,
    pub authors: Vec<MediaServerAuthor>,
    pub release_year: Option<i32>,
    pub links: Vec<WebLink>,

    pub status_lock: bool,
    pub title_lock: bool,
    pub title_sort_lock: bool,
    pub alternative_titles_lock: bool,
    pub summary_lock: bool,
    pub reading_direction_lock: bool,
    pub publisher_lock: bool,
    pub age_rating_lock: bool,
    pub language_lock: bool,
    pub genres_lock: bool,
    pub tags_lock: bool,
    pub total_book_count_lock: bool,
    pub authors_lock: bool,
    pub release_year_lock: bool,
    pub links_lock: bool,
}

#[derive(Debug, Clone)]
pub struct MediaServerAlternativeTitle {
    pub label: String,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct MediaServerAuthor {
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone)]
pub struct MediaServerSeriesThumbnail {
    pub id: MediaServerThumbnailId,
    pub series_id: MediaServerSeriesId,
    pub r#type: String,
    pub selected: bool,
}

#[derive(Debug, Clone)]
pub struct MediaServerBook {
    pub id: MediaServerBookId,
    pub series_id: MediaServerSeriesId,
    pub library_id: Option<MediaServerLibraryId>,
    pub series_title: String,
    pub name: String,
    pub url: String,
    /// 文件名（komga: fileName；kavita: 文件 stem）。eHentai gid 提取候选。
    pub file_name: String,
    pub number: i32,
    pub oneshot: bool,
    pub metadata: MediaServerBookMetadata,
    pub deleted: bool,
}

#[derive(Debug, Clone, Default)]
pub struct MediaServerBookMetadata {
    pub title: String,
    pub summary: String,
    pub number: String,
    pub number_sort: Option<String>,
    pub release_date: Option<String>,
    pub authors: Vec<MediaServerAuthor>,
    pub tags: Vec<String>,
    pub isbn: Option<String>,
    pub links: Vec<WebLink>,

    pub title_lock: bool,
    pub summary_lock: bool,
    pub number_lock: bool,
    pub number_sort_lock: bool,
    pub release_date_lock: bool,
    pub authors_lock: bool,
    pub tags_lock: bool,
    pub isbn_lock: bool,
    pub links_lock: bool,
}

#[derive(Debug, Clone)]
pub struct MediaServerBookThumbnail {
    pub id: MediaServerThumbnailId,
    pub book_id: MediaServerBookId,
    pub r#type: String,
    pub selected: bool,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct MediaServerLibrary {
    pub id: MediaServerLibraryId,
    pub name: String,
    pub roots: Vec<String>,
}

/// 收藏夹（Komga Collection）。Rust 扩展：Auto-Identify Library 匹配失败系列归集用。
#[derive(Debug, Clone)]
pub struct MediaServerCollection {
    pub id: String,
    pub name: String,
    pub series_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MediaServerSeriesSearch {
    pub series: MediaServerSeries,
    pub books: Vec<MediaServerBook>,
}

/// 分页 —— 对应 `Page.kt`。
#[derive(Debug, Clone)]
pub struct Page<T> {
    pub content: Vec<T>,
    pub page_number: i32,
    pub total_elements: i64,
    pub total_pages: i32,
}

/// 系列 + 书籍元数据 —— 对应 `SeriesAndBookMetadata.kt`。
#[derive(Debug, Clone)]
pub struct SeriesAndBookMetadata {
    pub series_metadata: komf_core::model::SeriesMetadata,
    pub book_metadata: std::collections::HashMap<MediaServerBookId, Option<komf_core::model::BookMetadata>>,
    /// book_id -> oneshot（对齐 Kotlin `SeriesAndBookMetadata.bookMetadata` 的 key
    /// 是 `MediaServerBook` 对象，含 oneshot 字段；Rust 侧单独携带）。
    pub book_oneshots: std::collections::HashMap<MediaServerBookId, bool>,
}

impl SeriesAndBookMetadata {
    pub fn new(
        series_metadata: komf_core::model::SeriesMetadata,
        book_metadata: std::collections::HashMap<MediaServerBookId, Option<komf_core::model::BookMetadata>>,
    ) -> Self {
        Self {
            series_metadata,
            book_metadata,
            book_oneshots: std::collections::HashMap::new(),
        }
    }

    /// 携带每个 book 的 oneshot 信息（对齐 Kotlin key 为 MediaServerBook）。
    pub fn with_oneshots(
        mut self,
        books: &[MediaServerBook],
    ) -> Self {
        self.book_oneshots = books
            .iter()
            .map(|book| (book.id.clone(), book.oneshot))
            .collect();
        self
    }

    /// 合并时保留 original 的 oneshot 信息。
    pub fn with_book_oneshots(
        mut self,
        book_oneshots: std::collections::HashMap<MediaServerBookId, bool>,
    ) -> Self {
        self.book_oneshots = book_oneshots;
        self
    }
}

/// 媒体服务器类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaServer {
    Komga,
    Kavita,
}

impl MediaServer {
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaServer::Komga => "KOMGA",
            MediaServer::Kavita => "KAVITA",
        }
    }
}

/// 系列元数据更新负载 —— 对应 `MediaServerSeriesMetadataUpdate.kt`。
#[derive(Debug, Clone, Default)]
pub struct MediaServerSeriesMetadataUpdate {
    pub status: Option<SeriesStatus>,
    pub title: Option<komf_core::model::SeriesTitle>,
    pub title_sort: Option<komf_core::model::SeriesTitle>,
    pub alternative_titles: Option<Vec<(String, Option<komf_core::model::TitleType>, Option<String>)>>,
    pub summary: Option<String>,
    pub publisher: Option<String>,
    pub alternative_publishers: Option<Vec<String>>,
    pub reading_direction: Option<ReadingDirection>,
    pub age_rating: Option<i32>,
    pub language: Option<String>,
    pub genres: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub total_book_count: Option<i32>,
    pub authors: Option<Vec<MediaServerAuthor>>,
    pub release_year: Option<i32>,
    pub links: Option<Vec<WebLink>>,

    pub status_lock: Option<bool>,
    pub title_lock: Option<bool>,
    pub title_sort_lock: Option<bool>,
    pub alternative_titles_lock: Option<bool>,
    pub summary_lock: Option<bool>,
    pub publisher_lock: Option<bool>,
    pub reading_direction_lock: Option<bool>,
    pub age_rating_lock: Option<bool>,
    pub language_lock: Option<bool>,
    pub genres_lock: Option<bool>,
    pub tags_lock: Option<bool>,
    pub total_book_count_lock: Option<bool>,
    pub links_lock: Option<bool>,
}

/// 书籍元数据更新负载 —— 对应 `MediaServerBookMetadataUpdate.kt`。
#[derive(Debug, Clone, Default)]
pub struct MediaServerBookMetadataUpdate {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub release_date: Option<String>,
    pub authors: Option<Vec<MediaServerAuthor>>,
    pub tags: Option<Vec<String>>,
    pub isbn: Option<String>,
    pub links: Option<Vec<WebLink>>,
    pub number: Option<String>,
    pub number_sort: Option<f64>,

    pub title_lock: Option<bool>,
    pub summary_lock: Option<bool>,
    pub number_lock: Option<bool>,
    pub number_sort_lock: Option<bool>,
    pub release_date_lock: Option<bool>,
    pub authors_lock: Option<bool>,
    pub tags_lock: Option<bool>,
    pub isbn_lock: Option<bool>,
    pub links_lock: Option<bool>,
}
