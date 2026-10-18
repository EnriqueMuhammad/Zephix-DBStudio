//! Integration tests against a real MongoDB server (see `dev/docker-compose.yml`).
//! Skipped unless `RDBS_IT=1`.

#![allow(clippy::expect_used)]

use rdbs_adapter_mongodb::MongoFactory;
use rdbs_core::{
    AdapterFactory, CancelHandle, CellValue, ConnectionProfile, DatabaseAdapter, DatabaseKind,
    DbErrorKind, SslMode,
};

fn profile() -> Option<ConnectionProfile> {
    if std::env::var("RDBS_IT").as_deref() != Ok("1") {
        eprintln!("skipped: set RDBS_IT=1 to run MongoDB integration tests");
        return None;
    }
    let mut p = ConnectionProfile::new(DatabaseKind::MongoDb);
    p.host = "127.0.0.1".into();
    p.port = 57017;
    p.username = "root".into();
    p.database = "rdbs_it".into();
    p.ssl_mode = SslMode::Disable;
    Some(p)
}

async fn session() -> Option<Box<dyn DatabaseAdapter>> {
    let p = profile()?;
    Some(
        MongoFactory
            .connect(&p, Some("rdbs_test_pw"))
            .await
            .expect("connect"),
    )
}

#[tokio::test]
async fn crud_aggregate_and_tree() {
    let Some(mut s) = session().await else { return };
    assert!(
        s.server_version().starts_with("MongoDB 8"),
        "{}",
        s.server_version()
    );
    let out = s
        .execute(
            "db.people.drop()\n\
             db.people.insertMany([{_id: 1, name: 'Ayu', city: 'Jakarta', score: 9.5},\n\
                                   {_id: 2, name: 'Budi', city: 'Bandung', score: 7},\n\
                                   {_id: 3, name: \"Citra\", city: 'Jakarta', tags: ['x']}])\n\
             db.people.updateOne({_id: 2}, {$set: {score: 8}})\n\
             db.people.find({city: 'Jakarta'}).sort({_id: -1}).limit(5)",
            CancelHandle::new(),
        )
        .await
        .expect("script");
    let rs = out.results.last().expect("find");
    assert_eq!(rs.rows.len(), 2);
    assert_eq!(rs.rows[0][1], CellValue::Text("Citra".into()));
    assert!(
        out.messages
            .iter()
            .any(|m| m.contains("matched 1, modified 1")),
        "{:?}",
        out.messages
    );

    let out = s
        .execute(
            "db.people.aggregate([{$group: {_id: '$city', n: {$sum: 1}}}, {$sort: {_id: 1}}])",
            CancelHandle::new(),
        )
        .await
        .expect("aggregate");
    assert_eq!(
        out.results[0].rows[1],
        vec![CellValue::Text("Jakarta".into()), CellValue::Int(2)]
    );

    let out = s
        .execute(
            "db.people.countDocuments({score: {$gte: 8}})",
            CancelHandle::new(),
        )
        .await
        .expect("count");
    assert_eq!(out.results[0].rows[0][0], CellValue::Int(2));

    let tree = s.load_objects().await.expect("tree");
    let db = tree.iter().find(|d| d.name == "rdbs_it").expect("db");
    let people = &db.children[0]
        .children
        .iter()
        .find(|c| c.name == "people")
        .expect("people");
    assert!(people.children.iter().any(|c| c.name == "city"));
}

#[tokio::test]
async fn errors_are_reported() {
    let Some(mut s) = session().await else { return };
    let err = s
        .execute("db.x.find({a: ", CancelHandle::new())
        .await
        .expect_err("syntax");
    assert_eq!(err.kind, DbErrorKind::Query);
    let err = s
        .execute("db.runCommand({noSuchCommand: 1})", CancelHandle::new())
        .await
        .expect_err("unknown command");
    assert_eq!(err.code.as_deref(), Some("59"), "{err:?}");
    let p = profile().expect("profile");
    let err = MongoFactory
        .connect(&p, Some("wrong"))
        .await
        .err()
        .expect("bad password");
    assert_eq!(err.kind, DbErrorKind::Authentication, "{err:?}");
}
