//! bangumi/Archive 离线数据源 —— 对齐 BangumiKomga `bangumi_archive/`。
//!
//! 架构（v8：主数据直接入库，无 mmap）：
//! ```text
//! BangumiArchiveService（tokio::spawn 后台：下载 → 构建 → 周期更新）
//!   └── Arc<RwLock<Option<Arc<BangumiArchiveStore>>>>（原子替换共享槽）
//!         └── Mutex<rusqlite::Connection>
//!               ├── subjects:        主数据（id/type/name/name_cn/aliases/json 原文）
//!               ├── subjects_fts:    FTS5 unicode61 + 分析链预分词（content 指向 subjects）
//!               ├── relations_idx:   (subject_id, relation_type, related_subject_id)
//!               ├── persons:         person 实体（id/name/name_cn/type/career/aliases）
//!               ├── subject_persons: subject↔person 关联（含 position 角色）
//!               └── archive_update:  key/value（last_updated / fts_version）
//! ```
//!
//! 数据源：https://github.com/bangumi/Archive（release zip 约 418MB，解压后
//! jsonlines 约 1GB+）。导入成功后 jsonlines 自动删除（主数据已完整入库，
//! zip 缓存保留，更新/重建时重新解压）。查询全部走 SQLite（page cache
//! 由 PRAGMA cache_size 钳制）——v7 及之前的 mmap jsonlines 方案会让批量匹配
//! 的 RSS 随触过的文件页单调涨到 ~1GB，v8 起彻底移除。未就绪/构建中 → 调用方
//! 回退在线 API。

use crate::providers::bangumi::{BangumiInfoBoxItem, BangumiRating, BangumiSubject, BangumiTag};
use crate::providers::{CoreProviders, ProviderError};
use futures::StreamExt;
use komf_api_models::config::DownloadProgress;
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
                        let item = item.split(['\r', '\n', '}']).next().unwrap_or(item).trim();
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
    fn make_item(
        key: &str,
        value: Option<&str>,
        list: &[(Option<String>, String)],
    ) -> BangumiInfoBoxItem {
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
                } else if let Some(inner) =
                    value.strip_prefix('{').and_then(|v| v.strip_suffix('}'))
                {
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
            let s = trimmed.trim_start_matches('[').trim_end_matches(']').trim();
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

/// 关联条目（relations_idx JOIN subjects 输出）。
#[derive(Debug, Clone)]
pub struct RelatedSubject {
    pub id: u64,
    pub name: String,
    pub name_cn: Option<String>,
    pub subject_type: Option<i32>,
    pub relation: Option<String>,
}

// ---------------------------------------------------------------------------
// ArchiveDataStore —— SQLite 单库（主数据 + 索引）
// ---------------------------------------------------------------------------

/// v8 主数据表：jsonlines 原文整行入库（id 主键）。FTS content 表即本表。
const DDL_SUBJECTS: &str = "CREATE TABLE IF NOT EXISTS subjects (
    id      INTEGER PRIMARY KEY,
    type    INTEGER,
    name    TEXT,
    name_cn TEXT,
    aliases TEXT,
    json    TEXT
)";
const DDL_FTS: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS subjects_fts USING fts5(
    name,
    name_cn,
    aliases,
    content='subjects',
    content_rowid='id',
    tokenize='unicode61'
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

const SCHEMA_VERSION: i64 = 8;

/// FTS 召回候选上限 = limit*25（clamp 25..=1000）：每个候选都要按主键读出一整行
/// subject JSON 并解析，旧固定值 1000 对 limit=10~25 的搜索是纯浪费
/// （对齐 bookwalker/mangabaka 的"小候选集 + 精排"模式）。
fn fts_max_candidates(limit: usize) -> i64 {
    limit.saturating_mul(25).clamp(25, 1000) as i64
}
/// 查询 token 数上限：防御超长查询构造巨型 MATCH 表达式。
const FTS_MAX_QUERY_TERMS: usize = 60;
/// FTS 结构版本（写入 archive_update.fts_version）：分词器/写入方式变化时随
/// SCHEMA_VERSION 递增。init_schema 据此本地重建 FTS。
/// 注意：不能用 COUNT(*) 判 FTS 是否为空——外部内容表的无 MATCH 查询直通
/// content 表（FTS5 文档 4.4.4），COUNT 恒等于 subjects 行数。
/// "9"：修复 rebuild_fts 游标（旧 MAX(rowid) 直通 content 表导致仅索引首批
/// 50k 行）——既有 v8 库据此标记启动时全量重建 FTS。
const FTS_SCHEMA_VERSION: &str = "9";

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
}

