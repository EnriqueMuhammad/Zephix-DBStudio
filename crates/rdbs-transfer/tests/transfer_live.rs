//! Data transfer and sync between real databases. SQLite → SQLite always runs; the
//! PostgreSQL → MySQL / SQL Server / SQLite scenarios need `RDBS_IT=1` and the dev containers.

#![allow(clippy::expect_used)]

use rdbs_adapter_mssql::MssqlFactory;
use rdbs_adapter_mysql::MySqlFactory;
use rdbs_adapter_postgres::PostgresFactory;
use rdbs_adapter_sqlite::SqliteAdapter;
use rdbs_core::job::{TransferMode, TransferOptions};
use rdbs_core::{
    AdapterFactory, CancelHandle, CellValue, ConnectionProfile, DatabaseAdapter, DatabaseKind,
    SslMode,
};
use rdbs_transfer::transfer::run;

async fn exec(s: &mut dyn DatabaseAdapter, sql: &str) {
    s.execute(sql, CancelHandle::new())
        .await
        .unwrap_or_else(|e| panic!("{sql}\n→ {e}"));
}

async fn scalar(s: &mut dyn DatabaseAdapter, sql: &str) -> CellValue {
    let out = s.execute(sql, CancelHandle::new()).await.expect(sql);
    out.results[0].rows[0][0].clone()
}

fn as_int(v: CellValue) -> i64 {
    match v {
        CellValue::Int(i) => i,
        CellValue::Decimal(d) => d.parse().expect("number"),
        other => panic!("not a number: {other:?}"),
    }
}

async fn sqlite_file(name: &str) -> Box<dyn DatabaseAdapter> {
    let path = std::env::temp_dir().join(format!("rdbs-xfer-{}-{name}.sqlite", std::process::id()));
    std::fs::write(&path, b"").expect("create");
    Box::new(
        SqliteAdapter::open(&path.display().to_string())
            .await
            .expect("open"),
    )
}

async fn transfer(
    source: &mut dyn DatabaseAdapter,
    sk: DatabaseKind,
    target: &mut dyn DatabaseAdapter,
    tk: DatabaseKind,
    options: TransferOptions,
) -> Vec<rdbs_transfer::transfer::TableSummary> {
    run(
        source,
        sk,
        target,
        tk,
        &options,
        &CancelHandle::new(),
        |_| {},
    )
    .await
    .expect("transfer")
}

#[tokio::test]
async fn sqlite_copy_then_sync() {
    let mut src = sqlite_file("src").await;
    let mut dst = sqlite_file("dst").await;
    exec(
        src.as_mut(),
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, vip BOOLEAN); \
         CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers(id), \
                              total NUMERIC); \
         INSERT INTO customers VALUES (1, 'Ayu', 1), (2, 'Budi', 0), (3, 'Citra', NULL); \
         INSERT INTO orders VALUES (10, 1, 9.5), (11, 2, 20), (12, 1, 3.25);",
    )
    .await;
    let options = TransferOptions {
        page_rows: 2, // exercise paging
        batch_rows: 2,
        ..TransferOptions::default()
    };
    let summary = transfer(
        src.as_mut(),
        DatabaseKind::Sqlite,
        dst.as_mut(),
        DatabaseKind::Sqlite,
        options.clone(),
    )
    .await;
    assert_eq!(
        summary.iter().map(|s| s.table.as_str()).collect::<Vec<_>>(),
        ["customers", "orders"]
    );
    assert!(summary.iter().all(|s| s.created));
    assert_eq!(
        as_int(scalar(dst.as_mut(), "SELECT count(*) FROM orders").await),
        3
    );
    let fks = dst.describe_schema(None).await.expect("describe");
    let orders = fks.iter().find(|t| t.name == "orders").expect("orders");
    assert_eq!(
        orders.foreign_keys.len(),
        1,
        "foreign keys are added after the data"
    );

    // Change the source, then synchronise.
    exec(
        src.as_mut(),
        "UPDATE customers SET name = 'Ayu S.' WHERE id = 1; \
         INSERT INTO customers VALUES (4, 'Dewi', 1); \
         DELETE FROM orders WHERE id = 11; DELETE FROM customers WHERE id = 2;",
    )
    .await;
    let sync = TransferOptions {
        mode: TransferMode::Sync {
            delete_missing: true,
        },
        ..options
    };
    let summary = transfer(
        src.as_mut(),
        DatabaseKind::Sqlite,
        dst.as_mut(),
        DatabaseKind::Sqlite,
        sync,
    )
    .await;
    let customers = &summary[0];
    assert_eq!(
        (customers.inserted, customers.updated, customers.deleted),
        (1, 1, 1),
        "{summary:?}"
    );
    let orders = &summary[1];
    assert_eq!(
        (orders.inserted, orders.updated, orders.deleted),
        (0, 0, 1),
        "{summary:?}"
    );
    assert_eq!(
        scalar(dst.as_mut(), "SELECT name FROM customers WHERE id = 1").await,
        CellValue::Text("Ayu S.".into())
    );
    assert_eq!(
        as_int(scalar(dst.as_mut(), "SELECT count(*) FROM customers").await),
        3
    );

    // A second sync finds nothing to do.
    let again = transfer(
        src.as_mut(),
        DatabaseKind::Sqlite,
        dst.as_mut(),
        DatabaseKind::Sqlite,
        TransferOptions {
            mode: TransferMode::Sync {
                delete_missing: true,
            },
            ..TransferOptions::default()
        },
    )
    .await;
    assert!(
        again
            .iter()
            .all(|s| s.inserted + s.updated + s.deleted == 0),
        "{again:?}"
    );
}

