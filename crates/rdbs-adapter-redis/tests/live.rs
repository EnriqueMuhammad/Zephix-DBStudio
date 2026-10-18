//! Integration tests against a real Redis server (see `dev/docker-compose.yml`).
//! Skipped unless `RDBS_IT=1`.

#![allow(clippy::expect_used)]

use rdbs_adapter_redis::RedisFactory;
use rdbs_core::{
    AdapterFactory, CancelHandle, CellValue, ConnectionProfile, DatabaseAdapter, DatabaseKind,
    DbErrorKind, SslMode,
};

fn profile(db: &str) -> Option<ConnectionProfile> {
    if std::env::var("RDBS_IT").as_deref() != Ok("1") {
        eprintln!("skipped: set RDBS_IT=1 to run Redis integration tests");
        return None;
    }
    let mut p = ConnectionProfile::new(DatabaseKind::Redis);
    p.host = "127.0.0.1".into();
    p.port = 56379;
    p.database = db.into();
    p.ssl_mode = SslMode::Disable;
    Some(p)
}

async fn session(db: &str) -> Option<Box<dyn DatabaseAdapter>> {
    let p = profile(db)?;
    Some(
        RedisFactory
            .connect(&p, Some("rdbs_test_pw"))
            .await
            .expect("connect"),
    )
}

#[tokio::test]
async fn commands_inspect_and_tree() {
    let Some(mut s) = session("db3").await else {
        return;
    };
    assert!(
        s.server_version().starts_with("Redis 7"),
        "{}",
        s.server_version()
    );
    let out = s
        .execute(
            "FLUSHDB\n# comment\nSET \"user:1:name\" \"Ayu S\"\nHSET user:1 name Ayu city Jakarta\n\
             RPUSH queue a b c\nZADD scores 10 ayu 20 budi\nGET \"user:1:name\"\nHGETALL user:1",
            CancelHandle::new(),
        )
        .await
        .expect("script");
    let get = &out.results[out.results.len() - 2];
    assert_eq!(get.rows[0][0], CellValue::Text("Ayu S".into()));
    let hash = out.results.last().expect("hash");
    assert_eq!(hash.columns[0].name, "field");
    assert_eq!(hash.rows.len(), 2);

    let out = s
        .execute("INSPECT scores", CancelHandle::new())
        .await
        .expect("inspect");
    assert_eq!(out.results[0].columns[1].name, "score");
    assert_eq!(out.results[0].rows[1][0], CellValue::Text("budi".into()));
    assert!(out.messages[0].contains("zset"), "{:?}", out.messages);
    let out = s
        .execute("INSPECT queue", CancelHandle::new())
        .await
        .expect("inspect");
    assert_eq!(out.results[0].rows.len(), 3);

    let tree = s.load_objects().await.expect("tree");
    let db3 = tree.iter().find(|d| d.name == "db3").expect("db3");
    let keys = &db3.children[0].children;
    assert_eq!(
        keys.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
        ["queue", "scores", "user:1", "user:1:name"]
    );
}

#[tokio::test]
async fn errors_and_auth() {
    let Some(mut s) = session("4").await else {
        return;
    };
    let err = s
        .execute("NOSUCHCOMMAND x", CancelHandle::new())
        .await
        .expect_err("unknown command");
    assert_eq!(err.kind, DbErrorKind::Query);
    let p = profile("0").expect("profile");
    let err = RedisFactory
        .connect(&p, Some("wrong"))
        .await
        .err()
        .expect("bad password");
    assert_eq!(err.kind, DbErrorKind::Authentication, "{err:?}");
}
