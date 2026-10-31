//! Connection profiles: everything needed to open a session, except secrets.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier of a connection profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProfileId(pub Uuid);

impl ProfileId {
    /// Creates a new random identifier.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// A fixed identifier (built-in demo connections keep theirs across runs).
    pub const fn from_u128(value: u128) -> Self {
        Self(Uuid::from_u128(value))
    }
}

impl Default for ProfileId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Supported database engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DatabaseKind {
    /// Microsoft SQL Server / Azure SQL.
    SqlServer,
    /// PostgreSQL.
    PostgreSql,
    /// MySQL.
    MySql,
    /// MariaDB (MySQL wire protocol).
    MariaDb,
    /// SQLite database file.
    Sqlite,
    /// Oracle Database (needs Oracle Instant Client at run time).
    Oracle,
    /// MongoDB (document database; queries use shell syntax).
    MongoDb,
    /// Redis (key-value store; queries are Redis commands).
    Redis,
}

impl DatabaseKind {
    /// All engines, in the order they are offered in the UI.
    pub const ALL: [DatabaseKind; 8] = [
        DatabaseKind::PostgreSql,
        DatabaseKind::MySql,
        DatabaseKind::SqlServer,
        DatabaseKind::MariaDb,
        DatabaseKind::Sqlite,
        DatabaseKind::Oracle,
        DatabaseKind::MongoDb,
        DatabaseKind::Redis,
    ];

    /// Human readable engine name.
    pub fn display_name(self) -> &'static str {
        match self {
            DatabaseKind::SqlServer => "SQL Server",
            DatabaseKind::PostgreSql => "PostgreSQL",
            DatabaseKind::MySql => "MySQL",
            DatabaseKind::MariaDb => "MariaDB",
            DatabaseKind::Sqlite => "SQLite",
            DatabaseKind::Oracle => "Oracle",
            DatabaseKind::MongoDb => "MongoDB",
            DatabaseKind::Redis => "Redis",
        }
    }

    /// Two-letter badge used by the UI in place of vendor logos.
    pub fn badge(self) -> &'static str {
        match self {
            DatabaseKind::SqlServer => "MS",
            DatabaseKind::PostgreSql => "PG",
            DatabaseKind::MySql => "MY",
            DatabaseKind::MariaDb => "MA",
            DatabaseKind::Sqlite => "SL",
            DatabaseKind::Oracle => "OR",
            DatabaseKind::MongoDb => "MG",
            DatabaseKind::Redis => "RD",
        }
    }

    /// Default TCP port, or `None` for file-based engines.
    pub fn default_port(self) -> Option<u16> {
        match self {
            DatabaseKind::SqlServer => Some(1433),
            DatabaseKind::PostgreSql => Some(5432),
            DatabaseKind::MySql | DatabaseKind::MariaDb => Some(3306),
            DatabaseKind::Sqlite => None,
            DatabaseKind::Oracle => Some(1521),
            DatabaseKind::MongoDb => Some(27017),
            DatabaseKind::Redis => Some(6379),
        }
    }

    /// Conventional administrator login, used to pre-fill new profiles.
    pub fn default_username(self) -> &'static str {
        match self {
            DatabaseKind::SqlServer => "sa",
            DatabaseKind::PostgreSql => "postgres",
            DatabaseKind::MySql | DatabaseKind::MariaDb => "root",
            DatabaseKind::Oracle => "system",
            DatabaseKind::Sqlite | DatabaseKind::MongoDb | DatabaseKind::Redis => "",
        }
    }

    /// Whether the engine is reached over the network (host/port/user).
    pub fn is_networked(self) -> bool {
        self.default_port().is_some()
    }

    /// Whether the engine has schemas inside a database (as opposed to MySQL, where
    /// "schema" and "database" are synonyms).
    pub fn has_schemas(self) -> bool {
        matches!(
            self,
            DatabaseKind::SqlServer | DatabaseKind::PostgreSql | DatabaseKind::Oracle
        )
    }

    /// Whether the engine speaks SQL (table designer, diff, transfer, builder, backups).
    pub fn is_sql(self) -> bool {
        !matches!(self, DatabaseKind::MongoDb | DatabaseKind::Redis)
    }

    /// Whether a user name is required to connect.
    pub fn requires_username(self) -> bool {
        self.is_networked() && self.is_sql()
    }
}

