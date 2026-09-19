//! BookWalker Provider —— 对应 `snd.komf.providers.bookwalker` 包。
//!
//! BookWalker 是离线数据库驱动的 provider：元数据来自官方导出的 SQLite
//! 数据库（`https://static.bookwalker.com/data/bkwk-db.sqlite.zst`，zstd 压缩），
//! 由 `BookWalkerDbDownloader` 下载并建立 FTS5 全文索引后供查询。
//!
//! 对应 Kotlin 文件：`BookWalkerMetadataProvider.kt`、`BookWalkerMapper.kt`、
//! `db/BookWalkerDbDownloader.kt`、`db/BookWalkerSeriesRepository.kt`、
//! `db/tables/*.kt`、`model/*.kt`。

use crate::config::{BookMetadataConfig, ProviderConfig, SeriesMetadataConfig};
use crate::model::{
    Author, AuthorRole, BookMetadata, BookRange, Image, MatchQuery, ProviderBookId,
    ProviderBookMetadata, ProviderSeriesId, ProviderSeriesMetadata, ReleaseDate, SeriesBook,
    SeriesMetadata, SeriesSearchResult, SeriesTitle, TitleType, WebLink,
};
use crate::providers::{CoreProviders, MetadataProvider, ProviderError};
use crate::util::NameSimilarityMatcher;
use chrono::{Datelike, NaiveDate};
use komf_api_models::config::DownloadProgress;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const BOOK_WALKER_BASE_URL: &str = "https://bookwalker.com";
const SOS_BRIGADE_CDN: &str = "https://img.sos-dan.net";
const DATABASE_URL: &str = "https://static.bookwalker.com/data/bkwk-db.sqlite.zst";
/// 与 Kotlin `DOWNLOAD_BUFFER_SIZE`（1024 * 1024）一致。

// ---------------------------------------------------------------------------
// 内容类型 / 格式 / 角色（valueOf 语义 = `entries.getOrNull(number) ?: 默认`）
// ---------------------------------------------------------------------------

/// 对应 `BookWalkerContentType.kt`。Kotlin `valueOf(number)` 按 **entries 索引**
/// 取值：`[0]=MANGA [1]=NOVEL [2]=WEBTOONS [3]=AUDIOBOOK`，越界回退 MANGA。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookWalkerContentType {
    Manga,
    Novel,
    Webtoons,
    Audiobook,
}

impl BookWalkerContentType {
    /// 读方向：对应 Kotlin `valueOf(number)`，按 **entries 索引** 取值
    /// （`[0]=MANGA [1]=NOVEL [2]=WEBTOONS [3]=AUDIOBOOK`，越界回退 MANGA）。
    /// 这是 Kotlin 原版的既有行为（疑似 off-by-one bug），Rust 忠实复刻。
    fn value_of(number: i64) -> Self {
        match number {
            0 => Self::Manga,
            1 => Self::Novel,
            2 => Self::Webtoons,
            3 => Self::Audiobook,
            _ => Self::Manga,
        }
    }

    /// 写/过滤方向：对应 Kotlin `BookWalkerContentType.number` **属性**，
    /// 声明值为 `MANGA=1 NOVEL=2 WEBTOONS=3 AUDIOBOOK=4`，即真实 DB `series.type`
    /// 列存的原始整数。Kotlin 搜索 SQL 绑定的是 `.number`（1..=4），而非 entries 下标。
    /// 若此处误用下标（0..=3），Manga 配置会过滤 `type IN (0)` 而对真实库查不到任何结果。
    fn number(self) -> i64 {
        match self {
            Self::Manga => 1,
            Self::Novel => 2,
            Self::Webtoons => 3,
            Self::Audiobook => 4,
        }
    }
}

/// 对应 `BookWalkerBookFormat.kt`：`valueOf` 按 entries 索引，越界回退 EBOOK。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookWalkerBookFormat {
    Ebook,
    Audiobook,
}

impl BookWalkerBookFormat {
    fn value_of(number: i64) -> Self {
        match number {
            0 => Self::Ebook,
            1 => Self::Audiobook,
            _ => Self::Ebook,
        }
    }
}

/// 对应 `BookWalkerContributorRole.kt`：number 0..=25 按 entries 索引，其余 UNKNOWN。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookWalkerContributorRole {
    Contributor,
    Author,
    Artist,
    Illustrator,
    Unknown4,
    Unknown5,
    Colorist,
    Letterer,
    Editor,
    Translator,
    Narrator,
    OriginalAuthor,
    OriginalCharacterDesign,
    Adaptation,
    Compilation,
    ConsultingEditor,
    CoverDesign,
    Creator,
    Designer,
    Coordination,
    Idea,
    Unknown21,
    Producer,
    Script,
    Text,
    With,
    Unknown,
}

