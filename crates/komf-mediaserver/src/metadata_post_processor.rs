//! 元数据后处理 —— 对应 `MetadataPostProcessor.kt`。
use crate::model::{MediaServerBookId, SeriesAndBookMetadata};
use komf_core::model::{
    BookMetadata, MediaType, PublisherType, ReadingDirection, SeriesMetadata, SeriesTitle,
};
use komf_core::util::{replace_fullwidth_chars, strip_accents, BookNameParser};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct PublisherTagNameConfig {
    pub tag_name: String,
    pub language: String,
}

pub struct MetadataPostProcessor {
    #[allow(dead_code)]
    library_type: MediaType,
    series_title: bool,
    series_title_language: Option<String>,
    alternative_series_titles: bool,
    alternative_series_title_languages: Vec<String>,
    order_books: bool,
    reading_direction_value: Option<ReadingDirection>,
    language_value: Option<String>,
    fallback_to_alt_title: bool,
    score_tag_name: Option<String>,
    original_publisher_tag_name: Option<String>,
    publisher_tag_names: Vec<PublisherTagNameConfig>,
}

impl MetadataPostProcessor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        library_type: MediaType,
        series_title: bool,
        series_title_language: Option<String>,
        alternative_series_titles: bool,
        alternative_series_title_languages: Vec<String>,
        order_books: bool,
        reading_direction_value: Option<ReadingDirection>,
        language_value: Option<String>,
        fallback_to_alt_title: bool,
        score_tag_name: Option<String>,
        original_publisher_tag_name: Option<String>,
        publisher_tag_names: Vec<PublisherTagNameConfig>,
    ) -> Self {
        Self {
            library_type,
            series_title,
            series_title_language,
            alternative_series_titles,
            alternative_series_title_languages,
            order_books,
            reading_direction_value,
            language_value,
            fallback_to_alt_title,
            score_tag_name,
            original_publisher_tag_name,
            publisher_tag_names,
        }
    }

    /// 对应 `process`。
    pub fn process(&self, metadata: &SeriesAndBookMetadata) -> SeriesAndBookMetadata {
        let series_metadata =
            self.post_process_series(&metadata.series_metadata, &metadata.excluded_alt_titles);
        let book_metadata = self.post_process_books(&metadata.book_metadata);
        self.handle_komga_oneshot(
            series_metadata,
            book_metadata,
            &metadata.book_oneshots,
            &metadata.excluded_alt_titles,
        )
    }

    fn post_process_series(&self, series: &SeriesMetadata, excluded_alt_titles: &[String]) -> SeriesMetadata {
        let alt_titles: Vec<SeriesTitle> = if self.alternative_series_titles {
            let mut titles: Vec<SeriesTitle> = series.titles.iter().cloned().collect();
            // 配置列表中的语言按配置位置排前（替代按语言名字母序——
            // 字母序让 "ja" 恒先于 "ja-ro"，配置顺序失效；Kavita 只有单个
            // localizedName 字段取第一个备选标题，配置优先的语言必须排在前面）。
            // 未配置 / 不在配置中的语言保留并回退原有逻辑：有语言标签的按字母序
            // 在前、无语言最后。
            titles.sort_by(|a, b| match (self.language_priority(a.language.as_deref()), self.language_priority(b.language.as_deref())) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => match (&a.language, &b.language) {
                    (Some(x), Some(y)) => x.cmp(y),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
            });
            let mut seen = std::collections::HashSet::new();
            titles.retain(|t| seen.insert(distinct_name(&t.name)));
            titles
        } else {
            Vec::new()
        };

        let chosen_title: Option<SeriesTitle> = if self.series_title {
            self.choose_series_title(series)
                .or_else(|| if self.fallback_to_alt_title { alt_titles.first().cloned() } else { None })
        } else {
            series.title.clone()
        };

        let alts_without_series_title: Vec<SeriesTitle> = match &chosen_title {
            Some(chosen) => alt_titles
                .into_iter()
                .filter(|t| distinct_name(&t.name) != distinct_name(&chosen.name))
                .collect(),
            None => alt_titles,
        };

        // `seriesMetadata.alternativeTitles=false` 的 provider 标题：主标题选定后，
        // 从备选中剔除（按 distinct_name 归一比较，与主标题剔除逻辑一致）。
        // 主标题（chosen）不受影响——即使它来自禁写备选的 provider 也照常写入，
        // 因此主标题语言选择（seriesTitleLanguage）仍基于全量候选。
        let alts_without_series_title: Vec<SeriesTitle> = if excluded_alt_titles.is_empty() {
            alts_without_series_title
        } else {
            let excluded: std::collections::HashSet<String> = excluded_alt_titles
                .iter()
                .map(|n| distinct_name(n))
                .collect();
            alts_without_series_title
                .into_iter()
                .filter(|t| !excluded.contains(&distinct_name(&t.name)))
                .collect()
        };

        let mut tags = series.tags.clone();
        self.add_score_tag(&mut tags, series);
        self.add_original_publisher_tag(&mut tags, series);
        for config in &self.publisher_tag_names {
            self.add_publisher_tag(&mut tags, series, &config.tag_name, &config.language);
        }

        SeriesMetadata {
            title: chosen_title,
            titles: alts_without_series_title,
            reading_direction: self.reading_direction_value.or(series.reading_direction),
            language: series.language.clone().or_else(|| self.language_value.clone()),
            tags,
            ..series.clone()
        }
    }

    /// 语言在 alternativeSeriesTitleLanguages 配置中的位置；未配置或不在配置中
    /// 返回 None（排序回退原有逻辑：有语言标签的在前）。大小写不敏感
    /// （"JA-RO" 配置可匹配 provider 的 "ja-ro"）。
    fn language_priority(&self, language: Option<&str>) -> Option<usize> {
        let language = language?;
        self.alternative_series_title_languages
            .iter()
            .position(|l| l.eq_ignore_ascii_case(language))
    }

    fn add_score_tag(&self, tags: &mut Vec<String>, series: &SeriesMetadata) {
        let Some(tag_name) = &self.score_tag_name else { return };
        let Some(score) = series.score.map(|s| s as i32) else { return };
        tags.push(format!("{tag_name}: {score}"));
    }

    fn add_original_publisher_tag(&self, tags: &mut Vec<String>, series: &SeriesMetadata) {
        let Some(tag_name) = &self.original_publisher_tag_name else { return };
        let publishers: Vec<_> = series
            .alternative_publishers
            .iter()
            .chain(series.publisher.iter())
            .collect();
        if let Some(publisher) = publishers.iter().find(|p| p.r#type == Some(PublisherType::Original)) {
            tags.push(format!("{tag_name}: {}", publisher.name));
        }
    }

    fn add_publisher_tag(&self, tags: &mut Vec<String>, series: &SeriesMetadata, tag_name: &str, language: &str) {
        let publishers: Vec<_> = series
            .alternative_publishers
            .iter()
            .chain(series.publisher.iter())
            .collect();
        if let Some(publisher) = publishers
            .iter()
            .find(|p| p.language_tag.as_deref().map(|l| l.eq_ignore_ascii_case(language)).unwrap_or(false))
        {
            tags.push(format!("{tag_name}: {}", publisher.name));
        }
    }

    fn post_process_books(
        &self,
        books: &HashMap<MediaServerBookId, Option<BookMetadata>>,
    ) -> HashMap<MediaServerBookId, Option<BookMetadata>> {
        if !self.order_books {
            return books.clone();
        }
        // Kotlin 的 orderBook 依赖 MediaServerBook.name；Rust 侧书名在
        // `MetadataUpdater.update_book_metadata` 中可得，因此真实排序在该处
        // 通过 `Self::order_book(book_name, metadata)` 应用（见 updater）。
        books.clone()
    }

    /// 按书名解析卷/章号并写入 number/numberSort —— 对应 Kotlin `orderBook`。
    /// 仅在 orderBooks 配置开启时生效；由 `MetadataUpdater` 对每本书调用
    /// （Kotlin 在 postProcessBooks 中调用，二者等价，因为 updater 遍历同批 book）。
    pub fn maybe_order_book(&self, book_name: &str, metadata: &BookMetadata) -> BookMetadata {
        if self.order_books {
            order_book_by_name(self.library_type, book_name, metadata)
        } else {
            metadata.clone()
        }
    }

    /// 对齐 Kotlin `postProcessBooks`：orderBooks 开启时，无 provider 匹配的书
    /// （metadata == null）也会用空 BookMetadata 执行 orderBook，得到书名解析的
    /// number/numberSort（Kotlin `metadata ?: BookMetadata()` 语义）。
    pub fn order_books_enabled(&self) -> bool {
        self.order_books
    }

    /// alternativeSeriesTitleLanguages 配置（mylar 导出等按语言过滤备用标题用）。
    pub fn alternative_series_title_languages(&self) -> &[String] {
        &self.alternative_series_title_languages
    }

    fn choose_series_title(&self, series: &SeriesMetadata) -> Option<SeriesTitle> {
        let chosen = match &self.series_title_language {
            Some(lang) => {
                // 优先语言标识完全匹配；未命中再前缀匹配（如 zh 命中 zh-Hant/zh-CN），
                // 前缀匹配排除罗马音变体（归一后以 -ro 结尾，避免 ja 命中 ja-ro）。
                series
                    .titles
                    .iter()
                    .find(|t| t.language.as_deref() == Some(lang.as_str()))
                    .or_else(|| {
                        series.titles.iter().find(|t| {
                            t.language
                                .as_deref()
                                .map(|l| l.starts_with(lang.as_str()) && !l.ends_with("-ro"))
                                .unwrap_or(false)
                        })
                    })
            }
            None => series.titles.first(),
        };
        chosen.cloned().or_else(|| series.title.clone())
    }

    /// 对应 `handleKomgaOneshot`。
    fn handle_komga_oneshot(
        &self,
        series_metadata: SeriesMetadata,
        book_metadata: HashMap<MediaServerBookId, Option<BookMetadata>>,
        oneshots: &HashMap<MediaServerBookId, bool>,
        excluded_alt_titles: &[String],
    ) -> SeriesAndBookMetadata {
        // 对齐 Kotlin：size > 1 || key.oneshot == false 时原样返回。
        // Kotlin 的 oneshot 为 Boolean?，null 也继续合并；Rust 的 oneshot 为 bool，
        // 仅 oneshot == true 时合并。
        let single_oneshot = book_metadata.len() == 1
            && book_metadata
                .keys()
                .next()
                .map(|id| oneshots.get(id).copied().unwrap_or(false))
                .unwrap_or(false);
        if book_metadata.len() > 1 || !single_oneshot {
            let mut out = SeriesAndBookMetadata::new(series_metadata, book_metadata);
            out.excluded_alt_titles = excluded_alt_titles.to_vec();
            return out;
        }
        // 单本 oneshot 的 series：将系列字段合并到书元数据。
        // 对齐 Kotlin：新 BookMetadata 只构造 title/summary/tags/links/thumbnail 五字段
        // （其余字段按默认值，Kotlin data class 默认参数语义）；tags 去重（toSet）。
        let new_book_metadata: HashMap<MediaServerBookId, Option<BookMetadata>> = book_metadata
            .into_iter()
            .map(|(book_id, metadata)| {
                let merged = BookMetadata {
                    title: metadata.as_ref().and_then(|m| m.title.clone()),
                    summary: metadata
                        .as_ref()
                        .and_then(|m| m.summary.clone())
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| series_metadata.summary.clone()),
                    tags: {
                        let mut tags: Vec<String> = metadata
                            .as_ref()
                            .map(|m| m.tags.clone())
                            .filter(|t| !t.is_empty())
                            .unwrap_or_else(|| series_metadata.tags.clone());
                        // Kotlin toSet()：保持插入序去重（不排序）。
                        let mut seen = std::collections::HashSet::new();
                        tags.retain(|t| seen.insert(t.clone()));
                        tags
                    },
                    links: metadata
                        .as_ref()
                        .map(|m| m.links.clone())
                        .filter(|l| !l.is_empty())
                        .unwrap_or_else(|| series_metadata.links.clone()),
                    thumbnail: metadata
                        .as_ref()
                        .and_then(|m| m.thumbnail.clone())
                        .or_else(|| series_metadata.thumbnail.clone()),
                    ..Default::default()
                };
                (book_id, Some(merged))
            })
            .collect();

        let mut new_series_metadata = series_metadata;
        new_series_metadata.thumbnail = None; // series thumbnail should be null for oneshots
        let oneshots: HashMap<MediaServerBookId, bool> =
            new_book_metadata.keys().map(|id| (id.clone(), true)).collect();
        let mut out =
            SeriesAndBookMetadata::new(new_series_metadata, new_book_metadata).with_book_oneshots(oneshots);
        out.excluded_alt_titles = excluded_alt_titles.to_vec();
        out
    }
}

