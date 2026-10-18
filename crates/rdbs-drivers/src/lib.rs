//! The production [`AdapterFactory`]: picks the adapter crate matching a profile's engine and
//! routes the connection through an SSH tunnel when the profile has one.

use std::path::PathBuf;

use rdbs_adapter_mongodb::MongoFactory;
use rdbs_adapter_mssql::MssqlFactory;
use rdbs_adapter_mysql::MySqlFactory;
use rdbs_adapter_oracle::OracleFactory;
use rdbs_adapter_postgres::PostgresFactory;
use rdbs_adapter_redis::RedisFactory;
use rdbs_adapter_sqlite::SqliteFactory;
use rdbs_core::edit::Change;
use rdbs_core::schema::ObjectPath;
use rdbs_core::{
    AdapterFactory, BoxFuture, CancelHandle, Capabilities, ConnectionProfile, DatabaseAdapter,
    DatabaseKind, DbError, DbObject, QueryOutcome,
};
use rdbs_ssh::Tunnel;

/// Connects to real servers — PostgreSQL, MySQL/MariaDB, SQL Server, Oracle, MongoDB and
/// Redis (optionally over SSH) — and to SQLite files.
/// The SSH secret travels in [`rdbs_core::SshTunnel::secret`], filled in by the worker.
pub struct LiveFactory {
    known_hosts: PathBuf,
}

impl LiveFactory {
    /// `known_hosts` records trusted SSH host keys.
    pub fn new(known_hosts: PathBuf) -> Self {
        Self { known_hosts }
    }
}

/// Connects directly (no tunnel) with the engine's adapter.
fn direct<'a>(
    profile: &'a ConnectionProfile,
    password: Option<&'a str>,
) -> BoxFuture<'a, Result<Box<dyn DatabaseAdapter>, DbError>> {
    match profile.kind {
        DatabaseKind::PostgreSql => PostgresFactory.connect(profile, password),
        DatabaseKind::MySql | DatabaseKind::MariaDb => MySqlFactory.connect(profile, password),
        DatabaseKind::SqlServer => MssqlFactory.connect(profile, password),
        DatabaseKind::Sqlite => SqliteFactory.connect(profile, password),
        DatabaseKind::Oracle => OracleFactory.connect(profile, password),
        DatabaseKind::MongoDb => MongoFactory.connect(profile, password),
        DatabaseKind::Redis => RedisFactory.connect(profile, password),
    }
}

impl AdapterFactory for LiveFactory {
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn DatabaseAdapter>, DbError>> {
        Box::pin(async move {
            let Some(ssh) = &profile.ssh else {
                return direct(profile, password).await;
            };
            let tunnel = rdbs_ssh::open(
                ssh,
                ssh.secret.as_deref(),
                &profile.host,
                profile.port,
                &self.known_hosts,
            )
            .await?;
            // The database is reached through the local end of the tunnel.
            let mut local = profile.clone();
            local.host = "127.0.0.1".to_owned();
            local.port = tunnel.local_port();
            local.ssh = None;
            let inner = direct(&local, password).await?;
            let version = format!("{} (via SSH {})", inner.server_version(), ssh.host);
            let adapter: Box<dyn DatabaseAdapter> = Box::new(Tunneled {
                inner,
                version,
                _tunnel: tunnel,
            });
            Ok(adapter)
        })
    }
}

/// An adapter whose connection runs through a tunnel that lives as long as it does.
struct Tunneled {
    inner: Box<dyn DatabaseAdapter>,
    version: String,
    _tunnel: Tunnel,
}

impl DatabaseAdapter for Tunneled {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn server_version(&self) -> &str {
        &self.version
    }

    fn load_objects(&mut self) -> BoxFuture<'_, Result<Vec<DbObject>, DbError>> {
        self.inner.load_objects()
    }

    fn execute<'a>(
        &'a mut self,
        sql: &'a str,
        cancel: CancelHandle,
    ) -> BoxFuture<'a, Result<QueryOutcome, DbError>> {
        self.inner.execute(sql, cancel)
    }

    fn apply_changes<'a>(
        &'a mut self,
        changes: &'a [Change],
    ) -> BoxFuture<'a, Result<u64, DbError>> {
        self.inner.apply_changes(changes)
    }

    fn object_ddl<'a>(
        &'a mut self,
        object: &'a ObjectPath,
    ) -> BoxFuture<'a, Result<String, DbError>> {
        self.inner.object_ddl(object)
    }

    fn describe_schema<'a>(
        &'a mut self,
        schema: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<rdbs_core::TableSchema>, DbError>> {
        self.inner.describe_schema(schema)
    }

    fn explain<'a>(
        &'a mut self,
        sql: &'a str,
        analyze: bool,
    ) -> BoxFuture<'a, Result<rdbs_core::plan::ExplainPlan, DbError>> {
        self.inner.explain(sql, analyze)
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdbs_core::DbErrorKind;

    fn factory() -> LiveFactory {
        LiveFactory::new(std::env::temp_dir().join("rdbs-test-known_hosts"))
    }

    #[tokio::test]
    async fn sqlite_routes_to_the_file_adapter() {
        let mut profile = ConnectionProfile::new(DatabaseKind::Sqlite);
        profile.host = ":memory:".into();
        let adapter = factory().connect(&profile, None).await.unwrap();
        assert!(adapter.server_version().starts_with("SQLite 3."));
        profile.host = "Z:/definitely/missing.sqlite".into();
        let err = factory().connect(&profile, None).await.err().unwrap();
        assert_eq!(err.kind, DbErrorKind::Connection);
    }

    #[tokio::test]
    async fn unreachable_server_fails_with_connection_error() {
        // Port 1 on localhost is closed on any sane machine; this exercises routing to the
        // PostgreSQL adapter and its error mapping without needing a server.
        let mut profile = ConnectionProfile::new(DatabaseKind::PostgreSql);
        profile.host = "127.0.0.1".into();
        profile.port = 1;
        profile.ssl_mode = rdbs_core::SslMode::Disable;
        let err = factory().connect(&profile, None).await.err().unwrap();
        assert_eq!(err.kind, DbErrorKind::Connection, "{err}");
    }

    #[tokio::test]
    async fn unreachable_ssh_server_fails_before_database_connect() {
        let mut profile = ConnectionProfile::new(DatabaseKind::PostgreSql);
        profile.host = "db.internal".into();
        profile.ssh = Some(rdbs_core::SshTunnel {
            host: "127.0.0.1".into(),
            port: 1,
            username: "nobody".into(),
            ..rdbs_core::SshTunnel::default()
        });
        let err = factory().connect(&profile, None).await.err().unwrap();
        assert_eq!(err.kind, DbErrorKind::Connection, "{err}");
        assert!(
            err.message
                .starts_with("SSH connection to 127.0.0.1:1 failed"),
            "{err}"
        );
    }
}
