//! OAuth2 授权基础设施 —— 共享 client + 中转页回调模式。
//!
//! 背景：komf 是人人可自部署的服务，实例地址未知，无法让各平台把回调
//! 直接指到实例。因此采用「共享 client + 官方中转页」：
//!   1. 本项目在 AniList / MAL / Bangumi 注册共享应用（`client_id` 内置，
//!      用户无需注册；`client_secret` 为机密，**仅在编译时注入**（cargo build
//!      时经 `option_env!` 固化进二进制，不做运行时环境变量），
//!      变量名见 `OAuthProvider::secret_env_var`）；
//!   2. 注册回调固定指向官方中转页（`relay_url`）；
//!   3. 中转页按 `state.redirectUrl`（= 当前实例 `/api/oauth/{p}/callback`；
//!      `start` 支持 `redirect_path_prefix` 加路径前缀，反代/子路径部署用）
//!      把授权结果转交回实例；
//!   4. 本模块负责授权发起（PKCE/state）、token 交换、自动刷新与持久化。
//!
//! token 持久化于 `<work_dir>/oauth.sqlite`（rusqlite），内存缓存于
//! `RwLock`，供 provider 请求时附加 `Authorization: Bearer`。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use url::Url;
use uuid::Uuid;

/// 支持的 OAuth 平台。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OAuthProvider {
    Anilist,
    Mal,
    Bangumi,
    MangaBaka,
}

impl OAuthProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            OAuthProvider::Anilist => "anilist",
            OAuthProvider::Mal => "mal",
            OAuthProvider::Bangumi => "bangumi",
            OAuthProvider::MangaBaka => "mangabaka",
        }
    }

    pub fn from_str(name: &str) -> Option<OAuthProvider> {
        match name.to_ascii_lowercase().as_str() {
            "anilist" => Some(OAuthProvider::Anilist),
            "mal" | "myanimelist" => Some(OAuthProvider::Mal),
            "bangumi" | "bgm" => Some(OAuthProvider::Bangumi),
            "mangabaka" | "mb" => Some(OAuthProvider::MangaBaka),
            _ => None,
        }
    }

    /// 映射到 provider 框架枚举（用于 ProviderError 与名称）。
    pub fn core(&self) -> crate::providers::CoreProviders {
        match self {
            OAuthProvider::Anilist => crate::providers::CoreProviders::Anilist,
            OAuthProvider::Mal => crate::providers::CoreProviders::Mal,
            OAuthProvider::Bangumi => crate::providers::CoreProviders::Bangumi,
            OAuthProvider::MangaBaka => crate::providers::CoreProviders::MangaBaka,
        }
    }

    /// 注入该平台 client_secret 的**构建**环境变量名（cargo build 时经
    /// `option_env!` 固化进二进制；运行时设置无效，需重新编译才生效）。
    pub fn secret_env_var(&self) -> &'static str {
        match self {
            OAuthProvider::Anilist => "KOMF_OAUTH_ANILIST_CLIENT_SECRET",
            OAuthProvider::Mal => "KOMF_OAUTH_MAL_CLIENT_SECRET",
            OAuthProvider::Bangumi => "KOMF_OAUTH_BANGUMI_CLIENT_SECRET",
            OAuthProvider::MangaBaka => "KOMF_OAUTH_MANGABAKA_CLIENT_SECRET",
        }
    }
}

/// PKCE 校验方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkceMethod {
    None,
    Plain,
    S256,
}