pub(crate) fn distinct_name(title: &str) -> String {
    replace_fullwidth_chars(&strip_accents(&title.replace(' ', ""))).to_lowercase()
}

/// 在真实排序场景（updater）中使用的按卷/章排序逻辑 —— 对应 Kotlin `orderBook`。
pub fn order_book_by_name(library_type: MediaType, book_name: &str, metadata: &BookMetadata) -> BookMetadata {
    let range = match library_type {
        MediaType::Manga => BookNameParser::get_volumes(book_name)
            .or_else(|| BookNameParser::get_chapters(book_name))
            .or_else(|| BookNameParser::get_book_number(book_name)),
        MediaType::Novel | MediaType::Comic => BookNameParser::get_book_number(book_name),
        MediaType::Webtoon => BookNameParser::get_chapters(book_name).or_else(|| BookNameParser::get_book_number(book_name)),
    };

    match range {
        Some(range) => {
            let start = range.start;
            BookMetadata {
                number: Some(range),
                number_sort: Some(start),
                ..metadata.clone()
            }
        }
        None => metadata.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komf_core::model::{SeriesMetadata, SeriesTitle, TitleType};
    use std::collections::HashMap;

    /// Bangumi 风格 titles：原名 Native、中文名 null/zh、别名 Localized
    fn bangumi_series() -> SeriesMetadata {
        SeriesMetadata {
            title: None,
            titles: vec![
                SeriesTitle {
                    name: "葬送のフリーレン".into(),
                    r#type: Some(TitleType::Native),
                    language: None,
                },
                SeriesTitle {
                    name: "葬送的芙莉莲".into(),
                    r#type: None,
                    language: Some("zh".into()),
                },
                SeriesTitle {
                    name: "Frieren".into(),
                    r#type: Some(TitleType::Localized),
                    language: Some("en".into()),
                },
            ],
            ..Default::default()
        }
    }

    fn processor(series_title_language: Option<String>) -> MetadataPostProcessor {
        MetadataPostProcessor::new(
            MediaType::Manga,
            true,
            series_title_language,
            true,
            vec!["zh".to_string(), "en".to_string()],
            false,
            None,
            None,
            false,
            None,
            None,
            vec![],
        )
    }

    /// 主标题尊重 seriesTitleLanguage 配置：配置 zh → name_cn
    #[test]
    fn series_title_respects_language_config() {
        let out = processor(Some("zh".into()))
            .process(&SeriesAndBookMetadata::new(bangumi_series(), HashMap::new()));
        assert_eq!(out.series_metadata.title.as_ref().unwrap().name, "葬送的芙莉莲");
    }

    /// 完全匹配未命中时前缀匹配兜底：配置 zh 可命中 zh-Hant/zh-CN 标题
    #[test]
    fn series_title_falls_back_to_prefix_match() {
        let mut series = bangumi_series();
        series.titles = vec![
            SeriesTitle {
                name: "葬送のフリーレン".into(),
                r#type: Some(TitleType::Native),
                language: None,
            },
            SeriesTitle {
                name: "葬送的芙莉蓮".into(),
                r#type: None,
                language: Some("zh-Hant".into()),
            },
            SeriesTitle {
                name: "Frieren".into(),
                r#type: Some(TitleType::Localized),
                language: Some("en".into()),
            },
        ];
        let out = processor(Some("zh".into()))
            .process(&SeriesAndBookMetadata::new(series, HashMap::new()));
        assert_eq!(out.series_metadata.title.as_ref().unwrap().name, "葬送的芙莉蓮");
    }

    /// 前缀匹配排除罗马音变体：配置 ja 时仅有 ja-ro（归一后的罗马音）不命中 → title 保持 None
    #[test]
    fn series_title_prefix_match_excludes_romaji() {
        let mut series = bangumi_series();
        series.title = None;
        series.titles = vec![
            SeriesTitle {
                name: "Frieren".into(),
                r#type: Some(TitleType::Romaji),
                language: Some("ja-ro".into()),
            },
            SeriesTitle {
                name: "Frieren (EN)".into(),
                r#type: Some(TitleType::Localized),
                language: Some("en".into()),
            },
        ];
        let out = processor(Some("ja".into()))
            .process(&SeriesAndBookMetadata::new(series, HashMap::new()));
        assert_eq!(out.series_metadata.title, None);
    }

    /// 未配置 seriesTitleLanguage → titles.first()（日文原名），不预设 name_cn 优先
    #[test]
    fn series_title_defaults_to_first_title() {
        let out = processor(None).process(&SeriesAndBookMetadata::new(bangumi_series(), HashMap::new()));
        assert_eq!(out.series_metadata.title.as_ref().unwrap().name, "葬送のフリーレン");
    }

    /// seriesTitle 关闭时保留 provider 原 title（对齐 Kotlin else 分支，非置 None）
    #[test]
    fn series_title_disabled_keeps_provider_title() {
        let mut series = bangumi_series();
        series.title = Some(SeriesTitle {
            name: "オリジナル名".into(),
            r#type: Some(TitleType::Native),
            language: None,
        });
        let processor = MetadataPostProcessor::new(
            MediaType::Manga,
            false, // seriesTitle 关闭
            Some("zh".into()),
            true,
            vec!["zh".to_string(), "en".to_string()],
            false,
            None,
            None,
            false,
            None,
            None,
            vec![],
        );
        let out = processor.process(&SeriesAndBookMetadata::new(series, HashMap::new()));
        // 主标题保留 provider 原值（不被 seriesTitleLanguage 选择逻辑替换）
        assert_eq!(out.series_metadata.title.as_ref().unwrap().name, "オリジナル名");
    }

    /// 主标题（名字级）从备选中剔除，但同名不同语言的 name_cn 若未成为主标题则保留
    #[test]
    fn series_title_removed_from_alt_titles() {
        let out = processor(Some("zh".into()))
            .process(&SeriesAndBookMetadata::new(bangumi_series(), HashMap::new()));
        let alts: Vec<&str> = out.series_metadata.titles.iter().map(|t| t.name.as_str()).collect();
        // 主标题 = 葬送的芙莉莲（zh），同名备选被 distinctName 剔除；语言排序 nullsLast：
        // 有语言(zh/en)在前、无语言(原名)在后
        assert_eq!(alts, vec!["Frieren", "葬送のフリーレン"]);
    }

    /// 备选标题按配置列表位置排序（而非语言名字母序）。
    /// 配置 ["en", "zh"] → en 排在最前（字母序会让 zh 在前）；Kavita 的
    /// localizedName 取第一个有语言的备选，得到配置优先的 en。
    /// fallbackToAltTitle 也回退到配置优先语言（排序后 first）。
    #[test]
    fn alt_titles_ordered_by_config_language_priority() {
        let p = MetadataPostProcessor::new(
            MediaType::Manga,
            true,
            Some("zh".into()),
            true,
            vec!["en".to_string(), "zh".to_string()],
            false,
            None,
            None,
            false,
            None,
            None,
            vec![],
        );
        let out = p.process(&SeriesAndBookMetadata::new(bangumi_series(), HashMap::new()));
        // 主标题 = zh；en(0) 排在 zh(1) 前、无语言最后；zh 同名备选被主标题剔除
        assert_eq!(out.series_metadata.title.as_ref().unwrap().name, "葬送的芙莉莲");
        let alts: Vec<&str> = out.series_metadata.titles.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(alts, vec!["Frieren", "葬送のフリーレン"]);
    }

    /// 语言匹配大小写不敏感：配置 "EN"，provider 语言 "en" 仍命中并排最前。
    #[test]
    fn alt_titles_language_match_is_case_insensitive() {
        let p = MetadataPostProcessor::new(
            MediaType::Manga,
            true,
            None,
            true,
            vec!["EN".to_string()],
            false,
            None,
            None,
            false,
            None,
            None,
            vec![],
        );
        let out = p.process(&SeriesAndBookMetadata::new(bangumi_series(), HashMap::new()));
        // 配置 ["EN"]：en 命中（ignoreCase）排前；zh 不在配置但保留，
        // 按原有逻辑（字母序）排在配置语言之后、无语言之前
        assert_eq!(out.series_metadata.title.as_ref().unwrap().name, "葬送のフリーレン");
        let alts: Vec<&str> = out.series_metadata.titles.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(alts, vec!["Frieren", "葬送的芙莉莲"]);
    }

    /// 配置语言全部未命中时只保留无语言备选（filter 语义与旧实现一致），主标题不受影响。
    #[test]
    fn alt_titles_empty_config_keeps_only_unlanguaged() {
        let mut series = bangumi_series();
        series.title = Some(SeriesTitle {
            name: "オリジナル名".into(),
            r#type: Some(TitleType::Native),
            language: None,
        });
        let p = MetadataPostProcessor::new(
            MediaType::Manga,
            false, // 保留 provider 主标题，避免主标题从备选剔除干扰断言
            None,
            true,
            vec![],
            false,
            None,
            None,
            false,
            None,
            None,
            vec![],
        );
        let out = p.process(&SeriesAndBookMetadata::new(series, HashMap::new()));
        // 空配置：不过滤任何语言；排序回退原有逻辑——有语言(en,zh)按字母序
        // 在前（en < zh）、无语言最后
        let alts: Vec<&str> = out.series_metadata.titles.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(alts, vec!["Frieren", "葬送的芙莉莲", "葬送のフリーレン"]);
    }

    /// alternativeTitles=false（excluded=全量标题名）：主标题语言选择仍基于全量候选，
    /// 不因备选被禁写而回退；备选清空（只影响写入）。
    #[test]
    fn excluded_alt_titles_respects_series_title_language() {
        let series = bangumi_series();
        let excluded: Vec<String> = series.titles.iter().map(|t| t.name.clone()).collect();
        let mut input = SeriesAndBookMetadata::new(series, HashMap::new());
        input.excluded_alt_titles = excluded;
        let out = processor(Some("zh".into())).process(&input);
        // 主标题仍为 zh（若提前 truncate(1) 则此处会回退为原名或 None）
        assert_eq!(out.series_metadata.title.as_ref().unwrap().name, "葬送的芙莉莲");
        assert!(out.series_metadata.titles.is_empty());
    }

    /// 部分排除：仅剔除名单中的备选，其余保留；主标题不受影响。
    #[test]
    fn excluded_alt_titles_partial_keeps_rest() {
        let series = bangumi_series();
        let mut input = SeriesAndBookMetadata::new(series, HashMap::new());
        input.excluded_alt_titles = vec!["Frieren".to_string()];
        let out = processor(Some("zh".into())).process(&input);
        assert_eq!(out.series_metadata.title.as_ref().unwrap().name, "葬送的芙莉莲");
        let alts: Vec<&str> = out.series_metadata.titles.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(alts, vec!["葬送のフリーレン"]);
    }
}
