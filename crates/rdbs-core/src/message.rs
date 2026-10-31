//! Typed messages between the UI thread and the background worker.

use std::time::Duration;

use crate::adapter::Capabilities;
use crate::error::DbError;
use crate::profile::{ConnectionProfile, ProfileId};
use crate::schema::DbObject;
use crate::value::ResultSet;

/// Correlates a request (query, connection test) with its events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

/// Successful result of executing SQL.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueryOutcome {
    /// Every result set produced by the script, in execution order.
    pub results: Vec<ResultSet>,
    /// Rows affected by the last DML statement, if reported.
    pub rows_affected: Option<u64>,
    /// Informational server messages (PRINT, NOTICE, warnings).
    pub messages: Vec<String>,
    /// Whether rows beyond [`crate::MAX_RESULT_ROWS`] were discarded.
    pub truncated: bool,
}

impl QueryOutcome {
    /// The last result set, which is what most tools show by default.
    pub fn last_result(&self) -> Option<&ResultSet> {
        self.results.last()
    }

    /// Total number of rows over all result sets.
    pub fn total_rows(&self) -> usize {
        self.results.iter().map(|r| r.rows.len()).sum()
    }

    /// Records the row count of a data-modifying statement: it becomes
    /// [`Self::rows_affected`] and a "(n rows affected)" line in [`Self::messages`], so the
    /// messages list every statement in execution order.
    pub fn record_rows_affected(&mut self, n: u64) {
        self.rows_affected = Some(n);
        self.messages.push(rows_affected_message(n));
    }

    /// Adds a result set together with its "(n rows affected)" line.
    pub fn push_result(&mut self, result: ResultSet) {
        self.messages
            .push(rows_affected_message(result.rows.len() as u64));
        self.results.push(result);
    }
}

/// Row count line in the style of database consoles: "(1 row affected)", "(3 rows affected)".
pub fn rows_affected_message(n: u64) -> String {
    if n == 1 {
        "(1 row affected)".to_owned()
    } else {
        format!("({n} rows affected)")
    }
}

