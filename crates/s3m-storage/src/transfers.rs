//! Durable transfer records in `transfers.db` (SQLite).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use s3m_core::{ObjectRef, TransferDirection, TransferJob, TransferState, TransferStore};
use uuid::Uuid;

/// SQLite-backed [`TransferStore`]. Storage errors are logged, never fatal: transfers keep
/// working in memory.
pub struct SqliteTransferStore {
    connection: Mutex<Connection>,
}

impl SqliteTransferStore {
    /// Opens (or creates) the database at `path`.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?)
    }

    /// In-memory database (demo runs, tests).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(connection: Connection) -> rusqlite::Result<Self> {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS jobs (
               id TEXT PRIMARY KEY,
               account_id TEXT NOT NULL,
               direction INTEGER NOT NULL,
               bucket TEXT NOT NULL,
               object_key TEXT NOT NULL,
               local_path TEXT NOT NULL,
               created_at TEXT NOT NULL,
               state INTEGER NOT NULL,
               bytes_done INTEGER NOT NULL,
               bytes_total INTEGER NOT NULL,
               attempts INTEGER NOT NULL,
               error TEXT NULL,
               finished_at TEXT NULL
             );",
        )?;
        // v2: source object of copy/move jobs; v3: object version and resume checkpoint.
        for (column, decl) in [
            ("src_bucket", "TEXT NULL"),
            ("src_key", "TEXT NULL"),
            ("version_id", "TEXT NULL"),
            ("resume", "TEXT NULL"),
            ("src_account", "TEXT NULL"),
        ] {
            let exists: bool = connection
                .prepare("SELECT 1 FROM pragma_table_info('jobs') WHERE name = ?1")?
                .exists([column])?;
            if !exists {
                connection
                    .execute_batch(&format!("ALTER TABLE jobs ADD COLUMN {column} {decl};"))?;
            }
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn try_load(&self) -> rusqlite::Result<Vec<TransferJob>> {
        let conn = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut stmt = conn.prepare(
            "SELECT id, account_id, direction, bucket, object_key, local_path, created_at, state,
                    bytes_done, bytes_total, attempts, error, finished_at, src_bucket, src_key, version_id, resume, src_account
             FROM jobs ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            let text = |i: usize| r.get::<_, String>(i);
            let uuid = |s: String| Uuid::parse_str(&s).unwrap_or_default();
            let time = |s: String| {
                DateTime::parse_from_rfc3339(&s)
                    .map_or_else(|_| Utc::now(), |t| t.with_timezone(&Utc))
            };
            Ok(TransferJob {
                id: uuid(text(0)?),
                account_id: uuid(text(1)?),
                direction: TransferDirection::from_i64(r.get(2)?),
                bucket: text(3)?,
                key: text(4)?,
                local_path: PathBuf::from(text(5)?),
                created_at: time(text(6)?),
                state: TransferState::from_i64(r.get(7)?),
                bytes_done: u64::try_from(r.get::<_, i64>(8)?).unwrap_or(0),
                bytes_total: u64::try_from(r.get::<_, i64>(9)?).unwrap_or(0),
                attempts: u32::try_from(r.get::<_, i64>(10)?).unwrap_or(0),
                error: r.get(11)?,
                finished_at: r.get::<_, Option<String>>(12)?.map(time),
                source: match (
                    r.get::<_, Option<String>>(13)?,
                    r.get::<_, Option<String>>(14)?,
                ) {
                    (Some(bucket), Some(key)) => Some(ObjectRef {
                        bucket,
                        key,
                        account_id: r
                            .get::<_, Option<String>>(17)?
                            .and_then(|a| Uuid::parse_str(&a).ok()),
                    }),
                    _ => None,
                },
                version_id: r.get(15)?,
                resume: r.get(16)?,
                rate: 0,
            })
        })?;
        rows.collect()
    }

    fn try_upsert(&self, job: &TransferJob) -> rusqlite::Result<()> {
        let conn = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        conn.execute(
            "INSERT INTO jobs (id, account_id, direction, bucket, object_key, local_path, created_at,
                               state, bytes_done, bytes_total, attempts, error, finished_at,
                               src_bucket, src_key, version_id, resume, src_account)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(id) DO UPDATE SET state = ?8, bytes_done = ?9, bytes_total = ?10,
                                           attempts = ?11, error = ?12, finished_at = ?13,
                                           resume = ?17",
            params![
                job.id.to_string(),
                job.account_id.to_string(),
                job.direction.as_i64(),
                job.bucket,
                job.key,
                job.local_path.to_string_lossy(),
                job.created_at.to_rfc3339(),
                job.state.as_i64(),
                i64::try_from(job.bytes_done).unwrap_or(i64::MAX),
                i64::try_from(job.bytes_total).unwrap_or(i64::MAX),
                i64::from(job.attempts),
                job.error,
                job.finished_at.map(|t| t.to_rfc3339()),
                job.source.as_ref().map(|s| s.bucket.as_str()),
                job.source.as_ref().map(|s| s.key.as_str()),
                job.version_id,
                job.resume,
                job.source
                    .as_ref()
                    .and_then(|s| s.account_id)
                    .map(|a| a.to_string()),
            ],
        )?;
        Ok(())
    }

    fn try_delete(&self, ids: &[Uuid]) -> rusqlite::Result<()> {
        let mut conn = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let tx = conn.transaction()?;
        for id in ids {
            tx.execute("DELETE FROM jobs WHERE id = ?1", [id.to_string()])?;
        }
        tx.commit()
    }
}

