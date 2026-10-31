//! Database errors that preserve vendor-specific information.

use std::fmt;

/// Broad error category, used by the UI to pick an icon/message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DbErrorKind {
    /// Network, DNS, TLS or handshake failure.
    Connection,
    /// Bad credentials or insufficient privileges.
    Authentication,
    /// The server rejected the SQL (syntax, missing object, constraint…).
    Query,
    /// The operation was cancelled by the user.
    Cancelled,
    /// Timeout while waiting for the server.
    Timeout,
    /// The adapter does not support the requested operation.
    Unsupported,
    /// Anything else.
    Internal,
}

/// An error returned by an adapter or the worker.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct DbError {
    /// Category.
    pub kind: DbErrorKind,
    /// Primary message.
    pub message: String,
    /// Vendor code: SQLSTATE (PostgreSQL), errno (MySQL), error number (SQL Server).
    pub code: Option<String>,
    /// Extra detail/hint from the server, if any.
    pub detail: Option<String>,
}

impl DbError {
    /// Creates an error without vendor code or detail.
    pub fn new(kind: DbErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            code: None,
            detail: None,
        }
    }

    /// Builder-style vendor code setter.
    #[must_use]
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    /// Builder-style detail setter.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Adds a troubleshooting hint to network-level failures (refused, unreachable, timed out)
    /// that carry no detail from the server. `address` is `host:port`.
    #[must_use]
    pub fn with_reachability_hint(mut self, address: &str) -> Self {
        if matches!(self.kind, DbErrorKind::Connection | DbErrorKind::Timeout)
            && self.detail.is_none()
        {
            self.detail = Some(format!(
                "Check that the server is running and accepts TCP connections on {address}, and that the host and port in this connection are correct."
            ));
        }
        self
    }

    /// Shorthand for a cancellation error.
    pub fn cancelled() -> Self {
        Self::new(DbErrorKind::Cancelled, "Query cancelled by user")
    }

    /// Whether this error means the session is dead and should be replaced.
    ///
    /// Transport failures count, and so do the vendor codes and phrases servers use when they
    /// close an idle session ("server has gone away", admin shutdown, "broken pipe"). A syntax
    /// error, a constraint failure, a statement timeout, or a bad password does not: the session
    /// is still usable.
    pub fn is_connection_lost(&self) -> bool {
        if self.kind == DbErrorKind::Connection {
            return true;
        }
        if matches!(
            self.kind,
            DbErrorKind::Cancelled | DbErrorKind::Authentication | DbErrorKind::Unsupported
        ) {
            return false;
        }
        self.code.as_deref().is_some_and(code_is_connection_lost)
            || text_is_connection_lost(&self.message)
            || self.detail.as_deref().is_some_and(text_is_connection_lost)
    }
}

/// Vendor codes that mean the server dropped the session, even when the adapter filed the
/// error as a query failure.
fn code_is_connection_lost(code: &str) -> bool {
    matches!(
        code,
        // MySQL / MariaDB errno
        "1053" | "1927" | "2006" | "2013" | "4031"
        // SQL Server
        | "64" | "233" | "596" | "10053" | "10054" | "10060" | "40197" | "40501" | "40613"
        // PostgreSQL SQLSTATE (class 08 and admin shutdown)
        | "08000" | "08001" | "08003" | "08004" | "08006" | "08007" | "57P01" | "57P02" | "57P03"
        // Oracle
        | "ORA-00028" | "ORA-01012" | "ORA-02396" | "ORA-02399" | "ORA-03111" | "ORA-03113"
        | "ORA-03114" | "ORA-03135" | "ORA-12170" | "ORA-12537" | "ORA-12547" | "ORA-12570"
        | "ORA-12571" | "ORA-25408"
        // MongoDB server codes (shutdown, not primary, interrupted)
        | "91" | "10107" | "11600" | "11602" | "13435" | "13436"
    )
}

/// Phrases drivers put in the message when the socket dies and there is no vendor code.
fn text_is_connection_lost(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    const PHRASES: &[&str] = &[
        "server has gone away",
        "lost connection",
        "connection reset",
        "connection closed",
        "connection is closed",
        "broken pipe",
        "server closed the connection",
        "error communicating with",
        "not connected",
        "session has been killed",
        "your session has been killed",
        "terminating connection",
        "the connection is broken",
        "transport-level error",
        "connection refused",
        "connection timed out",
        "i/o timed out",
        "network is unreachable",
        "no connection to the server",
        "ssl connection has been closed",
        "unexpected eof",
        "unexpected end of file",
        "server shutdown in progress",
        "client was disconnected",
        "ora-00028",
        "ora-01012",
        "ora-02396",
        "ora-03113",
        "ora-03114",
        "ora-03135",
        "ora-12170",
        "ora-12537",
        "ora-12570",
        "ora-12571",
        "ora-25408",
    ];
    PHRASES.iter().any(|phrase| lower.contains(phrase))
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(code) = &self.code {
            write!(f, "[{code}] ")?;
        }
        f.write_str(&self.message)?;
        if let Some(detail) = &self.detail {
            write!(f, " — {detail}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reachability_hint_only_for_network_errors_without_detail() {
        let refused = DbError::new(DbErrorKind::Connection, "refused")
            .with_reachability_hint("localhost:5432");
        assert!(
            refused
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("localhost:5432"))
        );

        let auth = DbError::new(DbErrorKind::Authentication, "bad password")
            .with_reachability_hint("localhost:5432");
        assert_eq!(auth.detail, None);

        let tls = DbError::new(DbErrorKind::Connection, "tls")
            .with_detail("certificate expired")
            .with_reachability_hint("localhost:5432");
        assert_eq!(tls.detail.as_deref(), Some("certificate expired"));
    }

    #[test]
    fn connection_lost_covers_transport_and_vendor_drop_signals() {
        assert!(
            DbError::new(DbErrorKind::Connection, "connection reset by peer").is_connection_lost()
        );
        assert!(
            DbError::new(DbErrorKind::Query, "MySQL server has gone away")
                .with_code("2006")
                .is_connection_lost()
        );
        assert!(
            DbError::new(
                DbErrorKind::Query,
                "error communicating with the database: broken pipe"
            )
            .is_connection_lost()
        );
        assert!(
            DbError::new(
                DbErrorKind::Query,
                "terminating connection due to administrator command"
            )
            .with_code("57P01")
            .is_connection_lost()
        );
        assert!(
            DbError::new(DbErrorKind::Query, "ORA-03114: not connected to ORACLE")
                .is_connection_lost()
        );

        assert!(
            !DbError::new(DbErrorKind::Query, "syntax error at or near \"from\"")
                .with_code("42601")
                .is_connection_lost()
        );
        assert!(
            !DbError::new(
                DbErrorKind::Authentication,
                "password authentication failed"
            )
            .is_connection_lost()
        );
        assert!(!DbError::cancelled().is_connection_lost());
        assert!(
            !DbError::new(
                DbErrorKind::Timeout,
                "canceling statement due to statement timeout"
            )
            .is_connection_lost()
        );
    }

    #[test]
    fn display_includes_code_and_detail() {
        let e = DbError::new(DbErrorKind::Query, "relation \"x\" does not exist")
            .with_code("42P01")
            .with_detail("check search_path");
        assert_eq!(
            e.to_string(),
            "[42P01] relation \"x\" does not exist — check search_path"
        );
    }
}
