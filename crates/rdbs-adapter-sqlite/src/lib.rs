//! SQLite adapter (rusqlite with a bundled SQLite).
//!
//! SQLite is an in-process library with a blocking API, so every call runs on Tokio's blocking
//! pool while holding the connection mutex. Cancellation uses `sqlite3_interrupt`, which is safe
//! to call from any thread.
//!
//! The profile's `host` field holds the database file path. The file must exist (a typo must
//! not silently create an empty database); `:memory:` opens a private in-memory database.

mod catalog;
mod values;

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use rdbs_core::adapter::row_count_mismatch;
use rdbs_core::edit::Change;
use rdbs_core::schema::ObjectPath;
use rdbs_core::{
    AdapterFactory, BoxFuture, CancelHandle, Capabilities, ColumnMeta, ConnectionProfile,
    DatabaseAdapter, DbError, DbErrorKind, DbObject, MAX_RESULT_ROWS, QueryOutcome, ResultSet,
    TableSchema,
};
use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::{Batch, Connection, InterruptHandle, OpenFlags};

/// Opens [`SqliteAdapter`] sessions.
#[derive(Debug, Default, Clone, Copy)]
pub struct SqliteFactory;

impl AdapterFactory for SqliteFactory {
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        _password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn DatabaseAdapter>, DbError>> {
        Box::pin(async move {
            let adapter: Box<dyn DatabaseAdapter> =
                Box::new(SqliteAdapter::open(profile.host.trim()).await?);
            Ok(adapter)
        })
    }
}

/// An open SQLite database.
pub struct SqliteAdapter {
    conn: Arc<Mutex<Connection>>,
    interrupt: Arc<InterruptHandle>,
    version: String,
}

impl SqliteAdapter {
    /// Opens an existing database file (or `:memory:`), with foreign keys enforced.
    pub async fn open(path: &str) -> Result<Self, DbError> {
        let path = path.to_owned();
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection, DbError> {
            let memory = path == ":memory:";
            if !memory && !Path::new(&path).is_file() {
                return Err(DbError::new(
                    DbErrorKind::Connection,
                    format!("database file not found: {path}"),
                )
                .with_detail("Choose an existing SQLite file (.db, .sqlite, .sqlite3)."));
            }
            let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            let conn = Connection::open_with_flags(&path, flags).map_err(connect_error)?;
            conn.execute_batch("PRAGMA foreign_keys = ON")
                .map_err(connect_error)?;
            // Fails early for files that are not SQLite databases.
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
                .map_err(connect_error)?;
            Ok(conn)
        })
        .await
        .map_err(join_error)??;
        let version = format!("SQLite {}", rusqlite::version());
        Ok(Self {
            interrupt: Arc::new(conn.get_interrupt_handle()),
            conn: Arc::new(Mutex::new(conn)),
            version,
        })
    }

    /// Runs `f` with the connection on the blocking pool.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, DbError> + Send + 'static,
    ) -> Result<T, DbError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().unwrap_or_else(PoisonError::into_inner);
            f(&mut conn)
        })
        .await
        .map_err(join_error)?
    }
}

