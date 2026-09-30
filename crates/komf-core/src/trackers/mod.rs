//! Tracker 服务 —— 以已登录平台的 OAuth
//! token 调平台"用户列表" API，同步阅读状态/进度/评分。
//!
//! 统一模型（`TrackStatus` / `TrackUpdate` / `TrackState` /
//! `TrackSearchItem`），各平台适配器按官方 API 实现。
//! OAuth token 复用 [`crate::oauth::OAuthManager`]（登录、自动刷新、SQLite
//! 持久化均已就绪），本模块不重复实现授权流程。
//!
//! komf 无阅读器，"进度值"由调用方（WebUI / 第三方 API / 未来的媒体服务器
//! 联动任务）提供；`update` 只做透传推送。

pub mod anilist;
pub mod bangumi;
pub mod mal;
pub mod mangabaka;

use std::sync::Arc;

/// 统一阅读状态（7 态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackStatus {
    Reading,
    Planning,
    Completed,
    Paused,
    Dropped,
    Rereading,
}

/// 待推送的跟踪状态变更。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackUpdate {
    /// 评分。平台原生口径：AniList 0-100；MAL / Bangumi 0-10。
    pub score: Option<i32>,
    pub status: Option<TrackStatus>,
    /// 已读章节数（整数平台取 floor）。
    pub last_read_chapter: Option<f32>,
    /// 已读卷数。
    pub last_read_volume: Option<i32>,
    /// 开始阅读日期 `YYYY-MM-DD`。
    pub start_read_date: Option<String>,
    /// 完成阅读日期 `YYYY-MM-DD`。
    pub finish_read_date: Option<String>,
    /// 条目标题（非平台字段）：随 update 附带给本地"已关联"台账展示用。
    pub title: Option<String>,
    /// 封面 URL（非平台字段）：同上，供"已关联"台账展示封面。
    pub cover_url: Option<String>,
}

/// 平台上的当前跟踪状态。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackState {
    pub score: Option<i32>,
    pub status: Option<TrackStatus>,
    pub last_read_chapter: Option<f32>,
    pub last_read_volume: Option<i32>,
    pub total_chapters: Option<i32>,
    pub total_volumes: Option<i32>,
    pub start_read_date: Option<String>,
    pub finish_read_date: Option<String>,
}

/// 搜索候选。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackSearchItem {
    pub id: String,
    pub title: String,
    pub cover_url: Option<String>,
    pub description: Option<String>,
    /// 当前用户是否已把该条目加入列表。
    pub tracked: bool,
    pub url: Option<String>,
}

/// 解析条目链接，容忍无 scheme 的输入（如 `anilist.co/manga/87395`）。
pub(crate) fn parse_tracker_url(query: &str) -> Option<url::Url> {
    let input = query.trim();
    match url::Url::parse(input) {
        Ok(u) => Some(u),
        Err(_) => url::Url::parse(&format!("https://{input}")).ok(),
    }
}

/// 平台 tracker 的统一接口。
#[async_trait::async_trait]
pub trait TrackerService: Send + Sync {
    /// 平台标识（`anilist` / `mal` / `bangumi`）。
    fn provider(&self) -> &'static str;
    /// 是否已登录（有可用 OAuth token）。
    fn is_logged_in(&self) -> bool;
    /// 按标题搜索平台上的条目。`nsfw = true` 包含成人内容（平台允许时）。
    async fn search(&self, query: &str, nsfw: bool) -> Result<Vec<TrackSearchItem>, String>;
    /// 读取某条目在平台上的跟踪状态。
    async fn get_state(&self, track_id: &str) -> Result<TrackState, String>;
    /// 推送状态/进度/评分到平台。
    async fn update(&self, track_id: &str, update: &TrackUpdate) -> Result<(), String>;
}

/// 四个平台的 tracker 容器（app 层持有，供路由按 provider 分发）。
pub struct TrackerServices {
    anilist: Arc<dyn TrackerService>,
    mal: Arc<dyn TrackerService>,
    bangumi: Arc<dyn TrackerService>,
    mangabaka: Arc<dyn TrackerService>,
}

impl TrackerServices {
    pub fn new(
        http: reqwest::Client,
        oauth: Option<Arc<crate::oauth::OAuthManager>>,
        series_title_language: Option<String>,
    ) -> Self {
        Self {
            anilist: Arc::new(anilist::AniListTracker::new(http.clone(), oauth.clone())),
            mal: Arc::new(mal::MalTracker::new(http.clone(), oauth.clone())),
            bangumi: Arc::new(bangumi::BangumiTracker::new(http.clone(), oauth.clone())),
            mangabaka: Arc::new(mangabaka::MangaBakaTracker::new(
                http.clone(),
                oauth.clone(),
                series_title_language,
            )),
        }
    }

    pub fn get(&self, provider: &str) -> Option<Arc<dyn TrackerService>> {
        match provider {
            "anilist" => Some(self.anilist.clone()),
            "mal" => Some(self.mal.clone()),
            "bangumi" => Some(self.bangumi.clone()),
            "mangabaka" => Some(self.mangabaka.clone()),
            _ => None,
        }
    }

    pub fn all(&self) -> Vec<Arc<dyn TrackerService>> {
        vec![
            self.anilist.clone(),
            self.mal.clone(),
            self.bangumi.clone(),
            self.mangabaka.clone(),
        ]
    }
}
