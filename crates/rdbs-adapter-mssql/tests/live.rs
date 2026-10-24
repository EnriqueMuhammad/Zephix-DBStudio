//! Integration tests against a real SQL Server (see `dev/docker-compose.yml`).
//!
//! Skipped unless `RDBS_IT=1`. Settings: `RDBS_MS_HOST`, `RDBS_MS_PORT`, `RDBS_MS_USER`,
//! `RDBS_MS_PASSWORD` (defaults match the compose file). Tests run in `tempdb`/`master`
//! so no database needs to be created first.

// Test helpers panic on purpose: a failing setup should fail the test loudly.
#![allow(clippy::expect_used)]

use std::time::{Duration, Instant};

use rdbs_adapter_mssql::MssqlFactory;
use rdbs_core::edit::Change;
use rdbs_core::{
    AdapterFactory, CancelHandle, CellValue, ConnectionProfile, DatabaseAdapter, DatabaseKind,
    DbErrorKind, ObjectKind, SslMode,
};

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn target() -> Option<(ConnectionProfile, String)> {
    if std::env::var("RDBS_IT").as_deref() != Ok("1") {
        eprintln!("skipped: set RDBS_IT=1 to run SQL Server integration tests");
        return None;
    }
    let mut p = ConnectionProfile::new(DatabaseKind::SqlServer);
    p.name = "it-mssql".into();
    p.host = env("RDBS_MS_HOST", "127.0.0.1");
    p.port = env("RDBS_MS_PORT", "51433").parse().unwrap_or(51433);
    p.username = env("RDBS_MS_USER", "sa");
    p.database = "master".into();
    // The container uses a self-signed certificate.
    p.ssl_mode = SslMode::Require;
    Some((p, env("RDBS_MS_PASSWORD", "Rdbs_Test_Pw_2026!")))
}

async fn session() -> Option<Box<dyn DatabaseAdapter>> {
    let (profile, password) = target()?;
    Some(
        MssqlFactory
            .connect(&profile, Some(&password))
            .await
            .expect("connect to test server"),
    )
}

#[tokio::test]
async fn reports_version_and_types() {
    let Some(mut s) = session().await else { return };
    assert!(
        s.server_version().starts_with("SQL Server"),
        "{}",
        s.server_version()
    );
    let out = s
        .execute(
            "SELECT CAST(42 AS int) AS i, CAST(12.30 AS decimal(10,2)) AS n, \
                    CAST(1 AS bit) AS b, N'héllo' AS t, CAST(NULL AS int) AS nul, \
                    CAST('2026-10-16' AS date) AS d, 0xDEAD AS raw",
            CancelHandle::new(),
        )
        .await
        .expect("select");
    let rs = out.last_result().expect("result set");
    assert_eq!(rs.columns[0].type_name, "int");
    assert_eq!(
        rs.rows[0],
        vec![
            CellValue::Int(42),
            CellValue::Decimal("12.30".into()),
            CellValue::Bool(true),
            CellValue::Text("héllo".into()),
            CellValue::Null,
            CellValue::Temporal("2026-10-16".into()),
            CellValue::Bytes(vec![0xde, 0xad]),
        ]
    );
}

#[tokio::test]
async fn batches_and_row_counts() {
    let Some(mut s) = session().await else { return };
    let out = s
        .execute(
            "CREATE TABLE #it_t (id int PRIMARY KEY, name nvarchar(10));\n\
             INSERT INTO #it_t VALUES (1, N'a'), (2, N'b'), (3, N'c');\n\
             GO\n\
             SELECT * FROM #it_t ORDER BY id;",
            CancelHandle::new(),
        )
        .await
        .expect("script");
    assert_eq!(out.last_result().expect("result set").rows.len(), 3);

    let out = s
        .execute(
            "UPDATE #it_t SET name = N'z' WHERE id > 1",
            CancelHandle::new(),
        )
        .await
        .expect("update");
    assert_eq!(out.rows_affected, Some(2));
}