impl DatabaseAdapter for SqliteAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            query_cancellation: true,
            streaming_results: false,
            transactions: true,
            schemas: false,
            multiple_databases: false,
            explain_plan: true,
        }
    }

    fn server_version(&self) -> &str {
        &self.version
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<(), DbError>> {
        // No server to time out. The file is open when this returns.
        Box::pin(self.blocking(|conn| {
            conn.query_row("SELECT 1", [], |_| Ok(()))
                .map_err(map_error)
        }))
    }

    fn load_objects(&mut self) -> BoxFuture<'_, Result<Vec<DbObject>, DbError>> {
        Box::pin(self.blocking(catalog::load))
    }

    fn execute<'a>(
        &'a mut self,
        sql: &'a str,
        cancel: CancelHandle,
    ) -> BoxFuture<'a, Result<QueryOutcome, DbError>> {
        let script = sql.to_owned();
        Box::pin(async move {
            let interrupt = self.interrupt.clone();
            let task = self.blocking(move |c| run_script(c, &script));
            tokio::pin!(task);
            tokio::select! {
                result = &mut task => result,
                () = cancel.cancelled() => {
                    interrupt.interrupt();
                    // Wait until SQLite has stopped, so the connection is free again.
                    let _ = task.await;
                    Err(DbError::cancelled())
                }
            }
        })
    }

    fn apply_changes<'a>(
        &'a mut self,
        changes: &'a [Change],
    ) -> BoxFuture<'a, Result<u64, DbError>> {
        let changes = changes.to_vec();
        Box::pin(self.blocking(move |c| {
            // Dropping the transaction without commit rolls it back.
            let tx = c.transaction().map_err(map_error)?;
            let mut total = 0;
            for (i, change) in changes.iter().enumerate() {
                let n = tx.execute(&change.sql, []).map_err(map_error)? as u64;
                if !change.accepts(n) {
                    return Err(row_count_mismatch(i, change, n));
                }
                total += n;
            }
            tx.commit().map_err(map_error)?;
            Ok(total)
        }))
    }

    fn object_ddl<'a>(
        &'a mut self,
        object: &'a ObjectPath,
    ) -> BoxFuture<'a, Result<String, DbError>> {
        let object = object.clone();
        Box::pin(self.blocking(move |c| catalog::object_ddl(c, &object)))
    }

    fn explain<'a>(
        &'a mut self,
        sql: &'a str,
        _analyze: bool,
    ) -> BoxFuture<'a, Result<rdbs_core::plan::ExplainPlan, DbError>> {
        let statement = sql.trim().trim_end_matches(';').to_owned();
        Box::pin(self.blocking(move |c| {
            let mut stmt = c
                .prepare(&format!("EXPLAIN QUERY PLAN {statement}"))
                .map_err(map_error)?;
            let rows: Vec<(i64, i64, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(3)?)))
                .and_then(Iterator::collect)
                .map_err(map_error)?;
            let raw = rows
                .iter()
                .map(|(id, parent, detail)| format!("{id}	{parent}	{detail}"))
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            Ok(rdbs_core::plan::from_parent_rows(&rows, raw))
        }))
    }

    fn describe_schema<'a>(
        &'a mut self,
        _schema: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<TableSchema>, DbError>> {
        Box::pin(self.blocking(catalog::describe))
    }
}

/// Executes every statement of `sql`, collecting result sets.
fn run_script(conn: &Connection, sql: &str) -> Result<QueryOutcome, DbError> {
    let mut outcome = QueryOutcome::default();
    let mut batch = Batch::new(conn, sql);
    while let Some(mut stmt) = batch.next().map_err(map_error)? {
        if stmt.column_count() == 0 {
            // SQLite keeps the count of the last data change, so other statements would
            // repeat it.
            let counted = stmt
                .expanded_sql()
                .is_none_or(|text| rdbs_core::sql::reports_row_count(&text));
            let n = stmt.raw_execute().map_err(map_error)?;
            if counted {
                outcome.record_rows_affected(n as u64);
            }
            continue;
        }
        let mut columns: Vec<ColumnMeta> = stmt
            .columns()
            .iter()
            .map(|c| ColumnMeta::new(c.name(), c.decl_type().unwrap_or_default()))
            .collect();
        let decl: Vec<String> = columns.iter().map(|c| c.type_name.clone()).collect();
        let mut rows = Vec::new();
        let mut raw = stmt.raw_query();
        while let Some(row) = raw.next().map_err(map_error)? {
            if rows.len() >= MAX_RESULT_ROWS {
                outcome.truncated = true;
                continue;
            }
            let cells = (0..decl.len())
                .map(|i| row.get_ref(i).map(|v| values::cell(v, &decl[i])))
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_error)?;
            rows.push(cells);
        }
        // Expressions have no declared type: name it after the first value's storage class.
        for (i, column) in columns.iter_mut().enumerate() {
            if column.type_name.is_empty() {
                column.type_name = rows
                    .iter()
                    .find_map(|r: &Vec<rdbs_core::CellValue>| values::storage_class(&r[i]))
                    .unwrap_or("")
                    .to_owned();
            }
        }
        outcome.push_result(ResultSet { columns, rows });
    }
    Ok(outcome)
}

fn join_error(e: tokio::task::JoinError) -> DbError {
    DbError::new(DbErrorKind::Internal, format!("SQLite task failed: {e}"))
}

fn connect_error(e: rusqlite::Error) -> DbError {
    let mut err = map_error(e);
    if err.kind == DbErrorKind::Query {
        err.kind = DbErrorKind::Connection;
    }
    err
}

