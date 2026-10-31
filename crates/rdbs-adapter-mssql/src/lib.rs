//! Microsoft SQL Server adapter built on `tiberius` (ADR 0004).
//!
//! Scripts are split into batches on `GO` lines. Each batch runs as a plain SQL batch; its
//! result sets and the row count of every statement (the DONE tokens, exposed by the
//! vendored tiberius patch, ADR 0012) are read from the TDS stream.
//!
//! TDS has an "attention" signal, but tiberius does not expose it. Cancelling therefore drops
//! the connection, which makes the server abort the request; the next execution reconnects
//! transparently. Session state (temporary tables, open transactions, `SET` options) is lost,
//! and the UI is told so.

mod agent;
mod batch;
mod catalog;
mod ddl;
mod describe;
mod folders;
mod showplan;
mod values;

use std::time::Duration;

use futures_util::TryStreamExt;
use rdbs_core::adapter::row_count_mismatch;
use rdbs_core::edit::Change;
use rdbs_core::{
    AdapterFactory, BoxFuture, CancelHandle, Capabilities, ColumnMeta, ConnectionProfile,
    DatabaseAdapter, DbError, DbErrorKind, DbObject, MAX_RESULT_ROWS, QueryOutcome, ResultSet,
    SslMode, rows_affected_message,
};
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, QueryItem};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) type MssqlClient = Client<Compat<TcpStream>>;

/// Opens SQL Server sessions.
#[derive(Debug, Default, Clone, Copy)]
pub struct MssqlFactory;

impl AdapterFactory for MssqlFactory {
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn DatabaseAdapter>, DbError>> {
        Box::pin(async move {
            let adapter: Box<dyn DatabaseAdapter> =
                Box::new(MssqlAdapter::connect(profile, password).await?);
            Ok(adapter)
        })
    }
}

/// An open SQL Server session. `client` is `None` after a cancellation until the next use.
pub struct MssqlAdapter {
    client: Option<MssqlClient>,
    config: Config,
    version: String,
}

fn build_config(profile: &ConnectionProfile, password: Option<&str>) -> Config {
    let mut config = Config::new();
    config.host(&profile.host);
    config.port(profile.port);
    if !profile.database.is_empty() {
        config.database(&profile.database);
    }
    config.application_name(rdbs_core::brand::APP_NAME);
    config.authentication(AuthMethod::sql_server(
        &profile.username,
        password.unwrap_or_default(),
    ));
    match profile.ssl_mode {
        // Login packets are always encrypted by SQL Server; `Off` encrypts only the login.
        SslMode::Disable => {
            config.encryption(EncryptionLevel::Off);
            config.trust_cert();
        }
        SslMode::Prefer => {
            config.encryption(EncryptionLevel::On);
            config.trust_cert();
        }
        SslMode::Require => {
            config.encryption(EncryptionLevel::Required);
            config.trust_cert();
        }
        SslMode::VerifyFull => config.encryption(EncryptionLevel::Required),
    }
    config
}

/// Opens a TCP connection and performs the TDS login, following one Azure-style redirect.
async fn open(mut config: Config) -> Result<MssqlClient, DbError> {
    for _ in 0..2 {
        let addr = config.get_addr();
        let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| {
                DbError::new(
                    DbErrorKind::Timeout,
                    format!(
                        "connection to {addr} timed out after {}s",
                        CONNECT_TIMEOUT.as_secs()
                    ),
                )
            })?
            .map_err(|e| DbError::new(DbErrorKind::Connection, format!("{addr}: {e}")))?;
        tcp.set_nodelay(true)
            .map_err(|e| DbError::new(DbErrorKind::Connection, e.to_string()))?;
        match Client::connect(config.clone(), tcp.compat_write()).await {
            Ok(client) => return Ok(client),
            Err(tiberius::error::Error::Routing { host, port }) => {
                config.host(&host);
                config.port(port);
            }
            Err(e) => return Err(connect_error(e)),
        }
    }
    Err(DbError::new(
        DbErrorKind::Connection,
        "too many server redirects",
    ))
}

impl MssqlAdapter {
    /// Connects with SQL Server authentication.
    pub async fn connect(
        profile: &ConnectionProfile,
        password: Option<&str>,
    ) -> Result<Self, DbError> {
        let config = build_config(profile, password);
        let mut client = open(config.clone()).await?;
        let version = catalog::text_rows(
            &mut client,
            "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(64)), \
                    CAST(SERVERPROPERTY('Edition') AS nvarchar(128))",
        )
        .await?
        .first()
        .map(|r| {
            let number = r.first().cloned().flatten().unwrap_or_default();
            let edition = r.get(1).cloned().flatten().unwrap_or_default();
            describe_version(&number, &edition)
        })
        .unwrap_or_else(|| "SQL Server".to_owned());
        Ok(Self {
            client: Some(client),
            config,
            version,
        })
    }

