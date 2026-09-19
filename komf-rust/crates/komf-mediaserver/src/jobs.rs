//! 任务追踪 —— 对应 `snd.komf.mediaserver.jobs` 包。
//!
//! 包含 `KomfJobsRepository`（SQLite 持久化）与 `KomfJobTracker`（内存事件流）。
use crate::model::{MediaServerBookId, MediaServerSeriesId};
use chrono::{DateTime, Utc};
use komf_core::providers::CoreProviders;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataJobStatus {
    Running,
    Failed,
    Completed,
}

impl MetadataJobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MetadataJobStatus::Running => "RUNNING",
            MetadataJobStatus::Failed => "FAILED",
            MetadataJobStatus::Completed => "COMPLETED",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetadataJob {
    pub series_id: MediaServerSeriesId,
    pub id: MetadataJobId,
    pub status: MetadataJobStatus,
    pub message: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl MetadataJob {
    pub fn new(series_id: MediaServerSeriesId) -> Self {
        Self {
            series_id,
            id: MetadataJobId(Uuid::new_v4()),
            status: MetadataJobStatus::Running,
            message: None,
            started_at: Utc::now(),
            finished_at: None,
        }
    }

    pub fn complete(mut self) -> Self {
        self.status = MetadataJobStatus::Completed;
        self.finished_at = Some(Utc::now());
        self
    }

    pub fn fail(mut self, message: String) -> Self {
        self.status = MetadataJobStatus::Failed;
        self.message = Some(message);
        self.finished_at = Some(Utc::now());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MetadataJobId(pub Uuid);

#[derive(Debug, Clone)]
pub enum MetadataJobEvent {
    ProviderSeries { provider: CoreProviders },
    ProviderBook { provider: CoreProviders, total_books: i32, book_progress: i32 },
    ProviderError { provider: CoreProviders, message: String },
    ProviderCompleted { provider: CoreProviders },
    PostProcessingStart,
    ProcessingError { message: String },
    Completed,
}

/// 任务记录表 —— 对应 `KomfJobRecord`。
#[derive(Debug, Clone)]
pub struct KomfJobRecord {
    pub id: MetadataJobId,
    pub series_id: MediaServerSeriesId,
    pub status: MetadataJobStatus,
    pub message: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// 系列匹配记录 —— 对应 `SeriesMatch`。
#[derive(Debug, Clone)]
pub struct SeriesMatch {
    pub series_id: MediaServerSeriesId,
    pub r#type: String,
    pub media_server: String,
    pub provider: CoreProviders,
    pub provider_series_id: String,
}

/// 系列缩略图记录 —— 对应 `SeriesThumbnail`。
#[derive(Debug, Clone)]
pub struct SeriesThumbnail {
    pub series_id: MediaServerSeriesId,
    pub thumbnail_id: String,
    pub media_server: String,
}

/// 书籍缩略图记录 —— 对应 `BookThumbnail`。
#[derive(Debug, Clone)]
pub struct BookThumbnail {
    pub series_id: MediaServerSeriesId,
    pub book_id: MediaServerBookId,
    pub thumbnail_id: String,
    pub media_server: String,
}

/// SQLite 任务/匹配仓库 —— 对应 `KomfJobsRepository` + 各 repository。
pub struct KomfJobsRepository {
    conn: StdMutex<Connection>,
}

impl KomfJobsRepository {
    pub fn open(file: &Path) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = file.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let conn = Connection::open(file)?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: StdMutex::new(conn),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS komf_job_record (
                id TEXT PRIMARY KEY,
                series_id TEXT NOT NULL,
                status TEXT NOT NULL,
                message TEXT,
                started_at INTEGER NOT NULL,
                finished_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS komf_job_series_id_idx ON komf_job_record (series_id);
            CREATE INDEX IF NOT EXISTS komf_job_status_idx ON komf_job_record (status);
            CREATE INDEX IF NOT EXISTS komf_job_started_at_idx ON komf_job_record (started_at);
            CREATE INDEX IF NOT EXISTS komf_job_finished_at_idx ON komf_job_record (finished_at);
            CREATE TABLE IF NOT EXISTS series_match (
                series_id TEXT NOT NULL,
                type TEXT NOT NULL,
                media_server TEXT NOT NULL,
                provider TEXT NOT NULL,
                provider_series_id TEXT NOT NULL,
                PRIMARY KEY (series_id, media_server)
            );
            CREATE INDEX IF NOT EXISTS series_match_type_idx ON series_match (type);
            CREATE TABLE IF NOT EXISTS series_thumbnail (
                series_id TEXT NOT NULL,
                thumbnail_id TEXT NOT NULL,
                media_server TEXT NOT NULL,
                PRIMARY KEY (series_id, media_server)
            );
            CREATE INDEX IF NOT EXISTS series_thumbnail_server_type_idx ON series_thumbnail (media_server);
            CREATE TABLE IF NOT EXISTS book_thumbnail (
                series_id TEXT NOT NULL,
                book_id TEXT NOT NULL,
                thumbnail_id TEXT NOT NULL,
                media_server TEXT NOT NULL,
                PRIMARY KEY (book_id, media_server)
            );
            CREATE INDEX IF NOT EXISTS book_thumbnails_series_idx ON book_thumbnail (series_id);
            CREATE INDEX IF NOT EXISTS book_thumbnails_server_type_idx ON book_thumbnail (media_server);
            "#,
        )?;
        Ok(())
    }

    // ---- job records ----
    pub fn insert_job(&self, record: &KomfJobRecord) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO komf_job_record (id, series_id, status, message, started_at, finished_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                record.id.0.to_string(),
                record.series_id.0,
                record.status.as_str(),
                record.message,
                record.started_at.timestamp_millis(),
                record.finished_at.map(|t| t.timestamp_millis()),
            ],
        )?;
        Ok(())
    }

    pub fn update_job_status(
        &self,
        id: &MetadataJobId,
        status: MetadataJobStatus,
        message: Option<String>,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE komf_job_record SET status = ?1, message = ?2, finished_at = ?3 WHERE id = ?4",
            rusqlite::params![
                status.as_str(),
                message,
                Some(Utc::now().timestamp_millis()),
                id.0.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn get_job(&self, id: &MetadataJobId) -> Result<Option<KomfJobRecord>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, series_id, status, message, started_at, finished_at FROM komf_job_record WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![id.0.to_string()], |row| {
            Ok(KomfJobRecord {
                id: MetadataJobId(Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_default()),
                series_id: MediaServerSeriesId(row.get(1)?),
                status: parse_status(&row.get::<_, String>(2)?),
                message: row.get(3)?,
                started_at: DateTime::from_timestamp_millis(row.get::<_, i64>(4)?).unwrap_or_default(),
                finished_at: row.get::<_, Option<i64>>(5)?.and_then(|ms| DateTime::from_timestamp_millis(ms)),
            })
        })?;
        rows.next().transpose()
    }

    pub fn list_jobs(&self) -> Result<Vec<KomfJobRecord>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, series_id, status, message, started_at, finished_at FROM komf_job_record ORDER BY started_at DESC LIMIT 200",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(KomfJobRecord {
                id: MetadataJobId(Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_default()),
                series_id: MediaServerSeriesId(row.get(1)?),
                status: parse_status(&row.get::<_, String>(2)?),
                message: row.get(3)?,
                started_at: DateTime::from_timestamp_millis(row.get::<_, i64>(4)?).unwrap_or_default(),
                finished_at: row.get::<_, Option<i64>>(5)?.and_then(|ms| DateTime::from_timestamp_millis(ms)),
            })
        })?;
        rows.collect()
    }

    pub fn find_all(
        &self,
        status: Option<MetadataJobStatus>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<KomfJobRecord>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut sql = String::from(
            "SELECT id, series_id, status, message, started_at, finished_at FROM komf_job_record",
        );
        // 对齐 Kotlin SQLDelight：匿名 `?` 占位符按顺序绑定。
        // no-status → [limit, offset]；with-status → [status, limit, offset]。
        // （原先硬编码 ?2/?3，no-status 时缺少 ?1 导致 OFFSET 错位 / 空结果。）
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(status) = status {
            sql.push_str(" WHERE status = ?");
            params.push(Box::new(status.as_str()));
        }
        sql.push_str(" ORDER BY started_at DESC LIMIT ? OFFSET ?");
        params.push(Box::new(limit));
        params.push(Box::new(offset));
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |row| {
            Ok(KomfJobRecord {
                id: MetadataJobId(Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_default()),
                series_id: MediaServerSeriesId(row.get(1)?),
                status: parse_status(&row.get::<_, String>(2)?),
                message: row.get(3)?,
                started_at: DateTime::from_timestamp_millis(row.get::<_, i64>(4)?).unwrap_or_default(),
                finished_at: row.get::<_, Option<i64>>(5)?.and_then(|ms| DateTime::from_timestamp_millis(ms)),
            })
        })?;
        rows.collect()
    }

    pub fn count_all(&self, status: Option<MetadataJobStatus>) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        match status {
            Some(status) => conn.query_row(
                "SELECT COUNT(*) FROM komf_job_record WHERE status = ?1",
                rusqlite::params![status.as_str()],
                |row| row.get(0),
            ),
            None => conn.query_row("SELECT COUNT(*) FROM komf_job_record", [], |row| row.get(0)),
        }
    }

    pub fn delete_all(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM komf_job_record", [])?;
        Ok(())
    }

    /// 对齐 Kotlin `cancellAllRunning`：所有 RUNNING 置 FAILED + message 'Cancelled'。
    pub fn cancel_all_running(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE komf_job_record SET status = 'FAILED', message = 'Cancelled' WHERE status = 'RUNNING'",
            [],
        )?;
        Ok(())
    }

    /// 对齐 Kotlin `deleteAllBeforeDate`：删除 startedAt <= cutoff 的记录。
    pub fn delete_all_before(&self, cutoff: DateTime<Utc>) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM komf_job_record WHERE started_at <= ?1",
            rusqlite::params![cutoff.timestamp_millis()],
        )?;
        Ok(())
    }

    // ---- series match ----
    pub fn save_series_match(&self, record: &SeriesMatch) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO series_match (series_id, type, media_server, provider, provider_series_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                record.series_id.0,
                record.r#type,
                record.media_server,
                record.provider.as_str(),
                record.provider_series_id,
            ],
        )?;
        Ok(())
    }

    pub fn find_manual_for(&self, series_id: &MediaServerSeriesId, media_server: &str) -> Result<Option<SeriesMatch>, rusqlite::Error> {        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT series_id, type, media_server, provider, provider_series_id FROM series_match WHERE series_id = ?1 AND media_server = ?2 AND type = 'MANUAL'",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![series_id.0, media_server], |row| {
            Ok(SeriesMatch {
                series_id: MediaServerSeriesId(row.get(0)?),
                r#type: row.get(1)?,
                media_server: row.get(2)?,
                provider: CoreProviders::from_str(&row.get::<_, String>(3)?).unwrap_or(CoreProviders::MangaUpdates),
                provider_series_id: row.get(4)?,
            })
        })?;
        rows.next().transpose()
    }

    pub fn delete_series_match(&self, series_id: &MediaServerSeriesId, media_server: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM series_match WHERE series_id = ?1 AND media_server = ?2",
            rusqlite::params![series_id.0, media_server],
        )?;
        Ok(())
    }

    // ---- thumbnails ----
    pub fn save_series_thumbnail(&self, record: &SeriesThumbnail) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO series_thumbnail (series_id, thumbnail_id, media_server) VALUES (?1, ?2, ?3)",
            rusqlite::params![record.series_id.0, record.thumbnail_id, record.media_server],
        )?;
        Ok(())
    }

    pub fn delete_series_thumbnail(&self, series_id: &MediaServerSeriesId, media_server: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM series_thumbnail WHERE series_id = ?1 AND media_server = ?2",
            rusqlite::params![series_id.0, media_server],
        )?;
        Ok(())
    }

    pub fn find_series_thumbnail(&self, series_id: &MediaServerSeriesId, media_server: &str) -> Result<Option<SeriesThumbnail>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT series_id, thumbnail_id, media_server FROM series_thumbnail WHERE series_id = ?1 AND media_server = ?2",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![series_id.0, media_server], |row| {
            Ok(SeriesThumbnail {
                series_id: MediaServerSeriesId(row.get(0)?),
                thumbnail_id: row.get(1)?,
                media_server: row.get(2)?,
            })
        })?;
        rows.next().transpose()
    }

    pub fn save_book_thumbnail(&self, record: &BookThumbnail) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO book_thumbnail (series_id, book_id, thumbnail_id, media_server) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![record.series_id.0, record.book_id.0, record.thumbnail_id, record.media_server],
        )?;
        Ok(())
    }

    pub fn delete_book_thumbnail(&self, book_id: &MediaServerBookId, media_server: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM book_thumbnail WHERE book_id = ?1 AND media_server = ?2",
            rusqlite::params![book_id.0, media_server],
        )?;
        Ok(())
    }

    pub fn find_book_thumbnail(&self, book_id: &MediaServerBookId, media_server: &str) -> Result<Option<BookThumbnail>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT series_id, book_id, thumbnail_id, media_server FROM book_thumbnail WHERE book_id = ?1 AND media_server = ?2",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![book_id.0, media_server], |row| {
            Ok(BookThumbnail {
                series_id: MediaServerSeriesId(row.get(0)?),
                book_id: MediaServerBookId(row.get(1)?),
                thumbnail_id: row.get(2)?,
                media_server: row.get(3)?,
            })
        })?;
        rows.next().transpose()
    }
}

