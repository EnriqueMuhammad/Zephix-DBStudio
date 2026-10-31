//! In-memory mock adapter.
//!
//! It behaves like a real engine from the UI's point of view — connection latency, object
//! tree, typed result sets with native type names, vendor error codes and cancellation —
//! without any network I/O. Milestone 1 runs entirely on it; later it backs UI tests.
//!
//! Special SQL understood by the mock:
//! - `... LIMIT n`, `SELECT TOP n ...`, `FETCH FIRST n ROWS` — number of generated rows
//!   (default 1 000, capped at 200 000);
//! - `FROM orders` returns order rows, anything else returns customer rows;
//! - text containing `sleep` or `waitfor` runs for ~10 s (to try cancellation);
//! - text containing `connection lost` fails like a server that closed an idle session
//!   (a query error with MySQL errno 2006, so the worker must notice and drop the session);
//! - text containing `error` fails with an engine-specific syntax error;
//! - a read of `sysjobhistory` returns a sample Agent job history;
//! - `sp_start_job`, `sp_stop_job` and `sp_update_job @enabled` change the sample Agent jobs
//!   so a refresh shows the new state;
//! - a host containing `fail` makes the connection attempt fail.

mod data;

use std::time::Duration;

use rdbs_core::sql::{self, StatementKind, TokenKind};
use rdbs_core::{
    AdapterFactory, BoxFuture, CancelHandle, Capabilities, ConnectionProfile, DatabaseAdapter,
    DatabaseKind, DbError, DbErrorKind, DbObject, Environment, ProfileId, QueryOutcome,
};

pub use data::{sample_objects, sample_tables};

/// Default and maximum number of rows a mock SELECT returns.
pub const DEFAULT_ROWS: usize = 1_000;
/// Upper bound for generated rows, to keep memory use reasonable.
pub const MAX_ROWS: usize = 200_000;

/// Creates [`MockAdapter`] sessions.
#[derive(Debug, Clone)]
pub struct MockAdapterFactory {
    connect_latency: Duration,
    query_latency: Duration,
}

impl MockAdapterFactory {
    /// Factory with realistic latencies (300 ms connect, 150 ms per query).
    pub fn new() -> Self {
        Self {
            connect_latency: Duration::from_millis(300),
            query_latency: Duration::from_millis(150),
        }
    }

    /// Factory without artificial latency, for tests.
    pub fn instant() -> Self {
        Self {
            connect_latency: Duration::ZERO,
            query_latency: Duration::ZERO,
        }
    }
}

impl Default for MockAdapterFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl AdapterFactory for MockAdapterFactory {
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        _password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn DatabaseAdapter>, DbError>> {
        Box::pin(async move {
            tokio::time::sleep(self.connect_latency).await;
            if profile.host.to_ascii_lowercase().contains("fail") {
                return Err(DbError::new(
                    DbErrorKind::Connection,
                    format!("could not connect to server at {}", profile.endpoint()),
                )
                .with_detail("Connection refused (mock)"));
            }
            let adapter: Box<dyn DatabaseAdapter> = Box::new(MockAdapter {
                kind: profile.kind,
                version: server_version(profile.kind).to_owned(),
                query_latency: self.query_latency,
                jobs: if profile.kind == DatabaseKind::SqlServer {
                    data::sample_agent_summaries()
                } else {
                    Vec::new()
                },
            });
            Ok(adapter)
        })
    }
}

/// A mock session for one engine.
#[derive(Debug)]
pub struct MockAdapter {
    kind: DatabaseKind,
    version: String,
    query_latency: Duration,
    /// SQL Server Agent jobs for this session. Start, stop and enable change them.
    jobs: Vec<rdbs_core::agent::AgentJobSummary>,
}

