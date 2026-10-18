//! SQLite tests on real database files (no server needed, always run).

#![allow(clippy::expect_used)]

use std::path::PathBuf;

use rdbs_adapter_sqlite::SqliteFactory;
use rdbs_core::schema::ObjectPath;
use rdbs_core::{
    AdapterFactory, CancelHandle, ConnectionProfile, DatabaseAdapter, DatabaseKind, ObjectKind,
};

#[path = "../../rdbs-core/testkit/schema_roundtrip.rs"]
mod roundtrip;

/// A fresh, empty database file in the temp directory.
fn fresh_file(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("rdbs-it-{name}-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    rusqlite_create(&path);
    path
}

/// Creates an empty SQLite file (the adapter refuses to create files itself).
fn rusqlite_create(path: &std::path::Path) {
    std::fs::write(path, b"").expect("create file");
}

async fn open(path: &std::path::Path) -> Box<dyn DatabaseAdapter> {
    let mut profile = ConnectionProfile::new(DatabaseKind::Sqlite);
    profile.host = path.display().to_string();
    SqliteFactory.connect(&profile, None).await.expect("open")
}

#[tokio::test]
async fn describe_create_alter_round_trip() {
    let a = fresh_file("rt-a");
    let b = fresh_file("rt-b");
    let mut s = open(&a).await;
    let mut copy = open(&b).await;
    roundtrip::schema_round_trip(
        DatabaseKind::Sqlite,
        s.as_mut(),
        None,
        copy.as_mut(),
        None,
        roundtrip::Types {
            int: "INTEGER",
            bigint: "BIGINT",
            varchar: "VARCHAR(50)",
            text: "TEXT",
        },
    )
    .await;
}

#[tokio::test]
async fn object_tree_and_ddl() {
    let path = fresh_file("tree");
    let mut s = open(&path).await;
    s.execute(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL); \
         CREATE INDEX items_name ON items (name); \
         CREATE VIEW item_names AS SELECT name FROM items;",
        CancelHandle::new(),
    )
    .await
    .expect("setup");
    let tree = s.load_objects().await.expect("tree");
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].name, "main");
    let tables = &tree[0].children[0];
    assert_eq!(tables.name, "Tables");
    assert_eq!(tables.children[0].name, "items");
    assert_eq!(
        tables.children[0].children[0].detail.as_deref(),
        Some("INTEGER · PK")
    );
    assert_eq!(
        tables.children[0].children[1].detail.as_deref(),
        Some("TEXT · NOT NULL")
    );
    assert_eq!(tree[0].children[1].children[0].name, "item_names");

    let ddl = s
        .object_ddl(&ObjectPath {
            database: Some("main".into()),
            schema: None,
            name: "items".into(),
            kind: ObjectKind::Table,
        })
        .await
        .expect("ddl");
    assert!(ddl.starts_with("CREATE TABLE items"), "{ddl}");
    assert!(
        ddl.contains("CREATE INDEX items_name ON items (name);"),
        "{ddl}"
    );
}

#[tokio::test]
async fn explain_query_plan() {
    let path = fresh_file("explain");
    let mut s = open(&path).await;
    s.execute(
        "CREATE TABLE a (id INTEGER PRIMARY KEY, b_id INTEGER); CREATE TABLE b (id INTEGER PRIMARY KEY);",
        CancelHandle::new(),
    )
    .await
    .expect("setup");
    let plan = s
        .explain(
            "SELECT * FROM a JOIN b ON b.id = a.b_id ORDER BY a.b_id",
            false,
        )
        .await
        .expect("plan");
    let objects: Vec<_> = plan.roots.iter().filter_map(|n| n.object.clone()).collect();
    assert!(objects.iter().any(|o| o.starts_with('a')), "{plan:?}");
    assert!(objects.iter().any(|o| o.starts_with('b')), "{plan:?}");
}
