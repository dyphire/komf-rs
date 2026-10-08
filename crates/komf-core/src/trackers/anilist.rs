//! AniList tracker —— GraphQL 用户列表实现。
//! GraphQL：搜索带 `mediaListEntry`（标记已入列表）、更新用 `SaveMediaListEntry`。

use super::{TrackSearchItem, TrackState, TrackStatus, TrackUpdate, TrackerService};
use crate::oauth::{OAuthManager, OAuthProvider};
use serde_json::{json, Value};
use std::sync::Arc;

const GRAPHQL_URL: &str = "https://graphql.anilist.co";

const SEARCH_QUERY_SFW: &str = r#"
query ($search: String) {
  Page(perPage: 20) {
    media(search: $search, type: MANGA, isAdult: false) {
      id
      title { userPreferred }
      description
      status
      format
      coverImage { medium }
      mediaListEntry { id }
    }
  }
}
"#;

// 无 isAdult 过滤：含成人内容。
const SEARCH_QUERY_ALL: &str = r#"
query ($search: String) {
  Page(perPage: 20) {
    media(search: $search, type: MANGA) {
      id
      title { userPreferred }
      description
      status
      format
      coverImage { medium }
      mediaListEntry { id }
    }
  }
}
"#;

const STATE_QUERY: &str = r#"
query ($id: Int) {
  Media(id: $id) {
    chapters
    volumes
    mediaListEntry {
      status
      score(format: POINT_100)
      progress
      progressVolumes
      startedAt { year month day }
      completedAt { year month day }
    }
  }
}
"#;

const UPDATE_MUTATION: &str = r#"
mutation (
  $id: Int, $status: MediaListStatus, $progress: Int, $volumes: Int,
  $score: Int, $startedAt: FuzzyDateInput, $completedAt: FuzzyDateInput
) {
  SaveMediaListEntry(
    mediaId: $id, status: $status, progress: $progress, progressVolumes: $volumes,
    scoreRaw: $score, startedAt: $startedAt, completedAt: $completedAt
  ) { id }
}
"#;

// 单条目查询（链接定位用，字段与搜索一致以便复用）。
const MEDIA_QUERY: &str = r#"
query ($id: Int) {
  Media(id: $id) {
    id
    title { userPreferred }
    description
    status
    format
    coverImage { medium }
    mediaListEntry { id }
  }
}
"#;

// 用户媒体列表评分格式（POINT_100 / POINT_10 / POINT_10_DECIMAL / POINT_5 / POINT_3）。
const VIEWER_QUERY: &str = r#"
query {
  Viewer { mediaListOptions { scoreFormat } }
}
"#;

pub struct AniListTracker {
    http: reqwest::Client,
    oauth: Option<Arc<OAuthManager>>,
    /// 用户媒体列表评分格式缓存（按 user_key 分键：不同用户的 scoreFormat
    /// 可能不同，如 POINT_10 与 POINT_100 混存；未登录/失败回退 POINT_100）。
    score_format: std::sync::RwLock<std::collections::HashMap<String, String>>,
}

impl AniListTracker {
    pub fn new(http: reqwest::Client, oauth: Option<Arc<OAuthManager>>) -> Self {
        Self {
            http,
            oauth,
            score_format: std::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// HTTP 401 或 GraphQL errors 含 Invalid token / Invalid Authentication
    /// 即视为 token 失效（AniList 对无效 token 可能返回 200 + errors）。
    fn is_unauthorized(status: reqwest::StatusCode, body: &Value) -> bool {
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return true;
        }
        body.get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| {
                errors
                    .iter()
                    .filter_map(|e| e.get("message").and_then(Value::as_str))
                    .any(|m| m.contains("Invalid token") || m.contains("Invalid Authentication"))
            })
    }