    async fn client(&mut self) -> Result<&mut MssqlClient, DbError> {
        if self.client.is_none() {
            self.client = Some(open(self.config.clone()).await?);
        }
        self.client
            .as_mut()
            .ok_or_else(|| DbError::new(DbErrorKind::Internal, "connection unavailable"))
    }

    async fn run(&mut self, script: &str) -> Result<QueryOutcome, DbError> {
        let batches = batch::split_batches(script);
        let client = self.client().await?;
        let mut outcome = QueryOutcome::default();
        for b in &batches {
            let mut stream = client.simple_query(*b).await.map_err(map_error)?;
            let mut current: Option<ResultSet> = None;
            loop {
                let item = stream.try_next().await.map_err(map_error)?;
                // Statements completed before this item, in order (the vendored tiberius
                // patch). A count taken while a result set is open is that set's row count.
                for count in stream.take_done_counts() {
                    match current.take() {
                        Some(set) => {
                            outcome.messages.push(rows_affected_message(count));
                            outcome.results.push(set);
                        }
                        None => outcome.record_rows_affected(count),
                    }
                }
                let Some(item) = item else { break };
                match item {
                    QueryItem::Metadata(meta) => {
                        if let Some(done) = current.take() {
                            outcome.results.push(done);
                        }
                        current = Some(ResultSet {
                            columns: meta
                                .columns()
                                .iter()
                                .map(|c| {
                                    ColumnMeta::new(c.name(), values::type_name(c.column_type()))
                                })
                                .collect(),
                            rows: Vec::new(),
                        });
                    }
                    QueryItem::Row(row) => {
                        if let Some(set) = current.as_mut() {
                            if set.rows.len() < MAX_RESULT_ROWS {
                                set.rows
                                    .push(row.into_iter().map(|d| values::cell(&d)).collect());
                            } else {
                                outcome.truncated = true;
                            }
                        }
                    }
                }
            }
            if let Some(done) = current {
                outcome.push_result(done);
            }
        }
        if outcome.truncated {
            outcome.messages.push(format!(
                "Result truncated to the first {MAX_RESULT_ROWS} rows"
            ));
        }
        Ok(outcome)
    }
}

impl DatabaseAdapter for MssqlAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // Implemented by resetting the connection (see module docs).
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
        Box::pin(async move { catalog::load(self.client().await?).await })
    }

    fn explain<'a>(
        &'a mut self,
        sql: &'a str,
        analyze: bool,
    ) -> BoxFuture<'a, Result<rdbs_core::plan::ExplainPlan, DbError>> {
        Box::pin(async move {
            let client = self.client().await?;
            let (on, off) = if analyze {
                ("SET STATISTICS XML ON", "SET STATISTICS XML OFF")
            } else {
                ("SET SHOWPLAN_XML ON", "SET SHOWPLAN_XML OFF")
            };
            // An actual plan executes the statement: roll its changes back.
            if analyze {
                catalog::text_rows(client, "BEGIN TRANSACTION").await?;
            }
            catalog::text_rows(client, on).await?;
            let rows = catalog::text_rows(client, sql).await;
            let reset = catalog::text_rows(client, off).await;
            if analyze {
                catalog::text_rows(client, "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await?;
            }
            reset?;
            let plans: Vec<String> = rows?
                .iter()
                .map(|r| catalog::col(r, 0))
                .filter(|text| text.starts_with("<ShowPlanXML"))
                .collect();
            showplan::parse(&plans, analyze)
        })
    }

    fn describe_schema<'a>(
        &'a mut self,
        schema: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<rdbs_core::TableSchema>, DbError>> {
        Box::pin(async move { describe::describe(self.client().await?, schema).await })
    }

    fn execute<'a>(
        &'a mut self,
        script: &'a str,
        cancel: CancelHandle,
    ) -> BoxFuture<'a, Result<QueryOutcome, DbError>> {
        Box::pin(async move {
            let finished = tokio::select! {
                result = self.run(script) => Some(result),
                () = cancel.cancelled() => None,
            };
            match finished {
                Some(result) => result,
                None => {
                    // Dropping the client closes the socket; SQL Server aborts the request.
                    self.client = None;
                    Err(DbError::cancelled().with_detail(
                        "The connection was reset to stop the query; temporary tables, \
                         open transactions and SET options of this session were discarded.",
                    ))
                }
            }
        })
    }

    fn object_ddl<'a>(
        &'a mut self,
        object: &'a rdbs_core::schema::ObjectPath,
    ) -> BoxFuture<'a, Result<String, DbError>> {
        Box::pin(async move { ddl::object_ddl(self.client().await?, object).await })
    }

    fn routine_signature<'a>(
        &'a mut self,
        object: &'a rdbs_core::schema::ObjectPath,
    ) -> BoxFuture<'a, Result<rdbs_core::routine::RoutineSignature, DbError>> {
        Box::pin(async move { ddl::routine_signature(self.client().await?, object).await })
    }

    fn apply_changes<'a>(
        &'a mut self,
        changes: &'a [Change],
    ) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async move {
            let client = self.client().await?;
            // BEGIN/COMMIT must run as plain batches: inside `sp_executesql` they would
            // unbalance the transaction count.
            run_batch(client, "SET XACT_ABORT ON; BEGIN TRANSACTION").await?;
            let mut total = 0;
            for (i, change) in changes.iter().enumerate() {
                let failure = match client.execute(change.sql.as_str(), &[]).await {
                    Err(e) => Some(map_error(e)),
                    Ok(result) => {
                        let n = result.total();
                        total += n;
                        (!change.accepts(n)).then(|| row_count_mismatch(i, change, n))
                    }
                };
                if let Some(e) = failure {
                    if let Err(rb) =
                        run_batch(client, "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await
                    {
                        tracing::warn!("SQL Server rollback failed: {rb}");
                    }
                    return Err(e);
                }
            }
            run_batch(client, "COMMIT TRANSACTION").await?;
            Ok(total)
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(client) = self.client.take()
                && let Err(e) = client.close().await
            {
                tracing::debug!("SQL Server close: {e}");
            }
        })
    }
}

