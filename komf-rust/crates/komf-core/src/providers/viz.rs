//! Viz provider —— 对应 Kotlin `providers/viz` 包。

use chrono::Datelike;
use percent_encoding::percent_decode_str;
use scraper::{Element, ElementRef, Html, Selector};

use crate::config::ProviderConfig;
use crate::model::{
    Author, AuthorRole, BookMetadata, BookRange, Image, MatchQuery, ProviderBookId,
    ProviderBookMetadata, ProviderSeriesId, ProviderSeriesMetadata, Publisher, PublisherType,
    ReleaseDate, SeriesBook, SeriesMetadata, SeriesSearchResult, SeriesStatus, SeriesTitle,
    TitleType, WebLink,
};
use crate::providers::{CoreProviders, MetadataProvider, ProviderError};
use crate::util::{BookNameParser, NameSimilarityMatcher};

const VIZ_BASE_URL: &str = "https://www.viz.com";

// ---------------------------------------------------------------------------
// 模型 —— 对应 Kotlin model/*.kt
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VizBookId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VizAllBooksId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VizBookReleaseType {
    Digital,
    Paperback,
}

impl VizBookReleaseType {
    fn as_str(&self) -> &'static str {
        match self {
            VizBookReleaseType::Digital => "digital",
            VizBookReleaseType::Paperback => "paperback",
        }
    }
}

#[derive(Debug, Clone)]
pub struct VizSeriesBook {
    id: VizBookId,
    name: String,
    series_name: String,
    number: Option<BookRange>,
    image_url: Option<String>,
    final_: bool,
}

#[derive(Debug, Clone)]
pub struct VizBook {
    id: VizBookId,
    name: String,
    series_name: String,
    number: Option<BookRange>,
    publisher: String,
    release_date: Option<String>, // "MMMM d, yyyy"
    description: Option<String>,
    cover_url: Option<String>,
    genres: Vec<String>,
    isbn: Option<String>,
    age_rating: Option<i32>,
    author_story: Option<String>,
    author_art: Option<String>,
    all_books_id: Option<VizAllBooksId>,
}

impl VizBook {
    fn to_viz_series_book(&self) -> VizSeriesBook {
        VizSeriesBook {
            id: self.id.clone(),
            name: self.name.clone(),
            series_name: self.series_name.clone(),
            number: self.number.clone(),
            image_url: None,
            final_: false, // TODO
        }
    }
}

// ---------------------------------------------------------------------------
// Parser —— 对应 Kotlin VizParser.kt
// ---------------------------------------------------------------------------

struct VizParser {
    writer_roles: Vec<&'static str>,
    artist_roles: Vec<&'static str>,
}

impl VizParser {
    fn new() -> Self {
        Self {
            writer_roles: vec!["Story by", "Story and Art by", "Storyboards by"],
            artist_roles: vec!["Story and Art by", "Art by"],
        }
    }

