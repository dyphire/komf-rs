"use strict";
const assert = require("assert");
const relay = require("../relay.js");

const { detectProvider, buildResult } = relay;

let passed = 0;
function ok(name) {
  passed++;
  console.log("  ✓ " + name);
}

function encodeState(obj) {
  return encodeURIComponent(JSON.stringify(obj));
}

// ---------- detectProvider ----------
console.log("detectProvider");
assert.strictEqual(detectProvider("/oauth-relay/anilist.html", ""), "anilist");
ok("路径 anilist.html → anilist");
assert.strictEqual(detectProvider("/oauth-relay/mal.html", ""), "mal");
ok("路径 mal.html → mal");
assert.strictEqual(
  detectProvider("/oauth-relay/bangumi.html", ""),
  "bangumi"
);
ok("路径 bangumi.html → bangumi");
assert.strictEqual(
  detectProvider("/oauth-relay/mangabaka.html", ""),
  "mangabaka"
);
ok("路径 mangabaka.html → mangabaka");
assert.strictEqual(detectProvider("/x", "?provider=anilist"), "anilist");
ok("?provider= 参数优先");
assert.strictEqual(detectProvider("/oauth-relay/foo.html", ""), null);
ok("非法 provider → null");

// ---------- buildResult：AniList 隐式流 ----------
console.log("buildResult · AniList 隐式流");
{
  const state = encodeState({
    redirectUrl: "http://192.168.1.5:8080/api/oauth/anilist/callback",
    ts: 1769000000,
    nonce: "abc",
  });
  const r = buildResult({
    provider: "anilist",
    hash: "access_token=TOKEN123&token_type=Bearer&expires_in=31536000&state=" + state,
    search: "",
  });
  assert.strictEqual(r.ok, true);
  const u = new URL(r.url);
  assert.strictEqual(
    u.origin + u.pathname,
    "http://192.168.1.5:8080/api/oauth/anilist/callback"
  );
  assert.strictEqual(u.searchParams.get("access_token"), "TOKEN123");
  // state 经编解码往返后应还原为 JSON 文本（服务器端 JSON.parse 可直接用）
  assert.strictEqual(u.searchParams.get("state"), decodeURIComponent(state));
  ok("token 从 fragment 移入 query，state 透传，跳回实例回调");
}
{
  // https 域名 + 子路径部署
  const state = encodeState({
    redirectUrl: "https://komf.example.com/komf/api/oauth/anilist/callback",
  });
  const r = buildResult({
    provider: "anilist",
    hash: "access_token=T&state=" + state,
    search: "",
  });
  assert.strictEqual(r.ok, true);
  assert.ok(new URL(r.url).pathname.startsWith("/komf/api/oauth/anilist/callback"));
  ok("https 域名 + 子路径正常");
}

// ---------- buildResult：授权码流 ----------
console.log("buildResult · 授权码流（MAL/Bangumi/MangaBaka）");
{
  const state = encodeState({
    redirectUrl: "http://localhost:8080/api/oauth/mal/callback",
  });
  const r = buildResult({
    provider: "mal",
    hash: "",
    search: "code=CODE456&state=" + state,
  });
  assert.strictEqual(r.ok, true);
  const u = new URL(r.url);
  assert.strictEqual(u.searchParams.get("code"), "CODE456");
  assert.strictEqual(u.searchParams.get("state"), decodeURIComponent(state));
  ok("code 透传，state 透传");
}

