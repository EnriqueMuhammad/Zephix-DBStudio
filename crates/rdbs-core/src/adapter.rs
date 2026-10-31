//! Capability-based database adapter abstraction.
//!
//! Each engine crate implements [`AdapterFactory`] (to open sessions) and
//! [`DatabaseAdapter`] (one open session). Methods return [`BoxFuture`]s so both traits stay
//! object-safe without extra dependencies; the worker stores sessions as
//! `Box<dyn DatabaseAdapter>`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use futures_util::task::AtomicWaker;

use crate::edit::Change;
use crate::error::{DbError, DbErrorKind};
use crate::message::QueryOutcome;
use crate::plan::ExplainPlan;
use crate::profile::{ConnectionProfile, ProfileId};
use crate::schema::{DbObject, ObjectPath};
use crate::table::TableSchema;

/// A boxed, `Send` future — the return type of adapter methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Optional features an adapter supports. The UI enables or disables actions based on these
/// flags instead of matching on the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// Running statements can be cancelled server-side.
    pub query_cancellation: bool,
    /// Rows can be delivered incrementally.
    pub streaming_results: bool,
    /// Explicit BEGIN/COMMIT/ROLLBACK is supported.
    pub transactions: bool,
    /// Objects are grouped in schemas inside databases.
    pub schemas: bool,
    /// One connection can browse several databases.
    pub multiple_databases: bool,
    /// An execution plan can be requested.
    pub explain_plan: bool,
}

/// Cancellation signal shared between the worker and a running query.
///
/// Adapters either poll [`CancelHandle::is_cancelled`] or race their I/O against
/// [`CancelHandle::cancelled`] and then trigger their protocol-level cancel (e.g. the
/// PostgreSQL cancel request).
#[derive(Debug, Clone, Default)]
pub struct CancelHandle(Arc<CancelInner>);

#[derive(Debug, Default)]
struct CancelInner {
    flag: AtomicBool,
    waker: AtomicWaker,
}

impl CancelHandle {
    /// Creates a handle in the "not cancelled" state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation and wakes the task awaiting [`CancelHandle::cancelled`].
    pub fn cancel(&self) {
        self.0.flag.store(true, Ordering::Release);
        self.0.waker.wake();
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.flag.load(Ordering::Acquire)
    }

    /// Future that completes once cancellation is requested. Only one task should await it
    /// at a time (the running query).
    pub fn cancelled(&self) -> Cancelled<'_> {
        Cancelled(self)
    }
}

/// Future returned by [`CancelHandle::cancelled`].
#[derive(Debug)]
pub struct Cancelled<'a>(&'a CancelHandle);

impl Future for Cancelled<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0.is_cancelled() {
            return Poll::Ready(());
        }
        self.0.0.waker.register(cx.waker());
        // Re-check after registering to avoid missing a concurrent `cancel()`.
        if self.0.is_cancelled() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

/// Error returned by a [`CredentialStore`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("credential store: {0}")]
pub struct CredentialError(pub String);

/// Which secret of a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// Database password.
    Database,
    /// SSH password or private-key passphrase of the tunnel.
    Ssh,
}

/// Secure storage for connection passwords (the OS keychain in production).
///
/// Implementations may block; the worker calls them from a blocking thread.
pub trait CredentialStore: Send + Sync {
    /// Returns the saved password for a profile, if any.
    fn get(
        &self,
        profile_id: ProfileId,
        kind: SecretKind,
    ) -> Result<Option<String>, CredentialError>;
    /// Saves (or replaces) the password for a profile.
    fn set(
        &self,
        profile_id: ProfileId,
        kind: SecretKind,
        secret: &str,
    ) -> Result<(), CredentialError>;
    /// Removes the saved password; succeeds if there was none.
    fn delete(&self, profile_id: ProfileId, kind: SecretKind) -> Result<(), CredentialError>;
}

/// A [`CredentialStore`] that keeps passwords in memory only (mock mode and tests).
#[derive(Debug, Default)]
pub struct MemoryCredentials(
    std::sync::Mutex<std::collections::HashMap<(ProfileId, SecretKind), String>>,
);

impl CredentialStore for MemoryCredentials {
    fn get(
        &self,
        profile_id: ProfileId,
        kind: SecretKind,
    ) -> Result<Option<String>, CredentialError> {
        let map = self.0.lock().map_err(|e| CredentialError(e.to_string()))?;
        Ok(map.get(&(profile_id, kind)).cloned())
    }

    fn set(
        &self,
        profile_id: ProfileId,
        kind: SecretKind,
        secret: &str,
    ) -> Result<(), CredentialError> {
        let mut map = self.0.lock().map_err(|e| CredentialError(e.to_string()))?;
        map.insert((profile_id, kind), secret.to_owned());
        Ok(())
    }

    fn delete(&self, profile_id: ProfileId, kind: SecretKind) -> Result<(), CredentialError> {
        let mut map = self.0.lock().map_err(|e| CredentialError(e.to_string()))?;
        map.remove(&(profile_id, kind));
        Ok(())
    }
}

/// Error for a feature an adapter does not implement.
pub fn unsupported(feature: &str) -> DbError {
    DbError::new(
        DbErrorKind::Unsupported,
        format!("{feature} are not available for this connection"),
    )
}

/// An open database session.
pub trait DatabaseAdapter: Send {
    /// Features supported by this session.
    fn capabilities(&self) -> Capabilities;

