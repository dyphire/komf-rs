//! e-hentai-db 离线数据源（URenko/e-hentai-db nightly SQLite dump）。
//!
//! 数据文件：`e-hentai.db`（SQLite，gallery/gid_tid/tag/torrent 表，字段与官方 gdata
//! JSON 一致；title/title_jpn 为 gdata 原始 HTML 实体编码，读取后由 ehentai.rs 的
//! `EHentaiBook::from_archive_row` 复用 html_unescape 解码）。
//!
//! 启动时应用内自动下载 `e-hentai.db.zstd`（GitHub nightly release）→ zstd 流式解压到
//! 本地 → 只读打开；周期检查 release 资产 Last-Modified 决定是否更新。
//!
//! 查询能力：
//! - gid 精准（PRIMARY KEY，O(1)）+ 标签联查 → GalleryRow；
//! - 标题搜索：主库内嵌 FTS5 预分词索引表 `gallery_fts`
//!   （unicode61 + Lucene 分析链：t2s/全半角/小写/折叠 + CJK bigram/unigram；
//!   contentless + detail=full——不存原文但保留位置，支持 ORDER BY rank（bm25）；
//!   372 万行构建 ~150s；查询毫秒级，简繁交叉命中 + 装饰后缀渐进回退），
//!   更新流程在 tmp 库内一并构建后单次 rename（数据与索引原子替换，无降级窗口）；
//!   由 meta 记录 fts_version 自动触发重建；未就绪/纯符号查询时降级主库 LIKE 扫描。
//!   短查询（<3 字符）走 FTS 前缀匹配，不再触发 LIKE
//!
//! 未就绪 / 构建中 / 未命中 → 调用方回退在线 gdata（与 bangumi archive 模式一致）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use futures::StreamExt;
use komf_api_models::config::DownloadProgress;
use serde_json::json;

use crate::providers::ProviderError;

/// 缺省下载 URL（URenko nightly release 的 SQLite dump，zstd 压缩）。
pub(crate) const DEFAULT_EHENTAI_ARCHIVE_URL: &str =
    "https://github.com/URenko/e-hentai-db/releases/download/nightly/e-hentai.db.zstd";

/// 旧版独立 FTS 库文件名（v6 起 FTS 并入主库，遗留文件在迁移重建成功后删除）。
const LEGACY_FTS_DB_FILE: &str = "e-hentai-fts.db";

/// FTS 索引格式版本：DDL/存储格式变更时递增，meta 版本不符自动重建。
/// 6 = FTS 并入主库（单文件）+ detail=full（支持 rank；SQLite 仅 full 有非零 bm25）。
const FTS_VERSION: u32 = 6;

/// 单条 gallery 的完整行（含标签联查结果）。
#[derive(Debug, Clone)]
pub(crate) struct GalleryRow {
    pub gid: i32,
    pub token: String,
    pub title: String,
    pub title_jpn: String,
    pub category: String,
    pub thumb: String,
    pub uploader: Option<String>,
    /// epoch 秒
    pub posted: i64,
    pub file_count: i64,
    pub file_size: i64,
    pub expunged: bool,
    /// 预留（上游 dump 字段；查询已过滤 removed，replaced 供未来处理旧画廊标记）。
    #[allow(dead_code)]
    pub removed: bool,
    #[allow(dead_code)]
    pub replaced: bool,
    /// 原始字符串（如 "4.54"），由调用方按 gdata 语义 round。
    pub rating: String,
    /// `namespace:name` 形式（与 gdata tags 一致）。
    pub tags: Vec<String>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn file_stamp(path: &Path) -> Option<(u64, u64)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((md.len(), mtime))
}

/// 只读 SQLite 查询句柄。空闲超过阈值时 `release_if_idle` 执行 `PRAGMA shrink_memory`
/// 归还未使用页面缓存（对齐 bangumi archive 的空闲释放语义）。
pub(crate) struct EHentaiArchiveStore {
    conn: Mutex<rusqlite::Connection>,
    last_used: AtomicU64,
    last_release: AtomicU64,
    idle_release_ms: u64,
}