impl MockAdapter {
    /// Applies a start, stop or enable statement to the session's sample jobs.
    fn apply_agent_action(
        &mut self,
        job_name: &str,
        action: &rdbs_core::agent::AgentJobAction,
    ) -> Result<QueryOutcome, DbError> {
        use rdbs_core::agent::AgentJobAction;
        let Some(job) = self.jobs.iter_mut().find(|job| job.name == job_name) else {
            return Err(DbError::new(
                DbErrorKind::Query,
                format!("The specified @job_name ('{job_name}') does not exist."),
            )
            .with_code("14262"));
        };
        match action {
            AgentJobAction::Start { step_name } => {
                if !job.enabled {
                    return Err(DbError::new(
                        DbErrorKind::Query,
                        format!(
                            "Request to run job {job_name} refused because the job is disabled."
                        ),
                    )
                    .with_code("14222"));
                }
                if job.running {
                    return Err(DbError::new(
                        DbErrorKind::Query,
                        format!(
                            "Request to run job {job_name} refused because the job is already running."
                        ),
                    ));
                }
                job.running = true;
                job.current_step = step_name
                    .clone()
                    .filter(|step| !step.is_empty())
                    .or_else(|| job.steps.first().map(|step| step.name.clone()))
                    .unwrap_or_default();
            }
            AgentJobAction::Stop => {
                if !job.running {
                    return Err(DbError::new(
                        DbErrorKind::Query,
                        format!(
                            "Request to stop job {job_name} refused because the job is not currently running."
                        ),
                    ));
                }
                job.running = false;
                job.current_step.clear();
            }
            AgentJobAction::SetEnabled(enabled) => job.enabled = *enabled,
        }
        Ok(QueryOutcome {
            messages: vec![rdbs_core::agent::action_success(job_name, action)],
            ..QueryOutcome::default()
        })
    }
}

fn server_version(kind: DatabaseKind) -> &'static str {
    match kind {
        DatabaseKind::PostgreSql => "PostgreSQL 17.2 (mock)",
        DatabaseKind::SqlServer => "Microsoft SQL Server 2022 16.0 (mock)",
        DatabaseKind::MySql => "MySQL 8.4.3 (mock)",
        DatabaseKind::MariaDb => "MariaDB 11.4.2 (mock)",
        DatabaseKind::Sqlite => "SQLite 3.47.0 (mock)",
        DatabaseKind::Oracle => "Oracle Database 23ai Free (mock)",
        DatabaseKind::MongoDb => "MongoDB 8.0.4 (mock)",
        DatabaseKind::Redis => "Redis 7.4.2 (mock)",
    }
}

