//! bangumi/Archive 离线数据源 —— 对齐 BangumiKomga `bangumi_archive/`。
//!
//! 架构：
//! ```text
//! BangumiArchiveService（tokio::spawn 后台：下载 → 构建 → 周期更新）
//!   └── Arc<RwLock<Option<Arc<BangumiArchiveStore>>>>（原子替换共享槽）
//!         ├── Mutex<rusqlite::Connection>
//!         │     ├── subjects_idx:  id/type/name/name_cn/row_offset
//!         │     ├── subjects_fts:  FTS5 trigram（content 指向 subjects_idx）
//!         │     ├── relations_idx: (subject_id, relation_type, related_subject_id)
//!         │     ├── persons: person 实体（id/name/name_cn/type/career）
//!         │     ├── subject_persons: subject↔person 关联（含 position 角色）
//!         │     └── archive_update: key/value（last_updated）
//!         └── memmap2::Mmap（subject.jsonlines 零拷贝读行）
//! ```
//!
//! 数据源：https://github.com/bangumi/Archive（release zip 约 418MB，解压后
//! jsonlines 约 1GB+）。查询：SQLite 查偏移 → mmap seek → readline → JSON。
//! 未就绪/构建中 → 调用方回退在线 API。

use crate::providers::bangumi::{BangumiInfoBoxItem, BangumiRating, BangumiSubject, BangumiTag};
use crate::providers::{CoreProviders, ProviderError};
use futures::StreamExt;
use serde::Deserialize;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// Archive release 元数据（https://raw.githubusercontent.com/bangumi/Archive/master/aux/latest.json）
const ARCHIVE_LATEST_URL: &str =
    "https://raw.githubusercontent.com/bangumi/Archive/master/aux/latest.json";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LatestMeta {
    #[serde(alias = "browser_download_url")]
    browser_download_url: Option<String>,
    #[serde(default, alias = "updated_at")]
    updated_at: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

// ---------------------------------------------------------------------------
// 数据模型 —— archive jsonlines 行（字段全 default 容错；无封面）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveSubject {
    pub id: u64,
    pub name: String,
    #[serde(default, alias = "name_cn")]
    pub name_cn: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    /// archive 的 platform 为数字（1001=漫画 / 1002=小说 / 1003=画集 / 4001=游戏）。
    #[serde(default)]
    pub platform: Option<serde_json::Value>,
    /// archive 的 infobox 为 Wiki 模板字符串（{{Infobox ... |key=value ...}}）；
    /// 兼容在线 API 数组形式。解析见 infobox_items()。
    #[serde(default)]
    pub infobox: Option<serde_json::Value>,
    #[serde(default)]
    pub tags: Vec<BangumiTag>,
    #[serde(rename = "type", default)]
    pub subject_type: Option<i32>,
    /// archive 特有：是否系列（BangumiKomga resort 用；缺省视为系列避免误伤单本漫画）。
    #[serde(default)]
    pub series: Option<bool>,
    #[serde(default)]
    pub eps: Option<i32>,
    #[serde(default)]
    pub volumes: Option<i32>,
    #[serde(default)]
    pub rank: Option<i32>,
    #[serde(default)]
    pub total: Option<i32>,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub rating: Option<BangumiRating>,
    /// 成人内容标记（在线 API 与 Archive 均有）
    #[serde(default)]
    pub nsfw: Option<bool>,
}

/// person 实体（person.jsonlines）：name=日文原名；name_cn 从 infobox「简体中文名」提取。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivePerson {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub r#type: Option<i32>,
    #[serde(default)]
    pub career: Vec<String>,
    #[serde(default)]
    pub infobox: Option<String>,
}

/// subject↔person 关联（subject-persons.jsonlines，字段为 snake_case）。
#[derive(Debug, Clone, Default, Deserialize)]
struct SubjectPersonRel {
    #[serde(alias = "personId")]
    person_id: u64,
    #[serde(alias = "subjectId")]
    subject_id: u64,
    #[serde(default)]
    position: Option<i32>,
    #[serde(default)]
    appear_eps: Option<String>,
}

/// 查询结果：person 实体 + 关联角色（get_persons / get_person 返回）。
#[derive(Debug, Clone, Default)]
pub struct PersonInfo {
    pub person_id: u64,
    pub name: String,
    pub name_cn: Option<String>,
    pub person_type: Option<i32>,
    pub career: Vec<String>,
    pub position: Option<i32>,
    pub appear_eps: String,
    pub aliases: Vec<String>,
}