#[tokio::test]
async fn returns_every_result_set() {
    let Some(mut s) = session().await else { return };
    let out = s
        .execute(
            "SELECT 1 AS a; SELECT N'x' AS b, 2 AS c",
            CancelHandle::new(),
        )
        .await
        .expect("script");
    assert_eq!(out.results.len(), 2);
    assert_eq!(out.results[0].columns[0].name, "a");
    assert_eq!(out.results[1].columns.len(), 2);
}

#[tokio::test]
async fn produces_ddl_from_catalog() {
    use rdbs_core::schema::ObjectPath;
    let Some(mut s) = session().await else { return };
    s.execute(
        "IF OBJECT_ID('dbo.it_ddl_child') IS NOT NULL DROP TABLE dbo.it_ddl_child;          IF OBJECT_ID('dbo.it_ddl_v') IS NOT NULL DROP VIEW dbo.it_ddl_v;          IF OBJECT_ID('dbo.it_ddl') IS NOT NULL DROP TABLE dbo.it_ddl;          CREATE TABLE dbo.it_ddl (id int IDENTITY(1,1) NOT NULL CONSTRAINT pk_it_ddl PRIMARY KEY,              code nvarchar(10) NOT NULL CONSTRAINT uq_it_ddl UNIQUE, qty int NULL DEFAULT (1));          CREATE INDEX ix_it_ddl_qty ON dbo.it_ddl (qty);          CREATE TABLE dbo.it_ddl_child (id int PRIMARY KEY, parent int REFERENCES dbo.it_ddl(id));",
        CancelHandle::new(),
    )
    .await
    .expect("setup");
    s.execute(
        "CREATE VIEW dbo.it_ddl_v AS SELECT id FROM dbo.it_ddl",
        CancelHandle::new(),
    )
    .await
    .expect("view");
    let path = |name: &str, kind| ObjectPath {
        database: Some("master".into()),
        schema: Some("dbo".into()),
        name: name.into(),
        kind,
    };
    let table = s
        .object_ddl(&path("it_ddl", ObjectKind::Table))
        .await
        .expect("table ddl");
    for part in [
        "CREATE TABLE [dbo].[it_ddl] (",
        "[id] int IDENTITY(1,1) NOT NULL",
        "[code] nvarchar(10) NOT NULL",
        "DEFAULT ((1))",
        "CONSTRAINT [pk_it_ddl] PRIMARY KEY CLUSTERED ([id])",
        "CONSTRAINT [uq_it_ddl] UNIQUE NONCLUSTERED ([code])",
        "CREATE NONCLUSTERED INDEX [ix_it_ddl_qty]",
    ] {
        assert!(
            table.contains(part),
            "missing {part} in
{table}"
        );
    }
    let child = s
        .object_ddl(&path("it_ddl_child", ObjectKind::Table))
        .await
        .expect("child ddl");
    assert!(
        child.contains("FOREIGN KEY ([parent]) REFERENCES [dbo].[it_ddl] ([id])"),
        "{child}"
    );
    let view = s
        .object_ddl(&path("it_ddl_v", ObjectKind::View))
        .await
        .expect("view ddl");
    assert!(view.starts_with("CREATE VIEW dbo.it_ddl_v"), "{view}");
}

#[tokio::test]
async fn errors_carry_error_number() {
    let Some(mut s) = session().await else { return };
    let err = s
        .execute("SELECT 1 +", CancelHandle::new())
        .await
        .expect_err("syntax error");
    assert_eq!(err.kind, DbErrorKind::Query);
    assert_eq!(err.code.as_deref(), Some("102"));
    assert!(s.execute("SELECT 1", CancelHandle::new()).await.is_ok());
}

#[tokio::test]
async fn cancellation_resets_and_reconnects() {
    let Some(mut s) = session().await else { return };
    let cancel = CancelHandle::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    let err = s
        .execute("WAITFOR DELAY '00:00:30'", cancel)
        .await
        .expect_err("cancelled");
    assert_eq!(err.kind, DbErrorKind::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(5));
    let after = s
        .execute("SELECT 1 AS ok", CancelHandle::new())
        .await
        .expect("reconnects after cancel");
    assert_eq!(
        after.last_result().expect("rows").rows[0][0],
        CellValue::Int(1)
    );
}

