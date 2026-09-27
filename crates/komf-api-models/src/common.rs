//! 通用类型 —— 对应 `CommonTypes.kt`。
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum KomfAuthorRole {
    Writer,
    Penciller,
    Inker,
    Colorist,
    Letterer,
    Cover,
    Editor,
    Translator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum KomfMediaType {
    Manga,
    Novel,
    Comic,
    Webtoon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum KomfNameMatchingMode {
    Exact,
    ClosestMatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum KomfReadingDirection {
    LeftToRight,
    RightToLeft,
    Vertical,
    Webtoon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum KomfUpdateMode {
    Api,
    ComicInfo,
    /// Rust 扩展：更新后导出 mylar 格式 series.json（对应 UpdateMode::MylarSeriesJson）。
    MylarSeriesJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaServer {
    Komga,
    Kavita,
}

/// 对应 Kotlin 的 sealed interface `KomfProviders`。
///
/// 序列化时始终输出字符串；反序列化时未知值回退为 `Unknown`，
/// 与 Kotlin 的 `KomfProvidersSerializer` 行为一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KomfProviders {
    MangaBaka,
    BookWalker,
    Mangadex,
    MangaUpdates,
    Anilist,
    Mal,
    ComicVine,
    Bangumi,
    EHentai,
    YenPress,
    Viz,
    Webtoons,
    Nautiljon,
    Kodansha,
    Hentag,
    Unknown(String),
}

impl fmt::Display for KomfProviders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl KomfProviders {
    pub fn as_str(&self) -> &str {
        match self {
            KomfProviders::MangaBaka => "MANGA_BAKA",
            KomfProviders::BookWalker => "BOOK_WALKER",
            KomfProviders::Mangadex => "MANGADEX",
            KomfProviders::MangaUpdates => "MANGA_UPDATES",
            KomfProviders::Anilist => "ANILIST",
            KomfProviders::Mal => "MAL",
            KomfProviders::ComicVine => "COMIC_VINE",
            KomfProviders::Bangumi => "BANGUMI",
            KomfProviders::EHentai => "EHENTAI",
            KomfProviders::YenPress => "YEN_PRESS",
            KomfProviders::Viz => "VIZ",
            KomfProviders::Webtoons => "WEBTOONS",
            KomfProviders::Nautiljon => "NAUTILJON",
            KomfProviders::Kodansha => "KODANSHA",
            KomfProviders::Hentag => "HENTAG",
            KomfProviders::Unknown(name) => name,
        }
    }
}

impl Serialize for KomfProviders {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for KomfProviders {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(match name.as_str() {
            "MANGA_BAKA" => KomfProviders::MangaBaka,
            "BOOK_WALKER" => KomfProviders::BookWalker,
            "MANGADEX" => KomfProviders::Mangadex,
            "MANGA_UPDATES" => KomfProviders::MangaUpdates,
            "ANILIST" => KomfProviders::Anilist,
            "MAL" => KomfProviders::Mal,
            "COMIC_VINE" => KomfProviders::ComicVine,
            "BANGUMI" => KomfProviders::Bangumi,
            "EHENTAI" => KomfProviders::EHentai,
            "YEN_PRESS" => KomfProviders::YenPress,
            "VIZ" => KomfProviders::Viz,
            "WEBTOONS" => KomfProviders::Webtoons,
            "NAUTILJON" => KomfProviders::Nautiljon,
            "KODANSHA" => KomfProviders::Kodansha,
            "HENTAG" => KomfProviders::Hentag,
            _ => KomfProviders::Unknown(name),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaDexLink {
    MangaDex,
    Anilist,
    AnimePlanet,
    BookwalkerJp,
    MangaUpdates,
    NovelUpdates,
    Kitsu,
    Amazon,
    EbookJapan,
    MyAnimeList,
    CdJapan,
    Raw,
    EnglishTl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MangaBakaMode {
    Api,
    Database,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KomfServerSeriesId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KomfServerLibraryId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KomfProviderSeriesId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KomfErrorResponse {
    pub message: String,
}

/// 对应 `KomfPage<T>`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfPage<T> {
    pub content: T,
    pub total_pages: i32,
    pub current_page: i32,
}
