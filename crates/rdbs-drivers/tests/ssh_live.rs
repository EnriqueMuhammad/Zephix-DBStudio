//! SSH tunnel integration tests against `dev/docker-compose.yml` (services `ssh`, `postgres`,
//! `mysql`). Skipped unless `RDBS_IT=1`.

// Test helpers panic on purpose: a failing setup should fail the test loudly.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use rdbs_core::{
    AdapterFactory, CancelHandle, CellValue, ConnectionProfile, DatabaseKind, DbErrorKind,
    ProfileId, SshAuth, SshTunnel, SslMode,
};
use rdbs_drivers::LiveFactory;

fn enabled() -> bool {
    let on = std::env::var("RDBS_IT").as_deref() == Ok("1");
    if !on {
        eprintln!("skipped: set RDBS_IT=1 to run SSH tunnel tests");
    }
    on
}

const PW: &str = "rdbs_tunnel_pw";

fn repo_file(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// The bastion's client key pair, generated per machine (see `dev/ssh/README.md`).
fn test_key(file: &str) -> PathBuf {
    let path = repo_file(&format!("dev/ssh/{file}"));
    assert!(
        path.exists(),
        "{} is missing: create the test key with `ssh-keygen -t ed25519 -N \"\" -C rdbs-it -f dev/ssh/test_key`, \
         then rebuild the bastion (`docker compose -f dev/docker-compose.yml up -d --build ssh`)",
        path.display()
    );
    path
}

/// Fresh known_hosts file per test.
fn known_hosts(name: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("rdbs-it-known_hosts-{name}-{}", ProfileId::new()));
    let _ = std::fs::remove_file(&path);
    path
}

fn bastion(auth: SshAuth, secret: Option<&str>) -> SshTunnel {
    SshTunnel {
        host: "127.0.0.1".into(),
        port: 52222,
        username: "tunnel".into(),
        auth,
        secret: secret.map(str::to_owned),
    }
}

fn postgres_via(ssh: SshTunnel) -> ConnectionProfile {
    let mut p = ConnectionProfile::new(DatabaseKind::PostgreSql);
    p.name = "pg over ssh".into();
    // Resolved by the SSH server inside the compose network.
    p.host = "postgres".into();
    p.port = 5432;
    p.database = "rdbs_test".into();
    p.username = "postgres".into();
    p.ssl_mode = SslMode::Disable;
    p.ssh = Some(ssh);
    p
}

fn factory(hosts: PathBuf) -> LiveFactory {
    LiveFactory::new(hosts)
}

#[tokio::test]
async fn password_tunnel_reaches_postgres_and_records_host_key() {
    if !enabled() {
        return;
    }
    let hosts = known_hosts("pw");
    let profile = postgres_via(bastion(SshAuth::Password, Some(PW)));
    let f = factory(hosts.clone());
    let mut session = f
        .connect(&profile, Some("rdbs_test_pw"))
        .await
        .expect("connect through tunnel");
    assert!(
        session.server_version().contains("via SSH 127.0.0.1"),
        "{}",
        session.server_version()
    );
    let out = session
        .execute("SELECT current_database()", CancelHandle::new())
        .await
        .expect("query");
    assert_eq!(
        out.last_result().unwrap().rows[0][0],
        CellValue::Text("rdbs_test".into())
    );
    let recorded = std::fs::read_to_string(&hosts).expect("known_hosts written");
    assert!(recorded.contains("[127.0.0.1]:52222"), "{recorded}");

    // A second connection reuses the recorded key.
    f.connect(&profile, Some("rdbs_test_pw"))
        .await
        .expect("reconnect with known key");
}

#[tokio::test]
async fn private_key_tunnel_reaches_mysql() {
    if !enabled() {
        return;
    }
    let key = test_key("test_key");
    let mut profile = ConnectionProfile::new(DatabaseKind::MySql);
    profile.name = "mysql over ssh".into();
    profile.host = "mysql".into();
    profile.port = 3306;
    profile.database = "rdbs_test".into();
    profile.username = "root".into();
    profile.ssl_mode = SslMode::Disable;
    profile.ssh = Some(bastion(
        SshAuth::PrivateKey {
            path: key.to_string_lossy().into_owned(),
        },
        None,
    ));
    let f = factory(known_hosts("key"));
    let mut session = f
        .connect(&profile, Some("rdbs_test_pw"))
        .await
        .expect("connect with key");
    let out = session
        .execute("SELECT DATABASE()", CancelHandle::new())
        .await
        .expect("query");
    assert_eq!(
        out.last_result().unwrap().rows[0][0],
        CellValue::Text("rdbs_test".into())
    );
}

#[tokio::test]
async fn wrong_ssh_password_is_an_authentication_error() {
    if !enabled() {
        return;
    }
    let profile = postgres_via(bastion(SshAuth::Password, Some("wrong")));
    let f = factory(known_hosts("bad"));
    let err = f
        .connect(&profile, Some("rdbs_test_pw"))
        .await
        .err()
        .expect("auth error");
    assert_eq!(err.kind, DbErrorKind::Authentication, "{err}");
}

#[tokio::test]
async fn changed_host_key_is_refused() {
    if !enabled() {
        return;
    }
    // Record a different ed25519 key (the test client key) for the bastion.
    let hosts = known_hosts("changed");
    let other_key = std::fs::read_to_string(test_key("test_key.pub")).unwrap();
    let fields: Vec<&str> = other_key.split_whitespace().collect();
    std::fs::write(
        &hosts,
        format!("[127.0.0.1]:52222 {} {}\n", fields[0], fields[1]),
    )
    .unwrap();
    let profile = postgres_via(bastion(SshAuth::Password, Some(PW)));
    let f = factory(hosts);
    let err = f
        .connect(&profile, Some("rdbs_test_pw"))
        .await
        .err()
        .expect("refused");
    assert!(err.message.contains("host key"), "{err}");
}

#[tokio::test]
async fn unreachable_target_is_reported_by_the_probe() {
    if !enabled() {
        return;
    }
    let mut profile = postgres_via(bastion(SshAuth::Password, Some(PW)));
    profile.host = "nowhere.invalid".into();
    let f = factory(known_hosts("probe"));
    let err = f
        .connect(&profile, Some("rdbs_test_pw"))
        .await
        .err()
        .expect("probe error");
    assert!(
        err.message
            .contains("could not open a tunnel to nowhere.invalid:5432"),
        "{err}"
    );
}
