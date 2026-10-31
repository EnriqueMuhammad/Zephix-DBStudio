//! PostgreSQL adapter built on `tokio-postgres` (ADR 0004).
//!
//! Scripts run through the simple-query protocol, which executes all statements in one round
//! trip and returns every value in PostgreSQL's own text format. Native column types are then
//! obtained by preparing the statement that produced the displayed result set.

mod catalog;
mod ddl;
mod describe;
mod tls;
mod values;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::StreamExt;
use rdbs_core::adapter::row_count_mismatch;
use rdbs_core::edit::Change;
use rdbs_core::sql;
use rdbs_core::{
    AdapterFactory, BoxFuture, CancelHandle, Capabilities, CellValue, ColumnMeta,
    ConnectionProfile, DatabaseAdapter, DbError, DbErrorKind, DbObject, MAX_RESULT_ROWS,
    QueryOutcome, ResultSet, rows_affected_message,
};
use tokio::task::JoinHandle;
use tokio_postgres::types::Type;
use tokio_postgres::{AsyncMessage, Client, SimpleQueryMessage};
use tokio_postgres_rustls::MakeRustlsConnect;

/// Opens PostgreSQL sessions.
#[derive(Debug, Default, Clone, Copy)]
pub struct PostgresFactory;

impl AdapterFactory for PostgresFactory {
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn DatabaseAdapter>, DbError>> {
        Box::pin(async move {
            let adapter: Box<dyn DatabaseAdapter> =
                Box::new(PostgresAdapter::connect(profile, password).await?);
            Ok(adapter)
        })
    }
}

/// An open PostgreSQL session.
pub struct PostgresAdapter {
    client: Client,
    tls: MakeRustlsConnect,
    connection_task: JoinHandle<()>,
    /// Server notices collected by the connection task (`RAISE NOTICE`, warnings).
    notices: Arc<Mutex<Vec<String>>>,
    version: String,
}

impl PostgresAdapter {
    /// Connects using the profile's host, port, database, user and SSL mode.
    pub async fn connect(
        profile: &ConnectionProfile,
        password: Option<&str>,
    ) -> Result<Self, DbError> {
        let mut config = tokio_postgres::Config::new();
        config
            .host(&profile.host)
            .port(profile.port)
            .user(&profile.username)
            .application_name(rdbs_core::brand::APP_NAME)
            .connect_timeout(Duration::from_secs(10));
        if !profile.database.is_empty() {
            config.dbname(&profile.database);
        }
        if let Some(password) = password {
            config.password(password);
        }
        let (ssl_mode, tls) = tls::connector(profile.ssl_mode)?;
        config.ssl_mode(ssl_mode);

        let (client, connection) = config.connect(tls.clone()).await.map_err(connect_error)?;
        // Drive the connection ourselves so asynchronous messages (notices) are not dropped.
        let notices: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = notices.clone();
        let mut connection = connection;
        let connection_task = tokio::spawn(async move {
            let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
            while let Some(message) = messages.next().await {
                match message {
                    Ok(AsyncMessage::Notice(notice)) => {
                        sink.lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push(format!("{}: {}", notice.severity(), notice.message()));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("PostgreSQL connection closed with error: {e}");
                        break;
                    }
                }
            }
        });

        let version = catalog::text_rows(&client, "SELECT version()")
            .await?
            .first()
            .and_then(|r| r.first().cloned().flatten())
            .map(|v| short_version(&v))
            .unwrap_or_else(|| "PostgreSQL".to_owned());

        Ok(Self {
            client,
            tls,
            connection_task,
            notices,
            version,
        })
    }