    async fn graphql(
        &self,
        user_key: &str,
        query: &str,
        variables: Value,
    ) -> Result<Value, String> {
        let mut token = match &self.oauth {
            Some(manager) => manager.access_token(OAuthProvider::Anilist, user_key).await,
            None => None,
        };
        let send = |token: Option<&str>| {
            let mut request = self.http.post(GRAPHQL_URL);
            if let Some(t) = token {
                request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {t}"));
            }
            request
                .json(&json!({ "query": query, "variables": variables }))
                .send()
        };
        let mut response = send(token.as_deref())
            .await
            .map_err(|e| format!("anilist request failed: {e}"))?;
        let mut status = response.status();
        let mut body: Value = response
            .json()
            .await
            .map_err(|e| format!("anilist response parse failed: {e}"))?;
        let mut unauthorized = Self::is_unauthorized(status, &body);
        if unauthorized {
            // token 被源站判无效：先强制刷新重试一次，失败才清登录态（四个 tracker 统一）。
            let refreshed = match &self.oauth {
                Some(manager) => manager.refresh_now(OAuthProvider::Anilist, user_key).await,
                None => None,
            };
            if let Some(new_token) = refreshed {
                token = Some(new_token);
                response = send(token.as_deref())
                    .await
                    .map_err(|e| format!("anilist request failed: {e}"))?;
                status = response.status();
                body = response
                    .json()
                    .await
                    .map_err(|e| format!("anilist response parse failed: {e}"))?;
                unauthorized = Self::is_unauthorized(status, &body);
            }
            if unauthorized {
                self.note_unauthorized(user_key);
            }
        }
        if let Some(errors) = body.get("errors").and_then(Value::as_array) {
            let message = errors
                .iter()
                .filter_map(|e| e.get("message").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(if message.is_empty() {
                format!("anilist GraphQL error (HTTP {status})")
            } else {
                message
            });
        }
        body.get("data")
            .cloned()
            .ok_or_else(|| format!("anilist returned no data (HTTP {status})"))
    }

    /// 源站判定 token 失效（HTTP 401 或 Invalid token 错误，且强制刷新失败/重试仍失效）
    /// 时清除该用户的登录态。
    fn note_unauthorized(&self, user_key: &str) {
        if let Some(manager) = &self.oauth {
            manager.logout(OAuthProvider::Anilist, user_key);
        }
    }

    /// tracker 请求的强制登录检查：graphql() 本身匿名降级（元数据用），
    /// 阅读状态同步必须拿到有效 token——失效（过期且无法刷新）时明确报错，
    /// 由路由层映射为 401，而不是静默降级为无用户态结果。
    async fn require_token(&self, user_key: &str) -> Result<String, String> {
        let Some(manager) = &self.oauth else {
            return Err("anilist: not logged in (oauth manager missing)".to_string());
        };
        manager
            .access_token(OAuthProvider::Anilist, user_key)
            .await
            .ok_or_else(|| "anilist: not logged in (no access token)".to_string())
    }

    fn date_input(date: Option<&str>) -> Option<Value> {
        date.and_then(|d| {
            let mut parts = d.split('-');
            let year = parts.next()?.parse::<i64>().ok()?;
            let month = parts.next()?.parse::<i64>().ok()?;
            let day = parts.next()?.parse::<i64>().ok()?;
            Some(json!({ "year": year, "month": month, "day": day }))
        })
    }

    fn date_output(value: Option<&Value>) -> Option<String> {
        let v = value?;
        let year = v.get("year")?.as_i64()?;
        if year == 0 {
            return None; // AniList 空日期哨兵（0-0-0）
        }
        let month = v.get("month")?.as_i64()?;
        let day = v.get("day")?.as_i64()?;
        Some(format!("{year:04}-{month:02}-{day:02}"))
    }

    fn to_status_str(status: TrackStatus) -> &'static str {
        match status {
            TrackStatus::Reading => "CURRENT",
            TrackStatus::Planning => "PLANNING",
            TrackStatus::Completed => "COMPLETED",
            TrackStatus::Paused => "PAUSED",
            TrackStatus::Dropped => "DROPPED",
            TrackStatus::Rereading => "REPEATING",
        }
    }

    fn from_status_str(status: &str) -> Option<TrackStatus> {
        match status {
            "CURRENT" => Some(TrackStatus::Reading),
            "PLANNING" => Some(TrackStatus::Planning),
            "COMPLETED" => Some(TrackStatus::Completed),
            "PAUSED" => Some(TrackStatus::Paused),
            "DROPPED" => Some(TrackStatus::Dropped),
            "REPEATING" => Some(TrackStatus::Rereading),
            _ => None,
        }
    }

    /// 读取指定用户的媒体列表评分格式（按用户缓存）。
    /// POINT_10 时读写需在 0-10 与 0-100（scoreRaw）间转换，其余格式直接使用 0-100。
    async fn store_score_format(&self, user_key: &str) -> String {
        if let Some(format) = self
            .score_format
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(user_key)
        {
            return format.clone();
        }
        let format = match self.graphql(user_key, VIEWER_QUERY, json!({})).await {
            Ok(data) => data
                .get("Viewer")
                .and_then(|v| v.get("mediaListOptions"))
                .and_then(|o| o.get("scoreFormat"))
                .and_then(Value::as_str)
                .unwrap_or("POINT_100")
                .to_string(),
            Err(_) => "POINT_100".to_string(),
        };
        self.score_format
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(user_key.to_string(), format.clone());
        format
    }

    /// 解析 anilist.co/manga/{id} 链接为条目 id（其余返回 None）。
    fn extract_manga_url(query: &str) -> Option<i64> {
        let parsed = super::parse_tracker_url(query)?;
        let host = parsed.host_str()?.to_ascii_lowercase();
        if host != "anilist.co" && !host.ends_with(".anilist.co") {
            return None;
        }
        let mut segments = parsed.path_segments()?;
        if segments.next()? != "manga" {
            return None;
        }
        segments.next()?.parse::<i64>().ok()
    }

