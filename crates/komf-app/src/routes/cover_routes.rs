//! 封面重定向路由 —— WebUI 展示 provider 封面时经 komf 中转一次：
//!
//! 浏览器请求 `/api/cover/redirect?url=<encoded>` → komf 返回 302 +
//! `Referrer-Policy: no-referrer` → 浏览器跟随重定向时**不带 Referer** 直连
//! 第三方封面 CDN。MangaDex 等封面 CDN 对白名单外 Referer（自部署域名 /
//! 局域网 IP / 搜索引擎）返回占位横幅，无 Referer 时放行真实封面。
//!
//! 仅校验 host 白名单（provider 封面域名，后缀匹配），防止开放重定向 /
//! SSRF；komf 自身不下载、不缓存封面字节。

use axum::extract::Query;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;

/// provider 封面 CDN 域名后缀（host 等于后缀或以 `.后缀` 结尾即放行）。
/// 覆盖：MangaDex（uploads.mangadex.org）、AniList（s4.anilist.co）、MAL
/// （cdn.myanimelist.net）、Bangumi（lain.bgm.tv）、MangaUpdates、BookWalker
/// （img.sos-dan.net）、eHentai（ehgt.org）、WebToons（webtoon-phinf.pstatic.net）、
/// Viz、ComicVine、YenPress、MangaBaka。
const ALLOWED_COVER_HOST_SUFFIXES: &[&str] = &[
    "mangadex.org",
    "anilist.co",
    "myanimelist.net",
    "bgm.tv",
    "mangaupdates.com",
    "bookwalker.jp",
    "bookwalker.com",
    "sos-dan.net",
    "ehgt.org",
    "pstatic.net",
    "viz.com",
    "gamespot.com",
    "yenpress.com",
    "mangabaka.org",
    "webtoons.com",
];

#[derive(Debug, serde::Deserialize)]
struct RedirectQuery {
    url: String,
}

pub fn router() -> Router<crate::routes::SharedState> {
    Router::new().route("/cover/redirect", axum::routing::get(redirect_cover))
}

async fn redirect_cover(Query(query): Query<RedirectQuery>) -> Response {
    let Ok(parsed) = url::Url::parse(&query.url) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let host = parsed.host_str().unwrap_or("");
    let allowed = ALLOWED_COVER_HOST_SUFFIXES
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")));
    if !allowed {
        return StatusCode::BAD_REQUEST.into_response();
    }
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, query.url)
        .header("Referrer-Policy", "no-referrer")
        .body(axum::body::Body::empty())
        .expect("static redirect response")
}