/// 共享 client 的应用配置（注册于各平台，回调指向官方中转页）。
/// `client_id` 为公开标识，内置；`client_secret` 为机密，**不内置、不做运行时
/// 环境变量**，仅在编译时经 `option_env!` 注入（见 `OAuthProvider::secret_env_var`）。
#[derive(Debug, Clone)]
pub struct OAuthApp {
    pub client_id: &'static str,
    pub client_secret: Option<String>,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    /// 是否需要 client_secret 参与 token 交换/刷新。
    pub confidential: bool,
    /// 该平台是否强制要求 client_secret（缺失时 token 交换必然失败）。
    pub requires_secret: bool,
    pub pkce: PkceMethod,
    pub scope: Option<&'static str>,
    /// 注册回调 = 官方中转页（所有实例共用）。
    pub relay_url: &'static str,
    /// 注入 client_secret 的构建环境变量名（编译时读取，运行时无效）。
    pub secret_env_var: &'static str,
}

/// 中转页根地址（部署位置，注册共享 client 时使用的 BASE）。

/// 编译时注入的 secret 清理：None / 空白 → None；否则 trim 后 Some。
fn secret_from(env_val: Option<&str>) -> Option<String> {
    env_val
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl OAuthApp {
    /// client_secret 仅在编译时注入（`option_env!`）：cargo build 时设置
    /// `KOMF_OAUTH_*_CLIENT_SECRET` 即固化进二进制；未设置/为空 → None。
    pub fn for_provider(provider: OAuthProvider) -> OAuthApp {
        match provider {
            OAuthProvider::Anilist => OAuthApp {
                // 共享 client_id（AniList 授权码 + PKCE S256；token 交换必须带 secret）。
                client_id: "52218",
                client_secret: secret_from(option_env!("KOMF_OAUTH_ANILIST_CLIENT_SECRET")),
                authorize_url: "https://anilist.co/api/v2/oauth/authorize",
                token_url: "https://anilist.co/api/v2/oauth/token",
                confidential: false,
                requires_secret: true,
                pkce: PkceMethod::S256,
                scope: None,
                relay_url: "https://dyphire.github.io/komf-rs/oauth-relay/anilist.html",
                secret_env_var: "KOMF_OAUTH_ANILIST_CLIENT_SECRET",
            },
            OAuthProvider::Mal => OAuthApp {
                // MAL 机密 client（token 交换必须带 secret，否则 401 invalid_client；
                // 刷新不需要 redirect_uri）。
                client_id: "787c7329c6c39b0aa5676e7e7bfb06cd",
                client_secret: secret_from(option_env!("KOMF_OAUTH_MAL_CLIENT_SECRET")),
                authorize_url: "https://myanimelist.net/v1/oauth2/authorize",
                token_url: "https://myanimelist.net/v1/oauth2/token",
                confidential: false,
                requires_secret: true,
                pkce: PkceMethod::Plain,
                scope: None,
                relay_url: "https://dyphire.github.io/komf-rs/oauth-relay/mal.html",
                secret_env_var: "KOMF_OAUTH_MAL_CLIENT_SECRET",
            },
            OAuthProvider::Bangumi => OAuthApp {
                // Bangumi 机密 client（token/refresh 均需 client_secret）。
                client_id: "bgm72036ab9feb16bdb3",
                client_secret: secret_from(option_env!("KOMF_OAUTH_BANGUMI_CLIENT_SECRET")),
                authorize_url: "https://bgm.tv/oauth/authorize",
                token_url: "https://bgm.tv/oauth/access_token",
                confidential: true,
                requires_secret: true,
                pkce: PkceMethod::None,
                scope: None,
                relay_url: "https://dyphire.github.io/komf-rs/oauth-relay/bangumi.html",
                secret_env_var: "KOMF_OAUTH_BANGUMI_CLIENT_SECRET",
            },
            OAuthProvider::MangaBaka => OAuthApp {
                // MangaBaka 机密 client（OIDC；token 交换必须带 secret）。
                // PKCE 官方仅支持 S256；scope 需 offline_access 才会下发 refresh_token，
                // openid 才能让 userinfo 返回标准 claims（preferred_username）。
                client_id: "flUhLWZbgpGRFcZnxLlNGXotAhxmMFbc",
                client_secret: secret_from(option_env!("KOMF_OAUTH_MANGABAKA_CLIENT_SECRET")),
                authorize_url: "https://mangabaka.org/auth/oauth2/authorize",
                token_url: "https://mangabaka.org/auth/oauth2/token",
                confidential: true,
                requires_secret: true,
                pkce: PkceMethod::S256,
                scope: Some("library.read library.write profile offline_access openid"),
                relay_url: "https://dyphire.github.io/komf-rs/oauth-relay/mangabaka.html",
                secret_env_var: "KOMF_OAUTH_MANGABAKA_CLIENT_SECRET",
            },
        }
    }
}

/// 已获得的 token 集（sqlite 持久化字段）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    /// 过期时刻（unix 秒）；None/0 = 永不过期。
    #[serde(default)]
    pub expires_at: Option<i64>,
}