impl BookWalkerContributorRole {
    fn value_of(number: i64) -> Self {
        match number {
            0 => Self::Contributor,
            1 => Self::Author,
            2 => Self::Artist,
            3 => Self::Illustrator,
            4 => Self::Unknown4,
            5 => Self::Unknown5,
            6 => Self::Colorist,
            7 => Self::Letterer,
            8 => Self::Editor,
            9 => Self::Translator,
            10 => Self::Narrator,
            11 => Self::OriginalAuthor,
            12 => Self::OriginalCharacterDesign,
            13 => Self::Adaptation,
            14 => Self::Compilation,
            15 => Self::ConsultingEditor,
            16 => Self::CoverDesign,
            17 => Self::Creator,
            18 => Self::Designer,
            19 => Self::Coordination,
            20 => Self::Idea,
            21 => Self::Unknown21,
            22 => Self::Producer,
            23 => Self::Script,
            24 => Self::Text,
            25 => Self::With,
            _ => Self::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// 模型 —— 对应 `model/*.kt`
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct BookWalkerImage {
    pub id: String,
    pub name: String,
    #[allow(dead_code)]
    pub mime: String,
    pub width: i64,
    pub height: i64,
}

impl BookWalkerImage {
    /// 对应 Kotlin `url600`：`{CDN}/600/{id[0..2]}/{id[3]}/{id[4]}/{id[5..]}.webp`。
    fn url600(&self) -> String {
        let bytes = self.id.as_bytes();
        let (first, second, third, rest) = if bytes.len() >= 5 {
            (
                // Kotlin：first=id[0..3]，second=id[3], third=id[4], last=id[5..]
                &self.id[0..3],
                &self.id[3..4],
                &self.id[4..5],
                &self.id[5..],
            )
        } else {
            (self.id.as_str(), "", "", "")
        };
        format!("{SOS_BRIGADE_CDN}/600/{first}/{second}/{third}/{rest}.webp")
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // 保留 Kotlin 模型对齐（当前不被消费）
pub struct BookWalkerTag {
    id: String,
    name: String,
    #[allow(dead_code)]
    slug: String,
    #[allow(dead_code)]
    description: String,
    #[allow(dead_code)]
    namespace: i64,
    #[allow(dead_code)]
    priority: i64,
}

#[derive(Debug, Clone)]
pub struct BookWalkerContributor {
    id: String,
    role: BookWalkerContributorRole,
    name: String,
}

#[derive(Debug, Clone)]
pub struct BookWalkerSeries {
    pub id: String,
    pub type_: BookWalkerContentType,
    pub title: String,
    pub alt_titles: Vec<String>,
    pub description: String,
    pub listed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub tags: Vec<BookWalkerTag>,
    pub image: Option<BookWalkerImage>,
}

impl BookWalkerSeries {
    /// 对应 Kotlin `url`：`{base}/series/{id 去掉 CNT_ 前缀}`。
    fn url(&self) -> String {
        format!(
            "{BOOK_WALKER_BASE_URL}/series/{}",
            self.id.strip_prefix("CNT_").unwrap_or(&self.id)
        )
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // 保留 Kotlin 模型对齐（contentType/format/altTitles 当前不被消费）
pub struct BookWalkerBook {
    id: String,
    content_id: String,
    series_id: String,
    level: i64,
    content_type: BookWalkerContentType,
    format: BookWalkerBookFormat,
    title: String,
    alt_titles: Vec<String>,
    display_title: String,
    description: String,
    display_order: f64,
    on_sale_at: Option<chrono::DateTime<chrono::Utc>>,
    contributors: Vec<BookWalkerContributor>,
    image: Option<BookWalkerImage>,
    isbn: Option<String>,
}

impl BookWalkerBook {
    /// 对应 Kotlin `url`：level == 3 → `/chapter/`，否则 `/volume/`。
    fn url(&self) -> String {
        let kind = if self.level == 3 { "chapter" } else { "volume" };
        format!(
            "{BOOK_WALKER_BASE_URL}/{kind}/{}",
            self.content_id
                .strip_prefix("CNT_")
                .unwrap_or(&self.content_id)
        )
    }
}

// ---------------------------------------------------------------------------
// 时间解析：Kotlin `Instant.parse`（ISO-8601）；兼容常见导出格式
// ---------------------------------------------------------------------------

fn parse_instant(text: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(text, fmt) {
            return Some(dt.and_utc());
        }
        if let Ok(date) = NaiveDate::parse_from_str(text, fmt) {
            return Some(date.and_hms_opt(0, 0, 0)?.and_utc());
        }
    }
    // 可能的 epoch 秒/毫秒
    if let Ok(secs) = text.parse::<i64>() {
        if let Some(dt) = chrono::DateTime::from_timestamp(secs, 0) {
            return Some(dt);
        }
        if let Some(dt) = chrono::DateTime::from_timestamp_millis(secs) {
            return Some(dt);
        }
    }
    None
}

fn parse_alt_titles(json: Option<String>) -> Vec<String> {
    match json {
        Some(text) => serde_json::from_str::<Vec<String>>(&text).unwrap_or_default(),
        None => Vec::new(),
    }
}

fn release_date_from(dt: Option<chrono::DateTime<chrono::Utc>>) -> Option<ReleaseDate> {
    let dt = dt?;
    Some(ReleaseDate::new(
        Some(dt.year()),
        Some(dt.month()),
        Some(dt.day()),
    ))
}

// ---------------------------------------------------------------------------
// 数据访问 —— 对应 `BookWalkerSeriesRepository.kt`
// ---------------------------------------------------------------------------

/// SQLite 只读仓储。连接按查询打开（SQLite 打开开销极小），保证 `Sync`；
/// 行为与 Kotlin 每次 `transaction(database)` 等价。
pub struct BookWalkerSeriesRepository {
    database_file: PathBuf,
}

const SERIES_COLUMNS: &str = "s.id, s.type, s.title, s.alt_titles, s.subtitle, s.display_title, \
     s.display_title_short, s.description, s.description_short, s.listed_at, s.image_id, \
     i.name AS i_name, i.mime AS i_mime, i.width AS i_width, i.height AS i_height";

impl BookWalkerSeriesRepository {
    pub fn new(database_file: impl Into<PathBuf>) -> Self {
        Self {
            database_file: database_file.into(),
        }
    }

    fn open(&self) -> Result<rusqlite::Connection, ProviderError> {
        rusqlite::Connection::open(&self.database_file)
            .map_err(|e| ProviderError::message(format!("failed to open BookWalker database: {e}")))
    }

    fn row_to_series(row: &rusqlite::Row<'_>) -> rusqlite::Result<BookWalkerSeries> {
        // Kotlin `toImageModel()`：image_id 为 NULL → None
        let image = match row.get::<_, Option<String>>("image_id")? {
            Some(image_id) => Some(BookWalkerImage {
                id: image_id,
                name: row.get("i_name").unwrap_or_default(),
                // Kotlin 原版此处取 name 列（疑似笔误）；Rust 侧取真实 mime 列，
                // 该字段在映射链路中不被消费，行为无差异。
                mime: row.get("i_mime").unwrap_or_default(),
                width: row.get("i_width").unwrap_or(0),
                height: row.get("i_height").unwrap_or(0),
            }),
            None => None,
        };
        Ok(BookWalkerSeries {
            id: row.get("id")?,
            type_: BookWalkerContentType::value_of(row.get("type")?),
            title: row.get("title")?,
            alt_titles: parse_alt_titles(row.get("alt_titles")?),
            description: row.get("description")?,
            listed_at: row
                .get::<_, Option<String>>("listed_at")?
                .as_deref()
                .and_then(parse_instant),
            tags: Vec::new(),
            image,
        })
    }

    fn load_series_tags(
        &self,
        conn: &rusqlite::Connection,
        ids: &[String],
    ) -> rusqlite::Result<std::collections::HashMap<String, Vec<BookWalkerTag>>> {
        let mut out = std::collections::HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT st.series_id AS series_id, t.id AS id, t.name AS name, t.slug AS slug, \
             t.description AS description, t.namespace AS namespace, t.priority AS priority \
             FROM series_tags st LEFT JOIN tags t ON st.tag_id = t.id WHERE st.series_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(ids))?;
        while let Some(row) = rows.next()? {
            out.entry(row.get::<_, String>("series_id")?)
                .or_insert_with(Vec::new)
                .push(BookWalkerTag {
                    id: row.get("id")?,
                    name: row.get("name")?,
                    slug: row.get("slug")?,
                    description: row.get("description")?,
                    namespace: row.get("namespace")?,
                    priority: row.get("priority")?,
                });
        }
        Ok(out)
    }

    /// 对应 `search`：FTS5 MATCH（`"$title"` 精确短语）+ type IN + `ORDER BY rank LIMIT 10`。
    pub fn search(
        &self,
        title: &str,
        content_types: &[BookWalkerContentType],
    ) -> Result<Vec<BookWalkerSeries>, ProviderError> {
        let conn = self.open()?;
        let quoted = format!("\"{title}\"");
        let mut sql =
            String::from("SELECT id FROM series_fts WHERE (title MATCH ? OR alt_titles MATCH ?)");
        let params: Vec<rusqlite::types::Value> = vec![
            rusqlite::types::Value::Text(quoted.clone()),
            rusqlite::types::Value::Text(quoted),
        ];
        if !content_types.is_empty() {
            let types = content_types
                .iter()
                .map(|t| format!("{}", t.number()))
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND type IN ({types})"));
        }
        sql.push_str(" ORDER BY rank LIMIT 10");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| ProviderError::message(format!("BookWalker search prepare: {e}")))?;
        let ids: Vec<String> = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| row.get(0))
            .map_err(|e| ProviderError::message(format!("BookWalker search: {e}")))?
            .collect::<Result<_, _>>()
            .map_err(|e| ProviderError::message(format!("BookWalker search rows: {e}")))?;
        drop(stmt);

        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.fetch_series_by_ids(&conn, &ids)
    }

    fn fetch_series_by_ids(
        &self,
        conn: &rusqlite::Connection,
        ids: &[String],
    ) -> Result<Vec<BookWalkerSeries>, ProviderError> {
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT {SERIES_COLUMNS} FROM series s LEFT JOIN images i ON s.image_id = i.id \
             WHERE s.id IN ({placeholders})"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| ProviderError::message(format!("BookWalker series prepare: {e}")))?;
        let mut series_list =
            {
                let mut rows = stmt
                    .query(rusqlite::params_from_iter(ids))
                    .map_err(|e| ProviderError::message(format!("BookWalker series query: {e}")))?;
                let mut series_list = Vec::new();
                while let Some(row) = rows
                    .next()
                    .map_err(|e| ProviderError::message(format!("BookWalker series rows: {e}")))?
                {
                    series_list.push(Self::row_to_series(row).map_err(|e| {
                        ProviderError::message(format!("BookWalker series row: {e}"))
                    })?);
                }
                series_list
            };
        drop(stmt);

        let series_ids: Vec<String> = series_list.iter().map(|s| s.id.clone()).collect();
        let tags = self
            .load_series_tags(conn, &series_ids)
            .map_err(|e| ProviderError::message(format!("BookWalker tags: {e}")))?;
        for s in series_list.iter_mut() {
            if let Some(t) = tags.get(&s.id) {
                s.tags = t.clone();
            }
        }
        Ok(series_list)
    }

    /// 对应 `getSeries`：按 id 查系列，无结果抛错。
    pub fn get_series(&self, series_id: &str) -> Result<BookWalkerSeries, ProviderError> {
        let conn = self.open()?;
        let sql = format!("SELECT {SERIES_COLUMNS} FROM series s LEFT JOIN images i ON s.image_id = i.id WHERE s.id = ?1");
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| ProviderError::message(format!("BookWalker get_series prepare: {e}")))?;
        let series_list = {
            let mut rows = stmt
                .query([series_id])
                .map_err(|e| ProviderError::message(format!("BookWalker get_series: {e}")))?;
            let mut series_list = Vec::new();
            while let Some(row) = rows
                .next()
                .map_err(|e| ProviderError::message(format!("BookWalker get_series rows: {e}")))?
            {
                series_list.push(Self::row_to_series(row).map_err(|e| {
                    ProviderError::message(format!("BookWalker get_series row: {e}"))
                })?);
            }
            series_list
        };
        drop(stmt);
        let mut series = series_list.into_iter().next().ok_or_else(|| {
            ProviderError::message(format!("failed to find series with id {series_id}"))
        })?;
        let tags = self
            .load_series_tags(&conn, &[series.id.clone()])
            .map_err(|e| ProviderError::message(format!("BookWalker tags: {e}")))?;
        if let Some(t) = tags.get(&series.id) {
            series.tags = t.clone();
        }
        Ok(series)
    }

    const BOOK_COLUMNS: &'static str = "p.id, p.image_id, p.content_id, p.series_id, p.level, p.content_type, \
         p.product_type, p.title, p.alt_titles, p.subtitle, p.display_title, p.display_title_short, \
         p.description, p.description_short, p.display_order, p.listed_at, p.label_id, p.geoblock_id, \
         p.display_name, p.copyright, p.on_presale_at, p.on_sale_at, p.off_sale_at, p.add_on, \
         p.add_on_campaign_only, i.name AS i_name, i.mime AS i_mime, i.width AS i_width, \
         i.height AS i_height, e.external_id AS e_external_id";

    fn fetch_books(
        &self,
        conn: &rusqlite::Connection,
        sql: &str,
        params: &[rusqlite::types::Value],
    ) -> Result<Vec<BookWalkerBook>, ProviderError> {
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| ProviderError::message(format!("BookWalker books prepare: {e}")))?;
        let mut books = {
            let mut rows = stmt
                .query(rusqlite::params_from_iter(params.iter()))
                .map_err(|e| ProviderError::message(format!("BookWalker books query: {e}")))?;
            let mut books = Vec::new();
            while let Some(row) = rows
                .next()
                .map_err(|e| ProviderError::message(format!("BookWalker books rows: {e}")))?
            {
                let image = match row.get::<_, Option<String>>("image_id")? {
                    Some(image_id) => Some(BookWalkerImage {
                        id: image_id,
                        name: row.get("i_name").unwrap_or_default(),
                        mime: row.get("i_mime").unwrap_or_default(),
                        width: row.get("i_width").unwrap_or(0),
                        height: row.get("i_height").unwrap_or(0),
                    }),
                    None => None,
                };
                books.push(BookWalkerBook {
                    id: row.get("id")?,
                    content_id: row.get("content_id")?,
                    series_id: row.get("series_id")?,
                    level: row.get("level")?,
                    content_type: BookWalkerContentType::value_of(row.get("content_type")?),
                    format: BookWalkerBookFormat::value_of(row.get("product_type")?),
                    title: row.get("title")?,
                    alt_titles: parse_alt_titles(row.get("alt_titles")?),
                    display_title: row.get("display_title")?,
                    description: row.get("description")?,
                    display_order: row.get("display_order")?,
                    on_sale_at: row
                        .get::<_, Option<String>>("on_sale_at")?
                        .as_deref()
                        .and_then(parse_instant),
                    contributors: Vec::new(),
                    image,
                    isbn: row.get("e_external_id")?,
                });
            }
            books
        };
        drop(stmt);

        let book_ids: Vec<String> = books.iter().map(|b| b.id.clone()).collect();
        // Kotlin `fetchAndMapBooks` 同时加载 tags/contributors；其中 book tags 在
        // `toBookMetadata` 中被注释不消费（`// tags = book.tags...`），故仅加载 contributors。
        let contributors = self
            .load_contributors(conn, &book_ids)
            .map_err(|e| ProviderError::message(format!("BookWalker contributors: {e}")))?;
        for b in books.iter_mut() {
            if let Some(c) = contributors.get(&b.id) {
                b.contributors = c.clone();
            }
        }
        Ok(books)
    }

    fn load_contributors(
        &self,
        conn: &rusqlite::Connection,
        ids: &[String],
    ) -> rusqlite::Result<std::collections::HashMap<String, Vec<BookWalkerContributor>>> {
        let mut out = std::collections::HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT pc.product_id AS product_id, c.id AS id, c.name AS name, pc.role AS role \
             FROM product_contributors pc LEFT JOIN contributors c ON pc.contributor_id = c.id \
             WHERE pc.product_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(ids))?;
        while let Some(row) = rows.next()? {
            out.entry(row.get::<_, String>("product_id")?)
                .or_insert_with(Vec::new)
                .push(BookWalkerContributor {
                    id: row.get("id")?,
                    role: BookWalkerContributorRole::value_of(row.get("role")?),
                    name: row.get("name")?,
                });
        }
        Ok(out)
    }