impl fmt::Display for DatabaseKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// Deployment environment of a connection; drives production safeguards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Environment {
    /// Local or development database.
    #[default]
    Development,
    /// Shared test/staging database.
    Staging,
    /// Production database: destructive statements require confirmation.
    Production,
}

impl Environment {
    /// All environments in display order.
    pub const ALL: [Environment; 3] = [
        Environment::Development,
        Environment::Staging,
        Environment::Production,
    ];

    /// Short badge text.
    pub fn short(self) -> &'static str {
        match self {
            Environment::Development => "DEV",
            Environment::Staging => "STG",
            Environment::Production => "PROD",
        }
    }

    /// Human readable label.
    pub fn label(self) -> &'static str {
        match self {
            Environment::Development => "Development",
            Environment::Staging => "Staging",
            Environment::Production => "Production",
        }
    }

    /// Whether non-read statements must be confirmed before running.
    pub fn requires_write_confirmation(self) -> bool {
        matches!(self, Environment::Production)
    }
}

/// Transport security requested for the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum SslMode {
    /// Never use TLS.
    Disable,
    /// Use TLS when the server supports it.
    #[default]
    Prefer,
    /// Require TLS but do not verify the certificate.
    Require,
    /// Require TLS and verify the server certificate chain and host name.
    VerifyFull,
}

impl SslMode {
    /// All modes in display order.
    pub const ALL: [SslMode; 4] = [
        SslMode::Disable,
        SslMode::Prefer,
        SslMode::Require,
        SslMode::VerifyFull,
    ];

    /// Human readable label.
    pub fn label(self) -> &'static str {
        match self {
            SslMode::Disable => "Disable",
            SslMode::Prefer => "Prefer",
            SslMode::Require => "Require",
            SslMode::VerifyFull => "Verify full",
        }
    }
}

/// How to authenticate to the SSH server of a tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SshAuth {
    /// Password (stored in the OS keychain).
    #[default]
    Password,
    /// Private key file; its passphrase (if any) is stored in the OS keychain.
    PrivateKey {
        /// Path to an OpenSSH/PEM private key.
        path: String,
    },
}

/// SSH tunnel the database connection goes through (local port forwarding).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshTunnel {
    /// SSH server (bastion) host.
    pub host: String,
    /// SSH port.
    pub port: u16,
    /// SSH user.
    pub username: String,
    /// Authentication method.
    pub auth: SshAuth,
    /// SSH password or key passphrase for the next connection. Never persisted: filled in
    /// from the dialog or the keychain right before connecting.
    #[serde(skip)]
    pub secret: Option<String>,
}

impl Default for SshTunnel {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 22,
            username: String::new(),
            auth: SshAuth::default(),
            secret: None,
        }
    }
}

impl std::fmt::Debug for SshTunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshTunnel")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("auth", &self.auth)
            .field("secret", &self.secret.as_ref().map(|_| "***"))
            .finish()
    }
}

/// A saved connection. Passwords are *not* part of the profile; they live in the OS
/// credential store and are looked up by [`ConnectionProfile::id`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    /// Stable identifier.
    pub id: ProfileId,
    /// Display name shown in the explorer.
    pub name: String,
    /// Database engine.
    pub kind: DatabaseKind,
    /// Host name or IP (networked engines) — or file path for SQLite.
    pub host: String,
    /// TCP port (ignored for SQLite).
    pub port: u16,
    /// Initial database; empty means server default.
    pub database: String,
    /// Login user name.
    pub username: String,
    /// Whether a password should be fetched from the credential store.
    pub save_password: bool,
    /// TLS behaviour.
    pub ssl_mode: SslMode,
    /// Environment classification.
    pub environment: Environment,
    /// Optional SSH tunnel; the host/port above are then resolved on the SSH server.
    #[serde(default)]
    pub ssh: Option<SshTunnel>,
}

