//! MangaBaka 管理 API DTO —— 对应 `snd.komf.api.mangabaka` 包。
//!
//! 全部为 `GET/POST/DELETE /api/mangabaka/*` 的请求/响应结构，字段命名与
//! Kotlin `KomfMangaBakaSeries` / `KomfMangaBakaTag` / Link/UnlinkRequest 一致
//! （camelCase，枚举序列化为大写名称原文）。
use super::common::KomfServerSeriesId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MangaBakaSeriesId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MangaBakaTagId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MangaBakaLinkId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MangaBakaRelationshipId(pub String);

/// `KomfMangaBakaLinkedSeries`：Komga 系列与其匹配的 MangaBaka 系列。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfMangaBakaLinkedSeries {
    pub komga_id: KomfServerSeriesId,
    pub manga_baka: KomfMangaBakaSeries,
}

/// `KomfMangaBakaSeries`：MangaBaka 系列完整详情（对应 Kotlin API DTO）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfMangaBakaSeries {
    pub id: MangaBakaSeriesId,
    pub has_anime: bool,
    #[serde(default)]
    pub anime: Option<MangaBakaAnimeInfo>,
    #[serde(default)]
    pub artists: Option<Vec<String>>,
    #[serde(default)]
    pub authors: Option<Vec<String>>,
    pub canonical_url: String,
    pub content_rating: MangaBakaContentRating,
    pub cover: MangaBakaCover,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub final_volume: Option<String>,
    pub is_licensed: bool,
    #[serde(default)]
    pub last_updated_at: Option<String>,
    #[serde(default)]
    pub merged_with: Option<i64>,
    #[serde(default)]
    pub original_language: Option<String>,
    #[serde(default)]
    pub publishers: Option<Vec<MangaBakaPublisher>>,
    #[serde(default)]
    pub rating: Option<f64>,
    pub state: MangaBakaSeriesState,
    pub status: MangaBakaStatus,
    #[serde(default)]
    pub total_chapters: Option<String>,
    #[serde(rename = "type")]
    pub r#type: MangaBakaType,
    #[serde(default)]
    pub links: Option<Vec<MangaBakaLink>>,
    #[serde(default)]
    pub published: Option<MangaBakaPublishedDate>,
    #[serde(default)]
    pub relationships: Option<Vec<MangaBakaRelationship>>,
    #[serde(default)]
    pub tags: Option<Vec<MangaBakaSeriesTag>>,
    #[serde(default)]
    pub titles: Option<Vec<MangaBakaTitle>>,
    pub source: MangaBakaSource,
}

/// `KomfMangaBakaTag`：标签目录条目（`GET /api/mangabaka/series/tags`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfMangaBakaTag {
    pub id: MangaBakaTagId,
    pub content_rating: MangaBakaContentRating,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_spoiler: Option<bool>,
    pub level: i32,
    pub name: String,
    pub name_path: String,
    #[serde(default)]
    pub parent_id: Option<MangaBakaTagId>,
    pub series_count: i32,
    pub is_genre: bool,
    #[serde(default)]
    pub merged_with: Option<i64>,
}

/// `KomfMangaBakaLinkRequest`：建立 Komga 系列与 MangaBaka 系列的关联。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfMangaBakaLinkRequest {
    pub komga_id: KomfServerSeriesId,
    pub manga_baka_series_id: MangaBakaSeriesId,
}