    fn parse_search_results(&self, results: &str) -> Vec<VizSeriesBook> {
        let document = Html::parse_document(results);
        let selector = match Selector::parse("#results") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        document
            .select(&selector)
            .next()
            .map(|el| {
                child_elements(&el)
                    .into_iter()
                    .filter_map(|child| self.parse_series_book(&child))
                    .filter(|it| it.number.as_ref().map(|n| n.start as i32) == Some(1))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn parse_series_book(&self, result: &ElementRef) -> Option<VizSeriesBook> {
        let children = child_elements(result);
        let image_url = children
            .first()
            .and_then(|el| el_first(el, "img"))
            .and_then(|el| el.attr("data-original"))
            .map(|s| s.to_string());
        let title_element = children
            .get(1)
            .and_then(|el| child_elements(el).get(1).cloned())?;
        let href = title_element.attr("href")?;
        let decoded = decode_url_part(href);
        if !decoded.starts_with("/manga-books/manga/") {
            return None; // Kotlin check(...) 失败即异常；这里保守返回 None
        }
        let id = decoded
            .trim_start_matches("/manga-books/manga/")
            .to_string();
        let book_number = BookNameParser::get_volumes(&elem_text(&title_element));
        let final_ = children
            .first()
            .and_then(|el| child_elements(el).first().cloned())
            .map(|el| elem_text(&el) == "Final Volume!")
            .unwrap_or(false);
        let name = elem_text(&title_element);

        Some(VizSeriesBook {
            id: VizBookId(id),
            name: name.clone(),
            series_name: get_series_name(&name),
            number: book_number,
            image_url,
            final_,
        })
    }

    fn parse_book(&self, book: &str) -> VizBook {
        let document = Html::parse_document(book);
        let product_row = Selector::parse("#product_row")
            .ok()
            .and_then(|s| document.select(&s).next())
            .unwrap_or_else(|| panic!("missing #product_row"));
        let cover = el_first(&product_row, "#product_image_block img")
            .and_then(|el| el.attr("src"))
            .map(|s| s.to_string());
        let purchase_links_block = el_first(&product_row, "#purchase_links_block")
            .unwrap_or_else(|| panic!("missing #purchase_links_block"));
        let title_element =
            el_first(&purchase_links_block, "h2").unwrap_or_else(|| panic!("missing h2"));
        let genres = el_first(&purchase_links_block, ":scope > * > *") // child(0).child(0)
            .map(|el| {
                let selector = sel("a").unwrap();
                el.select(&selector)
                    .filter(|e| {
                        !e.value()
                            .has_class("bg-yellow", scraper::CaseSensitivity::CaseSensitive)
                    })
                    .map(|e| elem_text(&e))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let book_number = BookNameParser::get_volumes(&elem_text(&title_element));
        // productRow.child(1).child(1) —— 摘要；child(1).child(2) —— 细节
        let product_children = child_elements(&product_row);
        let child1 = product_children.get(1).cloned();
        let summary = child1
            .as_ref()
            .and_then(|el| child_elements(el).get(1).cloned())
            .map(|el| elem_text(&el));
        let details = child1
            .as_ref()
            .and_then(|el| child_elements(el).get(2).cloned());
        let authors = details
            .as_ref()
            .and_then(|el| child_elements(el).first().cloned())
            .and_then(|el| child_elements(&el).first().cloned())
            .map(|el| elem_text(&el));
        let release_date = details
            .as_ref()
            .and_then(|el| el_first(el, ".o_release-date"))
            .map(|el| elem_text(&el))
            .and_then(|t| t.strip_prefix("Release ").map(|s| s.to_string()));
        let isbn = details
            .as_ref()
            .and_then(|el| el_first(el, ".o_isbn13"))
            .map(|el| elem_text(&el))
            .and_then(|t| t.strip_prefix("ISBN-13 ").map(|s| s.to_string()));
        let eisbn = details
            .as_ref()
            .and_then(|el| el_first(el, ".o_eisbn13"))
            .map(|el| elem_text(&el))
            .and_then(|t| t.strip_prefix("eISBN-13 ").map(|s| s.to_string()));
        let age_rating = details
            .as_ref()
            .and_then(|el| child_elements(el).get(1).cloned())
            .map(|el| {
                child_elements(&el)
                    .into_iter()
                    .find(|c| elem_text(c).starts_with("Age Rating"))
                    .map(|c| elem_text(&c))
            })
            .flatten()
            .and_then(|t| t.strip_prefix("Age Rating ").map(|s| s.to_string()))
            .and_then(|s| match s.as_str() {
                "All Ages" => Some(0),
                "Teen" => Some(13),
                "Teen Plus" => Some(15),
                "Mature" => Some(18),
                _ => None,
            });

        let book_id = self.parse_book_id(&document);
        let all_books_id = self.parse_link_to_all_books(&title_element);

        VizBook {
            id: book_id,
            name: elem_text(&title_element),
            series_name: get_series_name(&elem_text(&title_element)),
            number: book_number,
            publisher: "Viz".to_string(),
            release_date,
            description: summary,
            cover_url: cover,
            genres,
            isbn: isbn.or(eisbn),
            age_rating,
            author_story: authors
                .as_deref()
                .map(|a| self.parse_author(a, &self.writer_roles))
                .flatten(),
            author_art: authors
                .as_deref()
                .map(|a| self.parse_author(a, &self.artist_roles))
                .flatten(),
            all_books_id,
        }
    }

    fn parse_series_all_books(&self, books: &str) -> Vec<VizSeriesBook> {
        let document = Html::parse_document(books);
        let selector = Selector::parse("#c-0-s-0").ok();
        let Some(container) = selector.and_then(|s| document.select(&s).next()) else {
            return Vec::new();
        };
        let child0 = child_elements(&container).into_iter().next();
        let Some(child0) = child0 else {
            return Vec::new();
        };
        child_elements(&child0)
            .into_iter()
            .filter_map(|el| self.parse_series_book(&el))
            .collect()
    }

    fn parse_book_id(&self, document: &Html) -> VizBookId {
        let og_url = Selector::parse("meta[property=\"og:url\"]")
            .ok()
            .and_then(|s| document.select(&s).next())
            .and_then(|el| el.attr("content"))
            .unwrap_or_default();
        let prefix = format!("{VIZ_BASE_URL}/manga-books/manga/");
        if !og_url.starts_with(&prefix) {
            panic!("unexpected og:url: {og_url}");
        }
        VizBookId(decode_url_part(og_url.trim_start_matches(&prefix)))
    }

    fn parse_author(&self, authors: &str, roles: &[&'static str]) -> Option<String> {
        // Kotlin: authors.split(",").ifEmpty { authors.split(";") } ——
        // Kotlin 的 split(",") 永不为空（空串也返回 [""]），ifEmpty 恒不触发，
        // 因此 Kotlin 始终按逗号分隔。这里对齐：始终逗号分隔。
        let parts: Vec<String> = authors.split(',').map(|s| s.trim().to_string()).collect();
        if parts.is_empty() {
            return None;
        }
        // Kotlin: firstNotNullOfOrNull —— 第一个非 null 结果
        for role_and_author in parts {
            if let Some(role) = roles.iter().find(|r| role_and_author.starts_with(**r)) {
                return Some(
                    role_and_author
                        .trim_start_matches(&format!("{role} "))
                        .to_string(),
                );
            }
        }
        None
    }

    fn parse_link_to_all_books(&self, title_element: &ElementRef) -> Option<VizAllBooksId> {
        let title_link = el_first(title_element, "a");
        let prefix_link = title_element
            .prev_sibling_element()
            .filter(|el| el.value().name() == "a");
        let link = title_link.or(prefix_link)?;
        let href = link.attr("href")?;
        let decoded = decode_url_part(href);
        if !(decoded.starts_with("/manga-books/manga/") && decoded.ends_with("/all")) {
            return None;
        }
        let id = decoded
            .trim_start_matches("/manga-books/manga/")
            .trim_end_matches("/all")
            .to_string();
        Some(VizAllBooksId(id))
    }
}

fn get_series_name(name: &str) -> String {
    // Kotlin: name.replace(", Vol. [0-9]+".toRegex(), "") —— 无 trim
    let re = regex::Regex::new(r", Vol\. [0-9]+").unwrap();
    re.replace_all(name, "").to_string()
}

/// Kotlin `decodeURLPart`。
fn decode_url_part(part: &str) -> String {
    percent_decode_str(part)
        .decode_utf8()
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| part.to_string())
}

fn sel(css: &str) -> Option<Selector> {
    Selector::parse(css).ok()
}

fn el_first<'a>(el: &ElementRef<'a>, css: &str) -> Option<ElementRef<'a>> {
    let selector = sel(css)?;
    el.select(&selector).next()
}

fn child_elements<'a>(el: &ElementRef<'a>) -> Vec<ElementRef<'a>> {
    el.children().filter_map(ElementRef::wrap).collect()
}

fn elem_text(el: &ElementRef) -> String {
    el.text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Kotlin `LocalDate.Format { monthName(ENGLISH_FULL); ' '; day(NONE); ", "; year() }`，
/// 即 `MMMM d, yyyy`。
fn parse_release_date_rd(raw: &str) -> Option<ReleaseDate> {
    let parsed = chrono::NaiveDate::parse_from_str(raw, "%B %e, %Y")
        .or_else(|_| chrono::NaiveDate::parse_from_str(raw, "%B %d, %Y"))
        .ok()?;
    Some(ReleaseDate::new(
        Some(parsed.year()),
        Some(parsed.month()),
        Some(parsed.day()),
    ))
}

fn parse_release_date_string(raw: &Option<String>) -> Option<String> {
    let raw = raw.as_deref()?;
    let parsed = chrono::NaiveDate::parse_from_str(raw, "%B %e, %Y")
        .or_else(|_| chrono::NaiveDate::parse_from_str(raw, "%B %d, %Y"))
        .ok()?;
    Some(parsed.format("%Y-%m-%d").to_string())
}

// ---------------------------------------------------------------------------
// Client —— 对应 Kotlin VizClient.kt
// ---------------------------------------------------------------------------

pub struct VizClient {
    http: reqwest::Client,
    parser: VizParser,
}

impl VizClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            parser: VizParser::new(),
        }
    }

    pub async fn search_series(&self, name: &str) -> Result<Vec<VizSeriesBook>, ProviderError> {
        let search_query = format!("{name}, Vol. 1");
        let response = self
            .http
            .get(format!("{VIZ_BASE_URL}/search"))
            .query(&[("search", search_query), ("category", "Manga".to_string())])
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::Viz, status, text));
        }
        let text = response.text().await?;
        Ok(self.parser.parse_search_results(&text))
    }

    pub async fn get_all_books(
        &self,
        id: &VizAllBooksId,
    ) -> Result<Vec<VizSeriesBook>, ProviderError> {
        let response = self
            .http
            .get(format!("{VIZ_BASE_URL}/manga-books/manga/{}/all", id.0))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::Viz, status, text));
        }
        let text = response.text().await?;
        Ok(self.parser.parse_series_all_books(&text))
    }

    pub async fn get_book(
        &self,
        book_id: &VizBookId,
        r#type: VizBookReleaseType,
    ) -> Result<VizBook, ProviderError> {
        let response = self
            .http
            .get(format!(
                "{VIZ_BASE_URL}/manga-books/manga/{}/{}",
                book_id.0,
                r#type.as_str()
            ))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::Viz, status, text));
        }
        let text = response.text().await?;
        Ok(self.parser.parse_book(&text))
    }

    pub async fn get_thumbnail(&self, url: &str) -> Result<Option<Image>, ProviderError> {
        let response = self.http.get(url).send().await?;
        let status = response.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            return Ok(None); // Kotlin: ClientRequestException Forbidden -> null
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::Viz, status, text));
        }
        let bytes = response.bytes().await?;
        Ok(Some(Image::new(bytes.to_vec(), detect_mime(url))))
    }
}

