//! YenPress provider —— 对应 Kotlin `providers/yenpress` 包。

use chrono::Datelike;
use scraper::{ElementRef, Html, Selector};
use serde::Deserialize;

use crate::config::ProviderConfig;
use crate::model::{
    Author, AuthorRole, BookMetadata, BookRange, Image, MatchQuery, MediaType, ProviderBookId,
    ProviderBookMetadata, ProviderSeriesId, ProviderSeriesMetadata, Publisher, PublisherType,
    ReleaseDate, SeriesBook, SeriesMetadata, SeriesSearchResult, SeriesTitle, TitleType, WebLink,
};
use crate::providers::{CoreProviders, MetadataProvider, ProviderError};
use crate::util::{BookNameParser, NameSimilarityMatcher};

const YEN_PRESS_BASE_URL: &str = "https://yenpress.com/";
const YEN_PRESS_SEARCH_URL: &str = "https://enterprise-search.yenpress.com/";

// ---------------------------------------------------------------------------
// 模型 —— 对应 Kotlin model/YenPressBook.kt、YenPressSearchResult.kt
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct YenPressSearchResponse {
    results: Vec<YenPressSearchResult>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct YenPressSearchResult {
    title: YenPressSearchField,
    url: YenPressSearchField,
    image: Option<YenPressSearchField>,
}

impl YenPressSearchResult {
    fn id(&self) -> YenPressSeriesId {
        YenPressSeriesId(self.url.raw.trim_start_matches("/series/").to_string())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct YenPressSearchField {
    raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct YenPressSeriesId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct YenPressBookId(String);

#[derive(Debug, Clone)]
struct YenPressAuthor {
    role: String,
    name: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct YenPressBook {
    id: YenPressBookId,
    name: String,
    number: Option<BookRange>,
    series_id: YenPressSeriesId,
    authors: Vec<YenPressAuthor>,
    description: Option<String>,
    genres: Vec<String>,
    series_name: Option<String>,
    page_count: Option<i32>,
    release_date: Option<String>, // "MMM d, yyyy"
    isbn: Option<String>,
    age_rating: Option<String>,
    imprint: Option<String>,
    image_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct YenPressBookShort {
    id: YenPressBookId,
    number: Option<BookRange>,
    name: Option<String>,
}

// ---------------------------------------------------------------------------
// Parser —— 对应 Kotlin YenPressParser.kt（Ksoup → scraper）
// ---------------------------------------------------------------------------

struct YenPressParser;

/// 解析 CSS 选择器（失败返回 None）。
fn sel(css: &str) -> Option<Selector> {
    Selector::parse(css).ok()
}

fn doc_first<'a>(document: &'a Html, css: &str) -> Option<ElementRef<'a>> {
    let selector = sel(css)?;
    document.select(&selector).next()
}

fn el_first<'a>(el: &ElementRef<'a>, css: &str) -> Option<ElementRef<'a>> {
    let selector = sel(css)?;
    el.select(&selector).next()
}

impl YenPressParser {
    fn parse_book(html: &str, book_id: &YenPressBookId) -> YenPressBook {
        let document = Html::parse_document(html);

        let heading_content = doc_first(&document, ".heading-content")
            .unwrap_or_else(|| panic!("missing .heading-content"));
        let title = el_first(&heading_content, ".heading")
            .map(|el| elem_text(&el))
            .unwrap_or_default();
        let authors = el_first(&heading_content, ".story-details")
            .map(|el| {
                child_elements(&el)
                    .into_iter()
                    .filter_map(|child| {
                        let text = elem_text(&child);
                        let re = regex::Regex::new(r"(?<role>.*):\s(?<name>.*)").unwrap();
                        let caps = re.captures(&text)?;
                        let role = caps
                            .name("role")?
                            .as_str()
                            .trim_end_matches(':')
                            .to_string();
                        let name = caps.name("name")?.as_str().to_string();
                        Some(YenPressAuthor { role, name })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let book_info = doc_first(&document, ".book-info");
        let description = book_info
            .as_ref()
            .and_then(|el| el_first(el, ".content-heading-txt"))
            .and_then(|el| child_elements(&el).into_iter().nth(1))
            .map(|el| elem_text(&el));
        let cover = book_info
            .as_ref()
            .and_then(|el| el_first(el, ".series-cover img"))
            .and_then(|el| el.attr("data-src"))
            .map(|s| s.to_string());

        let book_details_element = doc_first(&document, ".book-details")
            .map(|el| child_elements(&el).last().cloned())
            .flatten();

        let genres = book_details_element
            .as_ref()
            .and_then(|el| el_first(el, ".txt-hold"))
            .map(|el| {
                child_elements(&el)
                    .last()
                    .map(|last| {
                        child_elements(&last)
                            .into_iter()
                            .map(|c| elem_text(&c))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        let book_details: std::collections::HashMap<String, String> = book_details_element
            .as_ref()
            .and_then(|el| el_first(el, ".detail-info"))
            .map(|el| {
                let selector = sel("div").unwrap();
                el.select(&selector)
                    .filter(|div| {
                        !child_elements(div)
                            .iter()
                            .any(|c| c.value().name() == "div")
                    })
                    .filter_map(|div| {
                        let children = child_elements(&div);
                        let mut children = children.into_iter();
                        let key = children.next().map(|c| elem_text(&c))?;
                        let value = children.next().map(|c| elem_text(&c))?;
                        Some((key, value))
                    })
                    .collect::<std::collections::HashMap<_, _>>()
            })
            .unwrap_or_default();

        let page_count = book_details
            .get("Page Count")
            .and_then(|v| v.strip_suffix(" pages"))
            .and_then(|v| v.trim().parse::<i32>().ok());

        let release_date = book_details.get("Release Date").cloned();

        let series_id = book_details_element
            .as_ref()
            .and_then(|el| el_first(el, ".social-share"))
            .and_then(|el| el_first(&el, ".center-btn-page"))
            .and_then(|el| el_first(&el, ".main-btn.black"))
            .and_then(|el| el.attr("href"))
            .map(|href| {
                YenPressSeriesId(
                    href.trim_start_matches("/series/")
                        .trim_end_matches("?format=Digital")
                        .to_string(),
                )
            })
            .unwrap_or_else(|| YenPressSeriesId(String::new()));

        YenPressBook {
            id: book_id.clone(),
            name: title.clone(),
            number: BookNameParser::get_volumes(&title)
                .or_else(|| BookNameParser::get_book_number(&title)),
            series_id,
            authors,
            description,
            genres,
            series_name: book_details.get("Series").cloned(),
            page_count,
            release_date,
            isbn: book_details.get("ISBN").cloned(),
            age_rating: book_details.get("Age Rating").cloned(),
            imprint: book_details.get("Imprint").cloned(),
            image_url: cover,
        }
    }

    fn parse_more_books_response(html: &str) -> (Vec<YenPressBookShort>, Option<i32>) {
        let document = Html::parse_document(html);
        let books = sel(".inline_block")
            .map(|selector| {
                document
                    .select(&selector)
                    .filter_map(|it| {
                        let children = child_elements(&it);
                        let mut children = children.into_iter();
                        let link = children.next()?;
                        let href = link.attr("href")?;
                        let book_id =
                            YenPressBookId(href.trim_start_matches("/titles/").to_string());
                        // Kotlin: val name = link.child(1).text()
                        let name = child_elements(&link).get(1).map(|el| elem_text(el));
                        let name_text = name.as_deref().unwrap_or("");
                        Some(YenPressBookShort {
                            id: book_id,
                            number: BookNameParser::get_volumes(name_text)
                                .or_else(|| BookNameParser::get_book_number(name_text)),
                            name,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let next_ord = doc_first(&document, ".show-more")
            .and_then(|el| el.attr("data-url"))
            .and_then(|url| {
                regex::Regex::new(r"&next_ord=(?<nextOrd>\d+)$")
                    .unwrap()
                    .captures(url)
                    .and_then(|c| c.name("nextOrd"))
                    .and_then(|m| m.as_str().parse::<i32>().ok())
            });
        (books, next_ord)
    }

    fn parse_search_key(html: &str) -> Option<String> {
        let document = Html::parse_document(html);
        let re = regex::Regex::new(r#""search_key":"(?<searchKey>.*)","#).unwrap();
        let selector = sel("script")?;
        document
            .select(&selector)
            .filter_map(|el| {
                let data: String = el.text().collect();
                if data.is_empty() {
                    None
                } else {
                    Some(data)
                }
            })
            .find_map(|data| {
                re.captures(&data)
                    .and_then(|c| c.name("searchKey"))
                    .map(|m| m.as_str().to_string())
            })
    }
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

// ---------------------------------------------------------------------------
// Client —— 对应 Kotlin YenPressClient.kt
// ---------------------------------------------------------------------------

pub struct YenPressClient {
    http: reqwest::Client,
    search_key: std::sync::Mutex<String>,
}

impl YenPressClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            search_key: std::sync::Mutex::new("search-vhfh3tijxttuxhjjmzgajcd4".to_string()),
        }
    }

    pub async fn search_series(
        &self,
        name: &str,
    ) -> Result<Vec<YenPressSearchResult>, ProviderError> {
        match self.search(name).await {
            Ok(results) => Ok(results),
            Err(ProviderError::Status(_, reqwest::StatusCode::UNAUTHORIZED, _)) => {
                let key = self.fetch_search_key().await?;
                *self.search_key.lock().unwrap() = key;
                self.search(name).await
            }
            Err(e) => Err(e),
        }
    }

    async fn search(&self, name: &str) -> Result<Vec<YenPressSearchResult>, ProviderError> {
        let key = self.search_key.lock().unwrap().clone();
        let body = construct_query_payload(name);
        let response = self
            .http
            .post(format!(
                "{YEN_PRESS_SEARCH_URL}api/as/v1/engines/yenpress/search.json"
            ))
            .header("Authorization", format!("Bearer {key}"))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::YenPress, status, text));
        }
        Ok(response.json::<YenPressSearchResponse>().await?.results)
    }

    pub async fn get_book_list(
        &self,
        id: &YenPressSeriesId,
    ) -> Result<Vec<YenPressBookShort>, ProviderError> {
        let mut all_books: Vec<YenPressBookShort> = Vec::new();
        let (books, mut next_ord) = self.get_more_books(id, 99999).await?;
        all_books.extend(books);
        let mut request_count = 0;
        while next_ord.is_some() && request_count < 50 {
            let (batch, next) = self.get_more_books(id, next_ord.unwrap()).await?;
            all_books.extend(batch);
            next_ord = next;
            request_count += 1;
        }
        all_books.sort_by(|a, b| {
            a.number
                .as_ref()
                .map(|n| n.start)
                .partial_cmp(&b.number.as_ref().map(|n| n.start))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(all_books)
    }

    async fn get_more_books(
        &self,
        id: &YenPressSeriesId,
        next_ord: i32,
    ) -> Result<(Vec<YenPressBookShort>, Option<i32>), ProviderError> {
        let response = self
            .http
            .get(format!("{YEN_PRESS_BASE_URL}series/get_more/{}", id.0))
            .query(&[("next_ord", next_ord.to_string())])
            .header("x-requested-with", "XMLHttpRequest")
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::YenPress, status, text));
        }
        let text = response.text().await?;
        Ok(YenPressParser::parse_more_books_response(&text))
    }

    pub async fn get_book(&self, book_id: &YenPressBookId) -> Result<YenPressBook, ProviderError> {
        let response = self
            .http
            .get(format!("{YEN_PRESS_BASE_URL}titles/{}", book_id.0))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::YenPress, status, text));
        }
        let text = response.text().await?;
        Ok(YenPressParser::parse_book(&text, book_id))
    }

    pub async fn get_book_thumbnail(
        &self,
        book: &YenPressBook,
    ) -> Result<Option<Image>, ProviderError> {
        let Some(url) = book.image_url.as_deref() else {
            return Ok(None);
        };
        let response = self.http.get(url).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Ok(None);
        }
        let bytes = response.bytes().await?;
        Ok(Some(Image::new(bytes.to_vec(), detect_mime(url))))
    }

    async fn fetch_search_key(&self) -> Result<String, ProviderError> {
        let response = self
            .http
            .get(format!("{YEN_PRESS_BASE_URL}search"))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::YenPress, status, text));
        }
        let text = response.text().await?;
        YenPressParser::parse_search_key(&text)
            .ok_or_else(|| ProviderError::message("failed to parse YenPress search key"))
    }
}

fn construct_query_payload(search_query: &str) -> String {
    serde_json::json!({
        "query": search_query,
        "filters": {
            "all": [
                { "all": [ { "type": "series" } ] }
            ]
        },
        "precision": 2,
        "search_fields": {
            "title": { "weight": 10 }
        },
        "result_fields": {
            "title": { "raw": {} },
            "url": { "raw": {} },
            "image": { "raw": {} }
        },
        "page": { "size": 10, "current": 1 }
    })
    .to_string()
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
// Metadata mapper —— 对应 Kotlin YenPressMetadataMapper.kt
// ---------------------------------------------------------------------------

pub struct YenPressMetadataMapper {
    metadata_config: crate::config::SeriesMetadataConfig,
    book_metadata_config: crate::config::BookMetadataConfig,
    author_roles: Vec<AuthorRole>,
    artist_roles: Vec<AuthorRole>,
}

impl YenPressMetadataMapper {
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
        book: &YenPressBook,
        books: &[YenPressBookShort],
        thumbnail: Option<Image>,
    ) -> ProviderSeriesMetadata {
        let cfg = &self.metadata_config;

        let title = SeriesTitle {
            name: series_title_from_book(&book.name),
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

        let authors = if cfg.authors {
            self.authors(&book.authors)
        } else {
            Vec::new()
        };
        let summary = cfg.summary.then_some(book.description.clone()).flatten();
        let genres: Vec<String> = if cfg.genres {
            book.genres.clone()
        } else {
            Vec::new()
        };
        let release_date = cfg
            .release_date
            .then(|| book.release_date.as_deref().and_then(parse_release_date_rd))
            .flatten();
        let age_rating = cfg
            .age_rating
            .then(|| book.age_rating.as_deref().and_then(age_rating))
            .flatten();
        let publisher = cfg
            .publisher
            .then(|| {
                book.imprint.as_ref().map(|name| Publisher {
                    name: name.clone(),
                    r#type: Some(PublisherType::Localized),
                    language_tag: None,
                })
            })
            .flatten();
        let total_book_count: Option<i32> = if cfg.total_book_count {
            if books.is_empty() {
                None
            } else {
                Some(books.len() as i32)
            }
        } else {
            None
        };
        let links = if cfg.links {
            vec![WebLink {
                label: "YenPress".to_string(),
                url: series_url(&book.series_id),
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
            books
                .iter()
                .map(|it| SeriesBook {
                    id: ProviderBookId(it.id.0.clone()),
                    number: it.number.clone(),
                    name: it.name.clone(),
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
        book: &YenPressBook,
        thumbnail: Option<Image>,
    ) -> ProviderBookMetadata {
        let cfg = &self.book_metadata_config;
        let metadata = BookMetadata {
            number: if cfg.number {
                book.number.clone()
            } else {
                None
            },
            title: if cfg.title {
                Some(book_title(&book.name))
            } else {
                None
            },
            authors: if cfg.authors {
                self.authors(&book.authors)
            } else {
                Vec::new()
            },
            summary: if cfg.summary {
                book.description.clone()
            } else {
                None
            },
            release_date: if cfg.release_date {
                parse_release_date(&book.release_date)
            } else {
                None
            },
            isbn: if cfg.isbn { book.isbn.clone() } else { None },
            start_chapter: None,
            end_chapter: None,
            thumbnail,
            links: if cfg.links {
                vec![WebLink {
                    label: "YenPress".to_string(),
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

    fn authors(&self, authors: &[YenPressAuthor]) -> Vec<Author> {
        authors
            .iter()
            .flat_map(|author| match author.role.as_str() {
                "Author" | "Original author" => self
                    .author_roles
                    .iter()
                    .map(|role| Author {
                        role: *role,
                        name: author.name.clone(),
                    })
                    .collect::<Vec<_>>(),
                "Illustrated by:" | "Artist" => self
                    .artist_roles
                    .iter()
                    .map(|role| Author {
                        role: *role,
                        name: author.name.clone(),
                    })
                    .collect::<Vec<_>>(),
                "Created by" => self
                    .author_roles
                    .iter()
                    .chain(self.artist_roles.iter())
                    .map(|role| Author {
                        role: *role,
                        name: author.name.clone(),
                    })
                    .collect::<Vec<_>>(),
                "Translated by" => vec![Author {
                    role: AuthorRole::Translator,
                    name: author.name.clone(),
                }],
                "Letterer" => vec![Author {
                    role: AuthorRole::Letterer,
                    name: author.name.clone(),
                }],
                _ => Vec::new(),
            })
            .collect()
    }

    pub fn to_series_search_result(&self, result: &YenPressSearchResult) -> SeriesSearchResult {
        SeriesSearchResult {
            url: Some(series_url(&result.id())),
            provider: CoreProviders::YenPress.as_str().to_string(),
            title: result.title.raw.clone(),
            result_id: result.id().0,
            image_url: result.image.as_ref().map(|i| i.raw.clone()),
            media_type: None,
            language: None,
        }
    }
}

fn book_title(name: &str) -> String {
    let re = regex::Regex::new(r"(\(light novel\))|(\(manga\))").unwrap();
    re.replace_all(name, "").trim().to_string()
}

fn series_title_from_book(name: &str) -> String {
    let re = regex::Regex::new(r", Vol\. [0-9]+").unwrap();
    re.replace_all(&book_title(name), "").trim().to_string()
}

/// Kotlin `LocalDate.Format { monthName(ENGLISH_ABBREVIATED); ' '; day(); ", "; year() }`，
/// 即 `MMM d, yyyy`（day 无前导零）。
fn parse_release_date(raw: &Option<String>) -> Option<String> {
    let raw = raw.as_deref()?;
    let parsed = parse_naive_date(raw)?;
    Some(parsed.format("%Y-%m-%d").to_string())
}

fn parse_release_date_rd(raw: &str) -> Option<ReleaseDate> {
    let parsed = parse_naive_date(raw)?;
    Some(ReleaseDate::new(
        Some(parsed.year()),
        Some(parsed.month()),
        Some(parsed.day()),
    ))
}

fn parse_naive_date(raw: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(raw, "%b %e, %Y")
        .or_else(|_| chrono::NaiveDate::parse_from_str(raw, "%b %d, %Y"))
        .ok()
}

fn age_rating(age_rating: &str) -> Option<i32> {
    match age_rating {
        "All Ages" => Some(6),
        "T (Teen)" => Some(13),
        "OT (Older Teen)" => Some(16),
        "18+ M (Mature)" => Some(18),
        _ => None,
    }
}

fn series_url(id: &YenPressSeriesId) -> String {
    format!("{YEN_PRESS_BASE_URL}series/{}", encode_path_segment(&id.0))
}

fn book_url(id: &YenPressBookId) -> String {
    format!("{YEN_PRESS_BASE_URL}titles/{}", id.0)
}

/// Kotlin `encodeURLPathPart`：URL 编码 path 段（保留 unreserved）。
fn encode_path_segment(segment: &str) -> String {
    url::form_urlencoded::byte_serialize(segment.as_bytes())
        .collect::<String>()
        .replace("+", "%20")
        .replace("%2F", "/")
}

// ---------------------------------------------------------------------------
// Provider —— 对应 Kotlin YenPressMetadataProvider.kt
// ---------------------------------------------------------------------------

pub struct YenPressMetadataProvider {
    client: YenPressClient,
    metadata_mapper: YenPressMetadataMapper,
    name_matcher: NameSimilarityMatcher,
    media_type: MediaType,
    fetch_series_covers: bool,
    fetch_book_covers: bool,
}

pub fn create_provider(
    config: &ProviderConfig,
    default_name_matcher: NameSimilarityMatcher,
    http_client: &reqwest::Client,
) -> Option<YenPressMetadataProvider> {
    if !config.enabled {
        return None;
    }
    if config.media_type == MediaType::Comic {
        return None; // Kotlin: throw IllegalStateException("Comics media type is not supported")
    }
    let name_matcher = config.name_matching_mode.unwrap_or(default_name_matcher);
    Some(YenPressMetadataProvider {
        client: YenPressClient::new(http_client.clone()),
        metadata_mapper: YenPressMetadataMapper::new(
            config.series_metadata.clone(),
            config.book_metadata.clone(),
            config.author_roles.clone(),
            config.artist_roles.clone(),
        ),
        name_matcher,
        media_type: config.media_type,
        fetch_series_covers: config.series_metadata.thumbnail,
        fetch_book_covers: config.book_metadata.thumbnail,
    })
}

#[async_trait::async_trait]
impl MetadataProvider for YenPressMetadataProvider {
    fn provider_name(&self) -> CoreProviders {
        CoreProviders::YenPress
    }

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        let all_books = self
            .client
            .get_book_list(&YenPressSeriesId(series_id.0.clone()))
            .await?;
        let first_book = all_books
            .iter()
            .find(|b| b.number.is_some())
            .cloned()
            .or_else(|| (all_books.len() == 1).then(|| all_books[0].clone()))
            .ok_or_else(|| ProviderError::message("Can't find first book"))?;
        let series_book = self.client.get_book(&first_book.id).await?;
        let thumbnail = if self.fetch_series_covers {
            self.client.get_book_thumbnail(&series_book).await?
        } else {
            None
        };
        Ok(self
            .metadata_mapper
            .to_series_metadata(&series_book, &all_books, thumbnail))
    }

    async fn get_series_cover(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError> {
        let all_books = self
            .client
            .get_book_list(&YenPressSeriesId(series_id.0.clone()))
            .await?;
        let Some(first) = all_books.first() else {
            return Ok(None);
        };
        let book = self.client.get_book(&first.id).await?;
        self.client.get_book_thumbnail(&book).await
    }

    async fn get_book_metadata(
        &self,
        _series_id: &ProviderSeriesId,
        book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError> {
        let book = self
            .client
            .get_book(&YenPressBookId(book_id.0.clone()))
            .await?;
        let thumbnail = if self.fetch_book_covers {
            self.client.get_book_thumbnail(&book).await?
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
        let truncated: String = series_name.chars().take(128).collect();
        let results = self.client.search_series(&truncated).await?;
        Ok(results
            .into_iter()
            .take(limit)
            .map(|r| self.metadata_mapper.to_series_search_result(&r))
            .collect())
    }

    async fn match_series_metadata(
        &self,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        let series_name = &match_query.series_name;
        let truncated: String = series_name.chars().take(128).collect();
        let results = self.client.search_series(&truncated).await?;

        let matched = results
            .into_iter()
            .filter(|r| !r.title.raw.contains("(audio)"))
            .filter(|r| match self.media_type {
                MediaType::Manga => !r.title.raw.contains("(light novel)"),
                MediaType::Novel => !r.title.raw.contains("(manga)"),
                MediaType::Comic | MediaType::Webtoon => false,
            })
            .find(|r| {
                let candidate = series_title_from_book(&r.title.raw);
                self.name_matcher.matches_single(
                    &match_query.normalized_series_name(),
                    &match_query.normalize_title(&candidate),
                )
            });

        let Some(result) = matched else {
            return Ok(None);
        };
        let books = self.client.get_book_list(&result.id()).await?;
        let first_book = books
            .iter()
            .find(|b| b.number.is_some())
            .cloned()
            .or_else(|| (books.len() == 1).then(|| books[0].clone()));
        let Some(first_book) = first_book else {
            return Ok(None);
        };
        let series_book = self.client.get_book(&first_book.id).await?;
        let thumbnail = if self.fetch_series_covers {
            self.client.get_book_thumbnail(&series_book).await?
        } else {
            None
        };
        Ok(Some(self.metadata_mapper.to_series_metadata(
            &series_book,
            &books,
            thumbnail,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_title_strips_suffixes() {
        // Kotlin: replace(regex, "") 后内部双空格保留（仅 trim 首尾）
        assert_eq!(book_title("My Series (manga) Vol. 3"), "My Series  Vol. 3");
        assert_eq!(
            book_title("My Series (light novel) Vol. 3"),
            "My Series  Vol. 3"
        );
        assert_eq!(
            series_title_from_book("My Series (manga), Vol. 3"),
            "My Series"
        );
        assert_eq!(series_title_from_book("My Series, Vol. 12"), "My Series");
    }

    #[test]
    fn parse_release_date_formats() {
        assert_eq!(
            parse_release_date(&Some("Sep 17, 2026".to_string())).as_deref(),
            Some("2026-09-17")
        );
        assert_eq!(
            parse_release_date(&Some("Jan 5, 2020".to_string())).as_deref(),
            Some("2020-01-05")
        );
        assert_eq!(parse_release_date(&None), None);
    }

    #[test]
    fn age_rating_mapping() {
        assert_eq!(age_rating("All Ages"), Some(6));
        assert_eq!(age_rating("T (Teen)"), Some(13));
        assert_eq!(age_rating("OT (Older Teen)"), Some(16));
        assert_eq!(age_rating("18+ M (Mature)"), Some(18));
        assert_eq!(age_rating("Unknown"), None);
    }

    #[test]
    fn search_payload_shape_matches_kotlin() {
        let payload = construct_query_payload("Naruto");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["query"], "Naruto");
        assert_eq!(v["filters"]["all"][0]["all"][0]["type"], "series");
        assert_eq!(v["precision"], 2);
        assert_eq!(v["search_fields"]["title"]["weight"], 10);
        assert_eq!(
            v["result_fields"]["title"]["raw"],
            serde_json::Value::Object(Default::default())
        );
        assert_eq!(v["page"]["size"], 10);
    }

    #[test]
    fn search_result_id_from_url() {
        let result = YenPressSearchResult {
            title: YenPressSearchField { raw: "Test".into() },
            url: YenPressSearchField {
                raw: "/series/test-series".into(),
            },
            image: None,
        };
        assert_eq!(result.id().0, "test-series");
    }

    #[test]
    fn titles_cleared_when_title_disabled() {
        // Kotlin MetadataConfigApplier.seriesTitles：title=false 时保留 name、清空 type/language
        let mut cfg = crate::config::SeriesMetadataConfig::default();
        cfg.title = false;
        let mapper = YenPressMetadataMapper::new(
            cfg,
            crate::config::BookMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
        );
        let book = YenPressBook {
            id: YenPressBookId("1".into()),
            name: "Test Series, Vol. 1".into(),
            number: None,
            series_id: YenPressSeriesId("test-series".into()),
            authors: vec![],
            description: None,
            genres: vec![],
            series_name: None,
            page_count: None,
            release_date: None,
            isbn: None,
            age_rating: None,
            imprint: None,
            image_url: None,
        };
        let meta = mapper.to_series_metadata(&book, &[], None);
        let t = meta.metadata.titles.first().unwrap();
        assert_eq!(t.name, "Test Series");
        assert_eq!(t.r#type, None);
        assert_eq!(t.language, None);
        // books 配置关闭 -> 空列表
        assert!(meta.books.is_empty());
    }
}