    /// 对应 `getSeriesBooks`：products LEFT JOIN images LEFT JOIN product_external_ids(type=3)。
    pub fn get_series_books(&self, series_id: &str) -> Result<Vec<BookWalkerBook>, ProviderError> {
        let conn = self.open()?;
        let sql = format!(
            "SELECT {} FROM products p \
             LEFT JOIN images i ON p.image_id = i.id \
             LEFT JOIN product_external_ids e ON p.id = e.product_id AND e.type = 3 \
             WHERE p.series_id = ?1",
            Self::BOOK_COLUMNS
        );
        self.fetch_books(
            &conn,
            &sql,
            &[rusqlite::types::Value::Text(series_id.to_string())],
        )
    }

    /// 对应 `getBook`：按书 id 查询。
    pub fn get_book(&self, book_id: &str) -> Result<BookWalkerBook, ProviderError> {
        let conn = self.open()?;
        let sql = format!(
            "SELECT {} FROM products p \
             LEFT JOIN images i ON p.image_id = i.id \
             LEFT JOIN product_external_ids e ON p.id = e.product_id AND e.type = 3 \
             WHERE p.id = ?1",
            Self::BOOK_COLUMNS
        );
        self.fetch_books(
            &conn,
            &sql,
            &[rusqlite::types::Value::Text(book_id.to_string())],
        )
        .and_then(|mut books| {
            books.drain(..).next().ok_or_else(|| {
                ProviderError::message(format!("failed to find book with id {book_id}"))
            })
        })
    }
}