/// 从 person infobox（{{Infobox Crt\n|简体中文名= 水树奈奈\n...}}）提取简体中文名。
fn person_name_cn(infobox: Option<&str>) -> Option<String> {
    let ib = infobox?.trim();
    for raw in ib.split('\n') {
        let line = raw.trim().strip_prefix('|').unwrap_or(raw.trim());
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == "简体中文名" {
                let v = v.trim();
                if !v.is_empty() && !v.starts_with('{') {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// 从 person infobox「别名」块提取全部别名（含子条目 v；`、`/`,` 拆分；`(注音)` 拆出；
/// 空值跳过，去重保序）。
fn person_aliases(infobox: Option<&str>) -> Vec<String> {
    fn push_parts(out: &mut Vec<String>, raw: &str) {
        for part in raw.split(['、', ',']) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if !out.contains(&part.to_string()) {
                out.push(part.to_string());
            }
            // 括号注音（如 "近藤奈々 (こんどう なな)"）也拆出
            if let Some(open) = part.find('(') {
                if let Some(close) = part.rfind(')') {
                    if close > open {
                        let inner = part[open + 1..close].trim();
                        if !inner.is_empty() && !out.contains(&inner.to_string()) {
                            out.push(inner.to_string());
                        }
                    }
                }
            }
        }
    }

    let mut out: Vec<String> = Vec::new();
    let Some(ib) = infobox else { return out };
    let ib = ib.trim();
    let mut lines = ib.split('\n').peekable();
    while let Some(raw) = lines.next() {
        let line = raw.trim().strip_prefix('|').unwrap_or(raw.trim());
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == "别名" {
                let mut block = v.trim().to_string();
                // 多行别名块：{ [..] [..] } 直到 '}' 或下一个 '|'
                if block.starts_with('{') {
                    while !block.contains('}') {
                        match lines.next() {
                            Some(l) => block.push_str(l.trim()),
                            None => break,
                        }
                    }
                    for item in block.split('[').skip(1) {
                        // 截断到行尾 / '}'（别名块可能尾随换行与 '}'）
                        let item = item.trim();
                        let item = item
                            .split(['\r', '\n', '}'])
                            .next()
                            .unwrap_or(item)
                            .trim();
                        let item = item.trim_end_matches(']').trim();
                        let item = item.strip_suffix(']').unwrap_or(item).trim();
                        if let Some((_, val)) = item.split_once('|') {
                            push_parts(&mut out, val);
                        } else if !item.is_empty() {
                            push_parts(&mut out, item);
                        }
                    }
                } else if !block.is_empty() && block != "{}" {
                    push_parts(&mut out, &block);
                }
            }
        }
    }
    out
}

impl ArchiveSubject {
    /// 转在线 API 格式（BangumiSubject）：封面字段留空（archive 无图，封面走在线 API）。
    pub fn to_bangumi_subject(&self) -> BangumiSubject {
        // archive 顶层 rank/total/score 与 rating 对象可能并存；rating 优先。
        let rating = self.rating.clone().or_else(|| {
            (self.score.is_some() || self.total.is_some()).then(|| BangumiRating {
                score: self.score,
                total: self.total,
            })
        });
        BangumiSubject {
            id: self.id,
            name: self.name.clone(),
            name_cn: self.name_cn.clone(),
            image: None,
            summary: self.summary.clone(),
            date: self.date.clone(),
            images: None,
            rating,
            rank: self.rank,
            subject_type: self.subject_type,
            platform: self.platform_str(),
            tags: self.tags.clone(),
            volumes: self.volumes,
            eps: self.eps,
            total_episodes: None,
            infobox: self.infobox_items(),
            eps_info: None,
            age_rating: None,
            nsfw: self.nsfw,
        }
    }

    /// platform 数字 → 中文名（对齐 BangumiKomga SubjectPlatform；字符串原样）。
    pub fn platform_str(&self) -> Option<String> {
        self.platform.as_ref().and_then(|v| match v {
            serde_json::Value::Number(n) => n.as_i64().map(|i| match i {
                1001 => "漫画".to_string(),
                1002 => "小说".to_string(),
                1003 => "画集".to_string(),
                4001 => "游戏".to_string(),
                other => other.to_string(),
            }),
            serde_json::Value::String(st) => Some(st.clone()),
            _ => None,
        })
    }

    /// infobox：字符串（archive Wiki 模板）→ 数组 [{key,value}]（对齐在线 API）；
    /// 数组原样透传（容错解析失败项）。
    pub fn infobox_items(&self) -> Vec<BangumiInfoBoxItem> {
        match &self.infobox {
            Some(serde_json::Value::String(s)) => parse_archive_infobox(s),
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|i| serde_json::from_value(i.clone()).ok())
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// 列表项：`key|value` → kv 对；纯值 → 无 key 项。
/// 空 value（`[韩版|]` 之类）无实际内容 → 返回 None 跳过（避免写入 "韩版|" 垃圾别名）。
fn parse_list_item(seg: &str) -> Option<(Option<String>, String)> {
    if let Some((k, v)) = seg.split_once('|') {
        let k = k.trim();
        let v = v.trim();
        if !k.is_empty() && !v.is_empty() {
            return Some((Some(k.to_string()), v.to_string()));
        }
        if v.is_empty() {
            return None;
        }
        return Some((None, v.to_string()));
    }
    let seg = seg.trim();
    if seg.is_empty() {
        None
    } else {
        Some((None, seg.to_string()))
    }
}

/// 解析 archive infobox Wiki 模板字符串 → [{key,value}]。
/// 支持：`|key= value`、`|key={ [item] [item] }` 列表、跨行值。
pub(crate) fn parse_archive_infobox(text: &str) -> Vec<BangumiInfoBoxItem> {
    fn make_item(key: &str, value: Option<&str>, list: &[(Option<String>, String)]) -> BangumiInfoBoxItem {
        if list.is_empty() {
            BangumiInfoBoxItem {
                key: Some(key.to_string()),
                value: value.map(|v| serde_json::Value::String(v.to_string())),
            }
        } else {
            // 列表项两种形式：纯值 [item] → {"v": item}（对齐在线别名/出版社等）；
            // key|value [k|v] → {"k": k, "v": v}（对齐在线「版本:*」等 kv 列表）。
            let arr: Vec<serde_json::Value> = list
                .iter()
                .map(|(k, v)| match k {
                    Some(k) => serde_json::json!({ "k": k, "v": v }),
                    None => serde_json::json!({ "v": v }),
                })
                .collect();
            BangumiInfoBoxItem {
                key: Some(key.to_string()),
                value: Some(serde_json::Value::Array(arr)),
            }
        }
    }
    let mut out: Vec<BangumiInfoBoxItem> = Vec::new();
    let body = text.trim();
    let body = body.strip_prefix("{{").unwrap_or(body);
    let body = body.strip_suffix("}}").unwrap_or(body);
    let mut current_key: Option<String> = None;
    let mut current_list: Vec<(Option<String>, String)> = Vec::new();
    let mut in_list = false;
    for raw in body.split('\n') {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('|') {
            if let Some(eq) = rest.find('=') {
                // 提交上一字段
                if let Some(k) = current_key.take() {
                    out.push(make_item(&k, None, &current_list));
                    current_list.clear();
                }
                in_list = false;
                let key = rest[..eq].trim().to_string();
                let value = rest[eq + 1..].trim().to_string();
                if value == "{" {
                    in_list = true;
                    current_key = Some(key);
                } else if let Some(inner) = value.strip_prefix('{').and_then(|v| v.strip_suffix('}')) {
                    // 单行列表 { [a] [b] } / { [k|v] }
                    let items: Vec<(Option<String>, String)> = inner
                        .split(']')
                        .filter_map(|seg| {
                            let seg = seg.trim().trim_start_matches('[').trim();
                            if seg.is_empty() {
                                return None;
                            }
                            parse_list_item(seg)
                        })
                        .collect();
                    out.push(make_item(&key, None, &items));
                } else {
                    out.push(make_item(&key, Some(&value), &[]));
                }
            }
        } else if in_list {
            let trimmed = line.trim();
            if trimmed.ends_with('}') {
                in_list = false;
                continue;
            }
            let s = trimmed
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim();
            if !s.is_empty() {
                if let Some(item) = parse_list_item(s) {
                    current_list.push(item);
                }
            }
        }
    }
    if let Some(k) = current_key.take() {
        out.push(make_item(&k, None, &current_list));
    }
    out
}

/// 关联条目（relations_idx JOIN subjects_idx 输出）。
#[derive(Debug, Clone)]
pub struct RelatedSubject {
    pub id: u64,
    pub name: String,
    pub name_cn: Option<String>,
    pub subject_type: Option<i32>,
    pub relation: Option<String>,
}

// ---------------------------------------------------------------------------
// ArchiveDataStore —— SQLite 索引 + mmap 数据源
// ---------------------------------------------------------------------------

const DDL_SUBJECTS_IDX: &str = "CREATE TABLE IF NOT EXISTS subjects_idx (
    id         INTEGER PRIMARY KEY,
    type       INTEGER,
    name       TEXT,
    name_cn    TEXT,
    aliases    TEXT,
    row_offset INTEGER NOT NULL
)";
const DDL_FTS: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS subjects_fts USING fts5(
    name,
    name_cn,
    aliases,
    content='subjects_idx',
    content_rowid='id',
    tokenize='trigram'
)";
const DDL_RELATIONS: &str = "CREATE TABLE IF NOT EXISTS relations_idx (
    subject_id         INTEGER NOT NULL,
    relation_type      TEXT,
    related_subject_id INTEGER NOT NULL
)";
const DDL_REL_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_relations_subject ON relations_idx(subject_id)";
const DDL_ARCHIVE_UPDATE: &str =
    "CREATE TABLE IF NOT EXISTS archive_update (key TEXT PRIMARY KEY, value TEXT)";
const DDL_PERSONS: &str = "CREATE TABLE IF NOT EXISTS persons (
    id     INTEGER PRIMARY KEY,
    name   TEXT,
    name_cn TEXT,
    type   INTEGER,
    career TEXT,
    aliases TEXT
)";
const DDL_SUBJECT_PERSONS: &str = "CREATE TABLE IF NOT EXISTS subject_persons (
    subject_id INTEGER NOT NULL,
    person_id  INTEGER NOT NULL,
    position   INTEGER,
    appear_eps TEXT
)";
const DDL_SP_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_subject_persons_subject ON subject_persons(subject_id)";

const SCHEMA_VERSION: i64 = 5;

/// Archive relation_type（数字）→ 中文名（对齐 BangumiKomga SubjectRelation；
/// 其余类型原样返回，Rust 侧只消费"单行本"）。
fn map_relation_type(t: Option<String>) -> Option<String> {
    t.map(|v| match v.as_str() {
        "1003" => "单行本".to_string(),
        "1002" => "系列".to_string(),
        "1004" => "画集".to_string(),
        "1" => "改编".to_string(),
        other => other.to_string(),
    })
}

