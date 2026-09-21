# Komga 与 Kavita 元数据抓取器（Rust）

[English](README.md) | **简体中文**

这是 [komf](https://github.com/Snd-R/komf) 的 **Rust 重写版**，用于为你的数字漫画库获取元数据与缩略图。它能自动捕获 **Komga** 和 **Kavita** 中新添加的系列并更新元数据、缩略图和书籍级数据；也支持手动按系列、按库或全库进行搜索、识别与匹配。

Rust 实现位于 [`komf-rust/`](komf-rust/README.md)，是独立隔离的 Cargo workspace：独立依赖、独立构建、独立二进制（`komf-app`）。它不读取也不修改 Kotlin 实现。

## 状态

- Komga：REST API + SSE 事件监听（自动更新、通知）
- Kavita：REST API + SignalR 事件监听（JWT 认证、自动刷新）
- 12 个元数据 provider 已实现，支持按 provider 与按库配置
- Discord webhook + Apprise 通知（Velocity 模板）
- ComicInfo 读写、书籍排序、评分标签、阅读方向覆盖
- 配置热更新（`PATCH /api/config`）、任务跟踪、元数据搜索/识别/匹配/重置端点
- 用户脚本兼容的配置界面

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
cd komf-rust
cargo build --release # 构建 release 二进制到 target/release/komf-app
cargo test --workspace # 运行单元测试
```

## 运行

仓库不提供 `application.yml`（避免提交真实凭据）；请使用模板：

```sh
cd komf-rust
cp application.example.yml application.yml # Linux/macOS
copy application.example.yml application.yml # Windows
# 编辑 application.yml：Komga/Kavita 凭据、providers、元数据更新等
./komf-app [path to config] # application.yml 的路径或其所在目录
```

模板（`application.example.yml`）包含全部配置项与内联注释；敏感字段（Komga 账号/密码/API key、e-hentai/exhentai cookie）默认为空/占位。

未传路径时使用 `KOMF_CONFIG_DIR` 环境变量；两者都未设置时，服务以内置默认配置启动（仅 HTTP 8085，无 mediaserver 凭据）；通过 `PATCH /api/config` 修改的配置会写回 `./application.yml`（不存在则创建）；数据库默认 `./database.sqlite`。

环境变量（与 Kotlin 版一致）：

| 变量                                             | 说明                                       |
| ---------------------------------------------- | ---------------------------------------- |
| `KOMF_KOMGA_BASE_URI`                          | Komga 服务地址                               |
| `KOMF_KOMGA_USER` / `KOMF_KOMGA_PASSWORD`      | Komga basic auth                         |
| `KOMF_KOMGA_API_KEY`                           | Komga API key（`X-API-Key` 认证，设置后优先于账号密码） |
| `KOMF_KAVITA_BASE_URI` / `KOMF_KAVITA_API_KEY` | Kavita 地址 + API key                      |
| `KOMF_SERVER_PORT`                             | HTTP 端口（默认 8085）                         |
| `KOMF_LOG_LEVEL`                               | 日志级别（默认 INFO）                            |
| `KOMF_DISCORD_WEBHOOKS`                        | 逗号分隔的 Discord webhook URL                |
| `KOMF_APPRISE_URLS`                            | 逗号分隔的 Apprise URL                        |
| `KOMF_METADATA_PROVIDERS_MAL_CLIENT_ID`        | MAL provider 必需                          |
| `KOMF_METADATA_PROVIDERS_COMIC_VINE_API_KEY`   | ComicVine provider 必需                    |
| `KOMF_METADATA_PROVIDERS_BANGUMI_TOKEN`        | Bangumi token（显示 NSFW 条目）                |

### Docker

```sh
docker run -d --name komf ghcr.io/dyphire/komf-rs:latest \
 -p 8085:8085 \
 -v /path/to/config:/config \ # 存放 application.yml 的目录
```

镜像发布在 `ghcr.io/dyphire/komf-rs`（`latest` + 版本 tag），将 `/config` 暴露为卷（`KOMF_CONFIG_DIR=/config`），把你的 `application.yml` 放到那里即可。如需本地构建，执行 `docker build -f komf-rust/docker/Dockerfile komf-rust -t komf-rust` 并把镜像名换成 `komf-rust`。

## 配置

仓库不包含 `application.yml`；所有配置项都在模板 [`komf-rust/application.example.yml`](komf-rust/application.example.yml) 中说明（每个字段带内联注释与代码默认值；敏感字段——Komga 账号/密码/API key、e-hentai/exhentai cookie——为空占位）。

使用方法：

1. 将模板复制为 `application.yml`（具体命令见 [运行](#运行)）。
2. 编辑 `application.yml`：填写 Komga/Kavita 凭据并启用需要的 provider；每个选项都有行内说明。
3. 用配置文件启动服务（路径参数或 `KOMF_CONFIG_DIR`）；无配置文件时以内置默认运行。

e-hentai / bangumi 的 provider 参考见 [`komf-rust/README.md`](komf-rust/README.md)。

## 按库配置

任何元数据更新选项或 provider 都可以按库（Komga 或 Kavita 库 id）限定作用范围：`metadataUpdate.library.<libraryId>` 与 `metadataProviders.libraryProviders.<libraryId>`——参见模板（`application.example.yml`）中的注释占位。

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

### 任务

- `GET /api/jobs`、`GET /api/jobs/all`（`DELETE`）、`GET /api/jobs/{jobId}/events`（SSE）

### 元数据（`{media-server}` = `komga` 或 `kavita`）

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

### 旧版路由（无 `/api` 前缀，兼容保留）

- `/config`、`/{media-server}/{providers,search,identify,match,reset}`

### 健康检查

- `GET /` —— 服务正常时返回 `200`，响应体为 `komf-rs`。
- Docker 镜像 `HEALTHCHECK` 通过 `wget -qO- http://127.0.0.1:8085/` 探测（`--interval=30s --timeout=5s --start-period=15s --retries=3`），与 Dockerfile 一致。

## Web UI 集成

用户脚本可直接在 Komga / Kavita Web UI 中配置 komf 并识别系列。它们与本 Rust 实现暴露的配置端点通信。

- [Komf 用户脚本](https://github.com/dyphire/komf-userscript)

## 与 Kotlin 版的差异

- **eHentai provider**（来自 [PR #284](https://github.com/Snd-R/komf/pull/284)）在 Rust 版可用；画廊搜索需要能访问 e-hentai.org（通常需代理）。PR 之上的扩展：可配置 `searchDomain`（`e-hentai` / `exhentai`，仅搜索请求域名）+ exhentai cookie 自动预热与 403 自动刷新（`ipbMemberId`/`ipbPassHash`）、`titlePriority`、`translatorKeywords`、`maleOnlyTagsFile`、参考实现 hentai-assistant 风格的系列标题 `titleTemplate`，以及 `gidOnlyMatch`（自动匹配仅做 gid 精准搜索：无 gid 或 gid 无结果都跳过，不回落普通标题相似度；links 匹配不受影响）。
- **Bangumi provider** 增强了内置 154 项标签白名单（源自 KomgaBangumi.user.js）、通过 `tagWhitelist`/`tagWhitelistFile` 配置额外白名单、动态标签计数阈值（3~35）+ 前 10 增强、搜索显示与匹配双向的 mediaType 过滤（库级覆盖优先）、`score:N` 标签解析为数值评分，以及尊重 `seriesTitleLanguage` 的 name_cn 处理。
- **Bangumi 离线数据源**（移植自 [BangumiKomga](https://github.com/kalxd/BangumiKomga) 的 `bangumi_archive`）：启用 `bangumi.archive.enabled` 后，应用后台下载 bangumi/Archive release（约 400+MB zip）并构建本地 SQLite（FTS5 trigram）索引 + mmap jsonlines。搜索与元数据解析离线优先（series/type/标签/mediaType 过滤、别名感知相似度复用、单行本 relations），索引未就绪或未命中时自动回退在线 API；封面始终走在线 API 获取。可配置 `dir`（缺省 `workDir/bangumi-archive`）与 `updateIntervalHours`（缺省 168）。
- **搜索标题提取**可按库配置（`searchTitleExtraction`）：括号/标题正则、作者分隔符、标题拆分符、符号归一与字符映射均可自定义，不再硬编码。
- **mylar `series.json` 导出**（Rust 扩展，移植自 [komga-mylar.py](https://github.com/dyphire/komga-mylar.py) 语义）：在 `metadataUpdate.<default|library>.<id>.updateModes` 加入 `MYLAR_SERIES_JSON` 后，每次元数据更新会把系列元数据导出为 mylar 格式 `series.json`（oneshot 为 `<url 中 zip 文件名>.oneshot.json`，写入 zip 所在目录）——publisher、标题、year（取 releaseDate 年份）、简介、mylar 分级（All/9+/12+/15+/17+/Adult）、总册数、mylar 状态（Continuing/Ended）、语言、阅读方向、发行日期、作者（role 小写）、链接、备选标题、体裁与标签。`mylarCovers: true` 额外下载系列封面（`cover.jpg` / `<zip 文件名>.cover.jpg`，已存在跳过）。`mylarOutputDir` 可重定向导出根目录（对应脚本的 `--output`；库根目录由应用内部从媒体服务器 API 自动获取，用于还原相对目录结构，无需配置）。路径支持 `${configDir}` 占位符（=配置目录，作为稳定的相对基准）。

## 鸣谢

- [EhTagTranslation/Database](https://github.com/EhTagTranslation/Database) —— eHentai provider 使用的标签翻译数据库
- [URenko/e-hentai-db](https://github.com/URenko/e-hentai-db) — eHentai provider 的离线 SQLite 转储（nightly） 离线归档数据源
- [bangumi/Archive](https://github.com/bangumi/Archive) —— Bangumi provider 离线数据源