/// `KomfMangaBakaUnlinkRequest`：解除 Komga 系列与 MangaBaka 的关联。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfMangaBakaUnlinkRequest {
    pub komga_id: KomfServerSeriesId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MangaBakaAnimeInfo {
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaBakaCover {
    #[serde(default)]
    pub raw: Option<MangaBakaCoverRaw>,
    #[serde(default)]
    pub x150: Option<MangaBakaCoverDpi>,
    #[serde(default)]
    pub x250: Option<MangaBakaCoverDpi>,
    #[serde(default)]
    pub x350: Option<MangaBakaCoverDpi>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaBakaCoverRaw {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub height: Option<i32>,
    #[serde(default)]
    pub width: Option<i32>,
    #[serde(default)]
    pub blurhash: Option<String>,
    #[serde(default)]
    pub thumbhash: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaBakaCoverDpi {
    #[serde(default)]
    pub x1: Option<String>,
    #[serde(default)]
    pub x2: Option<String>,
    #[serde(default)]
    pub x3: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaBakaTitle {
    pub language: String,
    pub title: String,
    pub traits: Vec<MangaBakaTitleTrait>,
    #[serde(default)]
    pub is_primary: Option<bool>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaBakaLink {
    pub id: MangaBakaLinkId,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub name: String,
    pub name_display: String,
    pub r#type: MangaBakaLinkType,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaBakaPublisher {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// Original / English
    #[serde(rename = "type", default)]
    pub r#type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaBakaPublishedDate {
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub end_date_is_estimated: Option<bool>,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub start_date_is_estimated: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaBakaRelationship {
    pub id: MangaBakaRelationshipId,
    pub chronology: MangaBakaRelationshipChronology,
    pub is_manual: bool,
    #[serde(default)]
    pub note: Option<String>,
    pub relation_type: MangaBakaRelationType,
    pub to_series_id: MangaBakaSeriesId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaBakaSeriesTag {
    pub id: MangaBakaTagId,
    pub content_rating: MangaBakaContentRating,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_spoiler: Option<bool>,
    pub level: i32,
    pub name: String,
    pub name_path: String,
    #[serde(default)]
    pub parent_id: Option<MangaBakaTagId>,
    pub series_count: i32,
    #[serde(default)]
    pub implied_by_tag_ids: Vec<MangaBakaTagId>,
    pub is_explicit: bool,
    pub is_genre: bool,
    #[serde(default)]
    pub merged_with: Option<i64>,
    pub weight: MangaBakaTagWeight,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MangaBakaSource {
    pub anilist: MangaBakaAniListSource,
    pub anime_news_network: MangaBakaAnimeNewsNetworkSource,
    pub anime_planet: MangaBakaAnimePlanetSource,
    pub kitsu: MangaBakaKitsuSource,
    pub manga_updates: MangaBakaMangaUpdatesSource,
    pub my_anime_list: MangaBakaMyAnimeListSource,
    pub shikimori: MangaBakaShikimoriSource,
}

macro_rules! source_entry {
    ($(#[$attr:meta])* $name:ident, $id_ty:ty) => {
        $(#[$attr])*
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            #[serde(default)]
            pub id: Option<$id_ty>,
            #[serde(default)]
            pub rating: Option<f64>,
            #[serde(default)]
            pub rating_normalized: Option<i32>,
        }
    };
}

source_entry!(MangaBakaAniListSource, i64);
source_entry!(MangaBakaAnimeNewsNetworkSource, i64);
source_entry!(MangaBakaAnimePlanetSource, String);
source_entry!(MangaBakaKitsuSource, i64);
source_entry!(MangaBakaMangaUpdatesSource, String);
source_entry!(MangaBakaMyAnimeListSource, i64);
source_entry!(MangaBakaShikimoriSource, i64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaBakaStatus {
    Cancelled,
    Completed,
    Hiatus,
    Releasing,
    Upcoming,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaBakaType {
    Manga,
    Novel,
    Manhwa,
    Manhua,
    Oel,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaBakaSeriesState {
    Active,
    Merged,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaBakaContentRating {
    Safe,
    Suggestive,
    Erotica,
    Pornographic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaBakaTitleTrait {
    Official,
    Native,
    Alternative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaBakaLinkType {
    Publisher,
    Retailer,
    Webplatform,
    Info,
    Social,
    News,
    Piracy,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaBakaRelationType {
    Adaptation,
    Alternative,
    Cameo,
    CharacterFocus,
    Compilation,
    Contains,
    Crossover,
    Expansion,
    Main,
    Other,
    Parent,
    Parody,
    Prequel,
    Reboot,
    Remake,
    Sequel,
    Series,
    SideStory,
    Source,
    SpinOff,
    Summary,
    Uncollected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaBakaRelationshipChronology {
    Narrative,
    Release,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaBakaTagWeight {
    Core,
    Defining,
    Recurrent,
    Incidental,
    Unweighted,
}