pub struct BangumiArchiveStore {
    conn: Mutex<rusqlite::Connection>,
    mm: Option<memmap2::Mmap>,
    /// 最近一次 mmap 读取的毫秒时间戳（活动标记，用于空闲释放）。
    last_used: std::sync::atomic::AtomicU64,
    /// 空闲释放阈值（毫秒）；0 = 禁用。
    idle_release_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl BangumiArchiveStore {
    pub fn open(
        db_path: &Path,
        subjects_path: &Path,
        idle_release_secs: u64,
    ) -> Result<Self, ProviderError> {
        let conn = rusqlite::Connection::open(db_path).map_err(|e| {
            ProviderError::message(format!("archive sqlite open failed: {e}"))
        })?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-32000;",
        )
        .map_err(|e| ProviderError::message(format!("archive pragma failed: {e}")))?;
        // 历史全量重建可能残留大 WAL（数百万行单事务 → 数百 MB）：打开时合并回主库并截断，
        // 避免 WAL 文件长期占用磁盘、读路径额外遍历 WAL 页。WAL 干净时该操作近乎零成本。
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
        let mm = std::fs::File::open(subjects_path).ok().and_then(|f| unsafe {
            memmap2::Mmap::map(&f).ok()
        });
        Ok(Self {
            conn: Mutex::new(conn),
            mm,
            last_used: std::sync::atomic::AtomicU64::new(now_ms()),
            idle_release_ms: idle_release_secs.saturating_mul(1000),
        })
    }

