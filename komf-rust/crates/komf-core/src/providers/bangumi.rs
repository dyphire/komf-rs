//! Bangumi provider
//!
use crate::config::ProviderConfig;
use crate::model::{
    Author, AuthorRole, BookMetadata, Image, MatchQuery, ProviderBookId, ProviderBookMetadata,
    ProviderSeriesId, ProviderSeriesMetadata, Publisher, PublisherType, SeriesBook, SeriesMetadata,
    SeriesSearchResult, SeriesStatus, SeriesTitle, TitleType, WebLink,
};
use crate::providers::{CoreProviders, MetadataProvider, ProviderError};
use crate::util::NameSimilarityMatcher;
use serde::Deserialize;

const BASE_URL: &str = "https://api.bgm.tv";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiSearchResponse {
    pub data: Vec<BangumiSubject>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiSubject {
    pub id: u64,
    pub name: String,
    #[serde(default, rename = "name_cn")]
    pub name_cn: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    pub summary: Option<String>,
    pub date: Option<String>,
    pub images: Option<BangumiImages>,
    pub rating: Option<BangumiRating>,
    pub rank: Option<i32>,
    #[serde(rename = "type")]
    pub subject_type: Option<i32>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub tags: Vec<BangumiTag>,
    pub volumes: Option<i32>,
    pub eps: Option<i32>,
    #[serde(default, rename = "total_episodes")]
    pub total_episodes: Option<i32>,
    #[serde(default)]
    pub infobox: Vec<BangumiInfoBoxItem>,
    #[serde(default)]
    pub eps_info: Option<Vec<BangumiEpisode>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiImages {
    pub large: Option<String>,
    pub common: Option<String>,
    pub medium: Option<String>,
    pub small: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiRating {
    pub score: Option<f64>,
    pub total: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiTag {
    pub name: String,
    pub count: i32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiInfoBoxItem {
    pub key: Option<String>,
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiEpisode {
    pub id: u64,
    pub name: Option<String>,
    pub name_cn: Option<String>,
    pub sort: Option<f64>,
    pub ep: Option<f64>,
}

/// 对应 Kotlin `SubjectRelation`（GET /v0/subjects/{id}/subjects，snake_case JSON）。
#[derive(Debug, Clone, Deserialize)]
pub struct BangumiSubjectRelation {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub name_cn: Option<String>,
    #[serde(default)]
    pub r#type: Option<i32>,
    #[serde(default)]
    pub relation: Option<String>,
}

pub struct BangumiClient {
    http: reqwest::Client,
}

impl BangumiClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    pub async fn search(
        &self,
        name: &str,
        _limit: u32,
    ) -> Result<Vec<BangumiSubject>, ProviderError> {
        // POST /v0/search/subjects，JSON body
        // { "keyword": ..., "sort": "match", "filter": { "type": [1], "nsfw": true } }
        let body = serde_json::json!({
            "keyword": name,
            "sort": "match",
            "filter": { "type": [1], "nsfw": true }
        });
        let response = self
            .http
            .post(format!("{BASE_URL}/v0/search/subjects"))
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::Bangumi, status, body));
        }
        let body: BangumiSearchResponse = response.json().await?;
        Ok(body.data)
    }

    pub async fn get(&self, id: u64) -> Result<BangumiSubject, ProviderError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/v0/subjects/{id}"))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::Bangumi, status, body));
        }
        Ok(response.json().await?)
    }

    /// 对应 Kotlin `getSubjectRelations`：GET /v0/subjects/{id}/subjects
    pub async fn get_subject_relations(
        &self,
        id: u64,
    ) -> Result<Vec<BangumiSubjectRelation>, ProviderError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/v0/subjects/{id}/subjects"))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::Bangumi, status, body));
        }
        Ok(response.json().await?)
    }

    pub async fn get_thumbnail(
        &self,
        subject: &BangumiSubject,
        min_size: u64,
        max_size: Option<u64>,
    ) -> Result<Option<Image>, ProviderError> {
        let mut candidates: Vec<String> = Vec::new();
        if let Some(url) = subject.image.clone().filter(|u| !u.is_empty()) {
            candidates.push(url);
        }
        if let Some(images) = &subject.images {
            for url in [&images.large, &images.medium, &images.common] {
                if let Some(url) = url.clone().filter(|u| !u.is_empty()) {
                    if !candidates.contains(&url) {
                        candidates.push(url);
                    }
                }
            }
        }
        for url in candidates {
            let Ok(response) = self.http.get(&url).send().await else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            let Ok(bytes) = response.bytes().await else {
                continue;
            };
            let size = bytes.len() as u64;
            if size == 0 || size < min_size {
                continue;
            }
            if let Some(max) = max_size {
                if size >= max {
                    continue;
                }
            }
            return Ok(Some(Image::new(bytes.to_vec(), None)));
        }
        Ok(None)
    }
}

pub struct BangumiMetadataMapper {
    series_metadata_config: crate::config::SeriesMetadataConfig,
    #[allow(dead_code)]
    book_metadata_config: crate::config::BookMetadataConfig,
    author_roles: Vec<AuthorRole>,
    artist_roles: Vec<AuthorRole>,
    tag_whitelist: Vec<String>,
}

impl BangumiMetadataMapper {
    pub fn new(
        series_metadata_config: crate::config::SeriesMetadataConfig,
        book_metadata_config: crate::config::BookMetadataConfig,
        author_roles: Vec<AuthorRole>,
        artist_roles: Vec<AuthorRole>,
        tag_whitelist: Vec<String>,
    ) -> Self {
        Self {
            series_metadata_config,
            book_metadata_config,
            author_roles,
            artist_roles,
            tag_whitelist,
        }
    }

