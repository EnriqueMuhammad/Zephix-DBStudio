//! Integration tests against a real PostgreSQL server (see `dev/docker-compose.yml`).
//!
//! Skipped unless `RDBS_IT=1`. Connection settings come from `RDBS_PG_HOST`, `RDBS_PG_PORT`,
//! `RDBS_PG_USER`, `RDBS_PG_PASSWORD` and `RDBS_PG_DB` (defaults match the compose file).

// Test helpers panic on purpose: a failing setup should fail the test loudly.
#![allow(clippy::expect_used)]

use std::time::{Duration, Instant};

use rdbs_adapter_postgres::PostgresFactory;
use rdbs_core::edit::Change;
use rdbs_core::{
    AdapterFactory, CancelHandle, CellValue, ConnectionProfile, DatabaseAdapter, DatabaseKind,
    DbErrorKind, ObjectKind, SslMode,
};

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

/// Profile and password for the test server, or `None` when integration tests are off.
fn target() -> Option<(ConnectionProfile, String)> {
    if std::env::var("RDBS_IT").as_deref() != Ok("1") {
        eprintln!("skipped: set RDBS_IT=1 to run PostgreSQL integration tests");
        return None;
    }
    let mut p = ConnectionProfile::new(DatabaseKind::PostgreSql);
    p.name = "it-postgres".into();
    p.host = env("RDBS_PG_HOST", "127.0.0.1");
    p.port = env("RDBS_PG_PORT", "55432").parse().unwrap_or(55432);
    p.username = env("RDBS_PG_USER", "postgres");
    p.database = env("RDBS_PG_DB", "rdbs_test");
    p.ssl_mode = SslMode::Prefer;
    Some((p, env("RDBS_PG_PASSWORD", "rdbs_test_pw")))
}

async fn session() -> Option<Box<dyn DatabaseAdapter>> {
    let (profile, password) = target()?;
    Some(
        PostgresFactory
            .connect(&profile, Some(&password))
            .await
            .expect("connect to test server"),
    )
}

#[tokio::test]
async fn reports_version_and_types() {
    let Some(mut s) = session().await else { return };
    assert!(
        s.server_version().starts_with("PostgreSQL"),
        "{}",
        s.server_version()
    );

    let out = s
        .execute(
            "SELECT 42::int4 AS i, 1.5::float8 AS f, 12.30::numeric(10,2) AS n, true AS b, \
                    'x'::text AS t, NULL::text AS nul, '\\xdead'::bytea AS raw, \
                    '2026-10-16'::date AS d, '{\"a\":1}'::jsonb AS j",
            CancelHandle::new(),
        )
        .await
        .expect("select");
    let rs = out.last_result().expect("result set");
    let types: Vec<&str> = rs.columns.iter().map(|c| c.type_name.as_str()).collect();
    assert_eq!(
        types,
        [
            "int4", "float8", "numeric", "bool", "text", "text", "bytea", "date", "jsonb"
        ]
    );
    assert_eq!(
        rs.rows[0],
        vec![
            CellValue::Int(42),
            CellValue::Float(1.5),
            CellValue::Decimal("12.30".into()),
            CellValue::Bool(true),
            CellValue::Text("x".into()),
            CellValue::Null,
            CellValue::Bytes(vec![0xde, 0xad]),
            CellValue::Temporal("2026-10-16".into()),
            CellValue::Json("{\"a\": 1}".into()),
        ]
    );
}

#[tokio::test]
async fn scripts_return_last_result_and_counts() {
    let Some(mut s) = session().await else { return };
    let out = s
        .execute(
            "CREATE TEMP TABLE it_t (id int PRIMARY KEY, name text); \
             INSERT INTO it_t VALUES (1, 'a'), (2, 'b'), (3, 'c'); \
             SELECT * FROM it_t ORDER BY id",
            CancelHandle::new(),
        )
        .await
        .expect("script");
    let rs = out.last_result().expect("result set");
    assert_eq!(rs.rows.len(), 3);
    assert_eq!(rs.columns[0].type_name, "int4");
    assert_eq!(out.rows_affected, Some(3));
    // CREATE reports no count; INSERT and SELECT each get a line.
    assert_eq!(out.messages, vec!["(3 rows affected)", "(3 rows affected)"]);

    let out = s
        .execute(
            "UPDATE it_t SET name = 'z' WHERE id > 1",
            CancelHandle::new(),
        )
        .await
        .expect("update");
    assert_eq!(out.rows_affected, Some(2));
    assert!(out.results.is_empty());

    let out = s
        .execute(
            "BEGIN TRANSACTION;\n--Query\nDELETE FROM it_t WHERE id = 1;\n--COMMIT;\nROLLBACK;",
            CancelHandle::new(),
        )
        .await
        .expect("transaction template");
    assert_eq!(out.messages, vec!["(1 row affected)"]);
}

