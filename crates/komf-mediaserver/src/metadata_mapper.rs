//! 元数据映射 —— 对应 `MetadataMapper.kt`。
use crate::model::*;
use komf_core::model::{Author, AuthorRole, BookMetadata, SeriesMetadata};

pub struct MetadataMapper;

impl MetadataMapper {
    /// 对应 `toBookMetadataUpdate`。
    pub fn to_book_metadata_update(
        &self,
        book_metadata: Option<&BookMetadata>,
        series_metadata: Option<&SeriesMetadata>,
        book: &MediaServerBook,
    ) -> MediaServerBookMetadataUpdate {
        let current = &book.metadata;

        let authors: Option<Vec<MediaServerAuthor>> = {
            let from_book = book_metadata.and_then(|m| if m.authors.is_empty() { None } else { Some(m.authors.clone()) });
            let from_series = series_metadata.and_then(|m| if m.authors.is_empty() { None } else { Some(m.authors.clone()) });
            from_book.or(from_series).map(|authors| {
                authors
                    .iter()
                    .map(|a| MediaServerAuthor {
                        name: a.name.clone(),
                        role: role_to_string(&a.role),
                    })
                    .collect()
            })
        };

        MediaServerBookMetadataUpdate {
            title: get_if_not_locked_or_empty(book_metadata.and_then(|m| m.title.clone()), current.title_lock),
            summary: get_if_not_locked_or_empty(book_metadata.and_then(|m| m.summary.clone()), current.summary_lock),
            release_date: get_if_not_locked_or_empty(
                book_metadata.and_then(|m| m.release_date.clone()),
                current.release_date_lock,
            ),
            authors: get_if_not_locked_or_empty(authors, current.authors_lock),
            tags: get_if_not_locked_or_empty(
                book_metadata.map(|m| m.tags.clone()).filter(|t| !t.is_empty()),
                current.tags_lock,
            ),
            isbn: get_if_not_locked_or_empty(book_metadata.and_then(|m| m.isbn.clone()), current.isbn_lock),
            links: get_if_not_locked_or_empty(
                book_metadata.map(|m| m.links.clone()).filter(|l| !l.is_empty()),
                current.links_lock,
            ),
            // ignore lock since we can't know if komf was the one to lock number
            number: book_metadata.and_then(|m| m.number.as_ref()).map(|n| n.to_string()),
            number_sort: book_metadata.and_then(|m| m.number.as_ref()).map(|n| n.start),
            // lock if number is not null; do not unlock if was locked
            number_lock: Some(current.number_lock || book_metadata.and_then(|m| m.number.as_ref()).is_some()),
            number_sort_lock: Some(current.number_sort_lock || book_metadata.and_then(|m| m.number_sort.as_ref()).is_some()),
            ..Default::default()
        }
    }