impl DatabaseAdapter for MockAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            query_cancellation: true,
            streaming_results: false,
            transactions: true,
            schemas: self.kind.has_schemas(),
            multiple_databases: self.kind != DatabaseKind::Sqlite,
            explain_plan: true,
        }
    }

    fn server_version(&self) -> &str {
        &self.version
    }

    fn apply_changes<'a>(
        &'a mut self,
        changes: &'a [rdbs_core::edit::Change],
    ) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async move {
            tokio::time::sleep(self.query_latency).await;
            Ok(changes
                .iter()
                .filter(|c| c.expected_rows != rdbs_core::edit::Change::ANY)
                .map(|c| c.expected_rows)
                .sum())
        })
    }

    fn load_objects(&mut self) -> BoxFuture<'_, Result<Vec<DbObject>, DbError>> {
        Box::pin(async move {
            tokio::time::sleep(self.query_latency / 2).await;
            let mut tree = sample_objects(self.kind);
            if self.kind == DatabaseKind::SqlServer {
                tree.retain(|object| object.name != rdbs_core::agent::AGENT_FOLDER);
                tree.push(rdbs_core::agent::jobs_node(&self.jobs, false));
            }
            Ok(tree)
        })
    }

    fn explain<'a>(
        &'a mut self,
        sql: &'a str,
        analyze: bool,
    ) -> BoxFuture<'a, Result<rdbs_core::plan::ExplainPlan, DbError>> {
        use rdbs_core::plan::{ExplainPlan, PlanNode};
        Box::pin(async move {
            tokio::time::sleep(self.query_latency).await;
            let table = if sql.to_ascii_lowercase().contains("orders") {
                "orders"
            } else {
                "customers"
            };
            let actual = |rows: f64, ms: f64| analyze.then_some((rows, ms));
            let node =
                |op: &str, object: Option<&str>, cost: f64, rows: f64, act: Option<(f64, f64)>| {
                    PlanNode {
                        operation: op.into(),
                        object: object.map(str::to_owned),
                        cost: Some(cost),
                        rows: Some(rows),
                        actual_rows: act.map(|a| a.0),
                        actual_ms: act.map(|a| a.1),
                        ..PlanNode::default()
                    }
                };
            let mut root = node("Limit", None, 48.2, 100.0, actual(100.0, 3.1));
            let mut sort = node("Sort", None, 47.9, 1000.0, actual(1000.0, 2.9));
            sort.details.push(("Sort Key".into(), "id".into()));
            let mut scan = node("Seq Scan", Some(table), 22.0, 1000.0, actual(1000.0, 1.2));
            scan.details
                .push(("Filter".into(), "(is_active = true)".into()));
            sort.children.push(scan);
            root.children.push(sort);
            Ok(ExplainPlan {
                raw: format!("mock plan for: {sql}"),
                roots: vec![root],
                analyzed: analyze,
                notes: if analyze {
                    vec!["Execution time: 3.200 ms (mock)".into()]
                } else {
                    Vec::new()
                },
            })
        })
    }

    fn object_ddl<'a>(
        &'a mut self,
        object: &'a rdbs_core::schema::ObjectPath,
    ) -> BoxFuture<'a, Result<String, DbError>> {
        Box::pin(async move {
            tokio::time::sleep(self.query_latency / 2).await;
            if object.kind == rdbs_core::ObjectKind::AgentJob {
                return Ok(rdbs_core::agent::create_script(&data::sample_agent_job(
                    &object.name,
                )));
            }
            if object.kind == rdbs_core::ObjectKind::Table {
                let table = sample_tables(self.kind, object.schema.as_deref())
                    .into_iter()
                    .find(|t| t.name == object.name);
                if let Some(t) = table {
                    return Ok(rdbs_core::ddlgen::script(&rdbs_core::ddlgen::create_table(
                        self.kind, &t,
                    )));
                }
            }
            data::sample_routine_ddl(self.kind, object).ok_or_else(|| {
                DbError::new(DbErrorKind::Query, format!("{} was not found", object.name))
            })
        })
    }

    fn routine_signature<'a>(
        &'a mut self,
        object: &'a rdbs_core::schema::ObjectPath,
    ) -> BoxFuture<'a, Result<rdbs_core::routine::RoutineSignature, DbError>> {
        Box::pin(async move {
            tokio::time::sleep(self.query_latency / 2).await;
            data::sample_routine(&object.name).ok_or_else(|| {
                DbError::new(DbErrorKind::Query, format!("{} was not found", object.name))
            })
        })
    }

    fn describe_schema<'a>(
        &'a mut self,
        schema: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<rdbs_core::TableSchema>, DbError>> {
        Box::pin(async move {
            tokio::time::sleep(self.query_latency / 2).await;
            Ok(sample_tables(self.kind, schema))
        })
    }

    fn execute<'a>(
        &'a mut self,
        sql_text: &'a str,
        cancel: CancelHandle,
    ) -> BoxFuture<'a, Result<QueryOutcome, DbError>> {
        Box::pin(async move {
            if let Some((job_name, action)) = rdbs_core::agent::parse_action_sql(sql_text) {
                return self.apply_agent_action(&job_name, &action);
            }

            let lower = sql_text.to_lowercase();
            let latency = if lower.contains("sleep") || lower.contains("waitfor") {
                Duration::from_secs(10)
            } else {
                self.query_latency
            };
            wait_cancellable(latency, &cancel).await?;

            if lower.contains("connection lost") {
                return Err(
                    DbError::new(DbErrorKind::Query, "MySQL server has gone away")
                        .with_code("2006"),
                );
            }
            if lower.contains("error") {
                return Err(syntax_error(self.kind));
            }

            match sql::classify(sql_text) {
                StatementKind::Empty => Ok(QueryOutcome {
                    messages: vec!["Nothing to execute".to_owned()],
                    ..QueryOutcome::default()
                }),
                StatementKind::Read => {
                    if lower.contains("sysjobhistory") {
                        let mut outcome = QueryOutcome::default();
                        outcome.push_result(data::sample_job_history());
                        return Ok(outcome);
                    }
                    // Paged reads (`OFFSET n`) see a table of DEFAULT_ROWS rows.
                    let limit = requested_rows(sql_text)
                        .unwrap_or(DEFAULT_ROWS)
                        .min(MAX_ROWS);
                    let rows = match requested_offset(sql_text) {
                        Some(offset) => limit.min(DEFAULT_ROWS.saturating_sub(offset)),
                        None => limit,
                    };
                    let dataset = if lower.contains("orders") {
                        data::Dataset::Orders
                    } else {
                        data::Dataset::Customers
                    };
                    let result = data::generate(self.kind, dataset, rows);
                    if cancel.is_cancelled() {
                        return Err(DbError::cancelled());
                    }
                    let mut outcome = QueryOutcome::default();
                    outcome.push_result(result);
                    Ok(outcome)
                }
                StatementKind::Write => {
                    let mut outcome = QueryOutcome::default();
                    outcome.record_rows_affected(3);
                    Ok(outcome)
                }
                StatementKind::Ddl | StatementKind::Transaction | StatementKind::Other => {
                    Ok(QueryOutcome {
                        messages: vec!["Command completed successfully (mock)".to_owned()],
                        ..QueryOutcome::default()
                    })
                }
            }
        })
    }
}

