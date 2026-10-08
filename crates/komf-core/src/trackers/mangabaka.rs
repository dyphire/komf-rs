//! MangaBaka tracker —— OIDC OAuth 用户收藏（library）实现。
//!
//! 库 API 均为 `/v1/my/...`（鉴权 Bearer）：
//! - 搜索 `GET /v1/series/search?q=...`（公开 API，`nsfw=false` 时追加
//!   `not_content_rating=erotica&not_content_rating=pornographic`）；
//! - 收藏批量查询 `GET /v1/my/library/batch?series_id=...`（tracked 标记，
//!   一次请求拿全部候选的收藏状态，避免逐条拉详情）；
//! - 单条状态 `GET /v1/my/library/{id}`（404 = 未收藏 → 空状态）；
//! - 更新 `PATCH`（已收藏）/ `POST`（未收藏）`/v1/my/library/{id}`，JSON body。
//!
//! 通用响应包装：`{ status, message, data, issues }`；错误时 data 为 null。

use super::{TrackSearchItem, TrackState, TrackStatus, TrackUpdate, TrackerService};
use crate::oauth::{OAuthManager, OAuthProvider};
use serde::Deserialize;
use std::sync::Arc;

const BASE_URL: &str = "https://api.mangabaka.org";

// ---------------------------------------------------------------------------
// API 模型（与 MangaBaka API 一致）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct SeriesDto {
    id: i64,
    title: String,
    #[serde(default)]
    titles: Option<Vec<SeriesTitleDto>>,
    #[serde(default)]
    cover: CoverDto,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    total_chapters: Option<String>,
    #[serde(default)]
    final_volume: Option<String>,
}

/// 多语言标题（与 provider `MangaBakaTitleDto` 同字段）。
#[derive(Debug, Clone, Deserialize)]
struct SeriesTitleDto {
    #[serde(default)]
    language: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    traits: Vec<String>,
    #[serde(default)]
    is_primary: Option<bool>,
}