#[tokio::test]
async fn bad_password_is_an_authentication_error() {
    let Some((profile, _)) = target() else { return };
    let err = MssqlFactory
        .connect(&profile, Some("definitely-wrong"))
        .await
        .err()
        .expect("auth failure");
    assert_eq!(err.kind, DbErrorKind::Authentication, "{err}");
    assert_eq!(err.code.as_deref(), Some("18456"));
}

#[tokio::test]
async fn object_tree_contains_tables_and_columns() {
    let Some(mut s) = session().await else { return };
    s.execute(
        "IF OBJECT_ID('dbo.it_catalog') IS NULL \
         CREATE TABLE dbo.it_catalog (id bigint PRIMARY KEY, label nvarchar(20) NOT NULL)",
        CancelHandle::new(),
    )
    .await
    .expect("create table");
    // Other tests in this binary create and drop schemas concurrently; a catalog read can
    // lose a deadlock against that DDL, so retry a few times.
    let mut attempt = 0;
    let tree = loop {
        match s.load_objects().await {
            Ok(tree) => break tree,
            Err(e) if attempt < 3 => {
                eprintln!("retrying load_objects after: {e}");
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
            Err(e) => panic!("objects: {e}"),
        }
    };
    let master = tree
        .iter()
        .find(|d| d.name == "master")
        .expect("master listed");
    let table = master
        .children
        .iter()
        .find(|c| c.kind == ObjectKind::Schema && c.name == "dbo")
        .and_then(|dbo| dbo.children.iter().find(|f| f.name == "Tables"))
        .and_then(|f| f.children.iter().find(|t| t.name == "it_catalog"))
        .expect("table listed");
    let details: Vec<&str> = table
        .children
        .iter()
        .filter_map(|c| c.detail.as_deref())
        .collect();
    assert_eq!(details, ["bigint · PK", "nvarchar(20) · NOT NULL"]);
}

const SETUP: &str = "IF OBJECT_ID('dbo.it_edit') IS NOT NULL DROP TABLE dbo.it_edit;     CREATE TABLE dbo.it_edit (id int PRIMARY KEY, name nvarchar(10));     INSERT INTO dbo.it_edit VALUES (1, N'a'), (2, N'b');";

fn change(sql: &str) -> Change {
    Change {
        sql: sql.into(),
        expected_rows: 1,
    }
}

#[tokio::test]
async fn apply_changes_is_transactional() {
    let Some(mut s) = session().await else { return };
    s.execute(SETUP, CancelHandle::new()).await.expect("setup");

    let saved = s
        .apply_changes(&[
            change("UPDATE it_edit SET name = 'z' WHERE id = 1"),
            change("INSERT INTO it_edit (id, name) VALUES (3, 'c')"),
            change("DELETE FROM it_edit WHERE id = 2"),
        ])
        .await
        .expect("apply");
    assert_eq!(saved, 3);

    // The second change matches no row: the first must be rolled back too.
    let err = s
        .apply_changes(&[
            change("UPDATE it_edit SET name = 'q' WHERE id = 1"),
            change("UPDATE it_edit SET name = 'q' WHERE id = 99"),
        ])
        .await
        .expect_err("row count mismatch");
    assert!(err.message.contains("affected 0 rows"), "{err}");

    let out = s
        .execute(
            "SELECT id, name FROM it_edit ORDER BY id",
            CancelHandle::new(),
        )
        .await
        .expect("select");
    let rows = &out.last_result().expect("rows").rows;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], CellValue::Text("z".into()));
    assert_eq!(rows[1][1], CellValue::Text("c".into()));
}

#[path = "../../rdbs-core/testkit/schema_roundtrip.rs"]
mod roundtrip;

