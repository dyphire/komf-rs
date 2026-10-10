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
//!
//! 多用户：token/登录态按 `(provider, user_key)` 隔离，`user_key` 是不透明
//! 字符串（由调用方如媒体服务器传入其用户 ID）。所有数据访问方法都要求
//! `user_key`；未显式指定时用 [`DEFAULT_USER_KEY`]（历史单用户数据迁移后
//! 也归于此），因此旧行为完全保留。

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

/// 未提供用户身份时的默认账户键：历史数据与单用户调用方（如 komf 自带
/// WebUI、元数据 provider）全部落在此键下。
pub const DEFAULT_USER_KEY: &str = "default";

/// 校验调用方传入的用户身份键：`[A-Za-z0-9_-]{1,64}`。
/// 字符集与 `validate_redirect_path_prefix` 一致（排除 `.`/`/`/`%` 等路径
/// 敏感字符），足以覆盖 UUID 与 komga 风格短 ID。SQL 全部参数绑定，此规则
/// 只是输入卫生，超长/非法字符在路由层即被拒（400）。
pub fn validate_user_key(key: &str) -> Result<(), String> {
    const MAX_LEN: usize = 64;
    if key.is_empty() || key.len() > MAX_LEN {
        return Err(format!(
            "invalid user key: length must be 1-{MAX_LEN} bytes, got {}",
            key.len()
        ));
    }
    if !key
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(format!(
            "invalid user key '{key}': only [A-Za-z0-9_-] allowed"
        ));
    }
    Ok(())
}

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

/// 内存态（token 缓存 + username），sqlite 为持久层。键 = (provider, user_key)。
#[derive(Default)]
struct OAuthInner {
    tokens: HashMap<(OAuthProvider, String), OAuthToken>,
    usernames: HashMap<(OAuthProvider, String), String>,
}

