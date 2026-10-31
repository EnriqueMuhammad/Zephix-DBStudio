//! MySQL / MariaDB adapter built on `mysql_async` (ADR 0004).
//!
//! The whole script is sent as one multi-statement `COM_QUERY`, so compound statements
//! (`CREATE PROCEDURE … BEGIN … END`) need no client-side splitting or `DELIMITER`. The last
//! result set is displayed. Cancellation issues `KILL QUERY <id>` from a second connection.

mod catalog;
mod ddl;
mod describe;
mod values;

use std::time::Duration;

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, OptsBuilder, SslOpts};
use rdbs_core::adapter::row_count_mismatch;
use rdbs_core::edit::Change;
use rdbs_core::{
    AdapterFactory, BoxFuture, CancelHandle, Capabilities, ColumnMeta, ConnectionProfile,
    DatabaseAdapter, DatabaseKind, DbError, DbErrorKind, DbObject, MAX_RESULT_ROWS, QueryOutcome,
    ResultSet, SslMode,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Opens MySQL and MariaDB sessions.
#[derive(Debug, Default, Clone, Copy)]
pub struct MySqlFactory;

impl AdapterFactory for MySqlFactory {
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn DatabaseAdapter>, DbError>> {
        Box::pin(async move {
            let adapter: Box<dyn DatabaseAdapter> =
                Box::new(MySqlAdapter::connect(profile, password).await?);
            Ok(adapter)
        })
    }
}

/// An open MySQL/MariaDB session.
pub struct MySqlAdapter {
    conn: Option<Conn>,
    /// Options used to open the side connection for `KILL QUERY`.
    opts: Opts,
    version: String,
}

fn build_opts(profile: &ConnectionProfile, password: Option<&str>, ssl: Option<SslOpts>) -> Opts {
    let db = (!profile.database.is_empty()).then(|| profile.database.clone());
    OptsBuilder::default()
        .ip_or_hostname(profile.host.clone())
        .tcp_port(profile.port)
        .user(Some(profile.username.clone()))
        .pass(password.map(str::to_owned))
        .db_name(db)
        .prefer_socket(false)
        .ssl_opts(ssl)
        // Affected rows count matched rows, as on the other engines: an UPDATE that writes the
        // values a row already has still counts, so grid edits and sync do not mistake it for
        // a row changed by someone else.
        .client_found_rows(true)
        .into()
}

/// TLS without certificate verification (`Prefer` / `Require`).
fn unverified_tls() -> SslOpts {
    SslOpts::default()
        .with_danger_accept_invalid_certs(true)
        .with_danger_skip_domain_validation(true)
}

/// `mysql_async` builds its rustls config from the process-wide default `CryptoProvider`.
/// rustls cannot pick one automatically when several providers are compiled in (feature
/// unification across the workspace), so `ring` is installed once before the first connect.
fn ensure_crypto_provider() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        // Err means another provider is already installed, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

async fn open(opts: Opts) -> Result<Conn, DbError> {
    ensure_crypto_provider();
    match tokio::time::timeout(CONNECT_TIMEOUT, Conn::new(opts)).await {
        Ok(result) => result.map_err(connect_error),
        Err(_) => Err(DbError::new(
            DbErrorKind::Timeout,
            format!("connection timed out after {}s", CONNECT_TIMEOUT.as_secs()),
        )),
    }
}

impl MySqlAdapter {
    /// Connects using the profile's settings. `Prefer` tries TLS first and falls back to a
    /// plain connection when TLS cannot be negotiated (but not on authentication errors).
    pub async fn connect(
        profile: &ConnectionProfile,
        password: Option<&str>,
    ) -> Result<Self, DbError> {
        let (mut conn, opts) = match profile.ssl_mode {
            SslMode::Disable => {
                let opts = build_opts(profile, password, None);
                (open(opts.clone()).await?, opts)
            }
            SslMode::Require => {
                let opts = build_opts(profile, password, Some(unverified_tls()));
                (open(opts.clone()).await?, opts)
            }
            SslMode::VerifyFull => {
                let opts = build_opts(profile, password, Some(SslOpts::default()));
                (open(opts.clone()).await?, opts)
            }
            SslMode::Prefer => {
                let tls = build_opts(profile, password, Some(unverified_tls()));
                match open(tls.clone()).await {
                    Ok(conn) => (conn, tls),
                    Err(e) if e.kind == DbErrorKind::Authentication => return Err(e),
                    Err(e) => {
                        tracing::info!("TLS connection failed ({e}); retrying without TLS");
                        let plain = build_opts(profile, password, None);
                        (open(plain.clone()).await?, plain)
                    }
                }
            }
        };

        let raw: Option<String> = conn
            .query_first("SELECT VERSION()")
            .await
            .map_err(map_error)?;
        let version = describe_version(profile.kind, raw.as_deref().unwrap_or("?"));
        Ok(Self {
            conn: Some(conn),
            opts,
            version,
        })
    }

    fn conn(&mut self) -> Result<&mut Conn, DbError> {
        self.conn
            .as_mut()
            .ok_or_else(|| DbError::new(DbErrorKind::Connection, "connection is closed"))
    }

    async fn run(&mut self, script: &str) -> Result<QueryOutcome, DbError> {
        let conn = self.conn()?;
        let mut result = conn.query_iter(script).await.map_err(map_error)?;
        let mut outcome = QueryOutcome::default();
        let ranges = rdbs_core::sql::split_statements(script);
        let mut statements = 0usize;
        let mut warnings = 0u64;

        // Each iteration consumes one result set; the bound guards against a driver state
        // that never reports completion.
        for _ in 0..100_000 {
            match result.columns() {
                Some(columns) if !columns.is_empty() => {
                    let meta: Vec<ColumnMeta> = columns
                        .iter()
                        .map(|c| {
                            ColumnMeta::new(
                                c.name_str(),
                                values::type_name(c.column_type(), c.flags(), c.character_set()),
                            )
                        })
                        .collect();
                    let classes: Vec<values::Class> = columns
                        .iter()
                        .map(|c| values::classify(c.column_type(), c.character_set()))
                        .collect();
                    let mut rows = Vec::new();
                    while let Some(row) = result.next().await.map_err(map_error)? {
                        if rows.len() >= MAX_RESULT_ROWS {
                            outcome.truncated = true;
                            continue;
                        }
                        rows.push(
                            row.unwrap()
                                .into_iter()
                                .zip(&classes)
                                .map(|(v, class)| values::cell(*class, v))
                                .collect(),
                        );
                    }
                    outcome.push_result(ResultSet {
                        columns: meta,
                        rows,
                    });
                }
                _ => {
                    // BEGIN, CREATE, … report zero rows; only data changes are listed.
                    let counted = ranges
                        .get(statements)
                        .is_none_or(|r| rdbs_core::sql::reports_row_count(&script[r.clone()]));
                    if counted {
                        outcome.record_rows_affected(result.affected_rows());
                    }
                    while result.next().await.map_err(map_error)?.is_some() {}
                }
            }
            warnings += u64::from(result.warnings());
            statements += 1;
            if result.is_empty() {
                break;
            }
        }

        if warnings > 0 {
            outcome.messages.push(format!(
                "{warnings} warning(s) — run SHOW WARNINGS for details"
            ));
        }
        if outcome.truncated {
            outcome.messages.push(format!(
                "Result truncated to the first {MAX_RESULT_ROWS} rows"
            ));
        }
        Ok(outcome)
    }
}

/// Stops the statement running on connection `id` using a short-lived side connection.
async fn kill_query(opts: Opts, id: u32) -> Result<(), DbError> {
    let mut side = open(opts).await?;
    side.query_drop(format!("KILL QUERY {id}"))
        .await
        .map_err(map_error)?;
    side.disconnect().await.map_err(map_error)
}

impl DatabaseAdapter for MySqlAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            query_cancellation: true,
            streaming_results: false,
            transactions: true,
            schemas: false,
            multiple_databases: true,
            explain_plan: true,
        }
    }

    fn server_version(&self) -> &str {
        &self.version
    }

    fn load_objects(&mut self) -> BoxFuture<'_, Result<Vec<DbObject>, DbError>> {
        Box::pin(async move { catalog::load(self.conn()?).await })
    }

    fn explain<'a>(
        &'a mut self,
        sql: &'a str,
        analyze: bool,
    ) -> BoxFuture<'a, Result<rdbs_core::plan::ExplainPlan, DbError>> {
        Box::pin(async move {
            let mariadb = self.version.contains("MariaDB");
            let statement = sql.trim().trim_end_matches(';');
            let prefix = match (mariadb, analyze) {
                (true, true) => "ANALYZE FORMAT=JSON",
                (true, false) => "EXPLAIN FORMAT=JSON",
                (false, true) => "EXPLAIN ANALYZE",
                (false, false) => "EXPLAIN FORMAT=TREE",
            };
            let conn = self.conn()?;
            // ANALYZE executes the statement: roll its changes back (transactional engines).
            if analyze {
                conn.query_drop("START TRANSACTION")
                    .await
                    .map_err(map_error)?;
            }
            let rows = catalog::text_rows(conn, &format!("{prefix} {statement}")).await;
            if analyze && let Err(e) = conn.query_drop("ROLLBACK").await {
                tracing::warn!("rollback after EXPLAIN ANALYZE failed: {e}");
            }
            let text: Vec<String> = rows?.iter().map(|r| catalog::col(r, 0)).collect();
            let text = text.join(
                "
",
            );
            let mut plan = if mariadb {
                rdbs_core::plan::parse_generic_json(&text)?
            } else {
                rdbs_core::plan::parse_mysql_tree(&text)?
            };
            plan.analyzed = analyze;
            Ok(plan)
        })
    }

    fn describe_schema<'a>(
        &'a mut self,
        schema: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<rdbs_core::TableSchema>, DbError>> {
        Box::pin(async move {
            let mariadb = self.version.contains("MariaDB");
            describe::describe(self.conn()?, schema, mariadb).await
        })
    }

    fn execute<'a>(
        &'a mut self,
        script: &'a str,
        cancel: CancelHandle,
    ) -> BoxFuture<'a, Result<QueryOutcome, DbError>> {
        Box::pin(async move {
            let id = self.conn()?.id();
            let opts = self.opts.clone();
            tokio::select! {
                result = self.run(script) => result,
                () = cancel.cancelled() => {
                    if let Err(e) = kill_query(opts, id).await {
                        tracing::warn!("KILL QUERY failed: {e}");
                    }
                    Err(DbError::cancelled())
                }
            }
        })
    }

    fn object_ddl<'a>(
        &'a mut self,
        object: &'a rdbs_core::schema::ObjectPath,
    ) -> BoxFuture<'a, Result<String, DbError>> {
        Box::pin(async move { ddl::object_ddl(self.conn()?, object).await })
    }

    fn routine_signature<'a>(
        &'a mut self,
        object: &'a rdbs_core::schema::ObjectPath,
    ) -> BoxFuture<'a, Result<rdbs_core::routine::RoutineSignature, DbError>> {
        Box::pin(async move { ddl::routine_signature(self.conn()?, object).await })
    }

    fn apply_changes<'a>(
        &'a mut self,
        changes: &'a [Change],
    ) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async move {
            let conn = self.conn()?;
            let mut tx = conn
                .start_transaction(mysql_async::TxOpts::default())
                .await
                .map_err(map_error)?;
            let mut total = 0;
            for (i, change) in changes.iter().enumerate() {
                let outcome = tx.query_drop(change.sql.as_str()).await.map_err(map_error);
                let n = tx.affected_rows();
                let failure = match outcome {
                    Err(e) => Some(e),
                    Ok(()) if !change.accepts(n) => Some(row_count_mismatch(i, change, n)),
                    Ok(()) => None,
                };
                if let Some(e) = failure {
                    if let Err(rb) = tx.rollback().await {
                        tracing::warn!("MySQL rollback failed: {rb}");
                    }
                    return Err(e);
                }
                total += n;
            }
            tx.commit().await.map_err(map_error)?;
            Ok(total)
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(conn) = self.conn.take()
                && let Err(e) = conn.disconnect().await
            {
                tracing::debug!("MySQL disconnect: {e}");
            }
        })
    }
}