#[tokio::test]
async fn describe_create_alter_round_trip() {
    let Some(mut s) = session().await else { return };
    let Some(mut copy) = session().await else {
        return;
    };
    for schema in ["it_rt_a", "it_rt_b"] {
        s.execute(
            &format!(
                "IF OBJECT_ID(N'{schema}.it_child') IS NOT NULL DROP TABLE {schema}.it_child; \
                 IF OBJECT_ID(N'{schema}.it_parent') IS NOT NULL DROP TABLE {schema}.it_parent; \
                 IF SCHEMA_ID(N'{schema}') IS NOT NULL DROP SCHEMA {schema};"
            ),
            CancelHandle::new(),
        )
        .await
        .expect("cleanup");
        s.execute(&format!("CREATE SCHEMA {schema}"), CancelHandle::new())
            .await
            .expect("create schema");
    }
    roundtrip::schema_round_trip(
        DatabaseKind::SqlServer,
        s.as_mut(),
        Some("it_rt_a"),
        copy.as_mut(),
        Some("it_rt_b"),
        roundtrip::Types {
            int: "int",
            bigint: "bigint",
            varchar: "varchar(50)",
            text: "nvarchar(max)",
        },
    )
    .await;
}

#[tokio::test]
async fn explain_estimated_and_actual_plans() {
    let Some(mut s) = session().await else { return };
    s.execute(
        "IF OBJECT_ID(N'dbo.it_explain') IS NOT NULL DROP TABLE dbo.it_explain; \
         CREATE TABLE dbo.it_explain (id int PRIMARY KEY, v nvarchar(10)); \
         INSERT INTO dbo.it_explain VALUES (1, N'a'), (2, N'b'), (3, N'c');",
        CancelHandle::new(),
    )
    .await
    .expect("setup");
    let plan = s
        .explain("SELECT * FROM dbo.it_explain WHERE id = 2", false)
        .await
        .expect("estimated plan");
    assert!(!plan.analyzed);
    let op = &plan.roots[0].children[0];
    assert!(op.operation.contains("Seek"), "{op:?}");
    assert!(
        op.object
            .as_deref()
            .is_some_and(|o| o.contains("it_explain")),
        "{op:?}"
    );

    let plan = s
        .explain("DELETE FROM dbo.it_explain WHERE id = 3", true)
        .await
        .expect("actual plan");
    assert!(plan.analyzed);
    assert!(
        plan.roots
            .iter()
            .any(|r| r.children.iter().any(|c| c.actual_rows.is_some())),
        "{plan:?}"
    );
    let out = s
        .execute("SELECT count(*) FROM dbo.it_explain", CancelHandle::new())
        .await
        .expect("count");
    assert_eq!(
        out.results[0].rows[0][0],
        CellValue::Int(3),
        "actual plan was rolled back"
    );
    // The session is back to normal execution.
    let out = s
        .execute("SELECT 1", CancelHandle::new())
        .await
        .expect("normal");
    assert_eq!(out.results[0].rows[0][0], CellValue::Int(1));
}

/// Imports insert explicit identity values with IDENTITY_INSERT inside the same batch.
#[tokio::test]
async fn identity_insert_within_one_change() {
    let Some(mut s) = session().await else { return };
    s.execute(
        "IF OBJECT_ID(N'dbo.it_ident') IS NOT NULL DROP TABLE dbo.it_ident; \
         CREATE TABLE dbo.it_ident (id int IDENTITY(1,1) PRIMARY KEY, v nvarchar(10));",
        CancelHandle::new(),
    )
    .await
    .expect("setup");
    let n = s
        .apply_changes(&[Change {
            sql:
                "SET IDENTITY_INSERT [dbo].[it_ident] ON; INSERT INTO [dbo].[it_ident] ([id], [v]) \
                  VALUES (10, N'a'), (20, N'b'); SET IDENTITY_INSERT [dbo].[it_ident] OFF"
                    .into(),
            expected_rows: 2,
        }])
        .await
        .expect("identity insert");
    assert_eq!(n, 2);
    let out = s
        .execute("SELECT max(id) FROM dbo.it_ident", CancelHandle::new())
        .await
        .expect("select");
    assert_eq!(out.results[0].rows[0][0], CellValue::Int(20));
}

