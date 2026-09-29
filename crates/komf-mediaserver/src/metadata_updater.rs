//! 元数据更新器 —— 对应 `MetadataUpdater.kt`。
use crate::client::{MediaServerClient, MediaServerError};
use crate::comic_info::{comic_info_from_metadata, series_comic_info, ComicInfoWriter};
use crate::mylar::{mylar_series_json_from_metadata, write_series_json};
use crate::jobs::{BookThumbnail, KomfJobsRepository, SeriesThumbnail};
use crate::metadata_mapper::{authors_to_comic_info_fields, MetadataMapper};
use crate::metadata_post_processor::MetadataPostProcessor;
use crate::model::*;
use crate::tag_translator::TagTranslator;
use komf_core::model::{BookMetadata, Image, SeriesMetadata, UpdateMode};
use komf_core::providers::CoreProviders;
use komf_core::util::{case_insensitive_nat_sort, BookNameParser};
use std::sync::Arc;

pub struct MetadataUpdater {
    media_server_client: Arc<dyn MediaServerClient>,
    repository: Arc<KomfJobsRepository>,
    media_server: &'static str,
    metadata_update_mapper: MetadataMapper,
    post_processor: MetadataPostProcessor,
    /// 标签翻译器：seriesTitleLanguage 为中文且 provider 非 bangumi/ehentai 时启用
    /// （编译时嵌入 tag_translation.json，无运行时配置）。None = 不翻译。
    tag_translator: Option<TagTranslator>,
    comic_info_writer: ComicInfoWriter,
    /// mylarCovers：导出 series.json 时同时下载系列封面（cover.jpg / {name}.cover.jpg）。
    mylar_covers: bool,
    /// mylar 导出根目录（null=系列原目录）。
    mylar_output_dir: Option<String>,
    /// mylar 导出路径 `${configDir}` 占位符基准（=配置目录）。
    mylar_config_dir: Option<std::path::PathBuf>,
    /// 库根目录缓存（library_id → root；内部从媒体服务器 API 获取，无需配置）。
    library_root_cache: std::sync::Mutex<std::collections::HashMap<String, Option<String>>>,

    update_modes: Vec<UpdateMode>,
    override_existing_covers: bool,
    upload_book_covers: bool,
    upload_series_covers: bool,
    lock_covers: bool,
}