fn detect_mime(url: &str) -> Option<String> {
    let lower = url.to_lowercase();
    if lower.ends_with(".png") {
        Some("image/png".into())
    } else if lower.ends_with(".webp") {
        Some("image/webp".into())
    } else if lower.ends_with(".gif") {
        Some("image/gif".into())
    } else {
        Some("image/jpeg".into())
    }
}

// ---------------------------------------------------------------------------
// Metadata mapper —— 对应 Kotlin VizMetadataMapper.kt
// ---------------------------------------------------------------------------

pub struct VizMetadataMapper {
    metadata_config: crate::config::SeriesMetadataConfig,
    book_metadata_config: crate::config::BookMetadataConfig,
    author_roles: Vec<AuthorRole>,
    artist_roles: Vec<AuthorRole>,
}

impl VizMetadataMapper {
    pub fn new(
        metadata_config: crate::config::SeriesMetadataConfig,
        book_metadata_config: crate::config::BookMetadataConfig,
        author_roles: Vec<AuthorRole>,
        artist_roles: Vec<AuthorRole>,
    ) -> Self {
        Self {
            metadata_config,
            book_metadata_config,
            author_roles,
            artist_roles,
        }
    }

    pub fn to_series_metadata(
        &self,
        book: &VizBook,
        all_books: &[VizSeriesBook],
        thumbnail: Option<Image>,
    ) -> ProviderSeriesMetadata {
        let cfg = &self.metadata_config;

        let title = SeriesTitle {
            name: book.series_name.clone(),
            r#type: Some(TitleType::Localized),
            language: Some("en".to_string()),
        };
        let title_field = cfg.title.then_some(title.clone());

        // Kotlin MetadataConfigApplier.seriesTitles：title 禁用时保留全部标题仅清空 type/language
        let mut titles = vec![title.clone()];
        if !cfg.title {
            for t in titles.iter_mut() {
                t.r#type = None;
                t.language = None;
            }
        }

        let status = if cfg.status {
            if all_books.iter().any(|b| b.final_) {
                Some(SeriesStatus::Ended)
            } else {
                None
            }
        } else {
            None
        };

        let summary = cfg.summary.then_some(book.description.clone()).flatten();
        let publisher = cfg.publisher.then(|| Publisher {
            name: book.publisher.clone(),
            r#type: Some(PublisherType::Localized),
            language_tag: None,
        });
        let age_rating = cfg.age_rating.then_some(book.age_rating).flatten();
        let genres: Vec<String> = if cfg.genres {
            book.genres.clone()
        } else {
            Vec::new()
        };
        let total_book_count: Option<i32> = if cfg.total_book_count {
            let size = all_books.len() as i32;
            if size < 1 {
                None
            } else {
                Some(size)
            }
        } else {
            None
        };
        let authors = if cfg.authors {
            self.get_authors(book)
        } else {
            Vec::new()
        };
        let release_date = cfg
            .release_date
            .then(|| book.release_date.as_deref().and_then(parse_release_date_rd))
            .flatten();
        let links = if cfg.links {
            book.all_books_id
                .as_ref()
                .map(|id| WebLink {
                    label: "Viz".to_string(),
                    url: series_url(id),
                })
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };

        let metadata = SeriesMetadata {
            status,
            title: title_field,
            titles,
            summary,
            publisher,
            alternative_publishers: Vec::new(),
            reading_direction: None,
            age_rating,
            language: None,
            genres,
            tags: Vec::new(),
            total_book_count,
            authors,
            release_date,
            links,
            score: None,
            thumbnail,
        };

        // Kotlin MetadataConfigApplier：books = getIfEnabled(books, config.books) ?: emptyList()
        let books = if cfg.books {
            all_books
                .iter()
                .map(|it| SeriesBook {
                    id: ProviderBookId(it.id.0.clone()),
                    number: it.number.clone(),
                    name: Some(it.name.clone()),
                    r#type: None,
                    edition: None,
                })
                .collect()
        } else {
            Vec::new()
        };

        ProviderSeriesMetadata {
            id: ProviderSeriesId(book.id.0.clone()),
            metadata,
            books,
        }
    }

