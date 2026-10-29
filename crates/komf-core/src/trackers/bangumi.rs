//! Bangumi tracker —— bgm.tv Next API（next.bgm.tv/p1）用户列表实现。
//! REST：搜索 `POST /p1/search/subjects`（type=1 书籍），状态/详情
//! `GET /p1/subjects/{id}`（响应内嵌当前用户 interest 收藏状态），更新
//! `PUT /p1/collections/subjects/{id}`（收藏状态/评分）+
//! `PATCH /p1/collections/subjects/{id}`（阅读进度）。
//!
//! 相比 v0 API 的变化：搜索结果与详情均内嵌 `interest`（收藏状态/评分/进度），
//! 不再需要先 `GET /v0/me` 再分页拉取全量收藏来标记 tracked；`get_state`
//! 也由 me + collection + subject 三次请求合并为一次 subject 请求。

use super::{
    TrackMediaType, TrackSearchItem, TrackState, TrackStatus, TrackUpdate, TrackerService,
};
use crate::oauth::{OAuthManager, OAuthProvider};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

const API_BASE: &str = "https://next.bgm.tv";
const SEARCH_LIMIT: usize = 20;

pub struct BangumiTracker {
    http: reqwest::Client,
    oauth: Option<Arc<OAuthManager>>,
}

/// Next API 条目（搜索/详情共用结构；详情比搜索多 eps/volumes 等字段）。
#[derive(Debug, Default, Deserialize)]
struct Subject {
    id: i64,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "nameCN")]
    name_cn: Option<String>,
    #[serde(default)]
    info: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    images: Option<Images>,
    #[serde(default)]
    platform: Option<SubjectPlatform>,
    /// 是否系列条目（系列 = 整部作品的聚合条目，非单行本）。
    #[serde(default)]
    series: bool,
    #[serde(default)]
    eps: Option<i32>,
    #[serde(default)]
    volumes: Option<i32>,
    /// 当前用户的收藏状态（未收藏 → null）。
    #[serde(default)]
    interest: Option<SubjectInterest>,
}

#[derive(Debug, Deserialize)]
struct SubjectPlatform {
    #[serde(default, rename = "typeCN")]
    type_cn: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Images {
    #[serde(default)]
    large: Option<String>,
}

/// 收藏状态（Next API 内嵌于 subject；type：1=wish 2=collect 3=do 4=on_hold 5=dropped）。
#[derive(Debug, Default, Deserialize)]
struct SubjectInterest {
    #[serde(default, rename = "type")]
    collection_type: Option<i64>,
    #[serde(default)]
    rate: Option<i32>,
    #[serde(default, rename = "epStatus")]
    ep_status: Option<i64>,
    #[serde(default, rename = "volStatus")]
    vol_status: Option<i32>,
}

impl BangumiTracker {
    pub fn new(http: reqwest::Client, oauth: Option<Arc<OAuthManager>>) -> Self {
        Self { http, oauth }
    }

    /// 源站判定 token 失效（HTTP 401，且强制刷新失败/重试仍 401）时清除该用户的登录态。
    fn note_unauthorized(&self, user_key: &str) {
        if let Some(manager) = &self.oauth {
            manager.logout(OAuthProvider::Bangumi, user_key);
        }
    }

    async fn token(&self, user_key: &str) -> Result<String, String> {
        let Some(manager) = &self.oauth else {
            return Err("bangumi: not logged in (oauth manager missing)".to_string());
        };
        manager
            .access_token(OAuthProvider::Bangumi, user_key)
            .await
            .ok_or_else(|| "bangumi: not logged in (no access token)".to_string())
    }