impl MetadataUpdater {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        media_server_client: Arc<dyn MediaServerClient>,
        repository: Arc<KomfJobsRepository>,
        media_server: &'static str,
        post_processor: MetadataPostProcessor,
        tag_translator: Option<TagTranslator>,
        update_modes: Vec<UpdateMode>,
        override_existing_covers: bool,
        upload_book_covers: bool,
        upload_series_covers: bool,
        lock_covers: bool,
        override_comic_info: bool,
        mylar_covers: bool,
        mylar_output_dir: Option<String>,
        mylar_config_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            media_server_client,
            repository,
            media_server,
            metadata_update_mapper: MetadataMapper,
            post_processor,
            tag_translator,
            comic_info_writer: ComicInfoWriter::new(override_comic_info),
            mylar_covers,
            mylar_output_dir,
            mylar_config_dir,
            library_root_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            update_modes,
            override_existing_covers,
            upload_book_covers,
            upload_series_covers,
            lock_covers,
        }
    }

    /// 对应 `updateMetadata`。
    /// `provider`：本次元数据来源的 provider；bangumi/ehentai 不翻译标签
    /// （bangumi 自身产出中文标签、ehentai 标签体系不适用），None（无 provider）
    /// 按可翻译处理——翻译只替换命中的英文标签，未命中/中文原样保留，无副作用。
    pub async fn update_metadata(
        &self,
        series: &MediaServerSeries,
        metadata: &SeriesAndBookMetadata,
        provider: Option<CoreProviders>,
    ) -> Result<(), MediaServerError> {
        let translated = match &self.tag_translator {
            Some(tr)
                if !matches!(
                    provider,
                    Some(CoreProviders::Bangumi) | Some(CoreProviders::EHentai)
                ) =>
            {
                translate_tags(metadata, tr)
            }
            _ => metadata.clone(),
        };
        let processed = self.post_processor.process(&translated);
        self.update_series_metadata(series, &processed.series_metadata).await?;
        self.update_book_metadata(series, &translated, &processed).await?;

        if self.update_modes.contains(&UpdateMode::MylarSeriesJson) {
            self.write_mylar_series_json(series, &processed.series_metadata).await?;
        }

        if self.update_modes.contains(&UpdateMode::ComicInfo) {
            self.media_server_client
                .refresh_metadata(&series.library_id, &series.id)
                .await?;
        }
        Ok(())
    }

    /// 对应 `resetLibraryMetadata`。
    pub async fn reset_library_metadata(
        &self,
        library_id: &MediaServerLibraryId,
        remove_comic_info: bool,
    ) -> Result<(), MediaServerError> {
        let mut page_number = 1;
        loop {
            let page = self.media_server_client.get_series_page(library_id, page_number).await?;
            for series in &page.content {
                self.reset_series_metadata(&series.id, remove_comic_info).await?;
            }
            if page.page_number >= page.total_pages - 1 {
                break;
            }
            page_number += 1;
        }
        Ok(())
    }

    pub async fn reset_series_metadata(
        &self,
        series_id: &MediaServerSeriesId,
        remove_comic_info: bool,
    ) -> Result<(), MediaServerError> {
        let series = self.media_server_client.get_series(series_id).await?;
        self.media_server_client.reset_series_metadata(&series).await?;

        let mut books = self.media_server_client.get_books(series_id).await?;
        books.sort_by(|a, b| case_insensitive_nat_sort(&a.name, &b.name));
        for (index, book) in books.iter().enumerate() {
            if remove_comic_info {
                // 对齐 Kotlin：removeComicInfo 内部 rethrow → job FAILED（HTTP 层 422）。
                self.comic_info_writer
                    .remove_comic_info(&book.url)
                    .map_err(|e| MediaServerError::ComicInfo(e.to_string()))?;
            }
            self.reset_book_metadata(book, Some(index as i32 + 1)).await?;
        }

        self.replace_series_thumbnail(series_id, None).await?;
        let _ = self.repository.delete_series_thumbnail(series_id, self.media_server);
        Ok(())
    }

    async fn reset_book_metadata(
        &self,
        book: &MediaServerBook,
        sort_number: Option<i32>,
    ) -> Result<(), MediaServerError> {
        self.media_server_client
            .reset_book_metadata(book, sort_number)
            .await?;
        self.replace_book_thumbnail(&book.id, None).await?;
        let _ = self.repository.delete_book_thumbnail(&book.id, self.media_server);
        Ok(())
    }

    async fn update_series_metadata(
        &self,
        series: &MediaServerSeries,
        metadata: &SeriesMetadata,
    ) -> Result<(), MediaServerError> {
        if self.update_modes.contains(&UpdateMode::Api) {
            let metadata_update = self
                .metadata_update_mapper
                .to_series_metadata_update(metadata, &series.metadata);
            self.media_server_client
                .update_series_metadata(&series.id, &metadata_update)
                .await?;
        }

        let new_thumbnail = if self.upload_series_covers {
            metadata.thumbnail.clone()
        } else {
            None
        };
        let thumbnail_id = self.replace_series_thumbnail(&series.id, new_thumbnail.as_ref()).await?;

        match thumbnail_id {
            Some(thumbnail_id) => {
                let _ = self.repository.save_series_thumbnail(&SeriesThumbnail {
                    series_id: series.id.clone(),
                    thumbnail_id,
                    media_server: self.media_server.to_string(),
                });
            }
            None => {
                let _ = self.repository.delete_series_thumbnail(&series.id, self.media_server);
            }
        }
        Ok(())
    }

    /// 内部获取库根目录（media server API get_library().roots 首个），带进程内缓存。
    /// 失败 / 无 root 返回 None → mylar 导出回退仅用目录名（拍平），不阻塞导出。
    async fn library_root_for(&self, library_id: &MediaServerLibraryId) -> Option<String> {
        let key = library_id.0.clone();
        if let Some(v) = self.library_root_cache.lock().unwrap().get(&key).cloned() {
            return v;
        }
        let root = self
            .media_server_client
            .get_library(library_id)
            .await
            .ok()
            .and_then(|l| l.roots.into_iter().next());
        self.library_root_cache
            .lock()
            .unwrap()
            .insert(key, root.clone());
        root
    }

    /// mylar 格式 series.json 导出（Rust 扩展，参考 komga-mylar.py）：
    /// 写 `<系列目录>/series.json`（oneshot 为 `<系列目录>/<系列名>.oneshot.json`）；
    /// `mylarCovers` 开启时下载系列封面（`cover.jpg` / `<系列名>.cover.jpg`，已存在跳过）。
    async fn write_mylar_series_json(
        &self,
        series: &MediaServerSeries,
        metadata: &SeriesMetadata,
    ) -> Result<(), MediaServerError> {
        let json = mylar_series_json_from_metadata(
            metadata,
            self.post_processor.alternative_series_title_languages(),
        );
        // 导出目录：mylarOutputDir 可重定向（对齐 py --output）；库根目录由内部从
        // 媒体服务器 API 获取（get_library().roots），用于还原相对目录结构，无需配置。
        // oneshot 的 series.url 指向 zip 文件 → 目录为 zip 父目录、文件名取 zip stem。
        let library_root = self.library_root_for(&series.library_id).await;
        let (dir, file_stem) = crate::mylar::resolve_mylar_output_dir(
            &series.url,
            series.oneshot,
            self.mylar_output_dir.as_deref(),
            library_root.as_deref(),
            self.mylar_config_dir.as_deref(),
        );
        let series_name_for_file = file_stem.as_deref().unwrap_or(&series.name);
        write_series_json(&dir, series_name_for_file, series.oneshot, &json)
            .map_err(|e| MediaServerError::Mylar(e))?;
        if self.mylar_covers {
            let cover_name = if series.oneshot {
                format!("{series_name_for_file}.cover.jpg")
            } else {
                "cover.jpg".to_string()
            };
            let cover_path = dir.join(&cover_name);
            if !cover_path.exists() {
                if let Some(image) = self.media_server_client.get_series_thumbnail(&series.id).await? {
                    std::fs::write(&cover_path, &image.bytes)
                        .map_err(|e| MediaServerError::Mylar(format!("write {}: {e}", cover_path.display())))?;
                }
            }
        }
        Ok(())
    }

    async fn update_book_metadata(
        &self,
        series: &MediaServerSeries,
        unprocessed: &SeriesAndBookMetadata,
        processed: &SeriesAndBookMetadata,
    ) -> Result<(), MediaServerError> {
        // 重新拉取书籍列表以获取最新书名（对应 Kotlin 中按书名排序）
        let books = self.media_server_client.get_books(&series.id).await?;
        let write_series_id = self.book_to_write_series_metadata(&unprocessed.book_metadata, &books);

        for book in books.iter() {
            let raw_metadata = processed
                .book_metadata
                .get(&book.id)
                .cloned()
                .flatten();
            // 对齐 Kotlin `postProcessBooks`：orderBooks 开启时所有书都经 orderBook
            // （metadata null 的书用空 BookMetadata 解析书名卷/章号，结果非 null）。
            let metadata = if self.post_processor.order_books_enabled() {
                Some(self.post_processor.maybe_order_book(
                    &book.name,
                    raw_metadata.as_ref().unwrap_or(&BookMetadata::default()),
                ))
            } else {
                raw_metadata
            };
            let write_series_metadata = Some(&book.id) == write_series_id.as_ref();

            for mode in &self.update_modes {
                match mode {
                    UpdateMode::Api => {
                        let update = self
                            .metadata_update_mapper
                            .to_book_metadata_update(metadata.as_ref(), Some(&processed.series_metadata), book);
                        self.media_server_client.update_book_metadata(&book.id, &update).await?;
                    }
                    UpdateMode::ComicInfo => {
                        if book.deleted {
                            continue;
                        }
                        let comic_info = if write_series_metadata {
                            let author_fields = authors_to_comic_info_fields(&processed.series_metadata.authors);
                            Some(series_comic_info(&processed.series_metadata, metadata.as_ref(), author_fields))
                        } else {
                            let authors = metadata
                                .as_ref()
                                .and_then(|m| if m.authors.is_empty() { None } else { Some(m.authors.clone()) })
                                .or_else(|| {
                                    if processed.series_metadata.authors.is_empty() {
                                        None
                                    } else {
                                        Some(processed.series_metadata.authors.clone())
                                    }
                                })
                                .unwrap_or_default();
                            let author_fields = authors_to_comic_info_fields(&authors);
                            comic_info_from_metadata(metadata.as_ref(), Some(&processed.series_metadata), author_fields)
                        };
                        if let Some(comic_info) = comic_info {
                            // 对齐 Kotlin：writeMetadata 内部 runCatching 后 rethrow → job FAILED
                            // （HTTP reset 场景 422；job 场景进错误事件流）。
                            self.comic_info_writer
                                .write_metadata(&book.url, &comic_info)
                                .map_err(|e| MediaServerError::ComicInfo(e.to_string()))?;
                        }
                    }
                    // mylar series.json 是系列级导出，在 update_metadata 中统一处理（不逐书写）。
                    UpdateMode::MylarSeriesJson => {}
                }
            }

            let new_thumbnail = if self.upload_book_covers {
                metadata.as_ref().and_then(|m| m.thumbnail.clone())
            } else {
                None
            };
            let thumbnail_id = self.replace_book_thumbnail(&book.id, new_thumbnail.as_ref()).await?;
            match thumbnail_id {
                Some(thumbnail_id) => {
                    let _ = self.repository.save_book_thumbnail(&BookThumbnail {
                        series_id: book.series_id.clone(),
                        book_id: book.id.clone(),
                        thumbnail_id,
                        media_server: self.media_server.to_string(),
                    });
                }
                None => {
                    let _ = self.repository.delete_book_thumbnail(&book.id, self.media_server);
                }
            }
        }
        Ok(())
    }

    async fn replace_book_thumbnail(
        &self,
        book_id: &MediaServerBookId,
        thumbnail: Option<&Image>,
    ) -> Result<Option<String>, MediaServerError> {
        let existing_match = self
            .repository
            .find_book_thumbnail(book_id, self.media_server)
            .ok()
            .flatten();
        let thumbnails = self.media_server_client.get_book_thumbnails(book_id).await?;

        let select_thumbnail = self.override_existing_covers
            || thumbnails.iter().all(|t| {
                t.r#type == "GENERATED" || existing_match.as_ref().map(|m| m.thumbnail_id == t.id.0).unwrap_or(false)
            });

        let uploaded = match thumbnail {
            Some(thumbnail) => {
                let existing_same = thumbnails.iter().find(|t| {
                    t.r#type == "USER_UPLOADED" && t.file_size == Some(thumbnail.bytes.len() as i64)
                });
                match existing_same {
                    Some(existing) => Some(crate::model::MediaServerBookThumbnail {
                        id: existing.id.clone(),
                        book_id: book_id.clone(),
                        r#type: existing.r#type.clone(),
                        selected: existing.selected,
                        file_size: existing.file_size,
                    }),
                    None => {
                        self.media_server_client
                            .upload_book_thumbnail(book_id, thumbnail, select_thumbnail, false)
                            .await?
                    }
                }
            }
            None => None,
        };

        if let Some(existing) = &existing_match {
            if thumbnails.iter().any(|t| t.id.0 == existing.thumbnail_id) {
                self.media_server_client
                    .delete_book_thumbnail(book_id, &MediaServerThumbnailId(existing.thumbnail_id.clone()))
                    .await?;
            }
        }

        Ok(uploaded.map(|t| t.id.0))
    }

    async fn replace_series_thumbnail(
        &self,
        series_id: &MediaServerSeriesId,
        thumbnail: Option<&Image>,
    ) -> Result<Option<String>, MediaServerError> {
        let matched = self
            .repository
            .find_series_thumbnail(series_id, self.media_server)
            .ok()
            .flatten();
        let thumbnails = self.media_server_client.get_series_thumbnails(series_id).await?;

        let select_thumbnail = self.override_existing_covers || thumbnails.is_empty();

        let uploaded = match thumbnail {
            Some(thumbnail) => {
                // 与 replace_book_thumbnail 一致：已有 USER_UPLOADED 且 file_size 相同的
                // 系列封面视为已最新，跳过重传（override_existing_covers=true 时也适用）。
                let existing_same = thumbnails.iter().find(|t| {
                    t.r#type == "USER_UPLOADED" && t.file_size == Some(thumbnail.bytes.len() as i64)
                });
                match existing_same {
                    Some(existing) => Some(crate::model::MediaServerSeriesThumbnail {
                        id: existing.id.clone(),
                        series_id: series_id.clone(),
                        r#type: existing.r#type.clone(),
                        selected: existing.selected,
                        file_size: existing.file_size,
                    }),
                    None => {
                        self.media_server_client
                            .upload_series_thumbnail(series_id, thumbnail, select_thumbnail, self.lock_covers)
                            .await?
                    }
                }
            }
            None => None,
        };

        if let Some(matched) = &matched {
            if thumbnails.iter().any(|t| t.id.0 == matched.thumbnail_id) {
                self.media_server_client
                    .delete_series_thumbnail(series_id, &MediaServerThumbnailId(matched.thumbnail_id.clone()))
                    .await?;
            }
        }

        Ok(uploaded.map(|t| t.id.0))
    }

    /// 对应 `bookToWriteSeriesMetadata`。
    fn book_to_write_series_metadata(
        &self,
        book_metadata: &std::collections::HashMap<MediaServerBookId, Option<BookMetadata>>,
        books: &[MediaServerBook],
    ) -> Option<MediaServerBookId> {
        if !self.update_modes.contains(&UpdateMode::ComicInfo) {
            return None;
        }
        // 按书名自然排序（对应 Kotlin sortedWith(natSortComparator)）
        let mut sorted: Vec<&MediaServerBook> = books.iter().collect();
        sorted.sort_by(|a, b| case_insensitive_nat_sort(&a.name, &b.name));

        // 优先选择卷号解析为 1 的书
        let first_book = sorted.iter().find_map(|book| {
            BookNameParser::get_volumes(&book.name)
                .filter(|range| range.start.fract() == 0.0 && range.start == 1.0)
                .map(|_| book.id.clone())
        });

        first_book.or_else(|| {
            if book_metadata.values().any(|m| m.is_some()) {
                None
            } else {
                sorted.first().map(|b| b.id.clone())
            }
        })
    }
}