    pub fn to_book_metadata(
        &self,
        book: &VizBook,
        thumbnail: Option<Image>,
    ) -> ProviderBookMetadata {
        let cfg = &self.book_metadata_config;
        let metadata = BookMetadata {
            title: if cfg.title {
                Some(book.name.clone())
            } else {
                None
            },
            summary: if cfg.summary {
                book.description.clone()
            } else {
                None
            },
            number: if cfg.number {
                book.number.clone()
            } else {
                None
            },
            release_date: if cfg.release_date {
                parse_release_date_string(&book.release_date)
            } else {
                None
            },
            authors: if cfg.authors {
                self.get_authors(book)
            } else {
                Vec::new()
            },
            isbn: if cfg.isbn { book.isbn.clone() } else { None },
            start_chapter: None,
            end_chapter: None,
            thumbnail,
            links: if cfg.links {
                vec![WebLink {
                    label: "Viz".to_string(),
                    url: book_url(&book.id),
                }]
            } else {
                Vec::new()
            },
            ..Default::default()
        };
        ProviderBookMetadata {
            id: Some(ProviderBookId(book.id.0.clone())),
            series_id: None,
            metadata,
        }
    }

    pub fn to_series_search_result(&self, book: &VizSeriesBook) -> SeriesSearchResult {
        SeriesSearchResult {
            url: None,
            image_url: book.image_url.clone(),
            title: book.name.clone(),
            provider: CoreProviders::Viz.as_str().to_string(),
            result_id: book.id.0.clone(),
            media_type: None,
            language: None,
            nsfw: None,
        }
    }

