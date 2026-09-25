//! Bangumi provider
//!
use crate::config::BangumiConfig;
use crate::model::{
    Author, AuthorRole, BookMetadata, Image, MatchQuery, ProviderBookId, ProviderBookMetadata,
    ProviderSeriesId, ProviderSeriesMetadata, Publisher, PublisherType, SeriesBook, SeriesMetadata,
    SeriesSearchResult, SeriesStatus, SeriesTitle, TitleType, WebLink,
};
use crate::providers::bangumi_archive::{ArchiveSubject, BangumiArchiveService, PersonInfo};
use crate::providers::{CoreProviders, MetadataProvider, ProviderError};
use crate::util::NameSimilarityMatcher;
use crate::util::chinese::{ChineseConverter, ChineseDirection};
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
    /// Bangumi API age_rating：0=全年龄、1=15+、2=18+（Archive 离线数据无此字段）
    #[serde(default)]
    pub age_rating: Option<i32>,
    /// Bangumi 成人内容标记（在线 API 与 Archive 均有）
    #[serde(default)]
    pub nsfw: Option<bool>,
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

    /// 封面 URL 候选：顶层 image（search API 有）→ images 各尺寸（详情 API 只有 images）。
    fn cover_url(subject: &BangumiSubject) -> Option<String> {
        if let Some(url) = subject.image.clone().filter(|u| !u.is_empty()) {
            return Some(url);
        }
        if let Some(images) = &subject.images {
            for url in [&images.large, &images.medium, &images.common, &images.small] {
                if let Some(url) = url.clone().filter(|u| !u.is_empty()) {
                    return Some(url);
                }
            }
        }
        None
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
        self.to_series_metadata_persons(subject, book_relations, thumbnail, &[])
    }

    /// 离线 person 增强版：persons 非空时作者/出版社采用 person 实体（name_cn 优先）；
    /// persons 为空（在线）保持 infobox 解析现状。
    pub fn to_series_metadata_persons(
        &self,
        subject: &BangumiSubject,
        book_relations: &[BangumiSubjectRelation],
        thumbnail: Option<Image>,
        persons: &[PersonInfo],
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

        // 在线：API 顶层 volumes/eps/total_episodes；离线（Archive）无这些字段，
        // 回退 infobox「册数」（已出版卷数）→「话数」（eps/total_episodes 语义）。
        // 键支持繁简变体（册数/冊数/卷数/巻数/册數/冊數/卷數/巻數；话数/話數/话數/話数）。
        const VOLUME_KEYS: [&str; 8] = ["册数", "冊数", "卷数", "巻数", "册數", "冊數", "卷數", "巻數"];
        const EPISODE_KEYS: [&str; 4] = ["话数", "話數", "话數", "話数"];
        let raw_total = subject
            .volumes
            .or(subject.eps)
            .or(subject.total_episodes)
            .or_else(|| info_box_count(&info_box, &VOLUME_KEYS))
            .or_else(|| info_box_count(&info_box, &EPISODE_KEYS));
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
                // 离线 Archive 漫画 infobox 以「连载结束」为主（「结束」键罕见），
                // 非空即视为已完结（在线无此键，不受影响）。
                let serial_ended = info_box
                    .get("连载结束")
                    .and_then(|v| v.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false);
                let ended = info_box.contains_key("结束")
                    || info_box.contains_key("完结")
                    || serial_ended
                    || raw_total.map(|c| c > 0).unwrap_or(false);
                if ended {
                    status = Some(SeriesStatus::Ended);
                }
                status
            })
            .flatten();

        // 白名单（内置+自定义）+ 非 statusTags → count 降序 → 动态阈值（3~35）→ 不足 10 补前 10
        // 补足从白名单命中项取，不引入非白名单标签
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
            // 预置主标题（原名/中文名）：别名与其相同则跳过
            let mut alias_seen = std::collections::HashSet::new();
            alias_seen.insert(subject.name.clone());
            if let Some(cn) = &subject.name_cn {
                alias_seen.insert(cn.clone());
            }
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
                            if sub_key == Some("别名") || sub_key == Some("別名") || sub_key == Some("版本名") {
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
            if persons.is_empty() {
                extract_authors(subject, &self.author_roles, &self.artist_roles)
            } else {
                offline_person_authors(persons, &self.author_roles, &self.artist_roles)
            }
        } else {
            Vec::new()
        };

        // Kotlin: 出版社 → ORIGINAL；其他出版社 → LOCALIZED；alternativePublishers = drop(1) + other
        // 离线（persons 非空）：position=2004（出版社）→ 第一个 ORIGINAL，其余 LOCALIZED（alternative）；
        // name 采用 name_cn 优先。在线（persons 空）保持 infobox 解析现状。
        let (publisher, alternative_publishers): (Option<Publisher>, Vec<Publisher>) =
            if cfg.publisher {
                if persons.is_empty() {
                    let mut publishers: Vec<Publisher> = Vec::new();
                    let mut other_publishers: Vec<Publisher> = Vec::new();
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
                    let publisher = publishers.first().cloned();
                    // Kotlin: altPublishers = (publishers.drop(1) + otherPublishers).toSet() —— 保序去重
                    let mut alternative_publishers: Vec<Publisher> = publishers
                        .iter()
                        .skip(1)
                        .cloned()
                        .chain(other_publishers)
                        .collect();
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
                    (publisher, alternative_publishers)
                } else {
                    // 对齐 Kotlin：publisher 来自 infobox「出版社」（ORIGINAL）+「其他出版社」（LOCALIZED）；
                    // persons 仅提供 name_cn 映射（离线扩展，匹配到时 name_cn 优先）
                    let mut publishers: Vec<Publisher> = Vec::new();
                    let mut other_publishers: Vec<Publisher> = Vec::new();
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
                                        name: publisher_display_name(persons, name),
                                        r#type: Some(kind),
                                        language_tag: None,
                                    });
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
                    (publisher, alternative_publishers)
                }
            } else {
                (None, Vec::new())
            };

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
            age_rating: cfg
                .age_rating
                .then_some(map_bangumi_age_rating(subject.age_rating, subject.nsfw, &tags))
                .flatten(),
            language: None,
            // platform（漫画/小说）写入 genres；受 cfg.genres 控制（与其余 provider 一致）
            genres: if cfg.genres {
                subject.platform.clone().into_iter().collect()
            } else {
                Vec::new()
            },
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
        // js 排序：单行本先按标题提取的卷号排，无法提取 / 卷号相同回退 id 升序
        //（离线 get_related 与在线 API 均按此统一，消除对返回顺序的隐式依赖）
        let mut relations: Vec<&BangumiSubjectRelation> = book_relations.iter().collect();
        relations.sort_by(|a, b| {
            let na = get_book_number(&a.name).map(|r| r.start);
            let nb = get_book_number(&b.name).map(|r| r.start);
            match (na, nb) {
                (Some(x), Some(y)) if x != y => {
                    x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
                }
                _ => a.id.cmp(&b.id),
            }
        });
        let books: Vec<SeriesBook> = if cfg.books {
            relations
                .into_iter()
                .enumerate()
                .map(|(i, rel)| SeriesBook {
                    id: ProviderBookId(rel.id.to_string()),
                    // js：extractVolumeNumber(...) || (index + 1)——标题提取失败回退排序位置
                    number: get_book_number(&rel.name)
                        .or_else(|| Some(crate::model::BookRange::single((i + 1) as f64))),
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
            image_url: BangumiClient::cover_url(subject),
            title,
            provider: CoreProviders::Bangumi.as_str().to_string(),
            result_id: subject.id.to_string(),
            media_type: platform_media_type(subject.platform.as_deref()),
            language: None,
            nsfw: subject.nsfw,
        }
    }

    /// 对应 Kotlin `toBookMetadata`（含 BookMetadataConfig apply 语义）。
    pub fn to_book_metadata(
        &self,
        book: &BangumiSubject,
        thumbnail: Option<Image>,
    ) -> ProviderBookMetadata {
        self.to_book_metadata_persons(book, thumbnail, &[])
    }

    /// 离线 person 增强版（同系列：persons 非空时作者 name_cn 优先）。
    pub fn to_book_metadata_persons(
        &self,
        book: &BangumiSubject,
        thumbnail: Option<Image>,
        persons: &[PersonInfo],
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

        // Kotlin: releaseDate = book.date?.let { LocalDate.parse(it) }
        // 对齐 series：归一化（ISO 原样补零；"2019年7月25日"/"2019-07" → ISO 后写入），
        // 在线/离线（Archive）统一，空值 → None。
        let release_date = book.date.as_deref().and_then(normalize_bangumi_date);

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
                if persons.is_empty() {
                    extract_authors(book, &self.author_roles, &self.artist_roles)
                } else {
                    offline_person_authors(persons, &self.author_roles, &self.artist_roles)
                }
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

/// 别名项 k 的语言代码白名单（忽略非语言标签）：
/// `[非官方|电锯人]` 的 k 是别名类型标签而非语言，写入时 language 应为 None；
/// 仅真实语言代码（en/zh/hk…）保留为 language。
fn alias_language(k: &str) -> Option<String> {
    let lower = k.trim().to_ascii_lowercase();
    match lower.as_str() {
        "en" | "en-us" | "en-gb" | "ja" | "jp" | "ja-ro" | "zh" | "zh-cn" | "zh-hans"
        | "zh-hk" | "zh-tw" | "zh-hant" | "zh-sg" | "cn" | "hk" | "tw" | "ko" | "ko-kr"
        | "fr" | "de" | "es" | "it" | "ru" | "th" | "vi" | "id" | "pt" | "pt-br" | "ar" => {
            let normalized = match lower.as_str() {
                "hk" => "zh-hk",
                "tw" => "zh-tw",
                "cn" => "zh-cn",
                other => other,
            };
            Some(normalized.to_string())
        }
        _ => None,
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
                            obj.get("k")
                                .and_then(|k| k.as_str())
                                .and_then(alias_language)
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
                obj.get("k")
                    .and_then(|k| k.as_str())
                    .and_then(alias_language)
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

/// 遍历键变体（繁简）取首个命中数字。
fn info_box_count(
    info_box: &std::collections::HashMap<&str, &serde_json::Value>,
    keys: &[&str],
) -> Option<i32> {
    keys.iter()
        .find_map(|k| info_box.get(*k).and_then(|v| infobox_count(*v)))
        .filter(|c| *c > 0)
}

/// 从 infobox 值提取首个数字序列（如 "97话"、"全 11 卷" → 97/11）。
fn infobox_count(v: &serde_json::Value) -> Option<i32> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\d+").unwrap());
    let s = v.as_str()?;
    re.find(s)?.as_str().parse::<i32>().ok()
}

/// 组合式 age_rating：API age_rating 字段优先（0→0 全年龄、1→15、2→18）；
/// 无 API 值（含 Archive 离线数据）→ 标签含成人指示词推断 18；未知 API 值 → None。
fn map_bangumi_age_rating(api: Option<i32>, nsfw: Option<bool>, tags: &[String]) -> Option<i32> {
    if let Some(r) = api {
        return match r {
            1 => Some(15),
            2 => Some(18),
            0 => Some(0),
            _ => None,
        };
    }
    if nsfw == Some(true) {
        return Some(18);
    }
    if nsfw == Some(false) {
        // 明确的非成人声明：不写分级（也不做标签推断）
        return None;
    }
    const ADULT_TAGS: [&str; 24] = [
        "エロ", "官能", "乱交", "SM", "触手", "鬼畜", "催眠", "扶她",
        "母系", "熟女", "调教", "恶堕", "幼女", "R18", "成年コミック",
        "成人漫画", "アダルトコミック", "18X", "無修正", "無修", "无修",
        "H本", "A书", "黄漫",
    ];
    tags.iter()
        .any(|t| ADULT_TAGS.contains(&t.as_str()))
        .then_some(18)
}

fn classify_bangumi_status(value: &str) -> Option<SeriesStatus> {
    let s = value.to_lowercase();
    if s.contains("休刊") || s.contains("停刊") || s.contains("停止连载") || s.contains("长期休载")
    {
        Some(SeriesStatus::Hiatus)
    } else if s.contains("连载中") || s.contains("连载") {
        Some(SeriesStatus::Ongoing)
    } else if s.contains("腰斩") || s.contains("完结") {
        Some(SeriesStatus::Ended)
    } else {
        None
    }
}

const BANGUMI_STATUS_TAGS: [&str; 9] = [
    "连载",
    "连载中",
    "完结",
    "已完结",
    "腰斩",
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

/// book release_date 归一化（对齐 series parse_bangumi_date）：
/// ISO 原样、年月补 -01、中文格式转 ISO、空 → None。
#[test]
    fn book_release_date_normalized() {
        let mapper = BangumiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            crate::config::BookMetadataConfig::default(),
            vec![],
            vec![],
            vec![],
        );
        let mk = |date: Option<&str>| {
            let d = match date {
                Some(d) => format!("\"{d}\"") ,
                None => "null".to_string(),
            };
            let json = format!(
                r#"{{
                    "id": 9, "name": "X", "name_cn": null, "summary": null, "tags": [],
                    "date": {d},
                    "infobox": []
                }}"#
            );
            let subject: BangumiSubject = serde_json::from_str(&json).unwrap();
            mapper.to_book_metadata(&subject, None).metadata.release_date
        };
        assert_eq!(mk(Some("2019-07-25")), Some("2019-07-25".to_string()));
        assert_eq!(mk(Some("2019-7-5")), Some("2019-07-05".to_string()));
        assert_eq!(mk(Some("2019-07")), Some("2019-07-01".to_string()));
        assert_eq!(mk(Some("2019年7月25日")), Some("2019-07-25".to_string()));
        assert_eq!(mk(None), None);
    }

/// 繁简键变体：冊数/巻数/話數 等也能回退（archive 港台条目 infobox 键为繁体）。
#[test]
    fn total_book_count_variant_keys() {
        let mapper = BangumiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            crate::config::BookMetadataConfig::default(),
            vec![],
            vec![],
            vec![],
        );
        let mk = |infobox: &str| {
            let json = format!(
                r#"{{
                    "id": 9, "name": "X", "name_cn": null, "summary": null, "tags": [],
                    "volumes": null, "eps": null, "total_episodes": null,
                    "infobox": {infobox}
                }}"#
            );
            let subject: BangumiSubject = serde_json::from_str(&json).unwrap();
            mapper.to_series_metadata(&subject, &[], None).metadata.total_book_count
        };
        // 繁体册数（冊数/巻数/冊數）
        assert_eq!(mk(r#"[{"key": "冊数", "value": "12"}]"#), Some(12));
        assert_eq!(mk(r#"[{"key": "巻数", "value": "8"}]"#), Some(8));
        // 繁体话数（話數/话數/話数）
        assert_eq!(mk(r#"[{"key": "話數", "value": "120"}]"#), Some(120));
        assert_eq!(mk(r#"[{"key": "话數", "value": "33"}]"#), Some(33));
        // 繁体册数优先于繁体话数
        assert_eq!(
            mk(r#"[{"key": "冊数", "value": "12"}, {"key": "話數", "value": "120"}]"#),
            Some(12)
        );
        // 简体键仍工作
        assert_eq!(mk(r#"[{"key": "册数", "value": "5"}]"#), Some(5));
    }

/// 话数回退：无 volumes/eps/册数时用 infobox「话数」（eps/total_episodes 语义）；
/// 册数优先于话数；"97话" 等容错解析。
#[test]
    fn total_book_count_episodes_fallback() {
        let mapper = BangumiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            crate::config::BookMetadataConfig::default(),
            vec![],
            vec![],
            vec![],
        );
        // 无册数 → 话数回退（97）
        let j1 = r#"{
            "id": 9, "name": "X", "name_cn": null, "summary": null, "tags": [],
            "volumes": null, "eps": null, "total_episodes": null,
            "infobox": [{"key": "话数", "value": "97"}]
        }"#;
        let s1: BangumiSubject = serde_json::from_str(j1).unwrap();
        assert_eq!(
            mapper.to_series_metadata(&s1, &[], None).metadata.total_book_count,
            Some(97)
        );
        // 册数优先于话数（11 vs 97）
        let j2 = r#"{
            "id": 9, "name": "X", "name_cn": null, "summary": null, "tags": [],
            "volumes": null, "eps": null, "total_episodes": null,
            "infobox": [{"key": "册数", "value": "11"}, {"key": "话数", "value": "97"}]
        }"#;
        let s2: BangumiSubject = serde_json::from_str(j2).unwrap();
        assert_eq!(
            mapper.to_series_metadata(&s2, &[], None).metadata.total_book_count,
            Some(11)
        );
        // 容错：话数 "97话"、册数 "全 11 卷"
        let j3 = r#"{
            "id": 9, "name": "X", "name_cn": null, "summary": null, "tags": [],
            "volumes": null, "eps": null, "total_episodes": null,
            "infobox": [{"key": "话数", "value": "97话"}]
        }"#;
        let s3: BangumiSubject = serde_json::from_str(j3).unwrap();
        assert_eq!(
            mapper.to_series_metadata(&s3, &[], None).metadata.total_book_count,
            Some(97)
        );
        // 在线 volumes 仍优先
        let j4 = r#"{
            "id": 9, "name": "X", "name_cn": null, "summary": null, "tags": [],
            "volumes": 20, "eps": null, "total_episodes": null,
            "infobox": [{"key": "话数", "value": "97"}]
        }"#;
        let s4: BangumiSubject = serde_json::from_str(j4).unwrap();
        assert_eq!(
            mapper.to_series_metadata(&s4, &[], None).metadata.total_book_count,
            Some(20)
        );
    }

/// js 行为：infobox 状态键缺失时从 tags 解析状态（BANGUMI_STATUS_TAGS 精确匹配）。
#[test]
    fn status_from_tags_fallback() {
        let mapper = BangumiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            crate::config::BookMetadataConfig::default(),
            vec![],
            vec![],
            vec![],
        );
        let mk = |tag: &str| {
            let json = format!(
                r#"{{
                    "id": 9,
                    "name": "X",
                    "name_cn": null,
                    "summary": null,
                    "tags": [{{"name": "{tag}", "count": 100}}, {{"name": "漫画", "count": 1}}],
                    "infobox": []
                }}"#
            );
            let subject: BangumiSubject = serde_json::from_str(&json).unwrap();
            let md = mapper.to_series_metadata(&subject, &[], None);
            md.metadata.status
        };
        assert_eq!(mk("已完结"), Some(SeriesStatus::Ended));
        assert_eq!(mk("完结"), Some(SeriesStatus::Ended));
        assert_eq!(mk("连载中"), Some(SeriesStatus::Ongoing));
        assert_eq!(mk("连载"), Some(SeriesStatus::Ongoing));
        assert_eq!(mk("休刊"), Some(SeriesStatus::Hiatus));
        // 非状态词 → 不解析
        assert_eq!(mk("漫画"), None);
    }

/// 离线 Archive 形态：无 volumes/eps/total_episodes 顶层字段时，
/// total_book_count 回退 infobox「册数」，status 由「连载结束」非空判定为 Ended。
#[test]
    fn archive_status_and_total_book_count() {
        let json = r#"{
            "id": 268279,
            "name": "チェンソーマン",
            "name_cn": "链锯人",
            "summary": null,
            "tags": [],
            "volumes": null,
            "eps": null,
            "total_episodes": null,
            "infobox": [
                {"key": "册数", "value": "11"},
                {"key": "连载开始", "value": "2018-12-03"},
                {"key": "连载结束", "value": "2020-12-14"}
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
        assert_eq!(md.metadata.total_book_count, Some(11));
        assert_eq!(md.metadata.status, Some(SeriesStatus::Ended));

        // 在线形态：API 顶层 volumes 优先于 infobox 册数
        let j2 = r#"{
            "id": 2,
            "name": "X",
            "name_cn": null,
            "summary": null,
            "tags": [],
            "volumes": 20,
            "eps": null,
            "total_episodes": null,
            "infobox": [{"key": "册数", "value": "5"}]
        }"#;
        let s2: BangumiSubject = serde_json::from_str(j2).unwrap();
        let md2 = mapper.to_series_metadata(&s2, &[], None);
        assert_eq!(md2.metadata.total_book_count, Some(20));
    }

/// 特殊别名结构：`[非官方|电锯人]` 的 k 是标签非语言 → language None；
/// `[en|xxx]` 语言代码保留；别名与 name/name_cn 相同不写入。
#[test]
    fn alias_tag_not_language_and_title_dedup() {
        let json = r#"{
            "id": 268279,
            "name": "チェンソーマン",
            "name_cn": "链锯人",
            "summary": null,
            "tags": [],
            "infobox": [
                {"key": "别名", "value": [
                    {"k": "非官方", "v": "电锯人"},
                    {"v": "Chainsaw man"},
                    {"k": "en", "v": "Chainsaw Man"}
                ]},
                {"key": "版本:东立版", "value": [
                    {"k": "版本名", "v": "鏈鋸人"}
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
        let titles: Vec<(String, Option<String>)> = md
            .metadata
            .titles
            .iter()
            .map(|t| (t.name.clone(), t.language.clone()))
            .collect();
        // Native 原名 + zh 中文名 + 别名（非官方→None、纯值→None、en→en、版本名→None）
        assert!(titles.contains(&("チェンソーマン".to_string(), None)));
        assert!(titles.contains(&("链锯人".to_string(), Some("zh".to_string()))));
        assert!(titles.contains(&("电锯人".to_string(), None)));
        assert!(titles.contains(&("Chainsaw man".to_string(), None)));
        assert!(titles.contains(&("Chainsaw Man".to_string(), Some("en".to_string()))));
        assert!(titles.contains(&("鏈鋸人".to_string(), None)));
        // 与 name_cn 相同的别名不重复写入
        let j2 = r#"{
            "id": 1,
            "name": "A",
            "name_cn": "B",
            "summary": null,
            "tags": [],
            "infobox": [{"key": "别名", "value": [{"v": "B"}, {"v": "C"}]}]
        }"#;
        let s2: BangumiSubject = serde_json::from_str(j2).unwrap();
        let md2 = mapper.to_series_metadata(&s2, &[], None);
        let alt2: Vec<&str> = md2
            .metadata
            .titles
            .iter()
            .filter(|t| t.r#type == Some(TitleType::Localized))
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(alt2, vec!["C"]);
    }

/// 嵌套别名收集：
/// infobox 任意项的 value 数组内 k=="别名" 的子项（如「版本:*」条目），无语言。
#[test]
    fn nested_aliases_collected() {
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
    // 直接别名 1 条 + 嵌套去重后 2 条（三月的狮子 / 三月的獅子）；
    // 版本名「3月的狮子」与 name_cn 相同 → 被排除（别名与主标题相同不写入）
    assert_eq!(
        localized,
        vec!["March comes in like a lion", "三月的狮子", "三月的獅子"]
    );
}

/// 离线 persons：作者/出版社 name_cn 优先（仅离线路径；在线保持 infobox 现状）。
#[test]
fn offline_person_authors_and_publishers() {
    let mapper = BangumiMetadataMapper::new(
        crate::config::SeriesMetadataConfig::default(),
        crate::config::BookMetadataConfig::default(),
        vec![AuthorRole::Writer],
        vec![AuthorRole::Penciller],
        vec![],
    );
    let subject: BangumiSubject = serde_json::from_str(
        r#"{"id":268279,"name":"チェンソーマン","name_cn":"链锯人","summary":null,"tags":[],"infobox":[{"key":"作者","value":"藤本タツキ"},{"key":"出版社","value":"集英社"}]}"#,
    )
    .unwrap();
    let persons = vec![
        PersonInfo {
            person_id: 23155,
            name: "藤本タツキ".to_string(),
            name_cn: Some("藤本树".to_string()),
            person_type: Some(1),
            career: vec!["mangaka".to_string()],
            position: Some(2001),
            appear_eps: String::new(),
            aliases: Vec::new(),
        },
        PersonInfo {
            person_id: 588,
            name: "白泉社".to_string(),
            name_cn: Some("白泉社".to_string()),
            person_type: Some(2),
            career: Vec::new(),
            position: Some(2004),
            appear_eps: String::new(),
            aliases: Vec::new(),
        },
        PersonInfo {
            person_id: 7611,
            name: "尖端出版".to_string(),
            name_cn: Some("台湾尖端".to_string()),
            person_type: Some(2),
            career: Vec::new(),
            position: Some(2004),
            appear_eps: String::new(),
            aliases: Vec::new(),
        },
    ];
    let md = mapper.to_series_metadata_persons(&subject, &[], None, &persons);
    // 作者：position=2001 → author_roles + artist_roles，name_cn 优先 → 藤本树
    assert_eq!(
        md.metadata.authors,
        vec![
            Author { name: "藤本树".to_string(), role: AuthorRole::Writer },
            Author { name: "藤本树".to_string(), role: AuthorRole::Penciller },
        ]
    );
    // 出版社：对齐 Kotlin —— publisher 来自 infobox「出版社」（ORIGINAL），
    // persons 仅做 name_cn 映射（匹配到时 name_cn 优先）。
    // infobox 出版社=集英社，persons 无匹配 → 保持原文
    assert_eq!(
        md.metadata.publisher.as_ref().map(|p| p.name.clone()),
        Some("集英社".to_string())
    );
    assert_eq!(md.metadata.publisher.as_ref().and_then(|p| p.r#type), Some(PublisherType::Original));
    assert!(md.metadata.alternative_publishers.is_empty());
    // infobox 出版社=尖端出版 → persons 匹配 → 台湾尖端（name_cn 优先）
    let subject2: BangumiSubject = serde_json::from_str(
        r#"{"id":1,"name":"X","name_cn":null,"summary":null,"tags":[],"infobox":[{"key":"出版社","value":"尖端出版"},{"key":"其他出版社","value":"白泉社"}]}"#,
    )
    .unwrap();
    let md3 = mapper.to_series_metadata_persons(&subject2, &[], None, &persons);
    assert_eq!(
        md3.metadata.publisher.as_ref().map(|p| p.name.clone()),
        Some("台湾尖端".to_string())
    );
    assert_eq!(
        md3.metadata.publisher.as_ref().and_then(|p| p.r#type),
        Some(PublisherType::Original)
    );
    // alternative = 出版社.drop(1) + 其他出版社 → 白泉社（name_cn 匹配 → 白泉社），LOCALIZED
    assert_eq!(md3.metadata.alternative_publishers.len(), 1);
    assert_eq!(md3.metadata.alternative_publishers[0].name, "白泉社");
    assert_eq!(
        md3.metadata.alternative_publishers[0].r#type,
        Some(PublisherType::Localized)
    );
    // 在线（persons 空）→ 保持 infobox 现状：作者=藤本タツキ（无中文名）、出版社=集英社
    let online = mapper.to_series_metadata(&subject, &[], None);
    assert_eq!(online.metadata.authors[0].name, "藤本タツキ");
    assert_eq!(online.metadata.publisher.as_ref().map(|p| p.name.as_str()), Some("集英社"));
    // name_cn 为空 → 回退日文原名
    let p_no_cn = PersonInfo {
        person_id: 1954,
        name: "週刊少年ジャンプ".to_string(),
        name_cn: None,
        person_type: Some(2),
        career: Vec::new(),
        position: Some(2005),
        appear_eps: String::new(),
        aliases: Vec::new(),
    };
    assert_eq!(person_display_name(&p_no_cn), "週刊少年ジャンプ");
    // 连载杂志（2005）不影响 authors；publisher 仍来自 infobox（Kotlin 语义）
    let md2 = mapper.to_series_metadata_persons(&subject, &[], None, &[p_no_cn]);
    assert!(md2.metadata.authors.is_empty());
    assert_eq!(
        md2.metadata.publisher.as_ref().map(|p| p.name.as_str()),
        Some("集英社")
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
/// 注意：白名单命中用精确匹配子串判断
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
        // 不足 10：白名单命中项（count 降序）全取前 10，不引入非白名单标签。
        final_tags = valid.iter().cloned().take(10).collect();
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


/// person 显示名：name_cn 优先（非空），否则日文原名。
fn person_display_name(p: &PersonInfo) -> String {
    p.name_cn
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&p.name)
        .to_string()
}

/// 出版社显示名：persons 中按 name 或 name_cn 匹配时用 name_cn 优先（离线扩展），
/// 未匹配保持原名（对齐 Kotlin infobox 出版社原文）。
fn publisher_display_name(persons: &[PersonInfo], name: &str) -> String {
    for p in persons.iter().filter(|p| p.position == Some(2004)) {
        if p.name == name || p.name_cn.as_deref() == Some(name) {
            return person_display_name(p);
        }
    }
    name.to_string()
}

/// 离线作者（persons 路径）：position 角色映射对齐 infobox 语义，
/// 2001（作者）→ author_roles + artist_roles；2007（原作）→ author_roles；
/// 2002（作画/人物原案/人物设定）→ artist_roles；name 用 name_cn 优先。
fn offline_person_authors(
    persons: &[PersonInfo],
    author_roles: &[AuthorRole],
    artist_roles: &[AuthorRole],
) -> Vec<Author> {
    let mut out: Vec<Author> = Vec::new();
    for p in persons {
        let roles: Vec<AuthorRole> = match p.position {
            Some(2001) => author_roles
                .iter()
                .chain(artist_roles.iter())
                .cloned()
                .collect(),
            Some(2007) => author_roles.to_vec(),
            Some(2010) => author_roles.to_vec(), // 脚本
            Some(2002) => artist_roles.to_vec(),
            Some(2003) => artist_roles.to_vec(), // 插图（小说插画师）
            Some(2009) => artist_roles.to_vec(), // 插画/画师（样例均为画师/插画家）
            _ => continue,
        };
        let name = person_display_name(p);
        for role in roles {
            if !out.iter().any(|a: &Author| a.name == name && a.role == role) {
                out.push(Author {
                    name: name.clone(),
                    role,
                });
            }
        }
    }
    out
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
    // 漫画/小说相关职务（动画专用如「系列构成」不映射；「人物设定」保留现状）：
    // 脚本/原案/分镜/脚本·分镜 → authorRoles（writer 系）
    for key in ["脚本", "原案", "分镜", "脚本·分镜"] {
        if let Some(name) = single(key) {
            for role in author_roles {
                push_cleaned(&mut authors, &name, *role);
            }
        }
    }
    for key in ["作画", "人物原案", "人物设定"] {
        if let Some(name) = single(key) {
            for role in artist_roles {
                push_cleaned(&mut authors, &name, *role);
            }
        }
    }
    // 插图/插画家/铅稿 → artistRoles（画师系）
    for key in ["插图", "插画家", "铅稿"] {
        if let Some(name) = single(key) {
            for role in artist_roles {
                push_cleaned(&mut authors, &name, *role);
            }
        }
    }
    // 上色 → 固定 COLORIST（不随 artistRoles 配置）
    if let Some(name) = single("上色") {
        push_cleaned(&mut authors, &name, AuthorRole::Colorist);
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
    /// bangumi/Archive 离线数据源（None = 未启用；get() 返回 None = 未就绪 → 回退在线）。
    archive: Option<std::sync::Arc<BangumiArchiveService>>,
    /// 简繁归一化（bangumi 匹配始终应用，不依赖 chineseConversion 配置；Rust 扩展）。
    chinese_t2s: ChineseConverter,
    chinese_s2t: ChineseConverter,
}

pub fn create_provider(
    config: &BangumiConfig,
    default_name_matcher: NameSimilarityMatcher,
    token: Option<&str>,
    http_client: &reqwest::Client,
    work_dir: Option<&std::path::Path>,
) -> Option<BangumiMetadataProvider> {
    let provider = &config.provider;
    if !provider.enabled {
        return None;
    }

    if provider.media_type == crate::model::MediaType::Comic {
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

    let name_matcher = provider.name_matching_mode.unwrap_or(default_name_matcher);
    // 标签白名单：内置 json 资源（或 tagWhitelistFile 指定文件）+ tagWhitelist 自定义补充
    let mut tag_whitelist = load_bangumi_tag_whitelist(provider.tag_whitelist_file.as_deref());
    for w in &provider.tag_whitelist {
        if !w.is_empty() && !tag_whitelist.iter().any(|t| t == w) {
            tag_whitelist.push(w.clone());
        }
    }
    // bangumi/Archive 离线数据源：enabled 时启动后台下载/构建（tokio::spawn 非阻塞）。
    // 数据目录：配置 dir → workDir/bangumi-archive。
    let archive = if config.archive.enabled {
        let dir = config
            .archive
            .dir
            .as_ref()
            .map(std::path::PathBuf::from)
            .or_else(|| work_dir.map(|d| d.join("bangumi-archive")))
            .unwrap_or_else(|| std::path::PathBuf::from("bangumi-archive"));
        Some(BangumiArchiveService::start(
            &config.archive,
            http_client.clone(),
            dir,
        ))
    } else {
        None
    };
    Some(BangumiMetadataProvider {
        client: BangumiClient::new(client),
        metadata_mapper: BangumiMetadataMapper::new(
            provider.series_metadata.clone(),
            provider.book_metadata.clone(),
            provider.author_roles.clone(),
            provider.artist_roles.clone(),
            tag_whitelist,
        ),
        name_matcher,
        fetch_series_covers: provider.series_metadata.thumbnail,
        media_type: provider.media_type,
        archive,
        chinese_t2s: ChineseConverter::new(ChineseDirection::T2s)
            .unwrap_or_else(|_| ChineseConverter::none()),
        chinese_s2t: ChineseConverter::new(ChineseDirection::S2t)
            .unwrap_or_else(|_| ChineseConverter::none()),
    })
}

#[async_trait::async_trait]
impl MetadataProvider for BangumiMetadataProvider {

    fn resolve_link_id(&self, query: &str) -> Option<String> {
        let re = regex::Regex::new(r"(?:bgm\.tv|bangumi\.tv|chii\.in)/subject/(\d+)").ok()?;
        re.captures(query)
            .map(|c| c.get(1).unwrap().as_str().to_string())
    }
    fn provider_name(&self) -> CoreProviders {
        CoreProviders::Bangumi
    }

    async fn resolve_link_search_result(&self, query: &str) -> Option<SeriesSearchResult> {
        let id = self.resolve_link_id(query)?;
        let id: u64 = id.parse().ok()?;
        // Archive 离线优先（与 get_series_metadata 一致）：元数据离线 + 封面在线补（archive 无图）。
        if let Some(archive) = &self.archive {
            if let Some(store) = archive.get() {
                if let Some(v) = store.get_by_id(id) {
                    let arch: ArchiveSubject = serde_json::from_value(v).ok()?;
                    if arch.id != 0 {
                        let mut subj = arch.to_bangumi_subject();
                        // 离线无图：在线补封面 URL（仅 URL 不下载；与普通搜索离线结果一致，
                        // 不受 seriesMetadata.thumbnail 配置限制）。
                        if subj.image.is_none() && subj.images.is_none() {
                            if let Ok(online) = self.client.get(id).await {
                                if online.image.is_some() || online.images.is_some() {
                                    subj.image = online.image;
                                    subj.images = online.images;
                                }
                            }
                        }
                        return Some(self.metadata_mapper.to_series_search_result(&subj));
                    }
                }
            }
        }
        // 在线回退
        let subj = self.client.get(id).await.ok()?;
        Some(self.metadata_mapper.to_series_search_result(&subj))
    }

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        let id: u64 = series_id.0.parse().map_err(|_| {
            ProviderError::message(format!("invalid Bangumi series id: {}", series_id.0))
        })?;
        // ① Archive 离线优先：元数据 + 单行本 relations 走离线；封面在线 API 取（archive 无图）。
        if let Some(archive) = &self.archive {
            if let Some(store) = archive.get() {
                if let Some(v) = store.get_by_id(id) {
                    let arch: ArchiveSubject = serde_json::from_value(v).unwrap_or_default();
                    if arch.id != 0 {
                        let relations = store
                            .get_related(id)
                            .into_iter()
                            .filter(|r| {
                                r.subject_type == Some(1)
                                    && r.relation.as_deref() == Some("单行本")
                            })
                            .map(|r| BangumiSubjectRelation {
                                id: r.id,
                                name: r.name,
                                name_cn: r.name_cn,
                                r#type: r.subject_type,
                                relation: r.relation,
                            })
                            .collect::<Vec<_>>();
                        let persons = store.get_persons(id);
                        let thumbnail = if self.fetch_series_covers {
                            match self.client.get(id).await {
                                Ok(s) => self
                                    .client
                                    .get_thumbnail(&s, 60 * 1024, None)
                                    .await
                                    .ok()
                                    .flatten(),
                                Err(_) => None,
                            }
                        } else {
                            None
                        };
                        return Ok(self.metadata_mapper.to_series_metadata_persons(
                            &arch.to_bangumi_subject(),
                            &relations,
                            thumbnail,
                            &persons,
                        ));
                    }
                }
            }
        }
        // ② 在线（未启用 archive / 未就绪 / 未命中）
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
        // ① Archive 离线优先（封面在线取）
        if let Some(archive) = &self.archive {
            if let Some(store) = archive.get() {
                if let Some(v) = store.get_by_id(id) {
                    let arch: ArchiveSubject = serde_json::from_value(v).unwrap_or_default();
                    if arch.id != 0 {
                        // 单行本 book 常无 persons 关联：回退系列 persons（同系列作者 name_cn 一致）
                        let mut persons = store.get_persons(id);
                        if persons.is_empty() {
                            if let Ok(sid) = _series_id.0.parse::<u64>() {
                                persons = store.get_persons(sid);
                            }
                        }
                        let thumbnail = if self.fetch_series_covers {
                            match self.client.get(id).await {
                                Ok(b) => self
                                    .client
                                    .get_thumbnail(&b, 30 * 1024, Some(1024 * 1024))
                                    .await
                                    .ok()
                                    .flatten(),
                                Err(_) => None,
                            }
                        } else {
                            None
                        };
                        return Ok(self.metadata_mapper.to_book_metadata_persons(
                            &arch.to_bangumi_subject(),
                            thumbnail,
                            &persons,
                        ));
                    }
                }
            }
        }
        // ② 在线
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
        // ① Archive 离线优先：series 过滤 + tag 过滤 + platform 过滤 + 相似度过滤（复用 matcher）
        if let Some(archive) = &self.archive {
            if let Some(store) = archive.get() {
                let results = store.search(series_name);
                let mut out: Vec<SeriesSearchResult> = Vec::new();
                for v in results {
                    if out.len() >= limit {
                        break;
                    }
                    let arch: ArchiveSubject = serde_json::from_value(v).unwrap_or_default();
                    if arch.id == 0 {
                        continue;
                    }
                    // BangumiKomga resort：非系列跳过（缺省视为系列，避免误伤单本漫画）
                    if arch.series == Some(false) {
                        continue;
                    }
                    if arch.tags.iter().any(|t| t.name == "漫画单行本") {
                        continue;
                    }
                    if let Some(platform) = platform_filter {
                        if arch.platform_str().as_deref() != Some(platform) {
                            continue;
                        }
                    }
                    let bs = arch.to_bangumi_subject();
                    let mut titles = vec![bs.name.clone()];
                    if let Some(cn) = &bs.name_cn {
                        titles.push(cn.clone());
                    }
                    // 别名/版本名参与相似度（顶层别名 + 嵌套别名/版本名，对齐 match_from_archive）
                    for item in &bs.infobox {
                        let is_alias_key = item.key.as_deref() == Some("别名")
                            || item.key.as_deref() == Some("別名");
                        if let Some(v) = item.value.as_ref() {
                            if is_alias_key {
                                match v {
                                    serde_json::Value::String(s) => titles.push(s.clone()),
                                    serde_json::Value::Array(items) => {
                                        for it in items {
                                            if let Some(v) = it.get("v").and_then(|v| v.as_str()) {
                                                titles.push(v.to_string());
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            if let serde_json::Value::Array(items) = v {
                                for sub in items {
                                    let k = sub.get("k").and_then(|k| k.as_str());
                                    if k == Some("别名") || k == Some("別名") || k == Some("版本名") {
                                        if let Some(v) = sub.get("v").and_then(|v| v.as_str()) {
                                            titles.push(v.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    let titles = self.variant_titles(titles);
                    if !self.name_matcher.matches(series_name, &titles) {
                        continue;
                    }
                    out.push(self.metadata_mapper.to_series_search_result(&bs));
                }
                if !out.is_empty() {
                    // 元数据离线 + 封面在线：离线命中后对每个结果在线补封面 URL；
                    // 在线失败保持 None（不影响搜索结果）。
                    for r in out.iter_mut() {
                        if r.image_url.is_some() {
                            continue;
                        }
                        let Ok(id) = r.result_id.parse::<u64>() else {
                            continue;
                        };
                        if let Ok(subject) = self.client.get(id).await {
                            r.image_url = BangumiClient::cover_url(&subject);
                        }
                    }
                    return Ok(out);
                }
            }
        }
        // ② 在线
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
        // ① Archive 离线优先（未就绪/未命中 → 在线）
        if let Some(archive) = &self.archive {
            if let Some(store) = archive.get() {
                if let Some(meta) = self.match_from_archive(&store, match_query).await? {
                    return Ok(Some(meta));
                }
            }
        }
        // ② 在线
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
                let titles = self.variant_titles(titles);
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
                    let titles = self.variant_titles(titles);
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

        // Rust 扩展：在线搜索命中后优先尝试从离线数据库读取对应 id 的数据并使用
        // （元数据离线 + persons name_cn 优先 + relations 离线；封面在线 API 补；未命中回退在线）。
        if let Some(archive) = &self.archive {
            if let Some(store) = archive.get() {
                if let Some(v) = store.get_by_id(subject.id) {
                    let arch: ArchiveSubject = serde_json::from_value(v).unwrap_or_default();
                    if arch.id != 0 {
                        let relations = store
                            .get_related(subject.id)
                            .into_iter()
                            .filter(|r| {
                                r.subject_type == Some(1)
                                    && r.relation.as_deref() == Some("单行本")
                            })
                            .map(|r| BangumiSubjectRelation {
                                id: r.id,
                                name: r.name,
                                name_cn: r.name_cn,
                                r#type: r.subject_type,
                                relation: r.relation,
                            })
                            .collect::<Vec<_>>();
                        let persons = store.get_persons(subject.id);
                        let thumbnail = if self.fetch_series_covers {
                            match self.client.get(subject.id).await {
                                Ok(s) => self
                                    .client
                                    .get_thumbnail(&s, 60 * 1024, None)
                                    .await
                                    .ok()
                                    .flatten(),
                                Err(_) => None,
                            }
                        } else {
                            None
                        };
                        return Ok(Some(self.metadata_mapper.to_series_metadata_persons(
                            &arch.to_bangumi_subject(),
                            &relations,
                            thumbnail,
                            &persons,
                        )));
                    }
                }
            }
        }

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
    /// 简繁归一化变体（Rust 扩展）：每个标题生成 [原, 繁→简, 简→繁] 保序去重，
    /// 使简繁任意写法都能相互命中（匹配层始终归一，不依赖 chineseConversion 配置）。
    fn variant_titles(&self, titles: Vec<String>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in titles {
            let v1 = self.chinese_t2s.convert(&t);
            let v2 = self.chinese_s2t.convert(&t);
            for v in [t, v1, v2] {
                if !v.is_empty() && !out.iter().any(|x| x == &v) {
                    out.push(v);
                }
            }
        }
        out
    }

    /// Kotlin `firstMatchingType`：逐个 getSubject 检查 platform（搜索响应项无 platform 字段）。：逐个 getSubject 检查 platform（搜索响应项无 platform 字段）。
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

    /// Archive 离线匹配：搜索 → series/tag/platform 过滤 → 相似度（name/name_cn/别名，
    /// 复用现有 matcher）→ 单/多候选（对齐在线 firstMatchingType 的 platform 语义）→
    /// 元数据（离线）+ 单行本 relations（离线）+ 封面（在线）。
    async fn match_from_archive(
        &self,
        store: &std::sync::Arc<crate::providers::bangumi_archive::BangumiArchiveStore>,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        let platform_filter = match_query
            .media_type
            .and_then(media_type_platform)
            .or_else(|| media_type_platform(self.media_type));
        let mut candidates: Vec<ArchiveSubject> = Vec::new();
        for v in store.search(&match_query.series_name) {
            let arch: ArchiveSubject = serde_json::from_value(v).unwrap_or_default();
            if arch.id == 0 {
                continue;
            }
            if arch.series == Some(false) {
                continue;
            }
            if arch.tags.iter().any(|t| t.name == "漫画单行本") {
                continue;
            }
            if let Some(platform) = platform_filter {
                if arch.platform_str().as_deref() != Some(platform) {
                    continue;
                }
            }
            candidates.push(arch);
        }
        let mut matches: Vec<ArchiveSubject> = candidates
            .into_iter()
            .filter(|arch| {
                let mut titles = vec![arch.name.clone()];
                if let Some(cn) = &arch.name_cn {
                    titles.push(cn.clone());
                }
                for item in &arch.infobox_items() {
                    let is_alias_key = item.key.as_deref() == Some("别名")
                        || item.key.as_deref() == Some("別名");
                    if let Some(v) = item.value.as_ref() {
                        if is_alias_key {
                            match v {
                                serde_json::Value::String(v) => titles.push(v.clone()),
                                serde_json::Value::Array(items) => {
                                    for it in items {
                                        if let Some(v) = it.get("v").and_then(|v| v.as_str()) {
                                            titles.push(v.to_string());
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        if let serde_json::Value::Array(items) = v {
                            for sub in items {
                                let k = sub.get("k").and_then(|k| k.as_str());
                                if k == Some("别名") || k == Some("別名") || k == Some("版本名") {
                                    if let Some(v) = sub.get("v").and_then(|v| v.as_str()) {
                                        titles.push(v.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
                let titles = self.variant_titles(titles);
                self.name_matcher.matches(
                    &match_query.normalized_series_name(),
                    &match_query.normalize_titles(&titles),
                )
            })
            .collect();
        if matches.is_empty() {
            return Ok(None);
        }
        // 单/多候选：对齐在线（1 → 直接取；多个 → platform 匹配优先，无则第一个）
        let arch = if matches.len() == 1 {
            matches.pop()
        } else {
            let idx = matches
                .iter()
                .position(|s| {
                    platform_filter.map_or(true, |p| s.platform_str().as_deref() == Some(p))
                })
                .unwrap_or(0);
            Some(matches.swap_remove(idx))
        };
        let Some(arch) = arch else {
            return Ok(None);
        };
        let id = arch.id;
        let relations = store
            .get_related(id)
            .into_iter()
            .filter(|r| r.subject_type == Some(1) && r.relation.as_deref() == Some("单行本"))
            .map(|r| BangumiSubjectRelation {
                id: r.id,
                name: r.name,
                name_cn: r.name_cn,
                r#type: r.subject_type,
                relation: r.relation,
            })
            .collect::<Vec<_>>();
        let persons = store.get_persons(id);
        let thumbnail = if self.fetch_series_covers {
            match self.client.get(id).await {
                Ok(s) => self
                    .client
                    .get_thumbnail(&s, 60 * 1024, None)
                    .await
                    .ok()
                    .flatten(),
                Err(_) => None,
            }
        } else {
            None
        };
        Ok(Some(self.metadata_mapper.to_series_metadata_persons(
            &arch.to_bangumi_subject(),
            &relations,
            thumbnail,
            &persons,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_person_roles_2002_2007_name_cn_priority() {
        // 182434 型：2002（作画）→ artist_roles、2007（原作）→ author_roles，name_cn 优先
        let persons = vec![
            PersonInfo {
                person_id: 1,
                name: "古屋庵".to_string(),
                name_cn: Some("古屋庵".to_string()),
                person_type: Some(1),
                career: vec!["mangaka".to_string()],
                position: Some(2002),
                appear_eps: String::new(),
                aliases: Vec::new(),
            },
            PersonInfo {
                person_id: 2,
                name: "るーすぼーい".to_string(),
                name_cn: Some("螺丝".to_string()),
                person_type: Some(1),
                career: vec!["writer".to_string()],
                position: Some(2007),
                appear_eps: String::new(),
                aliases: Vec::new(),
            },
        ];
        let authors = offline_person_authors(
            &persons,
            &[AuthorRole::Writer],
            &[AuthorRole::Penciller],
        );
        assert_eq!(authors.len(), 2);
        assert!(authors.contains(&Author {
            name: "古屋庵".to_string(),
            role: AuthorRole::Penciller,
        }));
        assert!(authors.contains(&Author {
            name: "螺丝".to_string(),
            role: AuthorRole::Writer,
        }));
        // 2001 → 全角色
        let all = vec![PersonInfo {
            person_id: 3,
            name: "藤本タツキ".to_string(),
            name_cn: Some("藤本树".to_string()),
            person_type: Some(1),
            career: vec!["mangaka".to_string()],
            position: Some(2001),
            appear_eps: String::new(),
            aliases: Vec::new(),
        }];
        let authors = offline_person_authors(&all, &[AuthorRole::Writer], &[AuthorRole::Penciller]);
        assert_eq!(authors.len(), 2);
        assert!(authors.iter().all(|a| a.name == "藤本树"));
    }

    /// 在线路径端到端：补足从白名单命中项取（182434 在线形态）
    /// 白名单命中 超能力/推理/校园（斗智 非白名单不引入；
    /// 漫画/るーすぼーい/古屋庵/2016/无能的奈奈 天然不在白名单）
    #[test]
    fn online_tags_valid_fill() {
        let mapper = BangumiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            crate::config::BookMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            load_bangumi_tag_whitelist(None),
        );
        let json = r#"{
            "id": 182434,
            "name": "無能なナナ",
            "name_cn": "无能的奈奈",
            "summary": null,
            "platform": "漫画",
            "date": "2016-05-12",
            "tags": [
                {"name": "漫画", "count": 197},
                {"name": "超能力", "count": 112},
                {"name": "推理", "count": 103},
                {"name": "るーすぼーい", "count": 60},
                {"name": "校园", "count": 59},
                {"name": "古屋庵", "count": 41},
                {"name": "2016", "count": 34},
                {"name": "无能的奈奈", "count": 27},
                {"name": "斗智", "count": 26}
            ],
            "infobox": [
                {"key": "原作", "value": "るーすぼーい"},
                {"key": "作画", "value": "古屋庵"},
                {"key": "出版社", "value": "スクウェア・エニックス"},
                {"key": "连载杂志", "value": "月刊少年ガンガン"}
            ]
        }"#;
        let subject: BangumiSubject = serde_json::from_str(json).unwrap();
        let md = mapper.to_series_metadata(&subject, &[], None);
        // 白名单命中 超能力/推理/校园（斗智 非白名单不引入；
        // 漫画/るーすぼーい/古屋庵/2016/无能的奈奈 天然不在白名单）
        assert_eq!(
            md.metadata.tags,
            vec!["超能力", "推理", "校园"]
        );
        // platform → genres
        assert_eq!(md.metadata.genres, vec!["漫画"]);
    }

    /// 在线 182434 完整 tags（30 个）：百合/奇幻 count 小被阈值过滤，但命中<10 →
    /// 白名单命中项全取，应包含 奇幻/百合（智斗/谋略/斗智 非白名单不引入）
    #[test]
    fn online_full_tags_fill_includes_fantasy_yuri() {
        let mapper = BangumiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            crate::config::BookMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            load_bangumi_tag_whitelist(None),
        );
        let json = r#"{
            "id": 182434,
            "name": "無能なナナ",
            "name_cn": "无能的奈奈",
            "summary": null,
            "date": "2016-05-12",
            "tags": [
                {"name": "漫画", "count": 198},
                {"name": "超能力", "count": 112},
                {"name": "推理", "count": 104},
                {"name": "悬疑", "count": 89},
                {"name": "智斗", "count": 71},
                {"name": "るーすぼーい", "count": 60},
                {"name": "校园", "count": 59},
                {"name": "古屋庵", "count": 41},
                {"name": "2016", "count": 35},
                {"name": "无能的奈奈", "count": 27},
                {"name": "斗智", "count": 26},
                {"name": "战斗", "count": 21},
                {"name": "谋略", "count": 17},
                {"name": "已完结", "count": 12},
                {"name": "日本", "count": 11},
                {"name": "月刊少年ガンガン", "count": 7},
                {"name": "漫画系列", "count": 6},
                {"name": "系列", "count": 5},
                {"name": "奇幻", "count": 4},
                {"name": "Square_Enix", "count": 4},
                {"name": "百合", "count": 3},
                {"name": "连载中", "count": 3},
                {"name": "原创", "count": 2},
                {"name": "未完结", "count": 2},
                {"name": "生存", "count": 2}
            ],
            "infobox": [
                {"key": "原作", "value": "るーすぼーい"},
                {"key": "作画", "value": "古屋庵"},
                {"key": "出版社", "value": "スクウェア・エニックス"},
                {"key": "连载杂志", "value": "月刊少年ガンガン"}
            ]
        }"#;
        let subject: BangumiSubject = serde_json::from_str(json).unwrap();
        let md = mapper.to_series_metadata(&subject, &[], None);
        let tags = md.metadata.tags;
        // 白名单命中 9 个（阈值 15 过滤后 7 个 <10）→ 白名单命中项全取：
        // 超能力/推理/悬疑/智斗/校园/战斗/奇幻/百合/生存（谋略/斗智 非白名单不引入）
        assert_eq!(
            tags,
            vec!["超能力", "推理", "悬疑", "智斗", "校园", "战斗", "奇幻", "百合", "生存"]
        );
    }

    /// Archive 模板字符串 infobox → 解析为数组 → extract_authors 拆分多作者
    /// （作者顿号分隔 4 人；插图 → artistRoles；与在线 v0 API 同构）
    #[test]
    fn archive_infobox_author_keys_end_to_end() {
        use crate::providers::bangumi_archive::parse_archive_infobox;
        let raw = "{{Infobox animanga/Novel\r\n|作者= 麻枝准、涼元悠一、魁、丘野塔也\r\n|插图= ごとP\r\n}}";
        let items = parse_archive_infobox(raw);
        let subject = BangumiSubject {
            id: 48,
            name: "X".to_string(),
            name_cn: None,
            image: None,
            summary: None,
            date: None,
            images: None,
            rating: None,
            rank: None,
            subject_type: None,
            platform: Some("小说".to_string()),
            tags: Vec::new(),
            volumes: None,
            eps: None,
            total_episodes: None,
            infobox: items,
            eps_info: None,
            age_rating: None,
            nsfw: None,
        };
        let authors = extract_authors(&subject, &[AuthorRole::Writer], &[AuthorRole::Penciller]);
        for name in ["麻枝准", "涼元悠一", "魁", "丘野塔也"] {
            assert!(
                authors.contains(&Author {
                    name: name.to_string(),
                    role: AuthorRole::Writer
                }),
                "missing writer {name}"
            );
        }
        assert!(
            authors.contains(&Author {
                name: "ごとP".to_string(),
                role: AuthorRole::Penciller
            }),
            "missing illustrator ごとP"
        );
    }

    /// 漫画/小说职务键全覆盖：作者/原作/作画/人物原案/脚本/原案/分镜/插图/上色等
    #[test]
    fn extract_authors_full_role_keys() {
        let subject: BangumiSubject = serde_json::from_str(
            r#"{"id":1,"name":"X","name_cn":null,"summary":null,"tags":[],"platform":"漫画","infobox":[
                {"key":"作者","value":"A"},
                {"key":"原作","value":"B"},
                {"key":"脚本","value":"C"},
                {"key":"原案","value":"D"},
                {"key":"作画","value":"E"},
                {"key":"插图","value":"F"},
                {"key":"上色","value":"G"}
            ]}"#,
        )
        .unwrap();
        let authors = extract_authors(&subject, &[AuthorRole::Writer], &[AuthorRole::Penciller]);
        // 作者 → Writer+Penciller；原作/脚本/原案 → Writer；作画/插图/上色 → Penciller
        for (name, roles) in [
            ("A", vec![AuthorRole::Writer, AuthorRole::Penciller]),
            ("B", vec![AuthorRole::Writer]),
            ("C", vec![AuthorRole::Writer]),
            ("D", vec![AuthorRole::Writer]),
            ("E", vec![AuthorRole::Penciller]),
            ("F", vec![AuthorRole::Penciller]),
            ("G", vec![AuthorRole::Colorist]),
        ] {
            for role in roles {
                assert!(
                    authors.contains(&Author { name: name.to_string(), role }),
                    "missing {name} {role:?}"
                );
            }
        }
    }

    /// offline_person_authors 补全：2003 插图 / 2009 画师 → artistRoles；2010 脚本 → authorRoles
    #[test]
    fn offline_person_extra_positions() {
        let mk = |pid: u64, name: &str, pos: i32| PersonInfo {
            person_id: pid,
            name: name.to_string(),
            name_cn: None,
            person_type: Some(1),
            career: Vec::new(),
            position: Some(pos),
            appear_eps: String::new(),
            aliases: Vec::new(),
        };
        let persons = vec![
            mk(1, "插画师A", 2003),
            mk(2, "画师B", 2009),
            mk(3, "脚本C", 2010),
        ];
        let authors = offline_person_authors(&persons, &[AuthorRole::Writer], &[AuthorRole::Penciller]);
        assert!(authors.contains(&Author { name: "插画师A".to_string(), role: AuthorRole::Penciller }));
        assert!(authors.contains(&Author { name: "画师B".to_string(), role: AuthorRole::Penciller }));
        assert!(authors.contains(&Author { name: "脚本C".to_string(), role: AuthorRole::Writer }));
        assert_eq!(authors.len(), 3);
    }

    #[test]
    fn variant_titles_simplified_traditional_normalized() {
        let provider = BangumiMetadataProvider {
            client: BangumiClient::new(
                crate::providers::client_with_default_headers(reqwest::header::HeaderMap::new()),
            ),
            metadata_mapper: BangumiMetadataMapper::new(
                crate::config::SeriesMetadataConfig::default(),
                crate::config::BookMetadataConfig::default(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
            name_matcher: NameSimilarityMatcher::default(),
            fetch_series_covers: false,
            media_type: crate::model::MediaType::Manga,
            archive: None,
            chinese_t2s: ChineseConverter::new(ChineseDirection::T2s).unwrap(),
            chinese_s2t: ChineseConverter::new(ChineseDirection::S2t).unwrap(),
        };
        let variants = provider.variant_titles(vec!["三月的狮子".to_string()]);
        // 简体原样 + 繁→简（不变）+ 简→繁
        assert!(variants.iter().any(|v| v == "三月的狮子"));
        assert!(variants.iter().any(|v| v == "三月的獅子"));
        // 繁体 query 能命中简体候选（双向归一）
        assert!(provider
            .name_matcher
            .matches("三月的獅子", &provider.variant_titles(vec!["三月的狮子".to_string()])));
        // 简体 query 能命中繁体候选
        assert!(provider
            .name_matcher
            .matches("三月的狮子", &provider.variant_titles(vec!["三月的獅子".to_string()])));
    }

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
    fn normalize_date_supports_variants() {
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
    fn age_rating_api_and_tag_inference() {
        // API 优先
        assert_eq!(map_bangumi_age_rating(Some(0), None, &[]), Some(0));
        assert_eq!(map_bangumi_age_rating(Some(1), None, &[]), Some(15));
        assert_eq!(map_bangumi_age_rating(Some(2), None, &[]), Some(18));
        assert_eq!(map_bangumi_age_rating(Some(9), None, &[]), None);
        // API 未知值不推断（权威优先）
        assert_eq!(
            map_bangumi_age_rating(Some(9), Some(true), &["エロ".to_string()]),
            None
        );
        // 无 API → nsfw 标记
        assert_eq!(map_bangumi_age_rating(None, Some(true), &[]), Some(18));
        // nsfw=false：明确的非成人声明 → None（不做标签推断）
        assert_eq!(map_bangumi_age_rating(None, Some(false), &["エロ".to_string()]), None);
        // 无 API 无 nsfw → 标签推断
        assert_eq!(map_bangumi_age_rating(None, None, &["恋爱".to_string()]), None);
        assert_eq!(map_bangumi_age_rating(None, None, &["エロ".to_string()]), Some(18));
        assert_eq!(map_bangumi_age_rating(None, None, &["恋爱".to_string(), "官能".to_string()]), Some(18));
        assert_eq!(map_bangumi_age_rating(None, None, &["R18".to_string()]), Some(18));
        // 擦边词不推断
        assert_eq!(map_bangumi_age_rating(None, None, &["卖肉".to_string()]), None);
        assert_eq!(map_bangumi_age_rating(None, None, &["NTR".to_string()]), None);
    }

    #[test]
    fn classify_status_matches_variants() {
        assert_eq!(
            classify_bangumi_status("连载中"),
            Some(SeriesStatus::Ongoing)
        );
        assert_eq!(classify_bangumi_status("连载"), Some(SeriesStatus::Ongoing));
        assert_eq!(classify_bangumi_status("完结"), Some(SeriesStatus::Ended));
        assert_eq!(classify_bangumi_status("已完结"), Some(SeriesStatus::Ended));
        assert_eq!(classify_bangumi_status("停刊"), Some(SeriesStatus::Hiatus));
        assert_eq!(classify_bangumi_status("腰斩"), Some(SeriesStatus::Ended));
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
        // max=500 > 200 → 阈值 35：热血/搞笑 ≥35 保留（2 个 <10）→ 白名单命中项全取：
        // 白名单命中项全取（非白名单999 不引入）→ 热血/搞笑
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

    /// 标签处理：阈值过滤后不足 10 个 → 取白名单命中项前 10
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

    /// 182434 场景：白名单命中 4 个 <10 → 白名单命中项全取
    /// （智斗/斗智 非白名单不引入；漫画/作者 るーすぼーい・古屋庵/年份 2016/系列名 无能的奈奈 天然不在白名单）
    #[test]
    fn filter_tags_fill_from_valid() {
        let tags: Vec<(String, i32)> = vec![
            ("漫画".to_string(), 197),
            ("超能力".to_string(), 112),
            ("推理".to_string(), 103),
            ("悬疑".to_string(), 88),
            ("智斗".to_string(), 71),
            ("るーすぼーい".to_string(), 60),
            ("校园".to_string(), 59),
            ("古屋庵".to_string(), 41),
            ("2016".to_string(), 34),
            ("无能的奈奈".to_string(), 27),
            ("斗智".to_string(), 26),
        ];
        // 白名单命中：超能力/推理/悬疑/校园/智斗（5 个）→ 白名单命中项全取；
        // 斗智 非白名单不引入（漫画/作者/年份/系列名 天然不在白名单）
        let whitelist = load_bangumi_tag_whitelist(None);
        let out = filter_bangumi_tags(&tags, &whitelist);
        assert_eq!(out, vec!["超能力", "推理", "悬疑", "智斗", "校园"]);
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