impl OAuthToken {
    fn expired(&self) -> bool {
        match self.expires_at {
            Some(t) if t > 0 => t <= chrono::Utc::now().timestamp(),
            _ => false,
        }
    }
}

/// 登录状态（WebUI 展示用）。
#[derive(Debug, Clone, Serialize)]
pub struct OAuthStatus {
    pub logged_in: bool,
    pub username: Option<String>,
}

/// 内存态（token 缓存 + username），sqlite 为持久层。
#[derive(Default)]
struct OAuthInner {
    tokens: HashMap<OAuthProvider, OAuthToken>,
    usernames: HashMap<OAuthProvider, String>,
}

/// OAuth 管理器：授权发起、回调交换、自动刷新、持久化。
pub struct OAuthManager {
    http: reqwest::Client,
    db: Mutex<Option<Connection>>,
    inner: RwLock<OAuthInner>,
    /// 授权完成后的跳回路径（WebUI 前端路由）。
    pub callback_return_path: &'static str,
}

const PENDING_TTL_SECS: i64 = 600;

impl OAuthManager {
    /// `work_dir`：配置目录（与 mangabaka/ 等数据目录同级），oauth.sqlite 落于此。
    pub fn new(work_dir: Option<&Path>, http: reqwest::Client) -> Arc<OAuthManager> {
        let db_path = work_dir
            .map(|d| d.join("oauth.sqlite"))
            .unwrap_or_else(|| PathBuf::from("oauth.sqlite"));
        let db = open_db(&db_path);
        let manager = OAuthManager {
            http,
            db: Mutex::new(db),
            inner: RwLock::new(OAuthInner::default()),
            callback_return_path: "/",
        };
        let arc = Arc::new(manager);
        arc.reload_from_db();
        arc
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Option<Connection>> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 启动时把持久化 token 载入内存（供 provider 无回调直接使用）。
    fn reload_from_db(&self) {
        let mut guard = self.conn();
        let Some(conn) = guard.as_mut() else {
            return;
        };
        let mut stmt = match conn.prepare(
            "SELECT provider, access_token, refresh_token, token_type, expires_at, username \
             FROM oauth_tokens",
        ) {
            Ok(s) => s,
            Err(_) => return,
        };
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    OAuthToken {
                        access_token: row.get(1)?,
                        refresh_token: row.get(2)?,
                        token_type: row.get(3)?,
                        expires_at: row.get(4)?,
                    },
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .ok();
        if let Some(rows) = rows {
            let mut inner = self.inner.write().unwrap();
            for row in rows.flatten() {
                if let Some(p) = OAuthProvider::from_str(&row.0) {
                    inner.tokens.insert(p, row.1);
                    if let Some(name) = row.2 {
                        inner.usernames.insert(p, name);
                    }
                }
            }
        }
    }

    fn save_token(&self, provider: OAuthProvider, token: &OAuthToken, username: Option<String>) {
        {
            let mut inner = self.inner.write().unwrap();
            inner.tokens.insert(provider, token.clone());
            if let Some(name) = &username {
                inner.usernames.insert(provider, name.clone());
            }
        }
        let mut guard = self.conn();
        let Some(conn) = guard.as_mut() else {
            return;
        };
        let _ = conn.execute(
            "INSERT INTO oauth_tokens(provider, access_token, refresh_token, token_type, expires_at, username) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(provider) DO UPDATE SET \
               access_token=excluded.access_token, refresh_token=excluded.refresh_token, \
               token_type=excluded.token_type, expires_at=excluded.expires_at, username=excluded.username",
            params![
                provider.as_str(),
                token.access_token,
                token.refresh_token,
                token.token_type,
                token.expires_at,
                username,
            ],
        );
    }

    /// 清除指定平台的登录态（token + username + 残留 pending）。
    pub fn logout(&self, provider: OAuthProvider) {
        {
            let mut inner = self.inner.write().unwrap();
            inner.tokens.remove(&provider);
            inner.usernames.remove(&provider);
        }
        let mut guard = self.conn();
        if let Some(conn) = guard.as_mut() {
            let _ = conn.execute(
                "DELETE FROM oauth_tokens WHERE provider=?1",
                params![provider.as_str()],
            );
            let _ = conn.execute(
                "DELETE FROM oauth_pending WHERE provider=?1",
                params![provider.as_str()],
            );
        }
    }

    /// 登录状态（含 username，供 WebUI 展示）。
    pub fn status(&self, provider: OAuthProvider) -> OAuthStatus {
        let inner = self.inner.read().unwrap();
        OAuthStatus {
            logged_in: inner.tokens.contains_key(&provider),
            username: inner.usernames.get(&provider).cloned(),
        }
    }

    /// 生成 code_verifier（43-128 个 unreserved 字符；两段 uuid hex = 64 字符）。
    /// 记录"通过 komf API 关联"的条目（update 成功后调用；按 provider+track_id upsert）。
    pub fn record_tracker_link(
        &self,
        provider: &str,
        track_id: &str,
        title: Option<&str>,
        url: Option<&str>,
        cover_url: Option<&str>,
    ) {
        let mut guard = self.conn();
        let Some(conn) = guard.as_mut() else {
            return;
        };
        let _ = conn.execute(
            "INSERT INTO tracker_links(provider, track_id, title, url, cover_url, updated_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(provider, track_id) DO UPDATE SET title=excluded.title, url=excluded.url, cover_url=excluded.cover_url, updated_at=excluded.updated_at",
            params![
                provider,
                track_id,
                title,
                url,
                cover_url,
                chrono::Utc::now().timestamp()
            ],
        );
    }

    /// 已关联条目台账（按更新时间倒序）。
    /// 返回 (provider, track_id, title, url, updated_at)。
    pub fn list_tracker_links(
        &self,
    ) -> Vec<(
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
    )> {
        let mut guard = self.conn();
        let Some(conn) = guard.as_mut() else {
            return Vec::new();
        };
        let mut stmt = match conn.prepare(
            "SELECT provider, track_id, title, url, cover_url, updated_at FROM tracker_links ORDER BY updated_at DESC",
        ) {
            Ok(st) => st,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        });
        rows.and_then(|it| it.collect::<Result<Vec<_>, _>>())
            .unwrap_or_default()
    }

