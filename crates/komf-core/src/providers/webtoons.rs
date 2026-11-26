//! Webtoons provider —— 对应 Kotlin `providers/webtoons` 包。

use scraper::{Element, ElementRef, Html, Selector};
use serde::Deserialize;
use std::time::Duration;

use crate::config::{WebtoonsConfig, WebtoonsLanguage};
use crate::model::{
    Author, AuthorRole, BookMetadata, BookRange, Image, MatchQuery, ProviderBookId,
    ProviderBookMetadata, ProviderSeriesId, ProviderSeriesMetadata, SeriesBook, SeriesMetadata,
    SeriesSearchResult, SeriesStatus, SeriesTitle, TitleType, WebLink,
};
use crate::providers::{detect_image_mime, CoreProviders, MetadataProvider, ProviderError};
use crate::util::{NameSimilarityMatcher, TtlCache};

const BASE_URL: &str = "https://www.webtoons.com";
const MOBILE_BASE_URL: &str = "https://m.webtoons.com";
const IMAGE_BASE_URL: &str = "https://webtoon-phinf.pstatic.net";

// ---------------------------------------------------------------------------
// 模型 —— 对应 Kotlin model/*.kt
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WebtoonsSeriesId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct WebtoonsChapterId(String);

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct PersonInfo {
    name: String,
    description: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WebtoonsSeries {
    id: WebtoonsSeriesId,
    title: String,
    description: Option<String>,
    url: String,
    thumbnail_url: Option<String>,
    genres: Vec<String>,
    status: Status,
    author: Option<PersonInfo>,
    adapted_by: Option<PersonInfo>,
    artist: Option<PersonInfo>,
    views: String,
    subscribers: String,
    chapters: Option<Vec<Episode>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ongoing,
    Completed,
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct SearchApiResponse {
    result: SearchResult,
    success: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchResult {
    challenge_result: Option<ChallengeResult>,
    webtoon_result: Option<WebtoonResult>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ChallengeResult {
    total_count: i32,
    title_list: Vec<Title>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct WebtoonResult {
    total_count: i32,
    title_list: Vec<Title>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Title {
    title_no: i32,
    title: String,
    title_group_name: Option<String>,
    represent_genre: Option<String>,
    thumbnail_mobile: String,
    unsuitable_for_children: bool,
    picture_author_name: Option<String>,
    writing_author_name: String,
    last_episode_register_ymdt: i64,
    read_count: i32,
}

impl Title {
    fn get_original_url(&self, language: WebtoonsLanguage) -> Option<String> {
        let genre = self.represent_genre.as_deref()?;
        if genre.is_empty() {
            return None;
        }
        let title_group_name = self
            .title_group_name
            .clone()
            .unwrap_or_else(|| seo_encoding(&self.title));
        Some(format!(
            "{BASE_URL}/{}/{genre}/{title_group_name}/list?title_no={}",
            language.path_code(),
            self.title_no
        ))
    }

    fn get_canvas_url(&self, language: WebtoonsLanguage) -> String {
        let title_group_name = self
            .title_group_name
            .clone()
            .unwrap_or_else(|| seo_encoding(&self.title));
        format!(
            "{BASE_URL}/{}/canvas/{title_group_name}/list?title_no={}",
            language.path_code(),
            self.title_no
        )
    }

    fn get_original_id(&self, language: WebtoonsLanguage) -> Option<WebtoonsSeriesId> {
        let original_url = self.get_original_url(language)?;
        Some(WebtoonsSeriesId(encoded_path_and_query(&original_url)))
    }

    fn get_canvas_id(&self, language: WebtoonsLanguage) -> WebtoonsSeriesId {
        WebtoonsSeriesId(encoded_path_and_query(&self.get_canvas_url(language)))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct EpisodeListApiResponse {
    result: EpisodeListResult,
    success: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct EpisodeListResult {
    episode_list: Vec<Episode>,
    next_cursor: i32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Episode {
    episode_no: i32,
    thumbnail: String,
    episode_title: String,
    viewer_link: String,
    exposure_date_millis: i64,
    display_up: bool,
    #[serde(default)]
    has_bgm: bool,
}

impl Episode {
    fn get_url(&self) -> String {
        format!("{BASE_URL}{}", self.viewer_link)
    }

    fn get_id(&self) -> WebtoonsChapterId {
        WebtoonsChapterId(encoded_path_and_query(&self.get_url()))
    }
}

// From vendor-#####.js —— Kotlin replaceEncodedChars
fn replace_encoded_chars(input: &str) -> String {
    input
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

// From vendor-#####.js —— Kotlin seoEncoding
fn seo_encoding(input: &str) -> String {
    if input.is_empty() {
        return "_".to_string();
    }
    let mut processed = replace_encoded_chars(input).to_lowercase();
    let re = regex::Regex::new(r#"[`~!@#$%^&*|\\'";:/?({\[\]})]"#).unwrap();
    processed = re
        .replace_all(&processed, "")
        .replace(' ', "-")
        .replace('_', "-");
    let re_dash = regex::Regex::new(r"-+").unwrap();
    processed = re_dash.replace_all(&processed, "-").to_string();
    if let Some(stripped) = processed.strip_prefix('-') {
        processed = stripped.to_string();
    }
    if processed.is_empty() {
        "_".to_string()
    } else {
        processed
    }
}

/// Kotlin `Url(url).encodedPathAndQuery`。
fn encoded_path_and_query(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            let path = parsed.path().to_string();
            match parsed.query() {
                Some(q) => format!("{path}?{q}"),
                None => path,
            }
        }
        Err(_) => url.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Parser —— 对应 Kotlin WebtoonsParser.kt
// ---------------------------------------------------------------------------

struct WebtoonsParser;

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

fn elem_text(el: &ElementRef) -> String {
    el.text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// jsoup ownText()：仅直接文本节点。
fn own_text(el: &ElementRef) -> String {
    el.children()
        .filter_map(|node| node.value().as_text().map(|t| t.text.to_string()))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl WebtoonsParser {
    fn parse_series(&self, series: &str) -> WebtoonsSeries {
        let document = Html::parse_document(series);

        let detail_element = doc_first(&document, ".detail_header > div.info");
        let info_element = doc_first(&document, "#_asideDetail");
        let people_element = doc_first(
            &document,
            "#wrap > div._authorInfoLayer div._authorInnerContent",
        );

        let title = doc_first(&document, "h1.subj, h3.subj")
            .map(|el| elem_text(&el))
            .unwrap_or_default();
        let description = info_element
            .as_ref()
            .and_then(|el| el_first(el, "p.summary"))
            .map(|el| elem_text(&el));
        let day_info = info_element
            .as_ref()
            .and_then(|el| el_first(el, "p.day_info"));
        let status = parse_status(day_info.as_ref());
        let image_url = doc_first(&document, "head meta[property=\"og:image\"]")
            .and_then(|el| el.attr("content"))
            .map(|s| s.to_string());
        let genres = detail_element
            .as_ref()
            .map(|el| {
                let selector = sel(".genre").unwrap();
                el.select(&selector)
                    .map(|g| elem_text(&g))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let (author, adapted_by, artist): (
            Option<PersonInfo>,
            Option<PersonInfo>,
            Option<PersonInfo>,
        ) = if let Some(people) = people_element.as_ref() {
            let original = get_person_info(people, ORIGINAL_LABELS);
            let writer = get_person_info(people, WRITER_LABELS);
            let creator = get_person_info(people, CREATOR_LABELS);
            // 有原作时，编剧（Written by / 編劇）即改编者
            let adapted_by = get_person_info(people, ADAPTED_LABELS)
                .or_else(|| writer.clone().filter(|_| original.is_some()));
            let author = original.or(writer).or_else(|| creator.clone());
            let artist = get_person_info(people, ART_LABELS)
                .or(creator)
                .or_else(|| author.clone());
            (author, adapted_by, artist)
        } else {
            let author_backup = detail_element
                .as_ref()
                .and_then(|el| el_first(el, ".author_area"))
                .map(|el| own_text(&el))
                .unwrap_or_default();
            // Kotlin: .author:nth-of-type(1).first()?.ownText() ?: authorBackup
            // —— ?: 仅在 null 时回退；ownText 为空串时 Kotlin 保留空串
            let author_name = detail_element
                .as_ref()
                .and_then(|el| el_first(el, ".author:nth-of-type(1)"))
                .map(|el| own_text(&el))
                .unwrap_or(author_backup.clone());
            let artist_name = detail_element
                .as_ref()
                .and_then(|el| el_first(el, ".author:nth-of-type(2)"))
                .map(|el| own_text(&el));
            // Kotlin: PersonInfo(artistName ?: authorName) —— 回退到 authorName
            let artist_name = match artist_name {
                Some(name) => name,
                None => author_name.clone(),
            };
            (
                Some(PersonInfo {
                    name: author_name,
                    description: None,
                }),
                None,
                Some(PersonInfo {
                    name: artist_name,
                    description: None,
                }),
            )
        };

        let views = info_element
            .as_ref()
            .map(|el| {
                let selector = sel("li span.ico_view + em").unwrap();
                el.select(&selector)
                    .map(|e| elem_text(&e))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let subscribers = info_element
            .as_ref()
            .map(|el| {
                let selector = sel("li span.ico_subscribe + em").unwrap();
                el.select(&selector)
                    .map(|e| elem_text(&e))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();

        let url = get_document_url(&document);
        WebtoonsSeries {
            id: WebtoonsSeriesId(encoded_path_and_query(&url)),
            title,
            description,
            url,
            thumbnail_url: image_url,
            genres,
            status,
            author,
            adapted_by,
            artist,
            views,
            subscribers,
            chapters: None,
        }
    }
}

fn get_document_url(document: &Html) -> String {
    let selector = sel("meta[property=\"og:url\"]").unwrap();
    document
        .select(&selector)
        .next()
        .and_then(|el| el.attr("content"))
        .unwrap_or_default()
        .to_string()
}

// 作者信息弹层 `<p class="by">` 的标签文字，随语言站台不同（en / zh-hant）
const ORIGINAL_LABELS: &[&str] = &["Original work by", "原作"];
const WRITER_LABELS: &[&str] = &["Written by", "Story by", "編劇"];
const ADAPTED_LABELS: &[&str] = &["Adapted by", "改編"];
const ART_LABELS: &[&str] = &["Art by", "繪圖", "作畫"];
const CREATOR_LABELS: &[&str] = &["Created by", "作家"];

/// 连载状态：优先看各语言站台共用的图标 class（`txt_ico_up` / `txt_ico_completed`），
/// 与页面语言无关；取不到时回退到文字匹配。
fn parse_status(day_info: Option<&ElementRef>) -> Status {
    let Some(day_info) = day_info else {
        return Status::Unknown;
    };
    if el_first(day_info, ".txt_ico_completed").is_some() {
        return Status::Completed;
    }
    if el_first(day_info, ".txt_ico_up").is_some() {
        return Status::Ongoing;
    }
    status_from_text(&elem_text(day_info))
}

fn status_from_text(text: &str) -> Status {
    match text {
        s if s.contains("UP")
            || s.contains("EVERY")
            || s.contains("NOUVEAU")
            || s.contains("更新") =>
        {
            Status::Ongoing
        }
        s if s.contains("END")
            || s.contains("COMPLETED")
            || s.contains("TERMINÉ")
            || s.contains("完結") =>
        {
            Status::Completed
        }
        _ => Status::Unknown,
    }
}

/// Kotlin `p.by:contains($text) + h3.title`（scraper 不支持 :contains，手动模拟）。
/// `labels` 为各语言站台的候选标签，命中任一即可。
fn get_person_info(people_element: &ElementRef, labels: &[&str]) -> Option<PersonInfo> {
    let p_by = sel("p.by")?;
    let p_desc = sel("p.desc")?;
    for el in people_element.select(&p_by) {
        let label = elem_text(&el);
        if labels.iter().any(|text| label.contains(text)) {
            let title = el
                .next_sibling_element()
                .filter(|s| s.value().name() == "h3")?;
            // Kotlin: ...first()?.parent()?.select("p.desc")?.first()?.text()
            // p.desc 在 p.by 的父元素内，而不是在 h3.title 内
            let description = el
                .parent()
                .and_then(ElementRef::wrap)
                .and_then(|parent| parent.select(&p_desc).next())
                .map(|d| elem_text(&d));
            return Some(PersonInfo {
                name: elem_text(&title),
                description,
            });
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Client —— 对应 Kotlin WebtoonsClient.kt
// ---------------------------------------------------------------------------

pub struct WebtoonsClient {
    http: reqwest::Client,
    base_headers: reqwest::header::HeaderMap,
    mobile_headers: reqwest::header::HeaderMap,
    parser: WebtoonsParser,
    /// 语言站台（决定 locale cookie、搜索结果网址与 metadata 语言）。
    language: WebtoonsLanguage,
    /// 匀速限流（对齐原版 rateLimiter(1, 1s)，即 1 秒/许可）。
    limiter: crate::rate_limiter::ThroughputLimiter,
}

impl WebtoonsClient {
    pub fn new(http: reqwest::Client, language: WebtoonsLanguage) -> Self {
        use reqwest::header::{HeaderMap, HeaderValue, COOKIE, REFERER};
        // locale cookie 决定移动版搜索 API 返回哪个语言站台的作品；
        // ageGatePass / needGDPR 对齐 Kotlin 原版（跳过年龄确认与 GDPR 弹窗）。
        let cookies = format!(
            "ageGatePass=true; locale={}; needGDPR=false",
            language.cookie_locale()
        );
        let cookie_value =
            HeaderValue::from_str(&cookies).expect("webtoons cookie header is ASCII");

        let mut base_headers = HeaderMap::new();
        base_headers.insert(
            REFERER,
            HeaderValue::from_str(&format!("{BASE_URL}/{}", language.path_code()))
                .expect("webtoons referer is ASCII"),
        );
        base_headers.insert(COOKIE, cookie_value.clone());
        let mut mobile_headers = HeaderMap::new();
        mobile_headers.insert(REFERER, HeaderValue::from_static(MOBILE_BASE_URL));
        mobile_headers.insert(COOKIE, cookie_value);
        Self {
            http,
            base_headers,
            mobile_headers,
            parser: WebtoonsParser,
            language,
            limiter: crate::rate_limiter::ThroughputLimiter::new(
                1,
                std::time::Duration::from_secs(1),
            ),
        }
    }

    pub async fn search_series(
        &self,
        name: &str,
    ) -> Result<(Vec<Title>, Vec<Title>), ProviderError> {
        let webtoons = self
            .search_page(name, "WEBTOON")
            .await?
            .result
            .webtoon_result
            .map(|r| r.title_list)
            .unwrap_or_default();
        let originals = self
            .search_page(name, "CHALLENGE")
            .await?
            .result
            .challenge_result
            .map(|r| r.title_list)
            .unwrap_or_default();
        Ok((webtoons, originals))
    }

    async fn search_page(
        &self,
        name: &str,
        search_type: &str,
    ) -> Result<SearchApiResponse, ProviderError> {
        self.limiter.acquire().await;
        let response = self
            .http
            .get(format!("{MOBILE_BASE_URL}/undefined/search/result"))
            .query(&[("keyword", name), ("searchType", search_type)])
            .headers(self.mobile_headers.clone())
            .send()
            .await?;
        let response = super::ensure_success(CoreProviders::Webtoons, response).await?;
        Ok(response.json::<SearchApiResponse>().await?)
    }

    pub async fn get_series(&self, id: &WebtoonsSeriesId) -> Result<WebtoonsSeries, ProviderError> {
        self.limiter.acquire().await;
        let response = self
            .http
            .get(format!("{BASE_URL}{}", id.0))
            .headers(self.base_headers.clone())
            .send()
            .await?;
        let response = super::ensure_success(CoreProviders::Webtoons, response).await?;
        let text = response.text().await?;
        Ok(self.parser.parse_series(&text))
    }

    pub async fn get_chapters(&self, id: &WebtoonsSeriesId) -> Result<Vec<Episode>, ProviderError> {
        self.limiter.acquire().await;
        let chapters_path = if id.0.contains("/canvas/") {
            "canvas"
        } else {
            "webtoon"
        };
        let title_no = extract_title_no(&id.0).ok_or_else(|| {
            ProviderError::message(format!(
                "Expected either 'title_no' or 'titleNo' parameter to be present in the URL '{}'",
                id.0
            ))
        })?;
        let mut query = vec![("pageSize", "99999")];
        if chapters_path == "canvas" {
            // 对齐 Tachiyomi 扩展：canvas 章节列表需指定阅读语言
            let language = WebtoonsLanguage::from_url_path(&id.0).unwrap_or(self.language);
            query.push(("readingLanguageCode", language.path_code()));
        }
        let response = self
            .http
            .get(format!(
                "{MOBILE_BASE_URL}/api/v1/{chapters_path}/{title_no}/episodes"
            ))
            .query(&query)
            .headers(self.mobile_headers.clone())
            .send()
            .await?;
        let response = super::ensure_success(CoreProviders::Webtoons, response).await?;
        Ok(response
            .json::<EpisodeListApiResponse>()
            .await?
            .result
            .episode_list)
    }

    pub async fn get_series_with_chapters(
        &self,
        id: &WebtoonsSeriesId,
    ) -> Result<WebtoonsSeries, ProviderError> {
        let mut series = self.get_series(id).await?;
        series.chapters = Some(self.get_chapters(id).await?);
        Ok(series)
    }

    pub async fn get_series_thumbnail(
        &self,
        series: &WebtoonsSeries,
    ) -> Result<Option<Image>, ProviderError> {
        self.limiter.acquire().await;
        let Some(url_raw) = series.thumbnail_url.as_deref() else {
            return Ok(None);
        };
        let url = remove_query_param(url_raw, "type");
        let response = self
            .http
            .get(&url)
            .headers(self.base_headers.clone())
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Ok(None);
        }
        let bytes = response.bytes().await?;
        // Kotlin getThumbnail 扩展名推断（共享 detect_image_mime）
        Ok(Some(Image::new(bytes.to_vec(), detect_image_mime(&url))))
    }

    pub async fn get_chapter_thumbnail(&self, chapter: &Episode) -> Result<Image, ProviderError> {
        self.limiter.acquire().await;
        let url_raw = format!("{IMAGE_BASE_URL}{}", chapter.thumbnail);
        let url = remove_query_param(&url_raw, "type");
        let response = self
            .http
            .get(&url)
            .headers(self.mobile_headers.clone())
            .send()
            .await?;
        let response = super::ensure_success(CoreProviders::Webtoons, response).await?;
        let bytes = response.bytes().await?;
        // Kotlin getThumbnail 扩展名推断（共享 detect_image_mime）
        Ok(Image::new(bytes.to_vec(), detect_image_mime(&url)))
    }
}

fn extract_title_no(id: &str) -> Option<String> {
    // Ktor Url 可解析相对 URL；先尝试完整解析，失败则手动拆 query
    let query = match url::Url::parse(id) {
        Ok(parsed) => parsed.query().map(|q| q.to_string()),
        Err(_) => id.split('?').nth(1).map(|q| q.to_string()),
    };
    let query = query?;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        let value = parts.next().unwrap_or("");
        if key == "title_no" || key == "titleNo" {
            return Some(value.to_string());
        }
    }
    None
}

/// Kotlin `URLBuilder(url); builder.parameters.remove("type")`。
fn remove_query_param(url: &str, key: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            let rest: Vec<(String, String)> = parsed
                .query_pairs()
                .filter(|(k, _)| k != key)
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            if rest.is_empty() {
                parsed.set_query(None);
                return parsed.to_string();
            }
            parsed.set_query(None);
            {
                let mut pairs = parsed.query_pairs_mut();
                for (k, v) in rest {
                    pairs.append_pair(&k, &v);
                }
            }
            parsed.to_string()
        }
        Err(_) => url.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Metadata mapper —— 对应 Kotlin WebtoonsMetadataMapper.kt
// ---------------------------------------------------------------------------

pub struct WebtoonsMetadataMapper {
    metadata_config: crate::config::SeriesMetadataConfig,
    author_roles: Vec<AuthorRole>,
    artist_roles: Vec<AuthorRole>,
    language: WebtoonsLanguage,
}

impl WebtoonsMetadataMapper {
    pub fn new(
        metadata_config: crate::config::SeriesMetadataConfig,
        author_roles: Vec<AuthorRole>,
        artist_roles: Vec<AuthorRole>,
        language: WebtoonsLanguage,
    ) -> Self {
        Self {
            metadata_config,
            author_roles,
            artist_roles,
            language,
        }
    }

    pub fn to_series_search_result(
        &self,
        results: &(Vec<Title>, Vec<Title>),
    ) -> Vec<SeriesSearchResult> {
        let mut out = Vec::new();
        for title in &results.0 {
            let original_url = title.get_original_url(self.language);
            let result_id = title.get_original_id(self.language);
            if original_url.is_none() || result_id.is_none() {
                continue;
            }
            out.push(SeriesSearchResult {
                url: original_url,
                image_url: Some(format!("{IMAGE_BASE_URL}{}", title.thumbnail_mobile)),
                title: title.title.clone(),
                provider: CoreProviders::Webtoons.as_str().to_string(),
                result_id: result_id.map(|id| id.0).unwrap_or_default(),
                media_type: None,
                language: None,
                nsfw: None,
            });
        }
        for title in &results.1 {
            out.push(SeriesSearchResult {
                url: Some(title.get_canvas_url(self.language)),
                image_url: Some(format!("{IMAGE_BASE_URL}{}", title.thumbnail_mobile)),
                title: title.title.clone(),
                provider: CoreProviders::Webtoons.as_str().to_string(),
                result_id: title.get_canvas_id(self.language).0,
                media_type: None,
                language: None,
                nsfw: None,
            });
        }
        out
    }

    pub fn to_series_metadata(
        &self,
        series: &WebtoonsSeries,
        thumbnail: Option<Image>,
    ) -> ProviderSeriesMetadata {
        let cfg = &self.metadata_config;

        let author: Vec<Author> = series
            .author
            .as_ref()
            .map(|it| {
                self.author_roles
                    .iter()
                    .map(|role| Author {
                        name: it.name.clone(),
                        role: *role,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let artist: Vec<Author> = series
            .artist
            .as_ref()
            .map(|it| {
                self.artist_roles
                    .iter()
                    .map(|role| Author {
                        name: it.name.clone(),
                        role: *role,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut authors = author;
        authors.extend(artist);
        if let Some(adapted) = series.adapted_by.as_ref() {
            authors.push(Author {
                name: adapted.name.clone(),
                role: AuthorRole::Editor,
            });
        }

        // 以作品网址的语言为准（如手动指定了其他语言站台的作品），取不到才用配置值
        let series_language = WebtoonsLanguage::from_url_path(&series.url).unwrap_or(self.language);
        let title = SeriesTitle {
            name: series.title.clone(),
            r#type: Some(TitleType::Localized),
            language: Some(series_language.language_tag().to_string()),
        };
        let title_field = cfg.title.then_some(title.clone());

        // title 关闭仅表示不写入主标题（title 字段）；titles 列表的 type/language
        // 保留——alternativeTitles 写入（Komga 备选 label 由 type/language 生成）与
        // 聚合备选排序依赖语言信息，清空会误伤备选写入。
        let titles = vec![title.clone()];
        let status = if cfg.status {
            match series.status {
                Status::Ongoing => Some(SeriesStatus::Ongoing),
                Status::Completed => Some(SeriesStatus::Ended),
                Status::Unknown => None,
            }
        } else {
            None
        };

        let summary = cfg.summary.then_some(series.description.clone()).flatten();
        let genres: Vec<String> = if cfg.genres {
            series.genres.clone()
        } else {
            Vec::new()
        };
        let authors = if cfg.authors { authors } else { Vec::new() };
        let links = if cfg.links {
            vec![WebLink {
                label: "Webtoon".to_string(),
                url: series.url.clone(),
            }]
        } else {
            Vec::new()
        };

        let metadata = SeriesMetadata {
            status,
            title: title_field,
            titles,
            summary,
            publisher: None,
            alternative_publishers: Vec::new(),
            reading_direction: None,
            age_rating: None, // Kotlin 注释掉不设置
            // Kotlin: language = "en"（MetadataConfigApplier 按 cfg.language 裁剪）；
            // 此处改为按语言站台输出（zh-hant → zh-Hant）
            language: if cfg.language {
                Some(series_language.language_tag().to_string())
            } else {
                None
            },
            genres,
            tags: Vec::new(),
            total_book_count: None, // Kotlin 注释掉不设置
            authors,
            release_date: None, // Kotlin 注释掉不设置
            links,
            score: None,
            thumbnail,
        };

        // Kotlin MetadataConfigApplier：books = getIfEnabled(books, config.books) ?: emptyList()
        let books = if cfg.books {
            series
                .chapters
                .as_ref()
                .map(|chapters| {
                    chapters
                        .iter()
                        .enumerate()
                        .map(|(index, chapter)| SeriesBook {
                            id: ProviderBookId(chapter.get_id().0),
                            number: Some(BookRange::single((index + 1) as f64)),
                            name: Some(chapter.episode_title.clone()),
                            r#type: None,
                            edition: None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        ProviderSeriesMetadata {
            id: ProviderSeriesId(series.id.0.clone()),
            metadata,
            books,
        }
    }

    pub fn to_book_metadata(
        &self,
        index: usize,
        chapter: &Episode,
        series_id: ProviderSeriesId,
        thumbnail: Option<Image>,
    ) -> ProviderBookMetadata {
        let release_date =
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(chapter.exposure_date_millis)
                .map(|dt| dt.format("%Y-%m-%d").to_string());
        ProviderBookMetadata {
            id: Some(ProviderBookId(chapter.get_id().0)),
            series_id: Some(series_id),
            metadata: BookMetadata {
                title: Some(chapter.episode_title.clone()),
                number: Some(BookRange::single((index + 1) as f64)),
                release_date,
                links: vec![WebLink {
                    label: "Webtoon".to_string(),
                    url: chapter.get_url(),
                }],
                thumbnail,
                ..Default::default()
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Provider —— 对应 Kotlin WebtoonsMetadataProvider.kt（含双缓存）
// ---------------------------------------------------------------------------

pub struct WebtoonsMetadataProvider {
    client: WebtoonsClient,
    metadata_mapper: WebtoonsMetadataMapper,
    name_matcher: NameSimilarityMatcher,
    fetch_series_covers: bool,
    fetch_book_covers: bool,
    series_cache: TtlCache<ProviderSeriesId, WebtoonsSeries>,
    chapter_cache: TtlCache<ProviderSeriesId, Vec<Episode>>,
}

pub fn create_provider(
    webtoons_config: &WebtoonsConfig,
    default_name_matcher: NameSimilarityMatcher,
    http_client: &reqwest::Client,
) -> Option<WebtoonsMetadataProvider> {
    let config = &webtoons_config.provider;
    if !config.enabled {
        return None;
    }
    let language = webtoons_config.language;
    let name_matcher = config.name_matching_mode.unwrap_or(default_name_matcher);
    Some(WebtoonsMetadataProvider {
        client: WebtoonsClient::new(http_client.clone(), language),
        metadata_mapper: WebtoonsMetadataMapper::new(
            config.series_metadata.clone(),
            config.author_roles.clone(),
            config.artist_roles.clone(),
            language,
        ),
        name_matcher,
        fetch_series_covers: config.series_metadata.thumbnail,
        fetch_book_covers: config.book_metadata.thumbnail,
        series_cache: TtlCache::new(Duration::from_secs(30 * 60)),
        chapter_cache: TtlCache::new(Duration::from_secs(10 * 60)),
    })
}

#[async_trait::async_trait]
impl MetadataProvider for WebtoonsMetadataProvider {
    fn resolve_link_id(&self, query: &str) -> Option<String> {
        // series_id = URL path+query（对应 WebtoonsSeriesId 的 encoded_path_and_query）
        // Kotlin：链接解析取第一个捕获组（正则 OnceLock 缓存）
        super::capture_link_id(query, r"webtoons\.com([^\s]+)")
    }
    fn provider_name(&self) -> CoreProviders {
        CoreProviders::Webtoons
    }

    fn alternative_titles_enabled(&self) -> bool {
        self.metadata_mapper.metadata_config.alternative_titles
    }

    async fn resolve_link_search_result(&self, query: &str) -> Option<SeriesSearchResult> {
        let id = self.resolve_link_id(query)?;
        let series_id = ProviderSeriesId(id.clone());
        let series = self
            .series_cache
            .get_or_load(series_id.clone(), || async move {
                self.client
                    .get_series_with_chapters(&WebtoonsSeriesId(id))
                    .await
            })
            .await
            .ok()?;
        Some(SeriesSearchResult {
            url: Some(query.trim().to_string()),
            image_url: series
                .thumbnail_url
                .as_ref()
                .map(|u| remove_query_param(u, "type")),
            title: series.title.clone(),
            provider: CoreProviders::Webtoons.as_str().to_string(),
            result_id: series_id.0,
            media_type: None,
            language: None,
            nsfw: None,
        })
    }

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        let id = WebtoonsSeriesId(series_id.0.clone());
        let series_with_chapters = self
            .series_cache
            .get_or_load(series_id.clone(), || async move {
                self.client.get_series_with_chapters(&id).await
            })
            .await?;
        let thumbnail = if self.fetch_series_covers {
            self.client
                .get_series_thumbnail(&series_with_chapters)
                .await?
        } else {
            None
        };
        Ok(self
            .metadata_mapper
            .to_series_metadata(&series_with_chapters, thumbnail))
    }

    async fn get_series_cover(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError> {
        let id = WebtoonsSeriesId(series_id.0.clone());
        let series = self
            .series_cache
            .get_or_load(series_id.clone(), || async move {
                self.client.get_series(&id).await
            })
            .await?;
        self.client.get_series_thumbnail(&series).await
    }

    async fn get_book_metadata(
        &self,
        series_id: &ProviderSeriesId,
        book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError> {
        let id = WebtoonsSeriesId(series_id.0.clone());
        let chapters = self
            .chapter_cache
            .get_or_load(series_id.clone(), || async move {
                self.client.get_chapters(&id).await
            })
            .await?;
        let target = WebtoonsChapterId(book_id.0.clone());
        let (index, chapter) = chapters
            .iter()
            .enumerate()
            .find(|(_, c)| c.get_id() == target)
            .ok_or_else(|| ProviderError::message(format!("chapter not found: {}", book_id.0)))?;
        let thumbnail = if self.fetch_book_covers {
            Some(self.client.get_chapter_thumbnail(chapter).await?)
        } else {
            None
        };
        Ok(self
            .metadata_mapper
            .to_book_metadata(index, chapter, series_id.clone(), thumbnail))
    }

    async fn search_series(
        &self,
        series_name: &str,
        limit: usize,
        _media_type: Option<crate::model::MediaType>,
    ) -> Result<Vec<SeriesSearchResult>, ProviderError> {
        let truncated: String = series_name.chars().take(400).collect();
        let results = self.client.search_series(&truncated).await?;
        Ok(self
            .metadata_mapper
            .to_series_search_result(&results)
            .into_iter()
            .take(limit)
            .collect())
    }

    async fn match_series_metadata(
        &self,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        let truncated: String = match_query.search_name().chars().take(400).collect();
        let results = self.client.search_series(&truncated).await?;
        let search_results = self.metadata_mapper.to_series_search_result(&results);

        // Kotlin: matchSeriesMetadata 调用 searchSeries(seriesName.take(400))，
        // 接口默认 limit = 5，因此只在前 5 个搜索结果中匹配。
        let matched = search_results.into_iter().take(5).find(|it| {
            self.name_matcher.matches_single(
                &match_query.normalized_series_name(),
                &match_query.normalize_title(&it.title),
            )
        });
        let Some(matched) = matched else {
            return Ok(None);
        };
        let series_id = ProviderSeriesId(matched.result_id.clone());
        let id = WebtoonsSeriesId(matched.result_id);
        let series = self
            .series_cache
            .get_or_load(series_id, || async move {
                self.client.get_series_with_chapters(&id).await
            })
            .await?;
        let thumbnail = if self.fetch_series_covers {
            self.client.get_series_thumbnail(&series).await?
        } else {
            None
        };
        Ok(Some(
            self.metadata_mapper.to_series_metadata(&series, thumbnail),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seo_encoding_matches_vendor_page() {
        assert_eq!(seo_encoding(""), "_");
        assert_eq!(seo_encoding("Tower of God"), "tower-of-god");
        assert_eq!(seo_encoding("  Leading Spaces"), "leading-spaces");
        assert_eq!(seo_encoding("A&#39;B"), "ab");
        assert_eq!(seo_encoding("One_Piece"), "one-piece");
        assert_eq!(seo_encoding("-leading-dash"), "leading-dash");
        assert_eq!(seo_encoding("Star Wars: Episode"), "star-wars-episode");
    }

    #[test]
    fn title_urls_match_kotlin() {
        let title = Title {
            title_no: 10101,
            title: "Tower of God".to_string(),
            title_group_name: None,
            represent_genre: Some("fantasy".to_string()),
            thumbnail_mobile: "/thumb.jpg".to_string(),
            unsuitable_for_children: false,
            picture_author_name: None,
            writing_author_name: "Author".to_string(),
            last_episode_register_ymdt: 0,
            read_count: 0,
        };
        assert_eq!(
            title.get_original_url(WebtoonsLanguage::En).as_deref(),
            Some("https://www.webtoons.com/en/fantasy/tower-of-god/list?title_no=10101")
        );
        assert_eq!(
            title.get_canvas_url(WebtoonsLanguage::En),
            "https://www.webtoons.com/en/canvas/tower-of-god/list?title_no=10101"
        );
        assert_eq!(
            title.get_original_url(WebtoonsLanguage::ZhHant).as_deref(),
            Some("https://www.webtoons.com/zh-hant/fantasy/tower-of-god/list?title_no=10101")
        );
        assert_eq!(
            title.get_canvas_url(WebtoonsLanguage::ZhHant),
            "https://www.webtoons.com/zh-hant/canvas/tower-of-god/list?title_no=10101"
        );
        // representGenre 空 -> original null
        let mut title2 = title.clone();
        title2.represent_genre = Some(String::new());
        assert_eq!(title2.get_original_url(WebtoonsLanguage::En), None);
    }

    #[test]
    fn remove_type_query_param() {
        assert_eq!(
            remove_query_param("https://a.com/img?type=w540&other=1", "type"),
            "https://a.com/img?other=1"
        );
        assert_eq!(
            remove_query_param("https://a.com/img?type=w540", "type"),
            "https://a.com/img"
        );
    }

    #[test]
    fn status_mapping() {
        let map = status_from_text;
        assert_eq!(map("MON, UPDATED EVERY MON"), Status::Ongoing);
        assert_eq!(map("更新 在週二更新"), Status::Ongoing);
        assert_eq!(map("完結"), Status::Completed);
        assert_eq!(map("COMPLETED"), Status::Completed);
        assert_eq!(map("END"), Status::Completed);
        assert_eq!(map("something else"), Status::Unknown);
    }

    #[test]
    fn episode_id_from_url() {
        let episode = Episode {
            episode_no: 10,
            thumbnail: "/thumb.jpg".to_string(),
            episode_title: "Ch 10".to_string(),
            viewer_link: "/en/fantasy/tower-of-god/10?title_no=10101".to_string(),
            exposure_date_millis: 0,
            display_up: true,
            has_bgm: false,
        };
        assert_eq!(
            episode.get_id().0,
            "/en/fantasy/tower-of-god/10?title_no=10101"
        );
    }

    /// 台湾版（zh-hant）页面结构，取自 www.webtoons.com/zh-hant 实际页面并精简。
    const ZH_HANT_ONGOING: &str = r#"<html><head>
<meta property="og:url" content="https://www.webtoons.com/zh-hant/fantasy/abundance-goddess-genius-wretched-apostle/list?title_no=9349" />
<meta property="og:image" content="https://webtoon-phinf.pstatic.net/thumb.jpg?type=crop540_540" />
</head><body><div id="wrap">
<div class="detail_header type_white"><div class="info">
<h2 class="genre g_fantasy">奇幻冒險</h2>
<h1 class="subj">豐饒女神與天才使徒</h1>
<div class="author_area"> Saint Nine , gomsanta <button type="button" class="ico_info2 _btnAuthorInfo">作家資訊</button></div>
</div></div>
<div id="_asideDetail">
<p class="day_info"><span class="txt_ico_up">更新</span>在週二更新</p>
<p class="summary">地球毀滅後，需要仰賴回歸者從異世界取回的生命力。</p>
</div>
<div class="ly_wrap _authorInfoLayer"><div class="ly_box ly_creator"><div class="ly_creator_in _authorInnerContent">
<div class="ly_creator_item"><p class="by">作家</p><h3 class="title"> Saint Nine </h3><p class="desc"></p></div>
<div class="ly_creator_item"><p class="by">原作</p><h3 class="title"> gomsanta </h3><p class="desc"></p></div>
</div></div></div>
</div></body></html>"#;

    const ZH_HANT_COMPLETED: &str = r#"<html><head>
<meta property="og:url" content="https://www.webtoons.com/zh-hant/romance-m/for-daphne/list?title_no=9342" />
</head><body><div id="wrap">
<div class="detail_header"><div class="info"><h2 class="genre">愛情</h2><h1 class="subj">女神們的故事：獻給達芙妮</h1></div></div>
<div id="_asideDetail"><p class="day_info"><span class="txt_ico_completed"></span>完結</p><p class="summary">x</p></div>
<div class="ly_wrap _authorInfoLayer"><div class="ly_creator_in _authorInnerContent">
<div><p class="by">原作</p><h3 class="title">3_jokseom</h3></div>
<div><p class="by">編劇</p><h3 class="title">Dan Geung</h3></div>
<div><p class="by">繪圖</p><h3 class="title">KKAM</h3></div>
</div></div>
</div></body></html>"#;

    #[test]
    fn parse_zh_hant_ongoing_series() {
        let series = WebtoonsParser.parse_series(ZH_HANT_ONGOING);
        assert_eq!(series.title, "豐饒女神與天才使徒");
        assert_eq!(series.status, Status::Ongoing);
        assert_eq!(series.genres, vec!["奇幻冒險".to_string()]);
        assert_eq!(
            series.author.as_ref().map(|p| p.name.as_str()),
            Some("gomsanta")
        );
        assert_eq!(
            series.artist.as_ref().map(|p| p.name.as_str()),
            Some("Saint Nine")
        );
        assert!(series.adapted_by.is_none());
        assert_eq!(
            series.id.0,
            "/zh-hant/fantasy/abundance-goddess-genius-wretched-apostle/list?title_no=9349"
        );

        let mapper = WebtoonsMetadataMapper::new(
            Default::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            WebtoonsLanguage::En,
        );
        let metadata = mapper.to_series_metadata(&series, None).metadata;
        // 配置为 en，但作品网址是 zh-hant → 语言以网址为准
        assert_eq!(metadata.language.as_deref(), Some("zh-Hant"));
        assert_eq!(metadata.status, Some(SeriesStatus::Ongoing));
    }

    #[test]
    fn parse_zh_hant_completed_series_with_adapter() {
        let series = WebtoonsParser.parse_series(ZH_HANT_COMPLETED);
        assert_eq!(series.status, Status::Completed);
        assert_eq!(
            series.author.as_ref().map(|p| p.name.as_str()),
            Some("3_jokseom")
        );
        assert_eq!(
            series.adapted_by.as_ref().map(|p| p.name.as_str()),
            Some("Dan Geung")
        );
        assert_eq!(
            series.artist.as_ref().map(|p| p.name.as_str()),
            Some("KKAM")
        );
    }

    #[test]
    fn language_from_url_path() {
        assert_eq!(
            WebtoonsLanguage::from_url_path("/zh-hant/fantasy/x/list?title_no=1"),
            Some(WebtoonsLanguage::ZhHant)
        );
        assert_eq!(
            WebtoonsLanguage::from_url_path("https://www.webtoons.com/en/canvas/x/list?title_no=1"),
            Some(WebtoonsLanguage::En)
        );
        assert_eq!(
            WebtoonsLanguage::from_url_path("/episodeList?titleNo=1"),
            None
        );
    }

    /// 实网联调（默认忽略）：`cargo test -p komf-core webtoons_zh_hant_live -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn webtoons_zh_hant_live() {
        let client = WebtoonsClient::new(reqwest::Client::new(), WebtoonsLanguage::ZhHant);
        let mapper = WebtoonsMetadataMapper::new(
            Default::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            WebtoonsLanguage::ZhHant,
        );
        let results = mapper.to_series_search_result(&client.search_series("女神").await.unwrap());
        assert!(!results.is_empty());
        for r in results.iter().take(3) {
            println!("{} | {}", r.title, r.result_id);
        }
        let series = client
            .get_series_with_chapters(&WebtoonsSeriesId(results[0].result_id.clone()))
            .await
            .unwrap();
        let metadata = mapper.to_series_metadata(&series, None).metadata;
        println!(
            "title={:?} lang={:?} status={:?} authors={:?} genres={:?} chapters={}",
            metadata.title.map(|t| t.name),
            metadata.language,
            metadata.status,
            metadata
                .authors
                .iter()
                .map(|a| format!("{}/{:?}", a.name, a.role))
                .collect::<Vec<_>>(),
            metadata.genres,
            series.chapters.as_ref().map(|c| c.len()).unwrap_or(0)
        );
        assert!(results[0].result_id.starts_with("/zh-hant/"));
        assert_eq!(metadata.language.as_deref(), Some("zh-Hant"));
        assert!(metadata.status.is_some());
        assert!(!metadata.authors.is_empty());
    }
}