    /// 带 Bearer 发送请求；遇 401 先强制刷新重试一次，刷新失败或重试仍 401
    /// 才清登录态（四个 tracker 统一行为：401→强制刷新→重试一次→仍失败才登出）。
    async fn send_authed(
        &self,
        user_key: &str,
        method: reqwest::Method,
        url: &str,
        body: Option<Value>,
        label: &str,
    ) -> Result<reqwest::Response, String> {
        let mut token = self.token(user_key).await?;
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
                Some(manager) => manager.refresh_now(OAuthProvider::Bangumi, user_key).await,
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
                self.note_unauthorized(user_key);
            }
        }
        Ok(response)
    }

    async fn get_json(&self, user_key: &str, url: &str) -> Result<Value, String> {
        let response = self
            .send_authed(user_key, reqwest::Method::GET, url, None, "bangumi")
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

    /// 响应非 2xx 时转为错误（body 原文带上，便于排查）。
    async fn ensure_success(response: reqwest::Response, label: &str) -> Result<(), String> {
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let body: Value = response.json().await.unwrap_or_default();
        Err(format!("{label} error (HTTP {status}): {body}"))
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

    fn from_collection_type(collection_type: Option<i64>) -> Option<TrackStatus> {
        match collection_type {
            Some(1) => Some(TrackStatus::Planning),  // wish
            Some(2) => Some(TrackStatus::Completed), // collect
            Some(3) => Some(TrackStatus::Reading),   // do
            Some(4) => Some(TrackStatus::Paused),    // on_hold
            Some(5) => Some(TrackStatus::Dropped),
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

    /// subject → 搜索候选（tracked 取内嵌 interest 是否存在）。
    fn subject_to_item(subject: Subject) -> TrackSearchItem {
        let tracked = subject.interest.is_some();
        let title = subject
            .name_cn
            .as_deref()
            .filter(|t| !t.is_empty())
            .unwrap_or(&subject.name);
        // 书籍条目的 platform.typeCN 标注载体："漫画"/"小说"/"轻小说"/"画集"…
        let media_type = match subject.platform.as_ref().and_then(|p| p.type_cn.as_deref()) {
            Some(p) if p.contains("小说") => Some(TrackMediaType::Novel),
            Some(p) if p.contains("漫画") => Some(TrackMediaType::Manga),
            _ => None,
        };
        TrackSearchItem {
            id: subject.id.to_string(),
            title: if title.is_empty() {
                "Unknown Title"
            } else {
                title
            }
            .to_string(),
            cover_url: subject.images.as_ref().and_then(|i| i.large.clone()),
            description: subject.summary.or(subject.info),
            tracked,
            media_type,
            series: subject.series,
            url: Some(format!("https://bgm.tv/subject/{}", subject.id)),
        }
    }

    /// 单条目详情（Next API 响应内嵌当前用户 interest 与 eps/volumes）。
    async fn subject(&self, user_key: &str, id: i64) -> Result<Subject, String> {
        let body = self
            .get_json(user_key, &format!("{API_BASE}/p1/subjects/{id}"))
            .await?;
        serde_json::from_value(body).map_err(|e| format!("bangumi subject parse failed: {e}"))
    }
}

#[async_trait::async_trait]
impl TrackerService for BangumiTracker {
    fn provider(&self) -> &'static str {
        "bangumi"
    }

    fn is_logged_in(&self, user_key: &str) -> bool {
        match &self.oauth {
            Some(manager) => manager.status(OAuthProvider::Bangumi, user_key).logged_in,
            None => false,
        }
    }

    async fn search(
        &self,
        user_key: &str,
        query: &str,
        nsfw: bool,
    ) -> Result<Vec<TrackSearchItem>, String> {
        // 链接输入：bgm.tv/bangumi.tv/subject/{id} 直接定位单条目。
        if let Some(id) = Self::extract_subject_url(query) {
            let subject = self.subject(user_key, id).await?;
            return Ok(vec![Self::subject_to_item(subject)]);
        }
        // R18 条目仅在登录态可见（bgm.tv 对未登录请求忽略 NSFW 过滤；本路由已要求登录）。
        let response = self
            .send_authed(
                user_key,
                reqwest::Method::POST,
                &format!("{API_BASE}/p1/search/subjects?limit={SEARCH_LIMIT}"),
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
        let subjects: Vec<Subject> =
            serde_json::from_value(body.get("data").cloned().unwrap_or(Value::Null))
                .map_err(|e| format!("bangumi search parse failed: {e}"))?;
        Ok(subjects.into_iter().map(Self::subject_to_item).collect())
    }

    async fn get_state(&self, user_key: &str, track_id: &str) -> Result<TrackState, String> {
        let id: i64 = track_id
            .parse()
            .map_err(|_| format!("invalid bangumi id: {track_id}"))?;
        // 单次请求：interest（收藏状态/评分/进度）与 eps/volumes 均内嵌于详情。
        let subject = self.subject(user_key, id).await?;
        let interest = subject.interest.unwrap_or_default();
        Ok(TrackState {
            score: interest.rate,
            status: Self::from_collection_type(interest.collection_type),
            last_read_chapter: interest.ep_status.map(|v| v as f32),
            last_read_volume: interest.vol_status,
            total_chapters: subject.eps,
            total_volumes: subject.volumes,
            start_read_date: None, // Bangumi API 不返回收藏开始日期
            finish_read_date: None,
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
            .map_err(|_| format!("invalid bangumi id: {track_id}"))?;
        // Next API 拆分两类更新：收藏状态/评分（PUT）与阅读进度（PATCH）。
        // 动态构建 body：未提供的字段省略（WebUI "keep" 语义）；日期无可写端点，忽略。
        let mut collection_body = serde_json::Map::new();
        if let Some(status) = update.status {
            collection_body.insert("type".to_string(), json!(Self::to_type(status)));
        }
        if let Some(score) = update.score {
            collection_body.insert("rate".to_string(), json!(score));
        }
        let mut progress_body = serde_json::Map::new();
        if let Some(chapter) = update.last_read_chapter {
            progress_body.insert("epStatus".to_string(), json!(chapter.floor() as i64));
        }
        if let Some(volume) = update.last_read_volume {
            progress_body.insert("volStatus".to_string(), json!(volume));
        }
        let has_collection_update = !collection_body.is_empty();
        if !has_collection_update && progress_body.is_empty() {
            return Ok(());
        }
        // PUT 要求 type/rate 成对：只改其一时先读现有收藏值补齐
        //（未收藏缺省 type=3 在读 / rate=0）。
        if has_collection_update && (update.status.is_none() || update.score.is_none()) {
            let interest = self
                .subject(user_key, id)
                .await?
                .interest
                .unwrap_or_default();
            if update.status.is_none() {
                collection_body.insert(
                    "type".to_string(),
                    json!(interest.collection_type.unwrap_or(3)),
                );
            }
            if update.score.is_none() {
                collection_body.insert("rate".to_string(), json!(interest.rate.unwrap_or(0)));
            }
        }
        let url = format!("{API_BASE}/p1/collections/subjects/{id}");
        if has_collection_update {
            let response = self
                .send_authed(
                    user_key,
                    reqwest::Method::PUT,
                    &url,
                    Some(Value::Object(collection_body)),
                    "bangumi update",
                )
                .await?;
            Self::ensure_success(response, "bangumi update").await?;
        }
        if !progress_body.is_empty() {
            let response = self
                .send_authed(
                    user_key,
                    reqwest::Method::PATCH,
                    &url,
                    Some(Value::Object(progress_body.clone())),
                    "bangumi progress update",
                )
                .await?;
            if response.status() == reqwest::StatusCode::NOT_FOUND && !has_collection_update {
                // 未收藏时进度 PATCH 返回 404：先以默认状态建收藏，再重试进度更新。
                let response = self
                    .send_authed(
                        user_key,
                        reqwest::Method::PUT,
                        &url,
                        Some(json!({ "type": 3, "rate": 0 })),
                        "bangumi update",
                    )
                    .await?;
                Self::ensure_success(response, "bangumi update").await?;
                let response = self
                    .send_authed(
                        user_key,
                        reqwest::Method::PATCH,
                        &url,
                        Some(Value::Object(progress_body)),
                        "bangumi progress update",
                    )
                    .await?;
                Self::ensure_success(response, "bangumi progress update").await?;
            } else {
                Self::ensure_success(response, "bangumi progress update").await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(platform: Option<&str>) -> Subject {
        Subject {
            id: 1902,
            name: "Title".to_string(),
            platform: platform.map(|type_cn| SubjectPlatform {
                type_cn: Some(type_cn.to_string()),
            }),
            ..Subject::default()
        }
    }

    #[test]
    fn subject_to_item_maps_book_platforms() {
        let novel = BangumiTracker::subject_to_item(subject(Some("小说")));
        assert_eq!(novel.media_type, Some(TrackMediaType::Novel));
        let light_novel = BangumiTracker::subject_to_item(subject(Some("轻小说")));
        assert_eq!(light_novel.media_type, Some(TrackMediaType::Novel));
        let manga = BangumiTracker::subject_to_item(subject(Some("漫画")));
        assert_eq!(manga.media_type, Some(TrackMediaType::Manga));
    }

    #[test]
    fn subject_to_item_without_known_platform_has_no_media_type() {
        let artbook = BangumiTracker::subject_to_item(subject(Some("画集")));
        assert_eq!(artbook.media_type, None);
        let none = BangumiTracker::subject_to_item(subject(None));
        assert_eq!(none.media_type, None);
    }

    #[test]
    fn subject_to_item_reads_embedded_interest_and_series_flag() {
        let mut value = subject(Some("漫画"));
        assert!(!BangumiTracker::subject_to_item(subject(Some("漫画"))).tracked);
        value.interest = Some(SubjectInterest {
            collection_type: Some(3),
            rate: Some(8),
            ..SubjectInterest::default()
        });
        value.series = true;
        let item = BangumiTracker::subject_to_item(value);
        assert!(item.tracked);
        assert!(item.series);
    }

    #[test]
    fn collection_type_maps_to_track_status() {
        assert_eq!(
            BangumiTracker::from_collection_type(Some(1)),
            Some(TrackStatus::Planning)
        );
        assert_eq!(
            BangumiTracker::from_collection_type(Some(2)),
            Some(TrackStatus::Completed)
        );
        assert_eq!(
            BangumiTracker::from_collection_type(Some(3)),
            Some(TrackStatus::Reading)
        );
        assert_eq!(
            BangumiTracker::from_collection_type(Some(4)),
            Some(TrackStatus::Paused)
        );
        assert_eq!(
            BangumiTracker::from_collection_type(Some(5)),
            Some(TrackStatus::Dropped)
        );
        assert_eq!(BangumiTracker::from_collection_type(None), None);
    }
}
