//! HTTP 响应共享工具 —— 各媒体服务器客户端（komga/kavita/stump）共用的
//! 状态检查与图片下载样板。
//!
//! 对应 Kotlin 各 client 中重复出现的两段逻辑："非 2xx 读取响应体抛错"
//! （komga-client / KavitaClient / Stump 适配层的统一错误构造），以及
//! 封面/缩略图端点的 `Content-Type` + bytes → `Image` 构造
//! （`getThumbnail` 系列端点）。

use crate::client::MediaServerError;
use komf_core::model::Image;

/// 检查响应状态：2xx 返回原响应；否则读取 body 构造 `Status` 错误
/// （对齐 Kotlin 各 client 的非 2xx 处理：读取 error body 抛出携带
/// status 与 body 的异常）。
pub(crate) async fn ensure_success(
    response: reqwest::Response,
) -> Result<reqwest::Response, MediaServerError> {
    let status = response.status();
    if status.is_success() {
        Ok(response)
    } else {
        Err(MediaServerError::Status(
            status,
            response.text().await.unwrap_or_default(),
        ))
    }
}

/// 从已成功响应构造 `Image`：提取 `Content-Type` 作为 mime，读取全部 bytes。
/// 对应 Kotlin 封面/缩略图端点的 `Image(bytes, contentType)` 构造。
pub(crate) async fn image_from_response(
    response: reqwest::Response,
) -> Result<Image, MediaServerError> {
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = response.bytes().await?;
    Ok(Image::new(bytes.to_vec(), mime))
}