/// `8.4.3` → `MySQL 8.4.3`; `11.4.2-MariaDB-ubu2404` → `MariaDB 11.4.2`.
fn describe_version(kind: DatabaseKind, raw: &str) -> String {
    if raw.contains("MariaDB") {
        let number = raw.split('-').next().unwrap_or(raw);
        format!("MariaDB {number}")
    } else if kind == DatabaseKind::MariaDb {
        format!("MariaDB {raw}")
    } else {
        format!("MySQL {raw}")
    }
}

fn connect_error(e: mysql_async::Error) -> DbError {
    let mut err = map_error(e);
    if err.kind == DbErrorKind::Query {
        err.kind = DbErrorKind::Connection;
    }
    err
}

/// Maps a driver error, keeping MySQL's errno and SQLSTATE.
pub(crate) fn map_error(e: mysql_async::Error) -> DbError {
    match e {
        mysql_async::Error::Server(s) => {
            let kind = match s.code {
                1044 | 1045 | 1698 => DbErrorKind::Authentication,
                1317 => DbErrorKind::Cancelled,
                // Server shutdown, killed connection, "gone away", lost during query,
                // disconnected for inactivity.
                1053 | 1927 | 2006 | 2013 | 4031 => DbErrorKind::Connection,
                _ => DbErrorKind::Query,
            };
            DbError::new(kind, s.message)
                .with_code(s.code.to_string())
                .with_detail(format!("SQLSTATE {}", s.state))
        }
        mysql_async::Error::Io(io) => DbError::new(DbErrorKind::Connection, io.to_string()),
        other => DbError::new(DbErrorKind::Internal, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_strings_are_readable() {
        assert_eq!(
            describe_version(DatabaseKind::MySql, "8.4.3"),
            "MySQL 8.4.3"
        );
        assert_eq!(
            describe_version(DatabaseKind::MySql, "11.4.2-MariaDB-ubu2404"),
            "MariaDB 11.4.2"
        );
    }

    #[test]
    fn server_errors_keep_codes() {
        let e = map_error(mysql_async::Error::Server(mysql_async::ServerError {
            code: 1045,
            message: "Access denied".into(),
            state: "28000".into(),
        }));
        assert_eq!(e.kind, DbErrorKind::Authentication);
        assert_eq!(e.code.as_deref(), Some("1045"));
    }

    #[test]
    fn gone_away_is_a_lost_connection() {
        let e = map_error(mysql_async::Error::Server(mysql_async::ServerError {
            code: 2006,
            message: "MySQL server has gone away".into(),
            state: "HY000".into(),
        }));
        assert_eq!(e.kind, DbErrorKind::Connection);
        assert!(e.is_connection_lost());
    }
}