/// Maps a rusqlite error, keeping SQLite's (extended) result code.
pub(crate) fn map_error(e: rusqlite::Error) -> DbError {
    match &e {
        rusqlite::Error::SqliteFailure(code, message) => {
            if code.code == rusqlite::ErrorCode::OperationInterrupted {
                return DbError::cancelled();
            }
            let kind = match code.code {
                rusqlite::ErrorCode::CannotOpen
                | rusqlite::ErrorCode::NotADatabase
                | rusqlite::ErrorCode::SystemIoFailure => DbErrorKind::Connection,
                rusqlite::ErrorCode::PermissionDenied
                | rusqlite::ErrorCode::AuthorizationForStatementDenied => {
                    DbErrorKind::Authentication
                }
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                    DbErrorKind::Timeout
                }
                _ => DbErrorKind::Query,
            };
            DbError::new(kind, message.clone().unwrap_or_else(|| code.to_string()))
                .with_code(code.extended_code.to_string())
        }
        rusqlite::Error::SqlInputError {
            error, msg, offset, ..
        } => DbError::new(DbErrorKind::Query, msg.clone())
            .with_code(error.extended_code.to_string())
            .with_detail(format!("at character {}", offset + 1)),
        _ => DbError::new(DbErrorKind::Query, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdbs_core::CellValue;

    async fn memory() -> SqliteAdapter {
        SqliteAdapter::open(":memory:").await.expect("open")
    }

    #[tokio::test]
    async fn runs_scripts_with_several_results() {
        let mut db = memory().await;
        let out = db
            .execute(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, ok BOOLEAN, at DATETIME); \
                 INSERT INTO t (name, ok, at) VALUES ('a', 1, '2026-01-02'), ('b', 0, NULL); \
                 SELECT * FROM t ORDER BY id; SELECT count(*) AS n, 1.5 AS f FROM t;",
                CancelHandle::new(),
            )
            .await
            .expect("script");
        assert_eq!(out.results.len(), 2);
        let first = &out.results[0];
        assert_eq!(first.columns[1].type_name, "TEXT");
        assert_eq!(first.rows[0][1], CellValue::Text("a".into()));
        assert_eq!(first.rows[0][2], CellValue::Bool(true));
        assert_eq!(first.rows[0][3], CellValue::Temporal("2026-01-02".into()));
        assert_eq!(first.rows[1][3], CellValue::Null);
        let second = &out.results[1];
        assert_eq!(second.columns[0].type_name, "INTEGER");
        assert_eq!(
            second.rows[0],
            vec![CellValue::Int(2), CellValue::Float(1.5)]
        );
    }

    #[tokio::test]
    async fn errors_keep_sqlite_codes() {
        let mut db = memory().await;
        let err = db
            .execute("SELEC 1", CancelHandle::new())
            .await
            .expect_err("syntax error");
        assert_eq!(err.kind, DbErrorKind::Query);
        assert_eq!(err.code.as_deref(), Some("1"));
        assert!(err.message.contains("syntax error"), "{}", err.message);
    }

    #[tokio::test]
    async fn cancellation_interrupts_long_queries() {
        let mut db = memory().await;
        let cancel = CancelHandle::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            trigger.cancel();
        });
        let started = std::time::Instant::now();
        let err = db
            .execute(
                "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) \
                 SELECT count(*) FROM c",
                cancel,
            )
            .await
            .expect_err("cancelled");
        assert_eq!(err.kind, DbErrorKind::Cancelled);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        // The connection is usable afterwards.
        let out = db
            .execute("SELECT 1", CancelHandle::new())
            .await
            .expect("usable");
        assert_eq!(out.results[0].rows[0][0], CellValue::Int(1));
    }

    #[tokio::test]
    async fn apply_changes_is_transactional() {
        let mut db = memory().await;
        db.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT); INSERT INTO t VALUES (1, 'a'), (2, 'b')",
            CancelHandle::new(),
        )
        .await
        .expect("setup");
        let change = |sql: &str| Change {
            sql: sql.into(),
            expected_rows: 1,
        };
        let err = db
            .apply_changes(&[
                change("UPDATE t SET v = 'z' WHERE id = 1"),
                change("UPDATE t SET v = 'q' WHERE id = 99"),
            ])
            .await
            .expect_err("second change matches nothing");
        assert!(err.message.contains("change 2"), "{}", err.message);
        let out = db
            .execute("SELECT v FROM t ORDER BY id", CancelHandle::new())
            .await
            .expect("select");
        assert_eq!(out.results[0].rows[0][0], CellValue::Text("a".into()));
        assert_eq!(
            db.apply_changes(&[change("UPDATE t SET v = 'z' WHERE id = 1")])
                .await
                .expect("ok"),
            1
        );
    }

    #[tokio::test]
    async fn missing_files_are_not_created() {
        let path = std::env::temp_dir().join("rdbs-definitely-missing.sqlite");
        let err = SqliteAdapter::open(&path.display().to_string())
            .await
            .err()
            .expect("missing file");
        assert_eq!(err.kind, DbErrorKind::Connection);
        assert!(!path.exists());
    }
}
