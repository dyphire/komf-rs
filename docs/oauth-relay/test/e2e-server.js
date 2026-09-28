/*
 * E2E 测试辅助服务器（仅本地测试用，不属于交付物）：
 *   :8000 静态服务 docs/oauth-relay 目录（中转页）
 *   :8080 模拟 komf 实例回调端点，记录收到的完整 URL 并展示
 */
"use strict";
const http = require("http");
const fs = require("fs");
const path = require("path");

const ROOT = path.join(__dirname, "..");

http
  .createServer((req, res) => {
    let p;
    try {
      p = decodeURIComponent(new URL(req.url, "http://x").pathname);
    } catch (e) {
      p = "/";
    }
    if (p === "/") p = "/index.html";
    const file = path.resolve(ROOT, "." + p);
    if (!file.startsWith(ROOT)) {
      res.writeHead(403);
      return res.end("forbidden");
    }
    fs.readFile(file, (err, data) => {
      if (err) {
        res.writeHead(404);
        return res.end("not found: " + p);
      }
      const ct = {
        ".html": "text/html; charset=utf-8",
        ".js": "text/javascript; charset=utf-8",
        ".json": "application/json; charset=utf-8",
      }[path.extname(file)];
      res.writeHead(200, { "Content-Type": ct || "application/octet-stream" });
      res.end(data);
    });
  })
  .listen(8000, () => console.log("[e2e] relay server on http://localhost:8000"));

http
  .createServer((req, res) => {
    const line = `[callback] ${req.method} ${req.url}`;
    console.log(line);
    try {
      fs.appendFileSync(path.join(ROOT, "test", "callback.log"), line + "\n");
    } catch (e) {}
    res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
    res.end(
      '<html><body style="background:#0f1419;color:#7ee787;font:16px/1.6 monospace;padding:24px">' +
        "<h2 style='color:#e6edf3'>komf 实例回调已收到</h2>" +
        req.url.replace(/&/g, "&amp;").replace(/</g, "&lt;") +
        "</body></html>"
    );
  })
  .listen(8080, () =>
    console.log("[e2e] mock komf instance on http://localhost:8080")
  );