/// OAuth 管理器：授权发起、回调交换、自动刷新、持久化。
pub struct OAuthManager {
    http: reqwest::Client,
    db: Mutex<Option<Connection>>,
    inner: RwLock<OAuthInner>,
    /// 刷新串行锁：Bangumi/MangaBaka 等平台为 refresh_token 轮换制（旧 token
    /// 用后即废），并发刷新会让后到者拿着已作废的旧 refresh_token 撞上
    /// invalid_grant 而被误登出；锁内重读可让后到者直接复用先到者的刷新结果。
    refresh_lock: tokio::sync::Mutex<()>,
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
            refresh_lock: tokio::sync::Mutex::new(()),
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
            "SELECT provider, user_key, access_token, refresh_token, token_type, expires_at, username \
             FROM oauth_tokens",
        ) {
            Ok(s) => s,
            Err(_) => return,
        };
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    OAuthToken {
                        access_token: row.get(2)?,
                        refresh_token: row.get(3)?,
                        token_type: row.get(4)?,
                        expires_at: row.get(5)?,
                    },
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .ok();
        if let Some(rows) = rows {
            let mut inner = self.inner.write().unwrap();
            for row in rows.flatten() {
                if let Some(p) = OAuthProvider::from_str(&row.0) {
                    let key = (p, row.1);
                    inner.tokens.insert(key.clone(), row.2);
                    if let Some(name) = row.3 {
                        inner.usernames.insert(key, name);
                    }
                }
            }
        }
    }

    fn save_token(
        &self,
        provider: OAuthProvider,
        user_key: &str,
        token: &OAuthToken,
        username: Option<String>,
    ) {
        {
            let mut inner = self.inner.write().unwrap();
            let key = (provider, user_key.to_string());
            inner.tokens.insert(key.clone(), token.clone());
            if let Some(name) = &username {
                inner.usernames.insert(key, name.clone());
            }
        }
        let mut guard = self.conn();
        let Some(conn) = guard.as_mut() else {
            return;
        };
        let _ = conn.execute(
            "INSERT INTO oauth_tokens(provider, user_key, access_token, refresh_token, token_type, expires_at, username) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(provider, user_key) DO UPDATE SET \
               access_token=excluded.access_token, refresh_token=excluded.refresh_token, \
               token_type=excluded.token_type, expires_at=excluded.expires_at, username=excluded.username",
            params![
                provider.as_str(),
                user_key,
                token.access_token,
                token.refresh_token,
                token.token_type,
                token.expires_at,
                username,
            ],
        );
    }

    /// 清除指定平台、指定用户的登录态（token + username + 该用户残留 pending）。
    pub fn logout(&self, provider: OAuthProvider, user_key: &str) {
        {
            let mut inner = self.inner.write().unwrap();
            let key = (provider, user_key.to_string());
            inner.tokens.remove(&key);
            inner.usernames.remove(&key);
        }
        let mut guard = self.conn();
        if let Some(conn) = guard.as_mut() {
            let _ = conn.execute(
                "DELETE FROM oauth_tokens WHERE provider=?1 AND user_key=?2",
                params![provider.as_str(), user_key],
            );
            let _ = conn.execute(
                "DELETE FROM oauth_pending WHERE provider=?1 AND user_key=?2",
                params![provider.as_str(), user_key],
            );
        }
    }

    /// 登录状态（含 username，供 WebUI 展示）。
    pub fn status(&self, provider: OAuthProvider, user_key: &str) -> OAuthStatus {
        let inner = self.inner.read().unwrap();
        let key = (provider, user_key.to_string());
        OAuthStatus {
            logged_in: inner.tokens.contains_key(&key),
            username: inner.usernames.get(&key).cloned(),
        }
    }

    /// 记录"通过 komf API 关联"的条目（update 成功后调用；按 user_key + provider
    /// + track_id upsert）。
    pub fn record_tracker_link(
        &self,
        provider: &str,
        user_key: &str,
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
            "INSERT INTO tracker_links(provider, user_key, track_id, title, url, cover_url, updated_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(provider, user_key, track_id) DO UPDATE SET title=excluded.title, url=excluded.url, cover_url=excluded.cover_url, updated_at=excluded.updated_at",
            params![
                provider,
                user_key,
                track_id,
                title,
                url,
                cover_url,
                chrono::Utc::now().timestamp()
            ],
        );
    }

    /// 已关联条目台账（按更新时间倒序），仅返回指定用户的记录。
    /// 返回 (provider, track_id, title, url, cover_url, updated_at)。
    pub fn list_tracker_links(
        &self,
        user_key: &str,
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
            "SELECT provider, track_id, title, url, cover_url, updated_at FROM tracker_links \
             WHERE user_key=?1 ORDER BY updated_at DESC",
        ) {
            Ok(st) => st,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![user_key], |row| {
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

    /// 生成 code_verifier（43-128 个 unreserved 字符；两段 uuid hex = 64 字符）。
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

    /// 发起授权：生成 verifier/nonce 暂存（连同归属的 user_key），返回可跳转的
    /// authorize URL。`redirect_url`：state.redirectUrl（当前实例
    /// `/api/oauth/{p}/callback`，中转页据此把授权结果转交回来）。
    pub fn start(
        &self,
        provider: OAuthProvider,
        user_key: &str,
        redirect_url: &str,
    ) -> Result<String, String> {
        let app = OAuthApp::for_provider(provider);
        let verifier = Self::new_verifier();
        let nonce = Uuid::new_v4().simple().to_string();
        let state_payload = serde_json::json!({
            "redirectUrl": redirect_url,
            "nonce": nonce,
        });
        let state = serde_json::to_string(&state_payload).map_err(|e| e.to_string())?;

        // 暂存 pending（TTL）。主键 (provider, nonce)：同平台可并发发起多场
        // 授权（不同用户各自登录），互不覆盖。
        let mut guard = self.conn();
        if let Some(conn) = guard.as_mut() {
            let _ = conn.execute(
                "INSERT INTO oauth_pending(provider, nonce, verifier, user_key, created_at) \
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    provider.as_str(),
                    nonce,
                    verifier,
                    user_key,
                    chrono::Utc::now().timestamp()
                ],
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

    /// 校验回调 state 并完成 code→token 交换。token 归属的 user_key 从
    /// pending（发起时存入）取回，调用方无需也无法指定。
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
        let (verifier, user_key) = {
            let mut guard = self.conn();
            let conn = guard.as_mut().ok_or("数据库未就绪")?;
            let pending: Option<(String, String, i64)> = conn
                .query_row(
                    "SELECT verifier, user_key, created_at FROM oauth_pending \
                     WHERE provider=?1 AND nonce=?2",
                    params![provider.as_str(), nonce],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .ok();
            let _ = conn.execute(
                "DELETE FROM oauth_pending WHERE provider=?1 AND nonce=?2",
                params![provider.as_str(), nonce],
            );
            let Some((verifier, user_key, created)) = pending else {
                return Err("授权会话不存在或已过期，请重新发起授权".to_string());
            };
            if now - created > PENDING_TTL_SECS {
                return Err("授权会话已过期，请重新发起授权".to_string());
            }
            (verifier, user_key)
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

        self.save_token(provider, &user_key, &token, username);
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

    /// 取指定用户当前可用的 access token（过期自动刷新；无 refresh → 清除该
    /// 用户登录态并返回 None）。
    pub async fn access_token(&self, provider: OAuthProvider, user_key: &str) -> Option<String> {
        let token = {
            let inner = self.inner.read().unwrap();
            inner.tokens.get(&(provider, user_key.to_string())).cloned()
        }?;
        if !token.expired() {
            return Some(token.access_token);
        }
        let Some(_) = token.refresh_token.clone() else {
            // 无 refresh（AniList 等）：清除，需重新授权
            self.logout(provider, user_key);
            return None;
        };
        let app = OAuthApp::for_provider(provider);
        if app.requires_secret && app.client_secret.is_none() {
            // 无 secret 无法刷新：清除登录态，需重新授权（重新编译时注入即可）。
            tracing::warn!(
                "OAuth refresh skipped for {} (user {}): {} not set at build time",
                provider.as_str(),
                user_key,
                app.secret_env_var
            );
            self.logout(provider, user_key);
            return None;
        }
        // 串行化刷新并在锁内重读：等待锁期间可能已被其他任务刷新过，直接复用，
        // 避免轮换制 refresh_token 被重复刷新（后到者会撞 invalid_grant）。
        let _guard = self.refresh_lock.lock().await;
        let token = {
            let inner = self.inner.read().unwrap();
            inner.tokens.get(&(provider, user_key.to_string())).cloned()
        }?;
        if !token.expired() {
            return Some(token.access_token);
        }
        match self.do_refresh(provider, user_key, &app).await {
            Some(access_token) => Some(access_token),
            None => {
                tracing::warn!(
                    "OAuth refresh failed for {} (user {})",
                    provider.as_str(),
                    user_key
                );
                self.logout(provider, user_key);
                None
            }
        }
    }

    /// 强制刷新：源站 401（token 被判无效，本地 expires_at 尚未触发）时调用。
    /// 有 refresh_token 且 secret 齐备则直接走 token 刷新；成功返回新 access token
    /// 并持久化（username 保留），失败返回 None（由调用方决定是否登出）。
    /// 调用方持有的 token 被判 401 时，若等待锁期间其他任务已刷新出新 token，
    /// 直接返回新 token（复用，不做二次轮换）。
    pub async fn refresh_now(&self, provider: OAuthProvider, user_key: &str) -> Option<String> {
        let seen = {
            let inner = self.inner.read().unwrap();
            inner
                .tokens
                .get(&(provider, user_key.to_string()))
                .map(|t| t.access_token.clone())
        };
        let app = OAuthApp::for_provider(provider);
        let _guard = self.refresh_lock.lock().await;
        let token = {
            let inner = self.inner.read().unwrap();
            inner.tokens.get(&(provider, user_key.to_string())).cloned()
        }?;
        if app.requires_secret && app.client_secret.is_none() {
            return None;
        }
        if Some(&token.access_token) != seen.as_ref() {
            // 等待锁期间已有其他任务刷新出新 token，复用其结果。
            return Some(token.access_token);
        }
        self.do_refresh(provider, user_key, &app).await
    }

    /// 实际执行刷新（调用方必须已持有 `refresh_lock`）：重读内存 token，
    /// 走 refresh_token 授权交换新 token 并持久化。响应缺 refresh_token 时
    /// 沿用旧值（RFC 6749 §6 惯例，防服务端未返回时丢失刷新能力）。
    async fn do_refresh(
        &self,
        provider: OAuthProvider,
        user_key: &str,
        app: &OAuthApp,
    ) -> Option<String> {
        let token = {
            let inner = self.inner.read().unwrap();
            inner.tokens.get(&(provider, user_key.to_string())).cloned()
        }?;
        let refresh_token = token.refresh_token.clone()?;
        if app.requires_secret && app.client_secret.is_none() {
            return None;
        }
        let mut form: Vec<(&str, String)> = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token.clone()),
        ];
        form.push(("client_id", app.client_id.to_string()));
        if let Some(secret) = &app.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        if app.confidential {
            form.push(("redirect_uri", app.relay_url.to_string()));
        }
        let mut fresh = match self.exchange(app.token_url, &form).await {
            Ok(t) if !t.access_token.is_empty() => t,
            _ => return None,
        };
        if fresh.refresh_token.is_none() {
            fresh.refresh_token = Some(refresh_token);
        }
        let username = {
            let inner = self.inner.read().unwrap();
            inner
                .usernames
                .get(&(provider, user_key.to_string()))
                .cloned()
        };
        self.save_token(provider, user_key, &fresh, username);
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

/// 判断表是否已有指定列（`PRAGMA table_info`）。
fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map(|rows| rows.flatten().collect::<Vec<_>>())
        .unwrap_or_default();
    names.iter().any(|n| n == column)
}

/// 重建表：把旧表数据（缺失的列以默认值补齐）搬入新 schema 后丢弃旧表。
/// SQLite 不能改主键/加主键列，改键只能重建。
fn rebuild_table(conn: &Connection, table: &str, new_ddl: &str, insert_select: &str) {
    let backup = format!("{table}_old");
    let _ = conn.execute(&format!("ALTER TABLE {table} RENAME TO {backup}"), []);
    let _ = conn.execute_batch(new_ddl);
    let _ = conn.execute(insert_select, []);
    let _ = conn.execute(&format!("DROP TABLE {backup}"), []);
}

fn open_db(path: &Path) -> Option<Connection> {
    let conn = Connection::open(path).ok()?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS oauth_tokens (
            provider TEXT NOT NULL,
            user_key TEXT NOT NULL DEFAULT 'default',
            access_token TEXT NOT NULL,
            refresh_token TEXT,
            token_type TEXT,
            expires_at INTEGER,
            username TEXT,
            PRIMARY KEY (provider, user_key)
        );
        CREATE TABLE IF NOT EXISTS oauth_pending (
            provider TEXT NOT NULL,
            nonce TEXT NOT NULL,
            verifier TEXT NOT NULL,
            user_key TEXT NOT NULL DEFAULT 'default',
            created_at INTEGER NOT NULL,
            PRIMARY KEY (provider, nonce)
        );
        CREATE TABLE IF NOT EXISTS tracker_links (
            provider TEXT NOT NULL,
            user_key TEXT NOT NULL DEFAULT 'default',
            track_id TEXT NOT NULL,
            title TEXT,
            url TEXT,
            cover_url TEXT,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (provider, user_key, track_id)
        );",
    )
    .ok()?;
    // 迁移旧库（主键变更只能重建表；user_key 一律回填 'default'）：
    if !column_exists(&conn, "oauth_tokens", "user_key") {
        rebuild_table(
            &conn,
            "oauth_tokens",
            "CREATE TABLE oauth_tokens (
                provider TEXT NOT NULL,
                user_key TEXT NOT NULL DEFAULT 'default',
                access_token TEXT NOT NULL,
                refresh_token TEXT,
                token_type TEXT,
                expires_at INTEGER,
                username TEXT,
                PRIMARY KEY (provider, user_key)
            );",
            "INSERT INTO oauth_tokens(provider, user_key, access_token, refresh_token, token_type, expires_at, username) \
             SELECT provider, 'default', access_token, refresh_token, token_type, expires_at, username FROM oauth_tokens_old",
        );
    }
    if !column_exists(&conn, "oauth_pending", "user_key") {
        rebuild_table(
            &conn,
            "oauth_pending",
            "CREATE TABLE oauth_pending (
                provider TEXT NOT NULL,
                nonce TEXT NOT NULL,
                verifier TEXT NOT NULL,
                user_key TEXT NOT NULL DEFAULT 'default',
                created_at INTEGER NOT NULL,
                PRIMARY KEY (provider, nonce)
            );",
            "INSERT INTO oauth_pending(provider, nonce, verifier, user_key, created_at) \
             SELECT provider, nonce, verifier, 'default', created_at FROM oauth_pending_old",
        );
    }
    if !column_exists(&conn, "tracker_links", "user_key") {
        rebuild_table(
            &conn,
            "tracker_links",
            "CREATE TABLE tracker_links (
                provider TEXT NOT NULL,
                user_key TEXT NOT NULL DEFAULT 'default',
                track_id TEXT NOT NULL,
                title TEXT,
                url TEXT,
                cover_url TEXT,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (provider, user_key, track_id)
            );",
            // cover_url 一律回填 NULL（超旧库可能缺该列；INSERT SELECT 引用不存在的
            // 列会整体失败丢数据，台账行非关键数据，下次 update 会补回）。
            "INSERT INTO tracker_links(provider, user_key, track_id, title, url, cover_url, updated_at) \
             SELECT provider, 'default', track_id, title, url, NULL, updated_at FROM tracker_links_old",
        );
    } else {
        // 已是新 schema 的库：仅补齐 cover_url（列已存在时报错，忽略）。
        let _ = conn.execute("ALTER TABLE tracker_links ADD COLUMN cover_url TEXT", []);
    }
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

    /// user_key 校验：字符集与长度上限（kmrs 等调用方的用户 ID 形态须被接受，
    /// 路径敏感字符须被拒）。
    #[test]
    fn user_key_validation() {
        for ok in [
            "default",
            "alice",
            "01J5Y5ZQ0K",
            "550e8400-e29b-41d4-a716-446655440000",
            &"a".repeat(64),
        ] {
            assert!(validate_user_key(ok).is_ok(), "{ok}");
        }
        for bad in ["", "a b", "a/b", "a.b", "a%2f", "中文", &"a".repeat(65)] {
            assert!(validate_user_key(bad).is_err(), "{bad}");
        }
    }

    fn test_manager_dir(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komf-oauth-core-{test_name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup_dir(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 旧 schema（provider 单键、无 user_key 列）→ 新管理器打开后数据归入
    /// 'default' 用户且读写正常。
    #[test]
    fn migration_assigns_existing_tokens_to_default_user() {
        let dir = test_manager_dir("migrate");
        let db_path = dir.join("oauth.sqlite");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE oauth_tokens (
                    provider TEXT PRIMARY KEY,
                    access_token TEXT NOT NULL,
                    refresh_token TEXT,
                    token_type TEXT,
                    expires_at INTEGER,
                    username TEXT
                );
                CREATE TABLE tracker_links (
                    provider TEXT NOT NULL,
                    track_id TEXT NOT NULL,
                    title TEXT,
                    url TEXT,
                    cover_url TEXT,
                    updated_at INTEGER NOT NULL,
                    PRIMARY KEY (provider, track_id)
                );
                INSERT INTO oauth_tokens(provider, access_token, username) \
                    VALUES('anilist', 'legacy-token', 'legacy-user');
                INSERT INTO tracker_links(provider, track_id, title, updated_at) \
                    VALUES('anilist', '42', 'Legacy Title', 1700000000);",
            )
            .unwrap();
        }
        let mgr = OAuthManager::new(Some(&dir), reqwest::Client::new());
        // 旧 token 归 default。
        let status = mgr.status(OAuthProvider::Anilist, DEFAULT_USER_KEY);
        assert!(status.logged_in);
        assert_eq!(status.username.as_deref(), Some("legacy-user"));
        // 其他用户不受影响（未登录）。
        assert!(!mgr.status(OAuthProvider::Anilist, "alice").logged_in);
        // 旧台账归 default，新用户看不到。
        let links = mgr.list_tracker_links(DEFAULT_USER_KEY);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].1, "42");
        assert!(mgr.list_tracker_links("alice").is_empty());
        cleanup_dir(&dir);
    }

    /// 多用户隔离：同平台两个用户的 token 互不可见；logout 只清掉目标用户。
    #[tokio::test]
    async fn per_user_token_isolation() {
        let dir = test_manager_dir("isolate");
        let mgr = OAuthManager::new(Some(&dir), reqwest::Client::new());
        let token_a = OAuthToken {
            access_token: "token-alice".to_string(),
            refresh_token: None,
            token_type: Some("Bearer".to_string()),
            expires_at: None,
        };
        let token_b = OAuthToken {
            access_token: "token-bob".to_string(),
            ..token_a.clone()
        };
        mgr.save_token(
            OAuthProvider::Mal,
            "alice",
            &token_a,
            Some("alice".to_string()),
        );
        mgr.save_token(OAuthProvider::Mal, "bob", &token_b, Some("bob".to_string()));

        assert_eq!(
            mgr.access_token(OAuthProvider::Mal, "alice")
                .await
                .as_deref(),
            Some("token-alice")
        );
        assert_eq!(
            mgr.access_token(OAuthProvider::Mal, "bob").await.as_deref(),
            Some("token-bob")
        );
        assert_eq!(
            mgr.status(OAuthProvider::Mal, "alice").username.as_deref(),
            Some("alice")
        );

        mgr.logout(OAuthProvider::Mal, "alice");
        assert!(!mgr.status(OAuthProvider::Mal, "alice").logged_in);
        assert!(mgr.status(OAuthProvider::Mal, "bob").logged_in);

        // 重新打开（持久层重载）：bob 的 token 仍在，alice 已彻底清除。
        drop(mgr);
        let mgr = OAuthManager::new(Some(&dir), reqwest::Client::new());
        assert_eq!(
            mgr.access_token(OAuthProvider::Mal, "bob").await.as_deref(),
            Some("token-bob")
        );
        assert!(!mgr.status(OAuthProvider::Mal, "alice").logged_in);
        cleanup_dir(&dir);
    }

    /// 同平台并发发起多场授权：pending 按 (provider, nonce) 存储，互不覆盖，
    /// 且各场 pending 记住自己的 user_key。
    #[test]
    fn concurrent_pending_logins_same_provider() {
        let dir = test_manager_dir("pending");
        let mgr = OAuthManager::new(Some(&dir), reqwest::Client::new());

        let nonce_of = |url: &str| -> String {
            let url = Url::parse(url).unwrap();
            let state = url
                .query_pairs()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.into_owned())
                .unwrap();
            serde_json::from_str::<serde_json::Value>(&state).unwrap()["nonce"]
                .as_str()
                .unwrap()
                .to_string()
        };

        let url_a = mgr
            .start(
                OAuthProvider::Anilist,
                "alice",
                "http://x/api/oauth/anilist/callback",
            )
            .unwrap();
        let url_b = mgr
            .start(
                OAuthProvider::Anilist,
                "bob",
                "http://x/api/oauth/anilist/callback",
            )
            .unwrap();
        let nonce_a = nonce_of(&url_a);
        let nonce_b = nonce_of(&url_b);
        assert_ne!(nonce_a, nonce_b);

        let guard = mgr.conn();
        let conn = guard.as_ref().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM oauth_pending WHERE provider='anilist'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "两场同平台授权必须共存");
        let user_of_a: String = conn
            .query_row(
                "SELECT user_key FROM oauth_pending WHERE nonce=?1",
                params![nonce_a],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(user_of_a, "alice");
        cleanup_dir(&dir);
    }
}