fn parse_status(status: &str) -> MetadataJobStatus {
    match status {
        "COMPLETED" => MetadataJobStatus::Completed,
        "FAILED" => MetadataJobStatus::Failed,
        _ => MetadataJobStatus::Running,
    }
}

/// 内存任务追踪器 —— 对应 `KomfJobTracker.kt`。
///
/// 维护 job 状态与每个 job 的事件广播通道（供 SSE 消费）。
pub struct KomfJobTracker {
    repository: Arc<KomfJobsRepository>,
    jobs: RwLock<HashMap<MetadataJobId, JobState>>,
}

struct JobState {
    record: KomfJobRecord,
    broadcast: tokio::sync::broadcast::Sender<MetadataJobEvent>,
}

impl KomfJobTracker {
    pub fn new(repository: Arc<KomfJobsRepository>, _media_server: &str) -> Self {
        // 对齐 Kotlin `KomfJobTracker.init`：
        // 1. cancelAllRunning() —— 重启后残留 RUNNING 全部置 FAILED 'Cancelled'；
        // 2. 总任务数 > 10_000 时删除 30 天前的记录。
        let _ = repository.cancel_all_running();
        if let Ok(count) = repository.count_all(None) {
            if count > 10_000 {
                let cutoff = Utc::now() - chrono::Duration::days(30);
                let _ = repository.delete_all_before(cutoff);
            }
        }
        Self {
            repository,
            jobs: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register_job(&self, series_id: MediaServerSeriesId) -> (MetadataJobId, tokio::sync::broadcast::Sender<MetadataJobEvent>) {
        let job = MetadataJob::new(series_id);
        let (tx, _rx) = tokio::sync::broadcast::channel(256);
        let record = KomfJobRecord {
            id: job.id.clone(),
            series_id: job.series_id.clone(),
            status: job.status,
            message: None,
            started_at: job.started_at,
            finished_at: None,
        };
        let _ = self.repository.insert_job(&record);
        self.jobs.write().await.insert(
            job.id.clone(),
            JobState {
                record,
                broadcast: tx.clone(),
            },
        );
        (job.id, tx)
    }

    pub async fn get_job(&self, id: &MetadataJobId) -> Option<KomfJobRecord> {
        self.jobs.read().await.get(id).map(|s| s.record.clone())
    }

    pub async fn list_jobs(&self) -> Vec<KomfJobRecord> {
        let jobs = self.jobs.read().await;
        let mut records: Vec<KomfJobRecord> = jobs.values().map(|s| s.record.clone()).collect();
        records.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        if records.is_empty() {
            self.repository.list_jobs().unwrap_or_default()
        } else {
            records
        }
    }

    pub async fn complete_job(&self, id: &MetadataJobId) {
        // 对齐 Kotlin listener：CompletionEvent -> activeJobs.remove + complete。
        // 终态后移出内存 map，后续 subscribe 返回 None（SSE 发 EventStreamNotFoundEvent）。
        if let Some(mut state) = self.jobs.write().await.remove(id) {
            state.record.status = MetadataJobStatus::Completed;
            state.record.finished_at = Some(Utc::now());
            let _ = self
                .repository
                .update_job_status(id, MetadataJobStatus::Completed, None);
            let _ = state.broadcast.send(MetadataJobEvent::Completed);
        }
    }

    pub async fn fail_job(&self, id: &MetadataJobId, message: String, emit_error: bool) {
        // 对齐 Kotlin listener：ProviderError/ProcessingError -> activeJobs.remove + fail。
        // ProviderError 路径的错误事件已由调用方（finish_job）单独发出，这里不再补 ProcessingError。
        if let Some(mut state) = self.jobs.write().await.remove(id) {
            state.record.status = MetadataJobStatus::Failed;
            state.record.message = Some(message.clone());
            state.record.finished_at = Some(Utc::now());
            let _ = self
                .repository
                .update_job_status(id, MetadataJobStatus::Failed, Some(message.clone()));
            if emit_error {
                let _ = state.broadcast.send(MetadataJobEvent::ProcessingError { message });
            }
            let _ = state.broadcast.send(MetadataJobEvent::Completed);
        }
    }

    pub async fn emit(&self, id: &MetadataJobId, event: MetadataJobEvent) {
        if let Some(state) = self.jobs.read().await.get(id) {
            let _ = state.broadcast.send(event);
        }
    }

    /// 订阅某个 job 的事件流（返回 None 表示 job 不存在）。
    pub async fn subscribe(&self, id: &MetadataJobId) -> Option<tokio::sync::broadcast::Receiver<MetadataJobEvent>> {
        self.jobs.read().await.get(id).map(|s| s.broadcast.subscribe())
    }
}
