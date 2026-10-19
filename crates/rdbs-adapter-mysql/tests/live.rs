//! Integration tests against a real MySQL server (see `dev/docker-compose.yml`).
//!
//! Skipped unless `RDBS_IT=1`. Settings: `RDBS_MY_HOST`, `RDBS_MY_PORT`, `RDBS_MY_USER`,
//! `RDBS_MY_PASSWORD`, `RDBS_MY_DB` (defaults match the compose file).

// Test helpers panic on purpose: a failing setup should fail the test loudly.
#![allow(clippy::expect_used)]

use std::time::{Duration, Instant};

use rdbs_adapter_mysql::MySqlFactory;
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
        eprintln!("skipped: set RDBS_IT=1 to run MySQL integration tests");
        return None;
    }
    let mut p = ConnectionProfile::new(DatabaseKind::MySql);
    p.name = "it-mysql".into();
    p.host = env("RDBS_MY_HOST", "127.0.0.1");
    p.port = env("RDBS_MY_PORT", "53306").parse().unwrap_or(53306);
    p.username = env("RDBS_MY_USER", "root");
    p.database = env("RDBS_MY_DB", "rdbs_test");
    p.ssl_mode = SslMode::Prefer;
    Some((p, env("RDBS_MY_PASSWORD", "rdbs_test_pw")))
}

async fn session() -> Option<Box<dyn DatabaseAdapter>> {
    let (profile, password) = target()?;
    Some(
        MySqlFactory
            .connect(&profile, Some(&password))
            .await
            .expect("connect to test server"),
    )
}

#[tokio::test]
async fn reports_version_and_types() {
    let Some(mut s) = session().await else { return };
    assert!(
        s.server_version().starts_with("MySQL") || s.server_version().starts_with("MariaDB"),
        "{}",
        s.server_version()
    );
    let out = s
        .execute(
            "SELECT CAST(42 AS SIGNED) AS i, CAST(12.30 AS DECIMAL(10,2)) AS n, \
                    CAST(1.5 AS DOUBLE) AS f, 'x' AS t, NULL AS nul, \
                    CAST('2026-10-16' AS DATE) AS d, X'DEAD' AS raw",
            CancelHandle::new(),
        )
        .await
        .expect("select");
    let rs = out.last_result().expect("result set");
    assert_eq!(rs.columns[0].type_name, "BIGINT");
    assert_eq!(rs.columns[1].type_name, "DECIMAL");
    assert_eq!(rs.columns[5].type_name, "DATE");
    assert_eq!(
        rs.rows[0],
        vec![
            CellValue::Int(42),
            CellValue::Decimal("12.30".into()),
            CellValue::Float(1.5),
            CellValue::Text("x".into()),
            CellValue::Null,
            CellValue::Temporal("2026-10-16".into()),
            CellValue::Bytes(vec![0xde, 0xad]),
        ]
    );
}

#[tokio::test]
async fn scripts_return_last_result_and_counts() {
    let Some(mut s) = session().await else { return };
    let out = s
        .execute(
            "DROP TEMPORARY TABLE IF EXISTS it_t; \
             CREATE TEMPORARY TABLE it_t (id INT PRIMARY KEY, name VARCHAR(10)); \
             INSERT INTO it_t VALUES (1, 'a'), (2, 'b'), (3, 'c'); \
             SELECT * FROM it_t ORDER BY id",
            CancelHandle::new(),
        )
        .await
        .expect("script");
    assert_eq!(out.last_result().expect("result set").rows.len(), 3);
    assert_eq!(out.rows_affected, Some(3));

    let out = s
        .execute(
            "UPDATE it_t SET name = 'z' WHERE id > 1",
            CancelHandle::new(),
        )
        .await
        .expect("update");
    assert_eq!(out.rows_affected, Some(2));
    assert!(out.results.is_empty());
}

#[tokio::test]
async fn compound_statements_need_no_delimiter() {
    let Some(mut s) = session().await else { return };
    s.execute(
        "DROP PROCEDURE IF EXISTS it_proc; \
         CREATE PROCEDURE it_proc() BEGIN SELECT 1 AS a; SELECT 2 AS b; END",
        CancelHandle::new(),
    )
    .await
    .expect("create procedure");
    let out = s
        .execute("CALL it_proc()", CancelHandle::new())
        .await
        .expect("call");
    assert_eq!(out.last_result().expect("last result").columns[0].name, "b");
}

