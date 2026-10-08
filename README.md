# Komga, Kavita and Stump Metadata Fetcher (Rust)

**English** | [简体中文](README.zh-CN.md)

This is the **Rust implementation** of [komf](https://github.com/Snd-R/komf), a tool that fetches metadata and thumbnails for your digital comic book library. It automatically picks up added series in **Komga**, **Kavita** and **Stump** and updates their metadata, thumbnails and book-level data. You can also manually search, identify and match series — per series, per library, or for the whole library.

## Status

- Komga: REST API + SSE event listener (auto-update, notifications)
- Kavita: REST API + SignalR event listener (JWT auth, auto-refresh)
- Stump (Rust-only): GraphQL API + GraphQL WebSocket event listener (auto-update with job-based event batching, API-key or JWT auth)
- 12 metadata providers implemented, configurable per provider and per library
- Discord webhook + Apprise notifications with Velocity templates
- ComicInfo reading/writing, book ordering, score tags, reading direction override
- Config hot-reload (`PATCH /api/config`), job tracking, metadata search/identify/match/reset endpoints
- userscript compatible configuration UI
- Built-in WebUI workbench **(Rust-only)**: 12-provider matrix, per-library overrides, notification template editor, jobs with live SSE progress, search trial with one-click identify, tracker page (AniList / MAL / Bangumi reading-status sync), offline DB download, light/dark theme
- **OAuth login for MAL / AniList / MangaBaka / Bangumi (Rust-only)** : server-side OAuth2 with a shared client + official relay page — no per-instance callback registration needed; login/logout/status in the Providers page, token auto-refresh and SQLite persistence
- **Reading-list tracker sync (Rust-only)** : AniList / MyAnimeList / MangaBaka / Bangumi reading-status sync with a WebUI page and `/api/tracker/*` endpoints — search by title or platform link, `tracked` marking, state read and status/score/progress push

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
cargo build --release # builds the release binary at target/release/komf-app
cargo test --workspace # run unit tests
```

OAuth `client_secret` values are **build-time injected** (not runtime env vars): set
`KOMF_OAUTH_ANILIST_CLIENT_SECRET` / `KOMF_OAUTH_MAL_CLIENT_SECRET` /
`KOMF_OAUTH_BANGUMI_CLIENT_SECRET` when building and they are compiled into the
binary via `option_env!`. Released binaries/images already carry them; see
[`docs/oauth-relay/README.md`](docs/oauth-relay/README.md).

## Running

The repository does not ship `application.yml` (to avoid committing real credentials); use the template instead:

```sh
cp examples/application.example.yml application.yml # Linux/macOS
copy examples/application.example.yml application.yml # Windows
# edit application.yml: Komga/Kavita/Stump credentials, providers, metadata update, ...
./komf-app [path to config] # path to application.yml or its directory
```

The template (`application.example.yml`) contains every option with inline comments; sensitive fields (Komga user/password/API key, e-hentai/exhentai cookies) default to empty/placeholder values.

If no path is given, the `KOMF_CONFIG_DIR` environment variable is used. If neither is set, the service starts with built-in defaults (HTTP on 8085 only, no media-server credentials); configuration changed via `PATCH /api/config` is written back to `./application.yml` (created if missing), and the database defaults to `./database.sqlite`.

Environment variables (same as the Kotlin version):

| Variable                                       | Description                                                                                                                                                                                                                                                                                 |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `KOMF_KOMGA_BASE_URI`                          | Komga base URL                                                                                                                                                                                                                                                                              |
| `KOMF_KOMGA_USER` / `KOMF_KOMGA_PASSWORD`      | Komga basic auth                                                                                                                                                                                                                                                                            |
| `KOMF_KOMGA_API_KEY`                           | Komga API key (`X-API-Key` auth, takes precedence when set)                                                                                                                                                                                                                                 |
| `KOMF_KAVITA_BASE_URI` / `KOMF_KAVITA_API_KEY` | Kavita base URL + API key                                                                                                                                                                                                                                                                   |
| `KOMF_STUMP_BASE_URI` / `KOMF_STUMP_API_KEY`   | Stump base URL + API key (`stump_` prefix, takes precedence when set)                                                                                                                                                                                                                       |
| `KOMF_STUMP_USER` / `KOMF_STUMP_PASSWORD`      | Stump account password (only used to exchange a JWT when no API key is set)                                                                                                                                                                                                                 |
| `KOMF_SERVER_PORT`                             | HTTP port (default 8085, restart required)                                                                                                                                                                                                                                                  |
| `KOMF_SERVER_BIND`                             | HTTP bind address (default `0.0.0.0`; use `127.0.0.1` for local-only, restart required)                                                                                                                                                                                                     |
| `KOMF_LOG_LEVEL`                               | Log level (default INFO)                                                                                                                                                                                                                                                                    |
| `KOMF_DISCORD_WEBHOOKS`                        | Comma-separated Discord webhook URLs                                                                                                                                                                                                                                                        |
| `KOMF_APPRISE_URLS`                            | Comma-separated Apprise URLs                                                                                                                                                                                                                                                                |
| `KOMF_METADATA_PROVIDERS_MAL_CLIENT_ID`        | Required for MAL provider                                                                                                                                                                                                                                                                   |
| `KOMF_METADATA_PROVIDERS_COMIC_VINE_API_KEY`   | Required for ComicVine provider                                                                                                                                                                                                                                                             |
| `KOMF_METADATA_PROVIDERS_BANGUMI_TOKEN`        | Bangumi token (shows NSFW items)                                                                                                                                                                                                                                                            |
| `KOMF_AUTH_KEY`                                | Access key for sensitive operations (optional, strongly recommended for public exposure): when set, sensitive requests from outside the local network must present it (local/LAN bypass); `GET /version` and `GET /api/health` stay public; legacy `KOMF_WEBUI_KEY` still works as fallback |
| `KOMF_WEB_DIR`                                 | WebUI static directory override (`web/dist` -> `ui` lookup order)                                                                                                                                                                                                                           |
| `KOMF_AUTH_FORCE_REMOTE`                       | Debug only: `1` treats every client as remote to force the key path                                                                                                                                                                                                                         |
| `KOMF_TAG_TRANSLATION`                         | Env-only switch (no config option): built-in tag translation (EN→ZH tags mapping when seriesTitleLanguage is Chinese; not the ehentai-specific translator) is enabled by default; set to `0` or `false` (case-insensitive) to disable                                                       |

### Docker

```sh
docker run -d --name komf ghcr.io/dyphire/komf-rs:latest \
 -p 8085:8085 \
 -v /path/to/config:/config \ # 存放 application.yml 的目录
```

The image exposes `/config` as a volume (`KOMF_CONFIG_DIR=/config`), so place your `application.yml` there. The image is published to `ghcr.io/dyphire/komf-rs` (`latest` + version tags); to build locally instead, run `docker build -f docker/Dockerfile . -t komf-rs`.

## Configuration

The repository does not include an `application.yml`; all options are documented in the template [`application.example.yml`](examples/application.example.yml) (every field with an inline comment and its code default value; sensitive fields — Komga user/password/API key, e-hentai/exhentai cookies — are empty placeholders).

To use it:

1. Copy the template to `application.yml` (see [Running](#running) for the exact command).
2. Edit `application.yml`: fill in your Komga/Kavita/Stump credentials and enable the providers you want; each option is explained inline.
3. Start the service with the config file (path argument or `KOMF_CONFIG_DIR`); without a config file it runs on built-in defaults.

## Security

The service ships with an **optional key-based access gate** — **setting `KOMF_AUTH_KEY` is strongly recommended when exposed to the public internet**. Set `KOMF_AUTH_KEY` (legacy `KOMF_WEBUI_KEY` still works as fallback) and **sensitive** requests from **outside the local network** must present the key — local/loopback and LAN (RFC1918 / link-local / IPv6 ULA) clients are always allowed through without one. Non-sensitive `GET /version` and `GET /api/health` stay public for version/health checks. Unauthorized sensitive `/api/*` requests get `401`; page/asset requests get an inline login page (key input, styled to match the WebUI). A successful `POST /api/auth/login` sets an HttpOnly session cookie (`komf_auth`); `POST /api/auth/logout` clears it. A request passes with either credential: the cookie (derivation uses SHA-1 over the key with a fixed salt, compared in constant time) or an `Authorization: Bearer <base64(key)>` header (the key is base64-encoded for transport, decoded and constant-time compared server-side, keeping the raw key out of request headers/access logs) for scripts/API clients. `KOMF_AUTH_FORCE_REMOTE=1` (debug only) forces the key path for every client.

Without the key gate the service has **no built-in authentication**: anyone who can reach the HTTP port can read the (credential-masked) configuration and change it via `PATCH /api/config`, and use the metadata endpoints. Treat it like a database admin panel:

- **Local-only (recommended for home servers):** bind to loopback so only the machine itself can connect:
  
  ```yaml
  server:
    bind: 127.0.0.1
    port: 8085
  ```
  
  or `KOMF_SERVER_BIND=127.0.0.1` (restart required). Access the WebUI via SSH tunnel if needed.

- **LAN/VPS exposure:** put a reverse proxy with authentication in front and firewall the raw port. Examples:
  
  ```nginx
  # nginx: basic auth
  server {
    listen 80; server_name komf.example.com;
    location / {
      auth_basic "komf"; auth_basic_user_file /etc/nginx/.htpasswd;
      proxy_pass http://127.0.0.1:8085;
    }
  }
  ```
  
  ```caddy
  # Caddy: basic auth (one line)
  komf.example.com {
    basicauth { admin $2a$14$... }
    reverse_proxy 127.0.0.1:8085
  }
  ```

## Per-library configuration

Any metadata update option or provider can be scoped to a specific library by its id (Komga, Kavita or Stump library id) via `metadataUpdate.library.<libraryId>` and `metadataProviders.libraryProviders.<libraryId>` — see the commented placeholders in the template (`application.example.yml`).

## Metadata aggregation

By default, metadata is fetched from the first positive match in configured providers, in priority order. With `aggregate: true`, metadata from all providers is aggregated: a field is only taken from another provider if the previous one did not provide it. With `mergeGenres: true` / `mergeTags: true` (aggregate mode only), the series'/books' existing genres/tags on the media server are also merged into the aggregated result, so provider data does not wipe them.

## Notifications

If any webhook URLs are configured, webhooks are called after books are added. Message formats are customizable with Velocity templates placed in `templatesDirectory/discord` or `templatesDirectory/apprise`:

- Discord: `title.vm`, `title_url.vm`, `description.vm`, `footer.vm`, `field_<index>_name<_inline>.vm`, `field_<index>_value.vm`
- Apprise: `apprise_title.vm`, `apprise_body.vm`

For Docker deployments, templates go in the mounted `/config/discord` or `/config/apprise` directory.

## HTTP Endpoints

### Configuration

- `GET /api/config`, `PATCH /api/config` — read / update configuration (hot-reload)

### Authentication (active when `KOMF_AUTH_KEY` is set)

- `POST /api/auth/login` — `{"key":"..."}`: `204` + `komf_auth` cookie on success, `401` on failure
- `POST /api/auth/logout` — clears the auth cookie
- Protected requests present a credential (either): `Cookie: komf_auth=<from login>`, or `Authorization: Bearer <base64(KOMF_AUTH_KEY)>` (decoded and verified server-side)
- Public without auth (remote): `GET /version`, `GET /api/health`; all other sensitive requests require auth
- Unauthorized sensitive `/api/*` requests return `401`; other paths return the built-in login page; local/LAN clients bypass the gate

### Offline database download

- `POST /api/update-manga-baka-db`, `POST /api/update-book-walker-db` — trigger an offline DB download; streams NDJSON progress events (`ProgressEvent` / `FinishedEvent` / `ErrorEvent`) until the stream closes. Manual trigger; additionally, `updateIntervalHours` (per provider, default 24, 0 = manual only) enables a background scheduled update: MangaBaka checks the official sha1 checksum (a checksum-identical local DB is skipped), BookWalker does a HEAD request comparing `Last-Modified`. Downloads are atomic (temp file + rename) — on failure the old database is kept and the check retries after 15 minutes.

### Jobs

- `GET /api/jobs`, `GET /api/jobs/all` (`DELETE`), `GET /api/jobs/{jobId}/events` (SSE, per-job)
- `GET /api/jobs/events` (SSE, global firehose, optional `?ids=a,b,c` filter) — one connection observes all job activity; frames reuse the per-job event names plus `JobCreatedEvent` / `JobFinishedEvent` lifecycle frames, each `data` flattened with `jobId` + `seriesId`; replays current RUNNING jobs on connect, never closes on single-job completion, slow clients drop frames and catch up

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

### Cover redirect

- `GET /api/cover/redirect?url=<encoded>` — `302` + `Referrer-Policy: no-referrer` to the target cover URL. The WebUI uses it to render search-result covers: provider cover CDNs (MangaDex and others) return a placeholder banner for any `Referer` outside their allowlist, but serve the real cover when no `Referer` is sent — the redirect lets the browser drop the Referer and load the actual cover. The target host is validated against a provider-cover-domain allowlist (open-redirect / SSRF protection); other hosts get `400`. The search API's `imageUrl` stays a direct link, so third-party server-side consumers are unaffected and may optionally use this endpoint.

### OAuth login (`{provider}` = `anilist`, `mal`, `bangumi` or `mangabaka`)

Server-side OAuth2 login for metadata providers, using a **shared client + official relay page** (no per-instance callback registration). See [`docs/oauth-relay/README.md`](docs/oauth-relay/README.md) for the mechanism, client-secret injection and deployment notes.

- `GET /api/oauth/{provider}/start` — `302` redirect to the provider's authorization page (state carries the instance callback URL; PKCE for anilist/mal/mangabaka). Optional `?redirect_path_prefix=/prefix` prefixes the instance callback path, for reverse proxies that mount the callback inside their own namespace (e.g. kmrs under `/api/v1/komf`) or sub-path deployments. Optional `?user=<key>` attributes the login to a caller-defined user identity (see multi-user below)
- `GET /api/oauth/{provider}/callback` — OAuth callback (via the relay page): exchanges the code, stores the token in `<configDir>/oauth.sqlite` under the user identity recorded at `start`, then `302` to `/?oauth=success` (or `/?oauth=error&message=...`)
- `GET /api/oauth/{provider}/status` — `200` JSON `{"logged_in":bool,"username":string|null}`; optional `?user=<key>` or `X-Tracker-User` header
- `POST /api/oauth/{provider}/logout` — `204`, clears the stored token for the resolved user

Once logged in, the provider requests are authenticated with the OAuth bearer token (takes precedence over the manual `bangumiToken` / `KOMF_METADATA_PROVIDERS_MAL_CLIENT_ID` options); expired tokens are auto-refreshed when a refresh token exists, otherwise the login is cleared and the provider falls back to anonymous. The tracker endpoints return `401` whenever the login is lost at request time (expired and unrefreshable, no secret, refresh rejected, or the provider itself rejects the token with `401` — which also clears the stored login), so reading-status sync never silently operates without the user's account. The WebUI Providers page shows login status and offers login/logout per provider.

Multi-user: OAuth tokens and tracker state are isolated per caller-defined user identity (`user_key`, `[A-Za-z0-9_-]{1,64}`). `start` takes it as `?user=` (plain links), `status`/`logout` as `?user=` or `X-Tracker-User` header; the callback recovers it from the server-side pending record, so neither the OAuth `state` JSON nor the relay pages change. Requests without a key resolve to the built-in `default` user — komf's own WebUI and the metadata providers always operate on `default`, so single-user behavior is unchanged and existing databases migrate to `default` on first start. A media server in front (e.g. kmrs) passes its own user IDs to give every user an independent tracker account per platform.

### Tracker (`{provider}` = `anilist`, `mal`, `bangumi` or `mangabaka`; requires OAuth login)

Reading-list sync for the four platforms, backed by the OAuth login above. All endpoints are scoped by caller identity: pass the user with the `X-Tracker-User` header (absent = `default`; see multi-user above).

- `GET /api/tracker/{provider}/search?name=...&nsfw=...` — search the platform; each item carries `tracked` (whether it is already in the user's list). `nsfw` defaults to `true` and is filtered only when `false`. `name` may also be a platform entry link — `anilist.co/manga/{id}`, `myanimelist.net/manga/{id}`, `bgm.tv`/`bangumi.tv`/`subject/{id}`, `mangabaka.org/{id}` — with or without a scheme; the backend resolves it directly to the single item.
- `GET /api/tracker/{provider}/state?trackId=...` — the current list entry (`status`, `score`, chapters/volumes read, start/finish dates, totals); not in the list returns an empty state (Bangumi maps the "not collected" `404` to an empty state).
- `POST /api/tracker/{provider}/update` — push a state update:

```json
{ "trackId": "70345", "score": 8, "status": "reading", "lastReadChapter": 12, "lastReadVolume": 1, "startReadDate": "2026-09-01", "finishReadDate": null }
```

`status` is one of `reading`, `planning`, `completed`, `paused`, `dropped`, `rereading`; omit the field to keep the current value.

Notes: `tracked` is user-scoped — AniList via `mediaListEntry`, MAL via `my_list_status` (details fetched concurrently), Bangumi via the user's collection list, MangaBaka via the user's library (batched `GET /v1/my/library/batch`). Bangumi search falls back to the legacy `GET /search/subject/{q}?type=1` when the v0 API fails (the metadata matching provider has the same fallback). AniList scores follow the account's `mediaListOptions.scoreFormat` (POINT_10 accounts read/write 0–10; other formats 0–100). MangaBaka ratings are 0–100 and its `plan_to_read`/`considering` map to `planning`; updates create the library entry with `POST` when it does not exist yet, `PATCH` otherwise.

- `GET /api/tracker/links` — the local ledger of items linked through this komf instance: every successful `update` upserts `{provider, trackId, title, coverUrl, url, updatedAt}` (newest first) into the `tracker_links` table in `<configDir>/oauth.sqlite`, scoped to the resolved user. `update` accepts optional `title` / `coverUrl` fields that are not sent to the platform but are stored for this list.

In the WebUI Tracker page, the ledger is shown under **Linked** (mutually exclusive with search results); picking an item expands the state form inline under it, and clicking it again collapses it.

### Legacy (no `/api` prefix, kept for compatibility)

- `/config`, `/{media-server}/{providers,search,identify,match,reset}`

### Health check

- `GET /` — serves the built-in WebUI (`web/dist`); without a build it returns `404`.
- `GET /version` — returns `200` JSON `{"name":"komf-rs","version":"..."}` for structured checks.
- `GET /api/health` — returns `200` JSON `{"status":"ok","name":"komf-rs","version":"..."}` for structured health checks under the API prefix.
- All responses carry the `X-Komf-Version` header.
- Docker image `HEALTHCHECK` probes it via `wget -qO- http://127.0.0.1:8085/api/health` (`--interval=30s --timeout=5s --start-period=15s --retries=3`), matching the Dockerfile.

## Web UI integration

The userscript let you configure komf and identify series directly from the Komga / Kavita web UI. They talk to the same configuration endpoints exposed by this Rust implementation.

- [Komf userscript](https://github.com/dyphire/komf-userscript)

### Built-in config WebUI

This repo also ships a built-in configuration UI (`web/`, Vite + React + TS):

```sh
cd web && npm install && npm run build # outputs web/dist
./komf-app                             # serves it at http://localhost:8085/
npm run dev                            # dev mode (proxies /api to 127.0.0.1:8085)
```

It is a full workbench: media-server connections and libraries, the 12-provider matrix (enable/priority/per-field toggles, provider-specific options), metadata-update defaults, per-library overrides (library selector with provider gates, long-tail fields such as `publisherTagNames` / `alternateTitleLabels` / `chineseConversion.update.fields`), notification template editor (edit/render/send), jobs with match/reset triggers and a live SSE event stream, a search trial with one-click identify, a tracker page (AniList / MAL / MangaBaka / Bangumi reading-status sync: search by title or link, linked-item ledger, inline state editing), offline DB download with progress, and a PATCH preview (incremental semantics: omitted = keep, `null` = clear; empty passwords are not sent). The UI follows the system light/dark theme with a manual override (topbar toggle, persisted in `localStorage`). The backend serves `web/dist` (or `./ui` in Docker, overridable via `KOMF_WEB_DIR`) as the default route with SPA fallback; without a build the service runs API-only. Release Docker images build `web/dist` into `/app/ui` automatically.

## Differences from the Kotlin version

- **Stump media server** (Rust-only extension): full Stump support — GraphQL client with API-key auth (or password → JWT exchange), GraphQL WebSocket event listener (`readEvents` subscription with job-based batch window so one scan produces one batch of match jobs), series/book metadata updates (`SeriesMetadataInput` / `MediaMetadataInput`), cover upload (`uploadSeriesThumbnailBase64` / `uploadMediaThumbnailBase64`), series tags (`setSeriesTags`), paginated book listing, series reset (series metadata + series tags + book-level reset). Known limits: Stump's `Series.tags` are Tag objects (the read side returns empty tags), `CreatedManySeries` events carry no series ids (ignored with a log line), and book-level reset is emulated via `updateMediaMetadata` with empty fields.
- **Chinese-library workflow** (bangumi + `seriesTitleLanguage`): the bangumi provider matches against `name` + `name_cn` + aliases, so Chinese series names match automatically; with `postProcessing.seriesTitle: true` + `seriesTitleLanguage: zh` the main series title is picked from the provider's `titles` array by language (bangumi's `name_cn` becomes the series title, e.g. 「无能的奈奈」).
- **eHentai provider** (from [PR #284](https://github.com/Snd-R/komf/pull/284)) is available in the Rust version; gallery search requires network access to e-hentai.org (often via proxy). Extensions over the PR: configurable `searchDomain` (`e-hentai` / `exhentai`, search-only) with automatic exhentai cookie warm-up and 403 auto-refresh (`ipbMemberId`/`ipbPassHash`), `titlePriority`, `translatorKeywords`, `maleOnlyTagsFile`, a style `titleTemplate` for the series title, and `gidOnlyMatch` (match does gid-precise search only: no gid or no gid result skips, without falling back to title similarity; links matching stays unaffected).
- **Bangumi provider** is enhanced with a built-in 648-entry tag whitelist (from KomgaBangumi.user.js), configurable extras via `tagWhitelist` / `tagWhitelistFile`, dynamic tag count threshold (3~35) + top-10 boost, mediaType filtering for both search display and matching (library-level override wins), `score:N` tag parsing into a numeric score, and name_cn handling for authors/publishers controlled by `bangumi.archive.staffChineseNames` (default `false`).
- **Bangumi offline archive** (ported from [BangumiKomga](https://github.com/kalxd/BangumiKomga) `bangumi_archive`): when `bangumi.archive.enabled` is on, the app downloads the bangumi/Archive release (~400+MB zip) in the background and builds a local SQLite (FTS5 trigram) index over mmap'd jsonlines. Search and metadata resolution are offline-first (series/type/tag/mediaType filters, alias-aware similarity reuse, 单行本 relations) with automatic fallback to the online API while the index is not ready or misses; covers are always fetched from the online API. Configure `dir` (default `workDir/bangumi-archive`) and `updateIntervalHours` (default 168). Author/publisher Chinese names (`staffChineseNames`, see above) also lives in this section.
- **MangaBaka / BookWalker local DB auto-update** (mirrors the bangumi/ehentai archive pattern): each provider's `updateIntervalHours` (default 24, 0 = manual only) schedules a background update. MangaBaka compares the official sha1 checksum and re-downloads only when changed; BookWalker issues a HEAD request and re-downloads only when `Last-Modified` changes. Both downloads are fault-tolerant: they write to a temp file and atomically rename over the old DB, so a transient failure keeps the previous database usable and the check retries after 15 minutes.
- **Search title extraction** is configurable per library (`searchTitleExtraction`): bracket/title regex, author separators, title splitters, symbol normalization and character mappings can be customized instead of being hard-coded.
- **Mylar `series.json` export** (Rust extension, ported from [komga-mylar.py](https://github.com/dyphire/komga-mylar.py) semantics): add `MYLAR_SERIES_JSON` to `metadataUpdate.<default|library>.<id>.updateModes` to write a mylar-format `series.json` (oneshot: `<name>.oneshot.json`) into each series folder after metadata updates — publisher, title, year (from release date), summary, mylar age rating (All/9+/12+/15+/17+/Adult), total issues, mylar status (Continuing/Ended), language, reading direction, release date, authors, links, alternate titles, genres and tags. `mylarCovers: true` additionally downloads the series cover (`cover.jpg` / `<name>.cover.jpg`, skipped if present). `mylarOutputDir` redirects the export root (like the script's `--output`; the library root is fetched automatically from the media server API to restore the relative path structure). `${configDir}` expands to the config directory as a stable relative base.

## Acknowledgements

- [EhTagTranslation/Database](https://github.com/EhTagTranslation/Database) — tag translation database used by the eHentai provider
- [URenko/e-hentai-db](https://github.com/URenko/e-hentai-db) — offline SQLite dump (nightly) used by the eHentai provider offline archive
- [bangumi/Archive](https://github.com/bangumi/Archive) — offline data source used by the Bangumi provider offline archive
