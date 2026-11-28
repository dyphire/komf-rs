# AGENTS.md

Guidance for AI coding agents working in this repository.

## Project Overview

komf-rust: Rust implementation of [komf](https://github.com/Snd-R/komf) — a metadata/thumbnail fetcher for digital comic libraries (**Komga**, **Kavita**, **Stump**). It watches for new series via server events (SSE / SignalR / GraphQL WS), fetches metadata from 12 providers (MangaUpdates, MAL, AniList, MangaDex, BookWalker, Bangumi, ComicVine, YenPress, Viz, Webtoons, MangaBaka, eHentai), and writes it back to the media server. Ships with a built-in WebUI, Discord/Apprise notifications, OAuth login (MAL / AniList / MangaBaka / Bangumi), and reading-list tracker sync.

## Repository Layout

```
crates/
  komf-api-models    # API request/response & config DTOs (serde), shared with WebUI contract
  komf-core          # config model, metadata providers (providers/), trackers, OAuth, rate limiter
  komf-mediaserver   # Komga/Kavita/Stump clients, event listeners, ComicInfo, metadata merge/map/post-process
  komf-notifications # Discord + Apprise, Velocity template rendering
  komf-app           # binary: axum server, routes/, app context, main.rs
web/                 # React 18 + TypeScript + Vite WebUI (no test/lint tooling configured)
docker/              # Docker build assets
docs/oauth-relay/    # OAuth relay page assets
examples/            # sample configs
Cross.toml           # cross-compilation config (musl targets)
```

Dependency direction: `komf-app` → `komf-mediaserver` / `komf-notifications` → `komf-core` → `komf-api-models`. Keep it acyclic.

## Build & Test

```sh
cargo build --release          # release binary at target/release/komf-app
cargo test --workspace         # unit tests
cargo build --locked           # CI builds with --locked; keep Cargo.lock committed
cargo fmt && cargo clippy      # run before submitting changes
```

Frontend (in `web/`):

```sh
npm run dev      # vite dev server
npm run build    # tsc --noEmit && vite build (type check + bundle)
```

## Conventions

- Rust 2021 edition, stable toolchain. Async runtime: tokio; HTTP: axum 0.7 + reqwest (rustls, no native-tls); errors: `thiserror` in libraries, `anyhow` in the app crate; logging: `tracing`.
- **Shared dependencies live in `[workspace.dependencies]`** in the root `Cargo.toml` — add new common deps there and reference with `dep.workspace = true`; don't pin versions per-crate.
- Provider and media-type behavior must stay compatible with the Kotlin komf (see README for parity notes). Kodansha/Nautiljon/Hentag are intentionally unimplemented placeholders — enabling them is a no-op; do not "fix" this.
- README is bilingual: `README.md` (English) and `README.zh-CN.md` (简体中文). When changing user-facing behavior, update both.
- `mimalloc` is the global allocator in the app binary; keep it unless profiling shows otherwise.
- Config supports hot-reload via `PATCH /api/config` — new config fields should remain backward compatible (serde defaults) and flow through `komf-api-models` → `komf-core` → routes.

## Secrets & CI

- OAuth client secrets are **compile-time injected** via `option_env!` (env vars `KOMF_OAUTH_{ANILIST,MAL,BANGUMI,MANGABAKA}_CLIENT_SECRET`) in `release.yml` / `docker-test.yml`. Never hardcode or commit secrets.
- CI (`.github/workflows/`): `test.yml` runs tests; `release.yml` builds binaries for `x86_64/aarch64-unknown-linux-musl` via `cross`; `docker-test.yml` builds test images.

## When Modifying Code

- New metadata provider: implement under `komf-core/src/providers/`, register it, and update the provider tables in both READMEs and the WebUI matrix if applicable.
- New HTTP endpoint: add route under `komf-app/src/routes/`, put request/response types in `komf-api-models`, and mirror them in the WebUI (`web/src/`) types.
- Event listener changes (Komga SSE / Kavita SignalR / Stump GraphQL WS) live in `komf-mediaserver`; be careful with reconnection and auth-refresh logic.