impl ConnectionProfile {
    /// Creates a profile pre-filled with sensible defaults for `kind`.
    pub fn new(kind: DatabaseKind) -> Self {
        let host = if kind.is_networked() {
            "localhost".to_owned()
        } else {
            String::new()
        };
        let username = kind.default_username().to_owned();
        Self {
            id: ProfileId::new(),
            name: String::new(),
            kind,
            host,
            port: kind.default_port().unwrap_or(0),
            database: String::new(),
            username,
            save_password: true,
            ssl_mode: SslMode::default(),
            environment: Environment::default(),
            ssh: None,
        }
    }

    /// Switches the engine. Port and user name are reset only while they still hold the
    /// previous engine's defaults, so values typed by the user are kept.
    pub fn set_kind(&mut self, kind: DatabaseKind) {
        let had_default_port = self.kind.default_port() == Some(self.port);
        let had_default_user = self.username == self.kind.default_username();
        self.kind = kind;
        if had_default_port || self.port == 0 {
            self.port = kind.default_port().unwrap_or(0);
        }
        if had_default_user {
            self.username = kind.default_username().to_owned();
        }
    }

    /// Session key for a tab that runs in `database`.
    ///
    /// `None` is this profile's own database (one shared session). A different, non-empty name
    /// is a separate session, matching how the worker opens connections.
    pub fn session_database(&self, database: Option<String>) -> Option<String> {
        database.filter(|db| !db.is_empty() && *db != self.database)
    }

    /// `host:port/database` summary for tooltips and status bars.
    pub fn endpoint(&self) -> String {
        if !self.kind.is_networked() {
            return self.host.clone();
        }
        if self.database.is_empty() {
            format!("{}:{}", self.host, self.port)
        } else {
            format!("{}:{}/{}", self.host, self.port, self.database)
        }
    }

    /// Validates user-editable fields. Returns every problem found, not just the first.
    pub fn validate(&self) -> Result<(), Vec<ValidationIssue>> {
        let mut issues = Vec::new();
        if self.name.trim().is_empty() {
            issues.push(ValidationIssue::new(
                Field::Name,
                "Connection name is required",
            ));
        }
        if self.host.trim().is_empty() {
            let msg = if self.kind.is_networked() {
                "Host is required"
            } else {
                "Database file path is required"
            };
            issues.push(ValidationIssue::new(Field::Host, msg));
        }
        if self.kind.is_networked() {
            if self.port == 0 {
                issues.push(ValidationIssue::new(Field::Port, "Port must be 1–65535"));
            }
            if self.kind.requires_username() && self.username.trim().is_empty() {
                issues.push(ValidationIssue::new(
                    Field::Username,
                    "User name is required",
                ));
            }
        }
        if let Some(ssh) = &self.ssh {
            if ssh.host.trim().is_empty() {
                issues.push(ValidationIssue::new(Field::SshHost, "SSH host is required"));
            }
            if ssh.username.trim().is_empty() {
                issues.push(ValidationIssue::new(Field::SshUser, "SSH user is required"));
            }
            if matches!(&ssh.auth, SshAuth::PrivateKey { path } if path.trim().is_empty()) {
                issues.push(ValidationIssue::new(
                    Field::SshKey,
                    "Private key file is required",
                ));
            }
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }
}

/// Profile field referenced by a [`ValidationIssue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    /// [`ConnectionProfile::name`]
    Name,
    /// [`ConnectionProfile::host`]
    Host,
    /// [`ConnectionProfile::port`]
    Port,
    /// [`ConnectionProfile::username`]
    Username,
    /// [`SshTunnel::host`]
    SshHost,
    /// [`SshTunnel::username`]
    SshUser,
    /// [`SshAuth::PrivateKey`] path
    SshKey,
}

/// A single validation problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    /// Offending field.
    pub field: Field,
    /// User-facing message.
    pub message: &'static str,
}