impl TransferStore for SqliteTransferStore {
    fn load_all(&self) -> Vec<TransferJob> {
        self.try_load().unwrap_or_else(|e| {
            tracing::warn!("cannot load transfer records: {e}");
            Vec::new()
        })
    }

    fn upsert(&self, job: &TransferJob) {
        if let Err(e) = self.try_upsert(job) {
            tracing::warn!("cannot save transfer record: {e}");
        }
    }

    fn delete(&self, ids: &[Uuid]) {
        if let Err(e) = self.try_delete(ids) {
            tracing::warn!("cannot delete transfer records: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_v1_database() {
        let path = std::env::temp_dir().join(format!("s3m-v1-{}.db", Uuid::new_v4()));
        {
            let c = Connection::open(&path).expect("open");
            c.execute_batch(
                "CREATE TABLE jobs (id TEXT PRIMARY KEY, account_id TEXT NOT NULL, direction INTEGER NOT NULL,
                 bucket TEXT NOT NULL, object_key TEXT NOT NULL, local_path TEXT NOT NULL, created_at TEXT NOT NULL,
                 state INTEGER NOT NULL, bytes_done INTEGER NOT NULL, bytes_total INTEGER NOT NULL,
                 attempts INTEGER NOT NULL, error TEXT NULL, finished_at TEXT NULL);
                 INSERT INTO jobs VALUES ('6f1c1f9e-6a43-4c55-9d0a-0e0f3b2f7a11', '6f1c1f9e-6a43-4c55-9d0a-0e0f3b2f7a12',
                 1, 'b', 'k', 'C:/x', '2026-10-16T11:07:00+07:00', 3, 1, 1, 1, NULL, NULL);",
            )
            .expect("v1");
        }
        let store = SqliteTransferStore::open(&path).expect("migrate");
        let jobs = store.load_all();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].source, None);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn upsert_load_delete() {
        let store = SqliteTransferStore::open_in_memory().expect("open");
        let mut job = TransferJob {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            direction: TransferDirection::Download,
            bucket: "b".into(),
            key: "a/ü.txt".into(),
            local_path: PathBuf::from(r"C:\Users\x\ü.txt"),
            created_at: Utc::now(),
            state: TransferState::Running,
            bytes_done: 1,
            bytes_total: 10,
            attempts: 1,
            error: None,
            finished_at: None,
            source: Some(ObjectRef {
                bucket: "src".into(),
                key: "a/b.txt".into(),
                account_id: Some(Uuid::new_v4()),
            }),
            version_id: Some("v1".into()),
            resume: Some("{}".into()),
            rate: 0,
        };
        store.upsert(&job);
        job.state = TransferState::Failed;
        job.error = Some("boom".into());
        job.finished_at = Some(Utc::now());
        store.upsert(&job);
        let loaded = store.load_all();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].state, TransferState::Failed);
        assert_eq!(loaded[0].error.as_deref(), Some("boom"));
        assert_eq!(loaded[0].key, job.key);
        assert_eq!(loaded[0].local_path, job.local_path);
        assert_eq!(loaded[0].source, job.source);
        assert_eq!(loaded[0].version_id, job.version_id);
        assert_eq!(loaded[0].resume, job.resume);
        store.delete(&[job.id]);
        assert!(store.load_all().is_empty());
    }
}
