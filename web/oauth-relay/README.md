# komf OAuth 中转页（oauth-relay）

共享 OAuth client 的统一回调地址。授权结果经本页转交回发起授权的 komf 实例，
解决"人人可部署、实例地址未知"场景下的回调注册问题。

## 原理

```
WebUI 发起授权（state 携带 redirectUrl = 实例 /api/oauth/{provider}/callback）
  → Provider 授权页（回调 = 本中转页，注册时写死）
  → 本中转页读取 token/code + state
  → location.replace 跳回实例 /api/oauth/{provider}/callback
  → komf 服务器校验 state、入库 token
```

- **AniList（隐式流）**：token 在 URL fragment，服务器收不到，由本页移入 query 后跳回；
- **MAL / Bangumi / MangaBaka（授权码流）**：code 在 query，原样透传；
- `redirectUrl` 仅允许 `http(s)://…/api/oauth/{provider}/callback`，防止开放重定向；
- 本页为纯静态资源，无后端、无外部依赖，可托管于 GitHub Pages / Cloudflare Pages 等任意静态托管。

## 文件

| 文件 | 说明 |
| --- | --- |
| `relay.js` | 核心逻辑（UMD，可被 node 直接引用做单元测试） |
| `anilist.html` / `mal.html` / `bangumi.html` / `mangabaka.html` | 四个 provider 的回调壳页面 |
| `index.html` | 落地说明页（列出四个注册 URL） |
| `test/relay.test.js` | node 单元测试 |

## 部署

整个 `web/oauth-relay/` 目录部署到任意静态托管即可，无需构建。

**GitHub Pages**：

1. 把 `oauth-relay/` 内容放到仓库的 `gh-pages` 分支（或 `docs/` 目录 + Settings → Pages 选择该目录）；
2. 部署完成后记录站点根地址，例如 `https://<user>.github.io/oauth-relay/`。

**Cloudflare Pages / 其他**：直接把目录内容作为站点根上传。

## 注册共享 client 时填写的回调 URL

| Provider | 回调 URL |
| --- | --- |
| AniList | `https://<BASE>/anilist.html` |
| MyAnimeList | `https://<BASE>/mal.html` |
| Bangumi | `https://<BASE>/bangumi.html` |
| MangaBaka | `https://<BASE>/mangabaka.html` |

其中 `<BASE>` 为该目录的实际部署地址。四个 URL 都是长期固定、所有人共用的。

## state 协议

授权请求的 `state` 必须为 URL 编码的 JSON，至少包含 `redirectUrl`：

```json
{
  "redirectUrl": "http://<komf实例地址>/api/oauth/anilist/callback",
  "ts": 1769...,
  "nonce": "..."
}
```

中转页只透传 `state`（不改写），最终由 komf 服务器校验。

## 本地验证

```bash
# 1. 启动静态服务器
cd web/oauth-relay
python -m http.server 8000

# 2. 模拟 AniList 隐式流回调（浏览器打开）
# http://localhost:8000/anilist.html#access_token=TESTTOKEN&state=%7B%22redirectUrl%22%3A%22http%3A%2F%2Flocalhost%3A8080%2Fapi%2Foauth%2Fanilist%2Fcallback%22%2C%22ts%22%3A1%2C%22nonce%22%3A%22x%22%7D
# 应看到"授权成功"，随后跳转到 localhost:8080/api/oauth/anilist/callback?access_token=TESTTOKEN&state=...

# 3. 单元测试
node test/relay.test.js
```

## 安全说明

- 中转页不含任何密钥，可公开托管；
- 开放重定向防护：`redirectUrl` 白名单校验（协议 + 路径模式）；
- 真正的防 CSRF 在 komf 服务器端完成（校验 `state` 与发起会话匹配）。
