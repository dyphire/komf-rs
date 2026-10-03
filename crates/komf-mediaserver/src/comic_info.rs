//! ComicInfo 写入器 —— 对应 `snd.komf.comicinfo` 包。
//!
//! 将 ComicInfo.xml 写入 CBZ/ZIP 书籍文件（保留其余条目），
//! 或从书籍文件中移除 ComicInfo.xml。
use crate::metadata_mapper::ComicInfoAuthorFields;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComicInfo {
    pub title: Option<String>,
    pub series: Option<String>,
    pub number: Option<String>,
    pub count: Option<i32>,
    pub volume: Option<i32>,
    pub alternate_series: Option<String>,
    pub alternate_number: Option<String>,
    pub alternate_count: Option<i32>,
    pub summary: Option<String>,
    pub notes: Option<String>,
    pub year: Option<i32>,
    pub month: Option<u32>,
    pub day: Option<u32>,
    pub writer: Option<String>,
    pub penciller: Option<String>,
    pub inker: Option<String>,
    pub colorist: Option<String>,
    pub letterer: Option<String>,
    pub cover_artist: Option<String>,
    pub editor: Option<String>,
    pub translator: Option<String>,
    pub publisher: Option<String>,
    pub imprint: Option<String>,
    pub genre: Option<String>,
    pub tags: Option<String>,
    pub web: Option<String>,
    pub page_count: Option<i32>,
    pub language_iso: Option<String>,
    pub format: Option<String>,
    pub black_and_white: Option<String>,
    pub manga: Option<String>,
    pub characters: Option<String>,
    pub teams: Option<String>,
    pub locations: Option<String>,
    pub scan_information: Option<String>,
    pub story_arc: Option<String>,
    pub story_arc_number: Option<String>,
    pub series_group: Option<String>,
    pub age_rating: Option<String>,
    pub rating: Option<String>,
    pub localized_series: Option<String>,
    pub gtin: Option<String>,
    pub pages: Option<Vec<Page>>,
}

/// 对应 Kotlin `Page`（ComicInfo.xml `<Pages><Page>…</Page></Pages>`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub image: Option<i32>,
    pub r#type: Option<String>,
    pub double_page: Option<bool>,
    pub image_size: Option<i64>,
    pub key: Option<String>,
    pub bookmark: Option<String>,
    pub image_width: Option<i32>,
    pub image_height: Option<i32>,
}

/// 解析 "YYYY-MM-DD"（或年份部分）为 (year, month, day)。
fn parse_release_date(date: Option<&str>) -> (Option<i32>, Option<u32>, Option<u32>) {
    match date {
        Some(date) => {
            let mut parts = date.split('-');
            let year = parts.next().and_then(|p| p.parse::<i32>().ok());
            let month = parts.next().and_then(|p| p.parse::<u32>().ok());
            let day = parts.next().and_then(|p| p.parse::<u32>().ok());
            (year, month, day)
        }
        None => (None, None, None),
    }
}

