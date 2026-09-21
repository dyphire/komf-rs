# Komga, Kavita and Stump Metadata Fetcher (Rust)

**English** | [简体中文](README.zh-CN.md)

This is the **Rust rewrite** of [komf](https://github.com/Snd-R/komf), a tool that fetches metadata and thumbnails for your digital comic book library. It automatically picks up added series in **Komga**, **Kavita** and **Stump** and updates their metadata, thumbnails and book-level data. You can also manually search, identify and match series — per series, per library, or for the whole library.

The Rust implementation lives in [`komf-rust/`](komf-rust/README.md) as a fully isolated Cargo workspace: separate dependencies, separate build, separate binary (`komf-app`). It does not read or modify the Kotlin implementation.

## Status

- Komga: REST API + SSE event listener (auto-update, notifications)
- Kavita: REST API + SignalR event listener (JWT auth, auto-refresh)
- Stump (Rust-only): GraphQL API + GraphQL WebSocket event listener (auto-update with job-based event batching, API-key or JWT auth)
- 12 metadata providers implemented, configurable per provider and per library
- Discord webhook + Apprise notifications with Velocity templates
- ComicInfo reading/writing, book ordering, score tags, reading direction override
- Config hot-reload (`PATCH /api/config`), job tracking, metadata search/identify/match/reset endpoints
- userscript compatible configuration UI

## Metadata providers

| Provider                | Search                                  | Series metadata | Book metadata |
| ----------------------- | --------------------------------------- | --------------- | ------------- |
| MangaUpdates            | ✅                                       | ✅               | —             |
| MyAnimeList             | ✅                                       | ✅               | ✅             |
| AniList                 | ✅                                       | ✅               | —             |
| MangaDex                | ✅                                       | ✅               | ✅             |
| BookWalker              | ✅                                       | ✅               | ✅             |
| Bangumi (bgm.tv)        | ✅                                       | ✅               | ✅             |
| ComicVine               | ✅                                       | ✅               | ✅             |
| YenPress                | ✅                                       | ✅               | ✅             |
| Viz                     | ✅                                       | ✅               | ✅             |
| Webtoons                | ✅                                       | ✅               | ✅             |
| MangaBaka               | ✅                                       | ✅               | —             |
| **eHentai** (Rust-only) | ✅                                       | ✅               | —             |
| ~~Kodansha~~            | placeholder (unsupported in Kotlin too) |                 |               |
| ~~Nautiljon~~           | placeholder (unsupported in Kotlin too) |                 |               |
| ~~Hentag~~              | placeholder (unsupported in Kotlin too) |                 |               |

Providers can be configured globally (`metadataProviders.defaultProviders`) or per library (`metadataProviders.libraryProviders`). Each provider supports priority, enable/disable, media type filtering (`MANGA`/`NOVEL`/`COMIC`/`WEBTOON`), author/artist role mapping, per-field series/book metadata toggles, and provider-specific options (e.g. `coverLanguages`, `tagsScoreThreshold`, `preferredLanguages`). Kodansha, Nautiljon and Hentag are recognized in the configuration for compatibility, but the Kotlin version maps them to `error("Unsupported")` and the Rust version simply does not register them — enabling them has no effect.

## Building

Requirements: [Rust](https://rustup.rs/) (stable toolchain).

```sh
cd komf-rust
cargo build --release # builds the release binary at target/release/komf-app
cargo test --workspace # run unit tests
```

## Running

The repository does not ship `application.yml` (to avoid committing real credentials); use the template instead:

```sh
cd komf-rust
cp application.example.yml application.yml # Linux/macOS
copy application.example.yml application.yml # Windows
# edit application.yml: Komga/Kavita/Stump credentials, providers, metadata update, ...
./komf-app [path to config] # path to application.yml or its directory
```

The template (`application.example.yml`) contains every option with inline comments; sensitive fields (Komga user/password/API key, e-hentai/exhentai cookies) default to empty/placeholder values.

If no path is given, the `KOMF_CONFIG_DIR` environment variable is used. If neither is set, the service starts with built-in defaults (HTTP on 8085 only, no media-server credentials); configuration changed via `PATCH /api/config` is written back to `./application.yml` (created if missing), and the database defaults to `./database.sqlite`.

Environment variables (same as the Kotlin version):

| Variable                                       | Description                                                 |
| ---------------------------------------------- | ----------------------------------------------------------- |
| `KOMF_KOMGA_BASE_URI`                          | Komga base URL                                              |
| `KOMF_KOMGA_USER` / `KOMF_KOMGA_PASSWORD`      | Komga basic auth                                            |
| `KOMF_KOMGA_API_KEY`                           | Komga API key (`X-API-Key` auth, takes precedence when set) |
| `KOMF_KAVITA_BASE_URI` / `KOMF_KAVITA_API_KEY` | Kavita base URL + API key                                   |
| `KOMF_STUMP_BASE_URI` / `KOMF_STUMP_API_KEY`   | Stump base URL + API key (`stump_` prefix, takes precedence when set) |
| `KOMF_STUMP_USER` / `KOMF_STUMP_PASSWORD`      | Stump account password (only used to exchange a JWT when no API key is set) |
| `KOMF_SERVER_PORT`                             | HTTP port (default 8085)                                    |
| `KOMF_LOG_LEVEL`                               | Log level (default INFO)                                    |
| `KOMF_DISCORD_WEBHOOKS`                        | Comma-separated Discord webhook URLs                        |
| `KOMF_APPRISE_URLS`                            | Comma-separated Apprise URLs                                |
| `KOMF_METADATA_PROVIDERS_MAL_CLIENT_ID`        | Required for MAL provider                                   |
| `KOMF_METADATA_PROVIDERS_COMIC_VINE_API_KEY`   | Required for ComicVine provider                             |
| `KOMF_METADATA_PROVIDERS_BANGUMI_TOKEN`        | Bangumi token (shows NSFW items)                            |

### Docker

```sh
vdocker run -d --name komf ghcr.io/dyphire/komf-rs:latest \
 -p 8085:8085 \
 -v /path/to/config:/config \ # 存放 application.yml 的目录
```

The image exposes `/config` as a volume (`KOMF_CONFIG_DIR=/config`), so place your `application.yml` there. The image is published to `ghcr.io/dyphire/komf-rs` (`latest` + version tags); to build locally instead, run `docker build -f komf-rust/docker/Dockerfile komf-rust -t komf-rust` and replace the image name with `komf-rust`.

## Configuration

The repository does not include an `application.yml`; all options are documented in the template [`komf-rust/application.example.yml`](komf-rust/application.example.yml) (every field with an inline comment and its code default value; sensitive fields — Komga user/password/API key, e-hentai/exhentai cookies — are empty placeholders).

To use it:

1. Copy the template to `application.yml` (see [Running](#running) for the exact command).
2. Edit `application.yml`: fill in your Komga/Kavita/Stump credentials and enable the providers you want; each option is explained inline.
3. Start the service with the config file (path argument or `KOMF_CONFIG_DIR`); without a config file it runs on built-in defaults.

Per-provider reference (e-hentai / bangumi) is kept in [`komf-rust/README.md`](komf-rust/README.md).

## Per-library configuration

Any metadata update option or provider can be scoped to a specific library by its id (Komga, Kavita or Stump library id) via `metadataUpdate.library.<libraryId>` and `metadataProviders.libraryProviders.<libraryId>` — see the commented placeholders in the template (`application.example.yml`).

## Metadata aggregation

By default, metadata is fetched from the first positive match in configured providers, in priority order. With `aggregate: true`, metadata from all providers is aggregated: a field is only taken from another provider if the previous one did not provide it.

## Notifications

If any webhook URLs are configured, webhooks are called after books are added. Message formats are customizable with Velocity templates placed in `templatesDirectory/discord` or `templatesDirectory/apprise`:

- Discord: `title.vm`, `title_url.vm`, `description.vm`, `footer.vm`, `field_<index>_name<_inline>.vm`, `field_<index>_value.vm`
- Apprise: `apprise_title.vm`, `apprise_body.vm`

For Docker deployments, templates go in the mounted `/config/discord` or `/config/apprise` directory.

## HTTP Endpoints

### Configuration

- `GET /api/config`, `PATCH /api/config` — read / update configuration (hot-reload)

### Jobs

- `GET /api/jobs`, `GET /api/jobs/all` (`DELETE`), `GET /api/jobs/{jobId}/events` (SSE)

### Metadata (`{media-server}` = `komga`, `kavita` or `stump`)

- `GET /api/{media-server}/metadata/providers` — enabled providers (optional `libraryId`)
- `GET /api/{media-server}/metadata/search?name=...` — search (optional `libraryId`)
- `GET /api/{media-server}/metadata/series-cover?providerSeriesId=...`
- `POST /api/{media-server}/metadata/identify` — set metadata from a provider:

```json
{
 "libraryId": "09TDSWK3Q0XRA",
 "seriesId": "07XF6HKAWHHV4",
 "provider": "MANGA_UPDATES",
 "providerSeriesId": "1"
}
```

- `POST /api/{media-server}/metadata/match/library/{libraryId}` — match all series in a library
- `POST /api/{media-server}/metadata/match/library/{libraryId}/series/{seriesId}` — match one series
- `POST /api/{media-server}/metadata/reset/library/{libraryId}` — reset all series metadata
- `POST /api/{media-server}/metadata/reset/library/{libraryId}/series/{seriesId}` — reset one series

### Media server

- `GET /api/{media-server}/media-server/connected`, `GET /api/{media-server}/media-server/libraries`

### Notifications

- `GET|POST /api/notifications/{discord,apprise}/{templates,send,render}`

### Legacy (no `/api` prefix, kept for compatibility)

- `/config`, `/{media-server}/{providers,search,identify,match,reset}`

### Health check

- `GET /` — returns `200` with body `komf-rs` when the service is up.
- Docker image `HEALTHCHECK` probes it via `wget -qO- http://127.0.0.1:8085/` (`--interval=30s --timeout=5s --start-period=15s --retries=3`), matching the Dockerfile.

## Web UI integration

The userscript let you configure komf and identify series directly from the Komga / Kavita web UI. They talk to the same configuration endpoints exposed by this Rust implementation.

- [Komf userscript](https://github.com/dyphire/komf-userscript)

## Differences from the Kotlin version

- **Stump media server** (Rust-only extension): full Stump support — GraphQL client with API-key auth (or password → JWT exchange), GraphQL WebSocket event listener (`readEvents` subscription with job-based batch window so one scan produces one batch of match jobs), series/book metadata updates (`SeriesMetadataInput` / `MediaMetadataInput`), cover upload (`uploadSeriesThumbnailBase64` / `uploadMediaThumbnailBase64`), series tags (`setSeriesTags`), paginated book listing, series reset (series metadata + series tags + book-level reset). Known limits: Stump's `Series.tags` are Tag objects (the read side returns empty tags), `CreatedManySeries` events carry no series ids (ignored with a log line), and book-level reset is emulated via `updateMediaMetadata` with empty fields. Implementation and test record: [`komf-rust/STUMP-SUPPORT.md`](komf-rust/STUMP-SUPPORT.md).
- **Chinese-library workflow** (bangumi + `seriesTitleLanguage`): the bangumi provider matches against `name` + `name_cn` + aliases, so Chinese series names match automatically; with `postProcessing.seriesTitle: true` + `seriesTitleLanguage: zh` the main series title is picked from the provider's `titles` array by language (bangumi's `name_cn` becomes the series title, e.g. 「无能的奈奈」).
- **eHentai provider** (from [PR #284](https://github.com/Snd-R/komf/pull/284)) is available in the Rust version; gallery search requires network access to e-hentai.org (often via proxy). Extensions over the PR: configurable `searchDomain` (`e-hentai` / `exhentai`, search-only) with automatic exhentai cookie warm-up and 403 auto-refresh (`ipbMemberId`/`ipbPassHash`), `titlePriority`, `translatorKeywords`, `maleOnlyTagsFile`, a style `titleTemplate` for the series title, and `gidOnlyMatch` (match does gid-precise search only: no gid or no gid result skips, without falling back to title similarity; links matching stays unaffected).
- **Bangumi provider** is enhanced with a built-in 154-entry tag whitelist (from KomgaBangumi.user.js), configurable extras via `tagWhitelist` / `tagWhitelistFile`, dynamic tag count threshold (3~35) + top-10 boost, mediaType filtering for both search display and matching (library-level override wins), `score:N` tag parsing into a numeric score, and name_cn handling that respects `seriesTitleLanguage`.
- **Bangumi offline archive** (ported from [BangumiKomga](https://github.com/kalxd/BangumiKomga) `bangumi_archive`): when `bangumi.archive.enabled` is on, the app downloads the bangumi/Archive release (~400+MB zip) in the background and builds a local SQLite (FTS5 trigram) index over mmap'd jsonlines. Search and metadata resolution are offline-first (series/type/tag/mediaType filters, alias-aware similarity reuse, 单行本 relations) with automatic fallback to the online API while the index is not ready or misses; covers are always fetched from the online API. Configure `dir` (default `workDir/bangumi-archive`) and `updateIntervalHours` (default 168).
- **Search title extraction** is configurable per library (`searchTitleExtraction`): bracket/title regex, author separators, title splitters, symbol normalization and character mappings can be customized instead of being hard-coded.
- **Mylar `series.json` export** (Rust extension, ported from [komga-mylar.py](https://github.com/dyphire/komga-mylar.py) semantics): add `MYLAR_SERIES_JSON` to `metadataUpdate.<default|library>.<id>.updateModes` to write a mylar-format `series.json` (oneshot: `<name>.oneshot.json`) into each series folder after metadata updates — publisher, title, year (from release date), summary, mylar age rating (All/9+/12+/15+/17+/Adult), total issues, mylar status (Continuing/Ended), language, reading direction, release date, authors, links, alternate titles, genres and tags. `mylarCovers: true` additionally downloads the series cover (`cover.jpg` / `<name>.cover.jpg`, skipped if present). `mylarOutputDir` redirects the export root (like the script's `--output`; the library root is fetched automatically from the media server API to restore the relative path structure). `${configDir}` expands to the config directory as a stable relative base.

## Acknowledgements

- [EhTagTranslation/Database](https://github.com/EhTagTranslation/Database) — tag translation database used by the eHentai provider
- [URenko/e-hentai-db](https://github.com/URenko/e-hentai-db) — offline SQLite dump (nightly) used by the eHentai provider offline archive
- [bangumi/Archive](https://github.com/bangumi/Archive) — offline data source used by the Bangumi provider offline archive