#[tokio::test]
async fn returns_every_result_set() {
    let Some(mut s) = session().await else { return };
    let out = s
        .execute(
            "SELECT 1 AS a; SELECT 'x' AS b, 2 AS c",
            CancelHandle::new(),
        )
        .await
        .expect("script");
    assert_eq!(out.results.len(), 2);
    assert_eq!(out.results[0].columns[0].name, "a");
    assert_eq!(out.results[1].columns.len(), 2);
}

#[tokio::test]
async fn produces_ddl_via_show_create() {
    use rdbs_core::schema::ObjectPath;
    let Some(mut s) = session().await else { return };
    s.execute(
        "CREATE TABLE IF NOT EXISTS it_ddl (id INT PRIMARY KEY, code VARCHAR(10) NOT NULL);          CREATE OR REPLACE VIEW it_ddl_v AS SELECT id FROM it_ddl",
        CancelHandle::new(),
    )
    .await
    .expect("setup");
    let path = |name: &str, kind| ObjectPath {
        database: Some("rdbs_test".into()),
        schema: None,
        name: name.into(),
        kind,
    };
    let table = s
        .object_ddl(&path("it_ddl", ObjectKind::Table))
        .await
        .expect("table ddl");
    assert!(
        table.starts_with("CREATE TABLE `it_ddl`") && table.contains("PRIMARY KEY (`id`)"),
        "{table}"
    );
    let view = s
        .object_ddl(&path("it_ddl_v", ObjectKind::View))
        .await
        .expect("view ddl");
    assert!(view.contains("VIEW `it_ddl_v`"), "{view}");
}

#[tokio::test]
async fn errors_carry_errno() {
    let Some(mut s) = session().await else { return };
    let err = s
        .execute("SELEC 1", CancelHandle::new())
        .await
        .expect_err("syntax error");
    assert_eq!(err.code.as_deref(), Some("1064"));
    assert!(s.execute("SELECT 1", CancelHandle::new()).await.is_ok());
}

#[tokio::test]
async fn cancellation_uses_kill_query() {
    let Some(mut s) = session().await else { return };
    let cancel = CancelHandle::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    let err = s
        .execute("SELECT SLEEP(30)", cancel)
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
    let err = MySqlFactory
        .connect(&profile, Some("definitely-wrong"))
        .await
        .err()
        .expect("auth failure");
    assert_eq!(err.kind, DbErrorKind::Authentication, "{err}");
    assert_eq!(err.code.as_deref(), Some("1045"));
}

#[tokio::test]
async fn object_tree_contains_tables_and_columns() {
    let Some(mut s) = session().await else { return };
    s.execute(
        "CREATE TABLE IF NOT EXISTS it_catalog (id BIGINT PRIMARY KEY, label VARCHAR(20) NOT NULL)",
        CancelHandle::new(),
    )
    .await
    .expect("create table");
    let tree = s.load_objects().await.expect("objects");
    let db = tree
        .iter()
        .find(|d| d.kind == ObjectKind::Database && d.name == "rdbs_test")
        .expect("database listed");
    let table = db
        .children
        .iter()
        .find(|f| f.name == "Tables")
        .and_then(|f| f.children.iter().find(|t| t.name == "it_catalog"))
        .expect("table listed");
    let details: Vec<&str> = table
        .children
        .iter()
        .filter_map(|c| c.detail.as_deref())
        .collect();
    assert_eq!(details, ["bigint · PK", "varchar(20) · NOT NULL"]);
}

const SETUP: &str = "DROP TABLE IF EXISTS it_edit;     CREATE TABLE it_edit (id INT PRIMARY KEY, name VARCHAR(10)) ENGINE=InnoDB;     INSERT INTO it_edit VALUES (1, 'a'), (2, 'b')";

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

async fn session_in(database: &str) -> Box<dyn DatabaseAdapter> {
    let (mut profile, password) = target().expect("integration tests enabled");
    profile.database = database.into();
    MySqlFactory
        .connect(&profile, Some(&password))
        .await
        .expect("connect to test database")
}