// ---------------------------------------------------------------------------
// 映射 —— 对应 `BookWalkerMapper.kt` + `MetadataConfigApplier.kt`
// ---------------------------------------------------------------------------

pub struct BookWalkerMetadataMapper {
    series_metadata_config: SeriesMetadataConfig,
    book_metadata_config: BookMetadataConfig,
    author_roles: Vec<AuthorRole>,
    artist_roles: Vec<AuthorRole>,
}

impl BookWalkerMetadataMapper {
    pub fn new(
        series_metadata_config: SeriesMetadataConfig,
        book_metadata_config: BookMetadataConfig,
        author_roles: Vec<AuthorRole>,
        artist_roles: Vec<AuthorRole>,
    ) -> Self {
        Self {
            series_metadata_config,
            book_metadata_config,
            author_roles,
            artist_roles,
        }
    }

    pub fn to_series_metadata(
        &self,
        series: &BookWalkerSeries,
        books: &[BookWalkerBook],
        thumbnail: Option<Image>,
    ) -> ProviderSeriesMetadata {
        let cfg = &self.series_metadata_config;

        // titles = [main(LOCALIZED, "en")] + altTitles(type/language 空)
        let main_title = SeriesTitle {
            name: series.title.clone(),
            r#type: Some(TitleType::Localized),
            language: Some("en".to_string()),
        };
        let mut titles = vec![main_title.clone()];
        titles.extend(series.alt_titles.iter().map(|t| SeriesTitle {
            name: t.clone(),
            r#type: None,
            language: None,
        }));
        if !cfg.title {
            for t in titles.iter_mut() {
                t.r#type = None;
                t.language = None;
            }
        }
        let title_field = cfg.title.then_some(main_title);

        // summary：Kotlin 直接赋 series.description（非空）
        let summary = cfg.summary.then(|| series.description.clone());

        // publisher：Kotlin 恒 null（数据库无 series/book 关联）
        let publisher = None;

        let reading_direction = cfg
            .reading_direction
            .then(|| match series.type_ {
                BookWalkerContentType::Manga => Some(crate::model::ReadingDirection::RightToLeft),
                BookWalkerContentType::Webtoons => Some(crate::model::ReadingDirection::Webtoon),
                BookWalkerContentType::Novel | BookWalkerContentType::Audiobook => None,
            })
            .flatten();

        let tags = if cfg.tags {
            series.tags.iter().map(|t| t.name.clone()).collect()
        } else {
            Vec::new()
        };

        let total_book_count = cfg.total_book_count.then(|| books.len() as i32);

        let authors = if cfg.authors {
            get_authors_from_books(books, &self.author_roles, &self.artist_roles)
        } else {
            Vec::new()
        };

        let release_date = cfg
            .release_date
            .then(|| release_date_from(series.listed_at))
            .flatten();

        let links = if cfg.links {
            vec![WebLink {
                label: "BookWalker".to_string(),
                url: series.url(),
            }]
        } else {
            Vec::new()
        };

        let metadata = SeriesMetadata {
            status: None,
            title: title_field,
            titles,
            summary,
            publisher,
            alternative_publishers: Vec::new(),
            reading_direction,
            age_rating: None,
            language: None,
            genres: Vec::new(),
            tags,
            total_book_count,
            authors,
            release_date,
            links,
            score: None,
            thumbnail,
        };

        let series_books = if cfg.books {
            books
                .iter()
                .map(|b| SeriesBook {
                    id: ProviderBookId(b.id.clone()),
                    number: Some(BookRange::single(b.display_order)),
                    name: Some(b.display_title.clone()),
                    r#type: None,
                    edition: None,
                })
                .collect()
        } else {
            Vec::new()
        };

        ProviderSeriesMetadata {
            id: ProviderSeriesId(series.id.clone()),
            metadata,
            books: series_books,
        }
    }

    pub fn to_book_metadata(
        &self,
        book: &BookWalkerBook,
        thumbnail: Option<Image>,
    ) -> ProviderBookMetadata {
        let cfg = &self.book_metadata_config;

        let metadata = BookMetadata {
            title: cfg.title.then(|| book.title.clone()),
            summary: cfg.summary.then(|| book.description.clone()),
            number: cfg.number.then(|| BookRange::single(book.display_order)),
            number_sort: None,
            release_date: cfg
                .release_date
                .then(|| book.on_sale_at.map(|dt| dt.format("%Y-%m-%d").to_string()))
                .flatten(),
            authors: if cfg.authors {
                get_authors(&book.contributors, &self.author_roles, &self.artist_roles)
            } else {
                Vec::new()
            },
            tags: Vec::new(),
            isbn: cfg.isbn.then(|| book.isbn.clone()).flatten(),
            links: if cfg.links {
                vec![WebLink {
                    label: "BookWalker".to_string(),
                    url: book.url(),
                }]
            } else {
                Vec::new()
            },
            chapters: Vec::new(),
            story_arcs: None,
            start_chapter: None,
            end_chapter: None,
            thumbnail,
        };

        ProviderBookMetadata {
            id: Some(ProviderBookId(book.id.clone())),
            series_id: Some(ProviderSeriesId(book.series_id.clone())),
            metadata,
        }
    }

    pub fn to_series_search_result(&self, series: &BookWalkerSeries) -> SeriesSearchResult {
        SeriesSearchResult {
            url: Some(series.url()),
            image_url: series.image.as_ref().map(BookWalkerImage::url600),
            title: series.title.clone(),
            provider: CoreProviders::BookWalker.as_str().to_string(),
            result_id: series.id.clone(),
            media_type: None,
            language: None,
        }
    }
}

/// Kotlin `books.flatMap { it.contributors }.distinct()` → 去重后统一映射角色。
fn get_authors_from_books(
    books: &[BookWalkerBook],
    author_roles: &[AuthorRole],
    artist_roles: &[AuthorRole],
) -> Vec<Author> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for book in books {
        for c in &book.contributors {
            let key = (c.id.clone(), c.role as i32, c.name.clone());
            if seen.insert(key) {
                out.extend(get_author_for_role(c, author_roles, artist_roles));
            }
        }
    }
    out
}

fn get_authors(
    contributors: &[BookWalkerContributor],
    author_roles: &[AuthorRole],
    artist_roles: &[AuthorRole],
) -> Vec<Author> {
    let mut out = Vec::new();
    for c in contributors {
        out.extend(get_author_for_role(c, author_roles, artist_roles));
    }
    out
}

/// 对应 Kotlin `getAuthors` 的角色映射表。
fn get_author_for_role(
    c: &BookWalkerContributor,
    author_roles: &[AuthorRole],
    artist_roles: &[AuthorRole],
) -> Vec<Author> {
    use BookWalkerContributorRole::{
        Adaptation, Artist, Author as BwAuthor, Colorist, Compilation, ConsultingEditor,
        Contributor, Coordination, CoverDesign, Creator, Designer, Editor, Idea, Illustrator,
        Letterer, Narrator, OriginalAuthor, OriginalCharacterDesign, Producer, Script, Text,
        Translator, Unknown, Unknown21, Unknown4, Unknown5, With,
    };
    let name = c.name.clone();
    match c.role {
        Contributor
        | Unknown4
        | Unknown5
        | Narrator
        | OriginalCharacterDesign
        | Compilation
        | Coordination
        | Idea
        | Unknown21
        | Producer
        | With
        | Unknown => Vec::new(),
        BwAuthor => author_roles
            .iter()
            .map(|r| Author {
                name: name.clone(),
                role: *r,
            })
            .collect(),
        Artist => artist_roles
            .iter()
            .map(|r| Author {
                name: name.clone(),
                role: *r,
            })
            .collect(),
        Illustrator | Designer => vec![Author {
            name,
            role: AuthorRole::Penciller,
        }],
        Colorist => vec![Author {
            name,
            role: AuthorRole::Colorist,
        }],
        Letterer => vec![Author {
            name,
            role: AuthorRole::Letterer,
        }],
        Editor | ConsultingEditor => vec![Author {
            name,
            role: AuthorRole::Editor,
        }],
        Translator => vec![Author {
            name,
            role: AuthorRole::Translator,
        }],
        CoverDesign => vec![Author {
            name,
            role: AuthorRole::Cover,
        }],
        OriginalAuthor | Adaptation | Creator | Script | Text => {
            vec![Author {
                name,
                role: AuthorRole::Writer,
            }]
        }
    }
}

