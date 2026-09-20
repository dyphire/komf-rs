//! 领域模型 —— 对应 `snd.komf.model` 包。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    pub name: String,
    pub role: AuthorRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuthorRole {
    Writer,
    Penciller,
    Inker,
    Colorist,
    Letterer,
    Cover,
    Editor,
    Translator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaType {
    Manga,
    Novel,
    Comic,
    Webtoon,
}

impl Default for MediaType {
    fn default() -> Self {
        Self::Manga
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UpdateMode {
    Api,
    ComicInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SeriesStatus {
    Ended,
    Ongoing,
    Abandoned,
    Hiatus,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReadingDirection {
    LeftToRight,
    RightToLeft,
    Vertical,
    Webtoon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TitleType {
    Romaji,
    Localized,
    Native,
}

impl TitleType {
    pub fn label(&self) -> &'static str {
        match self {
            TitleType::Romaji => "Romaji",
            TitleType::Localized => "Localized",
            TitleType::Native => "Native",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PublisherType {
    Original,
    Localized,
}

/// 对应 Kotlin value class `ProviderSeriesId`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderSeriesId(pub String);

impl std::fmt::Display for ProviderSeriesId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 对应 Kotlin value class `ProviderBookId`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderBookId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesTitle {
    pub name: String,
    pub r#type: Option<TitleType>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publisher {
    pub name: String,
    #[serde(default)]
    pub r#type: Option<PublisherType>,
    #[serde(default)]
    pub language_tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebLink {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseDate {
    pub year: Option<i32>,
    pub month: Option<u32>,
    pub day: Option<u32>,
}

impl ReleaseDate {
    pub fn new(year: Option<i32>, month: Option<u32>, day: Option<u32>) -> Self {
        Self { year, month, day }
    }
}

/// 对应 `Image.kt` —— 图片字节 + MIME 类型。
/// `bytes` 不参与序列化（Kotlin 中标注 `@Transient`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub bytes: Vec<u8>,
    pub mime_type: Option<String>,
}

impl Image {
    pub fn new(bytes: Vec<u8>, mime_type: Option<String>) -> Self {
        Self { bytes, mime_type }
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            mime_type: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookRange {
    pub start: f64,
    pub end: f64,
}

impl BookRange {
    pub fn new(start: f64, end: f64) -> Self {
        Self { start, end }
    }

    pub fn single(start: f64) -> Self {
        Self { start, end: start }
    }
}

impl std::fmt::Display for BookRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let start = if self.start.fract() == 0.0 {
            format!("{}", self.start as i64)
        } else {
            format!("{}", self.start)
        };
        let end = if self.end.fract() == 0.0 {
            format!("{}", self.end as i64)
        } else {
            format!("{}", self.end)
        };
        if start == end {
            write!(f, "{start}")
        } else {
            write!(f, "{start}-{end}")
        }
    }
}

/// 系列元数据 —— 对应 `SeriesMetadata.kt`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SeriesMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SeriesStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<SeriesTitle>,
    #[serde(default)]
    pub titles: Vec<SeriesTitle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<Publisher>,
    #[serde(default)]
    pub alternative_publishers: Vec<Publisher>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reading_direction: Option<ReadingDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_rating: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_book_count: Option<i32>,
    #[serde(default)]
    pub authors: Vec<Author>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<ReleaseDate>,
    #[serde(default)]
    pub links: Vec<WebLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<Image>,
}

impl SeriesMetadata {
    /// 提供一个简化的 title 便捷访问（Kotlin 中 `title.name` 需判空）。
    pub fn title_name(&self) -> Option<String> {
        self.title.as_ref().map(|t| t.name.clone())
    }
}

/// 系列书籍（provider 返回的书目条目）—— 对应 `SeriesBook.kt`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesBook {
    pub id: ProviderBookId,
    pub number: Option<BookRange>,
    pub name: Option<String>,
    pub r#type: Option<String>,
    pub edition: Option<String>,
}

/// 系列元数据 + 书籍列表（provider 返回）—— 对应 `ProviderSeriesMetadata.kt`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSeriesMetadata {
    pub id: ProviderSeriesId,
    pub metadata: SeriesMetadata,
    #[serde(default)]
    pub books: Vec<SeriesBook>,
}

/// 书籍元数据 —— 对应 `BookMetadata.kt`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BookMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<BookRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_sort: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(default)]
    pub authors: Vec<Author>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isbn: Option<String>,
    #[serde(default)]
    pub links: Vec<WebLink>,
    #[serde(default)]
    pub chapters: Vec<Chapter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub story_arcs: Option<Vec<BookStoryArc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_chapter: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_chapter: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<Image>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookStoryArc {
    pub name: String,
    pub number: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub name: Option<String>,
    pub number: i32,
}

/// 对应 `ProviderBookMetadata.kt`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderBookMetadata {
    pub id: Option<ProviderBookId>,
    pub series_id: Option<ProviderSeriesId>,
    pub metadata: BookMetadata,
}

/// 对应 `SeriesSearchResult.kt`（序列化字段名对齐 Kotlin camelCase：
/// url / imageUrl / title / provider / resultId）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesSearchResult {
    pub url: Option<String>,
    pub image_url: Option<String>,
    pub title: String,
    pub provider: String,
    pub result_id: String,
    /// 搜索结果显示用：provider 返回条目的媒体类型（Bangumi 按 platform 填充）。
    /// Rust 扩展：按库配置 libraryType 过滤显示结果。Kotlin 无此字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<MediaType>,
    /// 搜索结果显示用：provider 返回条目的内容语言（BCP 47，如 zh/ja/en）。
    /// Rust 扩展：脚本展示/过滤用（eHentai forced-language + 语言标签、MangaDex
    /// originalLanguage 填充；无可靠来源的 provider 为 None）。Kotlin 无此字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// 搜索结果显示用：是否成人内容（Bangumi nsfw 字段、eHentai 分类推导；
    /// 无此概念的 provider 为 None）。Rust 扩展：脚本区分/显示用。Kotlin 无此字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nsfw: Option<bool>,
}

/// 匹配查询 —— 对应 `MatchQuery.kt`。
#[derive(Debug, Clone)]
pub struct MatchQuery {
    pub series_name: String,
    pub start_year: Option<i32>,
    pub book_qualifier: Option<BookQualifier>,
    pub series_folder: Option<String>,
    /// 符号归一正则（searchTitleExtraction.symbolNormalizeRegex）：匹配比较时对
    /// query 与 provider 候选标题两侧应用（匹配部分替换为空格 + trim）。
    /// None = 不归一（默认，对齐 Kotlin）。
    pub normalization_regex: Option<String>,
    /// 库配置的 mediaType（metadataUpdate 库级 libraryType）——匹配时优先于 provider 配置。
    /// Rust 扩展：Bangumi 匹配按库类型过滤 platform。Kotlin 无此字段。
    pub media_type: Option<MediaType>,
    /// Rust 扩展：简繁转换（匹配归一时对 query 与候选双向应用；None = 不转换）。
    /// 作用于 `normalized_series_name` / `normalize_title` / `normalize_titles`。
    pub chinese: Option<std::sync::Arc<crate::util::ChineseConverter>>,
    /// Rust 扩展：oneshot 系列标志（唯一书籍）——eHentai 匹配时决定是否从书标题/文件名提取 gid。
    pub oneshot: bool,
    /// Rust 扩展：第一本书文件名——eHentai 匹配时作为 gid 提取候选。
    pub book_file_name: Option<String>,
}

impl MatchQuery {
    pub fn new(
        series_name: String,
        start_year: Option<i32>,
        book_qualifier: Option<BookQualifier>,
        series_folder: Option<String>,
    ) -> Self {
        Self {
            series_name,
            start_year,
            book_qualifier,
            series_folder,
            normalization_regex: None,
            media_type: None,
            chinese: None,
            oneshot: false,
            book_file_name: None,
        }
    }

    /// 设置匹配归一正则（None = 不归一）。
    pub fn with_normalization(mut self, normalization_regex: Option<String>) -> Self {
        self.normalization_regex = normalization_regex;
        self
    }

    /// 设置书籍上下文（Rust 扩展：eHentai gid 提取候选）。
    pub fn with_book_context(mut self, oneshot: bool, book_file_name: Option<String>) -> Self {
        self.oneshot = oneshot;
        self.book_file_name = book_file_name;
        self
    }

    /// 设置简繁转换器（None = 不转换）。
    pub fn with_chinese(
        mut self,
        chinese: Option<std::sync::Arc<crate::util::ChineseConverter>>,
    ) -> Self {
        self.chinese = chinese;
        self
    }

    /// 归一后的 query 名（用于匹配比较；含简繁转换）。
    pub fn normalized_series_name(&self) -> String {
        normalize_matching_title(
            &self.series_name,
            self.normalization_regex.as_deref(),
            self.chinese.as_ref().map(|c| c.as_ref()),
        )
    }

    /// 设置库配置的 mediaType（None = 不覆盖，使用 provider 配置）。
    pub fn with_media_type(mut self, media_type: Option<MediaType>) -> Self {
        self.media_type = media_type;
        self
    }

    /// 归一单个候选标题（用于匹配比较；含简繁转换）。
    pub fn normalize_title(&self, title: &str) -> String {
        normalize_matching_title(
            title,
            self.normalization_regex.as_deref(),
            self.chinese.as_ref().map(|c| c.as_ref()),
        )
    }

    /// 归一候选标题列表。
    pub fn normalize_titles(&self, titles: &[String]) -> Vec<String> {
        titles.iter().map(|t| self.normalize_title(t)).collect()
    }
}

/// 符号归一：正则匹配部分替换为空格 + trim；正则非法时仅 trim；无正则时原样返回。
/// 之后应用简繁转换（chinese 非空时）。
fn normalize_matching_title(
    s: &str,
    pattern: Option<&str>,
    chinese: Option<&crate::util::ChineseConverter>,
) -> String {
    let normalized = match pattern {
        Some(pattern) => match regex::Regex::new(pattern) {
            Ok(re) => re.replace_all(s, " ").trim().to_string(),
            Err(_) => s.trim().to_string(),
        },
        None => s.to_string(),
    };
    match chinese {
        Some(c) => c.convert(&normalized),
        None => normalized,
    }
}

#[derive(Debug, Clone)]
pub struct BookQualifier {
    pub name: String,
    pub number: BookRange,
    pub cover: Option<Image>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NameSimilarityMatcher;

    fn query(name: &str, regex: Option<&str>) -> MatchQuery {
        MatchQuery::new(name.to_string(), None, None, None)
            .with_normalization(regex.map(|r| r.to_string()))
    }

    #[test]
    fn no_regex_keeps_original() {
        let q = query("進撃の巨人：完全版", None);
        assert_eq!(q.normalized_series_name(), "進撃の巨人：完全版");
        assert_eq!(
            q.normalize_title("進撃の巨人：完全版"),
            "進撃の巨人：完全版"
        );
    }

    #[test]
    fn symbol_normalize_applied_to_both_sides() {
        let q = query("進撃の巨人：完全版", Some("[:：•·․,，。'’?？!！~⁓～]"));
        assert_eq!(q.normalized_series_name(), "進撃の巨人 完全版");
        assert_eq!(q.normalize_title("進撃の巨人：完全版"), "進撃の巨人 完全版");
        assert_eq!(
            q.normalize_titles(&["A：B".to_string(), "C，D".to_string()]),
            vec!["A B".to_string(), "C D".to_string()]
        );
    }

    #[test]
    fn invalid_regex_only_trims() {
        let q = query("  進撃  ", Some("[unclosed"));
        assert_eq!(q.normalized_series_name(), "進撃");
    }

    #[test]
    fn normalization_enables_exact_match() {
        let matcher = NameSimilarityMatcher::Exact;
        let raw = "進撃の巨人：完全版".to_string();
        let provider_title = "進撃の巨人 完全版".to_string();
        // 未归一：Exact 不命中（"：" vs " "）
        assert!(!matcher.matches(&raw, &[provider_title.clone()]));
        // 归一后：两侧一致，Exact 命中
        let q = query(&raw, Some("[:：]"));
        assert!(matcher.matches(
            &q.normalized_series_name(),
            &q.normalize_titles(&[provider_title])
        ));
    }
}