/// 对系列（genres + tags）与书籍元数据（tags）做英文 → 中文翻译
/// （精确匹配，未命中保留原文）。score/publisher 等合成标签由 post_processor
/// 在翻译之后追加，天然不参与翻译。
fn translate_tags(metadata: &SeriesAndBookMetadata, translator: &TagTranslator) -> SeriesAndBookMetadata {
    let mut out = metadata.clone();
    out.series_metadata.genres = metadata
        .series_metadata
        .genres
        .iter()
        .map(|g| translator.translate(g))
        .collect();
    out.series_metadata.tags = metadata
        .series_metadata
        .tags
        .iter()
        .map(|t| translator.translate(t))
        .collect();
    for (_id, meta) in out.book_metadata.iter_mut() {
        if let Some(m) = meta.as_mut() {
            m.tags = m.tags.iter().map(|t| translator.translate(t)).collect();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use komf_core::model::{BookMetadata, SeriesMetadata, SeriesTitle};

    fn translator() -> TagTranslator {
        TagTranslator::builtin()
    }

    fn series_with(genres: Vec<String>, tags: Vec<String>) -> SeriesMetadata {
        SeriesMetadata {
            title: Some(SeriesTitle { name: "X".into(), r#type: None, language: None }),
            genres,
            tags,
            ..Default::default()
        }
    }

    /// series 的 genres 与 tags、book 的 tags 均被翻译；未命中保留原文。
    #[test]
    fn translate_tags_covers_genres_and_books() {
        let tr = translator();
        let series = series_with(
            vec!["Action".into(), "Unknown Genre".into()],
            vec!["Romance".into(), "Score: 8".into()],
        );
        let mut book_meta = BookMetadata::default();
        book_meta.tags = vec!["Adventure".into(), "4-koma".into()];
        let metadata = SeriesAndBookMetadata::new(series, {
            let mut m = std::collections::HashMap::new();
            m.insert(MediaServerBookId("b1".into()), Some(book_meta));
            m
        });

        let out = translate_tags(&metadata, &tr);
        assert_eq!(out.series_metadata.genres, vec!["动作", "Unknown Genre"]);
        assert_eq!(out.series_metadata.tags, vec!["恋爱", "Score: 8"]);
        let book = out.book_metadata.get(&MediaServerBookId("b1".into())).unwrap().as_ref().unwrap();
        assert_eq!(book.tags, vec!["冒险", "四格漫画"]);
        // 原对象不被修改（clone 语义）
        assert_eq!(metadata.series_metadata.genres[0], "Action");
    }
}