/// Runs a batch and drains its results.
async fn run_batch(client: &mut MssqlClient, sql: &str) -> Result<(), DbError> {
    client
        .simple_query(sql)
        .await
        .map_err(map_error)?
        .into_results()
        .await
        .map_err(map_error)?;
    Ok(())
}

/// `16.0.4135.4` + `Developer Edition (64-bit)` → `SQL Server 2022 (16.0.4135.4) Developer`.
fn describe_version(number: &str, edition: &str) -> String {
    let release = match number.split('.').next() {
        Some("17") => "2025",
        Some("16") => "2022",
        Some("15") => "2019",
        Some("14") => "2017",
        Some("13") => "2016",
        Some("12") => "2014",
        _ => "",
    };
    let edition = edition.split(" Edition").next().unwrap_or(edition).trim();
    let mut out = String::from("SQL Server");
    if !release.is_empty() {
        out.push(' ');
        out.push_str(release);
    }
    out.push_str(&format!(" ({number})"));
    if !edition.is_empty() {
        out.push(' ');
        out.push_str(edition);
    }
    out
}

fn connect_error(e: tiberius::error::Error) -> DbError {
    let mut err = map_error(e);
    if err.kind == DbErrorKind::Query {
        err.kind = DbErrorKind::Connection;
    }
    err
}

/// Maps a driver error, keeping the SQL Server error number, state and line.
pub(crate) fn map_error(e: tiberius::error::Error) -> DbError {
    use tiberius::error::Error;
    match e {
        Error::Server(t) => {
            let kind = match t.code() {
                18456 | 18452 | 4060 => DbErrorKind::Authentication,
                // Connection closed, network name gone, transport failures, Azure disconnects.
                64 | 233 | 596 | 10053 | 10054 | 10060 | 40197 | 40501 | 40613 => {
                    DbErrorKind::Connection
                }
                _ => DbErrorKind::Query,
            };
            DbError::new(kind, t.message())
                .with_code(t.code().to_string())
                .with_detail(format!(
                    "state {}, severity {}, line {}",
                    t.state(),
                    t.class(),
                    t.line()
                ))
        }
        Error::Io { kind, message } => {
            DbError::new(DbErrorKind::Connection, format!("{kind:?}: {message}"))
        }
        Error::Tls(message) => DbError::new(DbErrorKind::Connection, format!("TLS: {message}")),
        other => DbError::new(DbErrorKind::Internal, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_versions() {
        assert_eq!(
            describe_version("16.0.4135.4", "Developer Edition (64-bit)"),
            "SQL Server 2022 (16.0.4135.4) Developer"
        );
        assert_eq!(describe_version("99.0", ""), "SQL Server (99.0)");
    }
}
