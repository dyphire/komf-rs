//! WebUI 访问保护：非本地/局域网访问要求输入环境变量 `KOMF_WEBUI_KEY` 配置的密钥。
//!
//! 规则（`auth_guard` 中间件，挂在整个 base router 最外层）：
//! - 未配置 `KOMF_WEBUI_KEY` → 不启用，全部放行（保持原行为）；
//! - 请求来源为本地/局域网（loopback / RFC1918 私网 / link-local / ULA）→ 免密钥放行；
//! - 远程访问：校验 `komf_auth` cookie（= SHA-1(密钥 + 固定盐) 的 base64，恒定时间比较）；
//!   未通过时：`/api/*` 返回 401 JSON，其余路径返回内联登录页（样式对齐 Docker Copilot 类认证页）。
//!
//! `POST /api/auth/login`（校验密钥并种 cookie）与 `POST /api/auth/logout`（删 cookie）
//! 在中间件放行名单内，始终可访问。

use axum::body::Body;
use axum::extract::{ConnectInfo, Json, Request};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::net::{IpAddr, SocketAddr};

/// 认证 cookie 名与派生盐。
pub const COOKIE_NAME: &str = "komf_auth";
const COOKIE_SALT: &[u8] = b":komf-webui-auth";

/// 环境变量密钥：未设置/为空 → 不启用访问保护。
pub fn auth_key() -> Option<String> {
    std::env::var("KOMF_WEBUI_KEY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 本地/局域网判定：loopback、RFC1918 私网（10/8、172.16/12、192.168/16）、
/// link-local（169.254/16、fe80::/10）、IPv6 ULA（fc00::/7）。
pub fn is_local_or_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unicast_link_local()
                || (v6.segments()[0] & 0xfe00 == 0xfc00)
        }
    }
}

/// cookie 值 = base64(SHA-1(密钥 + 固定盐))；校验时重算并恒定时间比较。
fn cookie_value(key: &str) -> String {
    use base64::Engine as _;
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(key.as_bytes());
    h.update(COOKIE_SALT);
    base64::engine::general_purpose::STANDARD.encode(h.finalize())
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 从 Cookie 头解析并校验 `komf_auth`。
fn cookie_matches(request: &Request, key: &str) -> bool {
    let Some(h) = request.headers().get(header::COOKIE) else {
        return false;
    };
    let Ok(s) = h.to_str() else { return false };
    let expected = cookie_value(key);
    s.split(';')
        .map(|p| p.trim())
        .find_map(|p| p.strip_prefix(&format!("{COOKIE_NAME}=")).map(|v| v.trim()))
        .map(|v| constant_time_eq(v, &expected))
        .unwrap_or(false)
}

fn cookie_header(value: &str, expire: bool) -> String {
    let max_age = if expire { "; Max-Age=0" } else { "" };
    format!("{COOKIE_NAME}={value}; Path=/; HttpOnly; SameSite=Lax{max_age}")
}

/// 访问保护中间件（挂 base router 最外层）。
pub async fn auth_guard(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let Some(key) = auth_key() else {
        // 未配置密钥：保持原行为，全部放行
        return next.run(request).await;
    };
    // 本地/局域网：免密钥（`KOMF_AUTH_FORCE_REMOTE=1` 仅供调试/验证远程分支，默认关闭）
    let force_remote = std::env::var("KOMF_AUTH_FORCE_REMOTE").is_ok_and(|v| v.trim() == "1");
    if !force_remote && is_local_or_lan(addr.ip()) {
        return next.run(request).await;
    }
    // 登录/登出接口始终可访问
    if request.uri().path().starts_with("/api/auth/") {
        return next.run(request).await;
    }
    // 远程 + cookie 有效：放行
    if cookie_matches(&request, &key) {
        return next.run(request).await;
    }
    // 未授权：API 返回 401 JSON；页面/静态资源返回内联登录页
    if request.uri().path().starts_with("/api/") {
        Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"error":"unauthorized","hint":"请输入 KOMF_WEBUI_KEY 配置的密钥"}"#))
            .unwrap()
            .into_response()
    } else {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(login_page_html()))
            .unwrap()
            .into_response()
    }
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub key: String,
}

