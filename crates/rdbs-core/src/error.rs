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