    pub fn to_series_metadata(
        &self,
        subject: &BangumiSubject,
        book_relations: &[BangumiSubjectRelation],
        thumbnail: Option<Image>,
    ) -> ProviderSeriesMetadata {
        let cfg = &self.series_metadata_config;

        // Kotlin: infoBox = subject.infobox?.associate { it.key to it }
        let info_box: std::collections::HashMap<&str, &serde_json::Value> = subject
            .infobox
            .iter()
            .filter_map(|item| {
                item.key
                    .as_deref()
                    .map(|k| (k, item.value.as_ref().unwrap_or(&serde_json::Value::Null)))
            })
            .collect();

        let raw_total = subject.volumes.or(subject.eps).or(subject.total_episodes);
        let total_book_count = raw_total.filter(|c| *c > 0);

        let status = cfg
            .status
            .then(|| {
                let status_val = ["状态", "连载状态", "刊行状态"]
                    .iter()
                    .find_map(|k| info_box.get(*k).and_then(|v| v.as_str()));
                let mut status = status_val.and_then(classify_bangumi_status).or_else(|| {
                    subject
                        .tags
                        .iter()
                        .map(|t| t.name.as_str())
                        .find(|n| BANGUMI_STATUS_TAGS.contains(n))
                        .and_then(classify_bangumi_status)
                });
                let ended = info_box.contains_key("结束")
                    || info_box.contains_key("完结")
                    || raw_total.map(|c| c > 0).unwrap_or(false);
                if ended {
                    status = Some(SeriesStatus::Ended);
                }
                status
            })
            .flatten();

        // 白名单（内置+自定义）+ 非 statusTags → count 降序 → 动态阈值（3~35）→ 不足 10 补前 10
        let mut tags: Vec<String> = if cfg.tags {
            let raw: Vec<(String, i32)> = subject
                .tags
                .iter()
                .map(|t| (t.name.clone(), t.count))
                .collect();
            filter_bangumi_tags(&raw, &self.tag_whitelist)
        } else {
            Vec::new()
        };

        if cfg.score {
            if let Some(score) = subject
                .rating
                .as_ref()
                .and_then(|r| r.score)
                .filter(|s| *s > 0.0)
            {
                tags.push(format!("score:{}", score.round() as i64));
            }
        }

        // titles: name→Native（日文原名），nameCn→null/zh（保持 Kotlin），别名→Localized
        let mut titles: Vec<SeriesTitle> = Vec::new();
        if cfg.title {
            titles.push(SeriesTitle {
                name: subject.name.clone(),
                r#type: Some(TitleType::Native),
                language: None,
            });
            if let Some(name_cn) = &subject.name_cn {
                if !name_cn.is_empty() {
                    titles.push(SeriesTitle {
                        name: name_cn.clone(),
                        r#type: None,
                        language: Some("zh".into()),
                    });
                }
            }

            let mut aliases: Vec<(String, Option<String>)> = Vec::new();
            let mut alias_seen = std::collections::HashSet::new();
            for item in &subject.infobox {
                let is_alias_key =
                    item.key.as_deref() == Some("别名") || item.key.as_deref() == Some("別名");
                if let Some(value) = item.value.as_ref() {
                    if is_alias_key {
                        collect_alias_entries(value, &mut aliases, &mut alias_seen, true);
                    }
                    if let serde_json::Value::Array(sub_items) = value {
                        for sub in sub_items {
                            let sub_key = sub.get("k").and_then(|k| k.as_str());
                            if sub_key == Some("别名") || sub_key == Some("別名") {
                                if let Some(v) = sub.get("v") {
                                    collect_alias_entries(v, &mut aliases, &mut alias_seen, false);
                                }
                            }
                        }
                    }
                }
            }
            for (name, language) in aliases {
                titles.push(SeriesTitle {
                    name,
                    r#type: Some(TitleType::Localized),
                    language,
                });
            }
        }

        let title = None;

        let release_date = cfg
            .release_date
            .then(|| subject.date.as_deref().and_then(parse_bangumi_date))
            .flatten();

        let authors = if cfg.authors {
            extract_authors(subject, &self.author_roles, &self.artist_roles)
        } else {
            Vec::new()
        };

        // Kotlin: 出版社 → ORIGINAL；其他出版社 → LOCALIZED；alternativePublishers = drop(1) + other
        let mut publishers: Vec<Publisher> = Vec::new();
        let mut other_publishers: Vec<Publisher> = Vec::new();
        if cfg.publisher {
            for (key, kind) in [
                ("出版社", PublisherType::Original),
                ("其他出版社", PublisherType::Localized),
            ] {
                let target = if kind == PublisherType::Original {
                    &mut publishers
                } else {
                    &mut other_publishers
                };
                if let Some(value) = info_box.get(key).and_then(|v| v.as_str()) {
                    for name in value.split(['，', '、', ',']) {
                        let name = name.trim();
                        if !name.is_empty() {
                            target.push(Publisher {
                                name: name.to_string(),
                                r#type: Some(kind),
                                language_tag: None,
                            });
                        }
                    }
                }
            }
        }
        let publisher = publishers.first().cloned();
        // Kotlin: altPublishers = (publishers.drop(1) + otherPublishers).toSet() —— 保序去重
        let mut alternative_publishers: Vec<Publisher> = publishers
            .iter()
            .skip(1)
            .cloned()
            .chain(other_publishers)
            .collect();
        {
            let mut seen: Vec<(String, Option<PublisherType>)> = Vec::new();
            alternative_publishers.retain(|p| {
                let key = (p.name.clone(), p.r#type);
                if seen.contains(&key) {
                    false
                } else {
                    seen.push(key);
                    true
                }
            });
        }

        let metadata = SeriesMetadata {
            status,
            title,
            titles,
            summary: cfg
                .summary
                .then_some(subject.summary.clone())
                .flatten()
                .map(|s| trim_indent(&s)),
            publisher,
            alternative_publishers,
            reading_direction: None,
            age_rating: None,
            language: None,
            genres: Vec::new(),
            tags,
            total_book_count,
            authors,
            release_date,
            links: if cfg.links {
                vec![WebLink {
                    label: "Bangumi".to_string(),
                    url: format!("https://bgm.tv/subject/{}", subject.id),
                }]
            } else {
                Vec::new()
            },
            score: cfg
                .score
                .then_some(subject.rating.as_ref().and_then(|r| r.score))
                .flatten(),
            thumbnail,
        };

        // Kotlin books：bookRelations（provider 已过滤 type==BOOK && relation=="单行本"）
        let books: Vec<SeriesBook> = if cfg.books {
            book_relations
                .iter()
                .map(|rel| SeriesBook {
                    id: ProviderBookId(rel.id.to_string()),
                    number: get_book_number(&rel.name),
                    name: Some(rel.name.clone()),
                    r#type: None,
                    edition: None,
                })
                .collect()
        } else {
            Vec::new()
        };

        ProviderSeriesMetadata {
            id: ProviderSeriesId(subject.id.to_string()),
            metadata,
            books,
        }
    }

    pub fn to_series_search_result(&self, subject: &BangumiSubject) -> SeriesSearchResult {
        // Kotlin: title = nameCn.ifBlank { name } ?: name（优先中文名）；imageUrl = 顶层 image
        let title = subject
            .name_cn
            .clone()
            .filter(|cn| !cn.is_empty())
            .or_else(|| Some(subject.name.clone()))
            .unwrap_or_else(|| subject.name.clone());
        SeriesSearchResult {
            url: Some(format!("https://bgm.tv/subject/{}", subject.id)),
            image_url: subject.image.clone().filter(|u| !u.is_empty()),
            title,
            provider: CoreProviders::Bangumi.as_str().to_string(),
            result_id: subject.id.to_string(),
            media_type: platform_media_type(subject.platform.as_deref()),
            language: None,
        }
    }

    /// 对应 Kotlin `toBookMetadata`（含 BookMetadataConfig apply 语义）。
    pub fn to_book_metadata(
        &self,
        book: &BangumiSubject,
        thumbnail: Option<Image>,
    ) -> ProviderBookMetadata {
        let cfg = &self.book_metadata_config;

        let info_box: std::collections::HashMap<&str, &serde_json::Value> = book
            .infobox
            .iter()
            .filter_map(|item| {
                item.key
                    .as_deref()
                    .map(|k| (k, item.value.as_ref().unwrap_or(&serde_json::Value::Null)))
            })
            .collect();

        // Kotlin: tags = sortedByDescending(count).take(15).filter(count>1).map(name).toSet()
        let mut ranked: Vec<(i32, String)> = book
            .tags
            .iter()
            .map(|t| (t.count, t.name.clone()))
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0));
        ranked.truncate(15);
        ranked.retain(|(count, _)| *count > 1);
        let tags: Vec<String> = ranked.into_iter().map(|(_, name)| name).collect();