#[tokio::test]
async fn routines_can_be_edited_and_executed() {
    use rdbs_core::routine::{ArgValue, ParamMode, call_statement, edit_script};
    use rdbs_core::schema::ObjectPath;
    let Some(mut s) = session().await else { return };
    for statement in [
        "IF OBJECT_ID('dbo.it_tax') IS NOT NULL DROP FUNCTION dbo.it_tax",
        "IF OBJECT_ID('dbo.it_rows') IS NOT NULL DROP FUNCTION dbo.it_rows",
        "IF OBJECT_ID('dbo.it_total') IS NOT NULL DROP PROCEDURE dbo.it_total",
        "CREATE FUNCTION dbo.it_tax(@amount decimal(12,2), @rate decimal(5,2)) \
         RETURNS decimal(12,2) AS BEGIN RETURN @amount * @rate / 100 END",
        "CREATE FUNCTION dbo.it_rows(@n int) RETURNS TABLE AS RETURN \
         (SELECT TOP (@n) name FROM sys.objects ORDER BY name)",
        "CREATE PROCEDURE dbo.it_total @label nvarchar(20), @total int OUTPUT \
         AS BEGIN SET NOCOUNT ON; SET @total = LEN(@label) END",
    ] {
        s.execute(statement, CancelHandle::new())
            .await
            .expect(statement);
    }
    let path = |name: &str, kind| ObjectPath {
        database: None,
        schema: Some("dbo".into()),
        name: name.into(),
        kind,
    };

    let f = s
        .routine_signature(&path("it_tax", ObjectKind::Function))
        .await
        .expect("scalar signature");
    assert_eq!(f.params.len(), 2, "{f:?}");
    assert_eq!(f.params[0].name, "@amount");
    assert_eq!(f.params[0].data_type, "decimal(12,2)");
    assert!(!f.returns_table);
    let sql = call_statement(
        DatabaseKind::SqlServer,
        "[dbo].[it_tax]",
        ObjectKind::Function,
        &f,
        &[ArgValue::Text("200".into()), ArgValue::Text("5".into())],
    );
    let out = s
        .execute(&sql, CancelHandle::new())
        .await
        .expect("call scalar");
    assert_eq!(
        out.last_result().expect("rows").rows[0][0],
        CellValue::Decimal("10.00".into())
    );

    let t = s
        .routine_signature(&path("it_rows", ObjectKind::Function))
        .await
        .expect("table signature");
    assert!(t.returns_table);
    let sql = call_statement(
        DatabaseKind::SqlServer,
        "[dbo].[it_rows]",
        ObjectKind::Function,
        &t,
        &[ArgValue::Text("3".into())],
    );
    let out = s
        .execute(&sql, CancelHandle::new())
        .await
        .expect("call table function");
    assert_eq!(out.last_result().expect("rows").rows.len(), 3);

    let p = s
        .routine_signature(&path("it_total", ObjectKind::Procedure))
        .await
        .expect("procedure signature");
    assert_eq!(p.params[1].mode, ParamMode::Out);
    assert_eq!(p.params[0].data_type, "nvarchar(20)");
    let sql = call_statement(
        DatabaseKind::SqlServer,
        "[dbo].[it_total]",
        ObjectKind::Procedure,
        &p,
        &[ArgValue::Text("hello".into()), ArgValue::Null],
    );
    let out = s
        .execute(&sql, CancelHandle::new())
        .await
        .expect("call procedure");
    assert_eq!(
        out.last_result().expect("rows").rows[0][0],
        CellValue::Int(5)
    );

    // CREATE OR ALTER: the edit script saves the procedure when run unchanged.
    let ddl = s
        .object_ddl(&path("it_total", ObjectKind::Procedure))
        .await
        .expect("ddl");
    let script = edit_script(
        DatabaseKind::SqlServer,
        "[dbo].[it_total]",
        ObjectKind::Procedure,
        &ddl,
    );
    assert!(script.contains("CREATE OR ALTER"), "{script}");
    s.execute(&script, CancelHandle::new())
        .await
        .expect("edit script runs");
}

const AGENT_JOB: &str = "zephix_it_agent_job";

async fn delete_agent_job(s: &mut dyn DatabaseAdapter) {
    let sql = format!(
        "IF EXISTS (SELECT 1 FROM msdb.dbo.sysjobs WHERE name = N'{AGENT_JOB}') \
         EXEC msdb.dbo.sp_delete_job @job_name = N'{AGENT_JOB}', @delete_unused_schedule = 1"
    );
    let _ = s.execute(&sql, CancelHandle::new()).await;
}