#[tokio::test]
async fn returns_every_result_set_and_notices() {
    let Some(mut s) = session().await else { return };
    let out = s
        .execute(
            "SELECT 1::int8 AS a; DO $$ BEGIN RAISE NOTICE 'hello from plpgsql'; END $$; SELECT 'x' AS b, 2 AS c",
            CancelHandle::new(),
        )
        .await
        .expect("script");
    assert_eq!(out.results.len(), 2);
    assert_eq!(out.results[0].columns[0].type_name, "int8");
    assert_eq!(out.results[1].columns[0].name, "b");
    assert_eq!(out.results[1].columns[1].type_name, "int4");
    assert!(
        out.messages
            .iter()
            .any(|m| m == "NOTICE: hello from plpgsql"),
        "{:?}",
        out.messages
    );
}

#[tokio::test]
async fn produces_ddl_for_tables_views_and_functions() {
    use rdbs_core::schema::ObjectPath;
    let Some(mut s) = session().await else { return };
    s.execute(
        "DROP VIEW IF EXISTS it_ddl_v; DROP TABLE IF EXISTS it_ddl;          CREATE TABLE it_ddl (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,              code varchar(10) NOT NULL UNIQUE, qty int DEFAULT 1 CHECK (qty > 0));          CREATE INDEX it_ddl_qty ON it_ddl (qty);          CREATE VIEW it_ddl_v AS SELECT id, code FROM it_ddl;          CREATE OR REPLACE FUNCTION it_ddl_f(a int) RETURNS int LANGUAGE sql AS 'SELECT a + 1'",
        CancelHandle::new(),
    )
    .await
    .expect("setup");
    let path = |name: &str, kind| ObjectPath {
        database: None,
        schema: Some("public".into()),
        name: name.into(),
        kind,
    };
    let table = s
        .object_ddl(&path("it_ddl", ObjectKind::Table))
        .await
        .expect("table ddl");
    assert!(
        table.starts_with("CREATE TABLE \"public\".\"it_ddl\" ("),
        "{table}"
    );
    for part in [
        "GENERATED ALWAYS AS IDENTITY",
        "DEFAULT 1",
        "PRIMARY KEY (id)",
        "UNIQUE (code)",
        "CHECK (qty > 0)",
        "CREATE INDEX it_ddl_qty",
    ] {
        assert!(
            table.contains(part),
            "missing {part} in
{table}"
        );
    }
    let view = s
        .object_ddl(&path("it_ddl_v", ObjectKind::View))
        .await
        .expect("view ddl");
    assert!(
        view.starts_with("CREATE OR REPLACE VIEW \"public\".\"it_ddl_v\" AS"),
        "{view}"
    );
    let func = s
        .object_ddl(&path("it_ddl_f", ObjectKind::Function))
        .await
        .expect("function ddl");
    assert!(
        func.contains("FUNCTION public.it_ddl_f(a integer)"),
        "{func}"
    );
    // The generated DDL is valid: recreate the table from it under a new name.
    // Constraint names are unique per schema, so recreate it in a scratch schema.
    let recreated = table
        .replace("\"public\".", "\"it_copy\".")
        .replace(" ON public.", " ON it_copy.");
    s.execute(
        &format!("DROP SCHEMA IF EXISTS it_copy CASCADE; CREATE SCHEMA it_copy; {recreated}"),
        CancelHandle::new(),
    )
    .await
    .expect("DDL re-executes");
}

#[tokio::test]
async fn errors_carry_sqlstate() {
    let Some(mut s) = session().await else { return };
    let err = s
        .execute("SELEC 1", CancelHandle::new())
        .await
        .expect_err("syntax error");
    assert_eq!(err.kind, DbErrorKind::Query);
    assert_eq!(err.code.as_deref(), Some("42601"));
    // The session stays usable after an error.
    assert!(s.execute("SELECT 1", CancelHandle::new()).await.is_ok());
}

