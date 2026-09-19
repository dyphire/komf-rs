//! 元数据合并 —— 对应 `MetadataMerger.kt`。
use komf_core::model::{BookMetadata, SeriesMetadata, WebLink};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub struct MetadataMerger {
    pub merge_tags: bool,
    pub merge_genres: bool,
}

impl MetadataMerger {
    pub fn new(merge_tags: bool, merge_genres: bool) -> Self {
        Self {
            merge_tags,
            merge_genres,
        }
    }

    /// 对应 `mergeSeriesMetadata`。
    ///
    /// 对齐 Kotlin：
    /// - `title` 字段不合并（Kotlin 构造函数未传该参数，结果恒为 null）；
    /// - `titles` 为 original + new 拼接；
    /// - `links` 为 (old + new).distinctBy { label }；
    /// - tags/genres 在 merge 配置开启时为 (old + new).toSet().sorted()，否则 old.ifEmpty { new }。
    pub fn merge_series_metadata(&self, original: &SeriesMetadata, new: &SeriesMetadata) -> SeriesMetadata {
        SeriesMetadata {
            status: original.status.or(new.status),
            title: None,
            titles: original
                .titles
                .iter()
                .chain(new.titles.iter())
                .cloned()
                .collect(),
            summary: original.summary.clone().or_else(|| new.summary.clone()),
            publisher: original.publisher.clone().or_else(|| new.publisher.clone()),
            alternative_publishers: if original.alternative_publishers.is_empty() {
                new.alternative_publishers.clone()
            } else {
                original.alternative_publishers.clone()
            },
            reading_direction: original.reading_direction.or(new.reading_direction),
            age_rating: original.age_rating.or(new.age_rating),
            language: original.language.clone().or_else(|| new.language.clone()),
            genres: if self.merge_genres {
                merge_unique_sorted(&original.genres, &new.genres)
            } else if original.genres.is_empty() {
                new.genres.clone()
            } else {
                original.genres.clone()
            },
            tags: if self.merge_tags {
                merge_unique_sorted(&original.tags, &new.tags)
            } else if original.tags.is_empty() {
                new.tags.clone()
            } else {
                original.tags.clone()
            },
            total_book_count: original.total_book_count.or(new.total_book_count),
            authors: if original.authors.is_empty() {
                new.authors.clone()
            } else {
                original.authors.clone()
            },
            release_date: original.release_date.clone().or_else(|| new.release_date.clone()),
            links: distinct_by_label(
                original
                    .links
                    .iter()
                    .chain(new.links.iter())
                    .cloned()
                    .collect(),
            ),
            score: original.score.or(new.score),
            thumbnail: original.thumbnail.clone().or_else(|| new.thumbnail.clone()),
        }
    }

    /// 对应 `mergeBookMetadata`（map 级）。
    ///
    /// Kotlin 的 (old + new).distinct().groupBy(key) 每 key 至多两个条目（两 map 各一），
    /// 逐条 reduce 等价于：None+Some → Some；Some+Some → merge；None+None → None。
    /// distinct 仅在 (key, value) 完全相同去重，而单书 merge 幂等，故行为等价。
    pub fn merge_book_metadata(
        &self,
        original: &HashMap<String, Option<BookMetadata>>,
        new: &HashMap<String, Option<BookMetadata>>,
    ) -> HashMap<String, Option<BookMetadata>> {
        let mut result = original.clone();
        for (book_id, new_metadata) in new {
            let entry = result.entry(book_id.clone()).or_insert(None);
            match (entry.as_ref(), new_metadata) {
                (None, Some(nm)) => {
                    *entry = Some(nm.clone());
                }
                (Some(om), Some(nm)) => {
                    *entry = Some(self.merge_book_metadata_fields(om, nm));
                }
                _ => {}
            }
        }
        result
    }

    /// 对应单书 `mergeBookMetadata`。
    ///
    /// 对齐 Kotlin：book tags 恒 `old.ifEmpty { new }`（不受 mergeTags 控制）；
    /// links 为 old + new 拼接；storyArcs 不参与合并（结果恒 None）。
    fn merge_book_metadata_fields(&self, original: &BookMetadata, new: &BookMetadata) -> BookMetadata {
        BookMetadata {
            title: original.title.clone().or_else(|| new.title.clone()),
            summary: original.summary.clone().or_else(|| new.summary.clone()),
            number: original.number.clone().or(new.number.clone()),
            number_sort: original.number_sort.or(new.number_sort),
            release_date: original.release_date.clone().or_else(|| new.release_date.clone()),
            authors: if original.authors.is_empty() {
                new.authors.clone()
            } else {
                original.authors.clone()
            },
            tags: if original.tags.is_empty() {
                new.tags.clone()
            } else {
                original.tags.clone()
            },
            isbn: original.isbn.clone().or_else(|| new.isbn.clone()),
            links: original
                .links
                .iter()
                .chain(new.links.iter())
                .cloned()
                .collect(),
            chapters: if original.chapters.is_empty() {
                new.chapters.clone()
            } else {
                original.chapters.clone()
            },
            story_arcs: None,
            start_chapter: original.start_chapter.or(new.start_chapter),
            end_chapter: original.end_chapter.or(new.end_chapter),
            thumbnail: original.thumbnail.clone().or_else(|| new.thumbnail.clone()),
        }
    }
}

/// (old + new).toSet().sorted()。
fn merge_unique_sorted(original: &[String], new: &[String]) -> Vec<String> {
    let mut result: Vec<String> = original.iter().chain(new.iter()).cloned().collect();
    result.sort();
    result.dedup();
    result
}

/// distinctBy { it.label }。
fn distinct_by_label(links: Vec<WebLink>) -> Vec<WebLink> {
    let mut seen = std::collections::HashSet::new();
    links
        .into_iter()
        .filter(|link| seen.insert(link.label.clone()))
        .collect()
}
