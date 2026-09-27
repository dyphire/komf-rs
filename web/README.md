# komf-webui

The `komf-rs` configuration WebUI (Vite + React + TS, no heavy UI framework, single `App.tsx` for maintainability).

- Develop: `npm install && npm run dev` (proxies `/api` to `127.0.0.1:8085`)
- Build: `npm run build` → `web/dist/`; the backend serves it at `/` via SPA fallback (path overridable with `KOMF_WEB_DIR`)
- Access: `http://<host>:8085/`
- PATCH semantics: omitted = keep, `null` = clear; password fields left empty are not sent (the backend never returns plaintext)
