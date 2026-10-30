//! Oracle Database adapter (rust-oracle / ODPI-C).
//!
//! Requires **Oracle Instant Client** (or a full client) on the machine at run time: ODPI-C
//! loads the OCI library dynamically, so building does not need it, but connecting does.
//!
//! The profile's database is the service name of an Easy Connect string
//! (`//host:port/service`); a host that already contains `/` or a TNS alias is used as is.
//! Calls are blocking, so they run on Tokio's blocking pool; cancellation uses
//! `OCIBreak` (`Connection::break_execution`), which is safe from another thread.

mod catalog;

use std::sync::Arc;

use oracle::sql_type::OracleType;
use rdbs_core::adapter::row_count_mismatch;
use rdbs_core::edit::Change;
use rdbs_core::plan::ExplainPlan;
use rdbs_core::schema::ObjectPath;
use rdbs_core::{
    AdapterFactory, BoxFuture, CancelHandle, Capabilities, CellValue, ColumnMeta,
    ConnectionProfile, DatabaseAdapter, DbError, DbErrorKind, DbObject, MAX_RESULT_ROWS,
    QueryOutcome, ResultSet, TableSchema,
};

/// Opens [`OracleAdapter`] sessions.
#[derive(Debug, Default, Clone, Copy)]
pub struct OracleFactory;

impl AdapterFactory for OracleFactory {
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn DatabaseAdapter>, DbError>> {
        Box::pin(async move {
            let adapter: Box<dyn DatabaseAdapter> =
                Box::new(OracleAdapter::connect(profile, password).await?);
            Ok(adapter)
        })
    }
}

/// An open Oracle session.
pub struct OracleAdapter {
    conn: Arc<oracle::Connection>,
    version: String,
}

/// Easy Connect string for a profile.
pub fn connect_string(profile: &ConnectionProfile) -> String {
    let host = profile.host.trim();
    if host.contains('/') || (!host.contains('.') && !host.contains(':') && profile.port == 0) {
        return host.to_owned();
    }
    let service = if profile.database.trim().is_empty() {
        "FREEPDB1"
    } else {
        profile.database.trim()
    };
    format!("//{host}:{}/{service}", profile.port)
}

/// Maps an Oracle error, keeping the ORA-/DPI- code.
pub(crate) fn map_error(e: oracle::Error) -> DbError {
    use oracle::ErrorKind;
    let (code, message) = match e.db_error() {
        Some(db) => (db.code(), db.message().to_owned()),
        None => (0, e.to_string()),
    };
    if e.kind() == ErrorKind::DpiError && message.contains("DPI-1047") {
        return DbError::new(
            DbErrorKind::Connection,
            "Oracle Instant Client was not found",
        )
        .with_code("DPI-1047")
        .with_detail(
            "Install Oracle Instant Client (Basic or Basic Light, 64-bit) and add its folder \
                 to PATH, then restart Zephix.",
        );
    }
    let kind = match code {
        1013 => return DbError::cancelled(),
        1017 | 1045 | 28000 | 28001 => DbErrorKind::Authentication,
        3113 | 3114 | 3135 | 12154 | 12170 | 12514 | 12505 | 12541 | 12543 => {
            DbErrorKind::Connection
        }
        _ if e.kind() == ErrorKind::DpiError => DbErrorKind::Connection,
        _ => DbErrorKind::Query,
    };
    let err = DbError::new(kind, message);
    if code != 0 {
        err.with_code(format!("ORA-{code:05}"))
    } else {
        err
    }
}

fn join_error(e: tokio::task::JoinError) -> DbError {
    DbError::new(DbErrorKind::Internal, format!("Oracle task failed: {e}"))
}

/// Native type name for the result header.
fn type_name(t: &OracleType) -> String {
    t.to_string()
}