fn it() -> bool {
    std::env::var("RDBS_IT").as_deref() == Ok("1")
}

fn profile(kind: DatabaseKind, port: u16, user: &str, database: &str) -> ConnectionProfile {
    let mut p = ConnectionProfile::new(kind);
    p.host = "127.0.0.1".into();
    p.port = port;
    p.username = user.into();
    p.database = database.into();
    p.ssl_mode = if kind == DatabaseKind::SqlServer {
        SslMode::Require
    } else {
        SslMode::Prefer
    };
    p
}

async fn postgres() -> Box<dyn DatabaseAdapter> {
    PostgresFactory
        .connect(
            &profile(DatabaseKind::PostgreSql, 55432, "postgres", "rdbs_test"),
            Some("rdbs_test_pw"),
        )
        .await
        .expect("postgres")
}

async fn mysql(database: &str) -> Box<dyn DatabaseAdapter> {
    MySqlFactory
        .connect(
            &profile(DatabaseKind::MySql, 53306, "root", database),
            Some("rdbs_test_pw"),
        )
        .await
        .expect("mysql")
}

async fn mssql() -> Box<dyn DatabaseAdapter> {
    MssqlFactory
        .connect(
            &profile(DatabaseKind::SqlServer, 51433, "sa", "master"),
            Some("Rdbs_Test_Pw_2026!"),
        )
        .await
        .expect("mssql")
}

/// Source data in PostgreSQL with a spread of types.
async fn pg_source(schema: &str) -> Box<dyn DatabaseAdapter> {
    let mut pg = postgres().await;
    let sql = "DROP SCHEMA IF EXISTS it_xfer CASCADE; CREATE SCHEMA it_xfer; \
         CREATE TABLE it_xfer.teams (id int GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                                     name varchar(40) NOT NULL); \
         CREATE TABLE it_xfer.players (id int GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
             team_id int REFERENCES it_xfer.teams(id), name text NOT NULL, \
             rating numeric(5,2), active boolean DEFAULT true, joined timestamp, note text); \
         INSERT INTO it_xfer.teams (name) VALUES ('Garuda'), ('Komodo'); \
         INSERT INTO it_xfer.players (team_id, name, rating, active, joined, note) VALUES \
             (1, 'Ayu', 9.25, true, '2026-01-02 10:30:00', 'it''s fine'), \
             (2, 'Budi', 7.5, false, NULL, NULL), (1, 'Citra', NULL, true, '2025-12-31 23:59:59', 'x');";
    exec(pg.as_mut(), &sql.replace("it_xfer", schema)).await;
    pg
}

async fn check_target(dst: &mut dyn DatabaseAdapter, players: &str) {
    assert_eq!(
        as_int(scalar(dst, &format!("SELECT count(*) FROM {players}")).await),
        3
    );
    let note = scalar(
        dst,
        &format!("SELECT note FROM {players} WHERE name = 'Ayu'"),
    )
    .await;
    assert_eq!(note, CellValue::Text("it's fine".into()));
}