    /// 对应 `toSeriesMetadataUpdate`。
    pub fn to_series_metadata_update(
        &self,
        patch: &SeriesMetadata,
        metadata: &MediaServerSeriesMetadata,
    ) -> MediaServerSeriesMetadataUpdate {
        let authors: Option<Vec<MediaServerAuthor>> = if patch.authors.is_empty() {
            None
        } else {
            Some(
                patch
                    .authors
                    .iter()
                    .map(|a| MediaServerAuthor {
                        name: a.name.clone(),
                        role: role_to_string(&a.role),
                    })
                    .collect(),
            )
        };

        let alternative_titles: Option<Vec<(String, Option<komf_core::model::TitleType>, Option<String>)>> = {
            // 对齐 Kotlin `patch.titles.filter { it != patch.title }`：按 SeriesTitle 全等
            // （name + type + language）剔除主标题，而非仅按 name。
            let titles = patch
                .titles
                .iter()
                .filter(|t| match &patch.title {
                    Some(pt) => *t != pt,
                    None => true,
                })
                .map(|t| (t.name.clone(), t.r#type, t.language.clone()))
                .collect::<Vec<_>>();
            if titles.is_empty() { None } else { Some(titles) }
        };

        MediaServerSeriesMetadataUpdate {
            status: get_if_not_locked_or_empty(patch.status, metadata.status_lock),
            title: get_if_not_locked_or_empty(patch.title.clone(), metadata.title_lock),
            title_sort: get_if_not_locked_or_empty(patch.title.clone(), metadata.title_sort_lock),
            alternative_titles: get_if_not_locked_or_empty(alternative_titles, metadata.title_sort_lock),
            summary: get_if_not_locked_or_empty(patch.summary.clone(), metadata.summary_lock),
            publisher: get_if_not_locked_or_empty(patch.publisher.as_ref().map(|p| p.name.clone()), metadata.publisher_lock),
            alternative_publishers: get_if_not_locked_or_empty(
                Some(patch.alternative_publishers.iter().map(|p| p.name.clone()).collect::<Vec<_>>())
                    .filter(|p| !p.is_empty()),
                metadata.publisher_lock,
            ),
            reading_direction: get_if_not_locked_or_empty(patch.reading_direction, metadata.reading_direction_lock),
            age_rating: get_if_not_locked_or_empty(patch.age_rating, metadata.age_rating_lock),
            language: get_if_not_locked_or_empty(patch.language.clone(), metadata.language_lock),
            genres: get_if_not_locked_or_empty(Some(patch.genres.clone()).filter(|g| !g.is_empty()), metadata.genres_lock),
            tags: get_if_not_locked_or_empty(Some(patch.tags.clone()).filter(|t| !t.is_empty()), metadata.tags_lock),
            total_book_count: get_if_not_locked_or_empty(patch.total_book_count, metadata.total_book_count_lock),
            authors: get_if_not_locked_or_empty(authors, metadata.authors_lock),
            release_year: get_if_not_locked_or_empty(patch.release_date.as_ref().and_then(|d| d.year), metadata.release_year_lock),
            links: get_if_not_locked_or_empty(Some(patch.links.clone()).filter(|l| !l.is_empty()), metadata.links_lock),
            ..Default::default()
        }
    }
}

fn role_to_string(role: &AuthorRole) -> String {
    match role {
        AuthorRole::Writer => "WRITER",
        AuthorRole::Penciller => "PENCILLER",
        AuthorRole::Inker => "INKER",
        AuthorRole::Colorist => "COLORIST",
        AuthorRole::Letterer => "LETTERER",
        AuthorRole::Cover => "COVER",
        AuthorRole::Editor => "EDITOR",
        AuthorRole::Translator => "TRANSLATOR",
    }
    .to_string()
}

fn get_if_not_locked_or_empty<T>(patched: Option<T>, lock: bool) -> Option<T> {
    match patched {
        Some(v) if !lock => Some(v),
        _ => None,
    }
}

/// ComicInfo 年龄评级映射辅助（对应 Kotlin `AgeRating` 枚举全部非空条目，
/// 按 ageRating 升序排列；同 ageRating 时取枚举声明顺序靠前的条目）。
#[derive(Debug, Clone, Copy)]
pub enum AgeRating {
    Everyone,
    EarlyChildhood,
    KidsToAdults,
    PG,
    Everyone10,
    Teen,
    Ma15,
    M,
    AdultsOnly18,
}

impl AgeRating {
    pub fn age_rating(&self) -> Option<i32> {
        match self {
            AgeRating::Everyone => Some(0),
            AgeRating::EarlyChildhood => Some(3),
            AgeRating::KidsToAdults => Some(6),
            AgeRating::PG => Some(8),
            AgeRating::Everyone10 => Some(10),
            AgeRating::Teen => Some(13),
            AgeRating::Ma15 => Some(15),
            AgeRating::M => Some(17),
            AgeRating::AdultsOnly18 => Some(18),
        }
    }

    pub fn value(&self) -> &'static str {
        match self {
            AgeRating::Everyone => "Everyone",
            AgeRating::EarlyChildhood => "Early Childhood",
            AgeRating::KidsToAdults => "Kids to Adults",
            AgeRating::PG => "PG",
            AgeRating::Everyone10 => "Everyone 10+",
            AgeRating::Teen => "Teen",
            AgeRating::Ma15 => "MA15+",
            AgeRating::M => "M",
            AgeRating::AdultsOnly18 => "Adults Only 18+",
        }
    }
}

/// 将数字年龄评级映射为 ComicInfo 字符串 —— 对应 Kotlin 中的映射逻辑。
pub fn age_rating_to_comic_info(metadata_rating: i32) -> String {
    let mut entries: Vec<(i32, AgeRating)> = AgeRating::all()
        .iter()
        .filter_map(|r| r.age_rating().map(|a| (a, *r)))
        .collect();
    entries.sort_by_key(|(a, _)| *a);
    let matched = entries
        .iter()
        .find(|(age, _)| *age >= metadata_rating)
        .map(|(_, r)| *r)
        .unwrap_or(AgeRating::AdultsOnly18);
    matched.value().to_string()
}

impl AgeRating {
    pub fn all() -> [AgeRating; 9] {
        [
            AgeRating::Everyone,
            AgeRating::EarlyChildhood,
            AgeRating::KidsToAdults,
            AgeRating::PG,
            AgeRating::Everyone10,
            AgeRating::Teen,
            AgeRating::Ma15,
            AgeRating::M,
            AgeRating::AdultsOnly18,
        ]
    }
}

/// 由作者列表构造 ComicInfo 的字段（按角色分组）—— 对应 `toComicInfo` 中作者部分。
pub fn authors_to_comic_info_fields(authors: &[Author]) -> ComicInfoAuthorFields {
    ComicInfoAuthorFields {
        writer: join_by_role(authors, AuthorRole::Writer),
        penciller: join_by_role(authors, AuthorRole::Penciller),
        inker: join_by_role(authors, AuthorRole::Inker),
        colorist: join_by_role(authors, AuthorRole::Colorist),
        letterer: join_by_role(authors, AuthorRole::Letterer),
        cover_artist: join_by_role(authors, AuthorRole::Cover),
        editor: join_by_role(authors, AuthorRole::Editor),
        translator: join_by_role(authors, AuthorRole::Translator),
    }
}

#[derive(Debug, Clone, Default)]
pub struct ComicInfoAuthorFields {
    pub writer: Option<String>,
    pub penciller: Option<String>,
    pub inker: Option<String>,
    pub colorist: Option<String>,
    pub letterer: Option<String>,
    pub cover_artist: Option<String>,
    pub editor: Option<String>,
    pub translator: Option<String>,
}

fn join_by_role(authors: &[Author], role: AuthorRole) -> Option<String> {
    let names: Vec<String> = authors.iter().filter(|a| a.role == role).map(|a| a.name.clone()).collect();
    if names.is_empty() {
        None
    } else {
        Some(names.join(","))
    }
}