    async fn run(&self, script: &str) -> Result<QueryOutcome, DbError> {
        let stream = self
            .client
            .simple_query_raw(script)
            .await
            .map_err(query_error)?;
        futures_util::pin_mut!(stream);

        let mut outcome = QueryOutcome::default();
        let statements = sql::split_statements(script);
        let mut statement_index = 0usize;
        let mut current: Option<RawSet> = None;
        let mut sets: Vec<RawSet> = Vec::new();
        self.take_notices();

        while let Some(message) = stream.next().await {
            match message.map_err(query_error)? {
                SimpleQueryMessage::RowDescription(columns) => {
                    current = Some(RawSet {
                        statement_index,
                        names: columns.iter().map(|c| c.name().to_owned()).collect(),
                        rows: Vec::new(),
                    });
                }
                SimpleQueryMessage::Row(row) => {
                    if let Some(set) = current.as_mut() {
                        if set.rows.len() < MAX_RESULT_ROWS {
                            set.rows.push(
                                (0..row.len())
                                    .map(|i| row.get(i).map(str::to_owned))
                                    .collect(),
                            );
                        } else {
                            outcome.truncated = true;
                        }
                    }
                }
                SimpleQueryMessage::CommandComplete(count) => {
                    match current.take() {
                        Some(set) => {
                            // The server's count includes rows beyond the truncation limit.
                            outcome.messages.push(rows_affected_message(count));
                            sets.push(set);
                        }
                        None => {
                            // BEGIN, CREATE, … complete with a count of zero that means
                            // nothing; a statement the splitter did not see is reported.
                            let counted = statements
                                .get(statement_index)
                                .is_none_or(|r| sql::reports_row_count(&script[r.clone()]));
                            if counted {
                                outcome.record_rows_affected(count);
                            }
                        }
                    }
                    statement_index += 1;
                }
                _ => {}
            }
        }

        outcome.messages.extend(self.take_notices());
        if outcome.truncated {
            outcome.messages.push(format!(
                "Result truncated to the first {MAX_RESULT_ROWS} rows"
            ));
        }
        for set in sets {
            let types = self.column_types(script, &statements, &set).await;
            outcome.results.push(set.into_result(types.as_deref()));
        }
        Ok(outcome)
    }

    /// Drains NOTICE/WARNING messages received since the last call.
    fn take_notices(&self) -> Vec<String> {
        let mut notices = self.notices.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *notices)
    }

    /// Best-effort native types for a result set: prepares the statement that produced it.
    async fn column_types(
        &self,
        script: &str,
        statements: &[std::ops::Range<usize>],
        set: &RawSet,
    ) -> Option<Vec<Type>> {
        let range = statements.get(set.statement_index)?.clone();
        let statement = self.client.prepare(script.get(range)?).await.ok()?;
        let columns = statement.columns();
        let matches = columns.len() == set.names.len()
            && columns.iter().zip(&set.names).all(|(c, n)| c.name() == n);
        matches.then(|| columns.iter().map(|c| c.type_().clone()).collect())
    }
}

/// Rows of one result set as received, before type information is attached.
struct RawSet {
    statement_index: usize,
    names: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
}

impl RawSet {
    fn into_result(self, types: Option<&[Type]>) -> ResultSet {
        let ty = |i: usize| types.and_then(|t| t.get(i));
        let columns = self
            .names
            .iter()
            .enumerate()
            .map(|(i, name)| ColumnMeta::new(name.clone(), ty(i).map_or("unknown", |t| t.name())))
            .collect();
        let rows = self
            .rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .enumerate()
                    .map(|(i, v)| values::cell(ty(i), v))
                    .collect::<Vec<CellValue>>()
            })
            .collect();
        ResultSet { columns, rows }
    }
}

impl Drop for PostgresAdapter {
    fn drop(&mut self) {
        self.connection_task.abort();
    }
}

impl DatabaseAdapter for PostgresAdapter {
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
        Box::pin(catalog::load(&self.client))
    }

    fn object_ddl<'a>(
        &'a mut self,
        object: &'a rdbs_core::schema::ObjectPath,
    ) -> BoxFuture<'a, Result<String, DbError>> {
        Box::pin(ddl::object_ddl(&self.client, object))
    }

    fn routine_signature<'a>(
        &'a mut self,
        object: &'a rdbs_core::schema::ObjectPath,
    ) -> BoxFuture<'a, Result<rdbs_core::routine::RoutineSignature, DbError>> {
        Box::pin(ddl::routine_signature(&self.client, object))
    }

    fn explain<'a>(
        &'a mut self,
        sql: &'a str,
        analyze: bool,
    ) -> BoxFuture<'a, Result<rdbs_core::plan::ExplainPlan, DbError>> {
        Box::pin(async move {
            let statement = sql.trim().trim_end_matches(';');
            let options = if analyze {
                "FORMAT JSON, ANALYZE, BUFFERS"
            } else {
                "FORMAT JSON"
            };
            // ANALYZE executes the statement: run it in a transaction that is rolled back.
            if analyze {
                self.client
                    .batch_execute("BEGIN")
                    .await
                    .map_err(query_error)?;
            }
            let rows =
                catalog::text_rows(&self.client, &format!("EXPLAIN ({options}) {statement}")).await;
            if analyze && let Err(e) = self.client.batch_execute("ROLLBACK").await {
                tracing::warn!("rollback after EXPLAIN ANALYZE failed: {e}");
            }
            let text: Vec<String> = rows?.iter().map(|r| catalog::col(r, 0)).collect();
            rdbs_core::plan::parse_postgres_json(&text.join(
                "
",
            ))
        })
    }

    fn describe_schema<'a>(
        &'a mut self,
        schema: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<rdbs_core::TableSchema>, DbError>> {
        Box::pin(describe::describe(&self.client, schema))
    }

    fn apply_changes<'a>(
        &'a mut self,
        changes: &'a [Change],
    ) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async move {
            // Dropping the transaction without commit rolls it back.
            let tx = self.client.transaction().await.map_err(query_error)?;
            let mut total = 0;
            for (i, change) in changes.iter().enumerate() {
                let n = tx
                    .execute(change.sql.as_str(), &[])
                    .await
                    .map_err(query_error)?;
                if !change.accepts(n) {
                    return Err(row_count_mismatch(i, change, n));
                }
                total += n;
            }
            tx.commit().await.map_err(query_error)?;
            Ok(total)
        })
    }

    fn execute<'a>(
        &'a mut self,
        script: &'a str,
        cancel: CancelHandle,
    ) -> BoxFuture<'a, Result<QueryOutcome, DbError>> {
        Box::pin(async move {
            let token = self.client.cancel_token();
            let tls = self.tls.clone();
            tokio::select! {
                result = self.run(script) => result,
                () = cancel.cancelled() => {
                    if let Err(e) = token.cancel_query(tls).await {
                        tracing::warn!("PostgreSQL cancel request failed: {e}");
                    }
                    Err(DbError::cancelled())
                }
            }
        })
    }
}