/// Sleeps for `total`, checking `cancel` every 25 ms.
async fn wait_cancellable(total: Duration, cancel: &CancelHandle) -> Result<(), DbError> {
    const STEP: Duration = Duration::from_millis(25);
    let mut waited = Duration::ZERO;
    while waited < total {
        if cancel.is_cancelled() {
            return Err(DbError::cancelled());
        }
        let step = STEP.min(total - waited);
        tokio::time::sleep(step).await;
        waited += step;
    }
    if cancel.is_cancelled() {
        return Err(DbError::cancelled());
    }
    Ok(())
}

fn syntax_error(kind: DatabaseKind) -> DbError {
    let (code, message) = match kind {
        DatabaseKind::PostgreSql => ("42601", "syntax error at or near \"error\""),
        DatabaseKind::SqlServer => ("102", "Incorrect syntax near 'error'."),
        DatabaseKind::MySql | DatabaseKind::MariaDb => (
            "1064",
            "You have an error in your SQL syntax near 'error' at line 1",
        ),
        DatabaseKind::Sqlite => ("1", "near \"error\": syntax error"),
        DatabaseKind::Oracle => ("ORA-00900", "invalid SQL statement"),
        DatabaseKind::MongoDb => ("9", "FailedToParse: unexpected token near 'error'"),
        DatabaseKind::Redis => ("ERR", "unknown command 'error'"),
    };
    DbError::new(DbErrorKind::Query, message).with_code(code)
}

/// Extracts the row count from `LIMIT n`, `TOP n` / `TOP (n)` or `FETCH FIRST|NEXT n`.
pub fn requested_rows(sql_text: &str) -> Option<usize> {
    let words: Vec<&str> = sql::tokenize(sql_text)
        .into_iter()
        .filter(|t| {
            matches!(
                t.kind,
                TokenKind::Keyword | TokenKind::Identifier | TokenKind::Number
            )
        })
        .map(|t| &sql_text[t.range])
        .collect();
    words.windows(2).rev().find_map(|w| {
        let is_count_kw = ["limit", "top", "first", "next"]
            .iter()
            .any(|k| w[0].eq_ignore_ascii_case(k));
        if is_count_kw { w[1].parse().ok() } else { None }
    })
}