impl SeriesDto {
    /// 连载中（releasing）时无可靠总量，返回 None。
    fn totals(&self) -> (Option<i32>, Option<i32>) {
        if self.status == "releasing" {
            (None, None)
        } else {
            (
                self.total_chapters.as_deref().and_then(|v| v.parse().ok()),
                self.final_volume.as_deref().and_then(|v| v.parse().ok()),
            )
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
struct CoverDto {
    #[serde(default)]
    raw: Option<CoverRawDto>,
    #[serde(default)]
    x350: Option<CoverDpiDto>,
}

#[derive(Debug, Clone, Deserialize)]
struct CoverRawDto {
    url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CoverDpiDto {
    x1: Option<String>,
}

/// 收藏条目（v1 library entry 的扁平 data 字段）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
struct LibraryEntry {
    #[serde(default)]
    rating: Option<f64>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    progress_chapter: Option<i64>,
    #[serde(default)]
    progress_volume: Option<i64>,
    #[serde(default)]
    start_date: Option<String>,
    #[serde(default)]
    finish_date: Option<String>,
}

// ---------------------------------------------------------------------------
// Tracker
// ---------------------------------------------------------------------------

pub struct MangaBakaTracker {
    http: reqwest::Client,
    oauth: Option<Arc<OAuthManager>>,
    /// 主标题语言偏好（postProcessing.seriesTitleLanguage）：仅搜索结果显示用，
    /// 与 provider `primary_title` 同一口径（不参与匹配）。
    series_title_language: Option<String>,
}

impl MangaBakaTracker {
    pub fn new(
        http: reqwest::Client,
        oauth: Option<Arc<OAuthManager>>,
        series_title_language: Option<String>,
    ) -> Self {
        Self {
            http,
            oauth,
            series_title_language,
        }
    }

    /// 源站判定 token 失效（HTTP 401，且强制刷新失败/重试仍 401）时清除该用户的登录态。
    fn note_unauthorized(&self, user_key: &str) {
        if let Some(manager) = &self.oauth {
            manager.logout(OAuthProvider::MangaBaka, user_key);
        }
    }

    async fn token(&self, user_key: &str) -> Result<String, String> {
        let Some(manager) = &self.oauth else {
            return Err("mangabaka: not logged in (oauth manager missing)".to_string());
        };
        manager
            .access_token(OAuthProvider::MangaBaka, user_key)
            .await
            .ok_or_else(|| "mangabaka: not logged in (no access token)".to_string())
    }

    /// 带 Bearer 的 GET，解析 JSON 值。遇 401 先强制刷新重试一次，刷新失败或
    /// 重试仍 401 才清登录态（四个 tracker 统一行为：401→强制刷新→重试一次→仍失败才登出）。
    async fn get_json(&self, user_key: &str, url: &str) -> Result<serde_json::Value, String> {
        let mut token = self.token(user_key).await?;
        let mut response = self
            .http
            .get(url)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| format!("mangabaka request failed: {e}"))?;
        let mut status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let refreshed = match &self.oauth {
                Some(manager) => {
                    manager
                        .refresh_now(OAuthProvider::MangaBaka, user_key)
                        .await
                }
                None => None,
            };
            if let Some(new_token) = refreshed {
                token = new_token;
                response = self
                    .http
                    .get(url)
                    .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
                    .send()
                    .await
                    .map_err(|e| format!("mangabaka request failed: {e}"))?;
                status = response.status();
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.note_unauthorized(user_key);
            }
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("mangabaka response parse failed: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "mangabaka API error (HTTP {status}): {}",
                body.get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
            ));
        }
        Ok(body)
    }

    /// 公开 API 的 GET（无需登录）。
    async fn get_public(&self, url: &str) -> Result<serde_json::Value, String> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("mangabaka request failed: {e}"))?;
        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("mangabaka response parse failed: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "mangabaka API error (HTTP {status}): {}",
                body.get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
            ));
        }
        Ok(body)
    }

    /// 解析 mangabaka.org/{id} 链接为条目 id（其余返回 None）。
    fn extract_series_url(query: &str) -> Option<i64> {
        let parsed = super::parse_tracker_url(query)?;
        let host = parsed.host_str()?.to_ascii_lowercase();
        if host != "mangabaka.org" && !host.ends_with(".mangabaka.org") {
            return None;
        }
        parsed.path_segments()?.next()?.parse::<i64>().ok()
    }

    /// 主标题选择 —— 与 provider `primary_title` 同一口径（仅显示，不参与匹配）：
    /// 完全匹配偏好语言（-Latn 归一为 -ro）→ 前缀匹配（排除 -Latn）→ native →
    /// en+primary → first；无 titles 数组时回落 API `title` 字段。
    fn primary_title(titles: &[SeriesTitleDto], preference: Option<&str>) -> String {
        if let Some(pref) = preference {
            if let Some(t) = titles
                .iter()
                .find(|t| t.language.replace("-Latn", "-ro") == pref)
            {
                return t.title.clone();
            }
            if let Some(t) = titles
                .iter()
                .find(|t| t.language.starts_with(pref) && !t.language.ends_with("-Latn"))
            {
                return t.title.clone();
            }
        }
        if let Some(native) = titles
            .iter()
            .find(|t| t.traits.iter().any(|x| x == "native"))
        {
            return native.title.clone();
        }
        if let Some(primary_en) = titles
            .iter()
            .find(|t| t.language == "en" && t.is_primary == Some(true))
        {
            return primary_en.title.clone();
        }
        titles.first().map(|t| t.title.clone()).unwrap_or_default()
    }

    /// 解析 SeriesDto → TrackSearchItem（标题尊重系列标题语言偏好）。
    fn series_to_item(&self, series: &SeriesDto, tracked: bool) -> TrackSearchItem {
        let title = match &series.titles {
            Some(titles) if !titles.is_empty() => {
                Self::primary_title(titles, self.series_title_language.as_deref())
            }
            _ => series.title.clone(),
        };
        TrackSearchItem {
            id: series.id.to_string(),
            title,
            cover_url: series
                .cover
                .x350
                .as_ref()
                .and_then(|d| d.x1.clone())
                .or_else(|| series.cover.raw.as_ref().and_then(|r| r.url.clone())),
            description: series.description.clone(),
            tracked,
            url: Some(format!("https://mangabaka.org/{}", series.id)),
        }
    }

    /// 批量查询收藏状态 → 已收藏的 series_id 集合。
    async fn tracked_ids(
        &self,
        user_key: &str,
        ids: &[i64],
    ) -> Result<std::collections::HashSet<i64>, String> {
        if ids.is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        let query = ids
            .iter()
            .map(|id| format!("series_id={id}"))
            .collect::<Vec<_>>()
            .join("&");
        let body = self
            .get_json(user_key, &format!("{BASE_URL}/v1/my/library/batch?{query}"))
            .await?;
        let data = body
            .get("data")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(data
            .iter()
            .filter_map(|e| e.get("series_id").and_then(serde_json::Value::as_i64))
            .collect())
    }

    fn to_status_str(status: TrackStatus) -> &'static str {
        match status {
            TrackStatus::Reading => "reading",
            TrackStatus::Planning => "plan_to_read",
            TrackStatus::Completed => "completed",
            TrackStatus::Paused => "paused",
            TrackStatus::Dropped => "dropped",
            TrackStatus::Rereading => "rereading",
        }
    }

    fn from_status_str(status: &str) -> Option<TrackStatus> {
        match status {
            "reading" => Some(TrackStatus::Reading),
            "considering" | "plan_to_read" => Some(TrackStatus::Planning),
            "completed" => Some(TrackStatus::Completed),
            "paused" => Some(TrackStatus::Paused),
            "dropped" => Some(TrackStatus::Dropped),
            "rereading" => Some(TrackStatus::Rereading),
            _ => None,
        }
    }
}

