//! 元数据相关 API DTO —— 对应 `snd.komf.api.metadata` 包。
use super::common::{KomfMediaType, KomfProviderSeriesId, KomfProviders, KomfServerLibraryId, KomfServerSeriesId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfIdentifyRequest {
    pub library_id: Option<KomfServerLibraryId>,
    pub series_id: KomfServerSeriesId,
    pub provider: KomfProviders,
    pub provider_series_id: KomfProviderSeriesId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfMetadataJobResponse {
    pub id: KomfMetadataJobIdResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KomfMetadataJobIdResponse(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfMetadataSeriesSearchResult {
    pub url: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    pub title: String,
    pub provider: KomfProviders,
    pub result_id: KomfProviderSeriesId,
    /// 搜索结果显示用：provider 返回条目的媒体类型（Bangumi 按 platform 填充）。
    /// Rust 扩展：按库配置 libraryType 过滤显示结果。Kotlin 无此字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<KomfMediaType>,
    /// 搜索结果显示用：provider 返回条目的内容语言（BCP 47，如 zh/ja/en）。
    /// Rust 扩展：脚本展示/过滤用。Kotlin 无此字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}