/// Extracts `n` from `OFFSET n`.
pub fn requested_offset(sql_text: &str) -> Option<usize> {
    let lower = sql_text.to_ascii_lowercase();
    let rest = &lower[lower.rfind("offset ")? + 7..];
    rest.split_whitespace().next()?.parse().ok()
}

/// Demo connection profiles shown on first start.
pub fn sample_profiles() -> Vec<ConnectionProfile> {
    // Fixed ids, so demo tabs restored from the last run still find their connection.
    const DEMO_ID_BASE: u128 = 0x7a65_7068_6978_0000_0000_0000_0000_0000;
    let mut index = 0u128;
    let mut mk = |name: &str, kind: DatabaseKind, host: &str, db: &str, env: Environment| {
        index += 1;
        let mut p = ConnectionProfile::new(kind);
        p.id = ProfileId::from_u128(DEMO_ID_BASE + index);
        p.name = name.to_owned();
        p.host = host.to_owned();
        p.database = db.to_owned();
        p.environment = env;
        p
    };
    vec![
        mk(
            "Local PostgreSQL",
            DatabaseKind::PostgreSql,
            "localhost",
            "app_dev",
            Environment::Development,
        ),
        mk(
            "Analytics MySQL",
            DatabaseKind::MySql,
            "mysql.staging.internal",
            "shop",
            Environment::Staging,
        ),
        mk(
            "Sales SQL Server",
            DatabaseKind::SqlServer,
            "sql-prod-01",
            "SalesDB",
            Environment::Production,
        ),
        mk(
            "Inventory MariaDB",
            DatabaseKind::MariaDb,
            "127.0.0.1",
            "inventory",
            Environment::Development,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdbs_core::CellValue;

    async fn session(kind: DatabaseKind) -> Box<dyn DatabaseAdapter> {
        let mut p = ConnectionProfile::new(kind);
        p.name = "t".into();
        MockAdapterFactory::instant()
            .connect(&p, None)
            .await
            .unwrap()
    }

    #[test]
    fn parses_offsets() {
        assert_eq!(
            requested_offset("select a from t limit 5 offset 10"),
            Some(10)
        );
        assert_eq!(
            requested_offset("select a from t order by a offset 7 rows fetch next 5 rows only"),
            Some(7)
        );
        assert_eq!(requested_offset("select 1"), None);
    }

    #[test]
    fn parses_row_limits() {
        assert_eq!(requested_rows("select * from t limit 50"), Some(50));
        assert_eq!(requested_rows("SELECT TOP 10 * FROM t"), Some(10));
        assert_eq!(requested_rows("SELECT TOP (25) * FROM t"), Some(25));
        assert_eq!(
            requested_rows("select * from t fetch first 7 rows only"),
            Some(7)
        );
        assert_eq!(requested_rows("select * from t"), None);
    }

    #[tokio::test]
    async fn agent_actions_change_the_sample_jobs() {
        use rdbs_core::agent::{AgentJobAction, action_sql};
        let mut s = session(DatabaseKind::SqlServer).await;
        let nightly = "Nightly index maintenance";
        let start = action_sql(nightly, &AgentJobAction::Start { step_name: None });
        let err = s.execute(&start, CancelHandle::new()).await.unwrap_err();
        assert!(
            err.message.to_lowercase().contains("already running"),
            "{err}"
        );

        s.execute(
            &action_sql(nightly, &AgentJobAction::Stop),
            CancelHandle::new(),
        )
        .await
        .unwrap();
        s.execute(
            &action_sql(nightly, &AgentJobAction::SetEnabled(false)),
            CancelHandle::new(),
        )
        .await
        .unwrap();
        let err = s.execute(&start, CancelHandle::new()).await.unwrap_err();
        assert!(err.message.to_lowercase().contains("disabled"), "{err}");
        s.execute(
            &action_sql(nightly, &AgentJobAction::SetEnabled(true)),
            CancelHandle::new(),
        )
        .await
        .unwrap();
        s.execute(
            &action_sql(
                nightly,
                &AgentJobAction::Start {
                    step_name: Some("Update statistics".into()),
                },
            ),
            CancelHandle::new(),
        )
        .await
        .unwrap();
        let tree = s.load_objects().await.unwrap();
        let detail = tree
            .iter()
            .find(|o| o.name == rdbs_core::agent::AGENT_FOLDER)
            .and_then(|folder| folder.children.iter().find(|c| c.name == "Jobs"))
            .and_then(|jobs| jobs.children.iter().find(|j| j.name == nightly))
            .and_then(|job| job.detail.clone())
            .unwrap_or_default();
        assert!(detail.starts_with("running"), "{detail}");
        assert!(detail.contains("step Update statistics"), "{detail}");
    }

    #[tokio::test]
    async fn select_returns_typed_rows() {
        let mut s = session(DatabaseKind::PostgreSql).await;
        let out = s
            .execute("select * from customers limit 20", CancelHandle::new())
            .await
            .unwrap();
        let rs = out.last_result().unwrap();
        assert_eq!(rs.rows.len(), 20);
        assert_eq!(rs.columns[0].type_name, "int4");
        assert_eq!(rs.cell(0, 0), Some(&CellValue::Int(1)));
        assert!(rs.rows.iter().all(|r| r.len() == rs.columns.len()));
    }

    #[tokio::test]
    async fn native_type_names_follow_engine() {
        let mut s = session(DatabaseKind::SqlServer).await;
        let rs = s
            .execute("select top 1 * from customers", CancelHandle::new())
            .await
            .unwrap()
            .results
            .remove(0);
        assert_eq!(rs.columns[0].type_name, "int");
        assert_eq!(rs.columns[1].type_name, "nvarchar(100)");
    }

    #[tokio::test]
    async fn errors_carry_vendor_code() {
        let mut s = session(DatabaseKind::MySql).await;
        let err = s
            .execute("select error", CancelHandle::new())
            .await
            .unwrap_err();
        assert_eq!(err.kind, DbErrorKind::Query);
        assert_eq!(err.code.as_deref(), Some("1064"));
    }

    #[tokio::test]
    async fn cancellation_stops_long_query() {
        let mut s = session(DatabaseKind::PostgreSql).await;
        let cancel = CancelHandle::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c2.cancel();
        });
        let started = std::time::Instant::now();
        let err = s.execute("select pg_sleep(10)", cancel).await.unwrap_err();
        assert_eq!(err.kind, DbErrorKind::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn failing_host_is_reported() {
        let mut p = ConnectionProfile::new(DatabaseKind::PostgreSql);
        p.host = "fail.example".into();
        let err = MockAdapterFactory::instant()
            .connect(&p, None)
            .await
            .err()
            .unwrap();
        assert_eq!(err.kind, DbErrorKind::Connection);
    }

    #[test]
    fn sample_profiles_are_valid() {
        for p in sample_profiles() {
            assert!(p.validate().is_ok(), "{}", p.name);
        }
    }

    #[test]
    fn sample_profile_ids_are_stable_and_unique() {
        let first: Vec<_> = sample_profiles().into_iter().map(|p| p.id).collect();
        let second: Vec<_> = sample_profiles().into_iter().map(|p| p.id).collect();
        assert_eq!(first, second);
        let unique: std::collections::HashSet<_> = first.iter().collect();
        assert_eq!(unique.len(), first.len());
    }
}
