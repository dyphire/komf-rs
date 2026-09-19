//! 元数据更新器 —— 对应 `MetadataUpdater.kt`。
use crate::client::{MediaServerClient, MediaServerError};
use crate::comic_info::{comic_info_from_metadata, series_comic_info, ComicInfoWriter};
use crate::jobs::{BookThumbnail, KomfJobsRepository, SeriesThumbnail};
use crate::metadata_mapper::{authors_to_comic_info_fields, MetadataMapper};
use crate::metadata_post_processor::MetadataPostProcessor;
use crate::model::*;
use komf_core::model::{BookMetadata, Image, SeriesMetadata, UpdateMode};
use komf_core::util::{case_insensitive_nat_sort, BookNameParser};
use std::sync::Arc;

pub struct MetadataUpdater {
    media_server_client: Arc<dyn MediaServerClient>,
    repository: Arc<KomfJobsRepository>,
    media_server: &'static str,
    metadata_update_mapper: MetadataMapper,
    post_processor: MetadataPostProcessor,
    comic_info_writer: ComicInfoWriter,

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
        update_modes: Vec<UpdateMode>,
        override_existing_covers: bool,
        upload_book_covers: bool,
        upload_series_covers: bool,
        lock_covers: bool,
        override_comic_info: bool,
    ) -> Self {
        Self {
            media_server_client,
            repository,
            media_server,
            metadata_update_mapper: MetadataMapper,
            post_processor,
            comic_info_writer: ComicInfoWriter::new(override_comic_info),
            update_modes,
            override_existing_covers,
            upload_book_covers,
            upload_series_covers,
            lock_covers,
        }
    }

    /// 对应 `updateMetadata`。
    pub async fn update_metadata(
        &self,
        series: &MediaServerSeries,
        metadata: &SeriesAndBookMetadata,
    ) -> Result<(), MediaServerError> {
        let processed = self.post_processor.process(metadata);
        self.update_series_metadata(series, &processed.series_metadata).await?;
        self.update_book_metadata(series, metadata, &processed).await?;

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
                self.media_server_client
                    .upload_series_thumbnail(series_id, thumbnail, select_thumbnail, self.lock_covers)
                    .await?
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
