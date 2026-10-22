//! Data transfer and sync between real databases. SQLite → SQLite always runs; the
//! PostgreSQL → MySQL / SQL Server / SQLite scenarios need `RDBS_IT=1` and the dev containers.

#![allow(clippy::expect_used)]

use rdbs_adapter_mssql::MssqlFactory;
use rdbs_adapter_mysql::MySqlFactory;
use rdbs_adapter_postgres::PostgresFactory;
use rdbs_adapter_sqlite::SqliteAdapter;
use rdbs_core::job::{SyncScope, TransferMode, TransferOptions};
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
            scope: SyncScope::Data,
            drop_columns: false,
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
                scope: SyncScope::Data,
                drop_columns: false,
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

fn sync_options(scope: SyncScope, drop_columns: bool) -> TransferOptions {
    TransferOptions {
        mode: TransferMode::Sync {
            delete_missing: true,
            scope,
            drop_columns,
        },
        ..TransferOptions::default()
    }
}

async fn column_names(
    db: &mut dyn DatabaseAdapter,
    schema: Option<&str>,
    table: &str,
) -> Vec<String> {
    let tables = db.describe_schema(schema).await.expect("describe");
    let t = tables
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(table))
        .unwrap_or_else(|| panic!("table {table} missing"));
    t.columns.iter().map(|c| c.name.to_lowercase()).collect()
}

#[tokio::test]
async fn sqlite_structure_sync() {
    let mut src = sqlite_file("struct-src").await;
    let mut dst = sqlite_file("struct-dst").await;
    exec(
        src.as_mut(),
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT); \
         CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers(id)); \
         INSERT INTO customers VALUES (1, 'Ayu', 'ayu@example.com'), (2, 'Budi', NULL); \
         INSERT INTO orders VALUES (10, 1);",
    )
    .await;
    exec(
        dst.as_mut(),
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, legacy TEXT); \
         INSERT INTO customers VALUES (1, 'Old name', 'keep me');",
    )
    .await;

    // Tables and columns only: rows are untouched, extra columns are kept.
    let summary = transfer(
        src.as_mut(),
        DatabaseKind::Sqlite,
        dst.as_mut(),
        DatabaseKind::Sqlite,
        sync_options(SyncScope::Structure, false),
    )
    .await;
    let customers = summary
        .iter()
        .find(|s| s.table == "customers")
        .expect("customers");
    let orders = summary
        .iter()
        .find(|s| s.table == "orders")
        .expect("orders");
    assert!(customers.altered && !customers.created, "{summary:?}");
    assert!(orders.created, "{summary:?}");
    assert!(
        summary
            .iter()
            .all(|s| s.inserted + s.updated + s.deleted == 0)
    );
    assert_eq!(
        column_names(dst.as_mut(), None, "customers").await,
        ["id", "name", "email", "legacy"]
    );
    assert_eq!(
        scalar(dst.as_mut(), "SELECT legacy FROM customers WHERE id = 1").await,
        CellValue::Text("keep me".into()),
        "existing rows survive the table rebuild"
    );
    assert_eq!(
        as_int(scalar(dst.as_mut(), "SELECT count(*) FROM orders").await),
        0
    );

    // Tables, columns and data, dropping extra columns.
    let summary = transfer(
        src.as_mut(),
        DatabaseKind::Sqlite,
        dst.as_mut(),
        DatabaseKind::Sqlite,
        sync_options(SyncScope::StructureAndData, true),
    )
    .await;
    let customers = summary
        .iter()
        .find(|s| s.table == "customers")
        .expect("customers");
    assert!(customers.altered, "{summary:?}");
    assert_eq!(
        (customers.inserted, customers.updated),
        (1, 1),
        "{summary:?}"
    );
    assert_eq!(
        column_names(dst.as_mut(), None, "customers").await,
        ["id", "name", "email"]
    );
    assert_eq!(
        scalar(dst.as_mut(), "SELECT name FROM customers WHERE id = 1").await,
        CellValue::Text("Ayu".into())
    );
    assert_eq!(
        as_int(scalar(dst.as_mut(), "SELECT count(*) FROM orders").await),
        1
    );

    // Nothing left to do.
    let again = transfer(
        src.as_mut(),
        DatabaseKind::Sqlite,
        dst.as_mut(),
        DatabaseKind::Sqlite,
        sync_options(SyncScope::StructureAndData, true),
    )
    .await;
    assert!(
        again
            .iter()
            .all(|s| !s.created && !s.altered && s.inserted + s.updated + s.deleted == 0),
        "{again:?}"
    );
}

fn it() -> bool {
    std::env::var("RDBS_IT").as_deref() == Ok("1")
}