        let book_number = get_book_number(&book.name);

        // Kotlin: ISBN-13 优先，否则 ISBN-10 → isbn10ToIsbn13
        let isbn: Option<String> = info_box
            .get("ISBN-13")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                info_box
                    .get("ISBN")
                    .and_then(|v| v.as_str())
                    .and_then(isbn10_to_isbn13)
            });

        // Kotlin: releaseDate = book.date?.let { LocalDate.parse(it) } —— date 即 "YYYY-MM-DD"
        let release_date = book.date.clone();

        let metadata = BookMetadata {
            title: cfg.title.then_some(book.name.clone()),
            summary: cfg
                .summary
                .then_some(book.summary.clone())
                .flatten()
                .map(|s| trim_indent(&s)),
            number: cfg.number.then_some(book_number.clone()).flatten(),
            number_sort: cfg
                .number_sort
                .then_some(book_number.clone())
                .flatten()
                .map(|n| n.start),
            release_date: cfg.release_date.then_some(release_date.clone()).flatten(),
            authors: if cfg.authors {
                extract_authors(book, &self.author_roles, &self.artist_roles)
            } else {
                Vec::new()
            },
            tags: if cfg.tags { tags } else { Vec::new() },
            isbn: cfg.isbn.then_some(isbn.clone()).flatten(),
            links: if cfg.links {
                vec![WebLink {
                    label: "Bangumi".to_string(),
                    url: format!("https://bgm.tv/subject/{}", book.id),
                }]
            } else {
                Vec::new()
            },
            chapters: Vec::new(),
            story_arcs: None,
            thumbnail,
            start_chapter: None,
            end_chapter: None,
        };

        ProviderBookMetadata {
            id: Some(ProviderBookId(book.id.to_string())),
            series_id: None,
            metadata,
        }
    }
}

fn collect_alias_entries(
    value: &serde_json::Value,
    out: &mut Vec<(String, Option<String>)>,
    seen: &mut std::collections::HashSet<String>,
    with_language: bool,
) {
    fn push_alias(
        name: &str,
        language: Option<String>,
        out: &mut Vec<(String, Option<String>)>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        let name = name.trim();
        if !name.is_empty() && seen.insert(name.to_string()) {
            out.push((name.to_string(), language));
        }
    }
    match value {
        serde_json::Value::String(s) => push_alias(s, None, out, seen),
        serde_json::Value::Array(items) => {
            for item in items {
                match item {
                    serde_json::Value::String(s) => push_alias(s, None, out, seen),
                    obj => {
                        let name = obj.get("v").and_then(|v| v.as_str()).unwrap_or("");
                        let language = if with_language {
                            obj.get("k").and_then(|k| k.as_str()).map(|k| {
                                if k == "hk" {
                                    "zh-hk".to_string()
                                } else {
                                    k.to_string()
                                }
                            })
                        } else {
                            None
                        };
                        push_alias(name, language, out, seen);
                    }
                }
            }
        }
        obj => {
            let name = obj.get("v").and_then(|v| v.as_str()).unwrap_or("");
            let language = if with_language {
                obj.get("k").and_then(|k| k.as_str()).map(|k| {
                    if k == "hk" {
                        "zh-hk".to_string()
                    } else {
                        k.to_string()
                    }
                })
            } else {
                None
            };
            push_alias(name, language, out, seen);
        }
    }
}

fn get_book_number(name: &str) -> Option<crate::model::BookRange> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\(([^)]*)\)[^(]*$").unwrap());
    re.captures(name)?
        .get(1)?
        .as_str()
        .trim()
        .parse::<f64>()
        .ok()
        .map(crate::model::BookRange::single)
}