/// Creates a disabled job in msdb and reads it back from the explorer, the script and history.
/// Requires SQL Server Agent catalog tables (Developer/Standard/Enterprise; the compose file
/// sets `MSSQL_AGENT_ENABLED`).
#[tokio::test]
async fn agent_job_is_listed_and_scripted() {
    use rdbs_core::schema::ObjectPath;
    let Some(mut s) = session().await else { return };
    delete_agent_job(s.as_mut()).await;
    s.execute(
        "EXEC msdb.dbo.sp_add_job \
            @job_name = N'zephix_it_agent_job', @enabled = 0, \
            @description = N'zephix it: O''Hare', @owner_login_name = N'sa'; \
         EXEC msdb.dbo.sp_add_jobstep \
            @job_name = N'zephix_it_agent_job', @step_name = N'hello', \
            @subsystem = N'TSQL', @command = N'SELECT 1 AS zephix_agent', \
            @database_name = N'master'; \
         EXEC msdb.dbo.sp_add_jobschedule \
            @job_name = N'zephix_it_agent_job', @name = N'zephix_nightly', \
            @enabled = 1, @freq_type = 4, @freq_interval = 1, \
            @freq_subday_type = 1, @freq_subday_interval = 0, \
            @freq_relative_interval = 0, @freq_recurrence_factor = 0, \
            @active_start_date = 20261024, @active_end_date = 99991231, \
            @active_start_time = 20000, @active_end_time = 235959; \
         EXEC msdb.dbo.sp_add_jobserver \
            @job_name = N'zephix_it_agent_job', @server_name = N'(LOCAL)';",
        CancelHandle::new(),
    )
    .await
    .expect(
        "create sample Agent job (msdb.dbo.sysjobs must exist; \
         set MSSQL_AGENT_ENABLED=true on the SQL Server container)",
    );

    let mut attempt = 0;
    let tree = loop {
        match s.load_objects().await {
            Ok(tree) => break tree,
            Err(e) if attempt < 3 => {
                eprintln!("retrying load_objects after: {e}");
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            Err(e) => panic!("objects: {e}"),
        }
    };
    let agent = tree
        .iter()
        .find(|o| o.name == "SQL Server Agent")
        .expect("SQL Server Agent node");
    assert_ne!(
        agent.detail.as_deref(),
        Some("not available"),
        "Agent node: {} / {:?}",
        agent.detail.as_deref().unwrap_or(""),
        agent.children.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    let job = agent
        .children
        .iter()
        .find(|c| c.name == "Jobs")
        .and_then(|folder| folder.children.iter().find(|j| j.name == AGENT_JOB))
        .expect("sample job listed");
    assert_eq!(job.kind, ObjectKind::AgentJob);
    let detail = job.detail.as_deref().unwrap_or("");
    assert!(detail.contains("disabled"), "{detail}");
    assert!(detail.contains("sa"), "{detail}");
    assert!(detail.contains("never run"), "{detail}");

    let script = s
        .object_ddl(&ObjectPath {
            database: None,
            schema: None,
            name: AGENT_JOB.into(),
            kind: ObjectKind::AgentJob,
        })
        .await
        .expect("script job");
    for part in [
        "zephix_it_agent_job",
        "O''Hare",
        "SELECT 1 AS zephix_agent",
        "zephix_nightly",
        "@freq_type = 4",
        "sp_add_jobstep",
        "sp_add_jobschedule",
        "@server_name = N'(LOCAL)'",
    ] {
        assert!(script.contains(part), "missing {part} in\n{script}");
    }
    assert!(!script.contains("sp_start_job"), "{script}");

    let history = s
        .execute(
            &rdbs_core::agent::history_sql(AGENT_JOB),
            CancelHandle::new(),
        )
        .await
        .expect("job history");
    let columns: Vec<&str> = history
        .last_result()
        .expect("history columns")
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(columns.contains(&"step_id"), "{columns:?}");
    assert!(columns.contains(&"message"), "{columns:?}");
    assert!(columns.contains(&"outcome"), "{columns:?}");

    s.execute("SELECT 1 AS still_open", CancelHandle::new())
        .await
        .expect("session still usable after reading Agent");
    delete_agent_job(s.as_mut()).await;
}