// ---------------------------------------------------------------------------
// Provider —— 对应 `BookWalkerMetadataProvider.kt`
// ---------------------------------------------------------------------------

pub struct BookWalkerMetadataProvider {
    repository: BookWalkerSeriesRepository,
    metadata_mapper: BookWalkerMetadataMapper,
    name_matcher: NameSimilarityMatcher,
    fetch_series_covers: bool,
    fetch_book_covers: bool,
    http_client: Option<reqwest::Client>,
    category: BookWalkerContentType,
}

impl BookWalkerMetadataProvider {
    /// 库配置 mediaType 优先（Rust 扩展）：COMIC 等不支持类型回退 provider 全局配置。
    fn effective_category(
        &self,
        media_type: Option<crate::model::MediaType>,
    ) -> BookWalkerContentType {
        media_type
            .and_then(content_type_for)
            .unwrap_or(self.category)
    }
    async fn fetch_cover(&self, image: &BookWalkerImage) -> Option<Image> {
        let client = self.http_client.as_ref()?;
        let response = client.get(image.url600()).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = response.bytes().await.ok()?.to_vec();
        Some(Image::new(bytes, mime))
    }
}

#[async_trait::async_trait]
impl MetadataProvider for BookWalkerMetadataProvider {
    fn provider_name(&self) -> CoreProviders {
        CoreProviders::BookWalker
    }

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        let series = self.repository.get_series(&series_id.0)?;
        let books = self.repository.get_series_books(&series.id)?;
        let cover = if self.fetch_series_covers {
            match series.image.as_ref() {
                Some(i) => self.fetch_cover(i).await,
                None => None,
            }
        } else {
            None
        };
        Ok(self
            .metadata_mapper
            .to_series_metadata(&series, &books, cover))
    }

    async fn get_series_cover(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError> {
        let series = self.repository.get_series(&series_id.0)?;
        Ok(match series.image.as_ref() {
            Some(i) => self.fetch_cover(i).await,
            None => None,
        })
    }

    async fn get_book_metadata(
        &self,
        _series_id: &ProviderSeriesId,
        book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError> {
        let book = self.repository.get_book(&book_id.0)?;
        let cover = if self.fetch_book_covers {
            match book.image.as_ref() {
                Some(i) => self.fetch_cover(i).await,
                None => None,
            }
        } else {
            None
        };
        Ok(self.metadata_mapper.to_book_metadata(&book, cover))
    }

    async fn search_series(
        &self,
        series_name: &str,
        _limit: usize,
        media_type: Option<crate::model::MediaType>,
    ) -> Result<Vec<SeriesSearchResult>, ProviderError> {
        let results = self
            .repository
            .search(series_name, &[self.effective_category(media_type)])?;
        Ok(results
            .iter()
            .map(|s| {
                let mut result = self.metadata_mapper.to_series_search_result(s);
                result.media_type = category_media_type(self.effective_category(media_type));
                result
            })
            .collect())
    }

    async fn match_series_metadata(
        &self,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        let series_name = &match_query.series_name;
        let results = self.repository.search(
            series_name,
            &[self.effective_category(match_query.media_type)],
        )?;
        // Kotlin: `it.title + it.altTitles`（List.toString() → "[a, b]"），单元素候选集合
        let matched = results.iter().find(|s| {
            let candidate = format!("{}[{}]", s.title, s.alt_titles.join(", "));
            self.name_matcher.matches(
                &match_query.normalized_series_name(),
                &match_query.normalize_titles(&[candidate]),
            )
        });
        let Some(series) = matched else {
            return Ok(None);
        };
        let books = self.repository.get_series_books(&series.id)?;
        let cover = if self.fetch_series_covers {
            match series.image.as_ref() {
                Some(i) => self.fetch_cover(i).await,
                None => None,
            }
        } else {
            None
        };
        Ok(Some(
            self.metadata_mapper
                .to_series_metadata(series, &books, cover),
        ))
    }
}

/// 对应 `ProvidersModule.kt` 中 BookWalker 的创建条件：
/// 配置启用 **且** 数据库文件存在（Kotlin 中 `bookWalkerDatabase != null`）。
/// MediaType → BookWalkerContentType（COMIC 不支持）。
fn content_type_for(media_type: crate::model::MediaType) -> Option<BookWalkerContentType> {
    match media_type {
        crate::model::MediaType::Manga => Some(BookWalkerContentType::Manga),
        crate::model::MediaType::Webtoon => Some(BookWalkerContentType::Webtoons),
        crate::model::MediaType::Novel => Some(BookWalkerContentType::Novel),
        crate::model::MediaType::Comic => None,
    }
}

/// BookWalkerContentType → MediaType（搜索结果显示用；AUDIOBOOK 无对应 → None）。
fn category_media_type(category: BookWalkerContentType) -> Option<crate::model::MediaType> {
    match category {
        BookWalkerContentType::Manga => Some(crate::model::MediaType::Manga),
        BookWalkerContentType::Novel => Some(crate::model::MediaType::Novel),
        BookWalkerContentType::Webtoons => Some(crate::model::MediaType::Webtoon),
        BookWalkerContentType::Audiobook => None,
    }
}

pub fn create_provider(
    config: &ProviderConfig,
    database_file: Option<&Path>,
    default_name_matcher: NameSimilarityMatcher,
    http_client: &reqwest::Client,
) -> Option<BookWalkerMetadataProvider> {
    if !config.enabled {
        return None;
    }
    let database_file = database_file?;
    if !database_file.exists() {
        tracing::warn!(
            "BookWalker database not found at {}; provider not registered",
            database_file.display()
        );
        return None;
    }
    let category = match config.media_type {
        crate::model::MediaType::Manga => BookWalkerContentType::Manga,
        crate::model::MediaType::Webtoon => BookWalkerContentType::Webtoons,
        crate::model::MediaType::Novel => BookWalkerContentType::Novel,
        crate::model::MediaType::Comic => {
            tracing::warn!("BookWalker does not support comic media type; provider not registered");
            return None;
        }
    };
    let name_matcher = config.name_matching_mode.unwrap_or(default_name_matcher);
    Some(BookWalkerMetadataProvider {
        repository: BookWalkerSeriesRepository::new(database_file.to_path_buf()),
        metadata_mapper: BookWalkerMetadataMapper::new(
            config.series_metadata.clone(),
            config.book_metadata.clone(),
            config.author_roles.clone(),
            config.artist_roles.clone(),
        ),
        name_matcher,
        fetch_series_covers: config.series_metadata.thumbnail,
        fetch_book_covers: config.book_metadata.thumbnail,
        http_client: (config.series_metadata.thumbnail || config.book_metadata.thumbnail)
            .then(|| http_client.clone()),
        category,
    })
}

// ---------------------------------------------------------------------------
// 数据库下载器 —— 对应 `BookWalkerDbDownloader.kt`
// ---------------------------------------------------------------------------

/// BookWalker 官方数据库下载器：下载 zst → 解压 SQLite → 建 FTS5 索引 → 写 timestamp。
///
/// 并发控制与 Kotlin `Mutex.tryLock` 语义一致：下载进行中重复调用不会重启下载，
/// 返回的流 replay 最新事件（`watch` 语义 ≈ `MutableSharedFlow(replay=1)`）。
pub struct BookWalkerDbDownloader {
    work_dir: PathBuf,
    database_file: PathBuf,
    http: reqwest::Client,
    download_in_progress: Arc<std::sync::atomic::AtomicBool>,
    progress: Arc<std::sync::Mutex<Option<tokio::sync::watch::Sender<Option<DownloadProgress>>>>>,
}

impl BookWalkerDbDownloader {
    pub fn new(work_dir: impl Into<PathBuf>, http: reqwest::Client) -> Self {
        let work_dir = work_dir.into();
        Self {
            database_file: work_dir.join("bkwk-db.sqlite"),
            work_dir,
            http,
            download_in_progress: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            progress: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// 数据库文件路径（provider 注册判断用）。
    pub fn database_file(&self) -> PathBuf {
        self.database_file.clone()
    }

    /// 工作目录（timestamp 文件所在目录）。
    pub fn work_dir(&self) -> &PathBuf {
        &self.work_dir
    }

    /// 下载时间戳（`/config` 返回用）：读取 timestamp 文件，等价 Kotlin `downloadTimestamp`。
    pub fn download_timestamp(&self) -> Option<String> {
        std::fs::read_to_string(self.work_dir.join("timestamp"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 启动（或复用进行中的）下载，返回进度事件流。
    /// 事件流在 Finished/Error 后结束（路由侧 `take_while` 消费）。
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
            // 下载进行中：返回当前流（等价 Kotlin 返回同一 progressFlow）
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
        let database_url = DATABASE_URL.to_string();

        if let Err(e) = self
            .download_and_extract(sender, &emit, &database_url)
            .await
        {
            tracing::error!("BookWalker database download failed: {e}");
            let _ = std::fs::remove_file(self.work_dir.join("bkwk-db.sqlite.zst"));
            let _ = std::fs::remove_file(&self.database_file);
            emit(sender, DownloadProgress::ErrorEvent { message: e.clone() });
            return Err(e);
        }
        Ok(())
    }

    async fn download_and_extract(
        &self,
        sender: &tokio::sync::watch::Sender<Option<DownloadProgress>>,
        emit: &impl Fn(&tokio::sync::watch::Sender<Option<DownloadProgress>>, DownloadProgress),
        url: &str,
    ) -> Result<(), String> {
        emit(
            sender,
            DownloadProgress::ProgressEvent {
                total: 0,
                completed: 0,
                info: Some(url.to_string()),
            },
        );
        let _ = std::fs::remove_file(&self.database_file);

        let archive = self.work_dir.join("bkwk-db.sqlite.zst");
        if let Some(parent) = self.work_dir.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::create_dir_all(&self.work_dir);

        // 1. 下载压缩包
        let response = self
            .http
            .get(url)
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
                info: Some(url.to_string()),
            },
        );

        let mut stream = response.bytes_stream();
        let mut file = tokio::fs::File::create(&archive)
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
                    info: Some(url.to_string()),
                },
            );
        }
        file.flush()
            .await
            .map_err(|e| format!("FileSystemException: {e}"))?;
        drop(file);

        // 2. zstd 解压
        emit(
            sender,
            DownloadProgress::ProgressEvent {
                total: 0,
                completed: 0,
                info: Some(format!("extracting {archive:?}")),
            },
        );
        let input =
            std::fs::File::open(&archive).map_err(|e| format!("FileSystemException: {e}"))?;
        let output = std::fs::File::create(&self.database_file)
            .map_err(|e| format!("FileSystemException: {e}"))?;
        zstd::stream::copy_decode(input, output).map_err(|e| format!("ZstdException: {e}"))?;

        // 3. FTS5 搜索索引
        emit(
            sender,
            DownloadProgress::ProgressEvent {
                total: 0,
                completed: 0,
                info: Some("creating search index".to_string()),
            },
        );
        self.create_search_index()?;

        // 4. 写 timestamp + 清理压缩包
        let now = chrono::Utc::now();
        let ts = now.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        std::fs::write(self.work_dir.join("timestamp"), ts)
            .map_err(|e| format!("FileSystemException: {e}"))?;
        let _ = std::fs::remove_file(archive);

        emit(sender, DownloadProgress::FinishedEvent);
        Ok(())
    }

    fn create_search_index(&self) -> Result<(), String> {
        let conn = rusqlite::Connection::open(&self.database_file)
            .map_err(|e| format!("SQLiteException: {e}"))?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS series_fts USING fts5 \
             (id, title, alt_titles, type, tokenize = 'trigram');",
        )
        .map_err(|e| format!("SQLiteException: {e}"))?;
        conn.execute_batch(
            "INSERT INTO series_fts \
             SELECT s.id, s.title, GROUP_CONCAT(json_each.value, ', '), s.type \
             FROM series s, json_each(alt_titles) GROUP BY s.id;",
        )
        .map_err(|e| format!("SQLiteException: {e}"))?;
        Ok(())
    }
}