#[tokio::test]
async fn cancellation_stops_server_side_work() {
    let Some(mut s) = session().await else { return };
    let cancel = CancelHandle::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    let err = s
        .execute("SELECT pg_sleep(30)", cancel)
        .await
        .expect_err("cancelled");
    assert_eq!(err.kind, DbErrorKind::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(5));
    let after = s
        .execute("SELECT 1 AS ok", CancelHandle::new())
        .await
        .expect("session usable after cancel");
    assert_eq!(
        after.last_result().expect("rows").rows[0][0],
        CellValue::Int(1)
    );
}

#[tokio::test]
async fn bad_password_is_an_authentication_error() {
    let Some((profile, _)) = target() else { return };
    let err = PostgresFactory
        .connect(&profile, Some("definitely-wrong"))
        .await
        .err()
        .expect("auth failure");
    assert_eq!(err.kind, DbErrorKind::Authentication, "{err}");
    assert_eq!(err.code.as_deref(), Some("28P01"));
}

#[tokio::test]
async fn object_tree_contains_tables_and_columns() {
    let Some(mut s) = session().await else { return };
    s.execute(
        "CREATE TABLE IF NOT EXISTS it_catalog (id bigint PRIMARY KEY, label varchar(20) NOT NULL)",
        CancelHandle::new(),
    )
    .await
    .expect("create table");
    let tree = s.load_objects().await.expect("objects");
    let db = tree
        .iter()
        .find(|d| d.name == "rdbs_test")
        .expect("current database listed");
    let public = db
        .children
        .iter()
        .find(|c| c.kind == ObjectKind::Schema && c.name == "public")
        .expect("public schema");
    let tables = public
        .children
        .iter()
        .find(|f| f.name == "Tables")
        .expect("Tables folder");
    let table = tables
        .children
        .iter()
        .find(|t| t.name == "it_catalog")
        .expect("table listed");
    let details: Vec<&str> = table
        .children
        .iter()
        .filter_map(|c| c.detail.as_deref())
        .collect();
    assert_eq!(details, ["bigint · PK", "character varying(20) · NOT NULL"]);
}

const SETUP: &str = "DROP TABLE IF EXISTS it_edit;     CREATE TABLE it_edit (id int PRIMARY KEY, name text);     INSERT INTO it_edit VALUES (1, 'a'), (2, 'b')";

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

#[path = "../../rdbs-core/testkit/edit_without_key.rs"]
mod without_key;

#[tokio::test]
async fn edits_table_without_primary_key() {
    let Some(mut s) = session().await else { return };
    s.execute(
        "DROP TABLE IF EXISTS it_nokey; \
         CREATE TABLE it_nokey (code varchar(20), starts timestamp(3), note text); \
         INSERT INTO it_nokey VALUES ('a', '2026-10-29 07:00:00.123', NULL), \
             ('a', '2026-10-29 07:00:00.123', NULL), ('b', '2026-10-29 15:00:00', 'x')",
        CancelHandle::new(),
    )
    .await
    .expect("setup");
    without_key::edit_without_key(
        s.as_mut(),
        DatabaseKind::PostgreSql,
        Some("public"),
        "it_nokey",
    )
    .await;
}

#[path = "../../rdbs-core/testkit/schema_roundtrip.rs"]
mod roundtrip;

#[tokio::test]
async fn describe_create_alter_round_trip() {
    let Some(mut s) = session().await else { return };
    let Some(mut copy) = session().await else {
        return;
    };
    s.execute(
        "DROP SCHEMA IF EXISTS it_rt_a CASCADE; DROP SCHEMA IF EXISTS it_rt_b CASCADE; \
         CREATE SCHEMA it_rt_a; CREATE SCHEMA it_rt_b",
        CancelHandle::new(),
    )
    .await
    .expect("setup");
    roundtrip::schema_round_trip(
        DatabaseKind::PostgreSql,
        s.as_mut(),
        Some("it_rt_a"),
        copy.as_mut(),
        Some("it_rt_b"),
        roundtrip::Types {
            int: "integer",
            bigint: "bigint",
            varchar: "varchar(50)",
            text: "text",
        },
    )
    .await;
}