/// One value of a row as a grid cell.
fn cell(row: &oracle::Row, i: usize, t: &OracleType) -> Result<CellValue, oracle::Error> {
    let text = |row: &oracle::Row| row.get::<usize, Option<String>>(i);
    Ok(match t {
        OracleType::Number(_, 0) | OracleType::Int64 => match row.get::<usize, Option<i64>>(i) {
            Ok(Some(v)) => CellValue::Int(v),
            Ok(None) => CellValue::Null,
            // Larger than i64: keep the exact text.
            Err(_) => text(row)?.map_or(CellValue::Null, CellValue::Decimal),
        },
        OracleType::Number(..) | OracleType::Float(_) => {
            text(row)?.map_or(CellValue::Null, CellValue::Decimal)
        }
        OracleType::BinaryDouble | OracleType::BinaryFloat => row
            .get::<usize, Option<f64>>(i)?
            .map_or(CellValue::Null, CellValue::Float),
        OracleType::Boolean => row
            .get::<usize, Option<bool>>(i)?
            .map_or(CellValue::Null, CellValue::Bool),
        OracleType::Date
        | OracleType::Timestamp(_)
        | OracleType::TimestampTZ(_)
        | OracleType::TimestampLTZ(_)
        | OracleType::IntervalDS(..)
        | OracleType::IntervalYM(_) => text(row)?.map_or(CellValue::Null, CellValue::Temporal),
        OracleType::Raw(_) | OracleType::BLOB | OracleType::LongRaw => row
            .get::<usize, Option<Vec<u8>>>(i)?
            .map_or(CellValue::Null, CellValue::Bytes),
        OracleType::Json => text(row)?.map_or(CellValue::Null, CellValue::Json),
        _ => text(row)?.map_or(CellValue::Null, CellValue::Text),
    })
}

/// Splits a script into statements. PL/SQL blocks (`BEGIN`, `DECLARE`, `CREATE … PROCEDURE`)
/// end at a line containing only `/`; other statements end at `;`.
pub fn split_script(script: &str) -> Vec<String> {
    fn is_plsql(text: &str) -> bool {
        let head = text.trim_start().to_ascii_uppercase();
        let words: Vec<&str> = head.split_whitespace().take(6).collect();
        matches!(words.first(), Some(&"BEGIN") | Some(&"DECLARE"))
            || (words.first() == Some(&"CREATE")
                && words.iter().any(|w| {
                    matches!(
                        *w,
                        "PROCEDURE" | "FUNCTION" | "PACKAGE" | "TRIGGER" | "TYPE" | "BODY"
                    )
                }))
    }
    let mut out = Vec::new();
    for chunk in script
        .split('\n')
        .collect::<Vec<_>>()
        .split(|l| l.trim() == "/")
    {
        let text = chunk.join("\n");
        for range in rdbs_core::sql::split_statements(&text) {
            let piece = &text[range.clone()];
            if is_plsql(piece) {
                // The block runs to the end of the chunk (terminated by "/").
                let block = text[range.start..].trim();
                if !block.is_empty() {
                    out.push(block.to_owned());
                }
                break;
            }
            let s = piece.trim().trim_end_matches(';').trim();
            if !s.is_empty() {
                out.push(s.to_owned());
            }
        }
    }
    out
}

fn run_script(conn: &oracle::Connection, script: &str) -> Result<QueryOutcome, DbError> {
    let mut outcome = QueryOutcome::default();
    for sql in split_script(script) {
        let mut stmt = conn.statement(&sql).build().map_err(map_error)?;
        if stmt.is_query() {
            let rows = stmt.query(&[]).map_err(map_error)?;
            let info: Vec<(String, OracleType)> = rows
                .column_info()
                .iter()
                .map(|c| (c.name().to_owned(), c.oracle_type().clone()))
                .collect();
            let mut data = Vec::new();
            for row in rows {
                let row = row.map_err(map_error)?;
                if data.len() >= MAX_RESULT_ROWS {
                    outcome.truncated = true;
                    break;
                }
                let cells = info
                    .iter()
                    .enumerate()
                    .map(|(i, (_, t))| cell(&row, i, t))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(map_error)?;
                data.push(cells);
            }
            outcome.push_result(ResultSet {
                columns: info
                    .iter()
                    .map(|(n, t)| ColumnMeta::new(n.clone(), type_name(t)))
                    .collect(),
                rows: data,
            });
        } else {
            stmt.execute(&[]).map_err(map_error)?;
            let n = stmt.row_count().map_err(map_error)?;
            // Editor statements commit like an autocommit session.
            conn.commit().map_err(map_error)?;
            if stmt.is_dml() {
                outcome.record_rows_affected(n);
            } else {
                outcome.messages.push("Statement executed".into());
            }
        }
    }
    Ok(outcome)
}

