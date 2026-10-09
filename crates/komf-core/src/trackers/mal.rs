//! MyAnimeList tracker —— REST v2 用户列表实现。
//! REST：搜索 `GET /manga?q`，状态 `GET /manga/{id}?fields=...`，更新
//! `PATCH /manga/{id}/my_list_status`（form-urlencoded）。

use super::{TrackMediaType, TrackSearchItem, TrackState, TrackStatus, TrackUpdate, TrackerService};
use crate::oauth::{OAuthManager, OAuthProvider};
use std::sync::Arc;

const BASE_URL: &str = "https://api.myanimelist.net/v2";

const SEARCH_FIELDS: &str =
    "id,title,synopsis,num_chapters,main_picture,status,media_type,my_list_status";

pub struct MalTracker {
    http: reqwest::Client,
    oauth: Option<Arc<OAuthManager>>,
}

impl MalTracker {
    pub fn new(http: reqwest::Client, oauth: Option<Arc<OAuthManager>>) -> Self {
        Self { http, oauth }
    }

    /// 源站判定 token 失效（HTTP 401，且强制刷新失败/重试仍 401）时清除该用户的登录态。
    fn note_unauthorized(&self, user_key: &str) {
        if let Some(manager) = &self.oauth {
            manager.logout(OAuthProvider::Mal, user_key);
        }
    }

