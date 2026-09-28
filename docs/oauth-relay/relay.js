/*
 * komf OAuth 中转页（relay）核心逻辑
 *
 * 作用：共享 client 的回调地址固定指向本页；页面读取授权结果（token/code）后，
 * 依据 state 中编码的 redirectUrl 将结果转交回发起授权的 komf 实例。
 *
 * 协议约定：
 *  - state 必须为 URL 编码的 JSON，至少含 redirectUrl 字段，格式：
 *      {"redirectUrl":"http://host/api/oauth/{provider}/callback","ts":...,"nonce":"..."}
 *  - AniList 隐式流：token 在 URL fragment（#access_token=...），页面将其移入 query 后跳回，
 *    因为服务器端回调只能读取 query；
 *  - MAL / Bangumi / MangaBaka 授权码流：code 在 query，原样透传；
 *  - redirectUrl 仅允许 http(s) 且路径严格匹配 /api/oauth/{provider}/callback，
 *    防止开放重定向。
 */
(function (root, factory) {
  if (typeof module === "object" && module.exports) {
    module.exports = factory();
  } else {
    root.KomfOAuthRelay = factory();
  }
})(typeof self !== "undefined" ? self : this, function () {
  "use strict";

  var VALID_PROVIDERS = ["anilist", "mal", "bangumi", "mangabaka"];

  /**
   * 从 pathname / search 识别 provider。
   * 优先级：?provider= 参数 > 路径最后一段文件名（去 .html）。
   * 非法返回 null。
   */
  function detectProvider(pathname, search) {
    var q = new URLSearchParams(search || "");
    var fromQuery = (q.get("provider") || "").toLowerCase();
    var last = (pathname || "").replace(/\/+$/, "").split("/").pop() || "";
    var fromPath = last.toLowerCase().replace(/\.html$/, "");
    var p = fromQuery || fromPath;
    return VALID_PROVIDERS.indexOf(p) !== -1 ? p : null;
  }

  /**
   * 构建跳转结果（纯函数，便于测试）。
   * input: { provider, hash, search }  均为不含前导 # / ? 的原始字符串。
   * 返回: { ok: true, url } 或 { ok: false, error }
   */
  function buildResult(input) {
    var provider = input.provider;
    var hashParams = new URLSearchParams(input.hash || "");
    var queryParams = new URLSearchParams(input.search || "");

    var error = hashParams.get("error") || queryParams.get("error");
    if (error) {
      return { ok: false, error: "授权失败：" + error };
    }

    var token = hashParams.get("access_token") || queryParams.get("code");
    if (!token) {
      return { ok: false, error: "未收到授权结果（缺少 access_token / code）" };
    }

    var stateRaw = hashParams.get("state") || queryParams.get("state");
    if (!stateRaw) {
      return { ok: false, error: "缺少 state 参数" };
    }

    var state;
    try {
      state = JSON.parse(decodeURIComponent(stateRaw));
    } catch (e) {
      return { ok: false, error: "state 解析失败（应为 URL 编码的 JSON）" };
    }

    var redirectUrl =
      state && typeof state.redirectUrl === "string" ? state.redirectUrl : "";
    // 允许任意深度的路径前缀（子路径/反代部署），但必须以 /api/oauth/{provider}/callback 结尾
    var re = new RegExp(
      "^https?://[^/?#]+/(?:[^/?#]*/)*api/oauth/" + provider + "/callback$"
    );
    if (!re.test(redirectUrl)) {
      return {
        ok: false,
        error:
          "redirectUrl 不合法：应为当前实例的 /api/oauth/" +
          provider +
          "/callback（当前值: " +
          redirectUrl +
          "）",
      };
    }

    var target;
    try {
      target = new URL(redirectUrl);
    } catch (e) {
      return { ok: false, error: "redirectUrl 无法解析为 URL" };
    }
    // state 原样透传（服务器端校验）
    target.searchParams.set("state", stateRaw);
    if (hashParams.has("access_token")) {
      // 隐式流：token 从 fragment 移入 query，保证服务器回调可读取
      target.searchParams.set("access_token", token);
    } else {
      // 授权码流：透传 code
      target.searchParams.set("code", token);
    }
    return { ok: true, url: target.toString() };
  }

  /**
   * 浏览器入口：读取当前页面 URL，跳转或展示错误。
   */
  function main() {
    var provider = detectProvider(window.location.pathname, window.location.search);
    var statusEl = document.getElementById("relay-status");
    var detailEl = document.getElementById("relay-detail");

    function fail(message) {
      if (statusEl) {
        statusEl.textContent = "授权未完成";
        statusEl.className = "badge error";
      }
      if (detailEl) {
        detailEl.textContent = message;
      }
    }

    if (!provider) {
      fail(
        "无法识别 provider（路径: " +
          window.location.pathname +
          "）。本页只接受 anilist / mal / bangumi / mangabaka 四个回调路径。"
      );
      return;
    }

    var result = buildResult({
      provider: provider,
      hash: window.location.hash.substring(1),
      search: window.location.search.substring(1),
    });

    if (!result.ok) {
      fail(result.error);
      return;
    }

    if (statusEl) {
      statusEl.textContent = "授权成功，正在跳转回 komf…";
      statusEl.className = "badge ok";
    }
    if (detailEl) {
      detailEl.textContent = result.url;
    }
    // 短暂停留展示反馈后跳转；fragment 用 replace 不残留历史
    setTimeout(function () {
      window.location.replace(result.url);
    }, 1200);
  }

  return {
    VALID_PROVIDERS: VALID_PROVIDERS,
    detectProvider: detectProvider,
    buildResult: buildResult,
    main: main,
  };
});