/// Commands sent from the UI to the worker.
// Commands are sent one at a time, so the size of the job variants does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Command {
    /// Open (or reuse) a session for the profile and load its object tree.
    Connect {
        /// Profile to connect.
        profile: ConnectionProfile,
        /// Password entered for this session; `None` uses the credential store.
        password: Option<String>,
    },
    /// Close the session for the profile.
    Disconnect {
        /// Profile to disconnect.
        profile_id: ProfileId,
    },
    /// Close one session and open a new one. Editor tabs stay open; the caller keeps their text.
    ///
    /// `database` is the same override as [`Command::Execute`]: `None` replaces the profile's
    /// main session. Temporary tables and open transactions on that session are lost.
    Reconnect {
        /// Target connection.
        profile: ConnectionProfile,
        /// Password entered for this session; `None` uses the credential store.
        password: Option<String>,
        /// Session to replace (`None` = the profile's own database).
        database: Option<String>,
    },
    /// Reload the object tree of an open session.
    RefreshObjects {
        /// Profile whose objects to reload.
        profile_id: ProfileId,
    },
    /// Execute SQL, connecting first if necessary.
    Execute {
        /// Correlation id.
        request_id: RequestId,
        /// Target connection.
        profile: ConnectionProfile,
        /// SQL text.
        sql: String,
        /// Password entered for this session; `None` uses the credential store.
        password: Option<String>,
        /// Run in this database instead of the profile's (a separate session is used).
        database: Option<String>,
    },
    /// Open a session for another database of the server and load its objects.
    LoadDatabase {
        /// Connection profile.
        profile: ConnectionProfile,
        /// Database to browse.
        database: String,
        /// Password entered for this session; `None` uses the credential store.
        password: Option<String>,
    },
    /// Apply grid edits in one transaction.
    ApplyChanges {
        /// Correlation id.
        request_id: RequestId,
        /// Target connection.
        profile: ConnectionProfile,
        /// Database override (as for [`Command::Execute`]).
        database: Option<String>,
        /// Statements to apply.
        changes: Vec<crate::edit::Change>,
        /// Password entered for this session; `None` uses the credential store.
        password: Option<String>,
    },
    /// Fetch the DDL of an object.
    GetDdl {
        /// Correlation id.
        request_id: RequestId,
        /// Connection.
        profile: ConnectionProfile,
        /// Database override (session), as for [`Command::Execute`].
        database: Option<String>,
        /// The object.
        object: crate::schema::ObjectPath,
        /// Password entered for this session; `None` uses the credential store.
        password: Option<String>,
    },
    /// Fetch the parameters of a function or procedure.
    GetRoutine {
        /// Correlation id.
        request_id: RequestId,
        /// Connection.
        profile: ConnectionProfile,
        /// Database override (session), as for [`Command::Execute`].
        database: Option<String>,
        /// The routine.
        object: crate::schema::ObjectPath,
        /// Password entered for this session; `None` uses the credential store.
        password: Option<String>,
    },
    /// Describe the tables of a database (table designer, schema diff, ER diagram, transfer).
    DescribeSchema {
        /// Correlation id.
        request_id: RequestId,
        /// Connection.
        profile: ConnectionProfile,
        /// Database override (session), as for [`Command::Execute`].
        database: Option<String>,
        /// Only this schema (`None` = all user schemas).
        schema: Option<String>,
        /// Password entered for this session; `None` uses the credential store.
        password: Option<String>,
    },
    /// Run DDL statements one by one, stopping at the first failure.
    RunScript {
        /// Correlation id.
        request_id: RequestId,
        /// Connection.
        profile: ConnectionProfile,
        /// Database override (session), as for [`Command::Execute`].
        database: Option<String>,
        /// Statements without trailing `;`.
        statements: Vec<String>,
        /// Password entered for this session; `None` uses the credential store.
        password: Option<String>,
    },
    /// Fetch the execution plan of one statement.
    Explain {
        /// Correlation id.
        request_id: RequestId,
        /// Connection.
        profile: ConnectionProfile,
        /// Database override (session), as for [`Command::Execute`].
        database: Option<String>,
        /// The statement.
        sql: String,
        /// Execute it to collect actual figures (changes are rolled back where possible).
        analyze: bool,
        /// Password entered for this session; `None` uses the credential store.
        password: Option<String>,
    },
    /// Copy or synchronise tables between two connections (a background job; progress is
    /// reported with [`Event::JobProgress`], cancellation with [`Command::Cancel`]).
    Transfer {
        /// Correlation id.
        request_id: RequestId,
        /// Source connection.
        source: ConnectionProfile,
        /// Source database override.
        source_database: Option<String>,
        /// Password entered for the source; `None` uses the credential store.
        source_password: Option<String>,
        /// Target connection.
        target: ConnectionProfile,
        /// Target database override.
        target_database: Option<String>,
        /// Password entered for the target; `None` uses the credential store.
        target_password: Option<String>,
        /// What to copy and how.
        options: crate::job::TransferOptions,
    },
    /// Write a logical backup (SQL dump) of a database (a background job).
    Backup {
        /// Correlation id.
        request_id: RequestId,
        /// Connection.
        profile: ConnectionProfile,
        /// Database override.
        database: Option<String>,
        /// Password entered for this session; `None` uses the credential store.
        password: Option<String>,
        /// What to dump and where.
        options: crate::job::BackupOptions,
    },
    /// Start, stop, enable or disable a SQL Server Agent job, then reload the object tree.
    ///
    /// The UI confirms this on production connections, and confirms Stop on every connection.
    RunAgentJob {
        /// Correlation id.
        request_id: RequestId,
        /// Target connection.
        profile: ConnectionProfile,
        /// Password entered for this session; `None` uses the credential store.
        password: Option<String>,
        /// `sysjobs.name`.
        job_name: String,
        /// What to run. Not a step or schedule edit.
        action: crate::agent::AgentJobAction,
    },
    /// Cancel a running [`Command::Execute`] or background job.
    Cancel {
        /// Request to cancel.
        request_id: RequestId,
    },
    /// Open a throw-away session to verify settings (connection dialog "Test").
    TestConnection {
        /// Correlation id.
        request_id: RequestId,
        /// Profile under test (may be unsaved).
        profile: ConnectionProfile,
        /// Password typed in the dialog; `None` uses the credential store.
        password: Option<String>,
    },
    /// Stop the worker. Open sessions are closed.
    Shutdown,
    /// Ping every idle session and drop the ones the server has closed.
    ///
    /// The worker sends this on a timer. A query that holds the session is left alone.
    /// SQLite answers with a local `SELECT 1` (there is no server idle timeout).
    ProbeSessions,
}