    /// 带 Bearer 的 GET，解析为 JSON。遇 401 先强制刷新重试一次，刷新失败或
    /// 重试仍 401 才清登录态（四个 tracker 统一行为：401→强制刷新→重试一次→仍失败才登出）。
    async fn get_json(&self, user_key: &str, url: &str) -> Result<serde_json::Value, String> {
        let mut token = self.token(user_key).await?;
        let mut response = self
            .http
            .get(url)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| format!("mal request failed: {e}"))?;
        let mut status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let refreshed = match &self.oauth {
                Some(manager) => manager.refresh_now(OAuthProvider::Mal, user_key).await,
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
                    .map_err(|e| format!("mal request failed: {e}"))?;
                status = response.status();
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.note_unauthorized(user_key);
            }
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("mal response parse failed: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "mal API error (HTTP {status}): {}",
                body.get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
            ));
        }
        Ok(body)
    }

    async fn token(&self, user_key: &str) -> Result<String, String> {
        let Some(manager) = &self.oauth else {
            return Err("mal: not logged in (oauth manager missing)".to_string());
        };
        manager
            .access_token(OAuthProvider::Mal, user_key)
            .await
            .ok_or_else(|| "mal: not logged in (no access token)".to_string())
    }

    fn to_status_str(status: TrackStatus) -> &'static str {
        match status {
            TrackStatus::Reading => "reading",
            TrackStatus::Planning => "plan_to_read",
            TrackStatus::Completed => "completed",
            TrackStatus::Paused => "on_hold",
            TrackStatus::Dropped => "dropped",
            TrackStatus::Rereading => "reading", // MAL 无 rereading，映射为 reading
        }
    }

    fn from_status_str(status: &str) -> Option<TrackStatus> {
        match status {
            "reading" => Some(TrackStatus::Reading),
            "plan_to_read" => Some(TrackStatus::Planning),
            "completed" => Some(TrackStatus::Completed),
            "on_hold" => Some(TrackStatus::Paused),
            "dropped" => Some(TrackStatus::Dropped),
            _ => None,
        }
    }

    /// 解析 myanimelist.net/manga/{id} 链接为条目 id（其余返回 None）。
    fn extract_manga_url(query: &str) -> Option<i64> {
        let parsed = super::parse_tracker_url(query)?;
        let host = parsed.host_str()?.to_ascii_lowercase();
        if host != "myanimelist.net" && !host.ends_with(".myanimelist.net") {
            return None;
        }
        let mut segments = parsed.path_segments()?;
        if segments.next()? != "manga" {
            return None;
        }
        segments.next()?.parse::<i64>().ok()
    }

    /// 单条目详情 → TrackSearchItem（tracked 取 my_list_status 是否存在）。
    /// `media_type` 区分漫画与小说（复用 provider 侧 MAL media_type 口径：
    /// novel/light_novel → 小说，其余漫画类 → 漫画）。
    fn detail_to_item(id: i64, details: &serde_json::Value) -> Option<TrackSearchItem> {
        Some(TrackSearchItem {
            id: id.to_string(),
            title: details
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Unknown Title")
                .to_string(),
            cover_url: details
                .get("main_picture")
                .and_then(|p| p.get("large"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            description: details
                .get("synopsis")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            tracked: details
                .get("my_list_status")
                .map(|s| !s.is_null())
                .unwrap_or(false),
            media_type: media_type_of(details.get("media_type").and_then(serde_json::Value::as_str)),
            url: Some(format!("https://myanimelist.net/manga/{id}")),
        })
    }
}

/// MAL media_type 字符串 → 漫画/小说（manhwa/manhua 归漫画展示）。
fn media_type_of(media_type: Option<&str>) -> Option<TrackMediaType> {
    match media_type {
        Some("novel" | "light_novel") => Some(TrackMediaType::Novel),
        Some("manga" | "one_shot" | "doujinshi" | "manhua" | "manhwa" | "oel") => {
            Some(TrackMediaType::Manga)
        }
        _ => None,
    }
}

#[async_trait::async_trait]
impl TrackerService for MalTracker {
    fn provider(&self) -> &'static str {
        "mal"
    }

    fn is_logged_in(&self, user_key: &str) -> bool {
        match &self.oauth {
            Some(manager) => manager.status(OAuthProvider::Mal, user_key).logged_in,
            None => false,
        }
    }

    async fn search(
        &self,
        user_key: &str,
        query: &str,
        nsfw: bool,
    ) -> Result<Vec<TrackSearchItem>, String> {
        // 链接输入：myanimelist.net/manga/{id} 直接定位单条目。
        if let Some(id) = Self::extract_manga_url(query) {
            let details = self
                .get_json(
                    user_key,
                    &format!("{BASE_URL}/manga/{id}?fields={SEARCH_FIELDS}"),
                )
                .await?;
            return Ok(Self::detail_to_item(id, &details).into_iter().collect());
        }
        // MAL 搜索查询不能超过 64 字符。
        let q: String = query.chars().take(64).collect();
        let url = format!(
            "{BASE_URL}/manga?q={}&nsfw={nsfw}",
            url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>()
        );
        let body = self.get_json(user_key, &url).await?;
        let nodes = body
            .get("data")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut futs = Vec::with_capacity(nodes.len());
        for node in &nodes {
            let Some(entry) = node.get("node") else {
                continue;
            };
            let Some(id) = entry.get("id").and_then(serde_json::Value::as_i64) else {
                continue;
            };
            // 逐条并发拉详情以拿到 my_list_status（tracked 标记）。
            let entry = entry.clone();
            futs.push(async move {
                let details = self
                    .get_json(
                        user_key,
                        &format!("{BASE_URL}/manga/{id}?fields={SEARCH_FIELDS}"),
                    )
                    .await
                    .ok();
                (entry, id, details)
            });
        }
        let fetched = futures::future::join_all(futs).await;
        let mut results = Vec::with_capacity(fetched.len());
        for (entry, id, details) in fetched {
            let details = details.as_ref();
            results.push(TrackSearchItem {
                id: id.to_string(),
                title: details
                    .and_then(|d| d.get("title"))
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| entry.get("title").and_then(serde_json::Value::as_str))
                    .unwrap_or("Unknown Title")
                    .to_string(),
                cover_url: details
                    .and_then(|d| d.get("main_picture"))
                    .and_then(|p| p.get("large"))
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| {
                        entry
                            .get("main_picture")
                            .and_then(|p| p.get("large"))
                            .and_then(serde_json::Value::as_str)
                    })
                    .map(str::to_string),
                description: details
                    .and_then(|d| d.get("synopsis"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                tracked: details
                    .and_then(|d| d.get("my_list_status"))
                    .map(|s| !s.is_null())
                    .unwrap_or(false),
                media_type: media_type_of(
                    details
                        .and_then(|d| d.get("media_type"))
                        .and_then(serde_json::Value::as_str),
                ),
                url: Some(format!("https://myanimelist.net/manga/{id}")),
            });
        }
        Ok(results)
    }

    async fn get_state(&self, user_key: &str, track_id: &str) -> Result<TrackState, String> {
        let id: i64 = track_id
            .parse()
            .map_err(|_| format!("invalid mal id: {track_id}"))?;
        let body = self
            .get_json(
                user_key,
                &format!("{BASE_URL}/manga/{id}?fields=num_volumes,num_chapters,my_list_status"),
            )
            .await?;
        let status = body.get("my_list_status").filter(|s| !s.is_null());
        let Some(status) = status else {
            return Ok(TrackState::default());
        };
        Ok(TrackState {
            score: status
                .get("score")
                .and_then(serde_json::Value::as_i64)
                .map(|v| v as i32),
            status: status
                .get("status")
                .and_then(serde_json::Value::as_str)
                .and_then(Self::from_status_str),
            last_read_chapter: status
                .get("num_chapters_read")
                .and_then(serde_json::Value::as_i64)
                .map(|v| v as f32),
            last_read_volume: status
                .get("num_volumes_read")
                .and_then(serde_json::Value::as_i64)
                .map(|v| v as i32),
            total_chapters: body
                .get("num_chapters")
                .and_then(serde_json::Value::as_i64)
                .map(|v| v as i32),
            total_volumes: body
                .get("num_volumes")
                .and_then(serde_json::Value::as_i64)
                .map(|v| v as i32),
            start_read_date: status
                .get("start_date")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            finish_read_date: status
                .get("finish_date")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
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
            .map_err(|_| format!("invalid mal id: {track_id}"))?;
        let mut params: Vec<(String, String)> = Vec::new();
        if let Some(status) = update.status {
            params.push(("status".into(), Self::to_status_str(status).into()));
        }
        if let Some(score) = update.score {
            params.push(("score".into(), score.to_string()));
        }
        if let Some(chapter) = update.last_read_chapter {
            params.push(("num_chapters_read".into(), chapter.floor().to_string()));
        }
        if let Some(volume) = update.last_read_volume {
            params.push(("num_volumes_read".into(), volume.to_string()));
        }
        if let Some(date) = &update.start_read_date {
            // MAL 用 1969-12-31 / 1970-01-01 哨兵值清空日期
            let value = if date == "1969-12-31" || date == "1970-01-01" {
                String::new()
            } else {
                date.clone()
            };
            params.push(("start_date".into(), value));
        }
        if let Some(date) = &update.finish_read_date {
            let value = if date == "1969-12-31" || date == "1970-01-01" {
                String::new()
            } else {
                date.clone()
            };
            params.push(("finish_date".into(), value));
        }
        let form = params
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    url::form_urlencoded::byte_serialize(k.as_bytes()).collect::<String>(),
                    url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        let mut token = self.token(user_key).await?;
        let mut response = self
            .http
            .patch(format!("{BASE_URL}/manga/{id}/my_list_status"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form.clone())
            .send()
            .await
            .map_err(|e| format!("mal update failed: {e}"))?;
        let mut status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // 401：先强制刷新重试一次，失败才清登录态（与 get_json 一致）。
            let refreshed = match &self.oauth {
                Some(manager) => manager.refresh_now(OAuthProvider::Mal, user_key).await,
                None => None,
            };
            if let Some(new_token) = refreshed {
                token = new_token;
                response = self
                    .http
                    .patch(format!("{BASE_URL}/manga/{id}/my_list_status"))
                    .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .body(form.clone())
                    .send()
                    .await
                    .map_err(|e| format!("mal update failed: {e}"))?;
                status = response.status();
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.note_unauthorized(user_key);
            }
        }
        if !status.is_success() {
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            return Err(format!(
                "mal update error (HTTP {status}): {}",
                body.get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
            ));
        }
        Ok(())
    }
}
