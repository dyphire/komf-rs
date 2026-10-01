//! Bangumi tracker —— bgm.tv v0 用户列表实现。
//! REST：搜索 `POST /v0/search/subjects`（type=1 书籍），状态
//! `GET /v0/me` → `GET /v0/users/{username}/collections/{id}`，更新
//! `POST /v0/users/-/collections/{id}`（"-" 表示当前用户）。

use super::{TrackSearchItem, TrackState, TrackStatus, TrackUpdate, TrackerService};
use crate::oauth::{OAuthManager, OAuthProvider};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

const API_BASE: &str = "https://api.bgm.tv/v0";

pub struct BangumiTracker {
    http: reqwest::Client,
    oauth: Option<Arc<OAuthManager>>,
}

impl BangumiTracker {
    pub fn new(http: reqwest::Client, oauth: Option<Arc<OAuthManager>>) -> Self {
        Self { http, oauth }
    }

    /// 源站判定 token 失效（HTTP 401，且强制刷新失败/重试仍 401）时清除登录态。
    fn note_unauthorized(&self) {
        if let Some(manager) = &self.oauth {
            manager.logout(OAuthProvider::Bangumi);
        }
    }

    async fn token(&self) -> Result<String, String> {
        let Some(manager) = &self.oauth else {
            return Err("bangumi: not logged in (oauth manager missing)".to_string());
        };
        manager
            .access_token(OAuthProvider::Bangumi)
            .await
            .ok_or_else(|| "bangumi: not logged in (no access token)".to_string())
    }