/// PostgreSQL test port (`RDBS_PG_PORT`, default as in `dev/docker-compose.yml`).
fn pg_port() -> u16 {
    std::env::var("RDBS_PG_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(55432)
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
            &profile(DatabaseKind::PostgreSql, pg_port(), "postgres", "rdbs_test"),
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
                scope: SyncScope::Data,
                drop_columns: false,
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

/// Structure sync from PostgreSQL into an empty target, then after a column was added on each
/// side; a final sync must find nothing to change (no type or default noise across engines).
async fn structure_sync_round(
    pg: &mut dyn DatabaseAdapter,
    target: &mut dyn DatabaseAdapter,
    kind: DatabaseKind,
    target_schema: Option<&str>,
    players: &str,
) {
    let options = |drop_columns| TransferOptions {
        source_schema: Some("it_sync".into()),
        target_schema: target_schema.map(str::to_owned),
        ..sync_options(SyncScope::StructureAndData, drop_columns)
    };
    let label = format!("{kind:?}");
    let first = transfer(pg, DatabaseKind::PostgreSql, target, kind, options(false)).await;
    assert!(first.iter().all(|s| s.created), "{label}: {first:?}");
    check_target(target, players).await;

    exec(
        pg,
        "ALTER TABLE it_sync.players ADD COLUMN nickname varchar(30)",
    )
    .await;
    exec(
        pg,
        "UPDATE it_sync.players SET nickname = 'Yu' WHERE name = 'Ayu'",
    )
    .await;
    exec(
        target,
        &format!("ALTER TABLE {players} ADD legacy varchar(10)"),
    )
    .await;
    let second = transfer(pg, DatabaseKind::PostgreSql, target, kind, options(false)).await;
    let p = second
        .iter()
        .find(|s| s.table == "players")
        .expect("players");
    assert!(p.altered && p.updated == 1, "{label}: {second:?}");
    let cols = column_names(target, target_schema, "players").await;
    assert!(
        cols.contains(&"nickname".to_owned()) && cols.contains(&"legacy".to_owned()),
        "{label}: {cols:?}"
    );
    assert_eq!(
        scalar(
            target,
            &format!("SELECT nickname FROM {players} WHERE name = 'Ayu'")
        )
        .await,
        CellValue::Text("Yu".into()),
        "{label}"
    );

    let third = transfer(pg, DatabaseKind::PostgreSql, target, kind, options(true)).await;
    let p = third
        .iter()
        .find(|s| s.table == "players")
        .expect("players");
    assert!(p.altered, "{label}: legacy is dropped: {third:?}");
    assert!(
        !column_names(target, target_schema, "players")
            .await
            .contains(&"legacy".to_owned())
    );

    let last = transfer(pg, DatabaseKind::PostgreSql, target, kind, options(true)).await;
    assert!(
        last.iter()
            .all(|s| !s.created && !s.altered && s.inserted + s.updated + s.deleted == 0),
        "{label}: nothing left to do: {last:?}"
    );
}

#[tokio::test]
async fn postgres_structure_sync_to_postgres_mysql_and_mssql() {
    if !it() {
        eprintln!("skipped: set RDBS_IT=1");
        return;
    }

    // PostgreSQL, another schema
    let mut pg = pg_source("it_sync").await;
    let mut dst = postgres().await;
    exec(
        dst.as_mut(),
        "DROP SCHEMA IF EXISTS it_sync_dst CASCADE; CREATE SCHEMA it_sync_dst",
    )
    .await;
    structure_sync_round(
        pg.as_mut(),
        dst.as_mut(),
        DatabaseKind::PostgreSql,
        Some("it_sync_dst"),
        "it_sync_dst.players",
    )
    .await;

    // MySQL
    let mut pg = pg_source("it_sync").await;
    let mut admin = mysql("rdbs_test").await;
    exec(
        admin.as_mut(),
        "DROP DATABASE IF EXISTS rdbs_it_sync; CREATE DATABASE rdbs_it_sync",
    )
    .await;
    let mut my = mysql("rdbs_it_sync").await;
    structure_sync_round(
        pg.as_mut(),
        my.as_mut(),
        DatabaseKind::MySql,
        None,
        "players",
    )
    .await;

    // SQL Server
    let mut pg = pg_source("it_sync").await;
    let mut ms = mssql().await;
    exec(
        ms.as_mut(),
        "IF OBJECT_ID(N'it_sync.players') IS NOT NULL DROP TABLE it_sync.players; \
         IF OBJECT_ID(N'it_sync.teams') IS NOT NULL DROP TABLE it_sync.teams; \
         IF SCHEMA_ID(N'it_sync') IS NOT NULL DROP SCHEMA it_sync;",
    )
    .await;
    exec(ms.as_mut(), "CREATE SCHEMA it_sync").await;
    structure_sync_round(
        pg.as_mut(),
        ms.as_mut(),
        DatabaseKind::SqlServer,
        Some("it_sync"),
        "it_sync.players",
    )
    .await;
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