impl Clone for BookWalkerDbDownloader {
    fn clone(&self) -> Self {
        Self {
            work_dir: self.work_dir.clone(),
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
    use crate::config::{BookMetadataConfig, SeriesMetadataConfig};

    fn test_series() -> BookWalkerSeries {
        BookWalkerSeries {
            id: "CNT_1001".to_string(),
            type_: BookWalkerContentType::Manga,
            title: "Main Title".to_string(),
            alt_titles: vec!["Alt One".to_string(), "Alt Two".to_string()],
            description: "A description".to_string(),
            listed_at: chrono::DateTime::parse_from_rfc3339("2024-05-01T00:00:00Z")
                .map(|d| d.with_timezone(&chrono::Utc))
                .ok(),
            tags: vec![BookWalkerTag {
                id: "t1".to_string(),
                name: "Action".to_string(),
                slug: "action".to_string(),
                description: String::new(),
                namespace: 0,
                priority: 0,
            }],
            image: Some(BookWalkerImage {
                id: "0123456789".to_string(),
                name: "cover".to_string(),
                mime: "image/webp".to_string(),
                width: 600,
                height: 900,
            }),
        }
    }

    fn test_book() -> BookWalkerBook {
        BookWalkerBook {
            id: "CNT_2001".to_string(),
            content_id: "CNT_2001".to_string(),
            series_id: "CNT_1001".to_string(),
            level: 2,
            content_type: BookWalkerContentType::Manga,
            format: BookWalkerBookFormat::Ebook,
            title: "Volume 1".to_string(),
            alt_titles: Vec::new(),
            display_title: "Vol. 1".to_string(),
            description: "Book desc".to_string(),
            display_order: 1.0,
            on_sale_at: chrono::DateTime::parse_from_rfc3339("2024-06-15T00:00:00Z")
                .map(|d| d.with_timezone(&chrono::Utc))
                .ok(),
            contributors: vec![BookWalkerContributor {
                id: "c1".to_string(),
                role: BookWalkerContributorRole::Author,
                name: "Author A".to_string(),
            }],
            image: None,
            isbn: Some("9780000000000".to_string()),
        }
    }

    fn test_mapper() -> BookWalkerMetadataMapper {
        BookWalkerMetadataMapper::new(
            SeriesMetadataConfig::default(),
            BookMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
        )
    }

    #[test]
    fn content_type_value_of_matches_kotlin_entries_index() {
        // Kotlin entries.getOrNull(number) ?: MANGA → [0]=MANGA [1]=NOVEL [2]=WEBTOONS [3]=AUDIOBOOK
        assert_eq!(
            BookWalkerContentType::value_of(0),
            BookWalkerContentType::Manga
        );
        assert_eq!(
            BookWalkerContentType::value_of(1),
            BookWalkerContentType::Novel
        );
        assert_eq!(
            BookWalkerContentType::value_of(2),
            BookWalkerContentType::Webtoons
        );
        assert_eq!(
            BookWalkerContentType::value_of(3),
            BookWalkerContentType::Audiobook
        );
        assert_eq!(
            BookWalkerContentType::value_of(4),
            BookWalkerContentType::Manga
        );
        assert_eq!(
            BookWalkerContentType::value_of(-1),
            BookWalkerContentType::Manga
        );
    }

    #[test]
    fn image_url600_matches_kotlin_path_building() {
        // Kotlin: first=id[0..2] second=id[3] third=id[4] last=id[5..]
        // id "0123456789" → /600/012/3/4/56789.webp（Kotlin first=id[0..2] 闭区间）
        let image = BookWalkerImage {
            id: "0123456789".to_string(),
            name: "c".to_string(),
            mime: "image/webp".to_string(),
            width: 0,
            height: 0,
        };
        assert_eq!(
            image.url600(),
            "https://img.sos-dan.net/600/012/3/4/56789.webp"
        );
    }

    #[test]
    fn series_and_book_url_strip_cnt_prefix() {
        let series = test_series();
        assert_eq!(series.url(), "https://bookwalker.com/series/1001");
        let book = test_book();
        assert_eq!(book.url(), "https://bookwalker.com/volume/2001");
        let chapter = BookWalkerBook {
            level: 3,
            ..test_book()
        };
        assert_eq!(chapter.url(), "https://bookwalker.com/chapter/2001");
    }

    #[test]
    fn to_series_metadata_matches_kotlin_mapper() {
        let mapper = test_mapper();
        let series = test_series();
        let books = vec![
            test_book(),
            BookWalkerBook {
                id: "CNT_2002".to_string(),
                content_id: "CNT_2002".to_string(),
                display_title: "Vol. 2".to_string(),
                display_order: 2.0,
                ..test_book()
            },
        ];
        let meta = mapper.to_series_metadata(&series, &books, None);

        // titles = [main(LOCALIZED, "en")] + altTitles
        assert_eq!(meta.metadata.titles.len(), 3);
        assert_eq!(meta.metadata.titles[0].name, "Main Title");
        assert_eq!(meta.metadata.titles[0].r#type, Some(TitleType::Localized));
        assert_eq!(meta.metadata.titles[0].language.as_deref(), Some("en"));
        assert_eq!(meta.metadata.titles[1].name, "Alt One");
        assert_eq!(meta.metadata.titles[1].r#type, None);
        assert_eq!(meta.metadata.titles[1].language, None);

        // summary = description；publisher 恒 null
        assert_eq!(meta.metadata.summary.as_deref(), Some("A description"));
        assert!(meta.metadata.publisher.is_none());

        // tags = names
        assert_eq!(meta.metadata.tags, vec!["Action"]);

        // readingDirection MANGA → RightToLeft
        assert_eq!(
            meta.metadata.reading_direction,
            Some(crate::model::ReadingDirection::RightToLeft)
        );

        // totalBookCount = books.size
        assert_eq!(meta.metadata.total_book_count, Some(2));

        // releaseDate from listedAt (UTC)
        let rd = meta.metadata.release_date.clone().unwrap();
        assert_eq!(rd.year, Some(2024));
        assert_eq!(rd.month, Some(5));
        assert_eq!(rd.day, Some(1));

        // links
        assert_eq!(meta.metadata.links.len(), 1);
        assert_eq!(meta.metadata.links[0].label, "BookWalker");
        assert_eq!(
            meta.metadata.links[0].url,
            "https://bookwalker.com/series/1001"
        );

        // books = [SeriesBook(id, BookRange(displayOrder), displayTitle)]
        assert_eq!(meta.books.len(), 2);
        assert_eq!(meta.books[0].id.0, "CNT_2001");
        assert_eq!(meta.books[0].number, Some(BookRange::single(1.0)));
        assert_eq!(meta.books[0].name.as_deref(), Some("Vol. 1"));

        // authors：Author 角色 → authorRoles（Writer）；Kotlin `distinct()` 按
        // id/role/name 去重，两本书贡献同一个 Contributor → 1 个作者
        assert_eq!(meta.metadata.authors.len(), 1);
        assert_eq!(meta.metadata.authors[0].name, "Author A");
        assert_eq!(meta.metadata.authors[0].role, AuthorRole::Writer);
    }

    #[test]
    fn to_book_metadata_matches_kotlin_mapper() {
        let mapper = test_mapper();
        let book = test_book();
        let meta = mapper.to_book_metadata(&book, None);

        assert_eq!(meta.metadata.title.as_deref(), Some("Volume 1"));
        assert_eq!(meta.metadata.summary.as_deref(), Some("Book desc"));
        assert_eq!(meta.metadata.number, Some(BookRange::single(1.0)));
        assert_eq!(meta.metadata.release_date.as_deref(), Some("2024-06-15"));
        assert_eq!(meta.metadata.authors.len(), 1);
        assert_eq!(meta.metadata.authors[0].name, "Author A");
        assert_eq!(meta.metadata.authors[0].role, AuthorRole::Writer);
        assert!(meta.metadata.tags.is_empty());
        assert_eq!(meta.metadata.isbn.as_deref(), Some("9780000000000"));
        assert_eq!(
            meta.metadata.links[0].url,
            "https://bookwalker.com/volume/2001"
        );
        assert!(meta.metadata.start_chapter.is_none());
        assert!(meta.metadata.end_chapter.is_none());
    }

    #[test]
    fn author_role_mapping_matches_kotlin_table() {
        use BookWalkerContributorRole::*;
        let c = |role: BookWalkerContributorRole| BookWalkerContributor {
            id: "x".to_string(),
            role,
            name: "N".to_string(),
        };
        let empty = vec![AuthorRole::Writer];
        let artists = vec![AuthorRole::Penciller];

        assert!(get_authors(&[c(Contributor)], &empty, &artists).is_empty());
        assert!(get_authors(&[c(Unknown4)], &empty, &artists).is_empty());
        assert!(get_authors(&[c(Narrator)], &empty, &artists).is_empty());
        assert_eq!(
            get_authors(&[c(Author)], &empty, &artists),
            vec![crate::model::Author {
                name: "N".to_string(),
                role: AuthorRole::Writer
            }]
        );
        assert_eq!(
            get_authors(&[c(Artist)], &empty, &artists),
            vec![crate::model::Author {
                name: "N".to_string(),
                role: AuthorRole::Penciller
            }]
        );
        assert_eq!(
            get_authors(&[c(Illustrator)], &empty, &artists),
            vec![crate::model::Author {
                name: "N".to_string(),
                role: AuthorRole::Penciller
            }]
        );
        assert_eq!(
            get_authors(&[c(Translator)], &empty, &artists),
            vec![crate::model::Author {
                name: "N".to_string(),
                role: AuthorRole::Translator
            }]
        );
        assert_eq!(
            get_authors(&[c(CoverDesign)], &empty, &artists),
            vec![crate::model::Author {
                name: "N".to_string(),
                role: AuthorRole::Cover
            }]
        );
        assert_eq!(
            get_authors(&[c(Script)], &empty, &artists),
            vec![crate::model::Author {
                name: "N".to_string(),
                role: AuthorRole::Writer
            }]
        );
        assert!(get_authors(&[c(With)], &empty, &artists).is_empty());
    }

    #[test]
    fn parse_instant_supports_common_formats() {
        assert!(parse_instant("2024-01-01T00:00:00Z").is_some());
        assert!(parse_instant("2024-01-01T00:00:00.000Z").is_some());
        assert!(parse_instant("2024-01-01 00:00:00").is_some());
        assert!(parse_instant("2024-01-01").is_some());
        assert!(parse_instant("").is_none());
        assert!(parse_instant("garbage").is_none());
    }

    /// 真实 SQLite 回环：建库（含 FTS5 索引）→ search / get_series / get_series_books / get_book。
    #[test]
    fn sqlite_repository_roundtrip() {
        let dir = std::env::temp_dir().join(format!("komf-bw-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("bkwk-db.sqlite");

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE series (id TEXT PRIMARY KEY, type INTEGER, title TEXT, alt_titles TEXT, \
             subtitle TEXT, display_title TEXT, display_title_short TEXT, description TEXT, \
             description_short TEXT, image_id TEXT, listed_at TEXT); \
             CREATE TABLE images (id TEXT PRIMARY KEY, name TEXT, mime TEXT, width INTEGER, height INTEGER); \
             CREATE TABLE tags (id TEXT PRIMARY KEY, name TEXT, slug TEXT, description TEXT, \
             namespace INTEGER, priority INTEGER); \
             CREATE TABLE series_tags (series_id TEXT, tag_id TEXT); \
             CREATE TABLE products (id TEXT PRIMARY KEY, content_id TEXT, series_id TEXT, \
             parent_content_id TEXT, level INTEGER, content_type INTEGER, product_type INTEGER, \
             title TEXT, alt_titles TEXT, subtitle TEXT, display_title TEXT, display_title_short TEXT, \
             description TEXT, description_short TEXT, image_id TEXT, display_order REAL, listed_at TEXT, \
             label_id TEXT, geoblock_id TEXT, display_name TEXT, copyright TEXT, on_presale_at TEXT, \
             on_sale_at TEXT, off_sale_at TEXT, add_on INTEGER, add_on_campaign_only INTEGER); \
             CREATE TABLE product_external_ids (id TEXT PRIMARY KEY, product_id TEXT, source TEXT, \
             type INTEGER, external_id TEXT, external_id_original TEXT); \
             CREATE TABLE product_contributors (product_id TEXT, contributor_id TEXT, role INTEGER, \
             name_override TEXT); \
             CREATE TABLE contributors (id TEXT PRIMARY KEY, name TEXT, name_alt TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO images VALUES ('IMG_000001', 'cover', 'image/webp', 600, 900)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO series VALUES ('CNT_1001', 1, 'One Punch Man', '[\"ワンパンマン\", \"One-Punch Man\"]', \
             '', 'One Punch Man', '', 'A hero story', '', 'IMG_000001', '2024-05-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tags VALUES ('tag1', 'Action', 'action', '', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO series_tags VALUES ('CNT_1001', 'tag1')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO products VALUES ('CNT_2001', 'CNT_2001', 'CNT_1001', NULL, 2, 0, 0, \
             'One Punch Man Vol 1', '[]', '', 'Vol. 1', '', 'Book desc', '', 'IMG_000001', 1.0, \
             '2024-06-15T00:00:00Z', 'label1', NULL, 'One Punch Man Vol 1', NULL, NULL, \
             '2024-06-15T00:00:00Z', NULL, 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO product_external_ids VALUES ('e1', 'CNT_2001', 'bookwalker', 3, '9780000000000', '9780000000000')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO contributors VALUES ('c1', 'Author A', '')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO product_contributors VALUES ('CNT_2001', 'c1', 1, NULL)",
            [],
        )
        .unwrap();
        // FTS5 索引（与下载器 createSearchIndex 相同）
        conn.execute_batch(
            "CREATE VIRTUAL TABLE series_fts USING fts5 (id, title, alt_titles, type, tokenize = 'trigram'); \
             INSERT INTO series_fts SELECT s.id, s.title, GROUP_CONCAT(json_each.value, ', '), s.type \
             FROM series s, json_each(alt_titles) GROUP BY s.id;",
        )
        .unwrap();
        drop(conn);

        let repo = BookWalkerSeriesRepository::new(&db_path);

        // search：精确短语匹配（Kotlin `"$title"` 带引号）
        let results = match repo.search("One Punch Man", &[BookWalkerContentType::Manga]) {
            Ok(v) => v,
            Err(e) => panic!("search error: {e:?}"),
        };
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "CNT_1001");
        assert_eq!(results[0].title, "One Punch Man");
        assert_eq!(results[0].alt_titles.len(), 2);
        assert_eq!(results[0].tags.len(), 1);
        assert_eq!(results[0].tags[0].name, "Action");
        let image = results[0].image.as_ref().unwrap();
        assert_eq!(image.id, "IMG_000001");
        assert_eq!(
            image.url600(),
            "https://img.sos-dan.net/600/IMG/_/0/00001.webp"
        );

        // type 过滤：NOVEL 分类不命中（真实库 manga 的 type 列存 1，Novel.number()=2）
        let no_hits = repo
            .search("One Punch Man", &[BookWalkerContentType::Novel])
            .unwrap();
        assert!(no_hits.is_empty());

        // get_series
        let series = repo.get_series("CNT_1001").unwrap();
        assert_eq!(series.description, "A hero story");
        let rd = release_date_from(series.listed_at).unwrap();
        assert_eq!((rd.year, rd.month, rd.day), (Some(2024), Some(5), Some(1)));

        // get_series_books
        let books = repo.get_series_books("CNT_1001").unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].display_title, "Vol. 1");
        assert_eq!(books[0].display_order, 1.0);
        assert_eq!(books[0].isbn.as_deref(), Some("9780000000000"));
        assert_eq!(books[0].contributors.len(), 1);
        assert_eq!(books[0].contributors[0].name, "Author A");
        assert_eq!(
            books[0].contributors[0].role,
            BookWalkerContributorRole::Author
        );

        // get_book
        let book = repo.get_book("CNT_2001").unwrap();
        assert_eq!(book.id, "CNT_2001");
        assert_eq!(book.content_id, "CNT_2001");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 下载器 createSearchIndex 后 repository 可查询（下载器→索引→查询 闭环）。
    #[test]
    fn downloader_index_then_search_roundtrip() {
        let dir = std::env::temp_dir().join(format!("komf-bw-idx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("bkwk-db.sqlite");

        // 构造一个未建索引的 SQLite，再走下载器的 create_search_index
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE series (id TEXT PRIMARY KEY, type INTEGER, title TEXT, alt_titles TEXT, \
             subtitle TEXT, display_title TEXT, display_title_short TEXT, description TEXT, \
             description_short TEXT, image_id TEXT, listed_at TEXT); \
             CREATE TABLE images (id TEXT PRIMARY KEY, name TEXT, mime TEXT, width INTEGER, height INTEGER); \
             CREATE TABLE tags (id TEXT PRIMARY KEY, name TEXT, slug TEXT, description TEXT, \
             namespace INTEGER, priority INTEGER); \
             CREATE TABLE series_tags (series_id TEXT, tag_id TEXT); \
             INSERT INTO series VALUES ('CNT_7', 3, 'Re:Zero', '[\"Re：ゼロから始める異世界生活\"]', \
             '', 'Re:Zero', '', '', '', NULL, NULL);",
        )
        .unwrap();
        drop(conn);

        let http = reqwest::Client::new();
        let downloader = BookWalkerDbDownloader::new(&dir, http);
        downloader.create_search_index().unwrap();

        let repo = BookWalkerSeriesRepository::new(&db_path);
        let hits = repo
            .search("Re:Zero", &[BookWalkerContentType::Webtoons])
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "CNT_7");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
