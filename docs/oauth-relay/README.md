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
| `test/e2e-server.js` | E2E 辅助服务器（:8000 中转页 + :8080 模拟实例回调） |

## 部署

整个 `docs/oauth-relay/` 目录部署到任意静态托管即可，无需构建。

**GitHub Pages（本仓库，project site）**：

1. 内容已位于仓库 `docs/oauth-relay/`；
2. Settings → Pages → Source 选 `Deploy from a branch` → 选择本分支（`rust`）→ 目录选 `/docs` → Save；
3. 等待 1–2 分钟，站点根为 `https://<user>.github.io/<repo>/`，中转页根为 `https://<user>.github.io/<repo>/oauth-relay/`。

**Cloudflare Pages / 其他**：直接把目录内容作为站点根上传。

## 注册共享 client 时填写的回调 URL

| Provider | 回调 URL |
| --- | --- |
| AniList | `https://<BASE>/anilist.html` |
| MyAnimeList | `https://<BASE>/mal.html` |
| Bangumi | `https://<BASE>/bangumi.html` |
| MangaBaka | `https://<BASE>/mangabaka.html` |

其中 `<BASE>` 为该目录的实际部署地址（本项目 Pages 部署时为 `https://<user>.github.io/<repo>/oauth-relay/`）。
四个 URL 都是长期固定、所有人共用的。

## client_secret 注入（部署必读）

共享 client 的 `client_id` 内置在 `komf-core/src/oauth.rs`（公开标识，无需保密）；
**`client_secret` 不内置在源码，也非运行时环境变量**——仅在**编译时**经
`option_env!` 固化进二进制（cargo build 时设置即可，运行时设置无效，需重新编译）。

| Provider | 构建环境变量 |
| --- | --- |
| AniList | `KOMF_OAUTH_ANILIST_CLIENT_SECRET` |
| MyAnimeList | `KOMF_OAUTH_MAL_CLIENT_SECRET` |
| Bangumi | `KOMF_OAUTH_BANGUMI_CLIENT_SECRET` |

三个平台都**强制要求** secret（缺失时 token 交换必然 401，回调会返回明确错误提示）。
token 自动刷新同样依赖 secret，未注入时过期 token 会被清除、需要重新授权。

**GitHub Actions（本项目发布通道）**：先在仓库 Settings → Secrets and variables →
Actions 配置 `KOMF_OAUTH_ANILIST_CLIENT_SECRET` / `KOMF_OAUTH_MAL_CLIENT_SECRET` /
`KOMF_OAUTH_BANGUMI_CLIENT_SECRET` 三个 secret，`release.yml` 的构建 job 已通过
`env:` 引用注入，编译出的二进制与 Docker 镜像即携带 secret，**运行时无需再传**：

```yaml
env:
  KOMF_OAUTH_ANILIST_CLIENT_SECRET: ${{ secrets.KOMF_OAUTH_ANILIST_CLIENT_SECRET }}
  KOMF_OAUTH_MAL_CLIENT_SECRET: ${{ secrets.KOMF_OAUTH_MAL_CLIENT_SECRET }}
  KOMF_OAUTH_BANGUMI_CLIENT_SECRET: ${{ secrets.KOMF_OAUTH_BANGUMI_CLIENT_SECRET }}
```

**自行编译**（本地 / 自建 CI / fork 仓库）：

```bash
KOMF_OAUTH_ANILIST_CLIENT_SECRET='<anilist-secret>' \
KOMF_OAUTH_MAL_CLIENT_SECRET='<mal-secret>' \
KOMF_OAUTH_BANGUMI_CLIENT_SECRET='<bangumi-secret>' \
cargo build --release
```

> 凭据归属：secret 属于共享 client 的所有者（本项目注册者）。第三方自部署者
> 可自行在平台注册自己的 client，并在编译时注入（`client_id` 常量与
> `secret_env_var` 位置见 `oauth.rs`，欢迎 PR 支持配置化）。

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
cd docs/oauth-relay
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