impl OracleAdapter {
    /// Connects. Autocommit stays off so grid edits can run in one transaction; editor
    /// statements commit one by one.
    pub async fn connect(
        profile: &ConnectionProfile,
        password: Option<&str>,
    ) -> Result<Self, DbError> {
        let user = profile.username.trim().to_owned();
        let password = password.unwrap_or_default().to_owned();
        let target = connect_string(profile);
        let conn = tokio::task::spawn_blocking(move || -> Result<oracle::Connection, DbError> {
            let conn = oracle::Connection::connect(&user, &password, &target).map_err(|e| {
                let mut err = map_error(e);
                if err.kind == DbErrorKind::Query {
                    err.kind = DbErrorKind::Connection;
                }
                err
            })?;
            Ok(conn)
        })
        .await
        .map_err(join_error)??;
        let version = conn
            .server_version()
            .map(|(v, banner)| {
                let first = banner
                    .lines()
                    .next()
                    .unwrap_or("Oracle Database")
                    .to_owned();
                if first.contains(&v.to_string()) {
                    first
                } else {
                    format!("Oracle Database {v}")
                }
            })
            .unwrap_or_else(|_| "Oracle Database".into());
        Ok(Self {
            conn: Arc::new(conn),
            version,
        })
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&oracle::Connection) -> Result<T, DbError> + Send + 'static,
    ) -> Result<T, DbError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || f(&conn))
            .await
            .map_err(join_error)?
    }
}

impl DatabaseAdapter for OracleAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            query_cancellation: true,
            streaming_results: false,
            transactions: true,
            schemas: true,
            multiple_databases: false,
            explain_plan: true,
        }
    }

    fn server_version(&self) -> &str {
        &self.version
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
            let conn = self.conn.clone();
            let task = self.blocking(move |c| run_script(c, &script));
            tokio::pin!(task);
            tokio::select! {
                result = &mut task => result,
                () = cancel.cancelled() => {
                    let breaker = conn.clone();
                    let _ = tokio::task::spawn_blocking(move || breaker.break_execution()).await;
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
            // Autocommit is off: all changes commit together or roll back.
            let mut total = 0;
            for (i, change) in changes.iter().enumerate() {
                let mut stmt = c.statement(&change.sql).build().map_err(map_error)?;
                if let Err(e) = stmt.execute(&[]) {
                    let _ = c.rollback();
                    return Err(map_error(e));
                }
                let n = stmt.row_count().map_err(map_error)?;
                if !change.accepts(n) {
                    let _ = c.rollback();
                    return Err(row_count_mismatch(i, change, n));
                }
                total += n;
            }
            c.commit().map_err(map_error)?;
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

    fn routine_signature<'a>(
        &'a mut self,
        object: &'a ObjectPath,
    ) -> BoxFuture<'a, Result<rdbs_core::routine::RoutineSignature, DbError>> {
        let object = object.clone();
        Box::pin(self.blocking(move |c| catalog::routine_signature(c, &object)))
    }

    fn describe_schema<'a>(
        &'a mut self,
        schema: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<TableSchema>, DbError>> {
        let schema = schema.map(str::to_owned);
        Box::pin(self.blocking(move |c| catalog::describe(c, schema.as_deref())))
    }

    fn explain<'a>(
        &'a mut self,
        sql: &'a str,
        _analyze: bool,
    ) -> BoxFuture<'a, Result<ExplainPlan, DbError>> {
        let statement = sql.trim().trim_end_matches(';').to_owned();
        Box::pin(self.blocking(move |c| catalog::explain(c, &statement)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sql_and_plsql() {
        let parts = split_script(
            "SELECT 1 FROM dual; UPDATE t SET a = 1;\nBEGIN\n  NULL;\nEND;\n/\nSELECT 2 FROM dual",
        );
        assert_eq!(parts.len(), 4, "{parts:?}");
        assert_eq!(parts[0], "SELECT 1 FROM dual");
        assert!(
            parts[2].starts_with("BEGIN") && parts[2].ends_with("END;"),
            "{}",
            parts[2]
        );
        assert_eq!(parts[3], "SELECT 2 FROM dual");
    }

    #[test]
    fn easy_connect_strings() {
        let mut p = ConnectionProfile::new(rdbs_core::DatabaseKind::Oracle);
        p.host = "db.local".into();
        p.port = 1521;
        p.database = "ORCLPDB1".into();
        assert_eq!(connect_string(&p), "//db.local:1521/ORCLPDB1");
        p.host = "db.local:1522/OTHER".into();
        assert_eq!(connect_string(&p), "db.local:1522/OTHER");
    }
}
