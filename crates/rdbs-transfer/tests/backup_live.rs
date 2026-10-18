//! Backup → restore round trips. SQLite always runs; server engines need `RDBS_IT=1`.

#![allow(clippy::expect_used)]

use std::path::PathBuf;

use rdbs_adapter_mssql::MssqlFactory;
use rdbs_adapter_mysql::MySqlFactory;
use rdbs_adapter_postgres::PostgresFactory;
use rdbs_adapter_sqlite::SqliteAdapter;
use rdbs_core::job::BackupOptions;
use rdbs_core::{
    AdapterFactory, CancelHandle, CellValue, ConnectionProfile, DatabaseAdapter, DatabaseKind,
    SslMode,
};
use rdbs_transfer::backup::dump;

async fn exec(s: &mut dyn DatabaseAdapter, sql: &str) {
    s.execute(sql, CancelHandle::new())
        .await
        .unwrap_or_else(|e| panic!("{}\n→ {e}", &sql[..sql.len().min(400)]));
}

async fn count(s: &mut dyn DatabaseAdapter, table: &str) -> i64 {
    let out = s
        .execute(
            &format!("SELECT count(*) FROM {table}"),
            CancelHandle::new(),
        )
        .await
        .expect("count");
    match &out.results[0].rows[0][0] {
        CellValue::Int(i) => *i,
        other => panic!("{other:?}"),
    }
}

fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rdbs-backup-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn backup(
    s: &mut dyn DatabaseAdapter,
    kind: DatabaseKind,
    schema: Option<&str>,
    dir: &std::path::Path,
) -> String {
    let options = BackupOptions {
        schema: schema.map(str::to_owned),
        path: dir.join("db_{date}_{time}.sql"),
        keep: 3,
        ..BackupOptions::default()
    };
    let (path, summary) = dump(s, kind, &options, &CancelHandle::new(), |_| {})
        .await
        .expect("dump");
    assert!(summary.contains("table(s)"), "{summary}");
    std::fs::read_to_string(path).expect("read dump")
}