/// Kotlin `isbn10ToIsbn13`：10 位 ISBN 转 13 位（"978" + 前 9 位 + 校验位）。
fn isbn10_to_isbn13(isbn10: &str) -> Option<String> {
    let stripped = isbn10.replace('-', "");
    if stripped.len() == 13 {
        return Some(stripped);
    }
    if stripped.len() != 10 {
        return None;
    }
    let intermediate = format!("978{}", &stripped[..9]);
    let mut sum: u32 = 0;
    for (index, ch) in intermediate.chars().enumerate() {
        let d = if index % 2 == 0 { 1u32 } else { 3u32 };
        sum += ch.to_digit(10)? * d;
    }
    let mod10 = sum % 10;
    let check_digit = if mod10 == 0 { 0 } else { 10 - mod10 };
    Some(format!("{intermediate}{check_digit}"))
}

/// Kotlin `String.trimIndent()` 的近似：删除非空行的公共最小前导空白。
fn trim_indent(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let min_indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| {
            let skip = l.len().min(min_indent);
            &l[skip..]
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn parse_bangumi_date(date: &str) -> Option<crate::model::ReleaseDate> {
    let normalized = normalize_bangumi_date(date)?;
    let parts: Vec<&str> = normalized.split('-').collect();
    let year = parts.first().and_then(|p| p.parse::<i32>().ok());
    let month = parts.get(1).and_then(|p| p.parse::<u32>().ok());
    let day = parts.get(2).and_then(|p| p.parse::<u32>().ok());
    Some(crate::model::ReleaseDate::new(year, month, day))
}

fn normalize_bangumi_date(date: &str) -> Option<String> {
    let date = date.trim();
    if date.is_empty() {
        return None;
    }
    let pad = |v: &str| -> String { format!("{:0>2}", v) };

    if let Some(caps) = regex_capture(date, r"^(\d{4})年(\d{1,2})月(\d{1,2})日$") {
        return Some(format!("{}-{}-{}", caps[0], pad(&caps[1]), pad(&caps[2])));
    }
    if let Some(caps) = regex_capture(date, r"^(\d{4})年(\d{1,2})月$") {
        return Some(format!("{}-{}-01", caps[0], pad(&caps[1])));
    }
    if let Some(caps) = regex_capture(date, r"^(\d{4})-(\d{1,2})-(\d{1,2})$") {
        return Some(format!("{}-{}-{}", caps[0], pad(&caps[1]), pad(&caps[2])));
    }
    if let Some(caps) = regex_capture(date, r"^(\d{4})-(\d{1,2})$") {
        return Some(format!("{}-{}-01", caps[0], pad(&caps[1])));
    }
    None
}

fn regex_capture(text: &str, pattern: &str) -> Option<Vec<String>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, regex::Regex>>,
    > = std::sync::OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    let re = cache
        .entry(pattern.to_string())
        .or_insert_with(|| regex::Regex::new(pattern).unwrap());
    let caps = re.captures(text)?;
    Some(
        (1..caps.len())
            .filter_map(|i| caps.get(i).map(|m| m.as_str().to_string()))
            .collect(),
    )
}

fn classify_bangumi_status(value: &str) -> Option<SeriesStatus> {
    let s = value.to_lowercase();
    if s.contains("休刊") || s.contains("停刊") || s.contains("停止连载") || s.contains("长期休载")
    {
        Some(SeriesStatus::Hiatus)
    } else if s.contains("连载中") || s.contains("连载") {
        Some(SeriesStatus::Ongoing)
    } else if s.contains("完结") {
        Some(SeriesStatus::Ended)
    } else {
        None
    }
}

const BANGUMI_STATUS_TAGS: [&str; 8] = [
    "连载",
    "连载中",
    "完结",
    "已完结",
    "停刊",
    "长期休载",
    "停止连载",
    "休刊",
];

/// 内置标签白名单（按英文逗号拆分）
/// 内置 bangumi 标签白名单 json 资源（数组形式；由 BANGUMI_TAG_WHITELIST 字符串 json 化而来，
/// 修复了历史 "旅行，异世界" 中文逗号问题）。可通过 tagWhitelistFile 配置覆盖。
const BANGUMI_TAG_WHITELIST_JSON: &str = include_str!("bangumi_tag_whitelist.json");

/// 加载标签白名单：tagWhitelistFile 指定路径优先；否则内置资源。解析失败回退空列表。
fn load_bangumi_tag_whitelist(file: Option<&str>) -> Vec<String> {
    if let Some(path) = file {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(list) = serde_json::from_str::<Vec<String>>(&text) {
                return list;
            }
        }
    }
    serde_json::from_str(BANGUMI_TAG_WHITELIST_JSON).unwrap_or_default()
}

/// 嵌套别名收集：
/// infobox 任意项的 value 数组内 k=="别名" 的子项（如「版本:*」条目），无语言。
#[test]
fn nested_aliases_collected_like_js() {
    let json = r#"{
            "id": 1902,
            "name": "3月のライオン",
            "name_cn": "3月的狮子",
            "summary": null,
            "tags": [],
            "infobox": [
                {"key": "别名", "value": [{"v": "March comes in like a lion"}]},
                {"key": "版本:次元书馆版", "value": [
                    {"k": "版本名", "v": "3月的狮子"},
                    {"k": "别名", "v": "三月的狮子"}
                ]},
                {"key": "版本:尖端版", "value": [
                    {"k": "别名", "v": "三月的獅子"}
                ]},
                {"key": "版本:玉皇朝版", "value": [
                    {"k": "别名", "v": "三月的獅子"}
                ]}
            ]
        }"#;
    let subject: BangumiSubject = serde_json::from_str(json).unwrap();
    let mapper = BangumiMetadataMapper::new(
        crate::config::SeriesMetadataConfig::default(),
        crate::config::BookMetadataConfig::default(),
        vec![],
        vec![],
        vec![],
    );
    let md = mapper.to_series_metadata(&subject, &[], None);
    let localized: Vec<&str> = md
        .metadata
        .titles
        .iter()
        .filter(|t| t.r#type == Some(TitleType::Localized))
        .map(|t| t.name.as_str())
        .collect();
    // 直接别名 1 条 + 嵌套去重后 2 条（三月的狮子 / 三月的獅子）
    assert_eq!(
        localized,
        vec!["March comes in like a lion", "三月的狮子", "三月的獅子"]
    );
}

