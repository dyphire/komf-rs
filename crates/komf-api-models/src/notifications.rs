//! 通知相关 API DTO —— 对应 `snd.komf.api.notifications` 包。
use serde::{Deserialize, Serialize};

/// 对应 `KomfNotificationContext.kt` —— 模板渲染上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfNotificationContext {
    pub library: NotificationLibrary,
    pub series: NotificationSeries,
    pub books: Vec<NotificationBook>,
    pub media_server: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationLibrary {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationSeries {
    pub id: String,
    pub name: String,
    pub book_count: i32,
    pub metadata: NotificationSeriesMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationSeriesMetadata {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub title_sort: String,
    pub alternative_titles: Vec<NotificationAlternativeTitle>,
    #[serde(default)]
    pub summary: String,
    pub reading_direction: Option<String>,
    pub publisher: Option<String>,
    pub alternative_publishers: Vec<String>,
    pub age_rating: Option<i32>,
    pub language: Option<String>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub total_book_count: Option<i32>,
    pub authors: Vec<NotificationAuthor>,
    pub release_year: Option<i32>,
    pub links: Vec<NotificationLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationAlternativeTitle {
    #[serde(default)]
    pub label: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationBook {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub number: i32,
    pub metadata: NotificationBookMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationBookMetadata {
    #[serde(default)]
    pub title: String,
    pub summary: Option<String>,
    #[serde(default)]
    pub number: String,
    #[serde(default)]
    pub number_sort: Option<String>,
    pub release_date: Option<String>,
    pub authors: Vec<NotificationAuthor>,
    pub tags: Vec<String>,
    pub isbn: Option<String>,
    pub links: Vec<NotificationLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationAuthor {
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationLink {
    pub label: String,
    pub url: String,
}

/// 对应 `KomfDiscordRequest.kt` / `KomfAppriseRequest.kt`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfDiscordRequest {
    pub context: KomfNotificationContext,
    #[serde(default)]
    pub templates: Option<KomfDiscordTemplates>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfAppriseRequest {
    pub context: KomfNotificationContext,
    #[serde(default)]
    pub templates: Option<KomfAppriseTemplates>,
}

/// 对应 `KomfDiscordRenderResult.kt` / `KomfAppriseRenderResult.kt`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfDiscordRenderResult {
    pub title: Option<String>,
    pub title_url: Option<String>,
    pub description: Option<String>,
    pub footer: Option<String>,
    pub fields: Vec<KomfDiscordRenderField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfDiscordRenderField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfAppriseRenderResult {
    pub title: Option<String>,
    pub body: String,
}

/// 对应 `KomfDiscordTemplates.kt` / `KomfAppriseTemplates.kt`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfDiscordTemplates {
    pub title: Option<String>,
    pub title_url: Option<String>,
    pub description: Option<String>,
    pub footer: Option<String>,
    pub fields: Vec<KomfDiscordTemplateField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfDiscordTemplateField {
    pub name: Option<String>,
    pub value: Option<String>,
    pub inline: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfAppriseTemplates {
    pub title: Option<String>,
    pub body: Option<String>,
}