    /// Media 节点 → TrackSearchItem（tracked 取 mediaListEntry 是否存在）。
    fn media_to_item(media: &Value) -> Option<TrackSearchItem> {
        let id = media.get("id")?.as_i64()?;
        let title = media
            .get("title")
            .and_then(|t| t.get("userPreferred"))
            .and_then(Value::as_str)
            .unwrap_or("Unknown Title")
            .to_string();
        Some(TrackSearchItem {
            id: id.to_string(),
            title,
            cover_url: media
                .get("coverImage")
                .and_then(|c| c.get("medium"))
                .and_then(Value::as_str)
                .map(str::to_string),
            description: media
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            tracked: media
                .get("mediaListEntry")
                .map(|e| !e.is_null())
                .unwrap_or(false),
            url: Some(format!("https://anilist.co/manga/{id}")),
        })
    }
}

#[async_trait::async_trait]
impl TrackerService for AniListTracker {
    fn provider(&self) -> &'static str {
        "anilist"
    }

    fn is_logged_in(&self, user_key: &str) -> bool {
        match &self.oauth {
            Some(manager) => manager.status(OAuthProvider::Anilist, user_key).logged_in,
            None => false,
        }
    }

    async fn search(
        &self,
        user_key: &str,
        query: &str,
        nsfw: bool,
    ) -> Result<Vec<TrackSearchItem>, String> {
        self.require_token(user_key).await?;
        // 链接输入：anilist.co/manga/{id} 直接定位单条目。
        if let Some(id) = Self::extract_manga_url(query) {
            let data = self
                .graphql(user_key, MEDIA_QUERY, json!({ "id": id }))
                .await?;
            let media = data
                .get("Media")
                .ok_or_else(|| "anilist media not found".to_string())?;
            return Ok(Self::media_to_item(media).into_iter().collect());
        }
        let (graphql_query, variables) = if nsfw {
            (SEARCH_QUERY_ALL, json!({ "search": query }))
        } else {
            (
                SEARCH_QUERY_SFW,
                json!({ "search": query, "isAdult": false }),
            )
        };
        let data = self.graphql(user_key, graphql_query, variables).await?;
        let media = data
            .get("Page")
            .and_then(|p| p.get("media"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(media
            .iter()
            .filter_map(|m| Self::media_to_item(m))
            .collect())
    }

    async fn get_state(&self, user_key: &str, track_id: &str) -> Result<TrackState, String> {
        self.require_token(user_key).await?;
        let id: i64 = track_id
            .parse()
            .map_err(|_| format!("invalid anilist id: {track_id}"))?;
        let data = self
            .graphql(user_key, STATE_QUERY, json!({ "id": id }))
            .await?;
        let media = data
            .get("Media")
            .ok_or_else(|| "anilist media not found".to_string())?;
        let entry = media.get("mediaListEntry");
        let Some(entry) = entry.filter(|e| !e.is_null()) else {
            return Ok(TrackState::default());
        };
        let score_format = self.store_score_format(user_key).await;
        let raw_score = entry.get("score").and_then(Value::as_i64).map(|v| v as i32);
        // POINT_10 用户习惯 0-10 分，raw（0-100）转换后返回；其余格式直接 0-100。
        let score = if score_format == "POINT_10" {
            raw_score.map(|v| v / 10)
        } else {
            raw_score
        };
        Ok(TrackState {
            score,
            status: entry
                .get("status")
                .and_then(Value::as_str)
                .and_then(Self::from_status_str),
            last_read_chapter: entry
                .get("progress")
                .and_then(Value::as_i64)
                .map(|v| v as f32),
            last_read_volume: entry
                .get("progressVolumes")
                .and_then(Value::as_i64)
                .map(|v| v as i32),
            total_chapters: media
                .get("chapters")
                .and_then(Value::as_i64)
                .map(|v| v as i32),
            total_volumes: media
                .get("volumes")
                .and_then(Value::as_i64)
                .map(|v| v as i32),
            start_read_date: Self::date_output(entry.get("startedAt")),
            finish_read_date: Self::date_output(entry.get("completedAt")),
        })
    }

    async fn update(
        &self,
        user_key: &str,
        track_id: &str,
        update: &TrackUpdate,
    ) -> Result<(), String> {
        self.require_token(user_key).await?;
        let id: i64 = track_id
            .parse()
            .map_err(|_| format!("invalid anilist id: {track_id}"))?;
        // POINT_10 用户输入 0-10，写入 scoreRaw 前放大为 0-100。
        let score = if let Some(score) = update.score {
            let score_format = self.store_score_format(user_key).await;
            Some(if score_format == "POINT_10" {
                score * 10
            } else {
                score
            })
        } else {
            None
        };
        let _ = self
            .graphql(
                user_key,
                UPDATE_MUTATION,
                json!({
                    "id": id,
                    "status": update.status.map(Self::to_status_str),
                    "progress": update.last_read_chapter.map(|c| c.floor() as i64),
                    "volumes": update.last_read_volume,
                    "score": score,
                    "startedAt": Self::date_input(update.start_read_date.as_deref()),
                    "completedAt": Self::date_input(update.finish_read_date.as_deref()),
                }),
            )
            .await?;
        Ok(())
    }
}