impl EHentaiArchiveStore {
    pub fn open(db_path: &Path, idle_release_secs: u64) -> Result<Self, ProviderError> {
        use rusqlite::OpenFlags;
        let conn = rusqlite::Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| ProviderError::message(format!("ehentai archive open failed: {e}")))?;
        conn.pragma_update(None, "query_only", "ON")
            .map_err(|e| ProviderError::message(format!("ehentai archive pragma failed: {e}")))?;
        // 32MB 页面缓存上限（负值 = KB），限制常驻内存
        conn.pragma_update(None, "cache_size", -32000)
            .map_err(|e| ProviderError::message(format!("ehentai archive pragma failed: {e}")))?;
        Ok(Self {
            conn: Mutex::new(conn),
            last_used: AtomicU64::new(now_ms()),
            last_release: AtomicU64::new(now_ms()),
            idle_release_ms: idle_release_secs.saturating_mul(1000),
        })
    }

    /// 表结构有效（gallery 表存在且非空）。
    pub fn validate(&self) -> bool {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM gallery", [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    /// gid 精准查询（过滤 expunged/removed）；未命中 → None。
    pub fn get_by_gid(&self, gid: i32) -> Option<GalleryRow> {
        self.get_by_gids(&[gid]).into_iter().next()
    }

    /// 批量 gid 查询（过滤 expunged/removed），返回顺序与输入一致（未命中跳过）。
    pub fn get_by_gids(&self, gids: &[i32]) -> Vec<GalleryRow> {
        if gids.is_empty() {
            return Vec::new();
        }
        self.touch();
        let conn = self.conn.lock().unwrap();
        // gid 为 INTEGER PRIMARY KEY → rowid=gid，保序用 rowid IN (...) + CASE 排序
        let placeholders: Vec<String> = (1..=gids.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT gid, token, title, title_jpn, category, thumb, uploader, posted, \
             filecount, filesize, expunged, removed, replaced, rating \
             FROM gallery WHERE gid IN ({}) AND expunged = 0 AND removed = 0 \
             ORDER BY CASE gid {} END",
            placeholders.join(","),
            gids.iter()
                .enumerate()
                .map(|(i, g)| format!("WHEN {g} THEN {i}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let params: Vec<rusqlite::types::Value> = gids
            .iter()
            .map(|g| rusqlite::types::Value::Integer(*g as i64))
            .collect();
        let mut rows = match stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok(GalleryRow {
                gid: r.get(0)?,
                token: r.get(1)?,
                title: r.get(2)?,
                title_jpn: r.get(3)?,
                category: r.get(4)?,
                thumb: r.get(5)?,
                uploader: r.get(6)?,
                posted: r.get(7)?,
                file_count: r.get(8)?,
                file_size: r.get(9)?,
                expunged: r.get::<_, i64>(10)? != 0,
                removed: r.get::<_, i64>(11)? != 0,
                replaced: r.get::<_, i64>(12)? != 0,
                rating: r.get(13)?,
                tags: Vec::new(),
            })
        }) {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
        let mut out: Vec<GalleryRow> = Vec::new();
        while let Some(Ok(row)) = rows.next() {
            out.push(row);
        }
        drop(rows);
        // 标签联查（批量：gid IN (...)）
        if let Ok(mut tag_stmt) = conn.prepare(&format!(
            "SELECT gt.gid, t.name FROM tag t JOIN gid_tid gt ON t.id = gt.tid \
             WHERE gt.gid IN ({})",
            placeholders.join(",")
        )) {
            if let Ok(iter) = tag_stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok((r.get::<_, i32>(0)?, r.get::<_, String>(1)?))
            }) {
                let mut by_gid: std::collections::HashMap<i32, Vec<String>> =
                    std::collections::HashMap::new();
                for item in iter.flatten() {
                    by_gid.entry(item.0).or_default().push(item.1);
                }
                for row in &mut out {
                    if let Some(tags) = by_gid.get(&row.gid) {
                        row.tags = tags.clone();
                    }
                }
            }
        }
        out
    }

    /// 主库 LIKE 降级搜索（FTS 未就绪或查询无有效 token 时）。返回 gid 列表（最新优先）。
    fn like_search_gids(
        &self,
        query: &str,
        limit: usize,
        category_filter: &[String],
        uploader_filter: &[String],
    ) -> Vec<i32> {
        let mut sql = String::from(
            "SELECT gid FROM gallery WHERE (title LIKE ?1 OR title_jpn LIKE ?1) \
             AND expunged = 0 AND removed = 0",
        );
        let mut params: Vec<String> = vec![format!("%{query}%")];
        if !category_filter.is_empty() {
            let ph: Vec<String> = (params.len() + 1..)
                .take(category_filter.len())
                .map(|i| format!("?{i}"))
                .collect();
            sql.push_str(&format!(" AND category IN ({})", ph.join(",")));
            params.extend(category_filter.iter().cloned());
        }
        if !uploader_filter.is_empty() {
            let ph: Vec<String> = (params.len() + 1..)
                .take(uploader_filter.len())
                .map(|i| format!("?{i}"))
                .collect();
            sql.push_str(&format!(" AND uploader IN ({})", ph.join(",")));
            params.extend(uploader_filter.iter().cloned());
        }
        sql.push_str(" ORDER BY posted DESC LIMIT ?");
        params.push(limit.to_string());
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mut rows = match stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            r.get::<_, i32>(0)
        }) {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        while let Some(Ok(g)) = rows.next() {
            out.push(g);
        }
        out
    }

    /// FTS 索引表存在且非空（gallery_fts rowid 即 gid）。
    pub fn has_fts(&self) -> bool {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT rowid FROM gallery_fts LIMIT 1", [], |_| Ok(()))
            .is_ok()
    }

    /// 查询侧分析链 token 渐进前缀 AND：先全量 AND，未命中逐层丢尾部 token 重试
    /// （komga 系列名常带归档标题没有的后缀：卷数/系列/话数）。
    /// 短查询（<3 字符）自动改走 FTS 前缀匹配（"tok" *）：召回更宽，且不再降级
    /// 主库 LIKE 全表扫（372 万行 × 2 列，单次秒级）。
    /// 索引为 contentless（不存原文）+ detail=full，ORDER BY rank（bm25）可用。
    /// token 为空（纯符号查询）或无 FTS 表时返回 None，调用方降级 LIKE。
    pub fn search_gids(&self, query: &str, limit: usize) -> Option<Vec<i32>> {
        let tokens = crate::util::search_analyze(query);
        if tokens.is_empty() {
            return None;
        }
        let tokens = &tokens[..tokens.len().min(50)];
        // 短查询（1~2 字符）：精确 token AND 召回过窄，改前缀匹配扩大召回
        // （"愛" → 命中 愛/愛さ/愛される... 开头的所有词项）。
        let prefix = query.chars().count() < 3;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT rowid FROM gallery_fts WHERE gallery_fts MATCH ?1 ORDER BY rank LIMIT ?2",
            )
            .ok()?;
        // 渐进前缀 AND：全量未命中逐层丢尾部 token 重试，不低于 droppable_floor
        //（拉丁整词子句永不放宽、前缀至少两 token，避免单词查询冲爆候选）
        let floor = if tokens.len() <= 1 {
            1
        } else {
            crate::util::droppable_floor(tokens)
        };
        for n in (floor..=tokens.len()).rev() {
            let match_expr = fts_fragments_and_expr(&tokens[..n], prefix);
            let gids: Vec<i32> = stmt
                .query_map(rusqlite::params![match_expr, limit as i64], |r| {
                    r.get::<_, i32>(0)
                })
                .ok()?
                .filter_map(|r| r.ok())
                .collect();
            if !gids.is_empty() {
                return Some(gids);
            }
        }
        Some(Vec::new())
    }

    /// 周期强制释放：每 idle 秒无条件 `PRAGMA shrink_memory` 归还未使用页面缓存
    /// （CAS 防并发；供周期任务调用）。与旧"空闲释放"的关键区别：不再要求
    /// "距上次查询 ≥ idle"——密集查询持续刷新 last_used 时旧逻辑永不释放。
    pub fn release_periodic(&self) {
        let idle = self.idle_release_ms;
        if idle == 0 {
            return;
        }
        let now = now_ms();
        let last = self.last_release.load(Ordering::Relaxed);
        if now.saturating_sub(last) < idle {
            return;
        }
        if self
            .last_release
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            let _ = self
                .conn
                .lock()
                .unwrap()
                .execute_batch("PRAGMA shrink_memory;");
            tracing::debug!("ehentai archive: released sqlite page cache");
        }
    }

    fn touch(&self) {
        self.last_used.store(now_ms(), Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// FTS5 预分词索引（unicode61 + Lucene 分析链）
// ---------------------------------------------------------------------------

/// FTS5 AND 表达式（ehentai 专用）：索引无位置数据（detail=column），FTS5
/// 禁止多 token phrase——分析链产出的 token 若含 unicode61 分隔符（"3.0"、
/// "re:zero" 中的 `.`/`:`）会被引号短语语法整体报错。因此按 unicode61 的
/// token 字符集（L*/N*，is_alphanumeric 近似）把每个 token 切成单 token
/// 片段再 AND。`prefix=true`（短查询）时每个片段改用 FTS5 前缀匹配
/// （"frag" *），扩大召回。
fn fts_fragments_and_expr(tokens: &[String], prefix: bool) -> String {
    tokens
        .iter()
        .flat_map(|t| {
            t.split(|c: char| !c.is_alphanumeric())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        })
        .map(|t| {
            let t = t.replace('"', "\"\"");
            if prefix {
                format!("\"{t}\" *")
            } else {
                format!("\"{t}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// 在已打开的目标库连接内（重新）构建 FTS5 索引表 `gallery_fts`。
/// unicode61 + 预分词：写入前经 Lucene 链分析（t2s/全半角/小写/折叠，
/// 索引侧 unigram+bigram+ngram），查询侧同链 token AND（见 search_gids）。
/// content=''：不存 title/title_jpn 原文（gid 放 rowid），只存倒排；
/// detail=full：保留位置数据（bm25/rank 必需，SQLite 仅 full 支持非零 rank）。
/// 调用场景：① 更新流程在 tmp 库内构建（连同主库一起 rename，原子替换、
/// 无降级窗口）；② 启动迁移/版本不符时在现库内原地重建。
/// 分批按 gid 范围插入（分批事务 + 分析产物仅本批驻留内存）。
pub(crate) fn build_fts(conn: &rusqlite::Connection) -> Result<(), ProviderError> {
    // 同名重建（而非 delete-all + 重插）：DDL/分词器变更时最干净；旧 FTS 文件
    // 在迁移场景直接随 DROP 消失。FTS5 外部内容表才有 delete-all 缺陷，本表
    // contentless，普通 DROP 安全。
    conn.execute_batch("DROP TABLE IF EXISTS gallery_fts;")
        .map_err(|e| ProviderError::message(format!("ehentai fts drop failed: {e}")))?;
    conn.execute_batch(
        // detail=full：保留位置数据——bm25/rank 依赖位置计算词频（实测 SQLite
        // 3.50 下 detail=column/none 的 contentless 表 rank 恒为 0，仅 full 生效）。
        // content=''：不存 title/title_jpn 原文（gid 放 rowid），只存倒排——
        // 原文在主库已有，FTS 只负责命中 gid，体积增量可控。
        "CREATE VIRTUAL TABLE gallery_fts USING fts5(title, title_jpn, tokenize='unicode61', detail=full, content='');",
    )
    .map_err(|e| ProviderError::message(format!("ehentai fts create table failed: {e}")))?;
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM gallery", [], |r| r.get(0))
        .map_err(|e| ProviderError::message(format!("ehentai fts count failed: {e}")))?;
    if total == 0 {
        return Err(ProviderError::message("ehentai fts: source gallery empty"));
    }
    tracing::info!("ehentai fts building index for {total} rows...");
    // gid 为 INTEGER PRIMARY KEY → rowid=gid，按 gid 分批
    // （分批事务 + 分析后插入；分析产物仅本批驻留内存）
    const BATCH: i64 = 50_000;
    let mut last_gid: i64 = -1;
    let mut done: i64 = 0;
    while done < total {
        let batch_result = (|| -> Result<(i64, i64), ProviderError> {
            conn.execute_batch("BEGIN")
                .map_err(|e| ProviderError::message(format!("ehentai fts begin failed: {e}")))?;
            let result = (|| -> Result<(i64, i64), ProviderError> {
                let rows: Vec<(i64, String, Option<String>)> = {
                    let mut stmt = conn
                        .prepare(
                            "SELECT gid, title, title_jpn FROM gallery \
                             WHERE gid > ?1 ORDER BY gid LIMIT ?2",
                        )
                        .map_err(|e| {
                            ProviderError::message(format!("ehentai fts select failed: {e}"))
                        })?;
                    let mapped = stmt
                        .query_map(rusqlite::params![last_gid, BATCH], |r| {
                            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                        })
                        .map_err(|e| {
                            ProviderError::message(format!("ehentai fts select failed: {e}"))
                        })?;
                    mapped.filter_map(|r| r.ok()).collect()
                };
                // keyset 游标取本批（gallery 按 gid 升序）最大 gid——不能用
                // MAX(rowid) FROM gallery_fts（contentless FTS 不支持该查询）。
                let page_last = rows.last().map(|r| r.0).unwrap_or(last_gid);
                let mut ins = conn
                    .prepare("INSERT INTO gallery_fts(rowid, title, title_jpn) VALUES (?1,?2,?3)")
                    .map_err(|e| {
                        ProviderError::message(format!("ehentai fts insert failed: {e}"))
                    })?;
                for (gid, title, title_jpn) in &rows {
                    let t = crate::util::index_analyze_terms(title).join(" ");
                    let tj = title_jpn
                        .as_deref()
                        .map(|s| crate::util::index_analyze_terms(s).join(" "));
                    ins.execute(rusqlite::params![gid, t, tj]).map_err(|e| {
                        ProviderError::message(format!("ehentai fts insert failed: {e}"))
                    })?;
                }
                Ok((rows.len() as i64, page_last))
            })();
            match result {
                Ok(v) => {
                    conn.execute_batch("COMMIT").map_err(|e| {
                        ProviderError::message(format!("ehentai fts commit failed: {e}"))
                    })?;
                    Ok(v)
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    Err(e)
                }
            }
        })();
        let (n, page_last) = batch_result?;
        if n == 0 {
            break;
        }
        done += n;
        last_gid = page_last;
        if done % (BATCH * 4) == 0 || done >= total {
            tracing::info!("ehentai fts index progress {done}/{total}");
        }
    }
    tracing::info!("ehentai fts index ready ({done} rows)");
    Ok(())
}

// ---------------------------------------------------------------------------
// 下载 / 解压 / meta
// ---------------------------------------------------------------------------

/// meta 文件路径（缺省 workDir/ehentai/archive_meta.json）：记录 asset_stamp + fts_built_for。
fn meta_path(work_dir: Option<&Path>) -> PathBuf {
    work_dir
        .map(|d| d.join("ehentai").join("archive_meta.json"))
        .unwrap_or_else(|| PathBuf::from("archive_meta.json"))
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct ArchiveMeta {
    asset_stamp: Option<String>,
    last_checked: Option<String>,
    /// FTS 构建对应的主库文件指纹（len, mtime secs）。
    fts_built_for: Option<(u64, u64)>,
    /// FTS 索引格式版本（FTS_VERSION），格式变更时强制重建。
    fts_version: Option<u32>,
}

fn meta_read(meta: &Path) -> ArchiveMeta {
    std::fs::read_to_string(meta)
        .ok()
        // 容忍 UTF-8 BOM（外部工具/Windows 编辑器可能写入；serde 不解析 BOM 字符）
        .map(|raw| raw.trim_start_matches('\u{feff}').to_string())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn meta_write(
    meta: &Path,
    stamp: Option<&str>,
    fts_built_for: Option<(u64, u64)>,
    fts_version: Option<u32>,
) {
    if let Some(dir) = meta.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let m = ArchiveMeta {
        asset_stamp: stamp.map(String::from),
        last_checked: Some(chrono::Utc::now().to_rfc3339()),
        fts_built_for,
        fts_version,
    };
    let _ = std::fs::write(meta, json!(m).to_string());
}

/// HEAD 请求拿 Last-Modified（GitHub release asset 上传时间，更新时变化）。
async fn fetch_remote_stamp(
    client: &reqwest::Client,
    url: &str,
) -> Result<Option<String>, ProviderError> {
    let resp = client.head(url).send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    Ok(resp
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(String::from))
}

/// 下载 zstd → 流式解压到 db_path（先写 .tmp 再 rename）；校验 SQLite 有效后才替换。
/// 支持断点续传：`.zstd.tmp` 已存在时用 `Range: bytes=N-` 续传剩余部分
/// （GitHub release asset 支持 Range；大文件中断后重试只续传剩余字节，不重下 1GB+）。
/// `on_progress(written_total, expected_total)`：下载字节进度回调（续传时 written 为累计值）。
async fn download_and_extract(
    client: &reqwest::Client,
    url: &str,
    db_path: &Path,
    on_progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
) -> Result<(), ProviderError> {
    use std::io::Write;
    let zst_tmp = db_path.with_extension("db.zstd.tmp");
    let db_tmp = db_path.with_extension("db.tmp");
    let existing = std::fs::metadata(&zst_tmp).map(|m| m.len()).unwrap_or(0);
    let mut need_download = true;
    let mut req = client.get(url);
    if existing > 0 {
        // 已有部分文件：HEAD 拿远端大小，已完整则跳过下载直接解压（避免 416 续传死循环）
        if let Some(total) = fetch_content_length(client, url).await? {
            if existing >= total {
                need_download = false;
            }
        }
        if need_download {
            req = req.header(reqwest::header::RANGE, format!("bytes={existing}-"));
        }
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        return Err(ProviderError::message(format!(
            "ehentai archive download HTTP {}",
            resp.status()
        )));
    }
    let resumed = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    // ① 流式写压缩文件（1GB+，不能整包载入内存；206 续传时追加写，200 从头覆盖）
    if need_download {
        let mut out = if resumed {
            std::fs::OpenOptions::new().append(true).open(&zst_tmp)
        } else {
            std::fs::File::create(&zst_tmp)
        }
        .map_err(|e| ProviderError::message(format!("ehentai archive tmp write failed: {e}")))?;
        // Content-Length / Content-Range 在消费流之前读取（bytes_stream 会 move resp）
        let content_length = resp.content_length();
        let content_range_total = if resumed {
            content_range_total(&resp)
        } else {
            None
        };
        // 进度总大小：206 时 Content-Length 是剩余字节，完整大小 = existing + 剩余
        let progress_total: Option<u64> = if resumed {
            content_range_total.or_else(|| content_length.map(|l| existing + l))
        } else {
            content_length
        };
        let mut written: u64 = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let c = chunk.map_err(|e| {
                ProviderError::message(format!("ehentai archive download interrupted: {e}"))
            })?;
            out.write_all(&c).map_err(|e| {
                ProviderError::message(format!("ehentai archive tmp write failed: {e}"))
            })?;
            written += c.len() as u64;
            if let Some(on_progress) = on_progress {
                on_progress(existing + written, progress_total.unwrap_or(0));
            }
        }
        out.flush().map_err(|e| {
            ProviderError::message(format!("ehentai archive tmp flush failed: {e}"))
        })?;
        // 完整性校验：
        // 200（全量）：written == Content-Length
        // 206（续传）：Content-Length 是剩余字节，完整大小 = existing + written，
        //   与 Content-Range 头的 total（远端全量）比较；解析不到 total 时跳过（解压兜底）
        if resumed {
            if let Some(total) = content_range_total {
                if existing + written != total {
                    return Err(ProviderError::message(format!(
                        "ehentai archive download incomplete ({} + {} != {})",
                        existing, written, total
                    )));
                }
            }
        } else if let Some(cl) = content_length {
            if written != cl {
                return Err(ProviderError::message(format!(
                    "ehentai archive download incomplete ({} != {})",
                    written, cl
                )));
            }
        }
    }
    // ② zstd 流式解压；解压失败说明压缩缓存损坏（大小一致也可能内容损坏），
    //    删除 zst_tmp 让重试从头下载，避免反复续传坏数据
    {
        let input = std::fs::File::open(&zst_tmp)
            .map_err(|e| ProviderError::message(format!("ehentai archive tmp open failed: {e}")))?;
        let mut decoder = zstd::stream::read::Decoder::new(input).map_err(|e| {
            let _ = std::fs::remove_file(&zst_tmp);
            ProviderError::message(format!("ehentai archive zstd decode failed: {e}"))
        })?;
        let mut out = std::fs::File::create(&db_tmp).map_err(|e| {
            ProviderError::message(format!("ehentai archive tmp write failed: {e}"))
        })?;
        std::io::copy(&mut decoder, &mut out).map_err(|e| {
            let _ = std::fs::remove_file(&zst_tmp);
            ProviderError::message(format!("ehentai archive extract failed: {e}"))
        })?;
        out.flush().map_err(|e| {
            ProviderError::message(format!("ehentai archive tmp flush failed: {e}"))
        })?;
    }
    let _ = std::fs::remove_file(&zst_tmp);
    // ③ 校验 SQLite 有效
    let store = EHentaiArchiveStore::open(&db_tmp, 0)?;
    if !store.validate() {
        drop(store);
        let _ = std::fs::remove_file(&db_tmp);
        return Err(ProviderError::message(
            "ehentai archive db invalid (gallery table empty)",
        ));
    }
    drop(store);
    // ④ db.tmp 就绪：rename 由调用方 commit_archive 执行
    //    （Windows 下目标 db 可能被现有连接占用，需先释放连接再 rename）
    Ok(())
}

/// 解析 `Content-Range: bytes start-end/total` 的 total（远端全量大小）。
fn content_range_total(resp: &reqwest::Response) -> Option<u64> {
    let v = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)?
        .to_str()
        .ok()?;
    let total = v.rsplit('/').next()?.trim();
    total.parse().ok()
}

/// HEAD 请求拿远端 Content-Length（用于续传前判定已完整/计算剩余）。
async fn fetch_content_length(
    client: &reqwest::Client,
    url: &str,
) -> Result<Option<u64>, ProviderError> {
    let resp = client.head(url).send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    Ok(resp.content_length())
}

/// 提交已下载并校验的 db.tmp：
/// 1) 释放旧 store 连接（Windows 下 rename 目标若被打开会拒绝访问）；
/// 2) rename 短重试（最多 5 次 × 500ms，等待并发搜索请求释放 Arc）；
/// 3) 重开新 store 并重新入槽（就绪）。失败返回 false（调用方走快速重试）。
async fn commit_archive(
    store: &Arc<RwLock<Option<Arc<EHentaiArchiveStore>>>>,
    ready: &Arc<AtomicBool>,
    current: &mut Option<Arc<EHentaiArchiveStore>>,
    db_tmp: &Path,
    db_path: &Path,
    idle_secs: u64,
) -> bool {
    // 释放旧连接（Arc 计数归零即关闭 SQLite 句柄）；提交期间搜索短暂回退在线
    if let Some(old) = store.write().unwrap().take() {
        drop(old);
    }
    ready.store(false, Ordering::SeqCst);
    *current = None;
    let mut renamed = false;
    for _ in 0..5 {
        if std::fs::rename(db_tmp, db_path).is_ok() {
            renamed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    if !renamed {
        let _ = std::fs::remove_file(db_tmp);
        tracing::warn!("ehentai archive db rename failed; retrying later");
        return false;
    }
    if let Some(s) = EHentaiArchiveStore::open(db_path, idle_secs)
        .ok()
        .filter(|s| s.validate())
    {
        let s = Arc::new(s);
        *store.write().unwrap() = Some(s.clone());
        *current = Some(s);
        ready.store(true, Ordering::SeqCst);
        true
    } else {
        tracing::warn!("ehentai archive db invalid after commit");
        false
    }
}

/// 下载带指数退避重试（公共工具）：首次尝试 + 3 次重试（5s/30s/120s）。
/// 断点续传保证重试只续传剩余字节，不重新下载整个文件。
async fn download_with_retry(
    client: &reqwest::Client,
    url: &str,
    db_path: &Path,
    on_progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
) -> Result<(), ProviderError> {
    crate::util::download::with_retry("ehentai archive download", || {
        download_and_extract(client, url, db_path, on_progress)
    })
    .await
}

// ---------------------------------------------------------------------------
// Service —— 对齐 BangumiArchiveService 的后台生命周期
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct EHentaiArchiveService {
    store: Arc<RwLock<Option<Arc<EHentaiArchiveStore>>>>,
    ready: Arc<AtomicBool>,
    /// 主库内嵌 FTS 索引（gallery_fts）是否可用；false 时搜索降级 LIKE。
    fts_ready: Arc<AtomicBool>,
    /// 打开现有 db 的启动任务是否已完成（无论成败）。周期更新循环等待该标志，
    /// 避免打开任务与更新流程竞争同一 db 文件。
    opened: Arc<AtomicBool>,
    /// 周期自动更新循环是否已启动（防重复 spawn）。
    auto_update_started: Arc<AtomicBool>,
    http_client: reqwest::Client,
    dl_client: reqwest::Client,
    db_path: PathBuf,
    meta: PathBuf,
    url: String,
    idle_secs: u64,
    /// 更新互斥：下载/构建期间忽略新的触发（复用进行中的进度流）。
    download_in_progress: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<tokio::sync::watch::Sender<Option<DownloadProgress>>>>>,
}

impl EHentaiArchiveService {
    /// 启动后台任务（非阻塞）。db 文件缺省 workDir/ehentai/e-hentai.db。
    /// 只创建服务并加载现有数据；周期自动更新由编排方（app_context）统一调用
    /// `start_auto_update` 启动，服务本身不自行调度。
    pub fn start(
        config: &crate::config::EHentaiArchiveConfig,
        http_client: reqwest::Client,
        work_dir: Option<&Path>,
    ) -> Arc<Self> {
        let store: Arc<RwLock<Option<Arc<EHentaiArchiveStore>>>> = Arc::new(RwLock::new(None));
        let ready = Arc::new(AtomicBool::new(false));
        let fts_ready = Arc::new(AtomicBool::new(false));
        let opened = Arc::new(AtomicBool::new(false));
        let db_path = config
            .db_file
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| work_dir.map(|d| d.join("ehentai").join("e-hentai.db")))
            .unwrap_or_else(|| PathBuf::from("e-hentai.db"));
        let meta = meta_path(work_dir);
        let url = config
            .url
            .clone()
            .unwrap_or_else(|| DEFAULT_EHENTAI_ARCHIVE_URL.to_string());
        let idle_secs = config.idle_release_secs.unwrap_or(0);
        // 1GB+ 下载必须用长超时 client（共享 client 60s 总超时必断；失败回退共享 client）。
        // HEAD 检查等小请求仍用 http_client。
        let dl_client = crate::util::download::long_download_client()
            .map_err(|e| {
                tracing::warn!("ehentai archive long client build failed: {e}; falling back")
            })
            .unwrap_or_else(|_| http_client.clone());
        let svc = Arc::new(Self {
            store: store.clone(),
            ready: ready.clone(),
            fts_ready: fts_ready.clone(),
            opened: opened.clone(),
            auto_update_started: Arc::new(AtomicBool::new(false)),
            http_client: http_client.clone(),
            dl_client,
            db_path: db_path.clone(),
            meta: meta.clone(),
            url: url.clone(),
            idle_secs,
            download_in_progress: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Mutex::new(None)),
        });

        // 后台空闲释放：每 min(idle,60)s 检查一次，空闲超时释放 SQLite 页面缓存
        if idle_secs > 0 {
            let svc_idle = svc.clone();
            tokio::spawn(async move {
                let tick = std::time::Duration::from_secs(idle_secs.min(60).max(1));
                loop {
                    tokio::time::sleep(tick).await;
                    if let Some(s) = svc_idle.store.read().unwrap().as_ref() {
                        s.release_periodic();
                    }
                }
            });
        }

        tokio::spawn(async move {
            if let Some(dir) = db_path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            // 清理上次"下载完成 → commit rename"之间崩溃/被杀残留的临时文件
            // （db.tmp 可达数 GB）。正常路径下 commit_archive 会 rename 走它，
            // download_and_extract 开头也会清；但 up-to-date 跳过时两者都不执行，
            // 而启动时刻这些文件绝不可能是"正在使用"（更新流程尚未开始）。
            for tmp in [
                db_path.with_extension("db.tmp"),
                db_path.with_extension("db.zstd.tmp"),
                // 旧版独立 FTS 库的构建残留（v6 起 FTS 并入主库，该文件已无用）
                db_path
                    .parent()
                    .map(|d| d.join(LEGACY_FTS_DB_FILE))
                    .unwrap_or_else(|| PathBuf::from(LEGACY_FTS_DB_FILE))
                    .with_extension("db.tmp"),
            ] {
                if tmp.exists() {
                    if let Err(e) = std::fs::remove_file(&tmp) {
                        tracing::warn!("ehentai archive: remove stale {}: {e}", tmp.display());
                    } else {
                        tracing::info!("ehentai archive: removed stale {}", tmp.display());
                    }
                }
            }
            // 尝试打开现有 db（无条件：状态徽标/手动更新需要就绪）。
            // FTS 就绪与否不阻塞 ready——未就绪时搜索降级 LIKE（spawn 内迁移/重建）。
            let mut fts_ok = false;
            if let Ok(s) = EHentaiArchiveStore::open(&db_path, idle_secs) {
                if s.validate() {
                    fts_ok = s.has_fts() && meta_read(&meta).fts_version == Some(FTS_VERSION);
                    *store.write().unwrap() = Some(Arc::new(s));
                    ready.store(true, Ordering::SeqCst);
                    tracing::info!("ehentai archive ready (existing db)");
                }
            }
            opened.store(true, Ordering::SeqCst);
            if !fts_ok {
                // FTS 缺失/格式过旧（含旧版独立 fts 文件布局）→ 后台原地重建：
                // 主库单独开一个可写连接（读 store 的只读连接不冲突），
                // 成功后置 fts_ready、写 meta、删遗留的独立 fts 文件。
                // 搜索在重建完成前降级 LIKE（v6 起 FTS 在库内，与主库同生命周期，
                // 不再有 ATTACH/rename 撞车问题）。
                let db_path2 = db_path.clone();
                let meta2 = meta.clone();
                let fts_ready2 = fts_ready.clone();
                tokio::spawn(async move {
                    let db_path3 = db_path2.clone();
                    let built = crate::util::heavy_pool::spawn_heavy(move || {
                        let conn = rusqlite::Connection::open(&db_path3).map_err(|e| {
                            ProviderError::message(format!("ehentai fts open rw: {e}"))
                        })?;
                        conn.busy_timeout(std::time::Duration::from_secs(30))
                            .map_err(|e| {
                                ProviderError::message(format!("ehentai fts busy: {e}"))
                            })?;
                        build_fts(&conn)
                    })
                    .await;
                    match built {
                        Ok(Ok(())) => {
                            fts_ready2.store(true, Ordering::SeqCst);
                            let stamp = file_stamp(&db_path2);
                            let m = meta_read(&meta2);
                            meta_write(&meta2, m.asset_stamp.as_deref(), stamp, Some(FTS_VERSION));
                            // WAL 归并：迁移写在独立 rw 连接上，读 store 的长连接
                            // 阻止 close 自动 checkpoint——显式截断，避免 200MB+
                            // -wal 残留（旧库通常是 WAL 模式）。
                            if let Ok(c) = rusqlite::Connection::open(&db_path2) {
                                let _ = c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
                            }
                            // 迁移成功：删除旧版独立 FTS 库（已无读者）
                            if let Some(dir) = db_path2.parent() {
                                for f in [dir.join(LEGACY_FTS_DB_FILE)] {
                                    if f.exists() {
                                        if let Err(e) = std::fs::remove_file(&f) {
                                            tracing::warn!(
                                                "ehentai archive: remove legacy {}: {e}",
                                                f.display()
                                            );
                                        } else {
                                            tracing::info!(
                                                "ehentai archive: removed legacy {}",
                                                f.display()
                                            );
                                        }
                                    }
                                }
                            }
                            tracing::info!("ehentai fts index ready (rebuilt)");
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(
                                "ehentai fts build failed: {e}; search falls back to LIKE/online"
                            );
                        }
                        Err(e) => {
                            tracing::warn!("ehentai fts build task panicked/join failed: {e}");
                        }
                    }
                });
            } else {
                fts_ready.store(true, Ordering::SeqCst);
                tracing::info!("ehentai fts ready (existing index)");
            }
        });
        svc
    }

    /// 启动周期自动更新（幂等；interval=0 不启动）。
    /// 复用 launch_update（与手动触发同一互斥通道）；间隔 interval，
    /// 下载/构建失败后 15 分钟快速重试（旧库保留，provider 保持可用）。
    pub fn start_auto_update(self: &Arc<Self>, interval_hours: u64) {
        if interval_hours == 0 {
            return;
        }
        if self.auto_update_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let svc = self.clone();
        tokio::spawn(async move {
            // 等打开现有 db 完成后再开始周期检查（见 `opened` 注释：避免与打开/构建竞争）
            while !svc.opened.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            let interval = std::time::Duration::from_secs(interval_hours.max(1) * 3600);
            let retry_delay = std::time::Duration::from_secs(15 * 60);
            loop {
                let ok = Self::wait_update(&svc).await;
                tokio::time::sleep(if ok { interval } else { retry_delay }).await;
            }
        });
    }

    /// 当前 store（未就绪/构建中 → None → 调用方回退在线 API）。
    pub(crate) fn get(&self) -> Option<Arc<EHentaiArchiveStore>> {
        if !self.ready.load(Ordering::SeqCst) {
            return None;
        }
        self.store.read().unwrap().clone()
    }

    /// 离线标题搜索：FTS5 分析链索引优先（短查询自动前缀匹配），
    /// 未就绪/无有效 token 降级 LIKE。
    /// 返回 GalleryRow（最新优先，过滤 expunged/removed + category/uploader 白名单）。
    pub(crate) fn search_titles(
        &self,
        query: &str,
        limit: usize,
        category_filter: &[String],
        uploader_filter: &[String],
    ) -> Vec<GalleryRow> {
        let Some(store) = self.get() else {
            return Vec::new();
        };
        // ① FTS 路径（索引就绪；短查询在 search_gids 内自动改前缀匹配，不再走 LIKE）
        let gids: Vec<i32> = if self.fts_ready.load(Ordering::SeqCst) {
            match store.search_gids(query, limit.saturating_mul(4).max(50)) {
                Some(gids) => gids,
                // 纯符号查询（无有效 token）→ 降级主库 LIKE 全表扫
                None => store.like_search_gids(
                    query,
                    limit.saturating_mul(4).max(50),
                    category_filter,
                    uploader_filter,
                ),
            }
        } else {
            // FTS 未就绪（迁移/重建中）→ 降级主库 LIKE 全表扫
            store.like_search_gids(
                query,
                limit.saturating_mul(4).max(50),
                category_filter,
                uploader_filter,
            )
        };
        if gids.is_empty() {
            return Vec::new();
        }
        // ② 批量取行 + category/uploader 过滤（FTS 路径未提前过滤）→ 最新优先
        let mut rows = store.get_by_gids(&gids);
        if !category_filter.is_empty() || !uploader_filter.is_empty() {
            rows.retain(|r| {
                (category_filter.is_empty() || category_filter.iter().any(|c| c == &r.category))
                    && (uploader_filter.is_empty()
                        || r.uploader
                            .as_deref()
                            .map(|u| uploader_filter.iter().any(|x| x == u))
                            .unwrap_or(false))
            });
        }
        rows.sort_by(|a, b| b.posted.cmp(&a.posted));
        rows.truncate(limit);
        rows
    }

    #[allow(dead_code)]
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// 最近一次成功更新的数据时间（远端 release asset 的 Last-Modified，即离线数据版本）；
    /// 未下载过 → None（WebUI 显示「未下载」）。对应 `BookWalkerDbMetadata` 的 timestamp 语义。
    ///
    /// 兼容已有数据：meta 文件的 asset_stamp 是 HTTP-date（`Mon, 21 Sep 2026 ...`），
    /// 统一转 RFC3339 便于展示；旧版本/外部下载的 db 可能无 meta → 回退 db 文件修改时间（本地下载时间）。
    pub fn download_timestamp(&self) -> Option<String> {
        meta_read(&self.meta)
            .asset_stamp
            .and_then(|s| {
                chrono::DateTime::parse_from_rfc2822(&s)
                    .ok()
                    .map(|t| t.to_rfc3339())
                    .or(Some(s))
            })
            .or_else(|| {
                std::fs::metadata(&self.db_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
            })
    }

    /// 启动（或复用进行中的）更新，返回进度事件流（对齐 `MangaBakaDbDownloader::launch_download`）。
    pub fn launch_update(&self) -> tokio::sync::watch::Receiver<Option<DownloadProgress>> {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        if self
            .download_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            {
                let mut guard = self.progress.lock().unwrap();
                *guard = Some(sender.clone());
            }
            let this = self.clone();
            // do_update 含重 IO（commit rename + FTS 构建），整段跑在阻塞线程上
            //（block_on），不占用 tokio worker
            crate::util::heavy_pool::spawn_heavy(move || {
                let result = tokio::runtime::Handle::current().block_on(this.do_update(&sender));
                // 失败必须发射 ErrorEvent 终态，否则路由层 watch 流等不到 Finished/Error 会一直挂起
                if let Err(e) = &result {
                    tracing::error!("ehentai archive update failed: {e}");
                    let _ = sender.send(Some(DownloadProgress::ErrorEvent { message: e.clone() }));
                }
                this.download_in_progress.store(false, Ordering::SeqCst);
                let _ = result;
            });
        } else {
            let guard = self.progress.lock().unwrap();
            if let Some(current) = guard.as_ref() {
                return current.subscribe();
            }
        }
        receiver
    }

    /// 启动（或复用）更新并等待 Finished/Error 事件；返回是否成功（后台循环用）。
    async fn wait_update(d: &Arc<Self>) -> bool {
        let mut rx = d.launch_update();
        loop {
            // watch channel：事件变化后 changed() Ok；sender 全部 drop 后 Err（防悬挂）
            if rx.changed().await.is_err() {
                return false;
            }
            match rx.borrow().as_ref() {
                Some(DownloadProgress::FinishedEvent) => return true,
                Some(DownloadProgress::ErrorEvent { .. }) => return false,
                _ => {}
            }
        }
    }

    /// 单次更新：HEAD Last-Modified → 与本地 meta 对比（相同则跳过，不重复下载）→
    /// 下载+解压 → commit（原子替换）→ 写 meta → 重建 FTS。
    async fn do_update(
        &self,
        sender: &tokio::sync::watch::Sender<Option<DownloadProgress>>,
    ) -> Result<(), String> {
        let emit = |sender: &tokio::sync::watch::Sender<Option<DownloadProgress>>,
                    event: DownloadProgress| {
            let _ = sender.send(Some(event));
        };
        // 1. 远程 stamp（HEAD Last-Modified）
        emit(
            sender,
            DownloadProgress::ProgressEvent {
                total: 0,
                completed: 0,
                info: Some("checking ehentai archive update".to_string()),
            },
        );
        let remote = fetch_remote_stamp(&self.http_client, &self.url)
            .await
            .map_err(|e| format!("ehentai archive meta: {e}"))?;
        let Some(remote) = remote else {
            // HEAD 拿不到 stamp：无法判断更新，跳过本次（后台按正常间隔重试）
            emit(sender, DownloadProgress::FinishedEvent);
            return Ok(());
        };
        // 2. 更新检测：远端未变 → 跳过（不重复下载）
        let local = meta_read(&self.meta).asset_stamp;
        if local.as_deref() == Some(remote.as_str()) {
            emit(
                sender,
                DownloadProgress::ProgressEvent {
                    total: 0,
                    completed: 0,
                    info: Some("ehentai archive is up to date".to_string()),
                },
            );
            emit(sender, DownloadProgress::FinishedEvent);
            return Ok(());
        }
        // 3. 下载 + 解压（字节进度事件；断点续传只补剩余字节）
        let url = self.url.clone();
        emit(
            sender,
            DownloadProgress::ProgressEvent {
                total: 0,
                completed: 0,
                info: Some(format!("downloading {url}")),
            },
        );
        let emit_ref = &emit;
        let progress_emitter: &(dyn Fn(u64, u64) + Send + Sync) = &move |written, total| {
            emit_ref(
                sender,
                DownloadProgress::ProgressEvent {
                    total: total as i64,
                    completed: written as i64,
                    info: Some(url.clone()),
                },
            )
        };
        download_with_retry(
            &self.dl_client,
            &self.url,
            &self.db_path,
            Some(progress_emitter),
        )
        .await
        .map_err(|e| format!("ehentai archive download: {e}"))?;
        // 4. 在 tmp 库内一并构建 FTS 索引 → commit（rename 原子替换数据+索引）：
        //    单次 rename 后新库完整可用，无"主库新/索引旧"的降级窗口
        //    （旧版为 commit 后再花 ~150s 重建独立 fts 文件，期间搜索降级 LIKE）。
        //    FTS 构建失败 → 本次更新失败，保留旧库（数据与索引永不脱节）。
        let db_tmp = self.db_path.with_extension("db.tmp");
        {
            let db_tmp_c = db_tmp.clone();
            crate::util::heavy_pool::spawn_heavy(move || {
                let conn = rusqlite::Connection::open(&db_tmp_c)
                    .map_err(|e| ProviderError::message(format!("ehentai fts open tmp: {e}")))?;
                build_fts(&conn)
            })
            .await
            .map_err(|e| format!("ehentai archive fts build: {e}"))?
            .map_err(|e| format!("ehentai archive fts build: {e}"))?;
        }
        let mut current: Option<Arc<EHentaiArchiveStore>> = None;
        if !commit_archive(
            &self.store,
            &self.ready,
            &mut current,
            &db_tmp,
            &self.db_path,
            self.idle_secs,
        )
        .await
        {
            return Err("ehentai archive commit failed (rename/validate)".to_string());
        }
        self.fts_ready.store(true, Ordering::SeqCst);
        meta_write(&self.meta, Some(&remote), None, Some(FTS_VERSION));
        tracing::info!("ehentai archive updated");
        emit(sender, DownloadProgress::FinishedEvent);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 单测
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造最小 e-hentai-db schema 的临时库（gallery/gid_tid/tag）。
    fn build_test_db(path: &Path) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE gallery (
                 gid INTEGER PRIMARY KEY, token TEXT NOT NULL,
                 title TEXT NOT NULL, title_jpn TEXT NOT NULL DEFAULT '',
                 category TEXT NOT NULL, thumb TEXT NOT NULL, uploader TEXT,
                 posted INTEGER NOT NULL, filecount INTEGER NOT NULL,
                 filesize INTEGER NOT NULL, expunged INTEGER NOT NULL,
                 removed INTEGER NOT NULL DEFAULT 0, replaced INTEGER NOT NULL DEFAULT 0,
                 rating TEXT NOT NULL, torrentcount INTEGER NOT NULL,
                 root_gid INTEGER DEFAULT NULL, bytorrent INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE gid_tid (gid INTEGER NOT NULL, tid INTEGER NOT NULL,
                 PRIMARY KEY (gid, tid));
             CREATE TABLE tag (id INTEGER PRIMARY KEY AUTOINCREMENT,
                 name TEXT NOT NULL UNIQUE);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gallery (gid, token, title, title_jpn, category, thumb, uploader,
                posted, filecount, filesize, expunged, removed, replaced, rating,
                torrentcount, bytorrent)
             VALUES (4190146, '90f7fd33fa', 'test &amp; title', '[test] タイトル',
                'Doujinshi', 'https://ehgt.org/x.jpg', 'u', 1700000000, 20, 123456,
                0, 0, 0, '4.54', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO tag (name) VALUES ('language:chinese')", [])
            .unwrap();
        conn.execute("INSERT INTO tag (name) VALUES ('group:test circle')", [])
            .unwrap();
        conn.execute("INSERT INTO gid_tid (gid, tid) VALUES (4190146, 1)", [])
            .unwrap();
        conn.execute("INSERT INTO gid_tid (gid, tid) VALUES (4190146, 2)", [])
            .unwrap();
        // 已删除（expunged）→ 不应命中
        conn.execute(
            "INSERT INTO gallery (gid, token, title, title_jpn, category, thumb, uploader,
                posted, filecount, filesize, expunged, removed, replaced, rating,
                torrentcount, bytorrent)
             VALUES (111, 'aabbccddee', 'gone', '', 'Manga', '', '', 0, 1, 1,
                1, 0, 0, '1.00', 0, 0)",
            [],
        )
        .unwrap();
        // 含 "本3.0" 的条目（detail=none 下 punctuation token 不得触发 phrase 报错）
        conn.execute(
            "INSERT INTO gallery (gid, token, title, title_jpn, category, thumb, uploader,
                posted, filecount, filesize, expunged, removed, replaced, rating,
                torrentcount, bytorrent)
             VALUES (333, 'ccdd112233', 'Wakaraserareru Hon 3.0', 'わからせられる本3.0',
                'Doujinshi', '', '', 1700000000, 10, 100, 0, 0, 0, '4.00', 0, 0)",
            [],
        )
        .unwrap();
        // 正常第二条（搜索/过滤用）
        conn.execute(
            "INSERT INTO gallery (gid, token, title, title_jpn, category, thumb, uploader,
                posted, filecount, filesize, expunged, removed, replaced, rating,
                torrentcount, bytorrent)
             VALUES (222, 'bbccddeeff', 'Aisareru Shikaku', '愛される資格', 'Manga',
                'https://ehgt.org/y.jpg', 'v', 1690000000, 30, 234567,
                0, 0, 0, '4.80', 0, 0)",
            [],
        )
        .unwrap();
    }

    fn test_db_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ehentai_archive_test_{tag}_{}.db",
            std::process::id()
        ))
    }

    #[test]
    fn store_get_by_gid_maps_row_with_tags() {
        let path = test_db_path("row");
        let _ = std::fs::remove_file(&path);
        build_test_db(&path);
        let store = EHentaiArchiveStore::open(&path, 0).expect("open");
        assert!(store.validate());

        let row = store.get_by_gid(4190146).expect("hit");
        assert_eq!(row.gid, 4190146);
        assert_eq!(row.token, "90f7fd33fa");
        assert_eq!(row.title, "test &amp; title");
        assert_eq!(row.title_jpn, "[test] タイトル");
        assert_eq!(row.category, "Doujinshi");
        assert_eq!(row.rating, "4.54");
        assert_eq!(row.file_count, 20);
        assert_eq!(row.tags, vec!["language:chinese", "group:test circle"]);

        // expunged → 未命中
        assert!(store.get_by_gid(111).is_none());
        // 不存在
        assert!(store.get_by_gid(999999).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn store_get_by_gids_preserves_order() {
        let path = test_db_path("batch");
        let _ = std::fs::remove_file(&path);
        build_test_db(&path);
        let store = EHentaiArchiveStore::open(&path, 0).expect("open");
        let rows = store.get_by_gids(&[222, 4190146, 111, 999]);
        let gids: Vec<i32> = rows.iter().map(|r| r.gid).collect();
        // 保序 + 跳过缺失/expunged
        assert_eq!(gids, vec![222, 4190146]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn store_validate_fails_on_empty() {
        let path = test_db_path("empty");
        let _ = std::fs::remove_file(&path);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE gallery (gid INTEGER PRIMARY KEY);")
            .unwrap();
        drop(conn);
        let store = EHentaiArchiveStore::open(&path, 0).expect("open");
        assert!(!store.validate());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn meta_stamp_roundtrip() {
        let path =
            std::env::temp_dir().join(format!("ehentai_archive_meta_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let m = meta_read(&path);
        assert!(m.asset_stamp.is_none());
        meta_write(
            &path,
            Some("Wed, 08 Oct 2025 07:09:00 GMT"),
            Some((1234, 56)),
            Some(FTS_VERSION),
        );
        let m = meta_read(&path);
        assert_eq!(
            m.asset_stamp.as_deref(),
            Some("Wed, 08 Oct 2025 07:09:00 GMT")
        );
        assert_eq!(m.fts_built_for, Some((1234, 56)));
        assert_eq!(m.fts_version, Some(FTS_VERSION));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fts_build_and_search() {
        let db = test_db_path("fts_db");
        let _ = std::fs::remove_file(&db);
        build_test_db(&db);
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            build_fts(&conn).expect("build fts");
        }
        let store = EHentaiArchiveStore::open(&db, 0).expect("open");
        assert!(store.has_fts());
        // 分析链 token AND 匹配（中/日文）
        let gids = store.search_gids("愛される資", 50).expect("hit");
        assert_eq!(gids, vec![222]);
        let gids = store.search_gids("タイトル", 50).expect("hit");
        assert_eq!(gids, vec![4190146]);
        // 英文
        let gids = store.search_gids("Aisareru", 50).expect("hit");
        assert_eq!(gids, vec![222]);
        // 单字也可命中（索引侧 unigram；<3 字符自动走前缀匹配）
        let gids = store.search_gids("愛", 50).expect("hit");
        assert_eq!(gids, vec![222]);
        // 短查询前缀匹配：拉丁/日文片段前缀均可命中（不再降级 LIKE）
        let gids = store.search_gids("Ai", 50).expect("hit");
        assert_eq!(gids, vec![222]);
        let gids = store.search_gids("タ", 50).expect("hit");
        assert_eq!(gids, vec![4190146]);
        // 双字符 CJK：前缀模式（"愛さ" * 命中 愛される資格）
        let gids = store.search_gids("愛さ", 50).expect("hit");
        assert_eq!(gids, vec![222]);
        // 装饰后缀：渐进前缀 AND 兜底（标题 "愛される資格" 无此后缀）
        let gids = store.search_gids("愛される資格 第1話", 50).expect("hit");
        assert_eq!(gids, vec![222]);
        // 繁体查询命中简体索引（t2s 归一；标题日文 愛→爱）
        let gids = store.search_gids("愛される資格", 50).expect("hit");
        assert_eq!(gids, vec![222]);
        // 不存在
        let gids = store.search_gids("不存在的东西", 50).expect("ok");
        assert!(gids.is_empty());
        // 回归：含 unicode61 分隔符的 token（"3.0"）切成片段 AND 后应命中
        // 标题 "…本3.0" 的条目
        let gids = store.search_gids("本3.0", 50).expect("hit");
        assert_eq!(gids, vec![333]);
        let _ = std::fs::remove_file(&db);
    }

    /// rank 支持：detail=column 保留 docsize，ORDER BY rank 可用（多命中按
    /// bm25 相关度排序而非报错/退化为 rowid 序）。语料保证词项在少数文档中出现
    /// （idf > 0），两条命中标题等长（token 数相同，长度归一不干扰）、仅词频不同。
    #[test]
    fn fts_rank_ordering_supported() {
        let db = test_db_path("fts_rank");
        let _ = std::fs::remove_file(&db);
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE gallery (
                 gid INTEGER PRIMARY KEY, token TEXT NOT NULL,
                 title TEXT NOT NULL, title_jpn TEXT NOT NULL DEFAULT '',
                 category TEXT NOT NULL, thumb TEXT NOT NULL, uploader TEXT,
                 posted INTEGER NOT NULL, filecount INTEGER NOT NULL,
                 filesize INTEGER NOT NULL, expunged INTEGER NOT NULL,
                 removed INTEGER NOT NULL DEFAULT 0, replaced INTEGER NOT NULL DEFAULT 0,
                 rating TEXT NOT NULL, torrentcount INTEGER NOT NULL,
                 root_gid INTEGER DEFAULT NULL, bytorrent INTEGER NOT NULL DEFAULT 0
             );",
        )
        .unwrap();
        // 3 条无关填充（词项 "資格" 只出现在 2/5 文档 → idf > 0）
        for (gid, title) in [
            (1, "无关条目甲"),
            (2, "无关条目乙"),
            (3, "无关条目丙"),
            // 等长 6 字符（token 数相同）、"資格" 词频 1 vs 3
            (444, "資格一二三四"),
            (555, "資格資格資格"),
        ] {
            conn.execute(
                "INSERT INTO gallery (gid, token, title, category, thumb,
                    posted, filecount, filesize, expunged, rating, torrentcount)
                 VALUES (?1, 'tok', ?2, 'Manga', '', 1, 1, 1, 0, '1.0', 0)",
                rusqlite::params![gid, title],
            )
            .unwrap();
        }
        build_fts(&conn).expect("build fts");
        drop(conn);
        let store = EHentaiArchiveStore::open(&db, 0).expect("open");
        let gids = store.search_gids("資格", 50).expect("hit");
        assert_eq!(gids.len(), 2, "only the two contenders hit, got {gids:?}");
        // 词频高者排前：555（資格×3）应排在 444（資格×1）之前
        assert_eq!(gids[0], 555, "rank ordering expected, got {gids:?}");
        assert_eq!(gids[1], 444);
        let _ = std::fs::remove_file(&db);
    }

    /// 迁移回归：旧版独立 fts 文件布局的库（主库无 gallery_fts）→ 库内 build_fts
    /// 重建后 has_fts/search 可用（对齐启动迁移路径）。
    #[test]
    fn fts_in_db_migration_rebuild() {
        let db = test_db_path("fts_migrate");
        let legacy_fts = test_db_path("fts_migrate_legacyfts");
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(&legacy_fts);
        build_test_db(&db);
        // 模拟旧版：独立 fts 文件存在、主库无 gallery_fts
        {
            let conn = rusqlite::Connection::open(&legacy_fts).unwrap();
            conn.execute_batch(
                "CREATE VIRTUAL TABLE gallery_fts USING fts5(title, tokenize='unicode61',
                     detail=none, content='');",
            )
            .unwrap();
        }
        assert!(!EHentaiArchiveStore::open(&db, 0).unwrap().has_fts());
        // 迁移 = 主库内原地重建
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            build_fts(&conn).expect("rebuild in main db");
        }
        let store = EHentaiArchiveStore::open(&db, 0).unwrap();
        assert!(store.has_fts());
        assert_eq!(store.search_gids("愛される資", 50).expect("hit"), vec![222]);
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(&legacy_fts);
    }

    #[test]
    fn fts_fragments_expr_splits_punctuation() {
        let tokens = |ts: &[&str]| ts.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert_eq!(
            fts_fragments_and_expr(&tokens(&["本", "3.0"]), false),
            "\"本\" AND \"3\" AND \"0\""
        );
        assert_eq!(
            fts_fragments_and_expr(&tokens(&["re:zero", "生活"]), false),
            "\"re\" AND \"zero\" AND \"生活\""
        );
        assert_eq!(fts_fragments_and_expr(&[], false), "");
        // 短查询前缀模式：每个片段改为 FTS5 前缀匹配
        assert_eq!(fts_fragments_and_expr(&tokens(&["愛"]), true), "\"愛\" *");
        assert_eq!(
            fts_fragments_and_expr(&tokens(&["ai", "本"]), true),
            "\"ai\" * AND \"本\" *"
        );
    }

    #[test]
    fn like_search_and_filters() {
        let db = test_db_path("like");
        let _ = std::fs::remove_file(&db);
        build_test_db(&db);
        let store = EHentaiArchiveStore::open(&db, 0).expect("open");
        // LIKE 命中 title_jpn
        let gids = store.like_search_gids("愛される", 50, &[], &[]);
        assert_eq!(gids, vec![222]);
        // category 过滤
        let gids = store.like_search_gids("愛される", 50, &["Doujinshi".to_string()], &[]);
        assert!(gids.is_empty());
        let gids = store.like_search_gids("愛される", 50, &["Manga".to_string()], &[]);
        assert_eq!(gids, vec![222]);
        // uploader 过滤
        let gids = store.like_search_gids("愛される", 50, &[], &["v".to_string()]);
        assert_eq!(gids, vec![222]);
        let gids = store.like_search_gids("愛される", 50, &[], &["u".to_string()]);
        assert!(gids.is_empty());
        let _ = std::fs::remove_file(&db);
    }
}
