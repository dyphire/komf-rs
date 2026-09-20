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
//! - 标题搜索：附属 FTS5 trigram 索引库 `e-hentai-fts.db`
//!   （372 万行构建 ~150s、1.4GB；查询毫秒级，中文/日文子串匹配），
//!   构建/重建由 meta 记录主库 len+mtime 自动触发；未就绪时降级主库 LIKE 扫描。
//!
//! 未就绪 / 构建中 / 未命中 → 调用方回退在线 gdata（与 bangumi archive 模式一致）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use futures::StreamExt;
use serde_json::json;

use crate::providers::ProviderError;

/// 缺省下载 URL（URenko nightly release 的 SQLite dump，zstd 压缩）。
pub(crate) const DEFAULT_EHENTAI_ARCHIVE_URL: &str =
    "https://github.com/URenko/e-hentai-db/releases/download/nightly/e-hentai.db.zstd";

/// FTS 索引库文件名（与主库同目录）。
const FTS_DB_FILE: &str = "e-hentai-fts.db";

/// FTS 索引格式版本：DDL/存储格式变更时递增（如 detail=none / contentless），meta 版本不符自动重建。
const FTS_VERSION: u32 = 3;

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
            idle_release_ms: idle_release_secs.saturating_mul(1000),
        })
    }

    /// 表结构有效（gallery 表存在且非空）。
    pub fn validate(&self) -> bool {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM gallery", [], |r| {
                r.get::<_, i64>(0)
            })
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
        let params: Vec<rusqlite::types::Value> =
            gids.iter().map(|g| rusqlite::types::Value::Integer(*g as i64)).collect();
        let mut rows = match stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
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
            })
        {
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

    /// 主库 LIKE 降级搜索（FTS 未就绪或短词 <3 字符时）。返回 gid 列表（最新优先）。
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
            let ph: Vec<String> = (params.len() + 1..).take(category_filter.len()).map(|i| format!("?{i}")).collect();
            sql.push_str(&format!(" AND category IN ({})", ph.join(",")));
            params.extend(category_filter.iter().cloned());
        }
        if !uploader_filter.is_empty() {
            let ph: Vec<String> = (params.len() + 1..).take(uploader_filter.len()).map(|i| format!("?{i}")).collect();
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
        let mut rows = match stmt.query_map(
            rusqlite::params_from_iter(params.iter()),
            |r| r.get::<_, i32>(0),
        ) {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        while let Some(Ok(g)) = rows.next() {
            out.push(g);
        }
        out
    }

    /// 空闲释放：距上次查询超过 idle 阈值 → `PRAGMA shrink_memory` 归还未使用页面缓存。
    pub fn release_if_idle(&self) {
        let now = now_ms();
        let last = self.last_used.load(Ordering::Relaxed);
        let idle = self.idle_release_ms;
        if idle > 0
            && now.saturating_sub(last) >= idle
            && self
                .last_used
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let _ = self.conn.lock().unwrap().execute_batch("PRAGMA shrink_memory;");
            tracing::debug!("ehentai archive: released sqlite page cache");
        }
    }

    fn touch(&self) {
        let now = now_ms();
        let last = self.last_used.load(Ordering::Relaxed);
        let idle = self.idle_release_ms;
        if idle > 0 && now.saturating_sub(last) >= idle {
            if self
                .last_used
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                let _ = self.conn.lock().unwrap().execute_batch("PRAGMA shrink_memory;");
            }
        } else {
            self.last_used.store(now, Ordering::Relaxed);
        }
    }
}

// ---------------------------------------------------------------------------
// FTS5 trigram 索引
// ---------------------------------------------------------------------------

/// 只读 FTS5 trigram 索引句柄（标题子串匹配，毫秒级）。
pub(crate) struct EHentaiFtsStore {
    conn: Mutex<rusqlite::Connection>,
}