impl ValidationIssue {
    fn new(field: Field, message: &'static str) -> Self {
        Self { field, message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_profile_uses_engine_defaults() {
        let p = ConnectionProfile::new(DatabaseKind::SqlServer);
        assert_eq!(p.port, 1433);
        assert_eq!(p.username, "sa");
        assert_eq!(p.environment, Environment::Development);
    }

    #[test]
    fn set_kind_updates_default_port_only() {
        let mut p = ConnectionProfile::new(DatabaseKind::PostgreSql);
        p.set_kind(DatabaseKind::MySql);
        assert_eq!(p.port, 3306, "default port follows engine");
        assert_eq!(p.username, "root", "default user follows engine");

        p.username = "app".into();
        p.set_kind(DatabaseKind::PostgreSql);
        assert_eq!(p.username, "app", "custom user is preserved");

        p.port = 13306;
        p.set_kind(DatabaseKind::SqlServer);
        assert_eq!(p.port, 13306, "custom port is preserved");
    }

    #[test]
    fn validation_reports_all_missing_fields() {
        let mut p = ConnectionProfile::new(DatabaseKind::PostgreSql);
        p.host.clear();
        p.username.clear();
        p.port = 0;
        let issues = p.validate().err().unwrap_or_default();
        let fields: Vec<Field> = issues.iter().map(|i| i.field).collect();
        assert_eq!(
            fields,
            vec![Field::Name, Field::Host, Field::Port, Field::Username]
        );
    }

    #[test]
    fn ssh_tunnel_fields_are_validated() {
        let mut p = ConnectionProfile::new(DatabaseKind::PostgreSql);
        p.name = "pg".into();
        p.ssh = Some(SshTunnel {
            auth: SshAuth::PrivateKey { path: " ".into() },
            ..SshTunnel::default()
        });
        let fields: Vec<Field> = p
            .validate()
            .err()
            .unwrap_or_default()
            .iter()
            .map(|i| i.field)
            .collect();
        assert_eq!(fields, vec![Field::SshHost, Field::SshUser, Field::SshKey]);
    }

    #[test]
    fn ssh_secret_is_never_serialised_or_logged() {
        let mut p = ConnectionProfile::new(DatabaseKind::PostgreSql);
        p.ssh = Some(SshTunnel {
            secret: Some("hunter2".into()),
            ..SshTunnel::default()
        });
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("hunter2"), "{json}");
        assert!(!format!("{p:?}").contains("hunter2"));
        let back: ConnectionProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ssh.unwrap().secret, None);
    }

    #[test]
    fn profiles_without_ssh_field_still_load() {
        let mut p = ConnectionProfile::new(DatabaseKind::MySql);
        p.name = "old".into();
        let mut json: serde_json::Value = serde_json::to_value(&p).unwrap();
        json.as_object_mut().unwrap().remove("ssh");
        let back: ConnectionProfile = serde_json::from_value(json).unwrap();
        assert_eq!(back.ssh, None);
    }

    #[test]
    fn session_database_is_none_for_the_profile_default() {
        let mut p = ConnectionProfile::new(DatabaseKind::PostgreSql);
        p.database = "app".into();
        assert_eq!(p.session_database(None), None);
        assert_eq!(p.session_database(Some(String::new())), None);
        assert_eq!(p.session_database(Some("app".into())), None);
        assert_eq!(
            p.session_database(Some("analytics".into())),
            Some("analytics".into())
        );
    }

    #[test]
    fn sqlite_does_not_require_port_or_user() {
        let mut p = ConnectionProfile::new(DatabaseKind::Sqlite);
        p.name = "local".into();
        p.host = "/tmp/app.db".into();
        assert!(p.validate().is_ok());
        assert_eq!(p.endpoint(), "/tmp/app.db");
    }

    #[test]
    fn profile_round_trips_through_json() {
        let mut p = ConnectionProfile::new(DatabaseKind::MariaDb);
        p.name = "maria".into();
        p.environment = Environment::Production;
        let json = serde_json::to_string(&p).unwrap_or_default();
        let back: Option<ConnectionProfile> = serde_json::from_str(&json).ok();
        assert_eq!(back, Some(p));
    }
}