    fn new_verifier() -> String {
        format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
    }

    fn code_challenge(verifier: &str, method: PkceMethod) -> Option<String> {
        match method {
            PkceMethod::None => None,
            PkceMethod::Plain => Some(verifier.to_string()),
            PkceMethod::S256 => {
                let digest = Sha256::digest(verifier.as_bytes());
                Some(URL_SAFE_NO_PAD.encode(digest))
            }
        }
    }

    /// 发起授权：生成 verifier/nonce 暂存，返回可跳转的 authorize URL。
    /// `redirect_url`：state.redirectUrl（当前实例 `/api/oauth/{p}/callback`，
    /// 中转页据此把授权结果转交回来）。
    pub fn start(&self, provider: OAuthProvider, redirect_url: &str) -> Result<String, String> {
        let app = OAuthApp::for_provider(provider);
        let verifier = Self::new_verifier();
        let nonce = Uuid::new_v4().simple().to_string();
        let state_payload = serde_json::json!({
            "redirectUrl": redirect_url,
            "nonce": nonce,
        });
        let state = serde_json::to_string(&state_payload).map_err(|e| e.to_string())?;

        // 暂存 pending（TTL）
        let mut guard = self.conn();
        if let Some(conn) = guard.as_mut() {
            let _ = conn.execute(
                "INSERT INTO oauth_pending(provider, nonce, verifier, created_at) VALUES(?1, ?2, ?3, ?4) \
                 ON CONFLICT(provider) DO UPDATE SET nonce=excluded.nonce, verifier=excluded.verifier, created_at=excluded.created_at",
                params![provider.as_str(), nonce, verifier, chrono::Utc::now().timestamp()],
            );
        }
        drop(guard);

        let mut url = Url::parse(app.authorize_url).map_err(|e| e.to_string())?;
        url.query_pairs_mut()
            .append_pair("client_id", app.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", app.relay_url)
            .append_pair("state", &state);
        if let Some(scope) = app.scope {
            url.query_pairs_mut().append_pair("scope", scope);
        }
        if let Some(challenge) = Self::code_challenge(&verifier, app.pkce) {
            url.query_pairs_mut()
                .append_pair("code_challenge", &challenge)
                .append_pair(
                    "code_challenge_method",
                    match app.pkce {
                        PkceMethod::S256 => "S256",
                        PkceMethod::Plain => "plain",
                        PkceMethod::None => unreachable!(),
                    },
                );
        }
        Ok(url.to_string())
    }

    /// 校验回调 state 并完成 code→token 交换。
    pub async fn handle_callback(
        self: &Arc<Self>,
        provider: OAuthProvider,
        code: &str,
        state_raw: &str,
    ) -> Result<(), String> {
        // 1) 解析并校验 state 与 pending nonce
        let state: serde_json::Value =
            serde_json::from_str(state_raw).map_err(|_| "state 解析失败")?;
        let nonce = state
            .get("nonce")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "state 缺少 nonce")?;
        let now = chrono::Utc::now().timestamp();
        let verifier = {
            let mut guard = self.conn();
            let conn = guard.as_mut().ok_or("数据库未就绪")?;
            let pending: Option<(String, String, i64)> = conn
                .query_row(
                    "SELECT nonce, verifier, created_at FROM oauth_pending WHERE provider=?1",
                    params![provider.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .ok();
            let _ = conn.execute(
                "DELETE FROM oauth_pending WHERE provider=?1",
                params![provider.as_str()],
            );
            let Some((stored_nonce, verifier, created)) = pending else {
                return Err("授权会话不存在或已过期，请重新发起授权".to_string());
            };
            if stored_nonce != nonce {
                return Err("state 校验失败（nonce 不匹配）".to_string());
            }
            if now - created > PENDING_TTL_SECS {
                return Err("授权会话已过期，请重新发起授权".to_string());
            }
            verifier
        };

        // 2) 交换 token
        let app = OAuthApp::for_provider(provider);
        if app.requires_secret && app.client_secret.is_none() {
            return Err(format!(
                "{} client_secret 未配置：构建时未注入 {}（重新编译时设置该环境变量，见 docs/oauth-relay/README.md）",
                provider.as_str(),
                app.secret_env_var
            ));
        }
        let mut form: Vec<(&str, String)> = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("redirect_uri", app.relay_url.to_string()),
        ];
        form.push(("client_id", app.client_id.to_string()));
        if let Some(secret) = &app.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        if matches!(app.pkce, PkceMethod::Plain | PkceMethod::S256) {
            form.push(("code_verifier", verifier));
        }
        let token = self.exchange(app.token_url, &form).await.map_err(|e| e)?;

        // 3) 获取 username（失败不阻断登录）
        let username = self.fetch_username(provider, &token.access_token).await;

        self.save_token(provider, &token, username);
        Ok(())
    }

    async fn exchange(
        &self,
        token_url: &str,
        form: &[(&str, String)],
    ) -> Result<OAuthToken, String> {
        // 覆盖为浏览器 UA：全局 client 的 UA（dyphire/komf-rs）会被 AniList 的
        // Cloudflare 拦截（连接级 403/1010），而 MAL/Bangumi 不受影响。
        // 仅对 token 交换/刷新请求生效，不改变其他请求（如 MangaDex 封面）的 UA。
        let resp = self
            .http
            .post(token_url)
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
            )
            .form(form)
            .send()
            .await
            .map_err(|e| format!("token 请求失败：{e}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("token 交换失败（{status}）：{body}"));
        }
        #[derive(Deserialize)]
        struct TokenResp {
            access_token: String,
            #[serde(default)]
            refresh_token: Option<String>,
            #[serde(default)]
            token_type: Option<String>,
            #[serde(default)]
            expires_in: Option<i64>,
        }
        let parsed: TokenResp =
            serde_json::from_str(&body).map_err(|e| format!("token 响应解析失败：{e}：{body}"))?;
        let expires_at = parsed
            .expires_in
            .map(|secs| chrono::Utc::now().timestamp() + secs);
        Ok(OAuthToken {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token,
            token_type: parsed.token_type,
            expires_at,
        })
    }

    /// 获取平台用户名（展示用；失败返回 None）。
    async fn fetch_username(&self, provider: OAuthProvider, access_token: &str) -> Option<String> {
        let bearer = format!("Bearer {access_token}");
        let result = match provider {
            OAuthProvider::Anilist => {
                let body = serde_json::json!({ "query": "query { Viewer { name } }" });
                self.http
                    .post("https://graphql.anilist.co/")
                    .header("Authorization", &bearer)
                    .json(&body)
                    .send()
                    .await
                    .ok()?
                    .json::<serde_json::Value>()
                    .await
                    .ok()?
                    .pointer("/data/Viewer/name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            }
            OAuthProvider::Mal => {
                let resp = self
                    .http
                    .get("https://api.myanimelist.net/v2/users/@me")
                    .header("Authorization", &bearer)
                    .send()
                    .await
                    .ok()?;
                if !resp.status().is_success() {
                    return None;
                }
                resp.json::<serde_json::Value>()
                    .await
                    .ok()?
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            }
            OAuthProvider::Bangumi => {
                let resp = self
                    .http
                    .get("https://api.bgm.tv/v0/me")
                    .header("Authorization", &bearer)
                    .header("User-Agent", "dyphire/komf-rs")
                    .send()
                    .await
                    .ok()?;
                if !resp.status().is_success() {
                    return None;
                }
                resp.json::<serde_json::Value>()
                    .await
                    .ok()?
                    .get("username")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            }
            OAuthProvider::MangaBaka => {
                // OIDC userinfo；字段按 OIDC 标准取 preferred_username → nickname → name。
                let resp = self
                    .http
                    .get("https://mangabaka.org/auth/oauth2/userinfo")
                    .header("Authorization", &bearer)
                    .send()
                    .await
                    .ok()?;
                if !resp.status().is_success() {
                    return None;
                }
                let value = resp.json::<serde_json::Value>().await.ok()?;
                value
                    .get("preferred_username")
                    .or_else(|| value.get("nickname"))
                    .or_else(|| value.get("name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            }
        };
        result
    }

    /// 取当前可用的 access token（过期自动刷新；无 refresh → 清除并返回 None）。
    pub async fn access_token(&self, provider: OAuthProvider) -> Option<String> {
        let token = {
            let inner = self.inner.read().unwrap();
            inner.tokens.get(&provider).cloned()
        }?;
        if !token.expired() {
            return Some(token.access_token);
        }
        let Some(_) = token.refresh_token.clone() else {
            // 无 refresh（AniList 等）：清除，需重新授权
            self.logout(provider);
            return None;
        };
        let app = OAuthApp::for_provider(provider);
        if app.requires_secret && app.client_secret.is_none() {
            // 无 secret 无法刷新：清除登录态，需重新授权（重新编译时注入即可）。
            tracing::warn!(
                "OAuth refresh skipped for {}: {} not set at build time",
                provider.as_str(),
                app.secret_env_var
            );
            self.logout(provider);
            return None;
        }
        match self.refresh_now(provider).await {
            Some(access_token) => Some(access_token),
            None => {
                tracing::warn!("OAuth refresh failed for {}", provider.as_str());
                self.logout(provider);
                None
            }
        }
    }

    /// 强制刷新：源站 401（token 被判无效，本地 expires_at 尚未触发）时调用。
    /// 有 refresh_token 且 secret 齐备则直接走 token 刷新；成功返回新 access token
    /// 并持久化（username 保留），失败返回 None（由调用方决定是否登出）。
    pub async fn refresh_now(&self, provider: OAuthProvider) -> Option<String> {
        let token = {
            let inner = self.inner.read().unwrap();
            inner.tokens.get(&provider).cloned()
        }?;
        let Some(refresh_token) = token.refresh_token.clone() else {
            return None;
        };
        let app = OAuthApp::for_provider(provider);
        if app.requires_secret && app.client_secret.is_none() {
            return None;
        }
        let mut form: Vec<(&str, String)> = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token),
        ];
        form.push(("client_id", app.client_id.to_string()));
        if let Some(secret) = &app.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        if app.confidential {
            form.push(("redirect_uri", app.relay_url.to_string()));
        }
        let fresh = match self.exchange(app.token_url, &form).await {
            Ok(t) if !t.access_token.is_empty() => t,
            _ => return None,
        };
        let username = {
            let inner = self.inner.read().unwrap();
            inner.usernames.get(&provider).cloned()
        };
        self.save_token(provider, &fresh, username);
        Some(fresh.access_token)
    }
}

/// 校验 `start` 的 `redirect_path_prefix`：必须是 `/seg/seg` 形式（以 `/` 开头、
/// 不以 `/` 结尾、段非空），且不超过 256 字节——state 随前缀膨胀，部分平台对
/// state 大小有限制，超长会在授权阶段才失败。字符集不含 `.`：`.`/`..` 段会被
/// URL 规范化用于逃逸前缀、拼出白名单外的回调路径，索性整体禁掉；`%`/`?`/`#`
/// 等同样被字符集挡下。
pub fn validate_redirect_path_prefix(prefix: &str) -> Result<(), String> {
    const MAX_LEN: usize = 256;
    if prefix.len() > MAX_LEN {
        return Err(format!(
            "invalid redirect_path_prefix: {} bytes exceeds the {MAX_LEN}-byte limit",
            prefix.len()
        ));
    }
    let valid = prefix.len() > 1
        && prefix.starts_with('/')
        && !prefix.ends_with('/')
        && prefix[1..].split('/').all(|seg| {
            !seg.is_empty()
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid redirect_path_prefix '{prefix}': must look like /api/v1/komf (leading '/', no trailing '/', segments of [A-Za-z0-9_-])"
        ))
    }
}

fn open_db(path: &Path) -> Option<Connection> {
    let conn = Connection::open(path).ok()?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS oauth_tokens (
            provider TEXT PRIMARY KEY,
            access_token TEXT NOT NULL,
            refresh_token TEXT,
            token_type TEXT,
            expires_at INTEGER,
            username TEXT
        );
        CREATE TABLE IF NOT EXISTS oauth_pending (
            provider TEXT PRIMARY KEY,
            nonce TEXT NOT NULL,
            verifier TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS tracker_links (
            provider TEXT NOT NULL,
            track_id TEXT NOT NULL,
            title TEXT,
            url TEXT,
            cover_url TEXT,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (provider, track_id)
        );",
    )
    .ok()?;
    // 迁移旧库：tracker_links 无 cover_url 列时补充（列已存在时报错，忽略）。
    let _ = conn.execute("ALTER TABLE tracker_links ADD COLUMN cover_url TEXT", []);
    Some(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// client_secret 仅在编译时注入（`option_env!`），源码不内置任何 secret；
    /// 运行时修改环境变量不影响已编译的二进制。
    #[test]
    fn secret_reads_from_build_env_not_runtime() {
        // 四平台变量名与强制要求
        for p in [
            OAuthProvider::Anilist,
            OAuthProvider::Mal,
            OAuthProvider::Bangumi,
            OAuthProvider::MangaBaka,
        ] {
            let app = OAuthApp::for_provider(p);
            assert!(app.requires_secret, "{} requires secret", p.as_str());
            assert_eq!(app.secret_env_var, p.secret_env_var());
        }
        // 值与编译期注入一致（与宏本身同源；运行时 set_var 无法改变它）
        assert_eq!(
            OAuthApp::for_provider(OAuthProvider::Anilist)
                .client_secret
                .as_deref(),
            secret_from(option_env!("KOMF_OAUTH_ANILIST_CLIENT_SECRET")).as_deref(),
        );
    }

    #[test]
    fn secret_from_filters_none_blank_and_trims() {
        assert_eq!(secret_from(None), None);
        assert_eq!(secret_from(Some("")), None);
        assert_eq!(secret_from(Some("   ")), None);
        assert_eq!(secret_from(Some(" abc ")), Some("abc".to_string()));
    }

    /// 前缀校验：合法形式放行；点段、空段、尾斜杠、非法字符一律拒绝。
    #[test]
    fn redirect_path_prefix_validation() {
        for ok in ["/api/v1/komf", "/komf", "/a-b_c/1"] {
            assert!(validate_redirect_path_prefix(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "/", "api", "/api/", "//a", "/a//b", "/a b", "/a?b", "/a%2f", "/..", "/a/../b",
            "/a/./b", "/中",
        ] {
            assert!(validate_redirect_path_prefix(bad).is_err(), "{bad}");
        }
    }

    /// 长度上限：256 字节放行，257 拒绝（state 随前缀膨胀，平台对 state 大小有限制）。
    #[test]
    fn redirect_path_prefix_length_cap() {
        let ok = format!("/{}", "a".repeat(255));
        assert_eq!(ok.len(), 256);
        assert!(validate_redirect_path_prefix(&ok).is_ok());
        let too_long = format!("/{}", "a".repeat(256));
        assert_eq!(too_long.len(), 257);
        let err = validate_redirect_path_prefix(&too_long).unwrap_err();
        assert!(err.contains("256-byte limit"), "{err}");
    }

    /// 编译期注入可见性：cargo build / test 时设置了该变量，则 client_secret 必须
    /// 携带其值（证明 secret 固化在二进制内，而非运行时读取）。未设置时跳过。
    #[test]
    fn build_env_injection_visible() {
        let cases = [
            (
                OAuthProvider::Anilist,
                option_env!("KOMF_OAUTH_ANILIST_CLIENT_SECRET"),
            ),
            (
                OAuthProvider::Mal,
                option_env!("KOMF_OAUTH_MAL_CLIENT_SECRET"),
            ),
            (
                OAuthProvider::Bangumi,
                option_env!("KOMF_OAUTH_BANGUMI_CLIENT_SECRET"),
            ),
            (
                OAuthProvider::MangaBaka,
                option_env!("KOMF_OAUTH_MANGABAKA_CLIENT_SECRET"),
            ),
        ];
        for (p, build_val) in cases {
            let Some(build_val) = build_val else { continue };
            let app = OAuthApp::for_provider(p);
            assert_eq!(
                app.client_secret.as_deref(),
                Some(build_val.trim()),
                "{} build-time secret must be visible",
                p.as_str()
            );
        }
    }
}