    /// Server product/version string, e.g. `PostgreSQL 17.2`.
    fn server_version(&self) -> &str;

    /// Loads the object tree shown in the explorer.
    fn load_objects(&mut self) -> BoxFuture<'_, Result<Vec<DbObject>, DbError>>;

    /// Executes `sql` (possibly several statements) and returns the last result.
    ///
    /// Implementations must return [`DbError::cancelled`] promptly once `cancel` is set.
    fn execute<'a>(
        &'a mut self,
        sql: &'a str,
        cancel: CancelHandle,
    ) -> BoxFuture<'a, Result<QueryOutcome, DbError>>;

    /// Applies grid edits in one transaction. Every change must affect exactly
    /// `expected_rows` rows, otherwise everything is rolled back. Returns the rows changed.
    fn apply_changes<'a>(
        &'a mut self,
        changes: &'a [Change],
    ) -> BoxFuture<'a, Result<u64, DbError>> {
        let _ = changes;
        Box::pin(async {
            Err(DbError::new(
                DbErrorKind::Unsupported,
                "editing data is not supported by this connection",
            ))
        })
    }

    /// `CREATE` statement(s) for a table, view or routine.
    fn object_ddl<'a>(
        &'a mut self,
        object: &'a ObjectPath,
    ) -> BoxFuture<'a, Result<String, DbError>> {
        let _ = object;
        Box::pin(async {
            Err(DbError::new(
                DbErrorKind::Unsupported,
                "DDL is not available for this connection",
            ))
        })
    }

    /// Parameters of a function or procedure, to execute it.
    fn routine_signature<'a>(
        &'a mut self,
        object: &'a ObjectPath,
    ) -> BoxFuture<'a, Result<crate::routine::RoutineSignature, DbError>> {
        let _ = object;
        Box::pin(async { Err(unsupported("routine parameters")) })
    }

    /// Structured definitions of the base tables of the session's database, optionally only
    /// those of one schema. Tables are ordered by schema, then name.
    fn describe_schema<'a>(
        &'a mut self,
        schema: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<TableSchema>, DbError>> {
        let _ = schema;
        Box::pin(async { Err(unsupported("table definitions")) })
    }

    /// Execution plan of one statement. With `analyze` the statement is executed to collect
    /// actual figures; adapters roll back its changes where the engine allows it.
    fn explain<'a>(
        &'a mut self,
        sql: &'a str,
        analyze: bool,
    ) -> BoxFuture<'a, Result<ExplainPlan, DbError>> {
        let _ = (sql, analyze);
        Box::pin(async { Err(unsupported("execution plans")) })
    }

    /// Cheap round-trip that proves the session is still usable.
    ///
    /// The worker calls this on idle sessions. It must not change data. The default does
    /// nothing and reports success; networked engines override it with a ping or `SELECT 1`.
    /// A lost connection is [`DbErrorKind::Connection`] (or otherwise
    /// [`DbError::is_connection_lost`]) so the worker can drop the session.
    fn ping(&mut self) -> BoxFuture<'_, Result<(), DbError>> {
        Box::pin(async { Ok(()) })
    }

    /// Gracefully closes the session. The default implementation does nothing.
    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// Error for a change that did not affect the expected number of rows.
pub fn row_count_mismatch(index: usize, change: &Change, actual: u64) -> DbError {
    DbError::new(
        DbErrorKind::Query,
        format!(
            "change {} affected {actual} rows instead of {}; the row may have been changed or \
             deleted by someone else. Nothing was saved.",
            index + 1,
            change.expected_rows
        ),
    )
    .with_detail(change.sql.clone())
}

/// Opens sessions for one or more engines.
pub trait AdapterFactory: Send + Sync {
    /// Opens a new session for `profile`. `password` comes from the credential store.
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn DatabaseAdapter>, DbError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_future_wakes_on_cancel() {
        use std::sync::atomic::AtomicUsize;
        use std::task::{Wake, Waker};

        struct CountWake(AtomicUsize);
        impl Wake for CountWake {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let handle = CancelHandle::new();
        let counter = Arc::new(CountWake(AtomicUsize::new(0)));
        let waker = Waker::from(counter.clone());
        let mut cx = Context::from_waker(&waker);
        let mut fut = handle.cancelled();
        assert!(Pin::new(&mut fut).poll(&mut cx).is_pending());
        handle.clone().cancel();
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert!(Pin::new(&mut fut).poll(&mut cx).is_ready());
    }

    #[test]
    fn memory_credentials_round_trip() {
        let store = MemoryCredentials::default();
        let id = ProfileId::new();
        let db = SecretKind::Database;
        assert_eq!(store.get(id, db).unwrap(), None);
        store.set(id, db, "s3cret").unwrap();
        store.set(id, SecretKind::Ssh, "tunnel").unwrap();
        assert_eq!(store.get(id, db).unwrap().as_deref(), Some("s3cret"));
        assert_eq!(
            store.get(id, SecretKind::Ssh).unwrap().as_deref(),
            Some("tunnel")
        );
        store.delete(id, db).unwrap();
        store.delete(id, db).unwrap();
        assert_eq!(store.get(id, db).unwrap(), None);
        assert!(store.get(id, SecretKind::Ssh).unwrap().is_some());
    }

    #[test]
    fn cancel_handle_is_shared_between_clones() {
        let a = CancelHandle::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled());
    }
}