    /// 标记活动：距上次查询超过 idle 阈值时先释放 mmap 热页（MADV_DONTNEED，
    /// 仅 unix；下次访问按需重读），随后更新活动时间戳。
    fn touch(&self) {
        let now = now_ms();
        let last = self.last_used.load(std::sync::atomic::Ordering::Relaxed);
        let idle = self.idle_release_ms;
        if idle > 0 && now.saturating_sub(last) >= idle {
            // CAS 防止并发重复释放
            if self
                .last_used
                .compare_exchange(
                    last,
                    now,
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
            {
                #[cfg(unix)]
                if let Some(mm) = &self.mm {
                    unsafe {
                        libc::madvise(
                            mm.as_ptr() as *mut libc::c_void,
                            mm.len(),
                            libc::MADV_DONTNEED,
                        );
                    }
                    tracing::debug!(
                        "bangumi archive: released mmap hot pages after {}s idle",
                        idle / 1000
                    );
                }
            }
        } else {
            self.last_used.store(now, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn init_schema(&self) -> Result<(), ProviderError> {
        let c = self.conn.lock().unwrap();
        // 旧库缺 aliases 列（v3）→ 重建索引表（build 会全量重插）
        let has_alias = match c.prepare("PRAGMA table_info(subjects_idx)") {
            Ok(mut st) => st
                .query_map([], |r| r.get::<_, String>(1))
                .map(|rows| rows.filter_map(|r| r.ok()).any(|n| n == "aliases"))
                .unwrap_or(false),
            Err(_) => false,
        };
        if !has_alias {
            c.execute_batch("DROP TABLE IF EXISTS subjects_fts; DROP TABLE IF EXISTS subjects_idx;")
                .map_err(|e| ProviderError::message(format!("archive schema migrate: {e}")))?;
        }
        // v4 → v5：缺 persons 表（person 实体数据未入库）→ 清空索引强制下次 build 全量重建
        let has_persons = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='persons'")
            .map(|mut st| {
                st.query_map([], |_| Ok(()))
                    .map(|mut rows| rows.next().is_some())
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if !has_persons {
            c.execute_batch(
                "DROP TABLE IF EXISTS subjects_fts; DROP TABLE IF EXISTS subjects_idx;
                 DROP TABLE IF EXISTS relations_idx; DROP TABLE IF EXISTS subject_persons;",
            )
            .map_err(|e| ProviderError::message(format!("archive schema migrate v5: {e}")))?;
        }
        for ddl in [
            DDL_SUBJECTS_IDX,
            DDL_FTS,
            DDL_RELATIONS,
            DDL_REL_INDEX,
            DDL_ARCHIVE_UPDATE,
            DDL_PERSONS,
            DDL_SUBJECT_PERSONS,
            DDL_SP_INDEX,
        ] {
            c.execute_batch(ddl)
                .map_err(|e| ProviderError::message(format!("archive schema failed: {e}")))?;
        }
        // persons 缺 aliases 列（旧库）→ ALTER TABLE ADD COLUMN（不重建，下次 build 填充）
        let has_p_aliases = match c.prepare("PRAGMA table_info(persons)") {
            Ok(mut st) => st
                .query_map([], |r| r.get::<_, String>(1))
                .map(|rows| rows.filter_map(|r| r.ok()).any(|n| n == "aliases"))
                .unwrap_or(false),
            Err(_) => false,
        };
        if !has_p_aliases {
            c.execute_batch("ALTER TABLE persons ADD COLUMN aliases TEXT")
                .map_err(|e| ProviderError::message(format!("archive schema migrate aliases: {e}")))?;
        }
        c.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .map_err(|e| ProviderError::message(format!("archive version failed: {e}")))?;
        Ok(())
    }

    /// 全量重建索引（单事务，10000 行/批；先清空再导入 → FTS5 重建）。
    /// 返回 (subjects, relations, persons, subject_persons) 行数。
    pub fn build(
        &self,
        subjects_path: &Path,
        relations_path: &Path,
        persons_path: &Path,
        subject_persons_path: &Path,
    ) -> Result<(usize, usize, usize, usize), ProviderError> {
        let c = self.conn.lock().unwrap();
        let tx = |s: &str| c.execute_batch(s).map_err(|e| ProviderError::message(format!("archive build {e}")));
        tx("BEGIN")?;
        let result = (|| -> Result<(usize, usize, usize, usize), ProviderError> {
            tx("DELETE FROM subjects_idx")?;
            tx("DELETE FROM subjects_fts")?;
            tx("DELETE FROM relations_idx")?;
            tx("DELETE FROM persons")?;
            tx("DELETE FROM subject_persons")?;
            let subj = Self::import_subjects(&c, subjects_path)?;
            let rel = Self::import_relations(&c, relations_path)?;
            let persons = Self::import_persons(&c, persons_path)?;
            let sp = Self::import_subject_persons(&c, subject_persons_path)?;
            c.execute(
                "INSERT INTO subjects_fts(rowid, name, name_cn, aliases)
                 SELECT id, name, name_cn, aliases FROM subjects_idx",
                [],
            )
            .map_err(|e| ProviderError::message(format!("archive fts rebuild: {e}")))?;
            Ok((subj, rel, persons, sp))
        })();
        match result {
            Ok(v) => {
                tx("COMMIT")?;
                // 单事务全量构建的 WAL 可达数百 MB：COMMIT 后立即 checkpoint 合并回主库并截断
                let _ = c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
                Ok(v)
            }
            Err(e) => {
                let _ = c.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn import_subjects(
        c: &rusqlite::Connection,
        path: &Path,
    ) -> Result<usize, ProviderError> {
        let file = std::fs::File::open(path)
            .map_err(|e| ProviderError::message(format!("archive subjects open: {e}")))?;
        let reader = std::io::BufReader::new(file);
        let mut lines = reader.lines();
        let mut batch: Vec<(u64, i64, String, Option<String>, String, u64)> = Vec::new();
        let mut count = 0usize;
        let mut offset = 0u64;
        loop {
            let Some(line) = lines.next() else { break };
            let line = line.map_err(|e| ProviderError::message(format!("archive read: {e}")))?;
            let item: ArchiveSubject = serde_json::from_str(&line).unwrap_or_default();
            let aliases = archive_aliases_str(&item);
            batch.push((
                item.id,
                item.subject_type.unwrap_or(0) as i64,
                item.name,
                item.name_cn,
                aliases,
                offset,
            ));
            count += 1;
            offset += line.len() as u64 + 1;
            if batch.len() >= 10000 {
                Self::insert_subject_batch(&c, &batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            Self::insert_subject_batch(&c, &batch)?;
        }
        Ok(count)
    }

    /// 事务内逐行插入（rusqlite execute 不支持多行参数）。
    fn insert_subject_batch(
        c: &rusqlite::Connection,
        batch: &[(u64, i64, String, Option<String>, String, u64)],
    ) -> Result<(), ProviderError> {
        for b in batch {
            // OR REPLACE：dump 可能存在重复 id（BangumiKomga executemany 同语义，后行覆盖）
            c.execute(
                "INSERT OR REPLACE INTO subjects_idx (id, type, name, name_cn, aliases, row_offset)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                rusqlite::params![b.0 as i64, b.1, b.2, b.3, b.4, b.5 as i64],
            )
            .map_err(|e| ProviderError::message(format!("archive insert: {e}")))?;
        }
        Ok(())
    }

    fn import_relations(c: &rusqlite::Connection, path: &Path) -> Result<usize, ProviderError> {
        #[derive(Deserialize)]
        struct Rel {
            #[serde(alias = "subjectId")]
            subject_id: u64,
            // archive 的 relation_type 为数字（1003=单行本 OFFPRINT，见
            // bangumi/common subject_relations.yml / BangumiKomga SubjectRelation）
            #[serde(default, alias = "relationType")]
            relation_type: Option<serde_json::Value>,
            #[serde(alias = "relatedSubjectId")]
            related_subject_id: u64,
        }
        let file = std::fs::File::open(path)
            .map_err(|e| ProviderError::message(format!("archive relations open: {e}")))?;
        let reader = std::io::BufReader::new(file);
        let mut lines = reader.lines();
        let mut batch: Vec<(u64, Option<String>, u64)> = Vec::new();
        let mut count = 0usize;
        loop {
            let Some(line) = lines.next() else { break };
            let line = line.map_err(|e| ProviderError::message(format!("archive read: {e}")))?;
            let Ok(r) = serde_json::from_str::<Rel>(&line) else {
                continue;
            };
            let relation_type = r.relation_type.as_ref().map(|v| match v {
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::String(st) => st.clone(),
                _ => String::new(),
            });
            let relation_type = relation_type.filter(|s| !s.is_empty());
            batch.push((r.subject_id, relation_type, r.related_subject_id));
            count += 1;
            if batch.len() >= 10000 {
                Self::insert_relation_batch(&c, &batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            Self::insert_relation_batch(&c, &batch)?;
        }
        Ok(count)
    }

    /// 事务内逐行插入。
    fn insert_relation_batch(
        c: &rusqlite::Connection,
        batch: &[(u64, Option<String>, u64)],
    ) -> Result<(), ProviderError> {
        for b in batch {
            c.execute(
                "INSERT INTO relations_idx (subject_id, relation_type, related_subject_id)
                 VALUES (?1,?2,?3)",
                rusqlite::params![b.0 as i64, b.1, b.2 as i64],
            )
            .map_err(|e| ProviderError::message(format!("archive insert: {e}")))?;
        }
        Ok(())
    }

    fn import_persons(c: &rusqlite::Connection, path: &Path) -> Result<usize, ProviderError> {
        let file = std::fs::File::open(path)
            .map_err(|e| ProviderError::message(format!("archive persons open: {e}")))?;
        let reader = std::io::BufReader::new(file);
        let mut lines = reader.lines();
        let mut batch: Vec<(u64, String, Option<String>, Option<i64>, String, String)> =
            Vec::new();
        let mut count = 0usize;
        loop {
            let Some(line) = lines.next() else { break };
            let line = line.map_err(|e| ProviderError::message(format!("archive read: {e}")))?;
            let Ok(p) = serde_json::from_str::<ArchivePerson>(&line) else { continue };
            let career = serde_json::to_string(&p.career).unwrap_or_default();
            batch.push((
                p.id,
                p.name,
                person_name_cn(p.infobox.as_deref()),
                p.r#type.map(|t| t as i64),
                career,
                serde_json::to_string(&person_aliases(p.infobox.as_deref())).unwrap_or_default(),
            ));
            count += 1;
            if batch.len() >= 10000 {
                Self::insert_person_batch(&c, &batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            Self::insert_person_batch(&c, &batch)?;
        }
        Ok(count)
    }

    fn insert_person_batch(
        c: &rusqlite::Connection,
        batch: &[(u64, String, Option<String>, Option<i64>, String, String)],
    ) -> Result<(), ProviderError> {
        for b in batch {
            c.execute(
                "INSERT OR REPLACE INTO persons (id, name, name_cn, type, career, aliases)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                rusqlite::params![b.0 as i64, b.1, b.2, b.3, b.4, b.5],
            )
            .map_err(|e| ProviderError::message(format!("archive persons insert: {e}")))?;
        }
        Ok(())
    }

    fn import_subject_persons(
        c: &rusqlite::Connection,
        path: &Path,
    ) -> Result<usize, ProviderError> {
        let file = std::fs::File::open(path)
            .map_err(|e| ProviderError::message(format!("archive subject-persons open: {e}")))?;
        let reader = std::io::BufReader::new(file);
        let mut lines = reader.lines();
        let mut batch: Vec<(u64, u64, Option<i64>, String)> = Vec::new();
        let mut count = 0usize;
        loop {
            let Some(line) = lines.next() else { break };
            let line = line.map_err(|e| ProviderError::message(format!("archive read: {e}")))?;
            let Ok(r) = serde_json::from_str::<SubjectPersonRel>(&line) else { continue };
            batch.push((
                r.subject_id,
                r.person_id,
                r.position.map(|v| v as i64),
                r.appear_eps.unwrap_or_default(),
            ));
            count += 1;
            if batch.len() >= 10000 {
                Self::insert_subject_person_batch(&c, &batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            Self::insert_subject_person_batch(&c, &batch)?;
        }
        Ok(count)
    }

    fn insert_subject_person_batch(
        c: &rusqlite::Connection,
        batch: &[(u64, u64, Option<i64>, String)],
    ) -> Result<(), ProviderError> {
        for b in batch {
            c.execute(
                "INSERT INTO subject_persons (subject_id, person_id, position, appear_eps)
                 VALUES (?1,?2,?3,?4)",
                rusqlite::params![b.0 as i64, b.1 as i64, b.2, b.3],
            )
            .map_err(|e| ProviderError::message(format!("archive subject_persons insert: {e}")))?;
        }
        Ok(())
    }

    pub fn validate(&self) -> bool {
        let c = self.conn.lock().unwrap();
        let subj = c
            .query_row("SELECT COUNT(*) FROM subjects_idx", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0);
        let rel = c
            .query_row("SELECT COUNT(*) FROM relations_idx", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0);
        subj > 0 && rel > 0
    }

    pub fn get_meta(&self, key: &str) -> Option<String> {
        let c = self.conn.lock().unwrap();
        c.query_row(
            "SELECT value FROM archive_update WHERE key=?1",
            [key],
            |r| r.get(0),
        )
        .ok()
    }

    pub fn set_meta(&self, key: &str, value: &str) {
        let c = self.conn.lock().unwrap();
        let _ = c.execute(
            "INSERT OR REPLACE INTO archive_update VALUES (?1,?2)",
            [key, value],
        );
    }

    fn read_line_at_offset(&self, offset: u64) -> Option<serde_json::Value> {
        let mm = self.mm.as_ref()?;
        let start = offset as usize;
        if start >= mm.len() {
            return None;
        }
        let end = mm[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| start + i)
            .unwrap_or(mm.len());
        let line = std::str::from_utf8(&mm[start..end]).ok()?;
        serde_json::from_str(line).ok()
    }

    fn offsets_by_ids(&self, ids: &[u64]) -> Vec<(u64, u64)> {
        let c = self.conn.lock().unwrap();
        if ids.is_empty() {
            return Vec::new();
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT id, row_offset FROM subjects_idx WHERE id IN ({placeholders})"
        );
        let mut stmt = match c.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let params = rusqlite::params_from_iter(ids.iter().map(|i| *i as i64));
        stmt.query_map(params, |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64))
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
        .unwrap_or_default()
    }

    /// 搜索（FTS5 → LIKE 回退；仅书籍 type=1；FTS 相关性顺序）。
    pub fn search(&self, query: &str) -> Vec<serde_json::Value> {
        self.touch();
        if self.mm.is_none() || query.trim().is_empty() {
            return Vec::new();
        }
        let c = self.conn.lock().unwrap();
        let ids: Vec<u64> = c
            .prepare(
                "SELECT f.rowid FROM subjects_fts f JOIN subjects_idx i ON i.id = f.rowid
                 WHERE subjects_fts MATCH ?1 AND i.type = 1",
            )
            .ok()
            .and_then(|mut stmt| {
                stmt.query_map([fts_query(query)], |r| r.get::<_, i64>(0))
                    .ok()
                    .map(|rows| rows.filter_map(|r| r.ok()).map(|v| v as u64).collect())
            })
            .unwrap_or_default();
        let ids = if ids.is_empty() {
            // FTS 无结果 → LIKE 回退（仅 type=1）
            let like = format!("%{query}%");
            c.prepare(
                "SELECT id FROM subjects_idx WHERE type=1 AND (name LIKE ?1 OR name_cn LIKE ?1 OR aliases LIKE ?1)",
            )
            .ok()
            .and_then(|mut stmt| {
                stmt.query_map([&like], |r| r.get::<_, i64>(0))
                    .ok()
                    .map(|rows| rows.filter_map(|r| r.ok()).map(|v| v as u64).collect())
            })
            .unwrap_or_default()
        } else {
            ids
        };
        drop(c);
        let offsets: std::collections::HashMap<u64, u64> =
            self.offsets_by_ids(&ids).into_iter().collect();
        let mut out: Vec<serde_json::Value> = Vec::new();
        for id in &ids {
            if let Some(&off) = offsets.get(id) {
                if let Some(v) = self.read_line_at_offset(off) {
                    out.push(v);
                }
            }
        }
        out
    }

    pub fn get_by_id(&self, subject_id: u64) -> Option<serde_json::Value> {
        self.touch();
        let c = self.conn.lock().unwrap();
        let offset = c
            .query_row(
                "SELECT row_offset FROM subjects_idx WHERE id=?1",
                [subject_id as i64],
                |r| r.get::<_, i64>(0),
            )
            .ok()? as u64;
        drop(c);
        self.read_line_at_offset(offset)
    }

    /// 关联条目（JOIN subjects_idx 输出 id/name/name_cn/type/relation）。
    pub fn get_related(&self, subject_id: u64) -> Vec<RelatedSubject> {
        self.touch();
        let c = self.conn.lock().unwrap();
        let mut stmt = match c.prepare(
            "SELECT s.id, s.name, s.name_cn, s.type, r.relation_type
             FROM relations_idx r
             JOIN subjects_idx s ON r.related_subject_id = s.id
             WHERE r.subject_id = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([subject_id as i64], |r| {
            Ok(RelatedSubject {
                id: r.get::<_, i64>(0)? as u64,
                name: r.get::<_, String>(1)?,
                name_cn: r.get::<_, Option<String>>(2)?,
                subject_type: r.get::<_, Option<i64>>(3)?.map(|v| v as i32),
                relation: map_relation_type(r.get::<_, Option<String>>(4)?),
            })
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
        .unwrap_or_default()
    }

    /// 某条目的 person 关联（JOIN persons 实体；按 position 排序）。
    pub fn get_persons(&self, subject_id: u64) -> Vec<PersonInfo> {
        self.touch();
        let c = self.conn.lock().unwrap();
        let mut stmt = match c.prepare(
            "SELECT sp.person_id, p.name, p.name_cn, p.type, p.career, sp.position, sp.appear_eps, p.aliases
             FROM subject_persons sp
             JOIN persons p ON p.id = sp.person_id
             WHERE sp.subject_id = ?1
             ORDER BY sp.position",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([subject_id as i64], |r| {
            Ok(PersonInfo {
                person_id: r.get::<_, i64>(0)? as u64,
                name: r.get::<_, String>(1)?,
                name_cn: r.get::<_, Option<String>>(2)?,
                person_type: r.get::<_, Option<i64>>(3)?.map(|v| v as i32),
                career: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                position: r.get::<_, Option<i64>>(5)?.map(|v| v as i32),
                appear_eps: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                aliases: r
                    .get::<_, Option<String>>(7)?
                    .map(|a| serde_json::from_str(&a).unwrap_or_default())
                    .unwrap_or_default(),
            })
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
        .unwrap_or_default()
    }

    /// 单 person 实体（无关联角色信息）。
    pub fn get_person(&self, person_id: u64) -> Option<PersonInfo> {
        let c = self.conn.lock().unwrap();
        c.query_row(
            "SELECT id, name, name_cn, type, career, aliases FROM persons WHERE id = ?1",
            [person_id as i64],
            |r| {
                Ok(PersonInfo {
                    person_id: r.get::<_, i64>(0)? as u64,
                    name: r.get::<_, String>(1)?,
                    name_cn: r.get::<_, Option<String>>(2)?,
                    person_type: r.get::<_, Option<i64>>(3)?.map(|v| v as i32),
                    career: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                    position: None,
                    appear_eps: String::new(),
                    aliases: r
                        .get::<_, Option<String>>(5)?
                        .map(|a| serde_json::from_str(&a).unwrap_or_default())
                        .unwrap_or_default(),
                })
            },
        )
        .ok()
    }
}

/// 提取 archive subject 的别名列表（infobox 别名/別名 的 v 值，空格分隔、去重保序）。
fn archive_aliases_str(item: &ArchiveSubject) -> String {
    fn collect(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) => {
                if !s.is_empty() && !out.contains(s) {
                    out.push(s.clone());
                }
            }
            serde_json::Value::Array(items) => {
                for it in items {
                    match it {
                        serde_json::Value::String(s) => {
                            if !s.is_empty() && !out.contains(s) {
                                out.push(s.clone());
                            }
                        }
                        obj => {
                            if let Some(v) = obj.get("v").and_then(|v| v.as_str()) {
                                if !v.is_empty() && !out.iter().any(|x| x == v) {
                                    out.push(v.to_string());
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<String> = Vec::new();
    for it in item.infobox_items() {
        let is_alias_key =
            it.key.as_deref() == Some("别名") || it.key.as_deref() == Some("別名");
        if let Some(v) = it.value.as_ref() {
            if is_alias_key {
                collect(v, &mut out);
            }
            // 嵌套别名：value 数组内 k=="别名"/"別名" 的子项（版本:* 等条目），
            // 对齐在线 nested 别名收集行为（无语言）。
            if let serde_json::Value::Array(items) = v {
                for sub in items {
                    let k = sub.get("k").and_then(|k| k.as_str());
                    if k == Some("别名") || k == Some("別名") || k == Some("版本名") {
                        if let Some(v) = sub.get("v").and_then(|v| v.as_str()) {
                            if !v.is_empty() && !out.iter().any(|x| x == v) {
                                out.push(v.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    out.join(" ")
}

/// FTS5 查询构造（trigram；term 前缀匹配 + OR）。
fn fts_query(user_input: &str) -> String {
    let q = user_input.trim().replace('"', "\"\"");
    if q.is_empty() {
        return "\"\"".to_string();
    }
    let terms: Vec<String> = q
        .split_whitespace()
        .map(|t| format!("\"{t}\"*"))
        .collect();
    if terms.is_empty() {
        "\"\"".to_string()
    } else {
        terms.join(" OR ")
    }
}

// ---------------------------------------------------------------------------
// ArchiveService —— 后台下载/构建/周期更新（原子替换共享槽）
// ---------------------------------------------------------------------------

pub struct BangumiArchiveService {
    store: Arc<RwLock<Option<Arc<BangumiArchiveStore>>>>,
    ready: Arc<AtomicBool>,
}

impl BangumiArchiveService {
    /// 启动后台任务（非阻塞）。`dir` 为数据目录（缺省 workDir/bangumi-archive）。
    pub fn start(
        config: &crate::config::BangumiArchiveConfig,
        http_client: reqwest::Client,
        dir: PathBuf,
    ) -> Arc<Self> {
        let store: Arc<RwLock<Option<Arc<BangumiArchiveStore>>>> =
            Arc::new(RwLock::new(None));
        let ready = Arc::new(AtomicBool::new(false));
        let svc = Arc::new(Self {
            store: store.clone(),
            ready: ready.clone(),
        });
        let interval_hours = config.update_interval_hours;
        let idle_release_secs = config.idle_release_secs.unwrap_or(0);
        tokio::spawn(async move {
            let _ = std::fs::create_dir_all(&dir);
            let db_path = dir.join("archive_index.db");
            let subjects_path = dir.join("subject.jsonlines");
            let relations_path = dir.join("subject-relations.jsonlines");
            let persons_path = dir.join("person.jsonlines");
            let subject_persons_path = dir.join("subject-persons.jsonlines");
            // 尝试打开现有索引 + mmap
            let mut current: Option<Arc<BangumiArchiveStore>> = BangumiArchiveStore::open(
                &db_path,
                &subjects_path,
                idle_release_secs,
            )
            .ok()
                .map(|s| {
                    let _ = s.init_schema();
                    Arc::new(s)
                })
                .filter(|s| s.validate());
            if let Some(s) = &current {
                *store.write().unwrap() = Some(s.clone());
                ready.store(true, Ordering::SeqCst);
                tracing::info!("bangumi archive ready (existing index)");
            }
            // 首次构建 / 周期检查
            let interval = std::time::Duration::from_secs(interval_hours.max(1) * 3600);
            loop {
                if current.is_none() {
                    match download_and_rebuild(
                        &http_client,
                        &dir,
                        &db_path,
                        &subjects_path,
                        &relations_path,
                        &persons_path,
                        &subject_persons_path,
                        idle_release_secs,
                    )
                    .await
                    {
                        Ok(new_store) => {
                            *store.write().unwrap() = Some(new_store.clone());
                            current = Some(new_store);
                            ready.store(true, Ordering::SeqCst);
                            tracing::info!("bangumi archive ready (rebuilt)");
                        }
                        Err(e) => {
                            tracing::warn!("bangumi archive build failed: {e}; falling back to online");
                        }
                    }
                } else if interval_hours > 0 {
                    match check_update(
                        &http_client,
                        &dir,
                        &db_path,
                        &subjects_path,
                        &relations_path,
                        &persons_path,
                        &subject_persons_path,
                        idle_release_secs,
                    )
                    .await {
                        Ok(Some(new_store)) => {
                            *store.write().unwrap() = Some(new_store);
                            tracing::info!("bangumi archive updated");
                        }
                        Ok(None) => {}
                        Err(e) => tracing::warn!("bangumi archive update check failed: {e}"),
                    }
                }
                tokio::time::sleep(interval).await;
            }
        });
        svc
    }

    /// 当前 store（未就绪/构建中 → None → 调用方回退在线 API）。
    pub fn get(&self) -> Option<Arc<BangumiArchiveStore>> {
        if !self.ready.load(Ordering::SeqCst) {
            return None;
        }
        self.store.read().unwrap().clone()
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
}

/// 检查 latest.json；需要更新时下载 → 解压 → 重建 → 返回新 store（原子替换）。
async fn check_update(
    http_client: &reqwest::Client,
    dir: &Path,
    db_path: &Path,
    subjects_path: &Path,
    relations_path: &Path,
    persons_path: &Path,
    subject_persons_path: &Path,
    idle_release_secs: u64,
) -> Result<Option<Arc<BangumiArchiveStore>>, ProviderError> {
    let meta = fetch_latest_meta(http_client).await?;
    let remote_time = meta.updated_at.unwrap_or_default();
    if remote_time.is_empty() {
        return Ok(None);
    }
    // 本地 last_updated 对比（远程早于等于本地 → 无需更新）
    let store = BangumiArchiveStore::open(db_path, subjects_path, idle_release_secs)
        .map_err(|e| ProviderError::message(format!("archive open: {e}")))?;
    let local = store.get_meta("last_updated");
    if let Some(local) = local {
        if remote_after(&remote_time, &local).is_none_or(|b| !b) {
            return Ok(None);
        }
    }
    download_and_rebuild(
        http_client,
        dir,
        db_path,
        subjects_path,
        relations_path,
        persons_path,
        subject_persons_path,
        idle_release_secs,
    )
    .await
    .map(Some)
}

fn remote_after(remote: &str, local: &str) -> Option<bool> {
    let parse = |s: &str| -> Option<i64> {
        let s = s.trim_end_matches('Z').replace('T', " ");
        chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S%.f")
            .ok()
            .or_else(|| chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok())
            .map(|dt| dt.and_utc().timestamp())
    };
    Some(parse(remote)? > parse(local)?)
}

/// 下载 zip → 解压 → 全量重建 → 返回新 store。
async fn download_and_rebuild(
    http_client: &reqwest::Client,
    dir: &Path,
    db_path: &Path,
    subjects_path: &Path,
    relations_path: &Path,
    persons_path: &Path,
    subject_persons_path: &Path,
    idle_release_secs: u64,
) -> Result<Arc<BangumiArchiveStore>, ProviderError> {
    let meta = fetch_latest_meta(http_client).await?;
    let url = meta
        .browser_download_url
        .filter(|u| !u.is_empty())
        .ok_or_else(|| ProviderError::message("archive: no download url"))?;
    let expected_size = meta.size;
    // zip 持久缓存（约 418MB）：同 size 时跳过重复下载（构建失败/重启不重下）
    let zip_path = dir.join("archive-latest.zip");
    let cached_ok = std::fs::metadata(&zip_path)
        .map(|m| expected_size.map_or(true, |s| m.len() == s))
        .unwrap_or(false);
    if !cached_ok {
        tracing::info!("downloading bangumi archive ({url})");
        // 大文件：独立长超时 client（全局 client 超时不适合），流式写盘防内存峰值
        let dl = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3600))
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ProviderError::message(format!("archive client: {e}")))?;
        let response = dl
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::message(format!("archive download: {e}")))?;
        if !response.status().is_success() {
            return Err(ProviderError::Status(
                CoreProviders::Bangumi,
                response.status(),
                response.text().await.unwrap_or_default(),
            ));
        }
        let tmp_zip = zip_path.with_extension("tmp");
        let mut file = std::io::BufWriter::new(
            std::fs::File::create(&tmp_zip)
                .map_err(|e| ProviderError::message(format!("archive create: {e}")))?,
        );
        let mut stream = response.bytes_stream();
        let mut downloaded = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| ProviderError::message(format!("archive body: {e}")))?;
            downloaded += chunk.len() as u64;
            std::io::Write::write_all(&mut file, &chunk)
                .map_err(|e| ProviderError::message(format!("archive write: {e}")))?;
        }
        std::io::Write::flush(&mut file)
            .map_err(|e| ProviderError::message(format!("archive flush: {e}")))?;
        if let Some(size) = expected_size {
            if downloaded != size {
                let _ = std::fs::remove_file(&tmp_zip);
                return Err(ProviderError::message(format!(
                    "archive size mismatch: {downloaded} != {size}"
                )));
            }
        }
        std::fs::rename(&tmp_zip, &zip_path)
            .map_err(|e| ProviderError::message(format!("archive move: {e}")))?;
    } else {
        tracing::info!("bangumi archive zip cache hit ({} bytes)", expected_size.unwrap_or(0));
    }
    // 解压（zip crate 读取时自动校验 CRC）
    let zip_file = std::fs::File::open(&zip_path)
        .map_err(|e| ProviderError::message(format!("archive open: {e}")))?;
    let mut archive = zip::ZipArchive::new(zip_file)
        .map_err(|e| ProviderError::message(format!("archive zip: {e}")))?;
    let mut got_subjects = false;
    let mut got_relations = false;
    let mut got_persons = false;
    let mut got_subject_persons = false;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| ProviderError::message(format!("archive zip entry: {e}")))?;
        let name = entry.name().to_string();
        let target = if name == "subject.jsonlines" {
            Some(subjects_path)
        } else if name == "subject-relations.jsonlines" {
            Some(relations_path)
        } else if name == "person.jsonlines" {
            Some(persons_path)
        } else if name == "subject-persons.jsonlines" {
            Some(subject_persons_path)
        } else {
            None
        };
        if let Some(target) = target {
            let mut out = std::io::BufWriter::new(std::fs::File::create(target).map_err(
                |e| ProviderError::message(format!("archive write {name}: {e}")),
            )?);
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| ProviderError::message(format!("archive extract {name}: {e}")))?;
            match name.as_str() {
                "subject.jsonlines" => got_subjects = true,
                "subject-relations.jsonlines" => got_relations = true,
                "person.jsonlines" => got_persons = true,
                "subject-persons.jsonlines" => got_subject_persons = true,
                _ => {}
            }
        }
    }
    drop(archive);
    if !got_subjects || !got_relations || !got_persons || !got_subject_persons {
        return Err(ProviderError::message(
            "archive zip missing subject / subject-relations / person / subject-persons jsonlines",
        ));
    }
    // 重建索引
    let new_store = BangumiArchiveStore::open(db_path, subjects_path, idle_release_secs)
        .map_err(|e| ProviderError::message(format!("archive open: {e}")))?;
    new_store
        .init_schema()
        .map_err(|e| ProviderError::message(format!("archive schema: {e}")))?;
    let (subj, rel, persons, sp) = new_store
        .build(subjects_path, relations_path, persons_path, subject_persons_path)
        .map_err(|e| ProviderError::message(format!("archive build: {e}")))?;
    new_store.set_meta("last_updated", &meta.updated_at.unwrap_or_default());
    tracing::info!(
        "bangumi archive rebuilt: {subj} subjects, {rel} relations, {persons} persons, {sp} subject-persons"
    );
    Ok(Arc::new(new_store))
}

async fn fetch_latest_meta(http_client: &reqwest::Client) -> Result<LatestMeta, ProviderError> {
    let response = http_client
        .get(ARCHIVE_LATEST_URL)
        .send()
        .await
        .map_err(|e| ProviderError::message(format!("archive meta: {e}")))?;
    if !response.status().is_success() {
        return Err(ProviderError::Status(
            CoreProviders::Bangumi,
            response.status(),
            response.text().await.unwrap_or_default(),
        ));
    }
    response
        .json()
        .await
        .map_err(|e| ProviderError::message(format!("archive meta json: {e}")))
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    /// person infobox 别名块解析：子条目 v、`、`/`,` 拆分、括号注音拆出、空值跳过
    #[test]
    fn person_aliases_parsed() {
        let ib = "{{Infobox Crt\r\n|简体中文名= 水树奈奈\r\n|别名={\r\n[第二中文名|]\r\n[英文名|]\r\n[日文名|近藤奈々 (こんどう なな)]\r\n[纯假名|みずき なな]\r\n[罗马字|Mizuki Nana]\r\n[昵称|奈々ちゃん、奈々さん、奈々様]\r\n}\r\n|性别= 女\r\n}}";
        let a = person_aliases(Some(ib));
        for w in [
            "近藤奈々 (こんどう なな)", "こんどう なな", "みずき なな", "Mizuki Nana",
            "奈々ちゃん", "奈々さん", "奈々様",
        ] {
            assert!(a.contains(&w.to_string()), "missing alias: {w}");
        }
        // 空子条目跳过
        assert!(!a.contains(&String::new()));
        assert!(!a.iter().any(|x| x.trim().is_empty()));
    }

    use super::*;

    fn write_subjects(path: &Path, rows: &[&str]) {
        let mut s = String::new();
        for r in rows {
            s.push_str(r);
            s.push('\n');
        }
        std::fs::write(path, s).unwrap();
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("komf-archive-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn parse_list_item_empty_v_skipped() {
        // [韩版|] 空 v → None（跳过）；[台版|無能力者娜娜] → kv；纯值 → 无 key
        assert!(parse_list_item("韩版|").is_none());
        assert_eq!(
            parse_list_item("台版|無能力者娜娜"),
            Some((Some("台版".to_string()), "無能力者娜娜".to_string()))
        );
        assert_eq!(parse_list_item("完结"), Some((None, "完结".to_string())));
        assert!(parse_list_item("").is_none());
        // 单行列表整体：空 v 项被过滤
        let items = parse_archive_infobox(
            "{{Infobox animanga/Manga\r\n|别名={\r\n[台版|無能力者娜娜]\r\n[韩版|]\r\n}\r\n|出版社= スクウェア・エニックス\r\n}}",
        );
        let alias = items.iter().find(|i| i.key.as_deref() == Some("别名")).unwrap();
        let arr = alias.value.as_ref().unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].get("k").and_then(|k| k.as_str()), Some("台版"));
        assert_eq!(arr[0].get("v").and_then(|v| v.as_str()), Some("無能力者娜娜"));
        let pub_ = items.iter().find(|i| i.key.as_deref() == Some("出版社")).unwrap();
        assert_eq!(pub_.value.as_ref().unwrap().as_str(), Some("スクウェア・エニックス"));
    }

    #[test]
    fn archive_store_search_and_get() {
        let dir = tmp_dir("store");
        let subjects = dir.join("subject.jsonlines");
        let relations = dir.join("subject-relations.jsonlines");
        write_subjects(
            &subjects,
            &[
                r#"{"id":1,"type":1,"name":"Test Manga","name_cn":"测试漫画","series":true,"platform":"漫画","tags":[{"name":"搞笑","count":3}],"infobox":[{"key":"别名","value":[{"k":"别名","v":"TM"}]}],"summary":"s","date":"2020-01-01","rank":1,"total":10,"score":8.5}"#,
                r#"{"id":2,"type":1,"name":"Other Book","name_cn":"","series":false,"platform":"小说"}"#,
                r#"{"id":3,"type":2,"name":"Anime","series":true}"#,
            ],
        );
        write_subjects(
            &relations,
            &[
                r#"{"subject_id":1,"relation_type":"单行本","related_subject_id":2}"#,
            ],
        );
        let persons = dir.join("person.jsonlines");
        let sp = dir.join("subject-persons.jsonlines");
        std::fs::write(&persons, "").unwrap();
        std::fs::write(&sp, "").unwrap();
        let store = BangumiArchiveStore::open(&dir.join("archive_index.db"), &subjects, 0).unwrap();
        store.init_schema().unwrap();
        let (subj, rel, _, _) = store.build(&subjects, &relations, &persons, &sp).unwrap();
        assert_eq!(subj, 3);
        assert_eq!(rel, 1);
        assert!(store.validate());

        // get_by_id → 转 BangumiSubject
        let v = store.get_by_id(1).expect("row 1");
        let arch: ArchiveSubject = serde_json::from_value(v).expect("deserialize");
        assert_eq!(arch.name, "Test Manga");
        let bs = arch.to_bangumi_subject();
        assert_eq!(bs.id, 1);
        assert_eq!(bs.rating.as_ref().and_then(|r| r.score), Some(8.5));
        assert!(bs.image.is_none());

        // search：FTS 命中
        let hits = store.search("Test Manga");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["id"], 1);
        // 仅 type=1；type=2 不进结果
        assert!(store.search("Anime").is_empty());

        // get_related
        let rels = store.get_related(1);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].id, 2);
        assert_eq!(rels[0].relation.as_deref(), Some("单行本"));
        assert_eq!(rels[0].name, "Other Book");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn archive_search_like_fallback() {
        let dir = tmp_dir("like");
        let subjects = dir.join("subject.jsonlines");
        let relations = dir.join("subject-relations.jsonlines");
        write_subjects(
            &subjects,
            &[r#"{"id":42,"type":1,"name":"魔法少女まどか","name_cn":"魔法少女小圆","series":true}"#],
        );
        std::fs::write(&relations, "").unwrap();
        let persons = dir.join("person.jsonlines");
        let sp = dir.join("subject-persons.jsonlines");
        std::fs::write(&persons, "").unwrap();
        std::fs::write(&sp, "").unwrap();
        let store = BangumiArchiveStore::open(&dir.join("archive_index.db"), &subjects, 0).unwrap();
        store.init_schema().unwrap();
        store.build(&subjects, &relations, &persons, &sp).unwrap();
        // FTS 无结果 → LIKE 回退
        let hits = store.search("魔法少女小圆");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["id"], 42);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 真实 archive 格式：platform 数字、infobox Wiki 字符串、relation_type 数字（1003=单行本）
    #[test]
    fn archive_real_format_roundtrip() {
        let dir = tmp_dir("real");
        let subjects = dir.join("subject.jsonlines");
        let relations = dir.join("subject-relations.jsonlines");
        write_subjects(
            &subjects,
            &[
                r#"{"id":495,"type":1,"name":"×××HOLiC","name_cn":"×××HOLiC","series":true,"platform":1001,"infobox":"{{Infobox animanga/Manga\n|中文名= ×××HOLiC\n|别名={\n[次元魔女]\n[XXXHOLIC]\n}\n|作者= CLAMP\n|版本:东立版={\n[版本名|xxxHOLiC 東立]\n[别名|xxxHOLiC 笼]\n[出版社|東立出版社]\n}\n|结束= 2011年3月号 (週刊ヤングマガジン)\n|发售日= 2003-07-25\n}}","tags":[{"name":"单行本","count":1}]}"#,
                r#"{"id":9,"type":1,"name":"×××HOLiC (9)","name_cn":"","series":false}"#,
            ],
        );
        write_subjects(
            &relations,
            &[r#"{"subject_id":495,"relation_type":1003,"related_subject_id":9}"#],
        );
        let persons = dir.join("person.jsonlines");
        let sp = dir.join("subject-persons.jsonlines");
        std::fs::write(&persons, "").unwrap();
        std::fs::write(&sp, "").unwrap();
        let store = BangumiArchiveStore::open(&dir.join("archive_index.db"), &subjects, 0).unwrap();
        store.init_schema().unwrap();
        let (subj, rel, _, _) = store.build(&subjects, &relations, &persons, &sp).unwrap();
        assert_eq!(subj, 2);
        assert_eq!(rel, 1);
        // platform 数字 → 字符串
        let v = store.get_by_id(495).expect("row");
        let arch: ArchiveSubject = serde_json::from_value(v).expect("deserialize");
        assert_eq!(arch.platform_str().as_deref(), Some("漫画"));
        assert_eq!(arch.name_cn.as_deref(), Some("×××HOLiC"));
        // infobox 字符串解析
        let items = arch.infobox_items();
        let alias = items
            .iter()
            .find(|i| i.key.as_deref() == Some("别名"))
            .expect("别名 key");
        let alias_v = alias.value.as_ref().and_then(|v| v.as_array()).expect("alias array");
        assert_eq!(alias_v.len(), 2);
        // 纯值列表项 → {"v": ..}（无 k，对齐在线 API → Localized 语言 None）
        assert_eq!(alias_v[0]["v"], "次元魔女");
        assert!(alias_v[0].get("k").is_none());
        let author = items.iter().find(|i| i.key.as_deref() == Some("作者")).expect("作者 key");
        assert_eq!(author.value.as_ref().and_then(|v| v.as_str()), Some("CLAMP"));
        // kv 列表项（版本:* 形式）→ {"k": .., "v": ..}
        let version = items
            .iter()
            .find(|i| i.key.as_deref() == Some("版本:东立版"))
            .expect("版本:东立版 key");
        let version_v = version.value.as_ref().and_then(|v| v.as_array()).expect("version array");
        assert_eq!(version_v[0]["k"], "版本名");
        assert_eq!(version_v[0]["v"], "xxxHOLiC 東立");
        // 数字 relation_type → 单行本
        let rels = store.get_related(495);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].relation.as_deref(), Some("单行本"));
        // 搜索（name_cn 命中）
        let hits = store.search("×××HOLiC");
        assert!(!hits.is_empty());
        // 别名作为搜索词可命中（FTS aliases 列）
        let alias_hits = store.search("次元魔女");
        assert!(alias_hits.iter().any(|v| v["id"] == 495));
        // LIKE 回退查别名：name/name_cn 均不含 XXXHOLIC（name_cn 是 ×××HOLiC），仅别名命中
        let alias_like = store.search("XXXHOLIC");
        assert!(alias_like.iter().any(|v| v["id"] == 495));
        // 嵌套别名（版本:* 条目内 k==别名）也进入 aliases 列并可检索
        let nested_alias = store.search("xxxHOLiC 笼");
        assert!(nested_alias.iter().any(|v| v["id"] == 495));
        // 版本名（版本:* 条目内 k==版本名）也进入 aliases 列并可检索
        let version_name = store.search("xxxHOLiC 東立");
        assert!(version_name.iter().any(|v| v["id"] == 495));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// persons 数据链：person.jsonlines（name + infobox 简体中文名）+
    /// subject-persons.jsonlines（position 角色）→ get_persons 返回实体。
    #[test]
    fn archive_persons_query() {
        let dir = tmp_dir("persons");
        let subjects = dir.join("subject.jsonlines");
        let relations = dir.join("subject-relations.jsonlines");
        let persons = dir.join("person.jsonlines");
        let sp = dir.join("subject-persons.jsonlines");
        write_subjects(&subjects, &[r#"{"id":268279,"type":1,"name":"チェンソーマン","name_cn":"链锯人","series":true}"#]);
        std::fs::write(&relations, "").unwrap();
        write_subjects(
            &persons,
            &[
                r#"{"id":23155,"name":"藤本タツキ","type":1,"career":["mangaka"],"infobox":"{{Infobox Crt\n|简体中文名= 藤本树\n|罗马字= Fujimoto Tatsuki\n}}"}"#,
                r#"{"id":1954,"name":"週刊少年ジャンプ","type":2,"career":["producer"],"infobox":"{{Infobox Crt\n|简体中文名= 周刊少年Jump\n}}"}"#,
            ],
        );
        write_subjects(
            &sp,
            &[
                r#"{"person_id":23155,"subject_id":268279,"position":2001,"appear_eps":""}"#,
                r#"{"person_id":1954,"subject_id":268279,"position":2005,"appear_eps":""}"#,
            ],
        );
        let store = BangumiArchiveStore::open(&dir.join("archive_index.db"), &subjects, 0).unwrap();
        if let Err(e) = store.init_schema() {
            panic!("init_schema failed: {e}");
        }
        if let Err(e) = store.build(&subjects, &relations, &persons, &sp) {
            panic!("build failed: {e}");
        }
        let ps = store.get_persons(268279);
        assert_eq!(ps.len(), 2);
        let author = ps.iter().find(|p| p.position == Some(2001)).unwrap();
        assert_eq!(author.name, "藤本タツキ");
        assert_eq!(author.name_cn.as_deref(), Some("藤本树"));
        assert_eq!(author.person_type, Some(1));
        assert_eq!(author.career, vec!["mangaka".to_string()]);
        let mag = ps.iter().find(|p| p.position == Some(2005)).unwrap();
        assert_eq!(mag.name_cn.as_deref(), Some("周刊少年Jump"));
        // get_person 单查
        let single = store.get_person(23155).expect("person");
        assert_eq!(single.name_cn.as_deref(), Some("藤本树"));
        // 无关联 → 空
        assert!(store.get_persons(999).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn archive_fts_query_builds() {
        assert_eq!(fts_query("魔法少女"), "\"魔法少女\"*");
        assert_eq!(
            fts_query("attack on titan"),
            "\"attack\"* OR \"on\"* OR \"titan\"*"
        );
        assert_eq!(fts_query(""), "\"\"");
    }
}