impl EHentaiFtsStore {
    pub fn open(fts_path: &Path) -> Result<Self, ProviderError> {
        use rusqlite::OpenFlags;
        let conn = rusqlite::Connection::open_with_flags(
            fts_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| ProviderError::message(format!("ehentai fts open failed: {e}")))?;
        conn.pragma_update(None, "query_only", "ON")
            .map_err(|e| ProviderError::message(format!("ehentai fts pragma failed: {e}")))?;
        conn.pragma_update(None, "cache_size", -32000)
            .map_err(|e| ProviderError::message(format!("ehentai fts pragma failed: {e}")))?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// FTS 索引有效（表存在且非空；contentless 下 count(*) 依赖内容列，改用 LIMIT 1）。
    pub fn validate(&self) -> bool {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT rowid FROM gallery_fts LIMIT 1", [], |_| Ok(()))
            .is_ok()
    }

    /// trigram 子串匹配。索引为 detail=none（无位置信息）+ contentless（不存原文），
    /// 不能直接用 `"完整词"` phrase 查询（FTS5 无位置无法验证子串连续性，返回空）——
    /// 因此把查询词手动切成 3-gram 单 token，构造 `"tg1" AND "tg2" AND ...`
    /// （detail=none 下单 token 查询正常），语义等价子串匹配
    /// （乱序假阳性极低，后续相似度匹配兜底）。
    /// 查询串需 ≥3 字符，否则返回 None（调用方降级 LIKE）。
    pub fn search_gids(&self, query: &str, limit: usize) -> Option<Vec<i32>> {
        let chars: Vec<char> = query.chars().collect();
        if chars.len() < 3 {
            return None;
        }
        let mut trigrams: Vec<String> = chars
            .windows(3)
            .map(|w| w.iter().collect::<String>())
            .collect();
        // 防御：长查询截断（50 个 trigram 足够精确，避免 MATCH 表达式过长）
        trigrams.truncate(50);
        let match_expr = trigrams
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT rowid FROM gallery_fts WHERE gallery_fts MATCH ?1 LIMIT ?2")
            .ok()?;
        let mut rows = stmt
            .query_map(rusqlite::params![match_expr, limit as i64], |r| {
                r.get::<_, i32>(0)
            })
            .ok()?;
        let mut out = Vec::new();
        while let Some(Ok(g)) = rows.next() {
            out.push(g);
        }
        Some(out)
    }
}

/// 从主库构建 FTS5 trigram 索引（写 .tmp 后 rename；分批按 gid 范围插入）。
fn build_fts(db_path: &Path, fts_path: &Path) -> Result<(), ProviderError> {
    let tmp = fts_path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let conn = rusqlite::Connection::open(&tmp)
        .map_err(|e| ProviderError::message(format!("ehentai fts create failed: {e}")))?;
    conn.execute_batch(
        // detail=none：子串匹配不需要位置信息，去掉位置数据显著缩小倒排。
        // content=''：不存 title/title_jpn 原文（gid 放 rowid），只存倒排——
        // 原文在主库已有，FTS 只负责命中 gid，体积进一步减半。
        "CREATE VIRTUAL TABLE gallery_fts USING fts5(title, title_jpn, tokenize='trigram', detail=none, content='');",
    )
    .map_err(|e| ProviderError::message(format!("ehentai fts create table failed: {e}")))?;
    // 只读 URI 打开主库（避免 ATTACH 以读写模式打开产生 WAL/-shm 残留）
    let db_uri = format!(
        "file:{}?mode=ro",
        db_path.to_string_lossy().replace('\\', "/").replace('?', "%3f").replace('#', "%23")
    );
    conn.execute_batch(&format!("ATTACH DATABASE '{db_uri}' AS src;"))
        .map_err(|e| ProviderError::message(format!("ehentai fts attach failed: {e}")))?;
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM src.gallery", [], |r| r.get(0))
        .map_err(|e| ProviderError::message(format!("ehentai fts count failed: {e}")))?;
    if total == 0 {
        return Err(ProviderError::message("ehentai fts: source gallery empty"));
    }
    tracing::info!("ehentai fts building index for {total} rows...");
    // gid 为 INTEGER PRIMARY KEY → rowid=gid，按 gid 分批
    const BATCH: i64 = 500_000;
    let mut last_gid: i64 = -1;
    let mut done: i64 = 0;
    while done < total {
        let n = conn
            .execute(
                "INSERT INTO gallery_fts(rowid, title, title_jpn) \
                 SELECT gid, title, title_jpn FROM src.gallery \
                 WHERE gid > ?1 ORDER BY gid LIMIT ?2",
                rusqlite::params![last_gid, BATCH],
            )
            .map_err(|e| ProviderError::message(format!("ehentai fts insert failed: {e}")))?;
        if n == 0 {
            break;
        }
        done += n as i64;
        last_gid = conn
            .query_row(
                "SELECT COALESCE(MAX(rowid), -1) FROM gallery_fts",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(|e| ProviderError::message(format!("ehentai fts progress failed: {e}")))?;
        if done % (BATCH * 2) == 0 || done >= total {
            tracing::info!("ehentai fts index progress {done}/{total}");
        }
    }
    conn.execute_batch("DETACH DATABASE src;")
        .map_err(|e| ProviderError::message(format!("ehentai fts detach failed: {e}")))?;
    drop(conn);
    std::fs::rename(&tmp, fts_path)
        .map_err(|e| ProviderError::message(format!("ehentai fts rename failed: {e}")))?;
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
async fn download_and_extract(
    client: &reqwest::Client,
    url: &str,
    db_path: &Path,
) -> Result<(), ProviderError> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(ProviderError::message(format!(
            "ehentai archive download HTTP {}",
            resp.status()
        )));
    }
    use std::io::Write;
    let zst_tmp = db_path.with_extension("db.zstd.tmp");
    let db_tmp = db_path.with_extension("db.tmp");
    // ① 流式写压缩文件（1GB+，不能整包载入内存）
    {
        let mut out = std::fs::File::create(&zst_tmp)
            .map_err(|e| ProviderError::message(format!("ehentai archive tmp write failed: {e}")))?;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            out.write_all(&chunk?)
                .map_err(|e| ProviderError::message(format!("ehentai archive tmp write failed: {e}")))?;
        }
        out.flush()
            .map_err(|e| ProviderError::message(format!("ehentai archive tmp flush failed: {e}")))?;
    }
    // ② zstd 流式解压
    {
        let input = std::fs::File::open(&zst_tmp)
            .map_err(|e| ProviderError::message(format!("ehentai archive tmp open failed: {e}")))?;
        let mut decoder = zstd::stream::read::Decoder::new(input)
            .map_err(|e| ProviderError::message(format!("ehentai archive zstd decode failed: {e}")))?;
        let mut out = std::fs::File::create(&db_tmp)
            .map_err(|e| ProviderError::message(format!("ehentai archive tmp write failed: {e}")))?;
        std::io::copy(&mut decoder, &mut out)
            .map_err(|e| ProviderError::message(format!("ehentai archive extract failed: {e}")))?;
        out.flush()
            .map_err(|e| ProviderError::message(format!("ehentai archive tmp flush failed: {e}")))?;
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
    // ④ 原子替换
    std::fs::rename(&db_tmp, db_path).map_err(|e| {
        ProviderError::message(format!("ehentai archive db rename failed: {e}"))
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Service —— 对齐 BangumiArchiveService 的后台生命周期
// ---------------------------------------------------------------------------

pub(crate) struct EHentaiArchiveService {
    store: Arc<RwLock<Option<Arc<EHentaiArchiveStore>>>>,
    fts: Arc<RwLock<Option<Arc<EHentaiFtsStore>>>>,
    ready: Arc<AtomicBool>,
}

impl EHentaiArchiveService {
    /// 启动后台任务（非阻塞）。db 文件缺省 workDir/ehentai/e-hentai.db。
    pub fn start(
        config: &crate::config::EHentaiArchiveConfig,
        http_client: reqwest::Client,
        work_dir: Option<&Path>,
    ) -> Arc<Self> {
        let store: Arc<RwLock<Option<Arc<EHentaiArchiveStore>>>> = Arc::new(RwLock::new(None));
        let fts: Arc<RwLock<Option<Arc<EHentaiFtsStore>>>> = Arc::new(RwLock::new(None));
        let ready = Arc::new(AtomicBool::new(false));
        let svc = Arc::new(Self {
            store: store.clone(),
            fts: fts.clone(),
            ready: ready.clone(),
        });
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
        let interval_hours = config.update_interval_hours;
        let idle_secs = config.idle_release_secs.unwrap_or(0);

        // 后台空闲释放：每 min(idle,60)s 检查一次，空闲超时释放 SQLite 页面缓存
        if idle_secs > 0 {
            let svc_idle = svc.clone();
            tokio::spawn(async move {
                let tick = std::time::Duration::from_secs(idle_secs.min(60).max(1));
                loop {
                    tokio::time::sleep(tick).await;
                    if let Some(s) = svc_idle.store.read().unwrap().as_ref() {
                        s.release_if_idle();
                    }
                }
            });
        }

        tokio::spawn(async move {
            if let Some(dir) = db_path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            // 尝试打开现有 db
            let mut current: Option<Arc<EHentaiArchiveStore>> =
                EHentaiArchiveStore::open(&db_path, idle_secs)
                    .ok()
                    .filter(|s| s.validate())
                    .map(Arc::new);
            if let Some(s) = &current {
                *store.write().unwrap() = Some(s.clone());
                ready.store(true, Ordering::SeqCst);
                tracing::info!("ehentai archive ready (existing db)");
                ensure_fts(&store, &fts, &db_path, &meta);
            }
            let interval = std::time::Duration::from_secs(interval_hours.max(1) * 3600);
            loop {
                if current.is_none() {
                    // 首次下载
                    match download_and_extract(&http_client, &url, &db_path).await {
                        Ok(()) => match EHentaiArchiveStore::open(&db_path, idle_secs)
                            .ok()
                            .filter(|s| s.validate())
                        {
                            Some(s) => {
                                let s = Arc::new(s);
                                *store.write().unwrap() = Some(s.clone());
                                current = Some(s);
                                ready.store(true, Ordering::SeqCst);
                                tracing::info!("ehentai archive ready (downloaded)");
                                ensure_fts(&store, &fts, &db_path, &meta);
                            }
                            None => {
                                tracing::warn!("ehentai archive db invalid after download");
                            }
                        },
                        Err(e) => {
                            tracing::warn!(
                                "ehentai archive download failed: {e}; falling back to online"
                            );
                        }
                    }
                } else if interval_hours > 0 {
                    // 周期检查更新：HEAD Last-Modified 与本地 meta 对比
                    match fetch_remote_stamp(&http_client, &url).await {
                        Ok(Some(stamp)) => {
                            let local = meta_read(&meta).asset_stamp;
                            if local.as_deref() != Some(stamp.as_str()) {
                                tracing::info!("ehentai archive update available, downloading");
                                match download_and_extract(&http_client, &url, &db_path).await {
                                    Ok(()) => {
                                        if let Some(s) =
                                            EHentaiArchiveStore::open(&db_path, idle_secs)
                                                .ok()
                                                .filter(|s| s.validate())
                                        {
                                            let s = Arc::new(s);
                                            *store.write().unwrap() = Some(s.clone());
                                            current = Some(s);
                                            meta_write(&meta, Some(&stamp), None, None);
                                            tracing::info!("ehentai archive updated");
                                            ensure_fts(&store, &fts, &db_path, &meta);
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("ehentai archive update failed: {e}");
                                    }
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(e) => tracing::warn!("ehentai archive update check failed: {e}"),
                    }
                }
                tokio::time::sleep(interval).await;
            }
        });
        svc
    }

    /// 当前 store（未就绪/构建中 → None → 调用方回退在线 API）。
    pub fn get(&self) -> Option<Arc<EHentaiArchiveStore>> {
        if !self.ready.load(Ordering::SeqCst) {
            return None;
        }
        self.store.read().unwrap().clone()
    }

    /// 离线标题搜索（阶段 B）：FTS5 trigram 优先，未就绪/短词降级 LIKE。
    /// 返回 GalleryRow（最新优先，过滤 expunged/removed + category/uploader 白名单）。
    pub fn search_titles(
        &self,
        query: &str,
        limit: usize,
        category_filter: &[String],
        uploader_filter: &[String],
    ) -> Vec<GalleryRow> {
        let Some(store) = self.get() else {
            return Vec::new();
        };
        // ① FTS 路径（≥3 字符且索引就绪）
        let gids: Vec<i32> = if query.chars().count() >= 3 {
            let fts_hit: Option<Vec<i32>> = {
                let guard = self.fts.read().unwrap();
                guard
                    .as_ref()
                    .and_then(|f| f.search_gids(query, limit.saturating_mul(4).max(50)))
            };
            match fts_hit {
                Some(gids) => gids,
                None => store.like_search_gids(query, limit.saturating_mul(4).max(50), category_filter, uploader_filter),
            }
        } else {
            store.like_search_gids(query, limit.saturating_mul(4).max(50), category_filter, uploader_filter)
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
                        || r
                            .uploader
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
}

/// 检查 FTS 索引是否与主库匹配（meta 记录 len+mtime），不匹配则后台重建。
fn ensure_fts(
    store: &Arc<RwLock<Option<Arc<EHentaiArchiveStore>>>>,
    fts: &Arc<RwLock<Option<Arc<EHentaiFtsStore>>>>,
    db_path: &Path,
    meta: &Path,
) {
    let fts_path = db_path
        .parent()
        .map(|d| d.join(FTS_DB_FILE))
        .unwrap_or_else(|| PathBuf::from(FTS_DB_FILE));
    let Some(stamp) = file_stamp(db_path) else {
        return;
    };
    let m = meta_read(meta);
    let fts_ok = m.fts_version == Some(FTS_VERSION)
        && m.fts_built_for == Some(stamp)
        && EHentaiFtsStore::open(&fts_path).map(|f| f.validate()).unwrap_or(false);
    if fts_ok {
        if let Ok(f) = EHentaiFtsStore::open(&fts_path) {
            *fts.write().unwrap() = Some(Arc::new(f));
            tracing::info!("ehentai fts ready (existing index)");
            return;
        }
    }
    // 需要（重新）构建：后台任务，构建中搜索降级 LIKE
    let fts_path2 = fts_path.clone();
    let db_path2 = db_path.to_path_buf();
    let meta2 = meta.to_path_buf();
    let store2 = store.clone();
    let fts2 = fts.clone();
    tokio::spawn(async move {
        let _ = std::fs::create_dir_all(db_path2.parent().unwrap_or(Path::new(".")));
        match build_fts(&db_path2, &fts_path2) {
            Ok(()) => match EHentaiFtsStore::open(&fts_path2) {
                Ok(f) if f.validate() => {
                    *fts2.write().unwrap() = Some(Arc::new(f));
                    let stamp = file_stamp(&db_path2);
                    let m = meta_read(&meta2);
                    meta_write(
                        &meta2,
                        m.asset_stamp.as_deref(),
                        stamp,
                        Some(FTS_VERSION),
                    );
                    tracing::info!("ehentai fts index ready (rebuilt)");
                }
                _ => {
                    tracing::warn!("ehentai fts built but failed to open");
                    let _ = std::fs::remove_file(&fts_path2);
                }
            },
            Err(e) => {
                tracing::warn!("ehentai fts build failed: {e}; search falls back to LIKE/online");
                let _ = std::fs::remove_file(&fts_path2);
            }
        }
        let _ = store2; // store 保持可用（LIKE 降级）
    });
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
        conn.execute("INSERT INTO tag (name) VALUES ('language:chinese')", []).unwrap();
        conn.execute("INSERT INTO tag (name) VALUES ('group:test circle')", []).unwrap();
        conn.execute("INSERT INTO gid_tid (gid, tid) VALUES (4190146, 1)", []).unwrap();
        conn.execute("INSERT INTO gid_tid (gid, tid) VALUES (4190146, 2)", []).unwrap();
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
        std::env::temp_dir().join(format!("ehentai_archive_test_{tag}_{}.db", std::process::id()))
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
        let path = std::env::temp_dir().join(format!(
            "ehentai_archive_meta_{}.json",
            std::process::id()
        ));
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
        let fts = test_db_path("fts_idx");
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(&fts);
        build_test_db(&db);
        build_fts(&db, &fts).expect("build fts");
        let store = EHentaiFtsStore::open(&fts).expect("open fts");
        assert!(store.validate());
        // trigram 子串匹配（中/日文）
        let gids = store.search_gids("愛される資", 50).expect("hit");
        assert_eq!(gids, vec![222]);
        let gids = store.search_gids("タイトル", 50).expect("hit");
        assert_eq!(gids, vec![4190146]);
        // 英文
        let gids = store.search_gids("Aisareru", 50).expect("hit");
        assert_eq!(gids, vec![222]);
        // 短词 → None（降级 LIKE）
        assert!(store.search_gids("愛", 50).is_none());
        // 不存在
        let gids = store.search_gids("不存在的东西", 50).expect("ok");
        assert!(gids.is_empty());
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(&fts);
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
