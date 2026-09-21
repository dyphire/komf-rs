//! 元数据服务 —— 对应 `MetadataService.kt` 与 `MetadataServiceProvider.kt`。
use crate::client::{MediaServerClient, MediaServerError};
use crate::config::{ChineseConversionConfig, ChineseField, SearchTitleExtractionConfig};
use crate::jobs::{KomfJobTracker, KomfJobsRepository, MetadataJobEvent, MetadataJobId, SeriesMatch};
use crate::metadata_merger::MetadataMerger;
use crate::metadata_updater::MetadataUpdater;
use crate::model::*;
use komf_core::model::{
    BookMetadata, BookQualifier, BookRange, Image, MatchQuery, MediaType, ProviderSeriesId, ProviderSeriesMetadata,
    SeriesBook, SeriesSearchResult, WebLink,
};
use komf_core::providers::{CoreProviders, MetadataProvider};
use komf_core::util::BookNameParser;
use std::collections::HashMap;
use std::sync::Arc;

/// Kotlin `"(?i)\\s?[EÉ]dition\\s?".toRegex()`。
const EDITION_REGEX: &str = r"(?i)\s?[EÉ]dition\s?";

/// links 直用结果（fetch_from_links 返回）。
enum LinksFetchOutcome {
    /// 无任何可解析的 provider 链接
    NoCandidates,
    /// 有候选但全部拉取失败
    AllFailed,
    /// 成功（合并后的元数据, 主 provider, 主 provider series id）
    Success(SeriesAndBookMetadata, CoreProviders, String),
}

/// match_series_metadata_inner 的结果（Rust 扩展：区分"跳过"与"匹配失败"，
/// 被跳过的系列不加入失败收藏夹）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchOutcome {
    /// 成功更新元数据
    Updated,
    /// 匹配失败（无 provider 命中）→ 加入失败收藏夹
    NoMatch,
    /// 被跳过（linksSkip 判定已匹配等）→ 不加入
    Skipped,
}

pub struct MetadataService {
    media_server_client: Arc<dyn MediaServerClient>,
    metadata_providers: Arc<komf_core::providers::MetadataProviders>,
    aggregate_metadata: bool,
    metadata_merger: MetadataMerger,
    metadata_update_service: Arc<MetadataUpdater>,
    series_match_repository: Arc<KomfJobsRepository>,
    media_server: &'static str,
    library_type: MediaType,
    job_tracker: Arc<KomfJobTracker>,
    search_title_extraction: SearchTitleExtractionConfig,
    links_skip_enabled: bool,
    links_match_enabled: bool,
    /// Auto-Identify Library 匹配失败系列归集收藏夹名（Rust 扩展）；None = 禁用。
    failed_match_collection_name: Option<String>,
    /// 本 service 对应的库（default service 为 None）。收藏夹按库级建（名 = 配置名 + `[库名]`）。
    library_id: Option<MediaServerLibraryId>,
    /// Rust 扩展：简繁转换配置（搜索/匹配/更新范围与字段）。
    chinese_conversion: ChineseConversionConfig,
    /// 转换器实例（按配置方向构建；未启用/初始化失败为 None → 不转换）。
    chinese_converter: Option<std::sync::Arc<komf_core::util::ChineseConverter>>,
}