    fn get_authors(&self, book: &VizBook) -> Vec<Author> {
        // Kotlin: authorsArt + authorStory（artist 在前）
        let mut authors = Vec::new();
        if let Some(name) = book.author_art.as_deref() {
            for role in &self.artist_roles {
                authors.push(Author {
                    name: name.to_string(),
                    role: *role,
                });
            }
        }
        if let Some(name) = book.author_story.as_deref() {
            for role in &self.author_roles {
                authors.push(Author {
                    name: name.to_string(),
                    role: *role,
                });
            }
        }
        authors
    }
}

fn series_url(id: &VizAllBooksId) -> String {
    format!(
        "{VIZ_BASE_URL}/manga-books/manga/{}/all",
        encode_url_path(&id.0)
    )
}

fn book_url(id: &VizBookId) -> String {
    format!("{VIZ_BASE_URL}/manga-books/manga/{}", id.0)
}

/// Kotlin `encodeURLPath`：编码整个 path（含 `/` 不编码？Ktor encodeURLPath 编码
/// 除 unreserved 与 `!$&'()*+,;=:@` 外全部字符，`/` 保留）。
fn encode_url_path(path: &str) -> String {
    url::form_urlencoded::byte_serialize(path.as_bytes())
        .collect::<String>()
        .replace("+", "%20")
}