/// Events sent from the worker to the UI.
#[derive(Debug, Clone)]
pub enum Event {
    /// A connection attempt started.
    Connecting {
        /// Profile being connected.
        profile_id: ProfileId,
    },
    /// The session is open.
    Connected {
        /// Connected profile.
        profile_id: ProfileId,
        /// Server version string.
        server_version: String,
        /// Session capabilities.
        capabilities: Capabilities,
    },
    /// The connection attempt failed.
    ConnectionFailed {
        /// Profile that failed.
        profile_id: ProfileId,
        /// Reason.
        error: DbError,
    },
    /// The session was closed.
    Disconnected {
        /// Closed profile.
        profile_id: ProfileId,
    },
    /// A session died (the server closed it, or the socket failed) and was discarded.
    ///
    /// Unlike [`Event::Disconnected`], the user did not close the connection: open tabs and
    /// their text stay, and the explorer tree is left in place. The next use of that session
    /// connects again; [`Command::Reconnect`] does it immediately.
    ConnectionLost {
        /// Profile whose session died.
        profile_id: ProfileId,
        /// Which session. `None` is the profile's own database.
        database: Option<String>,
        /// The error that revealed the drop.
        error: DbError,
    },
    /// [`Command::Reconnect`] opened a new session.
    Reconnected {
        /// Profile that was reconnected.
        profile_id: ProfileId,
        /// Which session. `None` is the profile's own database.
        database: Option<String>,
    },
    /// [`Command::Reconnect`] failed for a session that is not the profile's main one.
    ///
    /// A failed reconnect of the main session is [`Event::ConnectionFailed`] instead, which is
    /// how every failed connect is reported.
    ReconnectFailed {
        /// Profile that could not be reconnected.
        profile_id: ProfileId,
        /// Which session. `None` is the profile's own database.
        database: Option<String>,
        /// Reason.
        error: DbError,
    },
    /// The object tree was (re)loaded.
    ObjectsLoaded {
        /// Profile the objects belong to.
        profile_id: ProfileId,
        /// Top-level objects (usually databases).
        objects: Vec<DbObject>,
    },
    /// Loading the object tree failed.
    ObjectsFailed {
        /// Profile the request was for.
        profile_id: ProfileId,
        /// Reason.
        error: DbError,
    },
    /// Objects of a non-default database were loaded (children of its explorer node).
    DatabaseObjectsLoaded {
        /// Profile the database belongs to.
        profile_id: ProfileId,
        /// Database name.
        database: String,
        /// Schemas/folders inside the database.
        objects: Vec<DbObject>,
    },
    /// Loading a non-default database failed.
    DatabaseObjectsFailed {
        /// Profile the database belongs to.
        profile_id: ProfileId,
        /// Database name.
        database: String,
        /// Reason.
        error: DbError,
    },
    /// A query has been dispatched to the server.
    QueryStarted {
        /// Correlation id.
        request_id: RequestId,
    },
    /// A query finished, failed or was cancelled.
    QueryFinished {
        /// Correlation id.
        request_id: RequestId,
        /// Wall-clock time from dispatch to completion.
        elapsed: Duration,
        /// Outcome.
        result: Result<QueryOutcome, DbError>,
    },
    /// Grid edits were applied (`Ok` = rows changed) or rolled back.
    ChangesApplied {
        /// Correlation id.
        request_id: RequestId,
        /// Outcome.
        result: Result<u64, DbError>,
    },
    /// DDL of an object, or why it could not be produced.
    DdlLoaded {
        /// Correlation id.
        request_id: RequestId,
        /// Outcome.
        result: Result<String, DbError>,
    },
    /// Result of [`Command::GetRoutine`].
    RoutineLoaded {
        /// Correlation id.
        request_id: RequestId,
        /// Outcome.
        result: Result<crate::routine::RoutineSignature, DbError>,
    },
    /// Result of [`Command::DescribeSchema`].
    SchemaDescribed {
        /// Correlation id.
        request_id: RequestId,
        /// Outcome.
        result: Result<Vec<crate::table::TableSchema>, DbError>,
    },
    /// Result of [`Command::RunScript`]: `Ok` = statements executed. On failure the error's
    /// detail names the failing statement; earlier statements stay applied.
    ScriptFinished {
        /// Correlation id.
        request_id: RequestId,
        /// Outcome.
        result: Result<usize, DbError>,
    },
    /// Result of [`Command::Explain`].
    PlanReady {
        /// Correlation id.
        request_id: RequestId,
        /// Outcome.
        result: Result<crate::plan::ExplainPlan, DbError>,
    },
    /// Progress of a background job.
    JobProgress {
        /// Correlation id.
        request_id: RequestId,
        /// What is happening.
        message: String,
        /// Fraction done (0–1), when known.
        fraction: Option<f32>,
    },
    /// A background job finished; `Ok` carries a summary.
    JobFinished {
        /// Correlation id.
        request_id: RequestId,
        /// Outcome.
        result: Result<String, DbError>,
    },
    /// A SQL Server Agent job action finished. `Ok` is a sentence for the status bar.
    /// The object tree is refreshed with [`Event::ObjectsLoaded`] before this event.
    AgentJobFinished {
        /// Correlation id.
        request_id: RequestId,
        /// Connection the job belongs to.
        profile_id: ProfileId,
        /// Outcome. The error message is already a single sentence.
        result: Result<String, DbError>,
    },
    /// Idle sessions were checked. `dropped` of them were discarded.
    ///
    /// Emitted after [`Command::ProbeSessions`] (the worker's keepalive). The UI uses
    /// [`Event::ConnectionLost`] for the ones that died; this event only says the pass finished.
    SessionsProbed {
        /// Sessions that were idle and answered, or were dropped.
        checked: usize,
        /// Sessions discarded because the ping failed or timed out.
        dropped: usize,
    },
    /// A connection test finished; `Ok` carries the server version.
    TestFinished {
        /// Correlation id.
        request_id: RequestId,
        /// Outcome.
        result: Result<String, DbError>,
    },
}