/// `POST /api/auth/login`：校验密钥并种认证 cookie（远程访问登录入口）。
pub async fn login(Json(body): Json<LoginRequest>) -> Response {
    let Some(key) = auth_key() else {
        return (StatusCode::NOT_FOUND, "auth disabled").into_response();
    };
    if !constant_time_eq(body.key.trim(), &key) {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"error":"invalid key"}"#))
            .unwrap()
            .into_response();
    }
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header(header::SET_COOKIE, cookie_header(&cookie_value(&key), false))
        .body(Body::empty())
        .unwrap()
}

/// `POST /api/auth/logout`：删除认证 cookie。
pub async fn logout() -> Response {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header(header::SET_COOKIE, cookie_header("", true))
        .body(Body::empty())
        .unwrap()
}

/// 内联登录页（不依赖被保护的 SPA 静态资源），样式对齐 Docker Copilot 类认证页。
fn login_page_html() -> String {
    r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>komf-rs 配置</title>
<style>
  * { box-sizing: border-box; }
  body {
    margin: 0; min-height: 100vh; display: flex; align-items: center; justify-content: center;
    background: #0b0e12; color: #e6edf3;
    font: 14px/1.55 -apple-system, 'Segoe UI', 'PingFang SC', 'Microsoft YaHei', sans-serif;
  }
  .auth { width: 340px; padding: 12px; text-align: center; }
  .auth h1 { font-size: 22px; font-weight: 600; margin: 0 0 6px; letter-spacing: .5px; }
  .auth .sub { color: #8b949e; margin: 0 0 28px; font-size: 13.5px; }
  .auth form { display: flex; flex-direction: column; gap: 14px; }
  .pw { position: relative; display: flex; align-items: center; }
  .pw .icon { position: absolute; left: 12px; font-size: 15px; pointer-events: none; }
  .pw input {
    width: 100%; padding: 11px 40px 11px 38px; font-size: 14px; color: #e6edf3;
    background: #161b22; border: 1px solid #30363d; border-radius: 8px; outline: none;
  }
  .pw input:focus { border-color: #2f81f7; box-shadow: 0 0 0 3px rgba(47,129,247,.15); }
  .pw .eye {
    position: absolute; right: 10px; background: none; border: none; cursor: pointer;
    font-size: 15px; color: #8b949e; padding: 4px;
  }
  button.go {
    width: 100%; padding: 10px; font-size: 14px; font-weight: 600; color: #fff; cursor: pointer;
    background: #2f81f7; border: 1px solid #2f81f7; border-radius: 8px;
    display: flex; align-items: center; justify-content: center; gap: 8px;
  }
  button.go:hover { background: #3b8df8; }
  button.go:disabled { opacity: .6; cursor: not-allowed; }
  .err { color: #f85149; font-size: 12.5px; min-height: 18px; margin: 0; }
  .foot { margin-top: 26px; color: #6e7681; font-size: 11.5px; }
  /* 浅色主题（跟随系统或手动指定，与 SPA 共用 komf-theme） */
  @media (prefers-color-scheme: light) {
    :root:not([data-theme='dark']) body { background: #f6f8fa; color: #1f2328; }
    :root:not([data-theme='dark']) .pw input { background: #fff; border-color: #d0d7de; color: #1f2328; }
    :root:not([data-theme='dark']) .auth .sub, :root:not([data-theme='dark']) .foot { color: #59636e; }
  }
  :root[data-theme='light'] body { background: #f6f8fa; color: #1f2328; }
  :root[data-theme='light'] .pw input { background: #fff; border-color: #d0d7de; color: #1f2328; }
  :root[data-theme='light'] .auth .sub, :root[data-theme='light'] .foot { color: #59636e; }
</style>
</head>
<body>
<div class="auth">
  <h1>komf-rs 配置</h1>
  <p class="sub">请输入密钥进行认证</p>
  <form id="f">
    <div class="pw">
      <span class="icon">&#128273;</span>
      <input id="k" type="password" placeholder="请输入您的密钥" autocomplete="current-password" autofocus />
      <button type="button" class="eye" id="eye" title="显示/隐藏">&#128065;</button>
    </div>
    <p class="err" id="err"></p>
    <button class="go" id="go" type="submit">登录 &rarr;</button>
  </form>
  <p class="foot">密钥由服务端环境变量 KOMF_WEBUI_KEY 配置</p>
</div>
<script>
  // 语言：与 SPA 共用 localStorage 'komf-lang'（zh / en），默认跟随浏览器
  var L = 'zh';
  try {
    var _l = localStorage.getItem('komf-lang');
    if (_l === 'zh' || _l === 'en') { L = _l; }
    else { L = (/^zh/i.test(navigator.language)) ? 'zh' : 'en'; }
  } catch (e) {}
  var S = {
    zh: { title: 'komf-rs 配置', sub: '请输入密钥进行认证', ph: '请输入您的密钥', eye: '显示/隐藏', go: '登录 →', foot: '密钥由服务端环境变量 KOMF_WEBUI_KEY 配置', logging: '登录中…', badKey: '密钥不正确', fail: '登录失败', net: '网络错误：' },
    en: { title: 'komf-rs Config', sub: 'Enter your key to authenticate', ph: 'Enter your key', eye: 'Show/hide', go: 'Sign in →', foot: 'Key is configured via the KOMF_WEBUI_KEY environment variable', logging: 'Signing in…', badKey: 'Invalid key', fail: 'Sign-in failed', net: 'Network error: ' }
  };
  var s = S[L] || S.zh;
  document.title = s.title;
  document.documentElement.lang = L === 'zh' ? 'zh-CN' : 'en';
  document.querySelector('.auth h1').textContent = s.title;
  document.querySelector('.auth .sub').textContent = s.sub;
  var k = document.getElementById('k'), eye = document.getElementById('eye'), err = document.getElementById('err');
  k.placeholder = s.ph;
  eye.title = s.eye;
  var go = document.getElementById('go'); go.innerHTML = s.go;
  document.querySelector('.auth .foot').textContent = s.foot;
  try {
    var _t = localStorage.getItem('komf-theme');
    if (_t === 'light' || _t === 'dark') document.documentElement.setAttribute('data-theme', _t);
  } catch (e) {}
  eye.addEventListener('click', function () {
    k.type = k.type === 'password' ? 'text' : 'password';
    eye.textContent = k.type === 'password' ? '\u{1F441}' : '\u{1F441}';
  });
  document.getElementById('f').addEventListener('submit', async function (e) {
    e.preventDefault(); err.textContent = '';
    go.disabled = true; go.textContent = s.logging;
    try {
      // 相对路径（对齐 snd/komf PR#337）：反代子路径部署（如 https://host/komf/）
      // 下绝对 '/api/...' 会锚 host 根丢前缀；相对路径跟随页面目录。
      var r = await fetch('api/auth/login', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ key: k.value }), credentials: 'same-origin'
      });
      if (r.status === 204) { location.href = './'; return; }
      var t = await r.text().catch(function () { return ''; });
      err.textContent = r.status === 401 ? s.badKey : (s.fail + ' (' + r.status + ')');
    } catch (ex) {
      err.textContent = s.net + ex.message;
    }
    go.disabled = false; go.textContent = s.go;
  });
</script>
</body>
</html>
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_or_lan_detects_all_cases() {
        // loopback
        assert!(is_local_or_lan("127.0.0.1".parse().unwrap()));
        assert!(is_local_or_lan("::1".parse().unwrap()));
        // RFC1918 私网
        assert!(is_local_or_lan("10.1.2.3".parse().unwrap()));
        assert!(is_local_or_lan("172.16.0.1".parse().unwrap()));
        assert!(is_local_or_lan("172.31.255.255".parse().unwrap()));
        assert!(is_local_or_lan("192.168.1.1".parse().unwrap()));
        // link-local
        assert!(is_local_or_lan("169.254.10.20".parse().unwrap()));
        assert!(is_local_or_lan("fe80::1".parse().unwrap()));
        // ULA
        assert!(is_local_or_lan("fc00::1".parse().unwrap()));
        assert!(is_local_or_lan("fd12:3456::1".parse().unwrap()));
        // 公网
        assert!(!is_local_or_lan("8.8.8.8".parse().unwrap()));
        assert!(!is_local_or_lan("172.32.0.1".parse().unwrap()));
        assert!(!is_local_or_lan("2001:4860::8888".parse().unwrap()));
        assert!(!is_local_or_lan("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn cookie_value_roundtrip_and_constant_time() {
        let v = cookie_value("secret-key");
        assert!(!v.is_empty());
        assert!(constant_time_eq(&v, &cookie_value("secret-key")));
        assert!(!constant_time_eq(&v, &cookie_value("other-key")));
        assert!(!constant_time_eq(&v, ""));
    }
}