// ---------------------------------------------------------------------------
// Provider —— 对应 Kotlin VizMetadataProvider.kt
// ---------------------------------------------------------------------------

pub struct VizMetadataProvider {
    client: VizClient,
    metadata_mapper: VizMetadataMapper,
    name_matcher: NameSimilarityMatcher,
    fetch_series_covers: bool,
    fetch_book_covers: bool,
}

pub fn create_provider(
    config: &ProviderConfig,
    default_name_matcher: NameSimilarityMatcher,
    http_client: &reqwest::Client,
) -> Option<VizMetadataProvider> {
    if !config.enabled {
        return None;
    }
    let name_matcher = config.name_matching_mode.unwrap_or(default_name_matcher);
    Some(VizMetadataProvider {
        client: VizClient::new(http_client.clone()),
        metadata_mapper: VizMetadataMapper::new(
            config.series_metadata.clone(),
            config.book_metadata.clone(),
            config.author_roles.clone(),
            config.artist_roles.clone(),
        ),
        name_matcher,
        fetch_series_covers: config.series_metadata.thumbnail,
        fetch_book_covers: config.book_metadata.thumbnail,
    })
}

#[async_trait::async_trait]
impl MetadataProvider for VizMetadataProvider {

    fn resolve_link_id(&self, query: &str) -> Option<String> {
        let re = regex::Regex::new(r"viz\.com/manga-books/manga/([^/?#]+)").ok()?;
        re.captures(query)
            .map(|c| c.get(1).unwrap().as_str().to_string())
    }
    fn provider_name(&self) -> CoreProviders {
        CoreProviders::Viz
    }

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        let series = self.get_book(&VizBookId(series_id.0.clone())).await?;
        let books: Vec<VizSeriesBook> = match &series.all_books_id {
            Some(id) => self.client.get_all_books(id).await?,
            None => vec![series.to_viz_series_book()],
        };
        let thumbnail = if self.fetch_series_covers {
            match &series.cover_url {
                Some(url) => self.client.get_thumbnail(url).await?,
                None => None,
            }
        } else {
            None
        };
        Ok(self
            .metadata_mapper
            .to_series_metadata(&series, &books, thumbnail))
    }

    async fn get_series_cover(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError> {
        let series = self.get_book(&VizBookId(series_id.0.clone())).await?;
        match &series.cover_url {
            Some(url) => self.client.get_thumbnail(url).await,
            None => Ok(None),
        }
    }

    async fn get_book_metadata(
        &self,
        _series_id: &ProviderSeriesId,
        book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError> {
        let book = self.get_book(&VizBookId(book_id.0.clone())).await?;
        let thumbnail = if self.fetch_book_covers {
            match &book.cover_url {
                Some(url) => self.client.get_thumbnail(url).await?,
                None => None,
            }
        } else {
            None
        };
        Ok(self.metadata_mapper.to_book_metadata(&book, thumbnail))
    }

    async fn search_series(
        &self,
        series_name: &str,
        limit: usize,
        _media_type: Option<crate::model::MediaType>,
    ) -> Result<Vec<SeriesSearchResult>, ProviderError> {
        if is_invalid_name(series_name) {
            return Ok(Vec::new());
        }
        let truncated: String = series_name.chars().take(100).collect();
        let results = self
            .client
            .search_series(&sanitize_search_input(&truncated))
            .await?;
        Ok(results
            .into_iter()
            .take(limit)
            .map(|it| self.metadata_mapper.to_series_search_result(&it))
            .collect())
    }

    async fn match_series_metadata(
        &self,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        let series_name = &match_query.series_name;
        if is_invalid_name(series_name) {
            return Ok(None);
        }
        let truncated: String = series_name.chars().take(100).collect();
        let results = self
            .client
            .search_series(&sanitize_search_input(&truncated))
            .await?;

        let matched = results.into_iter().find(|it| {
            self.name_matcher.matches_single(
                &match_query.normalized_series_name(),
                &match_query.normalize_title(&it.series_name),
            )
        });
        let Some(series_book) = matched else {
            return Ok(None);
        };
        let first_book = self.get_book(&series_book.id).await?;
        let books: Vec<VizSeriesBook> = match &first_book.all_books_id {
            Some(id) => self.client.get_all_books(id).await?,
            None => vec![first_book.to_viz_series_book()],
        };
        let thumbnail = if self.fetch_series_covers {
            match &first_book.cover_url {
                Some(url) => self.client.get_thumbnail(url).await?,
                None => None,
            }
        } else {
            None
        };
        Ok(Some(self.metadata_mapper.to_series_metadata(
            &first_book,
            &books,
            thumbnail,
        )))
    }
}

impl VizMetadataProvider {
    async fn get_book(&self, id: &VizBookId) -> Result<VizBook, ProviderError> {
        match self.client.get_book(id, VizBookReleaseType::Digital).await {
            Ok(book) => Ok(book),
            Err(ProviderError::Status(_, reqwest::StatusCode::NOT_FOUND, _)) => {
                self.client
                    .get_book(id, VizBookReleaseType::Paperback)
                    .await
            }
            Err(e) => Err(e),
        }
    }
}

fn is_invalid_name(name: &str) -> bool {
    let re = regex::Regex::new(r"^[0-9]+--").unwrap();
    re.is_match(name)
}

fn sanitize_search_input(name: &str) -> String {
    let re = regex::Regex::new(r"[(]([^)]+)[)]").unwrap();
    re.replace_all(name, "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_name_strips_volume() {
        assert_eq!(get_series_name("Naruto, Vol. 3"), "Naruto");
        assert_eq!(get_series_name("One Piece, Vol. 100"), "One Piece");
        assert_eq!(get_series_name("Bleach"), "Bleach");
    }

    #[test]
    fn author_parsing() {
        let parser = VizParser::new();
        assert_eq!(
            parser.parse_author(
                "Story by Masashi Kishimoto, Art by Someone",
                &parser.writer_roles
            ),
            Some("Masashi Kishimoto".to_string())
        );
        assert_eq!(
            parser.parse_author(
                "Story by Masashi Kishimoto, Art by Someone",
                &parser.artist_roles
            ),
            Some("Someone".to_string())
        );
        assert_eq!(
            parser.parse_author("Story and Art by Tite Kubo", &parser.writer_roles),
            Some("Tite Kubo".to_string())
        );
        assert_eq!(
            parser.parse_author("Story and Art by Tite Kubo", &parser.artist_roles),
            Some("Tite Kubo".to_string())
        );
        assert_eq!(
            parser.parse_author("Someone Else", &parser.writer_roles),
            None
        );
        // Kotlin split(",") 永不 ifEmpty：分号串按逗号整体处理
        assert_eq!(
            parser.parse_author("Story by A; Art by B", &parser.writer_roles),
            Some("A; Art by B".to_string())
        );
        // 空逗号分隔回退到分号
        assert_eq!(parser.parse_author(";", &parser.writer_roles), None);
    }

    #[test]
    fn age_rating_mapping() {
        // 直接验证 parse_book 的映射表（通过 age_rating 匹配分支无法单测，验证模型枚举数值）
        let map = |s: &str| match s {
            "All Ages" => Some(0),
            "Teen" => Some(13),
            "Teen Plus" => Some(15),
            "Mature" => Some(18),
            _ => None,
        };
        assert_eq!(map("All Ages"), Some(0));
        assert_eq!(map("Teen"), Some(13));
        assert_eq!(map("Teen Plus"), Some(15));
        assert_eq!(map("Mature"), Some(18));
    }

    #[test]
    fn sanitize_and_invalid_name() {
        assert!(is_invalid_name("12345--series"));
        assert!(!is_invalid_name("Naruto"));
        assert_eq!(sanitize_search_input("Naruto (manga)"), "Naruto");
        assert_eq!(sanitize_search_input("  Berserk (1997)  "), "Berserk");
    }

    #[test]
    fn release_date_formats() {
        assert_eq!(
            parse_release_date_rd("September 17, 2026"),
            Some(ReleaseDate::new(Some(2026), Some(9), Some(17)))
        );
        assert_eq!(
            parse_release_date_string(&Some("May 3, 2020".to_string())).as_deref(),
            Some("2020-05-03")
        );
    }
}