impl MetadataService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        media_server_client: Arc<dyn MediaServerClient>,
        metadata_providers: Arc<komf_core::providers::MetadataProviders>,
        aggregate_metadata: bool,
        metadata_merger: MetadataMerger,
        metadata_update_service: Arc<MetadataUpdater>,
        series_match_repository: Arc<KomfJobsRepository>,
        media_server: &'static str,
        library_type: MediaType,
        job_tracker: Arc<KomfJobTracker>,
        search_title_extraction: SearchTitleExtractionConfig,
        links_skip_enabled: bool,
        links_match_enabled: bool,
        failed_match_collection_name: Option<String>,
        library_id: Option<MediaServerLibraryId>,
        chinese_conversion: ChineseConversionConfig,
    ) -> Self {
        let chinese_converter = if chinese_conversion.enabled {
            match komf_core::util::ChineseConverter::new(chinese_conversion.direction) {
                Ok(c) => Some(std::sync::Arc::new(c)),
                Err(e) => {
                    tracing::warn!("chinese conversion disabled (opencc init failed): {e}");
                    None
                }
            }
        } else {
            None
        };
        Self {
            media_server_client,
            metadata_providers,
            aggregate_metadata,
            metadata_merger,
            metadata_update_service,
            series_match_repository,
            media_server,
            library_type,
            job_tracker,
            search_title_extraction,
            links_skip_enabled,
            links_match_enabled,
            failed_match_collection_name,
            library_id,
            chinese_conversion,
            chinese_converter,
        }
    }

    pub fn available_providers(&self, library_id: &MediaServerLibraryId) -> Vec<CoreProviders> {
        self.metadata_providers
            .providers(&library_id.0)
            .iter()
            .map(|p| p.provider_name())
            .collect()
    }

    pub async fn search_series_metadata(
        &self,
        series_name: &str,
        library_id: Option<&MediaServerLibraryId>,
    ) -> Vec<SeriesSearchResult> {
        let providers = match library_id {
            Some(library_id) => self.metadata_providers.providers(&library_id.0),
            None => self.metadata_providers.default_providers_list(),
        };
        let mut tasks = Vec::new();
        let library_type = self.library_type;
        // Rust 扩展：简繁转换应用于搜索关键词（chineseConversion.search）。
        let name = if self.chinese_conversion.enabled
            && self.chinese_conversion.search
        {
            match &self.chinese_converter {
                Some(c) => c.convert(series_name),
                None => series_name.to_string(),
            }
        } else {
            series_name.to_string()
        };
        for provider in providers {
            let name = name.clone();
            let provider_ref: Arc<dyn MetadataProvider> = provider;
            tasks.push(tokio::spawn(async move {
                // Rust 扩展：输入为 provider 网页链接时直接按 id 获取（跳过站点搜索）。
                // 搜索结果由 provider 自行构造（封面/标题选择/mediaType/语言/nsfw 复用其搜索显示逻辑）。
                if let Some(result) = provider_ref.resolve_link_search_result(&name).await {
                    return vec![result];
                }
                match provider_ref.search_series(&name, 5, Some(library_type)).await {
                    Ok(results) => results,
                    Err(error) => {
                        tracing::error!("search failed for provider {}: {}", provider_ref.provider_name(), error);
                        Vec::new()
                    }
                }
            }));
        }
        let mut results = Vec::new();
        for task in tasks {
            if let Ok(mut task_results) = task.await {
                results.append(&mut task_results);
            }
        }
        results
    }

    pub async fn get_series_cover(
        &self,
        library_id: &MediaServerLibraryId,
        provider_name: CoreProviders,
        provider_series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, MediaServerError> {
        let provider = self
            .metadata_providers
            .provider(&library_id.0, provider_name)
            .ok_or_else(|| MediaServerError::message(format!("Provider {provider_name} is not enabled for library {}", library_id.0)))?;
        provider
            .get_series_cover(provider_series_id)
            .await
            .map_err(|e| MediaServerError::message(e.to_string()))
    }

    /// 对应 `setSeriesMetadata`（identify）。
    ///
    /// 对齐 Kotlin：返回 jobId（错误不向调用方传播，而是发错误事件并将 job 置 FAILED）。
    /// `edition` 对应 Kotlin 签名第 4 参数（deprecated identify 会传入）。
    pub async fn set_series_metadata(
        &self,
        series_id: &MediaServerSeriesId,
        provider_name: CoreProviders,
        provider_series_id: &ProviderSeriesId,
        edition: Option<&str>,
    ) -> MetadataJobId {
        let (job_id, tx) = self.job_tracker.register_job(series_id.clone()).await;
        let result = self
            .set_series_metadata_inner(&tx, series_id, provider_name, provider_series_id, edition)
            .await;
        self.finish_job(&job_id, &tx, result).await;
        job_id
    }

    /// identify（手动识别）专用 —— Rust 扩展：linksMatchEnabled=true 且系列 links
    /// 有可解析的 provider 链接时，直接使用 links 链接更新（含多 provider 合并，
    /// aggregate=true 时，与 Auto-Identify 共用 fetch_from_links）；不受 linksSkipEnabled
    /// 限制（手动操作不被自动跳过拦截）。links 无可用链接/全部失败时回退请求指定的
    /// provider + id。
    pub async fn identify_series_metadata(
        &self,
        series_id: &MediaServerSeriesId,
        fallback_provider: CoreProviders,
        fallback_provider_series_id: &ProviderSeriesId,
        edition: Option<&str>,
    ) -> MetadataJobId {
        let (job_id, tx) = self.job_tracker.register_job(series_id.clone()).await;
        let result = self
            .identify_series_metadata_inner(
                &tx,
                series_id,
                fallback_provider,
                fallback_provider_series_id,
                edition,
            )
            .await;
        // Rust 扩展：Identify 成功 → 从失败收藏夹移除。
        if result.is_ok() {
            self.maybe_remove_from_failed_collection(series_id).await;
        }
        self.finish_job(&job_id, &tx, result).await;
        job_id
    }

    async fn identify_series_metadata_inner(
        &self,
        tx: &tokio::sync::broadcast::Sender<MetadataJobEvent>,
        series_id: &MediaServerSeriesId,
        fallback_provider: CoreProviders,
        fallback_provider_series_id: &ProviderSeriesId,
        edition: Option<&str>,
    ) -> Result<(), (Option<CoreProviders>, String)> {
        let series = self
            .media_server_client
            .get_series(series_id)
            .await
            .map_err(|e| (None, e.to_string()))?;
        let books = self
            .media_server_client
            .get_books(series_id)
            .await
            .map_err(|e| (None, e.to_string()))?;
        let series_title = if series.metadata.title.trim().is_empty() {
            series.name.clone()
        } else {
            series.metadata.title.clone()
        };
        // Rust 扩展：identify 也受 linksMatchEnabled 控制（links 直用 + 多 provider 合并），
        // 不受 linksSkipEnabled 限制（手动操作不被自动跳过拦截）。
        if self.links_match_enabled {
            match self.fetch_from_links(&series, &books, tx).await {
                LinksFetchOutcome::Success(metadata, provider, provider_series_id) => {
                    // Rust 扩展：简繁转换应用于元数据更新（identify links 直用分支同样生效）
                    let metadata = self.apply_chinese_conversion(metadata);
                    let _ = tx.send(MetadataJobEvent::PostProcessingStart);
                    self.metadata_update_service
                        .update_metadata(&series, &metadata)
                        .await
                        .map_err(|e| (Some(provider), e.to_string()))?;
                    let _ = self.series_match_repository.save_series_match(&SeriesMatch {
                        series_id: series.id.clone(),
                        r#type: "MANUAL".to_string(),
                        media_server: self.media_server.to_string(),
                        provider,
                        provider_series_id,
                    });
                    tracing::info!(
                        "finished metadata update of series \"{series_title}\" {}",
                        series.id.0
                    );
                    return Ok(());
                }
                _ => {} // 无候选 / 全部失败 → 回退请求参数
            }
        }
        // 回退：请求指定的 provider（links 未启用 / 无候选 / 全部失败）
        self.set_series_metadata_inner(tx, series_id, fallback_provider, fallback_provider_series_id, edition)
            .await
    }

    /// links 直用（Auto-Identify 与 identify 共用）：收集 links 中所有可解析的
    /// provider 链接，逐个直用拉取元数据（不搜索）；aggregate=true 时多 provider 合并
    /// （与搜索路径 aggregate 相同的 merge 逻辑），aggregate=false 时只使用第一个成功
    /// 链接（与搜索路径单 provider 一致）。单个候选失败只跳过该候选。
    async fn fetch_from_links(
        &self,
        series: &MediaServerSeries,
        books: &[MediaServerBook],
        tx: &tokio::sync::broadcast::Sender<MetadataJobEvent>,
    ) -> LinksFetchOutcome {
        // oneshot 单本系列：书籍级链接（聚合优先，books 回退）参与收集
        let combined_links = series_links_including_books(series, books);
        let candidates = links_match_providers(&combined_links);
        if candidates.is_empty() {
            return LinksFetchOutcome::NoCandidates;
        }
        let series_title = if series.metadata.title.trim().is_empty() {
            series.name.clone()
        } else {
            series.metadata.title.clone()
        };
        let mut result: Option<SeriesAndBookMetadata> = None;
        let mut first: Option<(CoreProviders, String)> = None;
        for (provider_name, provider_series_id) in candidates {
            let Some(provider) = self.metadata_providers.provider(&series.library_id.0, provider_name)
            else {
                continue;
            };
            let _ = tx.send(MetadataJobEvent::ProviderSeries {
                provider: provider_name,
            });
            match provider.get_series_metadata(&provider_series_id).await {
                Ok(provider_metadata) => {
                    let book_metadata = self
                        .get_book_metadata(books, &provider_metadata, provider, None, tx)
                        .await;
                    let new = SeriesAndBookMetadata::new(provider_metadata.metadata, book_metadata)
                        .with_oneshots(books);
                    match result.take() {
                        Some(base) => {
                            // 多 provider 合并：与 aggregate_metadata_from_providers
                            // 相同的 merge_metadata（metadata_merger 配置生效）
                            let _ = tx.send(MetadataJobEvent::ProviderCompleted {
                                provider: provider_name,
                            });
                            tracing::info!(
                                "merging link metadata from {provider_name} for {series_title} {}",
                                series.id.0
                            );
                            result = Some(self.merge_metadata(base, new));
                        }
                        None => {
                            tracing::info!(
                                "using link from {provider_name} for {series_title} {}",
                                series.id.0
                            );
                            first = Some((provider_name, provider_series_id.0.clone()));
                            result = Some(new);
                            if !self.aggregate_metadata {
                                break; // 与搜索路径一致：非聚合只用第一个
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("link metadata fetch failed for {provider_name}: {e}");
                    // 跳过该候选继续下一个
                }
            }
        }
        match first {
            Some((provider, id)) => LinksFetchOutcome::Success(
                result.expect("first success implies result"),
                provider,
                id,
            ),
            None => LinksFetchOutcome::AllFailed,
        }
    }

    async fn set_series_metadata_inner(
        &self,
        tx: &tokio::sync::broadcast::Sender<MetadataJobEvent>,
        series_id: &MediaServerSeriesId,
        provider_name: CoreProviders,
        provider_series_id: &ProviderSeriesId,
        edition: Option<&str>,
    ) -> Result<(), (Option<CoreProviders>, String)> {
        let series = self
            .media_server_client
            .get_series(series_id)
            .await
            .map_err(|e| (None, e.to_string()))?;
        let books = self
            .media_server_client
            .get_books(series_id)
            .await
            .map_err(|e| (None, e.to_string()))?;
        let series_title = if series.metadata.title.trim().is_empty() {
            series.name.clone()
        } else {
            series.metadata.title.clone()
        };
        tracing::info!("Setting metadata for series \"{series_title}\" {} using {provider_name} {provider_series_id}", series.id.0);

        let provider = self
            .metadata_providers
            .provider(&series.library_id.0, provider_name)
            .ok_or_else(|| (None, format!("Provider {provider_name} is not enabled")))?;

        let _ = tx.send(MetadataJobEvent::ProviderSeries { provider: provider_name });
        let provider_metadata = provider
            .get_series_metadata(provider_series_id)
            .await
            .map_err(|e| (Some(provider_name), e.to_string()))?;
        let book_metadata = self
            .get_book_metadata(&books, &provider_metadata, provider, edition, &tx)
            .await;
        let _ = tx.send(MetadataJobEvent::ProviderCompleted { provider: provider_name });

        let metadata = if self.aggregate_metadata {
            let providers: Vec<Arc<dyn MetadataProvider>> = self
                .metadata_providers
                .providers(&series.library_id.0)
                .into_iter()
                .filter(|p| p.provider_name() != provider_name)
                .collect();
            self.aggregate_metadata_from_providers(&series, &books, provider_metadata.metadata, book_metadata, providers, edition, &tx)
                .await
        } else {
            // Kotlin key 为 MediaServerBook（内嵌 oneshot），Rust 以 book_oneshots 等价承载。
            SeriesAndBookMetadata::new(provider_metadata.metadata, book_metadata).with_oneshots(&books)
        };

        // Rust 扩展：简繁转换应用于元数据更新（chineseConversion.update.enabled + fields）
        let metadata = self.apply_chinese_conversion(metadata);
        let _ = tx.send(MetadataJobEvent::PostProcessingStart);
        self.metadata_update_service
            .update_metadata(&series, &metadata)
            .await
            .map_err(|e| (None, e.to_string()))?;

        let _ = self.series_match_repository.save_series_match(&SeriesMatch {
            series_id: series.id.clone(),
            r#type: "MANUAL".to_string(),
            media_server: self.media_server.to_string(),
            provider: provider_name,
            provider_series_id: provider_series_id.0.clone(),
        });

        tracing::info!("finished metadata update of series \"{series_title}\" {}", series.id.0);
        Ok(())
    }

    /// 收藏夹名称对：(主名 = 配置名 + `[库名]`，兜底名 = 配置名 + `[库id]`)。
    /// 按库 id 专属——同名库冲突时先建者占主名，后续库自动落到库 id 兜底名。
    /// default 服务无库 id，两名为同一配置名。
    async fn failed_collection_names(&self) -> Option<(String, String)> {
        let base = self.failed_match_collection_name.as_ref()?;
        let Some(library_id) = &self.library_id else {
            return Some((base.clone(), base.clone()));
        };
        match self.media_server_client.get_library(library_id).await {
            Ok(library) => Some((
                format!("{base} [{}]", library.name),
                format!("{base} [{}]", library_id.0),
            )),
            Err(_) => Some((base.clone(), base.clone())),
        }
    }

    /// 按名称对查找收藏夹（先主名后兜底名）→ 集合（id + seriesIds）；未找到/失败返回 None。
    async fn find_collection(&self, names: &(String, String)) -> Option<MediaServerCollection> {
        match self.media_server_client.get_collections().await {
            Ok(collections) => collections
                .into_iter()
                .find(|c| c.name == names.0 || c.name == names.1),
            Err(error) => {
                tracing::warn!("failed to load collections: {error}");
                None
            }
        }
    }

    /// 匹配成功 → 从失败收藏夹移除（public match / identify 用；仅配置非空时生效）。
    async fn maybe_remove_from_failed_collection(&self, series_id: &MediaServerSeriesId) {
        let Some(names) = self.failed_collection_names().await else {
            return;
        };
        let Some(collection) = self.find_collection(&names).await else {
            return;
        };
        if !collection.series_ids.iter().any(|id| id == &series_id.0) {
            return;
        }
        let ids: Vec<String> = collection
            .series_ids
            .into_iter()
            .filter(|id| id != &series_id.0)
            .collect();
        match self
            .media_server_client
            .update_collection_series(&collection.id, &ids)
            .await
        {
            Ok(()) => tracing::info!(
                "removed series {} from collection \"{}\" (matched)",
                series_id.0, collection.name
            ),
            Err(error) => tracing::warn!(
                "failed to remove series {} from collection \"{}\": {error}",
                series_id.0, collection.name
            ),
        }
    }

    /// 对应 `matchLibraryMetadata`。
    ///
    /// 对齐 Kotlin：返回 Unit（后台启动语义；分页/系列级错误仅 log，路由恒 202）。
    pub async fn match_library_metadata(&self, library_id: &MediaServerLibraryId) {
        // Rust 扩展：失败收藏夹（按库 id 专属；主名 = 配置名 + `[库名]`，
        // 同名冲突时自动落到配置名 + `[库id]`）。整库扫描加载一次，
        // 之后本地维护 seriesIds 缓存，避免每个系列一次 GET。
        let collection_names = self.failed_collection_names().await;
        let mut collection: Option<(String, Vec<String>)> = match &collection_names {
            Some(names) => self.find_collection(names).await.map(|c| (c.id, c.series_ids)),
            None => None,
        };
        let mut page_number = 1;
        loop {
            let page = match self.media_server_client.get_series_page(library_id, page_number).await {
                Ok(page) => page,
                Err(error) => {
                    tracing::error!("library scan failed: {error}");
                    break;
                }
            };
            for series in page.content {
                // 已在失败收藏夹内的条目自动跳过。
                if let Some((_, ids)) = &collection {
                    if ids.iter().any(|id| id == &series.id.0) {
                        tracing::info!(
                            "series \"{}\" {} is in failed-match collection, skipping",
                            series.name,
                            series.id.0
                        );
                        continue;
                    }
                }
                let collection_name_display = collection_names
                    .as_ref()
                    .map(|names| names.0.clone())
                    .unwrap_or_default();
                let outcome = self.match_series_metadata_outcome(&series.id).await;
                if outcome == MatchOutcome::Updated {
                    // 匹配成功 → 从收藏夹删除。
                    if let Some((collection_id, ids)) = &mut collection {
                        if ids.iter().any(|id| id == &series.id.0) {
                            ids.retain(|id| id != &series.id.0);
                            if let Err(error) = self
                                .media_server_client
                                .update_collection_series(collection_id, ids)
                                .await
                            {
                                tracing::warn!(
                                    "failed to remove series {} from collection: {error}",
                                    series.id.0
                                );
                            } else {
                                tracing::info!(
                                    "removed series \"{}\" {} from failed-match collection (matched)",
                                    series.name,
                                    series.id.0
                                );
                            }
                        }
                    }
                } else if outcome == MatchOutcome::NoMatch {
                    // 匹配失败 → 加入收藏夹（不存在则创建）；被跳过（Skipped）不加入。
                    let Some(names) = &collection_names else { continue };
                    match &mut collection {
                        Some((collection_id, ids)) => {
                            ids.push(series.id.0.clone());
                            if let Err(error) = self
                                .media_server_client
                                .update_collection_series(collection_id, ids)
                                .await
                            {
                                tracing::warn!(
                                    "failed to add series {} to collection: {error}",
                                    series.id.0
                                );
                            } else {
                                tracing::info!(
                                    "added series \"{}\" {} to failed-match collection \"{}\"",
                                    series.name, series.id.0, collection_name_display
                                );
                            }
                        }
                        None => {
                            // 先主名（配置名 + [库名]）；同名收藏夹被其他库占用（创建冲突）
                            // 时自动回退兜底名（配置名 + [库id]），保证按库 id 专属。
                            let mut created = self
                                .media_server_client
                                .create_collection(&names.0, &[series.id.0.clone()])
                                .await;
                            if created.is_err() && names.0 != names.1 {
                                tracing::warn!(
                                    "collection \"{}\" creation failed, falling back to \"{}\"",
                                    names.0, names.1
                                );
                                created = self
                                    .media_server_client
                                    .create_collection(&names.1, &[series.id.0.clone()])
                                    .await;
                            }
                            match created {
                                Ok(created_collection) => {
                                    tracing::info!(
                                        "created collection \"{}\" with series \"{}\" {}",
                                        created_collection.name,
                                        series.name,
                                        series.id.0
                                    );
                                    collection = Some((created_collection.id, created_collection.series_ids));
                                }
                                Err(error) => {
                                    tracing::warn!("failed to create failed-match collection: {error}");
                                }
                            }
                        }
                    }
                }
            }
            if page.page_number >= page.total_pages - 1 {
                break;
            }
            page_number += 1;
        }
        tracing::info!("Finished library scan");
    }

    /// 对应 `matchSeriesMetadata`。返回 jobId，错误通过 job 事件/状态体现。
    /// Rust 扩展：匹配成功（更新了元数据）→ 自动从失败收藏夹移除。
    pub async fn match_series_metadata(&self, series_id: &MediaServerSeriesId) -> MetadataJobId {
        self.match_series_metadata_impl(series_id, true).await
    }

    /// SSE 事件触发（on_books_added）专用：匹配**不受 linksSkipEnabled 影响**
    /// （有 provider 链接不跳过、继续搜索），但**尊重 linksMatchEnabled**（链接直用）。
    pub async fn match_series_metadata_no_links_skip(
        &self,
        series_id: &MediaServerSeriesId,
    ) -> MetadataJobId {
        self.match_series_metadata_impl(series_id, false).await
    }

    async fn match_series_metadata_impl(
        &self,
        series_id: &MediaServerSeriesId,
        apply_links_skip: bool,
    ) -> MetadataJobId {
        let (job_id, tx) = self.job_tracker.register_job(series_id.clone()).await;
        let result = self.match_series_metadata_inner(&tx, series_id, apply_links_skip).await;
        if result.as_ref().is_ok_and(|outcome| *outcome == MatchOutcome::Updated) {
            self.maybe_remove_from_failed_collection(series_id).await;
        }
        self.finish_job(&job_id, &tx, result.map(|_| ())).await;
        job_id
    }

    /// 库级扫描（Auto-Identify Library）用：注册 job 并执行匹配，但**不**做失败收藏夹
    /// 移除（由 match_library_metadata 用本地缓存统一维护）。返回匹配结果。
    async fn match_series_metadata_outcome(&self, series_id: &MediaServerSeriesId) -> MatchOutcome {
        let (job_id, tx) = self.job_tracker.register_job(series_id.clone()).await;
        let result = self.match_series_metadata_inner(&tx, series_id, true).await;
        let outcome = result.as_ref().map(|o| *o).unwrap_or(MatchOutcome::Skipped);
        self.finish_job(&job_id, &tx, result.map(|_| ())).await;
        outcome
    }

    /// 对应 `matchSeriesMetadata` 内部实现。Ok(Updated) = 成功更新；
    /// Ok(NoMatch) = 匹配失败；Ok(Skipped) = 被跳过（linksSkip 等，job 正常 complete）。
    async fn match_series_metadata_inner(
        &self,
        tx: &tokio::sync::broadcast::Sender<MetadataJobEvent>,
        series_id: &MediaServerSeriesId,
        apply_links_skip: bool,
    ) -> Result<MatchOutcome, (Option<CoreProviders>, String)> {
        let series = self
            .media_server_client
            .get_series(series_id)
            .await
            .map_err(|e| (None, e.to_string()))?;
        let books = self
            .media_server_client
            .get_books(series_id)
            .await
            .map_err(|e| (None, e.to_string()))?;
        let series_title = if series.metadata.title.trim().is_empty() {
            series.name.clone()
        } else {
            series.metadata.title.clone()
        };

        // 已有手动匹配则直接使用
        let existing_match = self
            .series_match_repository
            .find_manual_for(series_id, self.media_server)
            .ok()
            .flatten();
        let mut matched_provider: Option<CoreProviders> = None;
        let existing_provider = existing_match
            .as_ref()
            .and_then(|existing| self.metadata_providers.provider(&series.library_id.0, existing.provider));
        let match_result = if let (Some(existing), Some(provider)) = (existing_match.as_ref(), existing_provider) {
            tracing::info!(
                "using {} from previous manual identification for {series_title} {}",
                provider.provider_name(),
                series.id.0
            );
            let _ = tx.send(MetadataJobEvent::ProviderSeries { provider: existing.provider });
            let provider_metadata = provider
                .get_series_metadata(&ProviderSeriesId(existing.provider_series_id.clone()))
                .await
                .map_err(|e| (Some(existing.provider), e.to_string()))?;
            let book_metadata = self.get_book_metadata(&books, &provider_metadata, provider, None, &tx).await;
            matched_provider = Some(existing.provider);
            Some(SeriesAndBookMetadata::new(provider_metadata.metadata, book_metadata).with_oneshots(&books))
        } else {
            // Rust 扩展（用户需求）：系列 links 已包含任一 provider 识别特征（label/域名）。
            // linksMatchEnabled 优先于 linksSkipEnabled：启用链接直用时不再判断跳过
            // （Identify/Auto-Identify 预期：有链接就直接按链接更新）。
            // oneshot 单本系列：书籍级链接参与已匹配判定（聚合优先，books 回退）
            let combined_links = series_links_including_books(&series, &books);
            let links_match = links_indicate_matched(&combined_links);
            let from_link: Option<SeriesAndBookMetadata> = if links_match && self.links_match_enabled {
                // 链接直用：多 provider 合并（与 identify 共用 fetch_from_links）
                match self.fetch_from_links(&series, &books, &tx).await {
                    LinksFetchOutcome::Success(metadata, provider, _) => {
                        matched_provider = Some(provider);
                        Some(metadata)
                    }
                    // 有 provider 特征但无可用直用链接：回退按 skip 配置决定
                    // （SSE 触发 apply_links_skip=false → 不跳过，继续搜索）
                    LinksFetchOutcome::NoCandidates => {
                        if self.links_skip_enabled && apply_links_skip {
                            tracing::info!(
                                "series {series_title} {} already matched (provider link present), skipping",
                                series.id.0
                            );
                            return Ok(MatchOutcome::Skipped);
                        }
                        None
                    }
                    // 有候选但全部失败：搜索兜底
                    LinksFetchOutcome::AllFailed => None,
                }
            } else {
                // 链接直用未启用：按 skip 配置决定（Kotlin 无此行为，Rust 扩展）
                // （SSE 触发 apply_links_skip=false → 不跳过，继续搜索）
                if links_match && self.links_skip_enabled && apply_links_skip {
                    tracing::info!(
                        "series {series_title} {} already matched (provider link present), skipping",
                        series.id.0
                    );
                    return Ok(MatchOutcome::Skipped);
                }
                None
            };
            if let Some(metadata) = from_link {
                Some(metadata)
            } else {
                // 无手动匹配：尝试各 provider
                let search_titles = build_search_titles(
                &series_title,
                &series.metadata.alternative_titles,
                &self.search_title_extraction,
            );

                tracing::info!("attempting to match series \"{series_title}\" {}", series.id.0);

                let providers = self.metadata_providers.providers(&series.library_id.0);
                let mut result = None;
                for provider in providers {
                    if let Some(matched) = self
                        .match_series(&series, &books, &search_titles, provider.clone(), None, &tx)
                        .await
                    {
                        matched_provider = Some(provider.provider_name());
                        result = Some(matched);
                        break;
                    }
                }
                result
            }
        };

        let Some(matched) = match_result else {
            tracing::info!("no match found for series {series_title} {}", series.id.0);
            return Ok(MatchOutcome::NoMatch);
        };

        // 对齐 Kotlin：matchResult 非 null 后统一发 ProviderCompleted（含 manual 分支）。
        let _ = tx.send(MetadataJobEvent::ProviderCompleted {
            provider: matched_provider.expect("matched implies provider"),
        });

        let metadata = if self.aggregate_metadata {
            let providers: Vec<Arc<dyn MetadataProvider>> = self
                .metadata_providers
                .providers(&series.library_id.0)
                .into_iter()
                .filter(|p| Some(p.provider_name()) != matched_provider)
                .collect();
            // 聚合除首个成功 provider 外的其余 provider
            self.aggregate_metadata_from_providers(
                &series,
                &books,
                matched.series_metadata.clone(),
                matched.book_metadata.clone(),
                providers,
                None,
                &tx,
            )
            .await
        } else {
            matched
        };

        // Rust 扩展：简繁转换应用于元数据更新（chineseConversion.update.enabled + fields）
        let metadata = self.apply_chinese_conversion(metadata);
        let _ = tx.send(MetadataJobEvent::PostProcessingStart);
        self.metadata_update_service
            .update_metadata(&series, &metadata)
            .await
            .map_err(|e| (None, e.to_string()))?;
        tracing::info!("finished metadata update of series \"{series_title}\" {}", series.id.0);
        Ok(MatchOutcome::Updated)
    }

    /// 对齐 Kotlin `launchJob` 的 catch 链 + `KomfJobTracker` listener：
    /// 错误 → 发对应错误事件 → fail_job（ProviderError 的 fail message 为 "provider\nmessage" 两行）；
    /// 成功 → complete_job。
    async fn finish_job(
        &self,
        job_id: &MetadataJobId,
        tx: &tokio::sync::broadcast::Sender<MetadataJobEvent>,
        result: Result<(), (Option<CoreProviders>, String)>,
    ) {
        match result {
            Ok(()) => {
                self.job_tracker.complete_job(job_id).await;
            }
            Err((Some(provider), message)) => {
                let _ = tx.send(MetadataJobEvent::ProviderError {
                    provider,
                    message: message.clone(),
                });
                self.job_tracker
                    .fail_job(job_id, format!("{provider}\n{message}"), false)
                    .await;
            }
            Err((None, message)) => {
                let _ = tx.send(MetadataJobEvent::ProcessingError { message: message.clone() });
                self.job_tracker.fail_job(job_id, message, true).await;
            }
        }
    }

    /// Rust 扩展：简繁转换应用于系列元数据（chineseConversion.update.enabled + update.fields）。
    /// 仅转换系列级字段（标题/体裁/标签/简介）；书籍级元数据不参与。
    fn apply_chinese_conversion(&self, metadata: SeriesAndBookMetadata) -> SeriesAndBookMetadata {
        apply_chinese_conversion_to_metadata(
            metadata,
            &self.chinese_conversion,
            self.chinese_converter.as_ref().map(|v| &**v),
        )
    }

    async fn match_series(
        &self,
        series: &MediaServerSeries,
        books: &[MediaServerBook],
        search_titles: &[String],
        provider: Arc<dyn MetadataProvider>,
        edition: Option<&str>,
        tx: &tokio::sync::broadcast::Sender<MetadataJobEvent>,
    ) -> Option<SeriesAndBookMetadata> {
        for search_title in search_titles {
            tracing::info!("searching \"{search_title}\" using {}", provider.provider_name());
            let _ = tx.send(MetadataJobEvent::ProviderSeries { provider: provider.provider_name() });

            let query = self.create_match_query(search_title, series, books).await;
            // Rust 扩展：搜索标题为 provider 网页链接时直接按 id 获取（跳过搜索/相似度匹配）
            if let Some(id) = provider.resolve_link_id(&query.series_name) {
                if let Ok(result) = provider
                    .get_series_metadata(&ProviderSeriesId(id))
                    .await
                {
                    tracing::info!(
                        "found match via link: \"{}\" from {}  {}",
                        result.metadata.titles.first().map(|t| t.name.clone()).unwrap_or_default(),
                        provider.provider_name(),
                        result.id.0
                    );
                    let book_metadata = self.get_book_metadata(books, &result, provider.clone(), edition, tx).await;
                    return Some(SeriesAndBookMetadata::new(result.metadata, book_metadata).with_oneshots(books));
                }
            }
            let result = match provider.match_series_metadata(&query).await {
                Ok(result) => result,
                Err(error) => {
                    tracing::error!("match failed for provider {}: {}", provider.provider_name(), error);
                    let _ = tx.send(MetadataJobEvent::ProviderError {
                        provider: provider.provider_name(),
                        message: error.to_string(),
                    });
                    None
                }
            };

            if let Some(result) = result {
                tracing::info!(
                    "found match: \"{}\" from {}  {}",
                    result.metadata.titles.first().map(|t| t.name.clone()).unwrap_or_default(),
                    provider.provider_name(),
                    result.id.0
                );
                let book_metadata = self.get_book_metadata(books, &result, provider.clone(), edition, tx).await;
                return Some(SeriesAndBookMetadata::new(result.metadata, book_metadata).with_oneshots(books));
            }
        }
        None
    }

    /// 对应 `getBookMetadata`。对齐 Kotlin 的整批 try-catch 语义：
    /// 逐本 `.ok()`：单本获取失败只影响该书（该本置 None），其余书正常继续。
    /// 有意保留与 Kotlin「整批 try-catch 失败全部置 null」不同的行为（用户拍板）。
    async fn get_book_metadata(
        &self,
        books: &[MediaServerBook],
        series_meta: &ProviderSeriesMetadata,
        provider: Arc<dyn MetadataProvider>,
        edition: Option<&str>,
        tx: &tokio::sync::broadcast::Sender<MetadataJobEvent>,
    ) -> HashMap<MediaServerBookId, Option<BookMetadata>> {
        let metadata_match = self.associate_book_metadata(books, &series_meta.books, edition);

        let fetch_size = metadata_match.iter().filter(|(_, v)| v.is_some()).count();
        let mut progress = 1;
        let mut result = HashMap::new();
        for (book, series_book) in metadata_match {
            if let Some(series_book) = series_book {
                tracing::info!("({}) fetching metadata for book {}", provider.provider_name(), series_book.name.as_deref().unwrap_or(""));
                let _ = tx.send(MetadataJobEvent::ProviderBook {
                    provider: provider.provider_name(),
                    total_books: fetch_size as i32,
                    book_progress: progress,
                });
                progress += 1;
                // 逐本 .ok()：单本失败只影响该书；失败留痕（warn 日志）便于排障。
                let metadata = match provider.get_book_metadata(&series_meta.id, &series_book.id).await {
                    Ok(result) => Some(result.metadata),
                    Err(error) => {
                        tracing::warn!(
                            "({}) failed to fetch metadata for book {}: {}",
                            provider.provider_name(),
                            series_book.name.as_deref().unwrap_or(""),
                            error
                        );
                        None
                    }
                };
                result.insert(book.id.clone(), metadata);
            } else {
                result.insert(book.id.clone(), None);
            }
        }
        result
    }

    /// 对应 `associateBookMetadata`。
    fn associate_book_metadata(
        &self,
        books: &[MediaServerBook],
        provider_books: &[SeriesBook],
        edition: Option<&str>,
    ) -> Vec<(MediaServerBook, Option<SeriesBook>)> {
        // Kotlin: providerBooks.groupBy { it.edition }（edition 恒为 null，键集合仅含 None）
        let mut edition_books: HashMap<Option<String>, Vec<SeriesBook>> = HashMap::new();
        for book in provider_books {
            edition_books.entry(book.edition.clone()).or_default().push(book.clone());
        }

        if let Some(edition) = edition {
            let edition_name = edition.replace(EDITION_REGEX, "").to_lowercase();
            return books
                .iter()
                .map(|book| {
                    let book_number = self.get_book_number(&book.name);
                    let matched = edition_books
                        .get(&Some(edition_name.clone()))
                        .and_then(|candidates| {
                            candidates
                                .iter()
                                .find(|pb| pb.number.is_some() && pb.number.as_ref() == book_number.as_ref())
                        })
                        .cloned();
                    (book.clone(), matched)
                })
                .collect();
        }

        if books.len() == 1 && provider_books.len() == 1 {
            let book = &books[0];
            let chapter_number = BookNameParser::get_chapters(&book.name);
            return if chapter_number.is_none() {
                vec![(book.clone(), Some(provider_books[0].clone()))]
            } else {
                vec![(book.clone(), None)]
            };
        }

        books
            .iter()
            .map(|book| {
                let book_extra_data: Vec<String> = BookNameParser::get_extra_data(&book.name)
                    .into_iter()
                    .map(|s| s.to_lowercase())
                    .collect();
                // Kotlin: editionBooks.keys.firstOrNull { bookExtraData.contains(it) }
                //（edition key 恒为 null → contains(null) == false → edition = null）
                let edition_key: Option<String> = edition_books
                    .keys()
                    .find(|k| {
                        k.as_deref()
                            .is_some_and(|ed| book_extra_data.iter().any(|extra| extra == ed))
                    })
                    .cloned()
                    .flatten();
                let book_number = self.get_book_number(&book.name);
                let matched = edition_books
                    .get(&edition_key)
                    .and_then(|candidates| {
                        candidates
                            .iter()
                            .find(|pb| pb.number.is_some() && pb.number.as_ref() == book_number.as_ref())
                    })
                    .cloned();
                (book.clone(), matched)
            })
            .collect()
    }

    fn get_book_number(&self, book_name: &str) -> Option<BookRange> {
        match self.library_type {
            MediaType::Manga => BookNameParser::get_volumes(book_name),
            MediaType::Novel | MediaType::Comic => BookNameParser::get_book_number(book_name),
            MediaType::Webtoon => BookNameParser::get_chapters(book_name).or_else(|| BookNameParser::get_book_number(book_name)),
        }
    }

    async fn aggregate_metadata_from_providers(
        &self,
        series: &MediaServerSeries,
        books: &[MediaServerBook],
        series_metadata: komf_core::model::SeriesMetadata,
        book_metadata: HashMap<MediaServerBookId, Option<BookMetadata>>,
        providers: Vec<Arc<dyn MetadataProvider>>,
        edition: Option<&str>,
        tx: &tokio::sync::broadcast::Sender<MetadataJobEvent>,
    ) -> SeriesAndBookMetadata {
        if providers.is_empty() {
            return SeriesAndBookMetadata::new(series_metadata, book_metadata).with_oneshots(books);
        }

        let search_titles: Vec<String> = series_metadata
            .titles
            .iter()
            .map(|t| t.name.clone())
            .filter(|t| !t.trim().is_empty())
            .collect();

        let mut current = SeriesAndBookMetadata::new(series_metadata, book_metadata).with_oneshots(books);
        for provider in providers {
            let matched = self
                .match_series(series, books, &search_titles, provider.clone(), edition, tx)
                .await;
            let _ = tx.send(MetadataJobEvent::ProviderCompleted { provider: provider.provider_name() });
            if let Some(new_metadata) = matched {
                current = self.merge_metadata(current, new_metadata);
            }
        }
        current
    }

    fn merge_metadata(
        &self,
        original: SeriesAndBookMetadata,
        new: SeriesAndBookMetadata,
    ) -> SeriesAndBookMetadata {
        let merged_series = self
            .metadata_merger
            .merge_series_metadata(&original.series_metadata, &new.series_metadata);

        let original_map: HashMap<String, Option<BookMetadata>> = original
            .book_metadata
            .iter()
            .map(|(id, m)| (id.0.clone(), m.clone()))
            .collect();
        let new_map: HashMap<String, Option<BookMetadata>> = new
            .book_metadata
            .iter()
            .map(|(id, m)| (id.0.clone(), m.clone()))
            .collect();
        let merged = self.metadata_merger.merge_book_metadata(&original_map, &new_map);

        let original_books: HashMap<String, MediaServerBookId> = original
            .book_metadata
            .keys()
            .map(|id| (id.0.clone(), id.clone()))
            .collect();
        let merged_books = merged
            .into_iter()
            .filter_map(|(id, m)| original_books.get(&id).map(|book_id| (book_id.clone(), m)))
            .collect();

        // Kotlin mergeMetadata 的 key 为 MediaServerBook（保留 original 的 oneshot 信息）。
        SeriesAndBookMetadata::new(merged_series, merged_books)
            .with_book_oneshots(original.book_oneshots)
    }

    /// 对应 `createMatchQuery`。第 4 字段 seriesFolder 对齐 Kotlin：传 series.url。
    async fn create_match_query(
        &self,
        search_title: &str,
        series: &MediaServerSeries,
        books: &[MediaServerBook],
    ) -> MatchQuery {
        // 搜索请求标题：searchTitleExtraction 启用时应用完整候选处理（符号归一 + 字符映射 + trim），
        // 使标题链中所有候选（含 series_title 原始名）都以处理后的形式调 provider 搜索 API。
        let query_series_name = normalized_search_title(search_title, &self.search_title_extraction);
        let mut sorted: Vec<&MediaServerBook> = books.iter().collect();
        sorted.sort_by_key(|b| b.number);
        let (first_book, range) = sorted
            .iter()
            .map(|book| {
                let number = BookNameParser::get_volumes(&book.name)
                    .unwrap_or_else(|| BookRange::single(book.number as f64));
                (book, number)
            })
            .min_by(|(_, a), (_, b)| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(book, number)| ((*book).clone(), number))
            .unwrap_or_else(|| {
                let book = books.first().cloned().unwrap_or_else(|| MediaServerBook {
                    id: MediaServerBookId(String::new()),
                    series_id: series.id.clone(),
                    library_id: None,
                    series_title: String::new(),
                    name: String::new(),
                    url: String::new(),
                    file_name: String::new(),
                    number: 0,
                    oneshot: false,
                    metadata: MediaServerBookMetadata {
                        title: String::new(),
                        summary: String::new(),
                        number: String::new(),
                        number_sort: None,
                        release_date: None,
                        authors: Vec::new(),
                        tags: Vec::new(),
                        isbn: None,
                        links: Vec::new(),
                        title_lock: false,
                        summary_lock: false,
                        number_lock: false,
                        number_sort_lock: false,
                        release_date_lock: false,
                        authors_lock: false,
                        tags_lock: false,
                        isbn_lock: false,
                        links_lock: false,
                    },
                    deleted: false,
                });
                (book, BookRange::single(0.0))
            });

        let cover = self.media_server_client.get_book_thumbnail(&first_book.id).await.unwrap_or(None);
        let release_year = series.metadata.release_year.filter(|y| *y != 0);

        MatchQuery::new(
            query_series_name,
            release_year,
            Some(BookQualifier {
                name: first_book.name.clone(),
                number: range,
                cover,
            }),
            // 对齐 Kotlin：seriesFolder = series.url
            Some(series.url.clone()),
        )
        // searchTitleExtraction 启用时：symbol_normalize_regex 也应用于 provider 匹配标题（两侧归一）
        .with_normalization(if self.search_title_extraction.enabled {
            Some(self.search_title_extraction.symbol_normalize_regex.clone())
        } else {
            None
        })
        // 库配置 mediaType 优先：Bangumi 匹配/搜索按库类型过滤 platform（Rust 扩展）
        .with_media_type(Some(self.library_type))
        // Rust 扩展：简繁转换应用于自动匹配（chineseConversion.matching；
        // normalized_series_name/normalize_titles 两侧双向应用）
        .with_chinese(if self.chinese_conversion.enabled && self.chinese_conversion.matching {
            self.chinese_converter.clone()
        } else {
            None
        })
        // Rust 扩展：oneshot 标志 + 第一本书文件名（eHentai gid 提取候选）
        .with_book_context(
            sorted.len() == 1 && first_book.oneshot,
            (!first_book.file_name.is_empty()).then(|| first_book.file_name.clone()),
        )
    }
}

/// Rust 扩展：按配置对系列元数据应用简繁转换（update.enabled + update.fields 过滤）。
/// 纯函数，便于单测。
fn apply_chinese_conversion_to_metadata(
    metadata: SeriesAndBookMetadata,
    cfg: &ChineseConversionConfig,
    converter: Option<&komf_core::util::ChineseConverter>,
) -> SeriesAndBookMetadata {
    if !cfg.enabled || !cfg.update.enabled {
        return metadata;
    }
    let Some(converter) = converter else {
        return metadata;
    };
    let SeriesAndBookMetadata {
        mut series_metadata,
        book_metadata,
        book_oneshots,
    } = metadata;
    for field in &cfg.update.fields {
        match field {
            ChineseField::Title => {
                if let Some(t) = &mut series_metadata.title {
                    t.name = converter.convert(&t.name);
                }
                for t in &mut series_metadata.titles {
                    t.name = converter.convert(&t.name);
                }
            }
            ChineseField::Genres => {
                series_metadata.genres = series_metadata
                    .genres
                    .iter()
                    .map(|g| converter.convert(g))
                    .collect();
            }
            ChineseField::Tags => {
                series_metadata.tags = series_metadata
                    .tags
                    .iter()
                    .map(|t| converter.convert(t))
                    .collect();
            }
            ChineseField::Summary => {
                if let Some(summary) = &mut series_metadata.summary {
                    *summary = converter.convert(summary);
                }
            }
        }
    }
    SeriesAndBookMetadata {
        series_metadata,
        book_metadata,
        book_oneshots,
    }
}

fn remove_parentheses(name: &str) -> String {
    let regex = regex::Regex::new(r"[(\[{]([^)\]}]+)[)\]}]").unwrap();
    regex.replace_all(name, "").trim().to_string()
}

/// 构造搜索标题链：series_title → 提取候选（若启用）→ remove_parentheses → 备选标题。
/// `remove_parentheses` 排在提取处理之后（用户既定）。
///
/// 去重键为**归一后**标题（enabled 时）：`series_title = "進撃の巨人：完全版"` 与提取候选
/// `"進撃の巨人 完全版"` 归一后相同 → 只保留第一个，避免对同一标题重复调搜索 API。
/// disabled 时归一为恒等 → 去重键即原始字符串（行为不变）。
fn build_search_titles(
    series_title: &str,
    alternative_titles: &[MediaServerAlternativeTitle],
    cfg: &SearchTitleExtractionConfig,
) -> Vec<String> {
    let mut search_titles: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    push_search_title(&mut search_titles, &mut seen, series_title, cfg);
    if cfg.enabled {
        for candidate in extract_series_titles(series_title, cfg) {
            push_search_title(&mut search_titles, &mut seen, &candidate, cfg);
        }
    }
    let no_parens_title = remove_parentheses(series_title);
    push_search_title(&mut search_titles, &mut seen, &no_parens_title, cfg);
    for alt in alternative_titles {
        push_search_title(&mut search_titles, &mut seen, &alt.title, cfg);
    }
    search_titles
}

/// 按归一后去重键压入搜索标题（保留原始串）；空标题直接跳过。
fn push_search_title(
    titles: &mut Vec<String>,
    seen: &mut Vec<String>,
    candidate: &str,
    cfg: &SearchTitleExtractionConfig,
) {
    if candidate.is_empty() {
        return;
    }
    let key = normalized_search_title(candidate, cfg);
    if seen.contains(&key) {
        return;
    }
    seen.push(key);
    titles.push(candidate.to_string());
}

/// 括号式搜索标题提取：从系列名提取额外搜索候选（可配置版）。
///
/// 行为约定：
/// 1. `bracket_regex` 提取全部括号段 parts；
/// 2. 第一个命中 `author_separator`（**正则**）的段为作者段：按分隔符拆成作者候选，标题取第一个非作者段；
///    无命中时：parts[0] 为标题，parts[1] 作作者（其余括号段丢弃）；
/// 3. 标题段按 `title_splitters`（**正则**，编译失败退回字面量）拆分，候选顺序 = 标题拆分段 → 作者段；
/// 4. 所有候选应用 `symbol_normalize_regex`（→空格）+ `char_mappings`（**正则键**，编译失败退回字面量）+ trim，去重保序；
/// 5. `cleanup_regex` 整名清洗链结果作为前置候选。
///
/// 除 `symbol_normalize_regex` 外均无预设正则；未配置的环节自然跳过（不产生候选）。
///
/// 注：候选内部的 `normalize_candidate` 与搜索层 `normalized_search_title` 的处理相同且幂等
/// （标点→空格、字符映射、trim 重复应用结果不变）；提取侧保留归一用于**空段丢弃与链内去重基准**，
/// 搜索侧统一归一保证 series_title/去括号/备选也以处理后的形式进入搜索 API。
fn extract_series_titles(name: &str, cfg: &SearchTitleExtractionConfig) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();

    // 5. 整名清洗（cleanupRegex 链）→ 前置候选
    if !cfg.cleanup_regex.is_empty() {
        let mut minimal = name.to_string();
        for pattern in &cfg.cleanup_regex {
            if let Ok(regex) = regex::Regex::new(pattern) {
                minimal = regex.replace_all(&minimal, "").to_string();
            }
        }
        let minimal = normalize_candidate(&minimal, cfg);
        if !minimal.is_empty() {
            candidates.push(minimal);
        }
    }

    // 1. 括号段提取
    let parts: Vec<String> = match &cfg.bracket_regex {
        Some(pattern) => match regex::Regex::new(pattern) {
            Ok(regex) => regex
                .captures_iter(name)
                .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
                .collect(),
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    };
    if parts.is_empty() {
        return candidates;
    }

    // 2. 作者段识别（第一个命中 author_separator 正则的段）
    let author_re = cfg
        .author_separator
        .as_deref()
        .and_then(|p| regex::Regex::new(p).ok());
    let author_candidate = author_re
        .as_ref()
        .and_then(|re| parts.iter().find(|p| re.is_match(p)));
    let mut title: Option<&str> = None;
    let mut authors: Vec<&str> = Vec::new();
    if let Some(author_part) = author_candidate {
        // 有作者段：标题 = 第一个非作者段；作者 = 作者段按分隔符拆开
        for p in &parts {
            if p != author_part {
                title = Some(p);
                break;
            }
        }
        if let Some(re) = &author_re {
            authors = re.split(author_part).collect();
        }
    } else {
        // 无作者段：parts[0] 标题，parts[1] 作作者（其余丢弃）
        title = parts.first().map(String::as_str);
        if parts.len() > 1 {
            authors.push(parts[1].as_str());
        }
    }

    // 3. 标题段按 title_splitters 拆分（支持正则，编译失败退回字面量）
    let mut title_segments: Vec<String> = Vec::new();
    if let Some(title) = title {
        title_segments = vec![title.to_string()];
        for splitter in &cfg.title_splitters {
            let mut next: Vec<String> = Vec::new();
            match regex::Regex::new(splitter) {
                Ok(re) => {
                    for segment in title_segments {
                        next.extend(re.split(&segment).map(str::to_string));
                    }
                }
                Err(_) => {
                    for segment in title_segments {
                        next.extend(segment.split(splitter).map(str::to_string));
                    }
                }
            }
            title_segments = next;
        }
    }

    // 4. 候选 = 标题拆分段 → 作者段，归一 + 去重保序
    for segment in title_segments.into_iter().chain(authors.into_iter().map(str::to_string)) {
        let candidate = normalize_candidate(&segment, cfg);
        if !candidate.is_empty() && !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }

    candidates
}

/// 候选归一：`symbol_normalize_regex` 匹配部分替换为空格，`char_mappings` 单字符替换，trim。
fn normalize_candidate(input: &str, cfg: &SearchTitleExtractionConfig) -> String {
    let mut out = input.to_string();
    if let Ok(regex) = regex::Regex::new(&cfg.symbol_normalize_regex) {
        out = regex.replace_all(&out, " ").to_string();
    }
    for (from, to) in &cfg.char_mappings {
        // 支持正则映射（全局替换），编译失败退回字面量替换（兼容原配置 "／" → "/"）
        match regex::Regex::new(from) {
            Ok(re) => out = re.replace_all(&out, to.as_str()).to_string(),
            Err(_) => out = out.replace(from.as_str(), to.as_str()),
        }
    }
    out.trim().to_string()
}

/// 搜索请求标题：`searchTitleExtraction` 启用时应用完整候选处理（符号归一 + 字符映射 + trim）；
/// 归一结果为空时回退原始名（避免空标题搜索）。禁用时原样返回。
fn normalized_search_title(name: &str, cfg: &SearchTitleExtractionConfig) -> String {
    if cfg.enabled {
        let normalized = normalize_candidate(name, cfg);
        if normalized.is_empty() {
            name.to_string()
        } else {
            normalized
        }
    } else {
        name.to_string()
    }
}

/// 按 library 区分服务的 provider —— 对应 `MetadataServiceProvider.kt`。
pub struct MetadataServiceProvider {
    default_metadata_service: Arc<MetadataService>,
    library_metadata_services: HashMap<String, Arc<MetadataService>>,
    default_update_service: Arc<MetadataUpdater>,
    library_updater_services: HashMap<String, Arc<MetadataUpdater>>,
}

impl MetadataServiceProvider {
    pub fn new(
        default_metadata_service: Arc<MetadataService>,
        library_metadata_services: HashMap<String, Arc<MetadataService>>,
        default_update_service: Arc<MetadataUpdater>,
        library_updater_services: HashMap<String, Arc<MetadataUpdater>>,
    ) -> Self {
        Self {
            default_metadata_service,
            library_metadata_services,
            default_update_service,
            library_updater_services,
        }
    }

    pub fn metadata_service_for(&self, library_id: &str) -> Arc<MetadataService> {
        self.library_metadata_services
            .get(library_id)
            .cloned()
            .unwrap_or_else(|| self.default_metadata_service.clone())
    }

    pub fn default_metadata_service(&self) -> Arc<MetadataService> {
        self.default_metadata_service.clone()
    }

    pub fn update_service_for(&self, library_id: &str) -> Arc<MetadataUpdater> {
        self.library_updater_services
            .get(library_id)
            .cloned()
            .unwrap_or_else(|| self.default_update_service.clone())
    }
}

/// 判断系列 links 是否已命中任一 provider 的识别特征（label / 域名）。
///
/// Rust 扩展（用户需求）：任一 provider 命中 → 整个系列不再自动匹配。
/// - 常规：label 与任一 provider 实际写入的 label 大小写不敏感相等
///   （Bangumi / e-hentai / YenPress / AniList / MyAnimeList / BookWalker / Webtoon /
///   Viz / ComicVine / MangaDex / MangaUpdates / MangaBaka 及其第三方来源 label）。
/// - Bangumi 兼容：label 或 url 含 btv / bgm.tv / bangumi.tv。
/// - EHentai 兼容：label 或 url 含 exhentai / e-hentai.org / exhentai.org。
/// oneshot 系列（books 中任一 oneshot=true）时，把书籍级 links 合并进系列级 links
/// （去重保序：label+url 均相同视为重复）。非 oneshot 系列仅返回系列级 links。
///
/// 书籍级 links 来源优先取 `SeriesDto.booksMetadata.links`；该字段为空/缺失时回退
/// `books`（由调用方 get_books 已获取，遍历内存数组，无额外请求）。
fn series_links_including_books(
    series: &MediaServerSeries,
    books: &[MediaServerBook],
) -> Vec<WebLink> {
    let mut out = series.metadata.links.clone();
    if books.iter().any(|b| b.oneshot) {
        let book_links: Vec<WebLink> = if !series.books_metadata_links.is_empty() {
            series.books_metadata_links.clone()
        } else {
            books
                .iter()
                .flat_map(|b| b.metadata.links.clone())
                .collect()
        };
        for link in book_links {
            if !out
                .iter()
                .any(|x| x.label == link.label && x.url == link.url)
            {
                out.push(link.clone());
            }
        }
    }
    out
}

fn links_indicate_matched(links: &[WebLink]) -> bool {
    const KNOWN_LABELS: [&str; 17] = [
        "bangumi", "e-hentai", "yenpress", "anilist", "myanimelist", "bookwalker", "webtoon",
        "viz", "comicvine", "mangadex", "mangaupdates", "mangabaka", "animenewsnetwork",
        "animeplanet", "anime-planet", "kitsu", "shikimori",
    ];
    links.iter().any(|link| {
        let label = link.label.to_lowercase();
        let url = link.url.to_lowercase();
        KNOWN_LABELS.iter().any(|known| label == *known)
            || label.contains("btv")
            || label.contains("bgm.tv")
            || label.contains("bangumi.tv")
            || label.contains("exhentai")
            || label.contains("e-hentai.org")
            || label.contains("exhentai.org")
            || url.contains("bgm.tv")
            || url.contains("bangumi.tv")
            || url.contains("e-hentai.org")
            || url.contains("exhentai.org")
    })
}

/// 从系列 links 解析首个可映射到 komf provider 的链接 → (provider, providerSeriesId)。
/// 仅覆盖 URL 可可靠提取 ID 的 provider；无法解析（第三方 label 等）返回 None，调用方落入搜索。
/// 收集 links 中所有可解析的 provider 链接（保持 links 顺序；同 provider 去重保留首个）。
/// 返回空 Vec 表示无任何可用链接。
fn links_match_providers(links: &[WebLink]) -> Vec<(CoreProviders, ProviderSeriesId)> {
    let mut result: Vec<(CoreProviders, ProviderSeriesId)> = Vec::new();
    for link in links {
        let url = link.url.to_lowercase();
        let parsed: Option<(CoreProviders, ProviderSeriesId)> = if url.contains("bgm.tv") || url.contains("bangumi.tv")
        {
            // bangumi: bgm.tv / bangumi.tv /subject/{id}
            extract_path_segment(&url, "/subject/").map(|id| (CoreProviders::Bangumi, ProviderSeriesId(id)))
        } else if url.contains("e-hentai.org") || url.contains("exhentai.org") {
            // ehentai: /g/{gid}/{token} → "{gid};{token}"
            extract_ehentai_gid(&url).map(|id| (CoreProviders::EHentai, ProviderSeriesId(id)))
        } else if url.contains("anilist.co") {
            // anilist: anilist.co/(anime|manga)/{id}
            extract_path_segment_any(&url, &["/anime/", "/manga/"])
                .map(|id| (CoreProviders::Anilist, ProviderSeriesId(id)))
        } else if url.contains("myanimelist.net") {
            // mal: myanimelist.net/(anime|manga)/{id}
            extract_path_segment_any(&url, &["/anime/", "/manga/"])
                .map(|id| (CoreProviders::Mal, ProviderSeriesId(id)))
        } else if url.contains("mangadex.org") {
            // mangadex: mangadex.org/title/{id}
            extract_path_segment(&url, "/title/").map(|id| (CoreProviders::Mangadex, ProviderSeriesId(id)))
        } else if url.contains("mangaupdates.com") {
            // mangaupdates: /series/{id} 或 series.html?id={id}
            extract_path_segment(&url, "/series/")
                .or_else(|| extract_query_param(&url, "id"))
                .map(|id| (CoreProviders::MangaUpdates, ProviderSeriesId(id)))
        } else {
            None
        };
        if let Some(p) = parsed {
            if !result.iter().any(|(prov, _)| *prov == p.0) {
                result.push(p);
            }
        }
    }
    result
}

/// 提取 url 中首个 prefix 后的路径段（到 / ? # 为止）。
fn extract_path_segment(url: &str, prefix: &str) -> Option<String> {
    let rest = url.split(prefix).nth(1)?;
    let seg = rest.split(['/', '?', '#']).next().unwrap_or("");
    if seg.is_empty() {
        None
    } else {
        Some(seg.to_string())
    }
}

fn extract_path_segment_any(url: &str, prefixes: &[&str]) -> Option<String> {
    prefixes.iter().find_map(|p| extract_path_segment(url, p))
}

/// ehentai 链接 /g/{gid}/{token} → "{gid};{token}"（ProviderSeriesId 格式）。
fn extract_ehentai_gid(url: &str) -> Option<String> {
    let rest = url.split("/g/").nth(1)?;
    let mut parts = rest.split('/');
    let gid = parts.next()?.to_string();
    let token = parts.next()?.to_string();
    if gid.is_empty() || token.is_empty() {
        None
    } else {
        Some(format!("{gid};{token}"))
    }
}

fn extract_query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split('?').nth(1)?;
    for pair in query.split('&') {
        let mut kv = pair.split('=');
        if kv.next()? == key {
            return kv.next().map(|v| v.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SearchTitleExtractionConfig {
        SearchTitleExtractionConfig::default()
    }

    fn alt(title: &str) -> MediaServerAlternativeTitle {
        MediaServerAlternativeTitle {
            title: title.to_string(),
            label: String::new(),
        }
    }

    fn enabled_cfg() -> SearchTitleExtractionConfig {
        SearchTitleExtractionConfig {
            enabled: true,
            bracket_regex: Some(r"\[([^\[\]]+)\]".to_string()),
            author_separator: Some("×".to_string()),
            title_splitters: vec!["_".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn disabled_keeps_original_behavior() {
        let titles = build_search_titles("名侦探柯南 (境外版)", &[alt("柯南")], &cfg());
        // enabled=false：无提取候选，remove_parentheses 保留，备选在后
        assert_eq!(titles, vec!["名侦探柯南 (境外版)", "名侦探柯南", "柯南"]);
    }

    #[test]
    fn author_bracket_split_into_candidates() {
        let c = enabled_cfg();
        let titles = extract_series_titles("[死亡笔记_另一个笔记] [大场鸫×小畑健]", &c);
        // `[大场鸫×小畑健]` 命中 × → 作者段，按 × 拆成作者候选；
        // 标题 = 第一个非作者段 "[死亡笔记_另一个笔记]"，按 _ 拆成两个标题候选。
        // 标题候选在前、作者候选在后（行为约定）。
        assert_eq!(titles, vec!["死亡笔记", "另一个笔记", "大场鸫", "小畑健"]);
    }

    #[test]
    fn no_author_single_bracket_part_is_title() {
        let c = enabled_cfg();
        // 无 ×、单括号段：parts[0] 为标题
        let titles = extract_series_titles("航海王 [One Piece]", &c);
        assert_eq!(titles, vec!["One Piece"]);
    }

    #[test]
    fn no_author_second_part_becomes_author() {
        let c = enabled_cfg();
        // 无 ×、双括号段：parts[0] 标题、parts[1] 作作者，两者都进候选
        let titles = extract_series_titles("[某作品] [某作者]", &c);
        assert_eq!(titles, vec!["某作品", "某作者"]);
    }

    #[test]
    fn extra_bracket_parts_dropped() {
        let c = enabled_cfg();
        // 无 ×：只取 parts[0] 标题 + parts[1] 作者，其余括号段丢弃
        let titles = extract_series_titles("[标题] [作者] [废话]", &c);
        assert_eq!(titles, vec!["标题", "作者"]);
    }

    #[test]
    fn cleanup_regex_prepended_candidate() {
        let c = SearchTitleExtractionConfig {
            enabled: true,
            cleanup_regex: vec![r"\[.*?\]".to_string(), r"\s*单行本$".to_string()],
            ..Default::default()
        };
        let titles = extract_series_titles("咒术回战 [官方] 单行本", &c);
        // 整名清洗链：去括号段 + 去行尾"单行本" → 前置候选
        assert_eq!(titles, vec!["咒术回战"]);
    }

    #[test]
    fn symbol_normalize_default_applied() {
        let c = enabled_cfg();
        let titles = extract_series_titles("进击的巨人：完全版", &c);
        // 默认 symbolNormalizeRegex `[:：…]` → 空格；无括号段、无拆分符 → 无候选
        assert!(titles.is_empty());
        // 有括号段时归一生效：
        let titles = extract_series_titles("电锯人：地狱篇 [电锯,人]", &c);
        assert_eq!(titles, vec!["电锯 人"]);
    }

    #[test]
    fn char_mappings_applied() {
        let c = SearchTitleExtractionConfig {
            enabled: true,
            char_mappings: vec![("／".to_string(), "/".to_string())],
            bracket_regex: Some(r"\[([^\[\]]+)\]".to_string()),
            ..Default::default()
        };
        let titles = extract_series_titles("死亡笔记／另一个笔记 [L／夜神]", &c);
        assert_eq!(titles, vec!["L/夜神"]);
    }

    #[test]
    fn remove_parentheses_after_extracted_candidates() {
        let c = enabled_cfg();
        // 顺序断言：series_title → 提取候选 → remove_parentheses → 备选
        let titles = build_search_titles("电锯人 (Part 2) [电锯,人]", &[alt("链锯人")], &c);
        assert_eq!(
            titles,
            vec!["电锯人 (Part 2) [电锯,人]", "电锯 人", "电锯人", "链锯人"]
        );
    }

    #[test]
    fn empty_and_duplicate_candidates_dropped() {
        let c = SearchTitleExtractionConfig {
            enabled: true,
            bracket_regex: Some(r"\[([^\[\]]+)\]".to_string()),
            title_splitters: vec!["_".to_string()],
            ..Default::default()
        };
        // 括号段 "_" 按 _ 拆分为空段 → 无候选
        let titles = extract_series_titles("(无关内容) [_]", &c);
        assert_eq!(titles, Vec::<String>::new());
    }

    #[test]
    fn no_default_regexes_except_symbol_normalize() {
        // 未配置任何可选正则 → 仅 symbolNormalizeRegex 有默认值
        let c = cfg();
        assert_eq!(c.symbol_normalize_regex, "[:：•·․,，。'’?？!！~⁓～]");
        assert!(c.bracket_regex.is_none());
        assert!(c.author_separator.is_none());
        assert!(c.title_splitters.is_empty());
        assert!(c.char_mappings.is_empty());
        assert!(c.cleanup_regex.is_empty());
        assert!(!c.enabled);
    }

    #[test]
    fn title_splitters_support_regex() {
        let c = SearchTitleExtractionConfig {
            enabled: true,
            bracket_regex: Some(r"\[([^\[\]]+)\]".to_string()),
            title_splitters: vec![r"[-_]".to_string()],
            ..Default::default()
        };
        // 正则拆分符：`-` 和 `_` 都拆
        let titles = extract_series_titles("[A-B_C]", &c);
        assert_eq!(titles, vec!["A", "B", "C"]);
    }

    #[test]
    fn char_mappings_support_regex() {
        let c = SearchTitleExtractionConfig {
            enabled: true,
            char_mappings: vec![(r"\s+".to_string(), " ".to_string())],
            bracket_regex: Some(r"\[([^\[\]]+)\]".to_string()),
            ..Default::default()
        };
        // 正则映射：连续空白压缩为单个空格
        let titles = extract_series_titles("[L  /  夜神]", &c);
        assert_eq!(titles, vec!["L / 夜神"]);
    }

    #[test]
    fn invalid_regex_falls_back_to_literal() {
        let c = SearchTitleExtractionConfig {
            enabled: true,
            bracket_regex: Some(r"\[([^\[\]]+)\]".to_string()),
            title_splitters: vec![r"(".to_string()], // 非法正则 → 退回字面量
            char_mappings: vec![(r"[unclosed".to_string(), "X".to_string())],
            ..Default::default()
        };
        // 非法正则不 panic：title_splitters 按字面量 "(" 拆分，char_mappings 按字面量替换
        let titles = extract_series_titles("[A(B]", &c);
        assert_eq!(titles, vec!["A", "B"]);
    }

    #[test]
    fn search_title_normalized_when_enabled() {
        let c = SearchTitleExtractionConfig {
            enabled: true,
            ..Default::default()
        };
        // 启用：完整候选处理（符号归一 + trim）
        assert_eq!(normalized_search_title("進撃の巨人：完全版", &c), "進撃の巨人 完全版");
        // charMappings 也生效
        let c2 = SearchTitleExtractionConfig {
            enabled: true,
            char_mappings: vec![("／".to_string(), "/".to_string())],
            ..Default::default()
        };
        assert_eq!(normalized_search_title("死亡笔记／另一个笔记", &c2), "死亡笔记/另一个笔记");
    }

    #[test]
    fn search_title_unchanged_when_disabled() {
        let c = cfg();
        assert_eq!(normalized_search_title("進撃の巨人：完全版", &c), "進撃の巨人：完全版");
    }

    #[test]
    fn search_title_falls_back_when_normalized_empty() {
        let c = SearchTitleExtractionConfig {
            enabled: true,
            symbol_normalize_regex: ".".to_string(), // 匹配所有字符 → 归一后为空
            ..Default::default()
        };
        assert_eq!(normalized_search_title("進撃の巨人", &c), "進撃の巨人");
    }

    #[test]
    fn build_search_titles_dedupes_after_normalization() {
        let c = SearchTitleExtractionConfig {
            enabled: true,
            bracket_regex: Some(r"\[([^\[\]]+)\]".to_string()),
            title_splitters: vec!["_".to_string()],
            ..Default::default()
        };
        // 链内保留原始串；去重键用归一后（"："→空格）。series_title 与 no_parens 归一后不同（
        // 前者含 [完全版] 括号、后者不含），故都保留；提取候选 "完全版" 与二者归一后均不等。
        let titles = build_search_titles("進撃の巨人：完全版 [完全版]", &[alt("進撃の巨人")], &c);
        assert_eq!(titles, vec!["進撃の巨人：完全版 [完全版]", "完全版", "進撃の巨人：完全版", "進撃の巨人"]);
        // 断言去重后没有归一相等的重复项
        let keys: Vec<String> = titles.iter().map(|t| normalized_search_title(t, &c)).collect();
        let mut unique = keys.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(keys.len(), unique.len());
    }

    #[test]
    fn build_search_titles_disabled_keeps_raw_dedup() {
        let c = cfg();
        // disabled：归一恒等，去重键 = 原始串；重复备选也被去重
        let titles = build_search_titles("名侦探柯南", &[alt("柯南"), alt("柯南")], &c);
        assert_eq!(titles, vec!["名侦探柯南", "柯南"]);
    }

    fn link(label: &str, url: &str) -> WebLink {
        WebLink {
            label: label.to_string(),
            url: url.to_string(),
        }
    }

    #[test]
    fn oneshot_series_merges_book_links() {
        use crate::model::{
            MediaServerBook, MediaServerBookId, MediaServerBookMetadata, MediaServerLibraryId,
            MediaServerSeries, MediaServerSeriesId,
        };
        let series = MediaServerSeries {
            id: MediaServerSeriesId("s".into()),
            library_id: MediaServerLibraryId("l".into()),
            name: "oneshot".into(),
            books_count: 1,
            books_metadata_links: vec![link("Bangumi", "https://bgm.tv/subject/123")],
            metadata: MediaServerSeriesMetadata::default(),
            url: String::new(),
            deleted: false,
            oneshot: true,
        };
        let oneshot_book = MediaServerBook {
            id: MediaServerBookId("b".into()),
            series_id: MediaServerSeriesId("s".into()),
            library_id: Some(MediaServerLibraryId("l".into())),
            series_title: "oneshot".into(),
            name: "oneshot".into(),
            url: String::new(),
            file_name: String::new(),
            number: 1,
            oneshot: true,
            metadata: MediaServerBookMetadata {
                links: vec![link("e-hentai", "https://e-hentai.org/g/4177551/f277732e1c/")],
                ..Default::default()
            },
            deleted: false,
        };
        // ① oneshot + 聚合 links 优先：参与判定与收集（books 仅用于 oneshot 判定）
        let combined = series_links_including_books(&series, &[oneshot_book.clone()]);
        assert!(links_indicate_matched(&combined));
        let providers = links_match_providers(&combined);
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].0, CoreProviders::Bangumi);
        assert_eq!(providers[0].1 .0, "123");
        // 去重：系列级与聚合级同链接只保留一个
        let mut series_with_link = series.clone();
        series_with_link.metadata.links = vec![link("Bangumi", "https://bgm.tv/subject/123")];
        let combined2 = series_links_including_books(&series_with_link, &[oneshot_book.clone()]);
        assert_eq!(combined2.len(), 1);
        // ② 聚合缺失（旧 komga）→ 回退 books
        let mut old_komga = series.clone();
        old_komga.books_metadata_links = Vec::new();
        let combined3 = series_links_including_books(&old_komga, &[oneshot_book.clone()]);
        assert!(links_indicate_matched(&combined3));
        let providers3 = links_match_providers(&combined3);
        assert_eq!(providers3.len(), 1);
        assert_eq!(providers3[0].0, CoreProviders::EHentai);
        assert_eq!(providers3[0].1 .0, "4177551;f277732e1c");
        // ③ 非 oneshot（booksCount 无关）：books 均 oneshot=false → 不合并
        let mut normal_book = oneshot_book;
        normal_book.oneshot = false;
        let combined4 = series_links_including_books(&series, &[normal_book]);
        assert!(!links_indicate_matched(&combined4));
    }

    #[test]
    fn links_indicate_matched_matches_provider_labels() {
        assert!(links_indicate_matched(&[link("Bangumi", "https://bgm.tv/subject/3510")]));
        assert!(links_indicate_matched(&[link("e-hentai", "https://e-hentai.org/g/1/t")]));
        assert!(links_indicate_matched(&[link("MyAnimeList", "https://myanimelist.net/anime/1")]));
        assert!(links_indicate_matched(&[link("MangaDex", "https://mangadex.org/title/1")]));
        assert!(links_indicate_matched(&[link("Shikimori", "https://shikimori.one/manga/1")]));
        // MangaDex 第三方外链 label（Anime-Planet 带连字符，与 MangaBaka 的 AnimePlanet 并存）
        assert!(links_indicate_matched(&[link("Anime-Planet", "https://www.anime-planet.com/manga/1")]));
        assert!(links_indicate_matched(&[link("MangaDex", "https://mangadex.org/title/1")]));
        // 大小写不敏感
        assert!(links_indicate_matched(&[link("BANGUMI", "https://bgm.tv/subject/1")]));
        assert!(links_indicate_matched(&[link("e-hentai".to_uppercase().as_str(), "https://x.org/1")]));
    }

    #[test]
    fn chinese_conversion_applies_selected_fields() {
        // update.fields = [Title, Tags]：标题+标签转换，体裁/简介不动
        let cfg = ChineseConversionConfig {
            enabled: true,
            direction: komf_core::util::ChineseDirection::T2s,
            search: true,
            matching: true,
            update: crate::config::ChineseUpdateConfig {
                enabled: true,
                fields: vec![ChineseField::Title, ChineseField::Tags],
            },
        };
        let converter =
            komf_core::util::ChineseConverter::new(komf_core::util::ChineseDirection::T2s).unwrap();
        let mut meta = komf_core::model::SeriesMetadata {
            title: Some(komf_core::model::SeriesTitle {
                name: "愛される資格は過去に落としてきました".to_string(),
                r#type: None,
                language: None,
            }),
            titles: vec![komf_core::model::SeriesTitle {
                name: "繁體別名".to_string(),
                r#type: None,
                language: None,
            }],
            genres: vec!["漫畫".to_string()],
            tags: vec!["漢化".to_string()],
            summary: None,
            ..Default::default()
        };
        meta.summary = Some("繁體簡介".to_string());
        let input = SeriesAndBookMetadata::new(meta, std::collections::HashMap::new());
        let out = apply_chinese_conversion_to_metadata(input, &cfg, Some(&converter));
        // 标题转简
        assert_eq!(out.series_metadata.title.unwrap().name, "爱される资格は过去に落としてきました");
        assert_eq!(out.series_metadata.titles[0].name, "繁体别名");
        // 标签转简
        assert_eq!(out.series_metadata.tags, vec!["汉化"]);
        // 未选中的字段不动
        assert_eq!(out.series_metadata.genres, vec!["漫畫"]);
        assert_eq!(out.series_metadata.summary.as_deref(), Some("繁體簡介"));
    }

    #[test]
    fn chinese_conversion_disabled_noop() {
        let cfg = ChineseConversionConfig::default(); // enabled = false
        let converter =
            komf_core::util::ChineseConverter::new(komf_core::util::ChineseDirection::T2s).unwrap();
        let mut meta = komf_core::model::SeriesMetadata {
            title: Some(komf_core::model::SeriesTitle {
                name: "繁體中文".to_string(),
                r#type: None,
                language: None,
            }),
            ..Default::default()
        };
        meta.genres = vec!["漫畫".to_string()];
        let input = SeriesAndBookMetadata::new(meta, std::collections::HashMap::new());
        let out = apply_chinese_conversion_to_metadata(input, &cfg, Some(&converter));
        assert_eq!(out.series_metadata.title.unwrap().name, "繁體中文");
        assert_eq!(out.series_metadata.genres, vec!["漫畫"]);
    }

    #[test]
    fn links_indicate_matched_domain_compat() {
        // bangumi 兼容 bgm.tv / bangumi.tv（label 或 url）
        assert!(links_indicate_matched(&[link("btv", "https://btv/subject/1")]));
        assert!(links_indicate_matched(&[link("bgm.tv", "https://bgm.tv/subject/1")]));
        assert!(links_indicate_matched(&[link("bangumi.tv", "https://x.example/subject/1")]));
        assert!(links_indicate_matched(&[link("other", "https://bgm.tv/subject/1")]));
        // ehentai 兼容 e-hentai.org / exhentai.org（label 或 url）
        assert!(links_indicate_matched(&[link("e-hentai.org", "https://x.example/g/1/t")]));
        assert!(links_indicate_matched(&[link("exhentai.org", "https://x.example/g/1/t")]));
        assert!(links_indicate_matched(&[link("other", "https://exhentai.org/g/1/t")]));
    }

    #[test]
    fn links_indicate_matched_no_match() {
        assert!(!links_indicate_matched(&[]));
        assert!(!links_indicate_matched(&[link("Random", "https://example.com/x")]));
        assert!(!links_indicate_matched(&[link("Wiki", "https://en.wikipedia.org/bgm")]));
        assert!(!links_indicate_matched(&[link("Pixiv", "https://www.pixiv.net/artworks/1")]));
    }

    #[test]
    fn links_match_providers_parses_urls() {
        // 单个可解析链接
        assert_eq!(
            links_match_providers(&[link("Bangumi", "https://bgm.tv/subject/1902")]),
            vec![(CoreProviders::Bangumi, ProviderSeriesId("1902".to_string()))]
        );
        assert_eq!(
            links_match_providers(&[link("other", "https://bangumi.tv/subject/123")]),
            vec![(CoreProviders::Bangumi, ProviderSeriesId("123".to_string()))]
        );
        assert_eq!(
            links_match_providers(&[link("e-hentai", "https://e-hentai.org/g/4194822/bacf336cff")]),
            vec![(CoreProviders::EHentai, ProviderSeriesId("4194822;bacf336cff".to_string()))]
        );
        assert_eq!(
            links_match_providers(&[link("exhentai.org", "https://exhentai.org/g/1/t")]),
            vec![(CoreProviders::EHentai, ProviderSeriesId("1;t".to_string()))]
        );
        assert_eq!(
            links_match_providers(&[link("AniList", "https://anilist.co/manga/123")]),
            vec![(CoreProviders::Anilist, ProviderSeriesId("123".to_string()))]
        );
        assert_eq!(
            links_match_providers(&[link("MyAnimeList", "https://myanimelist.net/anime/456")]),
            vec![(CoreProviders::Mal, ProviderSeriesId("456".to_string()))]
        );
        assert_eq!(
            links_match_providers(&[link("MangaDex", "https://mangadex.org/title/abc-def")]),
            vec![(CoreProviders::Mangadex, ProviderSeriesId("abc-def".to_string()))]
        );
        assert_eq!(
            links_match_providers(&[link("MangaUpdates", "https://www.mangaupdates.com/series/789")]),
            vec![(CoreProviders::MangaUpdates, ProviderSeriesId("789".to_string()))]
        );
        assert_eq!(
            links_match_providers(&[link("MangaUpdates", "https://www.mangaupdates.com/series.html?id=42")]),
            vec![(CoreProviders::MangaUpdates, ProviderSeriesId("42".to_string()))]
        );
        // 多链接：收集所有可解析的（跳过不可解析的第三方 label），保持 links 顺序
        assert_eq!(
            links_match_providers(&[
                link("Bangumi", "https://bgm.tv/subject/1902"),
                link("Kitsu", "https://kitsu.app/manga/1"),
                link("MangaDex", "https://mangadex.org/title/abc-def"),
            ]),
            vec![
                (CoreProviders::Bangumi, ProviderSeriesId("1902".to_string())),
                (CoreProviders::Mangadex, ProviderSeriesId("abc-def".to_string())),
            ]
        );
        // 同 provider 重复链接：去重保留首个
        assert_eq!(
            links_match_providers(&[
                link("Bangumi", "https://bgm.tv/subject/1902"),
                link("Bangumi alt", "https://bangumi.tv/subject/999"),
            ]),
            vec![(CoreProviders::Bangumi, ProviderSeriesId("1902".to_string()))]
        );
        // 无法映射到 komf provider 的第三方 label / 空 / 无关链接 → 空 Vec
        assert_eq!(links_match_providers(&[link("Kitsu", "https://kitsu.app/manga/1")]), vec![]);
        assert_eq!(links_match_providers(&[link("Shikimori", "https://shikimori.one/manga/1")]), vec![]);
        assert_eq!(links_match_providers(&[]), vec![]);
        assert_eq!(links_match_providers(&[link("Random", "https://example.com/x")]), vec![]);
    }
}
