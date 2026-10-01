# Komga、Kavita 与 Stump 元数据抓取器（Rust）

[English](README.md) | **简体中文**

这是 [komf](https://github.com/Snd-R/komf) 的 **Rust 实现**，用于为你的数字漫画库获取元数据与缩略图。它能自动捕获 **Komga**、**Kavita** 和 **Stump** 中新添加的系列并更新元数据、缩略图和书籍级数据；也支持手动按系列、按库或全库进行搜索、识别与匹配。

## 状态

- Komga：REST API + SSE 事件监听（自动更新、通知）
- Kavita：REST API + SignalR 事件监听（JWT 认证、自动刷新）
- Stump（仅 Rust）：GraphQL API + GraphQL WebSocket 事件监听（基于任务批量窗口的自动更新，API Key 或 JWT 认证）
- 12 个元数据 provider 已实现，支持按 provider 与按库配置
- Discord webhook + Apprise 通知（Velocity 模板）
- ComicInfo 读写、书籍排序、评分标签、阅读方向覆盖
- 配置热更新（`PATCH /api/config`）、任务跟踪、元数据搜索/识别/匹配/重置端点
- 用户脚本兼容的配置界面
- 内置 WebUI 工作台 **（仅 Rust）**：12 个 Provider 矩阵、按库覆盖、通知模板编辑器、任务与实时 SSE 进度、搜索试跑一键设元数据、阅读状态同步页（AniList / MAL / Bangumi）、离线 DB 下载、明暗主题
- **MAL / AniList / MangaBaka / Bangumi OAuth 登录（仅 Rust）**：服务端 OAuth2，采用「共享 client + 官方中转页」方案——无需为每个实例注册回调；Provider 页面提供登录/退出/状态展示，token 自动刷新并持久化于 SQLite
- **阅读状态同步（Tracker，仅 Rust）**：AniList / MyAnimeList / MangaBaka / Bangumi 阅读状态同步，含 WebUI 页面与 `/api/tracker/*` 接口——支持标题或平台链接搜索、tracked 标记、状态读取与状态/评分/进度推送

## 元数据 provider

| Provider            | 搜索              | 系列元数据 | 书籍元数据 |
| ------------------- | --------------- | ----- | ----- |
| MangaUpdates        | ✅               | ✅     | —     |
| MyAnimeList         | ✅               | ✅     | ✅     |
| AniList             | ✅               | ✅     | —     |
| MangaDex            | ✅               | ✅     | ✅     |
| BookWalker          | ✅               | ✅     | ✅     |
| Bangumi (bgm.tv)    | ✅               | ✅     | ✅     |
| ComicVine           | ✅               | ✅     | ✅     |
| YenPress            | ✅               | ✅     | ✅     |
| Viz                 | ✅               | ✅     | ✅     |
| Webtoons            | ✅               | ✅     | ✅     |
| MangaBaka           | ✅               | ✅     | —     |
| **eHentai**（仅 Rust） | ✅               | ✅     | —     |
| ~~Kodansha~~        | 占位符（Kotlin 已弃用） |       |       |
| ~~Nautiljon~~       | 占位符（Kotlin 已弃用） |       |       |
| ~~Hentag~~          | 占位符（Kotlin 已弃用） |       |       |

Provider 可在全局（`metadataProviders.defaultProviders`）或按库（`metadataProviders.libraryProviders`）配置。每个 provider 支持优先级、启用/禁用、媒体类型过滤（`MANGA`/`NOVEL`/`COMIC`/`WEBTOON`）、作者/画师角色映射、按字段的系列/书籍元数据开关，以及 provider 专属选项（如 `coverLanguages`、`tagsScoreThreshold`、`preferredLanguages`）。Kodansha、Nautiljon、Hentag 仅为兼容性而识别——Kotlin 版映射为 `error("Unsupported")`，Rust 版直接不注册——启用它们无效。

## 构建

依赖：[Rust](https://rustup.rs/)（stable 工具链）。

```sh
cargo build --release # 构建 release 二进制到 target/release/komf-app
cargo test --workspace # 运行单元测试
```

OAuth `client_secret` 采用**编译时注入**（非运行时环境变量）：构建时设置
`KOMF_OAUTH_ANILIST_CLIENT_SECRET` / `KOMF_OAUTH_MAL_CLIENT_SECRET` /
`KOMF_OAUTH_BANGUMI_CLIENT_SECRET`，经 `option_env!` 固化进二进制。
官方发布的二进制/镜像已携带 secret；详见
[`docs/oauth-relay/README.md`](docs/oauth-relay/README.md)。

## 运行

仓库不提供 `application.yml`；请使用模板：

```sh
cp examples/application.example.yml application.yml # Linux/macOS
copy examples/application.example.yml application.yml # Windows
# 编辑 application.yml：Komga/Kavita/Stump 凭据、providers、元数据更新等
./komf-app [path to config] # application.yml 的路径或其所在目录
```

模板（`application.example.yml`）包含全部配置项与内联注释；敏感字段（Komga 账号/密码/API key、e-hentai/exhentai cookie）默认为空/占位。

未传路径时使用 `KOMF_CONFIG_DIR` 环境变量；两者都未设置时，服务以内置默认配置启动（仅 HTTP 8085，无 mediaserver 凭据）；通过 `PATCH /api/config` 修改的配置会写回 `./application.yml`（不存在则创建）；数据库默认 `./database.sqlite`。

环境变量（与 Kotlin 版一致）：

| 变量                                             | 说明                                                                                                                         |
| ---------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `KOMF_KOMGA_BASE_URI`                          | Komga 服务地址                                                                                                                 |
| `KOMF_KOMGA_USER` / `KOMF_KOMGA_PASSWORD`      | Komga basic auth                                                                                                           |
| `KOMF_KOMGA_API_KEY`                           | Komga API key（`X-API-Key` 认证，设置后优先于账号密码）                                                                                   |
| `KOMF_KAVITA_BASE_URI` / `KOMF_KAVITA_API_KEY` | Kavita 地址 + API key                                                                                                        |
| `KOMF_STUMP_BASE_URI` / `KOMF_STUMP_API_KEY`   | Stump 地址 + API key（`stump_` 前缀，设置后优先于账号密码）                                                                                 |
| `KOMF_STUMP_USER` / `KOMF_STUMP_PASSWORD`      | Stump 账号密码（仅当未配置 API key 时用于换取 JWT）                                                                                        |
| `KOMF_SERVER_PORT`                             | HTTP 端口（默认 8085，需重启）                                                                                                       |
| `KOMF_SERVER_BIND`                             | HTTP 监听地址（默认 `0.0.0.0`；仅本机用 `127.0.0.1`，需重启）                                                                               |
| `KOMF_LOG_LEVEL`                               | 日志级别（默认 INFO）                                                                                                              |
| `KOMF_DISCORD_WEBHOOKS`                        | 逗号分隔的 Discord webhook URL                                                                                                  |
| `KOMF_APPRISE_URLS`                            | 逗号分隔的 Apprise URL                                                                                                          |
| `KOMF_METADATA_PROVIDERS_MAL_CLIENT_ID`        | MAL provider 必需                                                                                                            |
| `KOMF_METADATA_PROVIDERS_COMIC_VINE_API_KEY`   | ComicVine provider 必需                                                                                                      |
| `KOMF_METADATA_PROVIDERS_BANGUMI_TOKEN`        | Bangumi token（显示 NSFW 条目）                                                                                                  |
| `KOMF_AUTH_KEY`                                | 敏感操作访问密钥（可选，公网暴露强烈建议设置）：设置后，非本地/局域网的敏感请求必须输入（本地/局域网免密钥）；`GET /version` 与 `GET /api/health` 照常放行；旧变量 `KOMF_WEBUI_KEY` 仍兼容回退 |
| `KOMF_WEB_DIR`                                 | WebUI 静态目录覆盖（查找顺序 `web/dist` -> `ui`）                                                                                      |
| `KOMF_AUTH_FORCE_REMOTE`                       | 仅调试：`1` 时把所有来源按远程处理，强制走密钥校验                                                                                                |
| `KOMF_TAG_TRANSLATION`                         | 仅环境变量开关（无配置项）：内置标签翻译（seriesTitleLanguage 为中文时的英文→中文 tags 映射，非 ehentai 独有翻译）默认启用；设为 `0` 或 `false`（大小写不敏感）时禁用                |

### Docker

```sh
docker run -d --name komf ghcr.io/dyphire/komf-rs:latest \
 -p 8085:8085 \
 -v /path/to/config:/config \ # 存放 application.yml 的目录
```

镜像发布在 `ghcr.io/dyphire/komf-rs`（`latest` + 版本 tag），将 `/config` 暴露为卷（`KOMF_CONFIG_DIR=/config`），把你的 `application.yml` 放到那里即可。如需本地构建，执行 `docker build -f docker/Dockerfile . -t komf-rs`。

## 配置

仓库不包含 `application.yml`；所有配置项都在模板 [`application.example.yml`](examples/application.example.yml) 中说明（每个字段带内联注释与代码默认值；敏感字段——Komga 账号/密码/API key、e-hentai/exhentai cookie——为空占位）。

使用方法：

1. 将模板复制为 `application.yml`（具体命令见 [运行](#运行)）。
2. 编辑 `application.yml`：填写 Komga/Kavita/Stump 凭据并启用需要的 provider；每个选项都有行内说明。
3. 用配置文件启动服务（路径参数或 `KOMF_CONFIG_DIR`）；无配置文件时以内置默认运行。

## 安全

服务内置**可选密钥认证门**——**公网暴露使用时强烈建议设置 `KOMF_AUTH_KEY`**。设置 `KOMF_AUTH_KEY`（旧变量 `KOMF_WEBUI_KEY` 仍兼容回退）后，**非本地/局域网**来源的**敏感操作**请求必须输入密钥——本地/回环与局域网（RFC1918 / link-local / IPv6 ULA）来源始终免密钥放行；非敏感的 `GET /version`（版本查询）与 `GET /api/health`（健康/状态查询）照常放行。未授权的敏感 `/api/*` 请求返回 `401`；页面/静态资源请求返回内置登录页（输入框样式与 WebUI 一致）。登录成功 `POST /api/auth/login` 会种下 HttpOnly 会话 cookie（`komf_auth`）；`POST /api/auth/logout` 清除之。凭证二选一通过即放行：cookie 由 SHA-1（密钥 + 固定盐）派生并以恒定时间比较；`Authorization: Bearer <base64(密钥)>` 请求头（密钥经 base64 编码后携带，服务端解码后恒定时间比较，避免明文密钥出现在请求头/访问日志）供脚本/API 客户端直接携带。`KOMF_AUTH_FORCE_REMOTE=1`（仅调试）把所有来源强制按远程处理以走密钥分支。

未启用密钥门时服务**没有内置鉴权**：任何能连上 HTTP 端口的人都可以读取（脱敏后的）配置、通过 `PATCH /api/config` 修改配置并调用元数据接口。请把它当数据库管理后台对待：

- **仅本机（家用推荐）：** 绑定回环地址，只有本机能连：
  
  ```yaml
  server:
    bind: 127.0.0.1
    port: 8085
  ```
  
  或 `KOMF_SERVER_BIND=127.0.0.1`（需重启）。需要远程访问时走 SSH 隧道。

- **局域网/VPS 暴露：** 在前面架带鉴权的反向代理，并用防火墙封掉原始端口。示例：
  
  ```nginx
  # nginx：basic auth
  server {
    listen 80; server_name komf.example.com;
    location / {
      auth_basic "komf"; auth_basic_user_file /etc/nginx/.htpasswd;
      proxy_pass http://127.0.0.1:8085;
    }
  }
  ```
  
  ```caddy
  # Caddy：basic auth（一行）
  komf.example.com {
    basicauth { admin $2a$14$... }
    reverse_proxy 127.0.0.1:8085
  }
  ```

## 按库配置

任何元数据更新选项或 provider 都可以按库（Komga、Kavita 或 Stump 库 id）限定作用范围：`metadataUpdate.library.<libraryId>` 与 `metadataProviders.libraryProviders.<libraryId>`——参见模板（`application.example.yml`）中的注释占位。

## 元数据聚合

默认按配置的 provider 优先级，从第一个正匹配处抓取元数据。设置 `aggregate: true` 时聚合所有 provider 的元数据：某字段仅在前一个 provider 未提供时才从下一个 provider 获取。

## 通知

配置了任何 webhook URL 后，书籍添加完成会调用 webhook。消息格式可用放在 `templatesDirectory/discord` 或 `templatesDirectory/apprise` 的 Velocity 模板自定义：

- Discord：`title.vm`、`title_url.vm`、`description.vm`、`footer.vm`、`field_<index>_name<_inline>.vm`、`field_<index>_value.vm`
- Apprise：`apprise_title.vm`、`apprise_body.vm`

Docker 部署时模板放在挂载的 `/config/discord` 或 `/config/apprise` 目录。

## HTTP 端点

### 配置

- `GET /api/config`、`PATCH /api/config` —— 读取 / 更新配置（热更新）

### 认证（设置 `KOMF_AUTH_KEY` 后启用）

- `POST /api/auth/login` —— `{"key":"..."}`：成功返回 `204` + `komf_auth` cookie，失败 `401`
- `POST /api/auth/logout` —— 清除认证 cookie
- 受保护请求携带凭证（二选一）：`Cookie: komf_auth=<登录所得>`，或 `Authorization: Bearer <base64(KOMF_AUTH_KEY)>`（服务端解码后校验）
- 免鉴权放行（远程）：`GET /version`、`GET /api/health`；其余敏感操作均需鉴权
- 未授权敏感 `/api/*` 请求返回 `401`；其他路径返回内置登录页；本地/局域网来源绕过认证门

### 离线数据库下载

- `POST /api/update-manga-baka-db`、`POST /api/update-book-walker-db` —— 触发离线 DB 下载；以 NDJSON 流输出进度事件（`ProgressEvent` / `FinishedEvent` / `ErrorEvent`）直到断流。支持手动触发；此外各 provider 的 `updateIntervalHours`（默认 24，0 = 仅手动）开启后台定时更新：MangaBaka 比较官方 sha1 checksum（本地库 checksum 一致时跳过），BookWalker 通过 HEAD 请求比较 `Last-Modified`。下载为原子操作（临时文件 + rename），失败时保留旧库并 15 分钟后快速重试。

### 任务

- `GET /api/jobs`、`GET /api/jobs/all`（`DELETE`）、`GET /api/jobs/{jobId}/events`（SSE，单任务）
- `GET /api/jobs/events`（SSE，全局 firehose，可选 `?ids=a,b,c` 过滤）—— 单连接观察全部任务活动；帧沿用单任务事件名，另加 `JobCreatedEvent` / `JobFinishedEvent` 生命周期帧，每帧 `data` 为扁平 JSON（含 `jobId` + `seriesId`）；连接时回放当前 RUNNING 快照，单个任务结束不断流，慢客户端丢帧追赶

### 元数据（`{media-server}` = `komga`、`kavita` 或 `stump`）

- `GET /api/{media-server}/metadata/providers` —— 已启用的 provider（可选 `libraryId`）
- `GET /api/{media-server}/metadata/search?name=...` —— 搜索（可选 `libraryId`）
- `GET /api/{media-server}/metadata/series-cover?providerSeriesId=...`
- `POST /api/{media-server}/metadata/identify` —— 从 provider 设置元数据：

```json
{
 "libraryId": "09TDSWK3Q0XRA",
 "seriesId": "07XF6HKAWHHV4",
 "provider": "MANGA_UPDATES",
 "providerSeriesId": "1"
}
```

- `POST /api/{media-server}/metadata/match/library/{libraryId}` —— 匹配库中全部系列
- `POST /api/{media-server}/metadata/match/library/{libraryId}/series/{seriesId}` —— 匹配单个系列
- `POST /api/{media-server}/metadata/reset/library/{libraryId}` —— 重置全部系列元数据
- `POST /api/{media-server}/metadata/reset/library/{libraryId}/series/{seriesId}` —— 重置单个系列元数据

### 媒体服务器

- `GET /api/{media-server}/media-server/connected`、`GET /api/{media-server}/media-server/libraries`

### 通知

- `GET|POST /api/notifications/{discord,apprise}/{templates,send,render}`

### 封面重定向

- `GET /api/cover/redirect?url=<encoded>` —— `302` + `Referrer-Policy: no-referrer` 跳转到目标封面 URL。WebUI 用它展示搜索结果封面：MangaDex 等封面 CDN 对白名单外 `Referer`（自部署域名、局域网 IP 等）返回占位横幅，无 `Referer` 时放行真实封面——重定向让浏览器丢弃 Referer 后直连加载真封面。目标 host 经 provider 封面域名白名单校验（防开放重定向 / SSRF），白名单外返回 `400`。搜索 API 的 `imageUrl` 保持直链，第三方服务端消费者不受影响，也可按需使用本端点。

### OAuth 登录（`{provider}` = `anilist`、`mal`、`bangumi` 或 `mangabaka`）

元数据 provider 的服务端 OAuth2 登录，采用**共享 client + 官方中转页**方案（无需按实例注册回调）。机制、client_secret 注入与部署说明见 [`docs/oauth-relay/README.md`](docs/oauth-relay/README.md)。

- `GET /api/oauth/{provider}/start` —— `302` 跳转到平台授权页（state 携带本实例回调地址；anilist/mal/mangabaka 使用 PKCE）。可选 `?redirect_path_prefix=/prefix` 给回调路径加前缀，用于反代把回调挂进自有命名空间（如 kmrs 的 `/api/v1/komf`）或子路径部署
- `GET /api/oauth/{provider}/callback` —— OAuth 回调（经中转页转交）：校验后以 code 换取 token，存入 `<configDir>/oauth.sqlite`，随后 `302` 到 `/?oauth=success`（或 `/?oauth=error&message=...`）
- `GET /api/oauth/{provider}/status` —— `200` JSON `{"logged_in":bool,"username":string|null}`
- `POST /api/oauth/{provider}/logout` —— `204`，清除已存 token

登录后该 provider 的请求以 OAuth bearer token 鉴权（优先于手动 `bangumiToken` / `KOMF_METADATA_PROVIDERS_MAL_CLIENT_ID`）；token 过期后若有 refresh token 则自动刷新，否则清除登录态并回退匿名请求。tracker 相关接口在请求时发现登录态已失效（过期且无法刷新、未注入 secret、刷新被拒、或源站以 `401` 拒绝该 token——同时清除已存登录态）时返回 `401`，避免阅读状态同步静默地以无用户态运行。WebUI Provider 页显示各平台登录状态并提供登录/退出入口。

### 阅读状态（Tracker）（`{provider}` = `anilist`、`mal`、`bangumi` 或 `mangabaka`；需先 OAuth 登录）

四平台阅读列表同步，基于上述 OAuth 登录。

- `GET /api/tracker/{provider}/search?name=...&nsfw=...` —— 搜索平台条目；每条带 `tracked`（是否已在用户列表中）。`nsfw` 缺省 `true`，仅在 `false` 时过滤成人内容。`name` 也支持平台条目链接——`anilist.co/manga/{id}`、`myanimelist.net/manga/{id}`、`bgm.tv`/`bangumi.tv/subject/{id}`、`mangabaka.org/{id}`——后端直接解析为单条结果。
- `GET /api/tracker/{provider}/state?trackId=...` —— 当前列表条目（`status`、`score`、已读卷/话、开始/完成日期、总量）；未入列表返回空状态。
- `POST /api/tracker/{provider}/update` —— 推送状态更新：

```json
{ "trackId": "70345", "score": 8, "status": "reading", "lastReadChapter": 12, "lastReadVolume": 1, "startReadDate": "2026-09-01", "finishReadDate": null }
```

`status` 取值为 `reading`（在读）、`planning`（想看）、`completed`（看过）、`paused`（搁置）、`dropped`（抛弃）、`rereading`（重看）；省略该字段表示保持当前状态。

说明：`tracked` 为用户态——AniList 取 `mediaListEntry`、MAL 取 `my_list_status`（详情并发拉取）、Bangumi 拉取用户收藏集合、MangaBaka 批量查用户收藏（`GET /v1/my/library/batch`）。Bangumi 搜索在 v0 API 失败时兜底旧版 `GET /search/subject/{q}?type=1`（元数据匹配 provider 同样兜底）。AniList 评分跟随账户 `mediaListOptions.scoreFormat`（POINT_10 账户读写 0–10，其余 0–100）。MangaBaka 评分为 0–100，其 `plan_to_read`/`considering` 映射为 `planning`；更新时条目不存在则 `POST` 创建、已存在则 `PATCH`。

- `GET /api/tracker/links` —— 本实例已关联条目的本地台账：每次成功 `update` 都会按 `{provider, trackId, title, coverUrl, url, updatedAt}`（最新在前）upsert 进 `<configDir>/oauth.sqlite` 的 `tracker_links` 表。`update` 可附带 `title` / `coverUrl`（不推送平台，仅用于台账展示）。

WebUI Tracker 页中该台账显示在**已关联**下（与搜索结果互斥）；选中条目后状态表单直接在条目下方展开，再次点击可收起。

### 旧版路由（无 `/api` 前缀，兼容保留）

- `/config`、`/{media-server}/{providers,search,identify,match,reset}`

### 健康检查

- `GET /` —— 内置 WebUI（`web/dist`）；未构建时返回 `404`。
- `GET /version` —— 返回 `200` JSON `{"name":"komf-rs","version":"..."}`，供结构化检查。
- `GET /api/health` —— 返回 `200` JSON `{"status":"ok","name":"komf-rs","version":"..."}`，`/api` 前缀下的结构化健康检查。
- 所有响应均携带 `X-Komf-Version` 响应头。
- Docker 镜像 `HEALTHCHECK` 通过 `wget -qO- http://127.0.0.1:8085/api/health` 探测（`--interval=30s --timeout=5s --start-period=15s --retries=3`），与 Dockerfile 一致。

## Web UI 集成

用户脚本可直接在 Komga / Kavita Web UI 中配置 komf 并识别系列。它们与本 Rust 实现暴露的配置端点通信。

- [Komf 用户脚本](https://github.com/dyphire/komf-userscript)

### 内置配置 WebUI

本仓库自带配置前端（`web/`，Vite + React + TS）：

```sh
cd web && npm install && npm run build # 生成 web/dist
./komf-app                             # 浏览器打开 http://localhost:8085/
npm run dev                            # 开发模式（/api 代理到 127.0.0.1:8085）
```

它是完整工作台：媒体服务器连接与库列表、12 个 Provider 矩阵（启用/优先级/字段开关、Provider 专属选项）、元数据更新默认策略、按库覆盖（库选择 + Provider 门控、长尾字段如 `publisherTagNames` / `alternateTitleLabels` / `chineseConversion.update.fields`）、通知模板编辑器（编辑/渲染/发送）、任务（match/reset 触发 + 实时 SSE 事件流）、搜索试跑一键设元数据、阅读状态同步页（AniList / MAL / MangaBaka / Bangumi：标题或链接搜索、已关联台账、内联状态编辑）、离线 DB 下载进度，以及 PATCH 预览（增量语义：缺省=保持、`null`=清空；密码留空=不发送）。UI 跟随系统明暗主题，并支持顶栏手动切换（持久化在 `localStorage`）。后端把 `web/dist`（Docker 内为 `./ui`，可用 `KOMF_WEB_DIR` 覆盖）作为默认路由并 SPA fallback；未构建时纯 API 模式。Release 镜像会自动把 `web/dist` 打进 `/app/ui`。

## 与 Kotlin 版的差异

- **Stump 媒体服务器**（仅 Rust 扩展）：完整的 Stump 支持——GraphQL 客户端（API Key 认证，或账号密码 → JWT 换取）、GraphQL WebSocket 事件监听（`readEvents` 订阅 + 基于任务状态的批量窗口，一次扫描只产生一批匹配任务）、系列/书籍元数据更新（`SeriesMetadataInput` / `MediaMetadataInput`）、封面上传（`uploadSeriesThumbnailBase64` / `uploadMediaThumbnailBase64`）、系列标签（`setSeriesTags`）、书籍列表翻页、系列重置（系列元数据 + 系列标签 + 书级重置）。已知限制：Stump 的 `Series.tags` 是 Tag 对象（读取侧返回空标签）、`CreatedManySeries` 事件不带系列 id（记日志后忽略）、书级重置以 `updateMediaMetadata` 置空变通。
- **中文库工作流**（bangumi + `seriesTitleLanguage`）：bangumi provider 用 `name` + `name_cn` + 别名匹配，中文系列名可自动命中；配置 `postProcessing.seriesTitle: true` + `seriesTitleLanguage: zh` 后，系列主标题按语言从 provider 的 `titles` 数组选择（bangumi 的 `name_cn` 成为系列标题，如「无能的奈奈」）。
- **eHentai provider**（来自 [PR #284](https://github.com/Snd-R/komf/pull/284)）在 Rust 版可用；画廊搜索需要能访问 e-hentai.org（通常需代理）。PR 之上的扩展：可配置 `searchDomain`（`e-hentai` / `exhentai`，仅搜索请求域名）+ exhentai cookie 自动预热与 403 自动刷新（`ipbMemberId`/`ipbPassHash`）、`titlePriority`、`translatorKeywords`、`maleOnlyTagsFile`、参考实现 hentai-assistant 风格的系列标题 `titleTemplate`，以及 `gidOnlyMatch`（自动匹配仅做 gid 精准搜索：无 gid 或 gid 无结果都跳过，不回落普通标题相似度；links 匹配不受影响）。
- **Bangumi provider** 增强了内置 648 项标签白名单（源自 KomgaBangumi.user.js）、通过 `tagWhitelist`/`tagWhitelistFile` 配置额外白名单、动态标签计数阈值（3~35）+ 前 10 增强、搜索显示与匹配双向的 mediaType 过滤（库级覆盖优先）、`score:N` 标签解析为数值评分，以及尊重 `seriesTitleLanguage` 的 name_cn 处理。
- **Bangumi 离线数据源**（移植自 [BangumiKomga](https://github.com/kalxd/BangumiKomga) 的 `bangumi_archive`）：启用 `bangumi.archive.enabled` 后，应用后台下载 bangumi/Archive release（约 400+MB zip）并构建本地 SQLite（FTS5 trigram）索引 + mmap jsonlines。搜索与元数据解析离线优先（series/type/标签/mediaType 过滤、别名感知相似度复用、单行本 relations），索引未就绪或未命中时自动回退在线 API；封面始终走在线 API 获取。可配置 `dir`（缺省 `workDir/bangumi-archive`）与 `updateIntervalHours`（缺省 168）。
- **搜索标题提取**可按库配置（`searchTitleExtraction`）：括号/标题正则、作者分隔符、标题拆分符、符号归一与字符映射均可自定义，不再硬编码。
- **mylar `series.json` 导出**（Rust 扩展，移植自 [komga-mylar.py](https://github.com/dyphire/komga-mylar.py) 语义）：在 `metadataUpdate.<default|library>.<id>.updateModes` 加入 `MYLAR_SERIES_JSON` 后，每次元数据更新会把系列元数据导出为 mylar 格式 `series.json`（oneshot 为 `<url 中 zip 文件名>.oneshot.json`，写入 zip 所在目录）——publisher、标题、year（取 releaseDate 年份）、简介、mylar 分级（All/9+/12+/15+/17+/Adult）、总册数、mylar 状态（Continuing/Ended）、语言、阅读方向、发行日期、作者（role 小写）、链接、备选标题、体裁与标签。`mylarCovers: true` 额外下载系列封面（`cover.jpg` / `<zip 文件名>.cover.jpg`，已存在跳过）。`mylarOutputDir` 可重定向导出根目录（对应脚本的 `--output`；库根目录由应用内部从媒体服务器 API 自动获取，用于还原相对目录结构，无需配置）。路径支持 `${configDir}` 占位符（=配置目录，作为稳定的相对基准）。

## 鸣谢

- [EhTagTranslation/Database](https://github.com/EhTagTranslation/Database) —— eHentai provider 使用的标签翻译数据库
- [URenko/e-hentai-db](https://github.com/URenko/e-hentai-db) — eHentai provider 的离线 SQLite 转储（nightly） 离线归档数据源
- [bangumi/Archive](https://github.com/bangumi/Archive) —— Bangumi provider 离线数据源