impl BangumiArchiveStore {
    pub fn open(db_path: &Path) -> Result<Self, ProviderError> {
        let conn = rusqlite::Connection::open(db_path)
            .map_err(|e| ProviderError::message(format!("archive sqlite open failed: {e}")))?;
        // 读写在应用层已串行（同一把 Mutex），WAL 无收益；改用 DELETE：单事务构建的
        // rollback journal 随 COMMIT 自动删除。cache_size 钳住 page cache（32MB），
        // 批量匹配的大结果集不再像 v7 mmap 那样把 RSS 推到文件全量。
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-32000;",
        )
        .map_err(|e| ProviderError::message(format!("archive pragma failed: {e}")))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn init_schema(&self) -> Result<(), ProviderError> {
        let c = self.conn.lock().unwrap();
        // v8 迁移：v7 及更早的库主数据在 subjects_idx（无 json 列，行定位靠
        // row_offset + mmap jsonlines）。主数据入库后旧表整体废弃——检测到
        // subjects_idx 即 drop 全部数据表，由启动/build 流程用本地 jsonlines
        // （若齐全）或下次下载更新重建。这比逐版本 ALTER 更干净，且老库必然
        // 伴随全量重写（1GB+ 主数据搬家）。
        let legacy = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='subjects_idx'")
            .map(|mut st| {
                st.query_map([], |_| Ok(()))
                    .map(|mut rows| rows.next().is_some())
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if legacy {
            c.execute_batch(
                "DROP TABLE IF EXISTS subjects_fts;
                 DROP TABLE IF EXISTS subjects_idx;
                 DROP TABLE IF EXISTS relations_idx;
                 DROP TABLE IF EXISTS persons;
                 DROP TABLE IF EXISTS subject_persons;",
            )
            .map_err(|e| ProviderError::message(format!("archive schema migrate v8: {e}")))?;
            // 清掉旧 fts_version：rebuild_fts 成功后才落新标记（中途失败下次启动自愈）
            let _ = c.execute("DELETE FROM archive_update WHERE key='fts_version'", []);
        }
        for ddl in [
            DDL_SUBJECTS,
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
        // persons 缺 aliases 列（v5 旧库）→ ALTER TABLE ADD COLUMN（不重建，下次 build 填充）
        let has_p_aliases = match c.prepare("PRAGMA table_info(persons)") {
            Ok(mut st) => st
                .query_map([], |r| r.get::<_, String>(1))
                .map(|rows| rows.filter_map(|r| r.ok()).any(|n| n == "aliases"))
                .unwrap_or(false),
            Err(_) => false,
        };
        if !has_p_aliases {
            c.execute_batch("ALTER TABLE persons ADD COLUMN aliases TEXT")
                .map_err(|e| {
                    ProviderError::message(format!("archive schema migrate aliases: {e}"))
                })?;
        }
        c.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .map_err(|e| ProviderError::message(format!("archive version failed: {e}")))?;
        // FTS 版本迁移：fts_version 落后且主表有数据 → 本地分析重建
        // （subjects 保留原始标题列，重新分析即可，无需重新下载——do_update
        // 在远程未更新时会跳过重建）。
        let fts_ver: String = c
            .query_row(
                "SELECT value FROM archive_update WHERE key='fts_version'",
                [],
                |r| r.get(0),
            )
            .unwrap_or_default();
        let idx_rows = c
            .query_row("SELECT COUNT(*) FROM subjects", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0);
        if idx_rows > 0 && fts_ver != FTS_SCHEMA_VERSION {
            Self::rebuild_fts(&c)?;
        }
        Ok(())
    }

    /// 全量重建索引：导入保持单事务（10000 行/批）；FTS 重建拆出导入大事务，
    /// 对齐 ehentai build_fts 的分批模式（50_000 行/批、keyset 分页、
    /// 每批独立事务），避免 40-60 万行一次性 collect（~200-500MB）。
    /// 返回 (subjects, relations, persons, subject_persons) 行数。
    pub fn build(
        &self,
        subjects_path: &Path,
        relations_path: &Path,
        persons_path: &Path,
        subject_persons_path: &Path,
    ) -> Result<(usize, usize, usize, usize), ProviderError> {
        let c = self.conn.lock().unwrap();
        let tx = |s: &str| {
            c.execute_batch(s)
                .map_err(|e| ProviderError::message(format!("archive build {e}")))
        };
        tx("BEGIN")?;
        let result = (|| -> Result<(usize, usize, usize, usize), ProviderError> {
            tx("DELETE FROM subjects")?;
            // delete-all 特殊命令清空 FTS（原因见 rebuild_fts；普通 DELETE 在此场景
            // 会触发 SQLite 的 malformed 缺陷）
            tx("INSERT INTO subjects_fts(subjects_fts) VALUES('delete-all')")?;
            tx("DELETE FROM relations_idx")?;
            tx("DELETE FROM persons")?;
            tx("DELETE FROM subject_persons")?;
            let subj = Self::import_subjects(&c, subjects_path)?;
            let rel = Self::import_relations(&c, relations_path)?;
            let persons = Self::import_persons(&c, persons_path)?;
            let sp = Self::import_subject_persons(&c, subject_persons_path)?;
            Ok((subj, rel, persons, sp))
        })();
        let counts = match result {
            Ok(v) => {
                tx("COMMIT")?;
                // DELETE 模式下 rollback journal 随 COMMIT 自动清理，无需 checkpoint
                v
            }
            Err(e) => {
                let _ = c.execute_batch("ROLLBACK");
                return Err(e);
            }
        };
        // FTS 重建（分批事务，独立于导入大事务；成功后落 fts_version）
        Self::rebuild_fts(&c)?;
        Ok(counts)
    }

    fn import_subjects(c: &rusqlite::Connection, path: &Path) -> Result<usize, ProviderError> {
        let file = std::fs::File::open(path)
            .map_err(|e| ProviderError::message(format!("archive subjects open: {e}")))?;
        let reader = std::io::BufReader::new(file);
        let mut lines = reader.lines();
        let mut batch: Vec<(u64, i64, String, Option<String>, String, String)> = Vec::new();
        let mut count = 0usize;
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
                line,
            ));
            count += 1;
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
        batch: &[(u64, i64, String, Option<String>, String, String)],
    ) -> Result<(), ProviderError> {
        for b in batch {
            // OR REPLACE：dump 可能存在重复 id（BangumiKomga executemany 同语义，后行覆盖）
            c.execute(
                "INSERT OR REPLACE INTO subjects (id, type, name, name_cn, aliases, json)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                rusqlite::params![b.0 as i64, b.1, b.2, b.3, b.4, b.5],
            )
            .map_err(|e| ProviderError::message(format!("archive insert: {e}")))?;
        }
        Ok(())
    }

    /// 读出 subjects 一页的原始标题并跑分析链（keyset 分页：id > after_id
    /// ORDER BY id LIMIT n），得到 FTS 行
    /// (id, name_tokens, name_cn_tokens, aliases_tokens)。
    fn subject_fts_rows_page(
        c: &rusqlite::Connection,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<(i64, String, Option<String>, Option<String>)>, ProviderError> {
        let mut stmt = c
            .prepare(
                "SELECT id, name, name_cn, aliases FROM subjects \
                 WHERE id > ?1 ORDER BY id LIMIT ?2",
            )
            .map_err(|e| ProviderError::message(format!("archive fts rows: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![after_id, limit], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| ProviderError::message(format!("archive fts rows: {e}")))?
            .filter_map(|r| r.ok())
            .map(|(id, name, name_cn, aliases)| {
                // aliases 为空格分隔的别名串（archive_aliases_str）：空格即
                // standard_tokenize 的分隔符，整条分析与逐条分析结果相同。
                let analyze = |s: &str| crate::util::index_analyze_terms(s).join(" ");
                (
                    id,
                    analyze(&name),
                    name_cn.as_deref().map(analyze),
                    aliases.as_deref().map(analyze),
                )
            })
            .collect();
        Ok(rows)
    }

    /// 事务内写入 FTS（调用方需已在事务中，或单次写入场景）。
    fn insert_fts_rows(
        c: &rusqlite::Connection,
        rows: &[(i64, String, Option<String>, Option<String>)],
    ) -> Result<(), ProviderError> {
        let mut stmt = c
            .prepare("INSERT INTO subjects_fts(rowid, name, name_cn, aliases) VALUES (?1,?2,?3,?4)")
            .map_err(|e| ProviderError::message(format!("archive fts insert: {e}")))?;
        for (id, name, name_cn, aliases) in rows {
            stmt.execute(rusqlite::params![id, name, name_cn, aliases])
                .map_err(|e| ProviderError::message(format!("archive fts insert: {e}")))?;
        }
        Ok(())
    }

    /// 重建 FTS（init_schema 迁移路径与 build 共用）：
    /// delete-all 清空 → 清除 fts_version 标记（中途失败下次启动自愈重建）→
    /// keyset 分页分批事务写入（分析产物仅本批驻留内存）→ 成功后落 fts_version。
    fn rebuild_fts(c: &rusqlite::Connection) -> Result<(), ProviderError> {
        // delete-all 特殊命令清空 FTS 索引。不能用 `DELETE FROM subjects_fts`：
        // FTS5 外部内容表经"drop 后以同名重建"后，普通 DELETE 的全表扫描路径
        // 会报 database disk image is malformed（SQLite 缺陷，3.45/3.50 均复现），
        // delete-all 走索引层清空，无此问题。
        c.execute_batch(
            "INSERT INTO subjects_fts(subjects_fts) VALUES('delete-all');
             DELETE FROM archive_update WHERE key='fts_version'",
        )
        .map_err(|e| ProviderError::message(format!("archive fts rebuild: {e}")))?;
        // 分批写入：50_000 行/批、每批独立事务（对齐 ehentai build_fts）
        const BATCH: i64 = 50_000;
        let mut last_id: i64 = -1;
        loop {
            let batch_result = (|| -> Result<(i64, i64), ProviderError> {
                c.execute_batch("BEGIN")
                    .map_err(|e| ProviderError::message(format!("archive fts rebuild: {e}")))?;
                let result = (|| -> Result<(i64, i64), ProviderError> {
                    let rows = Self::subject_fts_rows_page(c, last_id, BATCH)?;
                    // keyset 游标取本批（subjects 表按 id 升序）最大 id。
                    // 不能用 MAX(rowid) FROM subjects_fts——外部内容 FTS 的无 MATCH
                    // 查询直通 content 表，MAX 返回的是全表最大 id 而非本批进度，
                    // 首批后游标跳到末尾、仅索引第一批（v7 起潜伏，v8 修复）。
                    let page_last = rows.last().map(|r| r.0).unwrap_or(last_id);
                    Self::insert_fts_rows(c, &rows)?;
                    Ok((rows.len() as i64, page_last))
                })();
                match result {
                    Ok(v) => {
                        c.execute_batch("COMMIT").map_err(|e| {
                            ProviderError::message(format!("archive fts rebuild: {e}"))
                        })?;
                        Ok(v)
                    }
                    Err(e) => {
                        let _ = c.execute_batch("ROLLBACK");
                        Err(e)
                    }
                }
            })();
            let (n, page_last) = batch_result?;
            if n == 0 {
                break;
            }
            last_id = page_last;
        }
        c.execute(
            "INSERT OR REPLACE INTO archive_update VALUES ('fts_version', ?1)",
            [FTS_SCHEMA_VERSION],
        )
        .map_err(|e| ProviderError::message(format!("archive fts rebuild: {e}")))?;
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
        let mut batch: Vec<(u64, String, Option<String>, Option<i64>, String, String)> = Vec::new();
        let mut count = 0usize;
        loop {
            let Some(line) = lines.next() else { break };
            let line = line.map_err(|e| ProviderError::message(format!("archive read: {e}")))?;
            let Ok(p) = serde_json::from_str::<ArchivePerson>(&line) else {
                continue;
            };
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
            let Ok(r) = serde_json::from_str::<SubjectPersonRel>(&line) else {
                continue;
            };
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
            .query_row("SELECT COUNT(*) FROM subjects", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0);
        let rel = c
            .query_row("SELECT COUNT(*) FROM relations_idx", [], |r| {
                r.get::<_, i64>(0)
            })
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

    /// 按主键批量取 json 原文（IN 查询），返回 (id, json) 列表（顺序由 SQLite 决定，
    /// 调用方按需重排）。json 列即 jsonlines 原文，消费侧自行 serde 解析。
    fn json_by_ids(&self, ids: &[u64]) -> Vec<(u64, String)> {
        let c = self.conn.lock().unwrap();
        if ids.is_empty() {
            return Vec::new();
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!("SELECT id, json FROM subjects WHERE id IN ({placeholders})");
        let mut stmt = match c.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let params = rusqlite::params_from_iter(ids.iter().map(|i| *i as i64));
        stmt.query_map(params, |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
        .unwrap_or_default()
    }

    /// 搜索（FTS5 渐进前缀 AND；仅书籍 type=1）。
    /// FTS 未命中即返回空：v6 分析链索引（索引/查询同一归一化链）已覆盖
    /// LIKE 曾兜底的场景，不再保留三列 LIKE 全表扫回退（且该回退无 LIMIT，
    /// 命中后还会全量读出）。
    pub fn search(&self, query: &str, limit: usize) -> Vec<serde_json::Value> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let ids = {
            let c = self.conn.lock().unwrap();
            fts_search_ids(&c, query, limit)
        };
        if ids.is_empty() {
            return Vec::new();
        }
        let rows = self.json_by_ids(&ids);
        let by_id: std::collections::HashMap<u64, String> = rows.into_iter().collect();
        let mut out: Vec<serde_json::Value> = Vec::new();
        for id in &ids {
            if let Some(json) = by_id.get(id) {
                if let Ok(v) = serde_json::from_str(json) {
                    out.push(v);
                }
            }
        }
        out
    }

    pub fn get_by_id(&self, subject_id: u64) -> Option<serde_json::Value> {
        let json: String = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT json FROM subjects WHERE id=?1",
                [subject_id as i64],
                |r| r.get(0),
            )
            .ok()?;
        serde_json::from_str(&json).ok()
    }

    /// 关联条目（JOIN subjects 输出 id/name/name_cn/type/relation）。
    pub fn get_related(&self, subject_id: u64) -> Vec<RelatedSubject> {
        let c = self.conn.lock().unwrap();
        let mut stmt = match c.prepare(
            "SELECT s.id, s.name, s.name_cn, s.type, r.relation_type
             FROM relations_idx r
             JOIN subjects s ON r.related_subject_id = s.id
             WHERE r.subject_id = ?1
             ORDER BY r.related_subject_id ASC",
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
        let is_alias_key = it.key.as_deref() == Some("别名") || it.key.as_deref() == Some("別名");
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

/// FTS5 渐进前缀 AND 查询。
///
/// 查询侧分析链 token 先全量 AND；未命中时逐层丢尾部 token 重试
/// （komga 系列名常带归档标题没有的装饰后缀：系列/卷数/话数，如
/// "葬送的芙莉莲系列" → 尾部 token 命中不了时，前缀 token 集仍可命中
/// "葬送的芙莉莲"）。索引侧 unigram+bigram 保证查询前缀 token 集必有解析。
fn fts_search_ids(c: &rusqlite::Connection, query: &str, limit: usize) -> Vec<u64> {
    let tokens = crate::util::search_analyze(query);
    if tokens.is_empty() {
        return Vec::new();
    }
    let tokens = &tokens[..tokens.len().min(FTS_MAX_QUERY_TERMS)];
    let mut stmt = match c.prepare(
        "SELECT f.rowid FROM subjects_fts f JOIN subjects i ON i.id = f.rowid
         WHERE subjects_fts MATCH ?1 AND i.type = 1
         ORDER BY rank LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    // 单 token 查询（纯拉丁词等）直接查询；多 token 未命中时逐层丢尾部 token
    // 重试，但不低于 droppable_floor（拉丁整词子句永不放宽、前缀至少两 token，
    // 避免退化为单词查询冲爆候选上限——对齐 kmrs cjk_droppable_floor 的变体）。
    let floor = if tokens.len() <= 1 {
        1
    } else {
        crate::util::droppable_floor(tokens)
    };
    for n in (floor..=tokens.len()).rev() {
        let expr = fts_and_expr(&tokens[..n]);
        let rows = stmt
            .query_map(rusqlite::params![expr, fts_max_candidates(limit)], |r| {
                r.get::<_, i64>(0)
            })
            .ok()
            .map(|rows| {
                rows.filter_map(|r| r.ok())
                    .map(|v| v as u64)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !rows.is_empty() {
            return rows;
        }
    }
    Vec::new()
}

/// tokens 构造 FTS5 AND 表达式（逐 token 引号包裹，`"` 双写转义）。
fn fts_and_expr(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

// ---------------------------------------------------------------------------
// ArchiveService —— 后台下载/构建/周期更新（原子替换共享槽）
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct BangumiArchiveService {
    store: Arc<RwLock<Option<Arc<BangumiArchiveStore>>>>,
    ready: Arc<AtomicBool>,
    /// 打开现有数据的启动任务是否已完成（无论成败）。
    /// 周期更新循环等待该标志，避免打开任务与 do_update 的整文件替换撞车。
    opened: Arc<AtomicBool>,
    /// 周期自动更新循环是否已启动（防重复 spawn）。
    auto_update_started: Arc<AtomicBool>,
    http_client: reqwest::Client,
    data_dir: PathBuf,
    /// 更新互斥：下载/构建期间忽略新的触发（复用进行中的进度流）。
    download_in_progress: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<tokio::sync::watch::Sender<Option<DownloadProgress>>>>>,
}

/// 同数据目录的全局单例注册表：WebUI 保存配置触发热重载时会整体重建
/// ProvidersModule，若无单例保护会重复启动服务——两个实例各自做 FTS 迁移重建
/// 撞 database is locked、周期更新循环叠加（循环持有 Arc<Self> 永不退出）。
/// 键 = 数据目录（canonicalize 失败时原样）。
fn instances() -> &'static std::sync::Mutex<
    std::collections::HashMap<PathBuf, std::sync::Weak<BangumiArchiveService>>,
> {
    static INSTANCES: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<PathBuf, std::sync::Weak<BangumiArchiveService>>,
        >,
    > = std::sync::OnceLock::new();
    INSTANCES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

impl BangumiArchiveService {
    /// 启动后台任务（非阻塞）。`dir` 为数据目录（缺省 workDir/bangumi-archive）。
    /// 同 `dir` 全局单例：重复调用返回已存在实例（热重载重建模块时复用）。
    /// 周期自动更新由编排方（app_context）统一调用 `start_auto_update` 启动，
    /// 服务本身不自行调度。
    pub fn start(
        _config: &crate::config::BangumiArchiveConfig,
        http_client: reqwest::Client,
        dir: PathBuf,
    ) -> Arc<Self> {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        {
            let reg = instances().lock().unwrap();
            if let Some(existing) = reg.get(&key).and_then(|w| w.upgrade()) {
                tracing::debug!(
                    "bangumi archive: reuse existing service for {}",
                    key.display()
                );
                return existing;
            }
        }
        let store: Arc<RwLock<Option<Arc<BangumiArchiveStore>>>> = Arc::new(RwLock::new(None));
        let ready = Arc::new(AtomicBool::new(false));
        let opened = Arc::new(AtomicBool::new(false));
        let svc = Arc::new(Self {
            store: store.clone(),
            ready: ready.clone(),
            opened: opened.clone(),
            auto_update_started: Arc::new(AtomicBool::new(false)),
            http_client: http_client.clone(),
            data_dir: dir.clone(),
            download_in_progress: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Mutex::new(None)),
        });
        instances()
            .lock()
            .unwrap()
            .insert(key, Arc::downgrade(&svc));
        // 打开 + init_schema（v8 迁移可能 drop 旧表；FTS 本地重建是同步重 IO）
        // 跑在阻塞线程上，不占用 tokio worker。
        crate::util::heavy_pool::spawn_heavy(move || {
            let _ = std::fs::create_dir_all(&dir);
            let db_path = dir.join("archive_index.db");
            // 清理上次"构建完成 → rename 原子替换"之间崩溃/被杀残留的临时库
            // （v8 起 db.tmp 可达 1GB+）。正常路径 build_db_then_swap 开头会清，
            // 但 up-to-date 跳过时不会执行；启动时刻 tmp 绝不可能是"正在使用"。
            let tmp_db = db_path.with_extension("db.tmp");
            if tmp_db.exists() {
                if let Err(e) = std::fs::remove_file(&tmp_db) {
                    tracing::warn!("bangumi archive: remove stale {}: {e}", tmp_db.display());
                } else {
                    tracing::info!("bangumi archive: removed stale {}", tmp_db.display());
                }
            }
            let subjects_path = dir.join("subject.jsonlines");
            let relations_path = dir.join("subject-relations.jsonlines");
            let persons_path = dir.join("person.jsonlines");
            let subject_persons_path = dir.join("subject-persons.jsonlines");
            if let Ok(s) = BangumiArchiveStore::open(&db_path) {
                let _ = s.init_schema();
                // v8 迁移/首次解压后：库未就绪但本地 jsonlines 齐全 → 直接后台重建
                // （init_schema 已把旧表 drop 掉；do_update 在远程数据未更新时会
                // 跳过重建，不能指望更新流程自愈本地迁移）。
                if !s.validate()
                    && subjects_path.exists()
                    && relations_path.exists()
                    && persons_path.exists()
                    && subject_persons_path.exists()
                {
                    match s.build(
                        &subjects_path,
                        &relations_path,
                        &persons_path,
                        &subject_persons_path,
                    ) {
                        Ok((subj, rel, persons, sp)) => {
                            tracing::info!(
                                "bangumi archive rebuilt locally ({subj} subjects, {rel} relations, {persons} persons, {sp} subject_persons)"
                            );
                            // 主数据已完整入库（build 单事务提交 + FTS 重建成功）——
                            // jsonlines 是 ~1GB 的纯冗余，删除省磁盘（更新流程会
                            // 从 zip 缓存/远程重新解压）。
                            remove_extracted_jsonlines(&dir);
                        }
                        Err(e) => {
                            tracing::warn!("bangumi archive local rebuild failed: {e}");
                        }
                    }
                }
                if s.validate() {
                    *store.write().unwrap() = Some(Arc::new(s));
                    ready.store(true, Ordering::SeqCst);
                    tracing::info!("bangumi archive ready (existing index)");
                }
            }
            opened.store(true, Ordering::SeqCst);
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
            // 等打开现有数据完成后再开始周期检查（见 `opened` 注释：防 1224 竞态）
            while !svc.opened.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            let interval = std::time::Duration::from_secs(interval_hours.max(1) * 3600);
            let retry_delay = std::time::Duration::from_secs(15 * 60);
            loop {
                // 离线源更新全局互斥：与其他离线源（ehentai/mangabaka/bookwalker）
                // 强制串行，避免并发构建叠加内存峰值
                let _permit = crate::util::download::offline_update_permit().await;
                let ok = Self::wait_update(&svc).await;
                tokio::time::sleep(if ok { interval } else { retry_delay }).await;
            }
        });
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

    /// 最近一次成功更新的数据时间（远程 release `updated_at`，即离线数据版本）；
    /// 未下载过 → None（WebUI 显示「未下载」）。对应 `MangaBakaDbMetadata.timestamp` 语义。
    pub fn download_timestamp(&self) -> Option<String> {
        std::fs::read_to_string(self.data_dir.join("last_updated"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.get()
                    .and_then(|s| s.get_meta("last_updated"))
                    .filter(|s| !s.is_empty())
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
            // 更新主体是同步重 IO（zip 解压 + 全量建库），整段跑在阻塞线程上
            //（block_on），不占用 tokio worker
            crate::util::heavy_pool::spawn_heavy(move || {
                let result = tokio::runtime::Handle::current().block_on(this.do_update(&sender));
                // 失败必须发射 ErrorEvent 终态，否则路由层 watch 流等不到 Finished/Error 会一直挂起
                if let Err(e) = &result {
                    tracing::error!("bangumi archive update failed: {e}");
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

    /// 单次更新：远程 meta → 本地 last_updated 对比（不新则跳过，不重复下载）→
    /// 下载 → 解压 → 重建 → 原子替换 → 持久化更新时间。
    async fn do_update(
        &self,
        sender: &tokio::sync::watch::Sender<Option<DownloadProgress>>,
    ) -> Result<(), String> {
        let emit = |sender: &tokio::sync::watch::Sender<Option<DownloadProgress>>,
                    event: DownloadProgress| {
            let _ = sender.send(Some(event));
        };
        let db_path = self.data_dir.join("archive_index.db");
        let subjects_path = self.data_dir.join("subject.jsonlines");
        let relations_path = self.data_dir.join("subject-relations.jsonlines");
        let persons_path = self.data_dir.join("person.jsonlines");
        let subject_persons_path = self.data_dir.join("subject-persons.jsonlines");

        // 0. 打开任务未完成（启动/热重载后数秒内）：跳过本次检查。
        //    此时本地索引已在打开流程中，全量重建既无必要，也会与打开任务竞争
        //    同一批文件（Windows 下解压覆盖被打开的 archive_index.db 需 rename 重试）。
        //    打开失败/空库（opened 后仍未就绪）不在此列——继续走全量下载重建以自愈。
        if self.get().is_none() && !self.opened.load(Ordering::SeqCst) {
            emit(
                sender,
                DownloadProgress::ProgressEvent {
                    total: 0,
                    completed: 0,
                    info: Some(
                        "bangumi archive index is still opening; skipping update check".to_string(),
                    ),
                },
            );
            emit(sender, DownloadProgress::FinishedEvent);
            return Ok(());
        }

        // 1. 远程 meta（latest.json：下载地址 / 更新时间 / 大小）
        emit(
            sender,
            DownloadProgress::ProgressEvent {
                total: 0,
                completed: 0,
                info: Some("checking bangumi archive update".to_string()),
            },
        );
        let meta = fetch_latest_meta(&self.http_client)
            .await
            .map_err(|e| format!("bangumi archive meta: {e}"))?;
        let remote_time = meta.updated_at.clone().unwrap_or_default();

        // 2. 更新检测：本地索引存在且远程不晚于本地 → 跳过（不重复下载）。
        //    远程无 updated_at 时无从判断（对齐旧 check_update：视为无需更新）。
        if let Some(current) = self.get() {
            if remote_time.is_empty() {
                emit(
                    sender,
                    DownloadProgress::ProgressEvent {
                        total: 0,
                        completed: 0,
                        info: Some("bangumi archive has no update info; skipping".to_string()),
                    },
                );
                emit(sender, DownloadProgress::FinishedEvent);
                return Ok(());
            }
            let local = current.get_meta("last_updated");
            if let Some(local) = local {
                if remote_after(&remote_time, &local).is_none_or(|b| !b) {
                    emit(
                        sender,
                        DownloadProgress::ProgressEvent {
                            total: 0,
                            completed: 0,
                            info: Some("bangumi archive is up to date".to_string()),
                        },
                    );
                    emit(sender, DownloadProgress::FinishedEvent);
                    return Ok(());
                }
            }
        }

        // 3. 下载 → 解压 → 重建（进度事件流；失败保留旧库，provider 不失效）。
        //    重建前先出槽置未就绪；在途搜索短暂持有旧 store Arc 的窗口由
        //    解压写入的短重试兜底（见 create_extracted_file）。
        {
            let mut guard = self.store.write().unwrap();
            if guard.is_some() {
                *guard = None;
                self.ready.store(false, Ordering::SeqCst);
            }
        }
        let emit_c = |event: DownloadProgress| emit(sender, event);
        let emit_ref: Option<&(dyn Fn(DownloadProgress) + Send + Sync)> = Some(&emit_c);
        let new_store = download_and_rebuild(
            &self.data_dir,
            &db_path,
            &subjects_path,
            &relations_path,
            &persons_path,
            &subject_persons_path,
            &meta,
            emit_ref,
        )
        .await
        .map_err(|e| format!("bangumi archive rebuild: {e}"))?;

        // 4. 原子替换 + 持久化更新时间（download_timestamp 在 store 未打开时也能读）
        *self.store.write().unwrap() = Some(new_store);
        self.ready.store(true, Ordering::SeqCst);
        let _ = std::fs::write(self.data_dir.join("last_updated"), &remote_time);
        tracing::info!("bangumi archive ready (rebuilt)");
        emit(sender, DownloadProgress::FinishedEvent);
        Ok(())
    }
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

/// 下载 zip（支持断点续传）：`.tmp` 已存在时 Range 续传；GitHub release asset 支持 Range。
/// 大小校验用 expected_size（GitHub API 的 size，即完整大小）：
/// 206 续传时完整大小 = 已有 + 本次写入；200 全量时 = 本次写入。
/// `on_progress(written_total, expected_total)`：下载字节进度回调（断点续传时 written 为累计值）。
async fn download_zip(
    dl: &reqwest::Client,
    url: &str,
    tmp_zip: &Path,
    expected_size: Option<u64>,
    on_progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
) -> Result<(), ProviderError> {
    use std::io::Write;
    let existing = std::fs::metadata(tmp_zip).map(|m| m.len()).unwrap_or(0);
    let mut req = dl.get(url);
    if existing > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let response = req.send().await?;
    if !response.status().is_success() {
        return Err(ProviderError::Status(
            CoreProviders::Bangumi,
            response.status(),
            response.text().await.unwrap_or_default(),
        ));
    }
    let resumed = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    // 进度总大小在消费流之前读取（bytes_stream 会 move response）：
    // 206 时 Content-Length 是剩余字节，完整大小 = existing + 剩余
    let progress_total = if resumed {
        response
            .content_length()
            .map(|l| existing + l)
            .or_else(|| expected_size)
    } else {
        response.content_length().or(expected_size)
    };
    let mut file = std::io::BufWriter::new(
        if resumed {
            std::fs::OpenOptions::new().append(true).open(tmp_zip)
        } else {
            std::fs::File::create(tmp_zip)
        }
        .map_err(|e| ProviderError::message(format!("archive create: {e}")))?,
    );
    let mut stream = response.bytes_stream();
    let mut written = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| ProviderError::message(format!("archive body interrupted: {e}")))?;
        written += chunk.len() as u64;
        file.write_all(&chunk)
            .map_err(|e| ProviderError::message(format!("archive write: {e}")))?;
        if let Some(on_progress) = on_progress {
            on_progress(existing + written, progress_total.unwrap_or(0));
        }
    }
    file.flush()
        .map_err(|e| ProviderError::message(format!("archive flush: {e}")))?;
    // 大小校验：206 时 Content-Length 是剩余字节，用 expected_size（完整大小）判定
    if let Some(size) = expected_size {
        let base = if resumed { existing } else { 0 };
        if base + written != size {
            let _ = std::fs::remove_file(tmp_zip);
            return Err(ProviderError::message(format!(
                "archive size mismatch: {} != {size}",
                base + written
            )));
        }
    }
    Ok(())
}

/// zip 下载指数退避重试（公共工具）：首次 + 3 次（5s/30s/120s）；中断续传只补剩余字节。
async fn download_zip_with_retry(
    dl: &reqwest::Client,
    url: &str,
    tmp_zip: &Path,
    expected_size: Option<u64>,
    on_progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
) -> Result<(), ProviderError> {
    crate::util::download::with_retry("bangumi archive download", || {
        download_zip(dl, url, tmp_zip, expected_size, on_progress)
    })
    .await
}

/// 覆盖写归档解压目标文件（短重试）：Windows 下目标被在途读者占用时
/// `File::create` 可能失败；最多 5 次 × 递增 200ms
/// 等待在途搜索释放句柄。await 重试安全：do_update 整体跑在 spawn_blocking 线程
/// （block_on）上，future 无需 Send；`entry` 借用 `archive` 跨 await 仅影响 auto trait，
/// 不影响正确性。仅在 create 阶段重试（entry 流未被消费，重试不产生坏数据）。
async fn create_extracted_file(target: &Path, name: &str) -> Result<std::fs::File, ProviderError> {
    let mut last_err = None;
    for attempt in 0..5 {
        match std::fs::File::create(target) {
            Ok(file) => return Ok(file),
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(200 * (attempt + 1))).await;
            }
        }
    }
    Err(ProviderError::message(format!(
        "archive write {name}: {}",
        last_err.unwrap()
    )))
}

/// 构建索引到 `db_path.tmp`，成功后 rename 原子替换正式文件（对齐 ehentai 整文件替换模式）：
/// - 坏正式库天然被覆盖自愈（无需先删除/重试打开坏文件）；
/// - 构建中途崩溃只残留 .tmp（下次构建先清理），正式库不受影响；
/// - meta（last_updated）在 tmp 上写入，随 rename 一起生效；
/// - Windows：正式 db 可能被在途搜索短暂持有的旧 store 连接占用 → rename 短重试。
/// 返回 (新 store, 各表行数)。
async fn build_db_then_swap(
    db_path: &Path,
    subjects_path: &Path,
    relations_path: &Path,
    persons_path: &Path,
    subject_persons_path: &Path,
    meta_updated_at: &str,
) -> Result<(BangumiArchiveStore, (usize, usize, usize, usize)), ProviderError> {
    let tmp_path = db_path.with_extension("db.tmp");
    // 上次崩溃/失败可能残留 .tmp：先清理
    let _ = std::fs::remove_file(&tmp_path);
    let tmp_store = BangumiArchiveStore::open(&tmp_path)
        .map_err(|e| ProviderError::message(format!("archive open: {e}")))?;
    tmp_store
        .init_schema()
        .map_err(|e| ProviderError::message(format!("archive schema: {e}")))?;
    let counts = tmp_store
        .build(
            subjects_path,
            relations_path,
            persons_path,
            subject_persons_path,
        )
        .map_err(|e| ProviderError::message(format!("archive build: {e}")))?;
    tmp_store.set_meta("last_updated", meta_updated_at);
    // Windows：SQLite 打开的 tmp 文件无 FILE_SHARE_DELETE 共享标志，rename 前必须
    // 释放连接（与 ehentai build_fts drop(conn) 同理）；rename 后重新打开正式库返回。
    drop(tmp_store);
    let mut renamed = false;
    for _ in 0..5 {
        if std::fs::rename(&tmp_path, db_path).is_ok() {
            renamed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    if !renamed {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(ProviderError::message(format!(
            "archive db rename failed after 5 attempts: {}",
            db_path.display()
        )));
    }
    let store = BangumiArchiveStore::open(db_path)
        .map_err(|e| ProviderError::message(format!("archive open after swap: {e}")))?;
    Ok((store, counts))
}

/// 下载 zip → 解压 → 全量重建 → 返回新 store。
/// `meta` 为已获取的 latest.json（避免重复请求）；`emit` 输出进度事件（可选）。

async fn download_and_rebuild(
    dir: &Path,
    db_path: &Path,
    subjects_path: &Path,
    relations_path: &Path,
    persons_path: &Path,
    subject_persons_path: &Path,
    meta: &LatestMeta,
    emit: Option<&(dyn Fn(DownloadProgress) + Send + Sync)>,
) -> Result<Arc<BangumiArchiveStore>, ProviderError> {
    let url = meta
        .browser_download_url
        .clone()
        .filter(|u| !u.is_empty())
        .ok_or_else(|| ProviderError::message("archive: no download url"))?;
    let expected_size = meta.size;
    if let Some(emit) = emit {
        emit(DownloadProgress::ProgressEvent {
            total: 0,
            completed: 0,
            info: Some(format!("downloading {url}")),
        });
    }
    // zip 持久缓存（约 418MB）：同 size 时跳过重复下载（构建失败/重启不重下）
    let zip_path = dir.join("archive-latest.zip");
    let cached_ok = std::fs::metadata(&zip_path)
        .map(|m| expected_size.map_or(true, |s| m.len() == s))
        .unwrap_or(false);
    if !cached_ok {
        tracing::info!("downloading bangumi archive ({url})");
        let dl = crate::util::download::long_download_client().map_err(ProviderError::message)?;
        let tmp_zip = zip_path.with_extension("tmp");
        // 字节进度 → ProgressEvent（闭包借用 emit/url，生命周期与本次调用一致）
        let emit_c = emit;
        let url_c = url.clone();
        let progress_emitter: &(dyn Fn(u64, u64) + Send + Sync) = &move |written, total| {
            if let Some(f) = emit_c {
                f(DownloadProgress::ProgressEvent {
                    total: total as i64,
                    completed: written as i64,
                    info: Some(url_c.clone()),
                });
            }
        };
        download_zip_with_retry(&dl, &url, &tmp_zip, expected_size, Some(progress_emitter)).await?;
        std::fs::rename(&tmp_zip, &zip_path)
            .map_err(|e| ProviderError::message(format!("archive move: {e}")))?;
    } else {
        tracing::info!(
            "bangumi archive zip cache hit ({} bytes)",
            expected_size.unwrap_or(0)
        );
    }
    // 解压（zip crate 读取时自动校验 CRC）
    if let Some(emit) = emit {
        emit(DownloadProgress::ProgressEvent {
            total: 0,
            completed: 0,
            info: Some("extracting archive".to_string()),
        });
    }
    let zip_file = std::fs::File::open(&zip_path)
        .map_err(|e| ProviderError::message(format!("archive open: {e}")))?;
    let mut archive = zip::ZipArchive::new(zip_file).map_err(|e| {
        // zip 缓存损坏（size 一致也可能 CRC 失败）：删除缓存让下次重试重下
        let _ = std::fs::remove_file(&zip_path);
        ProviderError::message(format!("archive zip: {e}"))
    })?;
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
            // Windows：旧 store 仍被在途搜索短暂映射时 File::create 报 os error 1224；
            // 短重试（最多 5 次，累计约 2s）等待 Arc 释放。entry 流不消费，重试安全。
            let mut out =
                std::io::BufWriter::new(create_extracted_file(target, name.as_str()).await?);
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
    if let Some(emit) = emit {
        emit(DownloadProgress::ProgressEvent {
            total: 0,
            completed: 0,
            info: Some("building index".to_string()),
        });
    }
    let (new_store, (subj, rel, persons, sp)) = build_db_then_swap(
        db_path,
        subjects_path,
        relations_path,
        persons_path,
        subject_persons_path,
        &meta.updated_at.clone().unwrap_or_default(),
    )
    .await?;
    tracing::info!(
        "bangumi archive rebuilt: {subj} subjects, {rel} relations, {persons} persons, {sp} subject-persons"
    );
    // 主数据已完整入库（tmp 库构建成功并原子替换）——解压出的 jsonlines（~1GB）
    // 是纯冗余：zip 缓存仍在，下次更新/重建可重新解压。
    remove_extracted_jsonlines(dir);
    Ok(Arc::new(new_store))
}

/// 构建成功后删除解压出的 jsonlines（主数据已完整入库，仅省磁盘用）。
/// 删除失败不致命（残留文件不影响功能，下次构建前会覆盖写）。
fn remove_extracted_jsonlines(dir: &Path) {
    for name in [
        "subject.jsonlines",
        "subject-relations.jsonlines",
        "person.jsonlines",
        "subject-persons.jsonlines",
    ] {
        let path = dir.join(name);
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!("bangumi archive: remove {} failed: {e}", path.display());
            } else {
                tracing::debug!("bangumi archive: removed {}", path.display());
            }
        }
    }
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
            "近藤奈々 (こんどう なな)",
            "こんどう なな",
            "みずき なな",
            "Mizuki Nana",
            "奈々ちゃん",
            "奈々さん",
            "奈々様",
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
        let d =
            std::env::temp_dir().join(format!("komf-archive-test-{tag}-{}", std::process::id()));
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
        let alias = items
            .iter()
            .find(|i| i.key.as_deref() == Some("别名"))
            .unwrap();
        let arr = alias.value.as_ref().unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].get("k").and_then(|k| k.as_str()), Some("台版"));
        assert_eq!(
            arr[0].get("v").and_then(|v| v.as_str()),
            Some("無能力者娜娜")
        );
        let pub_ = items
            .iter()
            .find(|i| i.key.as_deref() == Some("出版社"))
            .unwrap();
        assert_eq!(
            pub_.value.as_ref().unwrap().as_str(),
            Some("スクウェア・エニックス")
        );
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
            &[r#"{"subject_id":1,"relation_type":"单行本","related_subject_id":2}"#],
        );
        let persons = dir.join("person.jsonlines");
        let sp = dir.join("subject-persons.jsonlines");
        std::fs::write(&persons, "").unwrap();
        std::fs::write(&sp, "").unwrap();
        let store = BangumiArchiveStore::open(&dir.join("archive_index.db")).unwrap();
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
        let hits = store.search("Test Manga", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["id"], 1);
        // 仅 type=1；type=2 不进结果
        assert!(store.search("Anime", 10).is_empty());

        // get_related
        let rels = store.get_related(1);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].id, 2);
        assert_eq!(rels[0].relation.as_deref(), Some("单行本"));
        assert_eq!(rels[0].name, "Other Book");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 回归：LIKE 回退已删除——FTS 未命中即返回空，不再三列全表扫 + 全量 mmap 读出。
    #[test]
    fn archive_search_miss_returns_empty() {
        let dir = tmp_dir("miss");
        let subjects = dir.join("subject.jsonlines");
        let relations = dir.join("subject-relations.jsonlines");
        write_subjects(
            &subjects,
            &[
                r#"{"id":42,"type":1,"name":"魔法少女まどか","name_cn":"魔法少女小圆","series":true}"#,
            ],
        );
        std::fs::write(&relations, "").unwrap();
        let persons = dir.join("person.jsonlines");
        let sp = dir.join("subject-persons.jsonlines");
        std::fs::write(&persons, "").unwrap();
        std::fs::write(&sp, "").unwrap();
        let store = BangumiArchiveStore::open(&dir.join("archive_index.db")).unwrap();
        store.init_schema().unwrap();
        store.build(&subjects, &relations, &persons, &sp).unwrap();
        // FTS 未命中 → 空（旧行为会 LIKE 全表扫回退）
        assert!(store.search("完全不存在的查询xyz", 10).is_empty());
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
        let store = BangumiArchiveStore::open(&dir.join("archive_index.db")).unwrap();
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
        let alias_v = alias
            .value
            .as_ref()
            .and_then(|v| v.as_array())
            .expect("alias array");
        assert_eq!(alias_v.len(), 2);
        // 纯值列表项 → {"v": ..}（无 k，对齐在线 API → Localized 语言 None）
        assert_eq!(alias_v[0]["v"], "次元魔女");
        assert!(alias_v[0].get("k").is_none());
        let author = items
            .iter()
            .find(|i| i.key.as_deref() == Some("作者"))
            .expect("作者 key");
        assert_eq!(
            author.value.as_ref().and_then(|v| v.as_str()),
            Some("CLAMP")
        );
        // kv 列表项（版本:* 形式）→ {"k": .., "v": ..}
        let version = items
            .iter()
            .find(|i| i.key.as_deref() == Some("版本:东立版"))
            .expect("版本:东立版 key");
        let version_v = version
            .value
            .as_ref()
            .and_then(|v| v.as_array())
            .expect("version array");
        assert_eq!(version_v[0]["k"], "版本名");
        assert_eq!(version_v[0]["v"], "xxxHOLiC 東立");
        // 数字 relation_type → 单行本
        let rels = store.get_related(495);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].relation.as_deref(), Some("单行本"));
        // 搜索（name_cn 命中）
        let hits = store.search("×××HOLiC", 10);
        assert!(!hits.is_empty());
        // 别名作为搜索词可命中（FTS aliases 列）
        let alias_hits = store.search("次元魔女", 10);
        assert!(alias_hits.iter().any(|v| v["id"] == 495));
        // 纯拉丁别名命中（aliases 列经分析链小写入库；unicode61 默认大小写不敏感）
        let alias_like = store.search("XXXHOLIC", 10);
        assert!(alias_like.iter().any(|v| v["id"] == 495));
        // 嵌套别名（版本:* 条目内 k==别名）也进入 aliases 列并可检索
        let nested_alias = store.search("xxxHOLiC 笼", 10);
        assert!(nested_alias.iter().any(|v| v["id"] == 495));
        // 版本名（版本:* 条目内 k==版本名）也进入 aliases 列并可检索
        let version_name = store.search("xxxHOLiC 東立", 10);
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
        write_subjects(
            &subjects,
            &[r#"{"id":268279,"type":1,"name":"チェンソーマン","name_cn":"链锯人","series":true}"#],
        );
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
        let store = BangumiArchiveStore::open(&dir.join("archive_index.db")).unwrap();
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

    /// 回归：外部内容 FTS（content='subjects'）的无 MATCH 查询直通 content 表，
    /// rebuild_fts 旧游标用 MAX(rowid) FROM subjects_fts 会一批后跳到全表末尾，
    /// 仅索引首批 50_000 行。此处造 50_001+ 行验证全量索引（id 大者必须可搜）。
    #[test]
    fn archive_fts_rebuild_covers_all_batches() {
        let dir = tmp_dir("ftsbatches");
        let subjects = dir.join("subject.jsonlines");
        let relations = dir.join("subject-relations.jsonlines");
        let persons = dir.join("person.jsonlines");
        let sp = dir.join("subject-persons.jsonlines");
        // 50_001 行：id 1..=50_001，仅最大 id 的行带唯一可检索中文名
        let mut content = String::new();
        for id in 1..=50_001i64 {
            if id == 50_001 {
                content.push_str(
                    r#"{"id":50001,"type":1,"name":"BatchMarker","name_cn":"批次尾行标记","series":true}"#,
                );
            } else {
                content.push_str(&format!(
                    r#"{{"id":{id},"type":1,"name":"Row{id}","series":true}}"#
                ));
            }
            content.push('\n');
        }
        std::fs::write(&subjects, content).unwrap();
        std::fs::write(&relations, "").unwrap();
        std::fs::write(&persons, "").unwrap();
        std::fs::write(&sp, "").unwrap();
        let store = BangumiArchiveStore::open(&dir.join("archive_index.db")).unwrap();
        store.init_schema().unwrap();
        store.build(&subjects, &relations, &persons, &sp).unwrap();
        // 最大 id（第二批）必须命中——旧游标 bug 下 FTS 只有首批 50_000 行
        let hits = store.search("批次尾行标记", 10);
        assert!(
            hits.iter().any(|v| v["id"] == 50001),
            "FTS must index rows beyond the first 50k batch"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// remove_extracted_jsonlines：仅删除 4 个归档 jsonlines，其余文件不动；
    /// 缺失的文件不报错。
    #[test]
    fn remove_extracted_jsonlines_only_targets_archive_files() {
        let dir = tmp_dir("rmjson");
        for name in [
            "subject.jsonlines",
            "subject-relations.jsonlines",
            "person.jsonlines",
            "subject-persons.jsonlines",
        ] {
            std::fs::write(dir.join(name), "{}").unwrap();
        }
        std::fs::write(dir.join("archive-latest.zip"), "zip").unwrap();
        std::fs::write(dir.join("archive_index.db"), "db").unwrap();
        // person.jsonlines 缺失场景不报错（先删掉再调用）
        std::fs::remove_file(dir.join("person.jsonlines")).unwrap();
        remove_extracted_jsonlines(&dir);
        assert!(!dir.join("subject.jsonlines").exists());
        assert!(!dir.join("subject-relations.jsonlines").exists());
        assert!(!dir.join("subject-persons.jsonlines").exists());
        assert!(dir.join("archive-latest.zip").exists(), "zip 缓存须保留");
        assert!(dir.join("archive_index.db").exists(), "db 须保留");
        std::fs::remove_dir_all(&dir).ok();
    }
    /// 回归：v7 及更早的 legacy 库（subjects_idx + row_offset）→ init_schema
    /// drop 旧表，build 从本地 jsonlines 重建（v8 主数据入库）后搜索可用。
    #[test]
    fn archive_v8_migration_from_legacy_subjects_idx() {
        let dir = tmp_dir("v8migrate");
        let subjects = dir.join("subject.jsonlines");
        let relations = dir.join("subject-relations.jsonlines");
        write_subjects(
            &subjects,
            &[
                r#"{"id":305429,"type":1,"name":"葬送のフリーレン","name_cn":"葬送的芙莉莲","series":true}"#,
            ],
        );
        std::fs::write(&relations, "").unwrap();
        let persons = dir.join("person.jsonlines");
        let sp = dir.join("subject-persons.jsonlines");
        std::fs::write(&persons, "").unwrap();
        std::fs::write(&sp, "").unwrap();
        let db = dir.join("archive_index.db");
        // 第一步：按 legacy 旧 schema 建库（subjects_idx + row_offset，无 json 列）
        {
            let store = BangumiArchiveStore::open(&db).unwrap();
            let c = store.conn.lock().unwrap();
            c.execute_batch(
                "CREATE TABLE subjects_idx (
                     id INTEGER PRIMARY KEY, type INTEGER, name TEXT, name_cn TEXT,
                     aliases TEXT, row_offset INTEGER NOT NULL);
                 INSERT INTO subjects_idx VALUES (305429, 1, '葬送のフリーレン', '葬送的芙莉莲', '', 0);
                 CREATE TABLE relations_idx (subject_id INTEGER, relation_type TEXT, related_subject_id INTEGER);
                 CREATE TABLE archive_update (key TEXT PRIMARY KEY, value TEXT);",
            )
            .unwrap();
        }
        // 第二步：重开（等价升级后的启动），init_schema 应 drop 旧表 → 库未就绪，
        // build 从 jsonlines 重建（对齐服务启动的本地自愈路径）。
        // validate 要求 relations 非空 → 补一条关联数据。
        write_subjects(
            &relations,
            &[r#"{"subject_id":305429,"relation_type":"单行本","related_subject_id":305429}"#],
        );
        {
            let store = BangumiArchiveStore::open(&db).unwrap();
            store.init_schema().unwrap();
            assert!(!store.validate(), "legacy 主数据被 drop 后未重建前应未就绪");
            store.build(&subjects, &relations, &persons, &sp).unwrap();
            assert!(store.validate(), "build 后应就绪");
            let hits = store.search("葬送的芙莉莲系列", 10);
            assert!(
                hits.iter().any(|v| v["id"] == 305429),
                "decorated query must hit after migration"
            );
            // 主数据确实在库内（v8：json 列），不再依赖外部 jsonlines 偏移
            let v = store.get_by_id(305429).expect("row");
            assert_eq!(v["name_cn"], "葬送的芙莉莲");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 门控：拉丁整词子句永不放宽（对齐 kmrs cjk_droppable_floor 语义）——
    /// "Berserk 系列" 不得退化为裸 "berserk" 查询；单 token 拉丁查询仍命中。
    #[test]
    fn archive_search_latin_clause_not_relaxed() {
        let dir = tmp_dir("latinfloor");
        let subjects = dir.join("subject.jsonlines");
        let relations = dir.join("subject-relations.jsonlines");
        write_subjects(
            &subjects,
            &[r#"{"id":11,"type":1,"name":"Berserk","name_cn":"剑风传奇","series":true}"#],
        );
        std::fs::write(&relations, "").unwrap();
        let persons = dir.join("person.jsonlines");
        let sp = dir.join("subject-persons.jsonlines");
        std::fs::write(&persons, "").unwrap();
        std::fs::write(&sp, "").unwrap();
        let store = BangumiArchiveStore::open(&dir.join("archive_index.db")).unwrap();
        store.init_schema().unwrap();
        store.build(&subjects, &relations, &persons, &sp).unwrap();

        // 单 token 拉丁查询：正常命中
        let hits = store.search("Berserk", 10);
        assert!(hits.iter().any(|v| v["id"] == 11), "bare latin term hits");
        // 拉丁 + 无命中 CJK 后缀：保持 Lucene parity，不回退成裸拉丁词
        assert!(
            store.search("Berserk 系列", 10).is_empty(),
            "latin clause must not be relaxed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn archive_fts_and_expr_builds() {
        assert_eq!(fts_and_expr(&["魔法少女".to_string()]), "\"魔法少女\"");
        assert_eq!(
            fts_and_expr(&["attack".to_string(), "on".to_string(), "titan".to_string()]),
            "\"attack\" AND \"on\" AND \"titan\""
        );
        assert_eq!(fts_and_expr(&[]), "");
        // 引号转义
        assert_eq!(fts_and_expr(&["a\"b".to_string()]), "\"a\"\"b\"");
    }

    /// 回归：komga 系列名带装饰后缀/卷号/繁体变体时，离线查询仍可命中归档标题。
    #[test]
    fn archive_search_decorated_and_traditional_queries() {
        let dir = tmp_dir("decorated");
        let subjects = dir.join("subject.jsonlines");
        let relations = dir.join("subject-relations.jsonlines");
        write_subjects(
            &subjects,
            &[
                r#"{"id":305429,"type":1,"name":"葬送のフリーレン","name_cn":"葬送的芙莉莲","series":true}"#,
            ],
        );
        std::fs::write(&relations, "").unwrap();
        let persons = dir.join("person.jsonlines");
        let sp = dir.join("subject-persons.jsonlines");
        std::fs::write(&persons, "").unwrap();
        std::fs::write(&sp, "").unwrap();
        let store = BangumiArchiveStore::open(&dir.join("archive_index.db")).unwrap();
        store.init_schema().unwrap();
        store.build(&subjects, &relations, &persons, &sp).unwrap();

        // 直接验证 FTS 索引内容（LIKE 无法伪造）：bigram 词项必须可 MATCH
        {
            let c = store.conn.lock().unwrap();
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM subjects_fts WHERE subjects_fts MATCH '\"葬送\"'",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(-1);
            assert_eq!(n, 1, "FTS index must contain bigram term 葬送");
        }

        // 精确命中
        let hits = store.search("葬送的芙莉莲", 10);
        assert!(hits.iter().any(|v| v["id"] == 305429), "exact");
        // 系列名带 "系列" 后缀（归档标题没有）→ 渐进前缀 AND 兜底
        let hits = store.search("葬送的芙莉莲系列", 10);
        assert!(
            hits.iter().any(|v| v["id"] == 305429),
            "系列-suffixed query must hit"
        );
        // 卷/话装饰（空格分隔 + 数字）
        let hits = store.search("葬送的芙莉莲 第01话", 10);
        assert!(
            hits.iter().any(|v| v["id"] == 305429),
            "chapter-decorated query must hit"
        );
        // 繁体查询命中简体标题（t2s 归一）
        let hits = store.search("葬送的芙莉蓮", 10);
        assert!(
            hits.iter().any(|v| v["id"] == 305429),
            "traditional query must hit"
        );
        // 日文原名亦可命中
        let hits = store.search("葬送のフリーレン", 10);
        assert!(
            hits.iter().any(|v| v["id"] == 305429),
            "japanese name must hit"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_db_then_swap_replaces_corrupt_db() {
        let dir = tmp_dir("swap_corrupt");
        let db = dir.join("archive_index.db");
        let subjects = dir.join("subject.jsonlines");
        let relations = dir.join("subject-relations.jsonlines");
        let persons = dir.join("person.jsonlines");
        let sp = dir.join("subject-persons.jsonlines");
        // 正式库：坏文件（随机字节，open 阶段即失败）；数据文件正常
        std::fs::write(&db, vec![0xabu8; 4096]).unwrap();
        write_subjects(
            &subjects,
            &[r#"{"id":42,"type":1,"name":"Test","series":true}"#],
        );
        write_subjects(
            &relations,
            &[r#"{"subject_id":42,"relation_type":1003,"related_subject_id":9}"#],
        );
        std::fs::write(&persons, "").unwrap();
        std::fs::write(&sp, "").unwrap();
        // build_db_then_swap 为 async（rename 重试走 tokio sleep）：测试内建
        // current_thread runtime 驱动
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (store, (subj, rel, _, _)) = rt
            .block_on(build_db_then_swap(
                &db,
                &subjects,
                &relations,
                &persons,
                &sp,
                "2026-10-01",
            ))
            .expect("build+swap should replace corrupt db");
        assert_eq!(subj, 1);
        assert_eq!(rel, 1);
        // .tmp 不残留；正式库已被替换为可用新库（坏文件天然被覆盖）
        assert!(!db.with_extension("db.tmp").exists(), "tmp must not remain");
        assert!(store.validate());
        assert_eq!(
            store.get_meta("last_updated").as_deref(),
            Some("2026-10-01")
        );
        drop(store);
        let reopened = BangumiArchiveStore::open(&db).expect("reopen ok");
        assert!(reopened.validate());
        drop(reopened);
        std::fs::remove_dir_all(&dir).ok();
    }
}