/// `PostgreSQL 17.2 on x86_64-pc-linux-gnu, compiled by …` → `PostgreSQL 17.2`.
fn short_version(full: &str) -> String {
    full.split(" on ").next().unwrap_or(full).trim().to_owned()
}

/// Maps a driver error raised while executing SQL.
pub(crate) fn query_error(e: tokio_postgres::Error) -> DbError {
    map_error(e, DbErrorKind::Query)
}

fn connect_error(e: tokio_postgres::Error) -> DbError {
    map_error(e, DbErrorKind::Connection)
}

fn map_error(e: tokio_postgres::Error, fallback: DbErrorKind) -> DbError {
    if let Some(db) = e.as_db_error() {
        let code = db.code().code();
        let kind = if code.starts_with("28") {
            DbErrorKind::Authentication
        } else if code == "57014" {
            DbErrorKind::Cancelled
        } else if code.starts_with("08") || matches!(code, "57P01" | "57P02" | "57P03") {
            // Connection exception, or the server shut the session down.
            DbErrorKind::Connection
        } else {
            DbErrorKind::Query
        };
        let mut err = DbError::new(kind, db.message()).with_code(code);
        let detail = [db.detail(), db.hint()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        if !detail.is_empty() {
            err = err.with_detail(detail);
        }
        return err;
    }
    // Non-server errors (I/O, TLS, timeouts) — include the source chain.
    // A closed connection during a query must not stay filed as a SQL error, or the worker
    // keeps the dead session and every later statement fails the same way.
    let fallback = if e.is_closed() {
        DbErrorKind::Connection
    } else {
        fallback
    };
    let mut message = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        message.push_str(": ");
        message.push_str(&s.to_string());
        source = s.source();
    }
    DbError::new(fallback, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_version_banner() {
        assert_eq!(
            short_version(
                "PostgreSQL 17.2 (Debian 17.2-1) on x86_64-pc-linux-gnu, compiled by gcc"
            ),
            "PostgreSQL 17.2 (Debian 17.2-1)"
        );
    }

    #[test]
    fn raw_set_without_types_falls_back_to_text() {
        let set = RawSet {
            statement_index: 0,
            names: vec!["a".into()],
            rows: vec![vec![Some("1".into())], vec![None]],
        };
        let rs = set.into_result(None);
        assert_eq!(rs.columns[0].type_name, "unknown");
        assert_eq!(rs.rows[0][0], CellValue::Text("1".into()));
        assert_eq!(rs.rows[1][0], CellValue::Null);
    }

    #[test]
    fn raw_set_with_types_is_classified() {
        let set = RawSet {
            statement_index: 0,
            names: vec!["n".into()],
            rows: vec![vec![Some("7".into())]],
        };
        let rs = set.into_result(Some(&[Type::INT8]));
        assert_eq!(rs.columns[0].type_name, "int8");
        assert_eq!(rs.rows[0][0], CellValue::Int(7));
    }
}