#[tokio::test]
async fn postgres_to_mysql_mssql_and_sqlite() {
    if !it() {
        eprintln!("skipped: set RDBS_IT=1");
        return;
    }
    let mut pg = pg_source("it_xfer").await;
    let options = TransferOptions {
        source_schema: Some("it_xfer".into()),
        ..TransferOptions::default()
    };

    // MySQL
    let mut admin = mysql("rdbs_test").await;
    exec(
        admin.as_mut(),
        "DROP DATABASE IF EXISTS rdbs_it_xfer; CREATE DATABASE rdbs_it_xfer",
    )
    .await;
    let mut my = mysql("rdbs_it_xfer").await;
    transfer(
        pg.as_mut(),
        DatabaseKind::PostgreSql,
        my.as_mut(),
        DatabaseKind::MySql,
        options.clone(),
    )
    .await;
    check_target(my.as_mut(), "players").await;

    // SQL Server
    let mut ms = mssql().await;
    exec(
        ms.as_mut(),
        "IF OBJECT_ID(N'it_xfer.players') IS NOT NULL DROP TABLE it_xfer.players; \
         IF OBJECT_ID(N'it_xfer.teams') IS NOT NULL DROP TABLE it_xfer.teams; \
         IF SCHEMA_ID(N'it_xfer') IS NOT NULL DROP SCHEMA it_xfer;",
    )
    .await;
    exec(ms.as_mut(), "CREATE SCHEMA it_xfer").await;
    let ms_options = TransferOptions {
        target_schema: Some("it_xfer".into()),
        ..options.clone()
    };
    transfer(
        pg.as_mut(),
        DatabaseKind::PostgreSql,
        ms.as_mut(),
        DatabaseKind::SqlServer,
        ms_options.clone(),
    )
    .await;
    check_target(ms.as_mut(), "it_xfer.players").await;
    // Identity values were kept and the identity continues after them.
    exec(
        ms.as_mut(),
        "INSERT INTO it_xfer.teams (name) VALUES (N'Rajawali')",
    )
    .await;
    assert_eq!(
        as_int(scalar(ms.as_mut(), "SELECT max(id) FROM it_xfer.teams").await),
        3
    );

    // SQLite
    let mut lite = sqlite_file("from-pg").await;
    transfer(
        pg.as_mut(),
        DatabaseKind::PostgreSql,
        lite.as_mut(),
        DatabaseKind::Sqlite,
        options.clone(),
    )
    .await;
    check_target(lite.as_mut(), "players").await;

    // Sync after changes in PostgreSQL reaches SQL Server.
    exec(
        pg.as_mut(),
        "UPDATE it_xfer.players SET rating = 8.0 WHERE name = 'Budi'; \
         DELETE FROM it_xfer.players WHERE name = 'Citra'",
    )
    .await;
    let summary = transfer(
        pg.as_mut(),
        DatabaseKind::PostgreSql,
        ms.as_mut(),
        DatabaseKind::SqlServer,
        TransferOptions {
            mode: TransferMode::Sync {
                delete_missing: true,
            },
            ..ms_options
        },
    )
    .await;
    let players = summary
        .iter()
        .find(|s| s.table == "players")
        .expect("players");
    assert_eq!((players.updated, players.deleted), (1, 1), "{summary:?}");
    let teams = summary.iter().find(|s| s.table == "teams").expect("teams");
    assert_eq!(
        teams.deleted, 1,
        "the extra team inserted on SQL Server is removed: {summary:?}"
    );
}

#[tokio::test]
async fn postgres_schema_to_schema_and_sequences() {
    if !it() {
        eprintln!("skipped: set RDBS_IT=1");
        return;
    }
    let mut src = pg_source("it_xfer_src").await;
    let mut dst = postgres().await;
    exec(
        dst.as_mut(),
        "DROP SCHEMA IF EXISTS it_xfer_copy CASCADE; CREATE SCHEMA it_xfer_copy",
    )
    .await;
    transfer(
        src.as_mut(),
        DatabaseKind::PostgreSql,
        dst.as_mut(),
        DatabaseKind::PostgreSql,
        TransferOptions {
            source_schema: Some("it_xfer_src".into()),
            target_schema: Some("it_xfer_copy".into()),
            ..TransferOptions::default()
        },
    )
    .await;
    check_target(dst.as_mut(), "it_xfer_copy.players").await;
    // The identity sequence was moved past the copied ids.
    exec(
        dst.as_mut(),
        "INSERT INTO it_xfer_copy.teams (name) VALUES ('Rajawali')",
    )
    .await;
    assert_eq!(
        as_int(scalar(dst.as_mut(), "SELECT max(id) FROM it_xfer_copy.teams").await),
        3
    );
}