// ---------- buildResult：错误路径 ----------
console.log("buildResult · 错误处理");
{
  const r = buildResult({ provider: "anilist", hash: "", search: "" });
  assert.strictEqual(r.ok, false);
  assert.ok(/access_token/.test(r.error));
  ok("缺 token → 报错");
}
{
  const r = buildResult({
    provider: "anilist",
    hash: "access_token=T",
    search: "",
  });
  assert.strictEqual(r.ok, false);
  assert.ok(/state/.test(r.error));
  ok("缺 state → 报错");
}
{
  const r = buildResult({
    provider: "anilist",
    hash: "access_token=T&state=not-json",
    search: "",
  });
  assert.strictEqual(r.ok, false);
  assert.ok(/state/.test(r.error));
  ok("state 非 JSON → 报错");
}
{
  // redirectUrl 与 provider 不匹配
  const state = encodeState({
    redirectUrl: "http://h/api/oauth/mal/callback",
  });
  const r = buildResult({
    provider: "anilist",
    hash: "access_token=T&state=" + state,
    search: "",
  });
  assert.strictEqual(r.ok, false);
  assert.ok(/redirectUrl/.test(r.error));
  ok("redirectUrl 的 provider 不匹配 → 拒绝");
}
{
  // 非 /api/oauth 路径
  const state = encodeState({ redirectUrl: "http://evil.example.com/steal" });
  const r = buildResult({
    provider: "anilist",
    hash: "access_token=T&state=" + state,
    search: "",
  });
  assert.strictEqual(r.ok, false);
  ok("redirectUrl 非回调路径 → 拒绝（防开放重定向）");
}
{
  // javascript: 协议
  const state = encodeState({ redirectUrl: "javascript:alert(1)" });
  const r = buildResult({
    provider: "anilist",
    hash: "access_token=T&state=" + state,
    search: "",
  });
  assert.strictEqual(r.ok, false);
  ok("javascript: 协议 → 拒绝");
}
{
  // 非 http(s) 协议
  const state = encodeState({ redirectUrl: "ftp://h/api/oauth/anilist/callback" });
  const r = buildResult({
    provider: "anilist",
    hash: "access_token=T&state=" + state,
    search: "",
  });
  assert.strictEqual(r.ok, false);
  ok("ftp:// 协议 → 拒绝");
}
{
  // 平台返回 error
  const r = buildResult({
    provider: "anilist",
    hash: "error=access_denied",
    search: "",
  });
  assert.strictEqual(r.ok, false);
  assert.ok(/access_denied/.test(r.error));
  ok("平台 error 参数 → 报错透传");
}
{
  // 严格路径：尾斜杠不通过
  const state = encodeState({
    redirectUrl: "http://h/api/oauth/anilist/callback/",
  });
  const r = buildResult({
    provider: "anilist",
    hash: "access_token=T&state=" + state,
    search: "",
  });
  assert.strictEqual(r.ok, false);
  ok("路径尾斜杠 → 拒绝（严格匹配）");
}

// ---------- main() 冒烟测试（浏览器集成路径） ----------
console.log("main() 冒烟");
{
  const state = encodeState({
    redirectUrl: "http://localhost:8080/api/oauth/anilist/callback",
  });
  let replaced = null;
  const els = {};
  global.window = {
    location: {
      pathname: "/oauth-relay/anilist.html",
      search: "",
      hash: "#access_token=TOKEN&state=" + state,
      replace: (u) => {
        replaced = u;
      },
    },
  };
  global.document = {
    getElementById: (id) => els[id] || (els[id] = { textContent: "", className: "" }),
  };
  const origSetTimeout = global.setTimeout;
  global.setTimeout = (fn) => {
    fn();
    return 0;
  };
  try {
    relay.main();
    assert.strictEqual(replaced !== null, true);
    const u = new URL(replaced);
    assert.strictEqual(
      u.pathname,
      "/api/oauth/anilist/callback"
    );
    assert.strictEqual(u.searchParams.get("access_token"), "TOKEN");
    assert.strictEqual(JSON.parse(u.searchParams.get("state")).redirectUrl,
      "http://localhost:8080/api/oauth/anilist/callback");
    assert.strictEqual(els["relay-status"].className, "badge ok");
  } finally {
    global.setTimeout = origSetTimeout;
    delete global.window;
    delete global.document;
  }
  ok("成功路径：更新状态并 location.replace 跳回实例");
}
{
  let replaced = null;
  const els = {};
  global.window = {
    location: {
      pathname: "/oauth-relay/anilist.html",
      search: "",
      hash: "#error=access_denied",
      replace: (u) => {
        replaced = u;
      },
    },
  };
  global.document = {
    getElementById: (id) => els[id] || (els[id] = { textContent: "", className: "" }),
  };
  const origSetTimeout = global.setTimeout;
  global.setTimeout = (fn) => {
    fn();
    return 0;
  };
  try {
    relay.main();
    assert.strictEqual(replaced, null);
    assert.strictEqual(els["relay-status"].className, "badge error");
    assert.ok(/access_denied/.test(els["relay-detail"].textContent));
  } finally {
    global.setTimeout = origSetTimeout;
    delete global.window;
    delete global.document;
  }
  ok("错误路径：展示错误信息、不跳转");
}

console.log("\n全部通过：" + passed + " 项断言");