#[async_trait::async_trait]
impl TrackerService for MangaBakaTracker {
    fn provider(&self) -> &'static str {
        "mangabaka"
    }

    fn is_logged_in(&self, user_key: &str) -> bool {
        match &self.oauth {
            Some(manager) => manager.status(OAuthProvider::MangaBaka, user_key).logged_in,
            None => false,
        }
    }

    async fn search(
        &self,
        user_key: &str,
        query: &str,
        nsfw: bool,
    ) -> Result<Vec<TrackSearchItem>, String> {
        // 链接输入：mangabaka.org/{id} 直接定位单条目（tracked 单查）。
        if let Some(id) = Self::extract_series_url(query) {
            let body = self
                .get_public(&format!("{BASE_URL}/v1/series/{id}"))
                .await?;
            let series: SeriesDto = serde_json::from_value(
                body.get("data").cloned().unwrap_or(serde_json::Value::Null),
            )
            .map_err(|e| format!("mangabaka series parse failed: {e}"))?;
            let tracked = self
                .get_json(user_key, &format!("{BASE_URL}/v1/my/library/{id}"))
                .await
                .map(|b| b.get("data").map(|d| !d.is_null()).unwrap_or(false))
                .unwrap_or(false);
            return Ok(vec![self.series_to_item(&series, tracked)]);
        }

        let mut url = format!(
            "{BASE_URL}/v1/series/search?q={}",
            url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>()
        );
        if !nsfw {
            url.push_str("&not_content_rating=erotica&not_content_rating=pornographic");
        }
        let body = self.get_public(&url).await?;
        let series: Vec<SeriesDto> =
            serde_json::from_value(body.get("data").cloned().unwrap_or(serde_json::Value::Null))
                .map_err(|e| format!("mangabaka search parse failed: {e}"))?;
        let ids = series.iter().map(|s| s.id).collect::<Vec<_>>();
        let tracked = self.tracked_ids(user_key, &ids).await.unwrap_or_default();
        Ok(series
            .iter()
            .map(|s| self.series_to_item(s, tracked.contains(&s.id)))
            .collect())
    }

    async fn get_state(&self, user_key: &str, track_id: &str) -> Result<TrackState, String> {
        let id: i64 = track_id
            .parse()
            .map_err(|_| format!("invalid mangabaka id: {track_id}"))?;
        // 总数：公开 series 详情（total_chapters / final_volume；连载中无可靠总量）。
        let totals = self
            .get_public(&format!("{BASE_URL}/v1/series/{id}"))
            .await
            .ok()
            .and_then(|b| {
                let s: Option<SeriesDto> = serde_json::from_value(
                    b.get("data").cloned().unwrap_or(serde_json::Value::Null),
                )
                .ok();
                s.map(|s| s.totals())
            });
        let (total_chapters, total_volumes) = totals.unwrap_or((None, None));
        // 收藏状态：404 = 未收藏 → 空状态（总数仍返回）。
        let Ok(body) = self
            .get_json(user_key, &format!("{BASE_URL}/v1/my/library/{id}"))
            .await
        else {
            return Ok(TrackState {
                total_chapters,
                total_volumes,
                ..TrackState::default()
            });
        };
        let data = body.get("data").filter(|d| !d.is_null());
        let Some(data) = data else {
            return Ok(TrackState {
                total_chapters,
                total_volumes,
                ..TrackState::default()
            });
        };
        let entry: LibraryEntry = serde_json::from_value(data.clone())
            .map_err(|e| format!("mangabaka library entry parse failed: {e}"))?;
        Ok(TrackState {
            score: entry.rating.map(|v| v.round() as i32),
            status: entry.state.as_deref().and_then(Self::from_status_str),
            last_read_chapter: entry.progress_chapter.map(|v| v as f32),
            last_read_volume: entry.progress_volume.map(|v| v as i32),
            total_chapters,
            total_volumes,
            start_read_date: entry.start_date,
            finish_read_date: entry.finish_date,
        })
    }

    async fn update(
        &self,
        user_key: &str,
        track_id: &str,
        update: &TrackUpdate,
    ) -> Result<(), String> {
        let id: i64 = track_id
            .parse()
            .map_err(|_| format!("invalid mangabaka id: {track_id}"))?;
        let mut token = self.token(user_key).await?;

        let mut body: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
        if let Some(status) = update.status {
            body.insert(
                "state".into(),
                serde_json::Value::String(Self::to_status_str(status).into()),
            );
        }
        if let Some(score) = update.score {
            body.insert("rating".into(), serde_json::Value::Number(score.into()));
        }
        if let Some(chapter) = update.last_read_chapter {
            body.insert(
                "progress_chapter".into(),
                serde_json::Value::Number((chapter.floor() as i64).into()),
            );
        }
        if let Some(volume) = update.last_read_volume {
            body.insert(
                "progress_volume".into(),
                serde_json::Value::Number(volume.into()),
            );
        }
        // 哨兵日期（1969-12-31 / 1970-01-01）丢弃：源站不接受该值。
        if let Some(date) = &update.start_read_date {
            if date != "1969-12-31" && date != "1970-01-01" {
                body.insert("start_date".into(), serde_json::Value::String(date.clone()));
            }
        }
        if let Some(date) = &update.finish_read_date {
            if date != "1969-12-31" && date != "1970-01-01" {
                body.insert(
                    "finish_date".into(),
                    serde_json::Value::String(date.clone()),
                );
            }
        }
        if body.is_empty() {
            return Ok(());
        }

        // 已收藏 → PATCH；未收藏 → POST（create 语义）。
        let existing = self
            .get_json(user_key, &format!("{BASE_URL}/v1/my/library/{id}"))
            .await
            .map(|b| b.get("data").map(|d| !d.is_null()).unwrap_or(false))
            .unwrap_or(false);
        let method = if existing { "PATCH" } else { "POST" };

        let mut response = self
            .http
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
                format!("{BASE_URL}/v1/my/library/{id}"),
            )
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_string(&body).unwrap_or_default())
            .send()
            .await
            .map_err(|e| format!("mangabaka update failed: {e}"))?;
        let mut status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // 401：先强制刷新重试一次，失败才清登录态（与 get_json 一致）。
            let refreshed = match &self.oauth {
                Some(manager) => {
                    manager
                        .refresh_now(OAuthProvider::MangaBaka, user_key)
                        .await
                }
                None => None,
            };
            if let Some(new_token) = refreshed {
                token = new_token;
                response = self
                    .http
                    .request(
                        reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
                        format!("{BASE_URL}/v1/my/library/{id}"),
                    )
                    .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(serde_json::to_string(&body).unwrap_or_default())
                    .send()
                    .await
                    .map_err(|e| format!("mangabaka update failed: {e}"))?;
                status = response.status();
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.note_unauthorized(user_key);
            }
        }
        if !status.is_success() {
            let resp_body: serde_json::Value = response.json().await.unwrap_or_default();
            return Err(format!(
                "mangabaka update error (HTTP {status}): {}",
                resp_body
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
            ));
        }
        Ok(())
    }
}