#[tokio::test]
async fn describe_create_alter_round_trip() {
    let Some(mut s) = session().await else { return };
    s.execute(
        "DROP DATABASE IF EXISTS rdbs_it_a; DROP DATABASE IF EXISTS rdbs_it_b; \
         CREATE DATABASE rdbs_it_a; CREATE DATABASE rdbs_it_b",
        CancelHandle::new(),
    )
    .await
    .expect("setup");
    let mut a = session_in("rdbs_it_a").await;
    let mut b = session_in("rdbs_it_b").await;
    roundtrip::schema_round_trip(
        DatabaseKind::MySql,
        a.as_mut(),
        None,
        b.as_mut(),
        None,
        roundtrip::Types {
            int: "int",
            bigint: "bigint",
            varchar: "varchar(50)",
            text: "text",
        },
    )
    .await;
}

#[tokio::test]
async fn explain_tree_and_analyze_rolls_back() {
    let Some(mut s) = session().await else { return };
    s.execute(
        "DROP TABLE IF EXISTS it_explain; CREATE TABLE it_explain (id int PRIMARY KEY, v text); \
         INSERT INTO it_explain VALUES (1,'a'),(2,'b'),(3,'c')",
        CancelHandle::new(),
    )
    .await
    .expect("setup");
    let plan = s
        .explain("SELECT * FROM it_explain WHERE id > 1", false)
        .await
        .expect("estimated plan");
    assert!(!plan.analyzed);
    assert!(plan.raw.contains("->"), "{}", plan.raw);
    assert!(plan.roots[0].cost.is_some(), "{plan:?}");

    let plan = s
        .explain("SELECT * FROM it_explain WHERE id > 1", true)
        .await
        .expect("analyzed plan");
    assert!(plan.analyzed);
    assert!(plan.roots[0].actual_rows.is_some(), "{plan:?}");
}

#[tokio::test]
async fn routines_can_be_edited_and_executed() {
    use rdbs_core::routine::{ArgValue, ParamMode, call_statement, edit_script};
    use rdbs_core::schema::ObjectPath;
    let Some(mut s) = session().await else { return };
    s.execute(
        "DROP FUNCTION IF EXISTS it_tax; DROP PROCEDURE IF EXISTS it_stats; \
         CREATE FUNCTION it_tax(amount DECIMAL(12,2), rate DECIMAL(5,2)) RETURNS DECIMAL(12,2) \
           DETERMINISTIC RETURN amount * rate / 100; \
         CREATE PROCEDURE it_stats(IN label VARCHAR(20), INOUT counter INT, OUT total INT) \
         BEGIN SET counter = counter + 1; SET total = CHAR_LENGTH(label); END",
        CancelHandle::new(),
    )
    .await
    .expect("create routines");
    let path = |name: &str, kind| ObjectPath {
        database: Some("rdbs_test".into()),
        schema: None,
        name: name.into(),
        kind,
    };

    let f = s
        .routine_signature(&path("it_tax", ObjectKind::Function))
        .await
        .expect("function signature");
    assert_eq!(f.params.len(), 2, "{f:?}");
    assert_eq!(f.params[0].data_type, "decimal(12,2)");
    let sql = call_statement(
        DatabaseKind::MySql,
        "`rdbs_test`.`it_tax`",
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
        CellValue::Decimal("10.00".into())
    );

    let p = s
        .routine_signature(&path("it_stats", ObjectKind::Procedure))
        .await
        .expect("procedure signature");
    let modes: Vec<ParamMode> = p.params.iter().map(|x| x.mode).collect();
    assert_eq!(modes, [ParamMode::In, ParamMode::InOut, ParamMode::Out]);
    let sql = call_statement(
        DatabaseKind::MySql,
        "`rdbs_test`.`it_stats`",
        ObjectKind::Procedure,
        &p,
        &[
            ArgValue::Text("hello".into()),
            ArgValue::Text("41".into()),
            ArgValue::Null,
        ],
    );
    let out = s
        .execute(&sql, CancelHandle::new())
        .await
        .expect("call procedure");
    let row = &out.last_result().expect("rows").rows[0];
    assert_eq!(row[0], CellValue::Int(42), "{row:?}");
    assert_eq!(row[1], CellValue::Int(5), "{row:?}");

    // The edit script (drop + create) recreates the procedure when run unchanged.
    let ddl = s
        .object_ddl(&path("it_stats", ObjectKind::Procedure))
        .await
        .expect("ddl");
    let script = edit_script(
        DatabaseKind::MySql,
        "`rdbs_test`.`it_stats`",
        ObjectKind::Procedure,
        &ddl,
    );
    s.execute(&script, CancelHandle::new())
        .await
        .expect("edit script runs");
    s.routine_signature(&path("it_stats", ObjectKind::Procedure))
        .await
        .expect("procedure still exists");
}