/// 从系列元数据构造 ComicInfo —— 对应 `MetadataMapper.toComicInfo`。
pub fn comic_info_from_metadata(
    book_metadata: Option<&komf_core::model::BookMetadata>,
    series_metadata: Option<&komf_core::model::SeriesMetadata>,
    author_fields: ComicInfoAuthorFields,
) -> Option<ComicInfo> {
    if book_metadata.is_none() && series_metadata.is_none() {
        return None;
    }
    let book = book_metadata;
    let series = series_metadata;
    let (year, month, day) = parse_release_date(book.and_then(|b| b.release_date.as_deref()));

    Some(ComicInfo {
        title: book.and_then(|b| b.title.clone()),
        series: series.and_then(|s| s.title_name()),
        number: book.and_then(|b| b.number.as_ref()).map(|n| n.to_string()),
        count: series.and_then(|s| s.total_book_count),
        summary: book.and_then(|b| b.summary.clone()),
        year,
        month,
        day,
        writer: author_fields.writer,
        penciller: author_fields.penciller,
        inker: author_fields.inker,
        colorist: author_fields.colorist,
        letterer: author_fields.letterer,
        cover_artist: author_fields.cover_artist,
        editor: author_fields.editor,
        translator: author_fields.translator,
        publisher: series
            .and_then(|s| s.publisher.as_ref())
            .map(|p| p.name.clone()),
        genre: series.and_then(|s| {
            if s.genres.is_empty() {
                None
            } else {
                Some(s.genres.join(","))
            }
        }),
        tags: book.and_then(|b| {
            if b.tags.is_empty() {
                None
            } else {
                Some(b.tags.join(","))
            }
        }),
        age_rating: series
            .and_then(|s| s.age_rating)
            .map(crate::metadata_mapper::age_rating_to_comic_info),
        language_iso: series.and_then(|s| s.language.clone()),
        localized_series: series.and_then(|s| {
            s.titles
                .iter()
                .find(|t| t.r#type.is_some())
                .map(|t| t.name.clone())
        }),
        story_arc: book.and_then(|b| b.story_arcs.as_ref()).and_then(|arcs| {
            if arcs.is_empty() {
                None
            } else {
                Some(
                    arcs.iter()
                        .map(|a| a.name.clone())
                        .collect::<Vec<_>>()
                        .join(","),
                )
            }
        }),
        story_arc_number: book.and_then(|b| b.story_arcs.as_ref()).and_then(|arcs| {
            if arcs.is_empty() {
                None
            } else {
                Some(
                    arcs.iter()
                        .map(|a| a.number.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                )
            }
        }),
        gtin: book
            .and_then(|b| b.isbn.clone())
            .filter(|i| !i.trim().is_empty()),
        ..Default::default()
    })
}

/// 系列版 ComicInfo（对应 `toSeriesComicInfo`）。
pub fn series_comic_info(
    series_metadata: &komf_core::model::SeriesMetadata,
    book_metadata: Option<&komf_core::model::BookMetadata>,
    author_fields: ComicInfoAuthorFields,
) -> ComicInfo {
    ComicInfo {
        title: book_metadata.and_then(|b| b.title.clone()),
        series: series_metadata.title_name(),
        number: book_metadata
            .and_then(|b| b.number.as_ref())
            .map(|n| n.to_string()),
        count: series_metadata.total_book_count,
        summary: series_metadata.summary.clone(),
        year: series_metadata.release_date.as_ref().and_then(|d| d.year),
        month: series_metadata.release_date.as_ref().and_then(|d| d.month),
        day: series_metadata.release_date.as_ref().and_then(|d| d.day),
        writer: author_fields.writer,
        penciller: author_fields.penciller,
        inker: author_fields.inker,
        colorist: author_fields.colorist,
        letterer: author_fields.letterer,
        cover_artist: author_fields.cover_artist,
        editor: author_fields.editor,
        translator: author_fields.translator,
        publisher: series_metadata.publisher.as_ref().map(|p| p.name.clone()),
        genre: if series_metadata.genres.is_empty() {
            None
        } else {
            Some(series_metadata.genres.join(","))
        },
        tags: if series_metadata.tags.is_empty() {
            None
        } else {
            Some(series_metadata.tags.join(","))
        },
        age_rating: series_metadata
            .age_rating
            .map(crate::metadata_mapper::age_rating_to_comic_info),
        language_iso: series_metadata.language.clone(),
        localized_series: series_metadata
            .titles
            .iter()
            .find(|t| t.r#type.is_some())
            .map(|t| t.name.clone()),
        story_arc: book_metadata
            .and_then(|b| b.story_arcs.as_ref())
            .map(|arcs| {
                arcs.iter()
                    .map(|a| a.name.clone())
                    .collect::<Vec<_>>()
                    .join(",")
            }),
        story_arc_number: book_metadata
            .and_then(|b| b.story_arcs.as_ref())
            .map(|arcs| {
                arcs.iter()
                    .map(|a| a.number.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            }),
        gtin: book_metadata.and_then(|b| b.isbn.clone()),
        ..Default::default()
    }
}

impl ComicInfo {
    /// 序列化为 ComicInfo.xml 文本。
    ///
    /// 对齐 Kotlin：xmlDeclMode=Charset → `<?xml version="1.0" encoding="UTF-8"?>`；
    /// 根标签无命名空间（`<ComicInfo>`）；元素顺序按 ComicInfo.kt 声明顺序。
    pub fn to_xml(&self) -> String {
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        xml.push_str("<ComicInfo>\n");
        push_optional(&mut xml, "Title", &self.title);
        push_optional(&mut xml, "Series", &self.series);
        push_optional(&mut xml, "Number", &self.number);
        push_optional(&mut xml, "Count", &self.count.map(|c| c.to_string()));
        push_optional(&mut xml, "Volume", &self.volume.map(|v| v.to_string()));
        push_optional(&mut xml, "AlternateSeries", &self.alternate_series);
        push_optional(&mut xml, "AlternateNumber", &self.alternate_number);
        push_optional(
            &mut xml,
            "AlternateCount",
            &self.alternate_count.map(|c| c.to_string()),
        );
        push_optional(&mut xml, "Summary", &self.summary);
        push_optional(&mut xml, "Notes", &self.notes);
        push_optional(&mut xml, "Year", &self.year.map(|y| y.to_string()));
        push_optional(&mut xml, "Month", &self.month.map(|m| m.to_string()));
        push_optional(&mut xml, "Day", &self.day.map(|d| d.to_string()));
        push_optional(&mut xml, "Writer", &self.writer);
        push_optional(&mut xml, "Penciller", &self.penciller);
        push_optional(&mut xml, "Inker", &self.inker);
        push_optional(&mut xml, "Colorist", &self.colorist);
        push_optional(&mut xml, "Letterer", &self.letterer);
        push_optional(&mut xml, "CoverArtist", &self.cover_artist);
        push_optional(&mut xml, "Editor", &self.editor);
        push_optional(&mut xml, "Translator", &self.translator);
        push_optional(&mut xml, "Publisher", &self.publisher);
        push_optional(&mut xml, "Imprint", &self.imprint);
        push_optional(&mut xml, "Genre", &self.genre);
        push_optional(&mut xml, "Tags", &self.tags);
        push_optional(&mut xml, "Web", &self.web);
        push_optional(
            &mut xml,
            "PageCount",
            &self.page_count.map(|c| c.to_string()),
        );
        push_optional(&mut xml, "LanguageISO", &self.language_iso);
        push_optional(&mut xml, "Format", &self.format);
        push_optional(&mut xml, "BlackAndWhite", &self.black_and_white);
        push_optional(&mut xml, "Manga", &self.manga);
        push_optional(&mut xml, "Characters", &self.characters);
        push_optional(&mut xml, "Teams", &self.teams);
        push_optional(&mut xml, "Locations", &self.locations);
        push_optional(&mut xml, "ScanInformation", &self.scan_information);
        push_optional(&mut xml, "StoryArc", &self.story_arc);
        push_optional(&mut xml, "StoryArcNumber", &self.story_arc_number);
        push_optional(&mut xml, "SeriesGroup", &self.series_group);
        push_optional(&mut xml, "AgeRating", &self.age_rating);
        push_optional(&mut xml, "Rating", &self.rating);
        push_optional(&mut xml, "LocalizedSeries", &self.localized_series);
        push_optional(&mut xml, "GTIN", &self.gtin);
        if let Some(pages) = &self.pages {
            xml.push_str("  <Pages>\n");
            for page in pages {
                xml.push_str("    <Page>\n");
                push_optional_indent(&mut xml, "Image", &page.image.map(|v| v.to_string()), 6);
                push_optional_indent(&mut xml, "Type", &page.r#type, 6);
                push_optional_indent(
                    &mut xml,
                    "DoublePage",
                    &page.double_page.map(|v| v.to_string()),
                    6,
                );
                push_optional_indent(
                    &mut xml,
                    "ImageSize",
                    &page.image_size.map(|v| v.to_string()),
                    6,
                );
                push_optional_indent(&mut xml, "Key", &page.key, 6);
                push_optional_indent(&mut xml, "Bookmark", &page.bookmark, 6);
                push_optional_indent(
                    &mut xml,
                    "ImageWidth",
                    &page.image_width.map(|v| v.to_string()),
                    6,
                );
                push_optional_indent(
                    &mut xml,
                    "ImageHeight",
                    &page.image_height.map(|v| v.to_string()),
                    6,
                );
                xml.push_str("    </Page>\n");
            }
            xml.push_str("  </Pages>\n");
        }
        xml.push_str("</ComicInfo>");
        xml
    }
}

fn push_optional(xml: &mut String, tag: &str, value: &Option<String>) {
    if let Some(value) = value {
        if !value.is_empty() {
            xml.push_str(&format!("  <{tag}>{}</{tag}>\n", escape_xml(value)));
        }
    }
}

fn push_optional_indent(xml: &mut String, tag: &str, value: &Option<String>, indent: usize) {
    if let Some(value) = value {
        if !value.is_empty() {
            xml.push_str(&format!(
                "{indent}<{tag}>{}</{tag}>\n",
                escape_xml(value),
                indent = " ".repeat(indent)
            ));
        }
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn unescape_xml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// 从已序列化的 ComicInfo.xml 文本中提取字段（对应 Kotlin `getComicInfo`，
/// 覆盖 ComicInfo.kt 全部声明字段，含 notes/imprint/pages 等 mapper 不写、仅旧文件保留的字段）。
fn parse_comic_info(xml: &str) -> ComicInfo {
    fn text<'a>(xml: &'a str, tag: &str) -> Option<String> {
        let open = format!("<{}>", tag);
        let close = format!("</{}>", tag);
        let start = xml.find(&open)?;
        let rest = &xml[start + open.len()..];
        let end = rest.find(&close)?;
        let inner = &rest[..end];
        if inner.is_empty() {
            None
        } else {
            Some(unescape_xml(inner))
        }
    }

    fn int_field(xml: &str, tag: &str) -> Option<i32> {
        text(xml, tag).and_then(|v| v.parse::<i32>().ok())
    }

    fn uint_field(xml: &str, tag: &str) -> Option<u32> {
        text(xml, tag).and_then(|v| v.parse::<u32>().ok())
    }

    fn long_field(xml: &str, tag: &str) -> Option<i64> {
        text(xml, tag).and_then(|v| v.parse::<i64>().ok())
    }

    fn bool_field(xml: &str, tag: &str) -> Option<bool> {
        text(xml, tag).and_then(|v| match v.as_str() {
            "true" | "True" | "1" => Some(true),
            "false" | "False" | "0" => Some(false),
            _ => None,
        })
    }

    fn pages_field(xml: &str) -> Option<Vec<Page>> {
        let open = "<Pages>";
        let close = "</Pages>";
        let start = xml.find(open)?;
        let rest = &xml[start + open.len()..];
        let end = rest.find(close)?;
        let inner = &rest[..end];
        let mut pages = Vec::new();
        let mut idx = 0;
        while idx < inner.len() {
            let Some(rel) = inner[idx..].find("<Page>") else {
                break;
            };
            let pstart = idx + rel;
            let after = &inner[pstart + "<Page>".len()..];
            let Some(pend) = after.find("</Page>") else {
                break;
            };
            let pbody = &after[..pend];
            pages.push(Page {
                image: int_field(pbody, "Image"),
                r#type: text(pbody, "Type"),
                double_page: bool_field(pbody, "DoublePage"),
                image_size: long_field(pbody, "ImageSize"),
                key: text(pbody, "Key"),
                bookmark: text(pbody, "Bookmark"),
                image_width: int_field(pbody, "ImageWidth"),
                image_height: int_field(pbody, "ImageHeight"),
            });
            idx = pstart + "<Page>".len() + pend + "</Page>".len();
        }
        if pages.is_empty() {
            None
        } else {
            Some(pages)
        }
    }

    ComicInfo {
        title: text(xml, "Title"),
        series: text(xml, "Series"),
        number: text(xml, "Number"),
        count: int_field(xml, "Count"),
        volume: int_field(xml, "Volume"),
        alternate_series: text(xml, "AlternateSeries"),
        alternate_number: text(xml, "AlternateNumber"),
        alternate_count: int_field(xml, "AlternateCount"),
        summary: text(xml, "Summary"),
        notes: text(xml, "Notes"),
        year: int_field(xml, "Year"),
        month: uint_field(xml, "Month"),
        day: uint_field(xml, "Day"),
        writer: text(xml, "Writer"),
        penciller: text(xml, "Penciller"),
        inker: text(xml, "Inker"),
        colorist: text(xml, "Colorist"),
        letterer: text(xml, "Letterer"),
        cover_artist: text(xml, "CoverArtist"),
        editor: text(xml, "Editor"),
        translator: text(xml, "Translator"),
        publisher: text(xml, "Publisher"),
        imprint: text(xml, "Imprint"),
        genre: text(xml, "Genre"),
        tags: text(xml, "Tags"),
        web: text(xml, "Web"),
        page_count: int_field(xml, "PageCount"),
        language_iso: text(xml, "LanguageISO"),
        format: text(xml, "Format"),
        black_and_white: text(xml, "BlackAndWhite"),
        manga: text(xml, "Manga"),
        characters: text(xml, "Characters"),
        teams: text(xml, "Teams"),
        locations: text(xml, "Locations"),
        scan_information: text(xml, "ScanInformation"),
        story_arc: text(xml, "StoryArc"),
        story_arc_number: text(xml, "StoryArcNumber"),
        series_group: text(xml, "SeriesGroup"),
        age_rating: text(xml, "AgeRating"),
        rating: text(xml, "Rating"),
        localized_series: text(xml, "LocalizedSeries"),
        gtin: text(xml, "GTIN"),
        pages: pages_field(xml),
    }
}

/// 合并旧 ComicInfo 与新 ComicInfo（对应 Kotlin `mergeComicInfoMetadata`：
/// 新值优先，新值为 None 时沿用旧值——覆盖 ComicInfo.kt 全部声明字段）。
fn merge_comic_info(old: &ComicInfo, new: &ComicInfo) -> ComicInfo {
    let or_str = |n: &Option<String>, o: &Option<String>| n.clone().or_else(|| o.clone());
    let or_int = |n: &Option<i32>, o: &Option<i32>| n.or(*o);
    ComicInfo {
        title: or_str(&new.title, &old.title),
        series: or_str(&new.series, &old.series),
        number: or_str(&new.number, &old.number),
        count: or_int(&new.count, &old.count),
        volume: or_int(&new.volume, &old.volume),
        alternate_series: or_str(&new.alternate_series, &old.alternate_series),
        alternate_number: or_str(&new.alternate_number, &old.alternate_number),
        alternate_count: or_int(&new.alternate_count, &old.alternate_count),
        summary: or_str(&new.summary, &old.summary),
        notes: or_str(&new.notes, &old.notes),
        year: or_int(&new.year, &old.year),
        month: new.month.or(old.month),
        day: new.day.or(old.day),
        writer: or_str(&new.writer, &old.writer),
        penciller: or_str(&new.penciller, &old.penciller),
        inker: or_str(&new.inker, &old.inker),
        colorist: or_str(&new.colorist, &old.colorist),
        letterer: or_str(&new.letterer, &old.letterer),
        cover_artist: or_str(&new.cover_artist, &old.cover_artist),
        editor: or_str(&new.editor, &old.editor),
        translator: or_str(&new.translator, &old.translator),
        publisher: or_str(&new.publisher, &old.publisher),
        imprint: or_str(&new.imprint, &old.imprint),
        genre: or_str(&new.genre, &old.genre),
        tags: or_str(&new.tags, &old.tags),
        web: or_str(&new.web, &old.web),
        page_count: or_int(&new.page_count, &old.page_count),
        language_iso: or_str(&new.language_iso, &old.language_iso),
        format: or_str(&new.format, &old.format),
        black_and_white: or_str(&new.black_and_white, &old.black_and_white),
        manga: or_str(&new.manga, &old.manga),
        characters: or_str(&new.characters, &old.characters),
        teams: or_str(&new.teams, &old.teams),
        locations: or_str(&new.locations, &old.locations),
        scan_information: or_str(&new.scan_information, &old.scan_information),
        story_arc: or_str(&new.story_arc, &old.story_arc),
        story_arc_number: or_str(&new.story_arc_number, &old.story_arc_number),
        series_group: or_str(&new.series_group, &old.series_group),
        age_rating: or_str(&new.age_rating, &old.age_rating),
        rating: or_str(&new.rating, &old.rating),
        localized_series: or_str(&new.localized_series, &old.localized_series),
        gtin: or_str(&new.gtin, &old.gtin),
        pages: new.pages.clone().or_else(|| old.pages.clone()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ComicInfoError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
}

/// ComicInfo 写入器。
///
/// 实现方式：单遍流式重写 ZIP——同时打开源归档（ZipArchive）与目标临时文件
/// （ZipWriter），条目逐一从 zip 读端直接 copy 到写端（零中间缓冲，O(1) 内存），
/// 再原子替换原文件。mimetype 条目（EPUB）以 Stored 方式排最前：先按名定位并
/// 第一个写出，再按索引顺序写出其余条目。
#[derive(Clone)]
pub struct ComicInfoWriter {
    /// 对应 Kotlin `overrideComicInfo`：true 时用新数据整体替换；
    /// false（append）时若已有 ComicInfo.xml，则与旧值合并后写回。
    override_existing: bool,
}

impl ComicInfoWriter {
    pub fn new(override_existing: bool) -> Self {
        Self { override_existing }
    }

    pub fn write_metadata(
        &self,
        book_url: &str,
        comic_info: &ComicInfo,
    ) -> Result<(), ComicInfoError> {
        let path = PathBuf::from(book_url);
        self.write_comic_info(&path, comic_info)
    }

    pub fn remove_comic_info(&self, book_url: &str) -> Result<(), ComicInfoError> {
        let path = PathBuf::from(book_url);
        self.remove_entry(&path, "ComicInfo.xml")
    }

    /// 读取 zip 中是否存在 ComicInfo.xml（且非空）。
    pub fn has_comic_info(&self, book_url: &str) -> bool {
        let Ok(file) = std::fs::File::open(book_url) else {
            return false;
        };
        let Ok(mut archive) = zip::ZipArchive::new(file) else {
            return false;
        };
        (0..archive.len()).any(|i| {
            archive
                .by_index(i)
                .map(|entry| entry.name().eq_ignore_ascii_case("ComicInfo.xml"))
                .unwrap_or(false)
        })
    }

    fn write_comic_info(&self, path: &Path, comic_info: &ComicInfo) -> Result<(), ComicInfoError> {
        let entry_name = "ComicInfo.xml";

        // 对应 Kotlin `writeMetadata`：
        // - overrideComicInfo=true：直接写入新数据；
        // - false（append）：若已有 ComicInfo.xml，解析旧值并合并（新值优先，旧值补缺）；
        //   合并结果与旧值相同时跳过写盘。
        // 仅读取 ComicInfo.xml 单个条目（不缓冲整本归档）。
        let effective = if !self.override_existing {
            match read_zip_entry(path, entry_name)? {
                Some(old_bytes) => {
                    let old_xml = String::from_utf8_lossy(&old_bytes);
                    let old_info = parse_comic_info(&old_xml);
                    let merged = merge_comic_info(&old_info, comic_info);
                    if old_info == merged {
                        return Ok(());
                    }
                    merged.to_xml()
                }
                None => comic_info.to_xml(),
            }
        } else {
            comic_info.to_xml()
        };

        rewrite_zip(path, Some((entry_name, effective.into_bytes())), None)
    }

    fn remove_entry(&self, path: &Path, entry_name: &str) -> Result<(), ComicInfoError> {
        rewrite_zip(path, None, Some(entry_name))
    }
}

/// 读取 zip 中指定单个条目的字节（不存在返回 None）。
fn read_zip_entry(path: &Path, entry_name: &str) -> Result<Option<Vec<u8>>, ComicInfoError> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let Ok(mut entry) = archive.by_name(entry_name) else {
        return Ok(None);
    };
    let mut data = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut data)?;
    Ok(Some(data))
}

/// 单遍流式重写 ZIP（mimetype 条目以 Stored 方式排在最前以兼容 EPUB）：
/// 源归档条目经 `std::io::copy` 直接从读端流到写端，无整本内存缓冲。
/// `replacement`：在末尾写入的新条目（ComicInfo.xml 写入场景）；
/// `remove_name`：按名跳过的条目（大小写不敏感，移除场景）。
fn rewrite_zip(
    path: &Path,
    replacement: Option<(&str, Vec<u8>)>,
    remove_name: Option<&str>,
) -> Result<(), ComicInfoError> {
    let source = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(source)?;
    let temp_path = path.with_extension("tmp");
    let target = std::fs::File::create(&temp_path)?;
    let mut writer = zip::ZipWriter::new(target);

    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    // mimetype 必须无压缩且是第一个条目（EPUB 规范）：先按名定位并第一个写出
    if let Some(index) = archive.index_for_name("mimetype") {
        let mut entry = archive.by_index(index)?;
        writer.start_file(
            "mimetype",
            options.compression_method(zip::CompressionMethod::Stored),
        )?;
        std::io::copy(&mut entry, &mut writer)?;
    }

    // 按原索引顺序流式复制其余条目；跳过 mimetype（已写出）与目标条目
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if name == "mimetype" {
            continue;
        }
        if let Some(remove) = remove_name {
            if name.eq_ignore_ascii_case(remove) {
                continue;
            }
        }
        if let Some((replace, _)) = &replacement {
            if name == *replace {
                continue;
            }
        }
        writer.start_file(name, options)?;
        std::io::copy(&mut entry, &mut writer)?;
    }

    // replace 场景：新 ComicInfo.xml 追加在最末（与全量重建的条目顺序一致）
    if let Some((name, bytes)) = replacement {
        writer.start_file(name, options)?;
        let mut cursor = Cursor::new(&bytes);
        std::io::copy(&mut cursor, &mut writer)?;
    }
    writer.finish()?;

    // 原子替换
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&temp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_xml() {
        let info = ComicInfo {
            title: Some("My Title".to_string()),
            series: Some("My Series".to_string()),
            number: Some("3".to_string()),
            writer: Some("Author & Co".to_string()),
            age_rating: Some("Teen".to_string()),
            ..Default::default()
        };
        let xml = info.to_xml();
        assert!(xml.contains("<Title>My Title</Title>"));
        assert!(xml.contains("<AgeRating>Teen</AgeRating>"));
    }

    #[test]
    fn merge_preserves_unmodeled_fields() {
        // 对齐 Kotlin mergeComicInfoMetadata：新值优先，旧值补缺，
        // notes/imprint/pages 等非 mapper 字段在合并重写时保留。
        let old = ComicInfo {
            title: Some("Old Title".to_string()),
            notes: Some("Old notes".to_string()),
            imprint: Some("Some Imprint".to_string()),
            page_count: Some(42),
            pages: Some(vec![Page {
                image: Some(0),
                r#type: Some("Story".to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let new = ComicInfo {
            title: Some("New Title".to_string()),
            ..Default::default()
        };
        let merged = merge_comic_info(&old, &new);
        assert_eq!(merged.title.as_deref(), Some("New Title"));
        assert_eq!(merged.notes.as_deref(), Some("Old notes"));
        assert_eq!(merged.imprint.as_deref(), Some("Some Imprint"));
        assert_eq!(merged.page_count, Some(42));
        assert_eq!(merged.pages.as_ref().map(|p| p.len()), Some(1));
    }

    #[test]
    fn parse_roundtrip_preserves_fields() {
        let info = ComicInfo {
            notes: Some("N&N".to_string()),
            imprint: Some("Imp".to_string()),
            pages: Some(vec![Page {
                image: Some(1),
                double_page: Some(false),
                image_size: Some(1024),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let xml = info.to_xml();
        let parsed = parse_comic_info(&xml);
        assert_eq!(parsed.notes, info.notes);
        assert_eq!(parsed.imprint, info.imprint);
        assert_eq!(parsed.pages, info.pages);
    }

    /// 构造临时 zip（mimetype + pages，模拟 EPUB/CBZ），返回路径。
    /// 条目顺序：mimetype 最先，其余按给定顺序。
    fn make_temp_zip(entries: &[(&str, &[u8])]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "komf-comic-info-test-{}-{}.zip",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (i, (name, bytes)) in entries.iter().enumerate() {
            let options = if *name == "mimetype" && i == 0 {
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored)
            } else {
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
            };
            writer.start_file(*name, options).unwrap();
            std::io::copy(&mut Cursor::new(bytes), &mut writer).unwrap();
        }
        writer.finish().unwrap();
        path
    }

    fn read_entry(path: &Path, name: &str) -> Option<Vec<u8>> {
        let file = std::fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut entry = archive.by_name(name).ok()?;
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        Some(data)
    }

    #[test]
    fn write_comic_info_streaming_roundtrip() {
        let path = make_temp_zip(&[
            ("mimetype", b"application/epub+zip"),
            ("page1.jpg", b"page1-bytes"),
            ("page2.jpg", b"page2-bytes"),
        ]);
        let writer = ComicInfoWriter::new(true);
        let info = ComicInfo {
            title: Some("T".to_string()),
            ..Default::default()
        };
        writer
            .write_metadata(path.to_str().unwrap(), &info)
            .unwrap();

        // 条目全部保留：mimetype 仍首位且 Stored，ComicInfo.xml 追加在末尾
        let file = std::fs::File::open(&path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 4);
        {
            let first = archive.by_index(0).unwrap();
            assert_eq!(first.name(), "mimetype");
            assert_eq!(first.compression(), zip::CompressionMethod::Stored);
        }
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert_eq!(
            names,
            vec!["mimetype", "page1.jpg", "page2.jpg", "ComicInfo.xml"]
        );

        // 已有 ComicInfo.xml 时被替换而非重复追加
        writer
            .write_metadata(path.to_str().unwrap(), &info)
            .unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 4);

        let xml = read_entry(&path, "ComicInfo.xml").unwrap();
        assert!(String::from_utf8_lossy(&xml).contains("<Title>T</Title>"));
        assert_eq!(
            read_entry(&path, "page2.jpg").as_deref(),
            Some(b"page2-bytes".as_slice())
        );

        // remove_comic_info 仅剔除目标条目，mimetype 仍首位 Stored
        writer.remove_comic_info(path.to_str().unwrap()).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 3);
        {
            let first = archive.by_index(0).unwrap();
            assert_eq!(first.name(), "mimetype");
            assert_eq!(first.compression(), zip::CompressionMethod::Stored);
        }
        assert!(read_entry(&path, "ComicInfo.xml").is_none());

        std::fs::remove_file(&path).ok();
    }
}