#[tokio::test]
async fn sqlite_backup_restores_into_an_empty_file() {
    let dir = out_dir("sqlite");
    std::fs::create_dir_all(&dir).expect("dir");
    let src_path = dir.join("src.sqlite");
    let dst_path = dir.join("dst.sqlite");
    std::fs::write(&src_path, b"").expect("src");
    std::fs::write(&dst_path, b"").expect("dst");
    let mut src = SqliteAdapter::open(&src_path.display().to_string())
        .await
        .expect("src");
    exec(
        &mut src,
        "CREATE TABLE a (id INTEGER PRIMARY KEY, label TEXT NOT NULL DEFAULT 'x', data BLOB); \
         CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), v REAL); \
         INSERT INTO a (label, data) VALUES ('one', X'0102'), ('two', NULL); \
         INSERT INTO b (a_id, v) VALUES (1, 1.5), (2, NULL), (1, -3);",
    )
    .await;
    let script = backup(&mut src, DatabaseKind::Sqlite, None, &dir).await;
    let mut dst = SqliteAdapter::open(&dst_path.display().to_string())
        .await
        .expect("dst");
    exec(&mut dst, &script).await;
    assert_eq!(count(&mut dst, "a").await, 2);
    assert_eq!(count(&mut dst, "b").await, 3);
    let restored = dst.describe_schema(None).await.expect("describe");
    let original = src.describe_schema(None).await.expect("describe");
    assert_eq!(restored, original, "the restored schema is identical");
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

/// Two related tables in schema `{s}`.
fn setup(schema_prefix: &str, text_prefix: &str) -> String {
    format!(
        "CREATE TABLE {s}teams (id int PRIMARY KEY, name varchar(40) NOT NULL); \
         CREATE TABLE {s}players (id int PRIMARY KEY, team_id int REFERENCES {s}teams(id), \
             name varchar(50) NOT NULL, rating decimal(5,2)); \
         INSERT INTO {s}teams VALUES (1, {n}'Garuda'), (2, {n}'Komodo'); \
         INSERT INTO {s}players VALUES (1, 1, {n}'Ayu', 9.25), (2, 2, {n}'Budi', NULL), \
             (3, 1, {n}'Citra', 7.5);",
        s = schema_prefix,
        n = text_prefix
    )
}

#[tokio::test]
async fn postgres_backup_restores() {
    if !it() {
        return;
    }
    let mut pg = PostgresFactory
        .connect(
            &profile(DatabaseKind::PostgreSql, 55432, "postgres", "rdbs_test"),
            Some("rdbs_test_pw"),
        )
        .await
        .expect("pg");
    exec(
        pg.as_mut(),
        &format!(
            "DROP SCHEMA IF EXISTS it_bk CASCADE; CREATE SCHEMA it_bk; {}",
            setup("it_bk.", "")
        ),
    )
    .await;
    let dir = out_dir("pg");
    let script = backup(pg.as_mut(), DatabaseKind::PostgreSql, Some("it_bk"), &dir).await;
    let before = pg.describe_schema(Some("it_bk")).await.expect("describe");
    exec(
        pg.as_mut(),
        "DROP SCHEMA it_bk CASCADE; CREATE SCHEMA it_bk",
    )
    .await;
    exec(pg.as_mut(), &script).await;
    assert_eq!(count(pg.as_mut(), "it_bk.players").await, 3);
    assert_eq!(
        pg.describe_schema(Some("it_bk")).await.expect("describe"),
        before
    );
}

#[tokio::test]
async fn mysql_backup_restores() {
    if !it() {
        return;
    }
    let mut admin = MySqlFactory
        .connect(
            &profile(DatabaseKind::MySql, 53306, "root", "rdbs_test"),
            Some("rdbs_test_pw"),
        )
        .await
        .expect("mysql");
    exec(
        admin.as_mut(),
        "DROP DATABASE IF EXISTS rdbs_it_bk; CREATE DATABASE rdbs_it_bk",
    )
    .await;
    let mut my = MySqlFactory
        .connect(
            &profile(DatabaseKind::MySql, 53306, "root", "rdbs_it_bk"),
            Some("rdbs_test_pw"),
        )
        .await
        .expect("mysql");
    exec(my.as_mut(), &setup("", "")).await;
    let dir = out_dir("mysql");
    let script = backup(my.as_mut(), DatabaseKind::MySql, None, &dir).await;
    let before = my.describe_schema(None).await.expect("describe");
    exec(my.as_mut(), "DROP TABLE players; DROP TABLE teams").await;
    exec(my.as_mut(), &script).await;
    assert_eq!(count(my.as_mut(), "players").await, 3);
    assert_eq!(my.describe_schema(None).await.expect("describe"), before);
}

#[tokio::test]
async fn mssql_backup_restores() {
    if !it() {
        return;
    }
    let mut ms = MssqlFactory
        .connect(
            &profile(DatabaseKind::SqlServer, 51433, "sa", "master"),
            Some("Rdbs_Test_Pw_2026!"),
        )
        .await
        .expect("mssql");
    let drop = "IF OBJECT_ID(N'it_bk.players') IS NOT NULL DROP TABLE it_bk.players; \
                IF OBJECT_ID(N'it_bk.teams') IS NOT NULL DROP TABLE it_bk.teams;";
    exec(
        ms.as_mut(),
        &format!("{drop} IF SCHEMA_ID(N'it_bk') IS NULL EXEC(N'CREATE SCHEMA it_bk')"),
    )
    .await;
    exec(ms.as_mut(), &setup("it_bk.", "N")).await;
    let dir = out_dir("mssql");
    let script = backup(ms.as_mut(), DatabaseKind::SqlServer, Some("it_bk"), &dir).await;
    let before = ms.describe_schema(Some("it_bk")).await.expect("describe");
    exec(ms.as_mut(), drop).await;
    exec(ms.as_mut(), &script).await;
    assert_eq!(count(ms.as_mut(), "it_bk.players").await, 3);
    let after = ms.describe_schema(Some("it_bk")).await.expect("describe");
    // System-generated constraint names differ after a restore; compare the rest.
    let strip = |mut v: Vec<rdbs_core::TableSchema>| {
        for t in &mut v {
            t.primary_key_name = None;
            for fk in &mut t.foreign_keys {
                fk.name.clear();
            }
        }
        v
    };
    assert_eq!(strip(after), strip(before));
}
