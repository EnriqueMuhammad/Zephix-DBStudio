//! Integration tests against Oracle Database Free (`docker compose --profile oracle up -d
//! oracle`). They need Oracle Instant Client on PATH, so they only run with
//! `RDBS_IT=1 RDBS_ORACLE=1`.

#![allow(clippy::expect_used)]

use rdbs_adapter_oracle::OracleFactory;
use rdbs_core::{
    AdapterFactory, CancelHandle, CellValue, ConnectionProfile, DatabaseAdapter, DatabaseKind,
};

#[path = "../../rdbs-core/testkit/schema_roundtrip.rs"]
mod roundtrip;

fn enabled() -> bool {
    let on = std::env::var("RDBS_IT").as_deref() == Ok("1")
        && std::env::var("RDBS_ORACLE").as_deref() == Ok("1");
    if !on {
        eprintln!("skipped: set RDBS_IT=1 RDBS_ORACLE=1 (needs Oracle Instant Client)");
    }
    on
}

async fn session(user: &str) -> Box<dyn DatabaseAdapter> {
    let mut p = ConnectionProfile::new(DatabaseKind::Oracle);
    p.host = "127.0.0.1".into();
    p.port = 51521;
    p.database = "FREEPDB1".into();
    p.username = user.into();
    OracleFactory
        .connect(&p, Some("rdbs_test_pw"))
        .await
        .expect("connect")
}

async fn drop_tables(s: &mut dyn DatabaseAdapter, owner: &str) {
    for t in ["IT_CHILD", "IT_PARENT", "it_child", "it_parent"] {
        let _ = s
            .execute(
                &format!("BEGIN EXECUTE IMMEDIATE 'DROP TABLE {owner}.\"{t}\" CASCADE CONSTRAINTS'; EXCEPTION WHEN OTHERS THEN NULL; END;\n/"),
                CancelHandle::new(),
            )
            .await;
    }
}

#[tokio::test]
async fn queries_and_plsql() {
    if !enabled() {
        return;
    }
    let mut s = session("rdbs").await;
    assert!(
        s.server_version().contains("Oracle"),
        "{}",
        s.server_version()
    );
    let out = s
        .execute(
            "SELECT 42 AS n, 1.5 AS f, 'x' AS t, DATE '2026-01-02' AS d FROM dual;\n\
             BEGIN\n  NULL;\nEND;\n/\nSELECT 2 FROM dual",
            CancelHandle::new(),
        )
        .await
        .expect("script");
    assert_eq!(out.results.len(), 2);
    assert_eq!(out.results[0].rows[0][0], CellValue::Int(42));
    assert_eq!(out.results[0].rows[0][2], CellValue::Text("x".into()));
}

#[tokio::test]
async fn describe_create_alter_round_trip() {
    if !enabled() {
        return;
    }
    // Two schemas: the app user and SYSTEM's copy area would need grants, so both sides use
    // the same user with different table names is not possible; use RDBS and SYSTEM.
    let mut a = session("rdbs").await;
    let mut b = session("system").await;
    drop_tables(a.as_mut(), "RDBS").await;
    drop_tables(b.as_mut(), "SYSTEM").await;
    roundtrip::schema_round_trip(
        DatabaseKind::Oracle,
        a.as_mut(),
        Some("RDBS"),
        b.as_mut(),
        Some("SYSTEM"),
        roundtrip::Types {
            int: "NUMBER(10)",
            bigint: "NUMBER(19)",
            varchar: "VARCHAR2(50 CHAR)",
            text: "CLOB",
        },
    )
    .await;
}