    /// 带 Bearer 发送请求；遇 401 先强制刷新重试一次，刷新失败或重试仍 401
    /// 才清登录态（四个 tracker 统一行为：401→强制刷新→重试一次→仍失败才登出）。
    async fn send_authed(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<Value>,
        label: &str,
    ) -> Result<reqwest::Response, String> {
        let mut token = self.token().await?;
        let build = |token: &str| {
            let mut request = self
                .http
                .request(method.clone(), url)
                .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
            if let Some(b) = &body {
                request = request.json(b);
            }
            request
        };
        let mut response = build(&token)
            .send()
            .await
            .map_err(|e| format!("{label} request failed: {e}"))?;
        let mut status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let refreshed = match &self.oauth {
                Some(manager) => manager.refresh_now(OAuthProvider::Bangumi).await,
                None => None,
            };
            if let Some(new_token) = refreshed {
                token = new_token;
                response = build(&token)
                    .send()
                    .await
                    .map_err(|e| format!("{label} request failed: {e}"))?;
                status = response.status();
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.note_unauthorized();
            }
        }
        Ok(response)
    }

    async fn get_json(&self, url: &str) -> Result<Value, String> {
        let response = self
            .send_authed(reqwest::Method::GET, url, None, "bangumi")
            .await?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("bangumi response parse failed: {e}"))?;
        if !status.is_success() {
            return Err(format!("bangumi API error (HTTP {status}): {body}"));
        }
        Ok(body)
    }

    fn to_type(status: TrackStatus) -> i64 {
        // Bangumi 收藏类型：1=wish 2=collect 3=do 4=on_hold 5=dropped
        match status {
            TrackStatus::Reading => 3,
            TrackStatus::Planning => 1,
            TrackStatus::Completed => 2,
            TrackStatus::Paused => 4,
            TrackStatus::Dropped => 5,
            TrackStatus::Rereading => 3,
        }
    }

    fn from_status_str(status: &str) -> Option<TrackStatus> {
        match status {
            "do" => Some(TrackStatus::Reading),
            "wish" => Some(TrackStatus::Planning),
            "collect" => Some(TrackStatus::Completed),
            "on_hold" => Some(TrackStatus::Paused),
            "dropped" => Some(TrackStatus::Dropped),
            _ => None,
        }
    }

    /// 解析 bgm.tv / bangumi.tv 条目链接为 subject id（其余返回 None）。
    fn extract_subject_url(query: &str) -> Option<i64> {
        let parsed = super::parse_tracker_url(query)?;
        let host = parsed.host_str()?.to_ascii_lowercase();
        if host != "bgm.tv" && host != "bangumi.tv" && !host.ends_with(".bgm.tv") {
            return None;
        }
        let mut segments = parsed.path_segments()?;
        if segments.next()? != "subject" {
            return None;
        }
        segments.next()?.parse::<i64>().ok()
    }

    /// 拉取当前用户全部书籍（subject_type=1）收藏 ID 集合，供搜索结果标记 tracked。
    /// 失败时返回空集（tracked 全部降级为 false，不阻塞搜索）。
    async fn tracked_subject_ids(&self, username: &str) -> HashSet<i64> {
        let mut ids = HashSet::new();
        let mut offset: i64 = 0;
        loop {
            let url = format!(
                "{API_BASE}/users/{username}/collections?subject_type=1&limit=100&offset={offset}"
            );
            let Ok(collection) = self.get_json(&url).await else {
                break;
            };
            let Some(data) = collection.get("data").and_then(Value::as_array) else {
                break;
            };
            if data.is_empty() {
                break;
            }
            for item in data {
                if let Some(sid) = item.get("subject_id").and_then(Value::as_i64) {
                    ids.insert(sid);
                }
            }
            let total = collection.get("total").and_then(Value::as_i64).unwrap_or(0);
            offset += 100;
            if total > 0 && offset >= total {
                break;
            }
        }
        ids
    }

    fn subject_to_item(subject: &Value, tracked: &HashSet<i64>) -> Option<TrackSearchItem> {
        let id = subject.get("id")?.as_i64()?;
        let name = subject.get("name").and_then(Value::as_str).unwrap_or("");
        let name_cn = subject.get("name_cn").and_then(Value::as_str).unwrap_or("");
        let title = if !name_cn.is_empty() { name_cn } else { name };
        Some(TrackSearchItem {
            id: id.to_string(),
            title: if title.is_empty() {
                "Unknown Title"
            } else {
                title
            }
            .to_string(),
            cover_url: subject
                .get("images")
                .and_then(|i| i.get("large"))
                .and_then(Value::as_str)
                .map(str::to_string),
            description: subject
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_string),
            // 用户态：命中当前用户书籍收藏即标记 tracked（未入列表为 false）。
            tracked: tracked.contains(&id),
            url: Some(format!("https://bgm.tv/subject/{id}")),
        })
    }

    /// v0 搜索：`POST /v0/search/subjects`（type=1 书籍）。
    async fn search_v0(&self, query: &str, nsfw: bool) -> Result<Vec<Value>, String> {
        let response = self
            .send_authed(
                reqwest::Method::POST,
                &format!("{API_BASE}/search/subjects?limit=20"),
                Some(json!({
                    "keyword": query,
                    "sort": "match",
                    "filter": { "type": [1], "nsfw": nsfw }
                })),
                "bangumi search",
            )
            .await?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("bangumi search response parse failed: {e}"))?;
        if !status.is_success() {
            return Err(format!("bangumi search error (HTTP {status}): {body}"));
        }
        Ok(body
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// 旧 API 兜底：`GET /search/subject/{query}?type=1`（公开，无鉴权）。
    async fn search_legacy(&self, query: &str) -> Result<Vec<Value>, String> {
        let encoded: String = query
            .as_bytes()
            .iter()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (*b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect();
        let response = self
            .http
            .get(format!(
                "https://api.bgm.tv/search/subject/{encoded}?type=1"
            ))
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| format!("bangumi legacy search request failed: {e}"))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("bangumi legacy search response parse failed: {e}"))?;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.note_unauthorized();
        }
        if !status.is_success() {
            return Err(format!(
                "bangumi legacy search error (HTTP {status}): {body}"
            ));
        }
        // 旧 API 响应字段为 list（v0 为 data），兼容两者。
        Ok(body
            .get("list")
            .or_else(|| body.get("subjects"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// 当前用户书籍收藏 ID 集合（内部共用；失败返回空集）。
    async fn current_tracked_ids(&self) -> HashSet<i64> {
        let mut tracked = HashSet::new();
        if let Ok(me) = self.get_json(&format!("{API_BASE}/me")).await {
            if let Some(username) = me.get("username").and_then(Value::as_str) {
                tracked = self.tracked_subject_ids(username).await;
            }
        }
        tracked
    }
}

#[async_trait::async_trait]
impl TrackerService for BangumiTracker {
    fn provider(&self) -> &'static str {
        "bangumi"
    }

    fn is_logged_in(&self) -> bool {
        match &self.oauth {
            Some(manager) => manager.status(OAuthProvider::Bangumi).logged_in,
            None => false,
        }
    }

    async fn search(&self, query: &str, nsfw: bool) -> Result<Vec<TrackSearchItem>, String> {
        // 链接输入：bgm.tv/bangumi.tv/subject/{id} 直接定位单条目。
        if let Some(id) = Self::extract_subject_url(query) {
            let tracked = self.current_tracked_ids().await;
            let subject = self.get_json(&format!("{API_BASE}/subjects/{id}")).await?;
            return Ok(Self::subject_to_item(&subject, &tracked)
                .into_iter()
                .collect());
        }
        // 用户态 tracked 标记：拉取当前用户书籍收藏集合（失败则降级为全 false）。
        let tracked = self.current_tracked_ids().await;
        // R18 条目仅在登录态可见（bgm.tv 对未登录请求忽略 NSFW 过滤；本路由已要求登录）。
        let subjects = match self.search_v0(query, nsfw).await {
            Ok(subjects) => subjects,
            Err(v0_err) => {
                // 旧 API 兜底：公开搜索接口（无鉴权），响应 { subjects: [...] }。
                let fallback = self.search_legacy(query).await.map_err(|legacy_err| {
                    format!("bangumi search failed (v0: {v0_err}; legacy: {legacy_err})")
                })?;
                fallback
            }
        };
        Ok(subjects
            .iter()
            .filter_map(|subject| Self::subject_to_item(subject, &tracked))
            .collect())
    }

    async fn get_state(&self, track_id: &str) -> Result<TrackState, String> {
        let id: i64 = track_id
            .parse()
            .map_err(|_| format!("invalid bangumi id: {track_id}"))?;
        // 先取当前用户 username（collection 查询要求指定用户名）。
        let me = self.get_json(&format!("{API_BASE}/me")).await?;
        let username = me
            .get("username")
            .and_then(Value::as_str)
            .ok_or_else(|| "bangumi /v0/me returned no username".to_string())?;
        let collection = match self
            .get_json(&format!("{API_BASE}/users/{username}/collections/{id}"))
            .await
        {
            Ok(v) => v,
            // 未收藏：该端点对未入列表条目返回 404（subject is not collected by user），
            // 与"已收藏但 type 缺失"一样按空状态处理，避免 WebUI 报 500。
            Err(e) if e.contains("HTTP 404") => return Ok(TrackState::default()),
            Err(e) => return Err(e),
        };
        if collection.get("type").is_none() {
            return Ok(TrackState::default()); // 未入列表
        }
        let subject = self
            .get_json(&format!("{API_BASE}/subjects/{id}"))
            .await
            .ok();
        let status_str = match collection.get("type").and_then(Value::as_i64) {
            Some(1) => "wish",
            Some(2) => "collect",
            Some(3) => "do",
            Some(4) => "on_hold",
            Some(5) => "dropped",
            _ => "",
        };
        Ok(TrackState {
            score: collection
                .get("rate")
                .and_then(Value::as_i64)
                .map(|v| v as i32),
            status: Self::from_status_str(status_str),
            last_read_chapter: collection
                .get("ep_status")
                .and_then(Value::as_i64)
                .map(|v| v as f32),
            last_read_volume: collection
                .get("vol_status")
                .and_then(Value::as_i64)
                .map(|v| v as i32),
            total_chapters: subject
                .as_ref()
                .and_then(|s| {
                    s.get("eps")
                        .and_then(Value::as_i64)
                        .or_else(|| s.get("total_episodes").and_then(Value::as_i64))
                })
                .map(|v| v as i32),
            total_volumes: subject
                .as_ref()
                .and_then(|s| s.get("volumes").and_then(Value::as_i64))
                .map(|v| v as i32),
            start_read_date: None, // Bangumi API 不返回收藏开始日期
            finish_read_date: None,
        })
    }

    async fn update(&self, track_id: &str, update: &TrackUpdate) -> Result<(), String> {
        let id: i64 = track_id
            .parse()
            .map_err(|_| format!("invalid bangumi id: {track_id}"))?;
        // 动态构建 body：未提供的字段省略（WebUI "keep" 语义），type 必须为整数。
        let mut body = serde_json::Map::new();
        if let Some(status) = update.status {
            body.insert("type".to_string(), json!(Self::to_type(status)));
        }
        if let Some(score) = update.score {
            body.insert("rate".to_string(), json!(score));
        }
        if let Some(chapter) = update.last_read_chapter {
            body.insert("ep_status".to_string(), json!(chapter.floor() as i64));
        }
        if let Some(volume) = update.last_read_volume {
            body.insert("vol_status".to_string(), json!(volume));
        }
        let response = self
            .send_authed(
                reqwest::Method::POST,
                &format!("{API_BASE}/users/-/collections/{id}"),
                Some(Value::Object(body)),
                "bangumi update",
            )
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body: Value = response.json().await.unwrap_or_default();
            return Err(format!("bangumi update error (HTTP {status}): {body}"));
        }
        Ok(())
    }
}