/// 评分标签：score 配置开启且评分>0 时追加 "score:N"（Math.round）
#[test]
fn score_tag_appended_when_enabled() {
    let json = r#"{
            "id": 1902,
            "name": "3月のライオン",
            "name_cn": "3月的狮子",
            "summary": null,
            "tags": [{"name": "治愈", "count": 500}],
            "rating": {"score": 9.1, "total": 1234},
            "infobox": []
        }"#;
    let subject: BangumiSubject = serde_json::from_str(json).unwrap();
    let mut cfg = crate::config::SeriesMetadataConfig::default();
    cfg.score = true;
    cfg.tags = true;
    let mapper = BangumiMetadataMapper::new(
        cfg,
        crate::config::BookMetadataConfig::default(),
        vec![],
        vec![],
        vec![],
    );
    let md = mapper.to_series_metadata(&subject, &[], None);
    assert!(md.metadata.tags.contains(&"score:9".to_string()));
    assert_eq!(md.metadata.score, Some(9.1));
    // score=0 时不追加
    let json0 = r#"{"id":1,"name":"x","name_cn":null,"summary":null,"tags":[],"rating":{"score":0.0},"infobox":[]}"#;
    let subject0: BangumiSubject = serde_json::from_str(json0).unwrap();
    let md0 = mapper.to_series_metadata(&subject0, &[], None);
    assert!(!md0.metadata.tags.iter().any(|t| t.starts_with("score:")));
    // score 关闭时不追加
    let mut cfg_off = crate::config::SeriesMetadataConfig::default();
    cfg_off.score = false;
    let mapper_off = BangumiMetadataMapper::new(
        cfg_off,
        crate::config::BookMetadataConfig::default(),
        vec![],
        vec![],
        vec![],
    );
    let md_off = mapper_off.to_series_metadata(&subject, &[], None);
    assert!(!md_off.metadata.tags.iter().any(|t| t.starts_with("score:")));
}

/// 标签处理：
/// 白名单（内置 + 自定义补充）∩ 非 statusTags → 按 count 降序 → 动态阈值
/// （max>200→35 / >125→25 / >60→15 / >30→10 / >10→5 / 否则 3）→ count ≥ 阈值；
/// 结果不足 10 个时替换为排序后前 10。返回标签名列表（保持 count 降序）。
/// 注意：白名单命中用精确匹配（JS 的 `tagLabels.includes(name + ',')` 子串判断
/// 会让单字标签如 "空"/"生" 误入，此处有意改进为精确匹配）。
fn filter_bangumi_tags(tags: &[(String, i32)], whitelist: &[String]) -> Vec<String> {
    let whitelist: std::collections::HashSet<&str> = whitelist.iter().map(String::as_str).collect();

    let mut valid: Vec<(String, i32)> = tags
        .iter()
        .filter(|(name, _)| {
            whitelist.contains(name.as_str()) && !BANGUMI_STATUS_TAGS.contains(&name.as_str())
        })
        .map(|(name, count)| (name.clone(), *count))
        .collect();
    if valid.is_empty() {
        return Vec::new();
    }
    valid.sort_by(|a, b| b.1.cmp(&a.1));
    let max = valid[0].1.max(1);
    let threshold = if max > 200 {
        35
    } else if max > 125 {
        25
    } else if max > 60 {
        15
    } else if max > 30 {
        10
    } else if max > 10 {
        5
    } else {
        3
    };
    let mut final_tags: Vec<(String, i32)> = valid
        .iter()
        .filter(|(_, count)| *count >= threshold)
        .cloned()
        .collect();
    if final_tags.len() < 10 {
        final_tags = valid.into_iter().take(10).collect();
    }
    final_tags.into_iter().map(|(name, _)| name).collect()
}

/// platform（Bangumi subject.platform 字段）→ MediaType（搜索结果显示用）。
fn platform_media_type(platform: Option<&str>) -> Option<crate::model::MediaType> {
    match platform {
        Some("漫画") => Some(crate::model::MediaType::Manga),
        Some("小说") => Some(crate::model::MediaType::Novel),
        _ => None,
    }
}

fn media_type_platform(media_type: crate::model::MediaType) -> Option<&'static str> {
    match media_type {
        crate::model::MediaType::Manga => Some("漫画"),
        crate::model::MediaType::Novel => Some("小说"),
        crate::model::MediaType::Comic | crate::model::MediaType::Webtoon => None,
    }
}