#[tokio::test]
async fn explain_estimates_and_analyze_rolls_back() {
    let Some(mut s) = session().await else { return };
    s.execute(
        "DROP TABLE IF EXISTS it_explain; CREATE TABLE it_explain (id int PRIMARY KEY, v text); \
         INSERT INTO it_explain SELECT g, 'v' || g FROM generate_series(1, 100) g; ANALYZE it_explain",
        CancelHandle::new(),
    )
    .await
    .expect("setup");
    let plan = s
        .explain("SELECT * FROM it_explain WHERE id = 5;", false)
        .await
        .expect("estimated plan");
    assert!(!plan.analyzed);
    let root = &plan.roots[0];
    assert!(
        root.cost.is_some() && root.actual_rows.is_none(),
        "{root:?}"
    );
    assert!(
        root.object
            .as_deref()
            .is_some_and(|o| o.contains("it_explain")),
        "{root:?}"
    );

    let plan = s
        .explain("DELETE FROM it_explain WHERE id <= 10", true)
        .await
        .expect("analyzed plan");
    assert!(plan.analyzed);
    assert!(plan.notes.iter().any(|n| n.starts_with("Execution time")));
    let out = s
        .execute("SELECT count(*) FROM it_explain", CancelHandle::new())
        .await
        .expect("count");
    assert_eq!(
        out.results[0].rows[0][0],
        CellValue::Int(100),
        "ANALYZE was rolled back"
    );
}

#[tokio::test]
async fn routines_can_be_edited_and_executed() {
    use rdbs_core::routine::{ArgValue, ParamMode, call_statement, edit_script};
    use rdbs_core::schema::ObjectPath;
    let Some(mut s) = session().await else { return };
    s.execute(
        "DROP FUNCTION IF EXISTS public.it_tax(numeric, numeric); \
         DROP PROCEDURE IF EXISTS public.it_proc(integer, integer); \
         CREATE FUNCTION public.it_tax(amount numeric, rate numeric DEFAULT 10) \
           RETURNS numeric LANGUAGE sql AS $$ SELECT amount * rate / 100 $$; \
         CREATE PROCEDURE public.it_proc(IN a integer, INOUT doubled integer) \
           LANGUAGE plpgsql AS $$ BEGIN doubled := a * 2; END $$;",
        CancelHandle::new(),
    )
    .await
    .expect("create routines");
    let path = |name: &str, kind| ObjectPath {
        database: None,
        schema: Some("public".into()),
        name: name.into(),
        kind,
    };

    let f = s
        .routine_signature(&path("it_tax", ObjectKind::Function))
        .await
        .expect("function signature");
    assert_eq!(f.params.len(), 2);
    assert_eq!(f.params[0].name, "amount");
    assert_eq!(f.params[0].data_type, "numeric");
    assert!(!f.returns_table);
    let sql = call_statement(
        DatabaseKind::PostgreSql,
        "\"public\".\"it_tax\"",
        ObjectKind::Function,
        &f,
        &[ArgValue::Text("200".into()), ArgValue::Text("5".into())],
    );
    let out = s
        .execute(&sql, CancelHandle::new())
        .await
        .expect("call function");
    assert_eq!(
        out.last_result().expect("rows").rows[0][0],
        CellValue::Decimal("10.0000000000000000".into())
    );

    let p = s
        .routine_signature(&path("it_proc", ObjectKind::Procedure))
        .await
        .expect("procedure signature");
    assert_eq!(p.params[1].mode, ParamMode::InOut);
    let sql = call_statement(
        DatabaseKind::PostgreSql,
        "\"public\".\"it_proc\"",
        ObjectKind::Procedure,
        &p,
        &[ArgValue::Text("21".into()), ArgValue::Null],
    );
    let out = s
        .execute(&sql, CancelHandle::new())
        .await
        .expect("call procedure");
    assert_eq!(
        out.last_result().expect("rows").rows[0][0],
        CellValue::Int(42)
    );

    // The edit script recreates the routine when run unchanged.
    let ddl = s
        .object_ddl(&path("it_tax", ObjectKind::Function))
        .await
        .expect("ddl");
    let script = edit_script(
        DatabaseKind::PostgreSql,
        "\"public\".\"it_tax\"",
        ObjectKind::Function,
        &ddl,
    );
    s.execute(&script, CancelHandle::new())
        .await
        .expect("edit script runs");
}