fn extract_authors(
    subject: &BangumiSubject,
    author_roles: &[AuthorRole],
    artist_roles: &[AuthorRole],
) -> Vec<Author> {
    let info_box: std::collections::HashMap<&str, &serde_json::Value> = subject
        .infobox
        .iter()
        .filter_map(|item| {
            item.key
                .as_deref()
                .map(|k| (k, item.value.as_ref().unwrap_or(&serde_json::Value::Null)))
        })
        .collect();

    let single = |key: &str| -> Option<String> {
        info_box
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    let mut authors: Vec<Author> = Vec::new();
    let push_cleaned = |authors: &mut Vec<Author>, name: &str, role: AuthorRole| {
        for cleaned in clean_author_names(name) {
            if !authors.iter().any(|a| a.name == cleaned && a.role == role) {
                authors.push(Author {
                    name: cleaned,
                    role,
                });
            }
        }
    };
    if let Some(name) = single("作者") {
        for role in author_roles.iter().chain(artist_roles.iter()) {
            push_cleaned(&mut authors, &name, *role);
        }
    }
    if let Some(name) = single("原作") {
        for role in author_roles {
            push_cleaned(&mut authors, &name, *role);
        }
    }
    for key in ["作画", "人物原案", "人物设定"] {
        if let Some(name) = single(key) {
            for role in artist_roles {
                push_cleaned(&mut authors, &name, *role);
            }
        }
    }

    let has_writer = authors.iter().any(|a| a.role == AuthorRole::Writer);
    if !has_writer {
        let pencillers: Vec<Author> = authors
            .iter()
            .filter(|a| a.role == AuthorRole::Penciller)
            .cloned()
            .collect();
        for p in pencillers {
            if !authors
                .iter()
                .any(|a| a.name == p.name && a.role == AuthorRole::Writer)
            {
                authors.push(Author {
                    name: p.name,
                    role: AuthorRole::Writer,
                });
            }
        }
    }
    if !authors.iter().any(|a| a.role == AuthorRole::Penciller)
        && subject.platform.as_deref() == Some("漫画")
    {
        let writers: Vec<Author> = authors
            .iter()
            .filter(|a| a.role == AuthorRole::Writer)
            .cloned()
            .collect();
        for w in writers {
            if !authors
                .iter()
                .any(|a| a.name == w.name && a.role == AuthorRole::Penciller)
            {
                authors.push(Author {
                    name: w.name,
                    role: AuthorRole::Penciller,
                });
            }
        }
    }
    authors
}

fn clean_author_names(value: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"[《【（\[\(（][^》】）\]\)）]*[》】）\]\)）]").unwrap()
    });
    let no_brackets = re.replace_all(value, "");
    no_brackets
        .split(['/', '／', '、', '_', '→', '・', ':', '×', '&', ',', '，'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

pub struct BangumiMetadataProvider {
    client: BangumiClient,
    metadata_mapper: BangumiMetadataMapper,
    name_matcher: NameSimilarityMatcher,
    fetch_series_covers: bool,
    media_type: crate::model::MediaType,
}

pub fn create_provider(
    config: &ProviderConfig,
    default_name_matcher: NameSimilarityMatcher,
    token: Option<&str>,
    _http_client: &reqwest::Client,
) -> Option<BangumiMetadataProvider> {
    if !config.enabled {
        return None;
    }

    if config.media_type == crate::model::MediaType::Comic {
        return None;
    }
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
    }
    if let Ok(value) = reqwest::header::HeaderValue::from_str("dyphire/komf-rs") {
        headers.insert(reqwest::header::USER_AGENT, value);
    }
    let client = crate::providers::client_with_default_headers(headers);

    let name_matcher = config.name_matching_mode.unwrap_or(default_name_matcher);
    // 标签白名单：内置 json 资源（或 tagWhitelistFile 指定文件）+ tagWhitelist 自定义补充
    let mut tag_whitelist = load_bangumi_tag_whitelist(config.tag_whitelist_file.as_deref());
    for w in &config.tag_whitelist {
        if !w.is_empty() && !tag_whitelist.iter().any(|t| t == w) {
            tag_whitelist.push(w.clone());
        }
    }
    Some(BangumiMetadataProvider {
        client: BangumiClient::new(client),
        metadata_mapper: BangumiMetadataMapper::new(
            config.series_metadata.clone(),
            config.book_metadata.clone(),
            config.author_roles.clone(),
            config.artist_roles.clone(),
            tag_whitelist,
        ),
        name_matcher,
        fetch_series_covers: config.series_metadata.thumbnail,
        media_type: config.media_type,
    })
}

#[async_trait::async_trait]
impl MetadataProvider for BangumiMetadataProvider {
    fn provider_name(&self) -> CoreProviders {
        CoreProviders::Bangumi
    }

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        let id: u64 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid Bangumi series id: {}", series_id.0))
        })?;
        let subject = self.client.get(id).await?;
        // Kotlin: getSubjectRelations → filter(type==BOOK) → filter(relation=="单行本")
        let relations = self
            .client
            .get_subject_relations(id)
            .await?
            .into_iter()
            .filter(|r| r.r#type == Some(1) && r.relation.as_deref() == Some("单行本"))
            .collect::<Vec<_>>();
        let thumbnail = if self.fetch_series_covers {
            self.client.get_thumbnail(&subject, 60 * 1024, None).await?
        } else {
            None
        };
        Ok(self
            .metadata_mapper
            .to_series_metadata(&subject, &relations, thumbnail))
    }

    async fn get_series_cover(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError> {
        let id: u64 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid Bangumi series id: {}", series_id.0))
        })?;
        let subject = self.client.get(id).await?;
        self.client.get_thumbnail(&subject, 60 * 1024, None).await
    }

    async fn get_book_metadata(
        &self,
        _series_id: &ProviderSeriesId,
        book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError> {
        // Kotlin: client.getSubject(bookId.id.toLong()) → toBookMetadata
        let id: u64 = book_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid Bangumi book id: {}", book_id.0))
        })?;
        let book = self.client.get(id).await?;
        let thumbnail = if self.fetch_series_covers {
            self.client
                .get_thumbnail(&book, 30 * 1024, Some(1024 * 1024))
                .await?
        } else {
            None
        };
        Ok(self.metadata_mapper.to_book_metadata(&book, thumbnail))
    }

    async fn search_series(
        &self,
        series_name: &str,
        limit: usize,
        media_type: Option<crate::model::MediaType>,
    ) -> Result<Vec<SeriesSearchResult>, ProviderError> {
        // Kotlin: filter 掉 tags 含"漫画单行本"的条目 + take(limit)
        // Rust 扩展：优先库配置 mediaType（media_type 参数）映射 platform 过滤搜索显示结果；无则用全局。
        let platform_filter = media_type
            .and_then(media_type_platform)
            .or_else(|| media_type_platform(self.media_type));
        let results = self.client.search(series_name, limit as u32).await?;
        Ok(results
            .into_iter()
            .filter(|s| !s.tags.iter().any(|t| t.name == "漫画单行本"))
            .filter(|s| match platform_filter {
                Some(platform) => s.platform.as_deref() == Some(platform),
                None => true,
            })
            .take(limit)
            .map(|s| self.metadata_mapper.to_series_search_result(&s))
            .collect())
    }

    async fn match_series_metadata(
        &self,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        let results = self.client.search(&match_query.series_name, 20).await?;
        // 优先库配置 mediaType（query.media_type），无则用 provider 全局配置
        let platform_filter = match_query
            .media_type
            .and_then(media_type_platform)
            .or_else(|| media_type_platform(self.media_type));
        let candidates: Vec<BangumiSubject> = results
            .into_iter()
            .filter(|s| !s.tags.iter().any(|t| t.name == "漫画单行本"))
            .filter(|s| match platform_filter {
                Some(platform) => s.platform.as_deref() == Some(platform),
                None => true,
            })
            .collect();

        let mut matches: Vec<BangumiSubject> = candidates
            .iter()
            .filter(|s| {
                let mut titles = vec![s.name.clone()];
                if let Some(name_cn) = &s.name_cn {
                    titles.push(name_cn.clone());
                }
                self.name_matcher.matches(
                    &match_query.normalized_series_name(),
                    &match_query.normalize_titles(&titles),
                )
            })
            .cloned()
            .collect();

        if matches.is_empty() {
            for s in &candidates {
                if let Ok(subject) = self.client.get(s.id).await {
                    let mut titles = vec![subject.name.clone()];
                    if let Some(name_cn) = &subject.name_cn {
                        titles.push(name_cn.clone());
                    }
                    if let Some(alias) = subject
                        .infobox
                        .iter()
                        .find(|i| i.key.as_deref() == Some("别名"))
                    {
                        match &alias.value {
                            Some(serde_json::Value::String(v)) => titles.push(v.clone()),
                            Some(serde_json::Value::Array(items)) => {
                                for item in items {
                                    if let Some(v) = item.get("v").and_then(|v| v.as_str()) {
                                        titles.push(v.to_string());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if self.name_matcher.matches(
                        &match_query.normalized_series_name(),
                        &match_query.normalize_titles(&titles),
                    ) {
                        matches.push(subject);
                    }
                }
            }
        }
        // Kotlin: 0 → null；1 → getSubject；多个 → firstMatchingType（按 mediaType 过滤 platform）
        let subject = match matches.len() {
            0 => None,
            1 => Some(self.client.get(matches[0].id).await?),
            _ => self.first_matching_type(&matches).await?,
        };
        let Some(subject) = subject else {
            return Ok(None);
        };

        // Kotlin: getSubjectRelations → filter(type==BOOK) → filter(relation=="单行本")
        let relations = self
            .client
            .get_subject_relations(subject.id)
            .await?
            .into_iter()
            .filter(|r| r.r#type == Some(1) && r.relation.as_deref() == Some("单行本"))
            .collect::<Vec<_>>();
        let thumbnail = if self.fetch_series_covers {
            self.client.get_thumbnail(&subject, 60 * 1024, None).await?
        } else {
            None
        };
        Ok(Some(
            self.metadata_mapper
                .to_series_metadata(&subject, &relations, thumbnail),
        ))
    }
}

impl BangumiMetadataProvider {
    /// Kotlin `firstMatchingType`：逐个 getSubject 检查 platform（搜索响应项无 platform 字段）。
    async fn first_matching_type(
        &self,
        matches: &[BangumiSubject],
    ) -> Result<Option<BangumiSubject>, ProviderError> {
        let match_platform = match self.media_type {
            crate::model::MediaType::Manga => "漫画",
            crate::model::MediaType::Novel => "小说",
            _ => return Ok(None),
        };
        for m in matches {
            let subject = self.client.get(m.id).await?;
            if subject.platform.as_deref() == Some(match_platform) {
                return Ok(Some(subject));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实 Bangumi v0 搜索响应（2026-09 抓取）：`data` 字段（非 `results`）。
    #[test]
    fn search_response_parses_data_field() {
        let json = r#"{"total":608,"limit":20,"offset":0,"data":[{"id":3510,"name":"ONE PIECE","name_cn":"海贼王","summary":"拥有财富、名声、权力……","image":"https://lain.bgm.tv/pic/cover/l/2f/37/3510_1j4W8.jpg","tags":[{"name":"热血","count":1234}],"rating":{"rank":1,"total":4567,"count":{"1":10,"10":200},"score":9.1},"type":1}]}"#;
        let response: BangumiSearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.data.len(), 1);
        let subject = &response.data[0];
        assert_eq!(subject.id, 3510);
        assert_eq!(subject.name, "ONE PIECE");
        assert_eq!(subject.name_cn.as_deref(), Some("海贼王"));
        assert_eq!(subject.tags.len(), 1);
        assert_eq!(subject.subject_type, Some(1));
    }

    /// Kotlin `isbn10ToIsbn13`：10 位转 13 位。
    #[test]
    fn isbn10_to_isbn13_matches_kotlin() {
        assert_eq!(
            isbn10_to_isbn13("0306406152").as_deref(),
            Some("9780306406157")
        );
        // 已是 13 位（含连字符时先剥离）
        assert_eq!(
            isbn10_to_isbn13("978-0306406157").as_deref(),
            Some("9780306406157")
        );
        // 长度非法 → None（Kotlin require 抛异常，Rust 返回 None 由调用方兜底）
        assert_eq!(isbn10_to_isbn13("123"), None);
    }

    /// Kotlin `bookNumberRegex = "\(([^)]*)\)[^(]*$"`：取末尾括号内的数字。
    #[test]
    fn book_number_extracts_trailing_parenthesized() {
        assert_eq!(
            get_book_number("葬送的芙莉莲 (1)").map(|n| n.start),
            Some(1.0)
        );
        assert_eq!(get_book_number("Vol 2 (3)").map(|n| n.start), Some(3.0));
        assert_eq!(get_book_number("无括号标题"), None);
    }

    /// Kotlin `String.trimIndent()` 近似：删除公共最小前导空白。
    #[test]
    fn trim_indent_removes_common_indent() {
        let input = "    第一行\n        第二行缩进";
        let out = trim_indent(input);
        assert_eq!(out, "第一行\n    第二行缩进");
    }

    #[test]
    fn normalize_date_supports_js_formats() {
        assert_eq!(
            normalize_bangumi_date("2020年1月5日").as_deref(),
            Some("2020-01-05")
        );
        assert_eq!(
            normalize_bangumi_date("2020年1月").as_deref(),
            Some("2020-01-01")
        );
        assert_eq!(
            normalize_bangumi_date("2020-1-5").as_deref(),
            Some("2020-01-05")
        );
        assert_eq!(
            normalize_bangumi_date("2020-1").as_deref(),
            Some("2020-01-01")
        );
        assert_eq!(normalize_bangumi_date("  "), None);
        let d = parse_bangumi_date("2020年12月3日").expect("date");
        assert_eq!(d.year, Some(2020));
        assert_eq!(d.month, Some(12));
        assert_eq!(d.day, Some(3));
    }

    #[test]
    fn classify_status_matches_js() {
        assert_eq!(
            classify_bangumi_status("连载中"),
            Some(SeriesStatus::Ongoing)
        );
        assert_eq!(classify_bangumi_status("连载"), Some(SeriesStatus::Ongoing));
        assert_eq!(classify_bangumi_status("完结"), Some(SeriesStatus::Ended));
        assert_eq!(classify_bangumi_status("已完结"), Some(SeriesStatus::Ended));
        assert_eq!(classify_bangumi_status("停刊"), Some(SeriesStatus::Hiatus));
        assert_eq!(
            classify_bangumi_status("长期休载"),
            Some(SeriesStatus::Hiatus)
        );
        assert_eq!(classify_bangumi_status("未知状态"), None);
    }

    #[test]
    fn clean_author_names_splits_and_strips() {
        assert_eq!(clean_author_names("尾田荣一郎（原作）"), vec!["尾田荣一郎"]);
        assert_eq!(
            clean_author_names("荒木飞吕彦/ 岸边露伴"),
            vec!["荒木飞吕彦", "岸边露伴"]
        );
        assert_eq!(
            clean_author_names(" 手冢治虫 、 藤子·F·不二雄 "),
            vec!["手冢治虫", "藤子·F·不二雄"]
        );
        assert_eq!(clean_author_names("（仅注释）"), Vec::<String>::new());
    }

    #[test]
    fn media_type_platform_mapping() {
        use crate::model::MediaType;
        assert_eq!(media_type_platform(MediaType::Manga), Some("漫画"));
        assert_eq!(media_type_platform(MediaType::Novel), Some("小说"));
        assert_eq!(media_type_platform(MediaType::Comic), None);
        assert_eq!(media_type_platform(MediaType::Webtoon), None);
    }

    /// Bangumi titles 类型映射：name→Native，别名→Localized，nameCn 保持 null/zh
    /// 影响 Komga alternateTitles label：原名标 "Native"，别名标 "Localized"
    #[test]
    fn bangumi_title_types_native_localized() {
        let json = r#"{
            "id": 3510,
            "name": "葬送のフリーレン",
            "name_cn": "葬送的芙莉莲",
            "summary": null,
            "tags": [],
            "infobox": [{"key": "别名", "value": [{"k": "en", "v": "Frieren"}]}]
        }"#;
        let subject: BangumiSubject = serde_json::from_str(json).unwrap();
        let mapper = BangumiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            crate::config::BookMetadataConfig::default(),
            vec![],
            vec![],
            vec![],
        );
        let md = mapper.to_series_metadata(&subject, &[], None);
        assert!(md.metadata.title.is_none());
        let titles = &md.metadata.titles;
        assert_eq!(titles.len(), 3);
        assert_eq!(titles[0].name, "葬送のフリーレン");
        assert_eq!(titles[0].r#type, Some(TitleType::Native));
        assert_eq!(titles[0].language, None);
        assert_eq!(titles[1].name, "葬送的芙莉莲");
        assert_eq!(titles[1].r#type, None);
        assert_eq!(titles[1].language.as_deref(), Some("zh"));
        assert_eq!(titles[2].name, "Frieren");
        assert_eq!(titles[2].r#type, Some(TitleType::Localized));
        assert_eq!(titles[2].language.as_deref(), Some("en"));
    }

    /// 标签处理：白名单命中 + statusTags 排除 + 动态阈值
    #[test]
    fn filter_tags_whitelist_and_threshold() {
        let tags = vec![
            ("热血".to_string(), 500),
            ("非白名单".to_string(), 999),
            ("连载".to_string(), 500), // statusTags 排除
            ("搞笑".to_string(), 400),
        ];
        // max=500 > 200 → 阈值 35；热血/搞笑 ≥35 保留，按 count 降序
        let whitelist = load_bangumi_tag_whitelist(None);
        let out = filter_bangumi_tags(&tags, &whitelist);
        assert_eq!(out, vec!["热血", "搞笑"]);
    }

    /// 标签处理：max ≤ 10 → 阈值 3，count<3 排除
    #[test]
    fn filter_tags_low_threshold() {
        let tags = vec![
            ("热血".to_string(), 8),
            ("搞笑".to_string(), 2),
            ("恋爱".to_string(), 5),
            ("推理".to_string(), 7),
        ];
        // max=8 ≤ 10 → 阈值 3；count≥3 有 热血(8)/推理(7)/恋爱(5)；结果 3 个 <10 → 替换为排序后前 10（含搞笑）
        let whitelist = load_bangumi_tag_whitelist(None);
        assert_eq!(
            filter_bangumi_tags(&tags, &whitelist),
            vec!["热血", "推理", "恋爱", "搞笑"]
        );
    }

    /// 标签处理：阈值过滤后不足 10 个 → 取排序后前 10（JS slice(0,10) 替换）
    #[test]
    fn filter_tags_top10_fallback() {
        let tags: Vec<(String, i32)> = vec![
            ("热血".to_string(), 300),
            ("搞笑".to_string(), 20),
            ("恋爱".to_string(), 18),
            ("推理".to_string(), 16),
            ("悬疑".to_string(), 14),
            ("侦探".to_string(), 12),
            ("竞技".to_string(), 10),
            ("体育".to_string(), 8),
            ("励志".to_string(), 6),
            ("职场".to_string(), 4),
            ("社会".to_string(), 3),
            ("史诗".to_string(), 2),
        ];
        // max=300 > 200 → 阈值 35：仅热血通过 → 不足 10 → 取前 10
        let whitelist = load_bangumi_tag_whitelist(None);
        let out = filter_bangumi_tags(&tags, &whitelist);
        assert_eq!(out.len(), 10);
        assert_eq!(out[0], "热血");
        assert_eq!(out[9], "职场");
    }

    /// 配置项：自定义白名单补充生效
    #[test]
    fn filter_tags_custom_whitelist() {
        let tags = vec![("自定义标签".to_string(), 100)];
        assert!(filter_bangumi_tags(&tags, &[]).is_empty());
        // 自定义白名单（配置 tagWhitelist 补充）生效
        assert_eq!(
            filter_bangumi_tags(&tags, &["自定义标签".to_string()]),
            vec!["自定义标签"]
        );
    }
}
