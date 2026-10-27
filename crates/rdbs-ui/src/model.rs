//! Runtime UI state shared between views, and the actions views request.

use rdbs_core::export::ExportFormat;
use rdbs_core::sql::StatementKind;
use rdbs_core::{Capabilities, DbError, DbObject, ObjectKind, ProfileId};

/// Connection status of a profile as seen by the UI.
#[derive(Debug, Clone, Default)]
pub enum ConnStatus {
    /// No session.
    #[default]
    Disconnected,
    /// Connect requested, waiting for the worker.
    Connecting,
    /// Session open.
    Connected {
        /// Server version string.
        server_version: String,
        /// Session capabilities.
        capabilities: Capabilities,
    },
    /// Last attempt failed.
    Failed(DbError),
}

/// Per-profile runtime state.
#[derive(Debug, Clone, Default)]
pub struct ConnState {
    /// Current status.
    pub status: ConnStatus,
    /// Loaded object tree (empty until connected).
    pub objects: Vec<DbObject>,
    /// Database the main session is bound to (expanded in the initial tree).
    pub default_database: Option<String>,
    /// Other databases whose objects are being loaded.
    pub loading_databases: std::collections::HashSet<String>,
}

impl ConnState {
    /// Whether a session is open.
    pub fn is_connected(&self) -> bool {
        matches!(self.status, ConnStatus::Connected { .. })
    }
}

/// Reference to a database object selected in the explorer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRef {
    /// Owning connection.
    pub profile_id: ProfileId,
    /// Database (catalog), if any.
    pub database: Option<String>,
    /// Schema, if the engine has schemas.
    pub schema: Option<String>,
    /// Object name.
    pub name: String,
    /// Object kind.
    pub kind: ObjectKind,
}

/// Something a view asks the application to do. Views collect actions while rendering;
/// [`crate::StudioApp`] applies them after the frame's widgets are laid out, which keeps
/// borrows simple and makes behaviour easy to follow.
#[derive(Debug, Clone)]
pub enum UiAction {
    /// Open a new query tab.
    NewQuery {
        /// Pre-selected connection.
        profile_id: Option<ProfileId>,
        /// Initial SQL text.
        sql: Option<String>,
        /// Tab title override.
        title: Option<String>,
        /// Execute immediately after opening.
        run: bool,
    },
    /// Open the rows of a table or view in a data tab (grid without an editor).
    OpenObject(ObjectRef),
    /// Open a query tab that selects the first `rows` rows of a table or view, and run it.
    SelectTop {
        /// Table or view.
        obj: ObjectRef,
        /// Row count.
        rows: u32,
    },
    /// Open the definition of a function or procedure as a script that saves changes.
    EditRoutine(ObjectRef),
    /// Ask for the parameters of a function or procedure, then execute it.
    ExecuteRoutine(ObjectRef),
    /// Fetch an object's DDL and open it in a new tab.
    ShowDdl(ObjectRef),
    /// Open a SQL Server Agent job's run history (one row per step) in a result tab.
    ViewJobHistory(ObjectRef),
    /// Start, stop, enable or disable a SQL Server Agent job.
    ///
    /// The application confirms every action on a production connection, and confirms Stop
    /// on every connection. Step and schedule edits are not an action.
    RunAgentJob {
        /// Job node.
        obj: ObjectRef,
        /// Procedure to run.
        action: rdbs_core::agent::AgentJobAction,
    },
    /// Connect a profile.
    Connect(ProfileId),
    /// Disconnect a profile.
    Disconnect(ProfileId),
    /// Reload a profile's object tree.
    Refresh(ProfileId),
    /// Show the connection dialog for a new profile.
    NewConnection,
    /// Show the connection dialog for an existing profile.
    EditConnection(ProfileId),
    /// Copy a profile.
    DuplicateConnection(ProfileId),
    /// Move a connection in the explorer next to another one.
    MoveConnection {
        /// Connection being moved.
        id: ProfileId,
        /// Connection it was dropped on.
        target: ProfileId,
        /// Place it below `target` (else above).
        below: bool,
    },
    /// Ask for confirmation, then delete a profile.
    DeleteConnection(ProfileId),
    /// Ask for confirmation before running a risky statement on production.
    ConfirmExecute {
        /// Tab that will receive the results.
        tab_id: u64,
        /// Target connection.
        profile_id: ProfileId,
        /// SQL to run.
        sql: String,
        /// Classification that triggered the confirmation.
        kind: StatementKind,
    },
    /// Load the objects of another database of the server.
    LoadDatabase {
        /// Connection profile.
        profile_id: ProfileId,
        /// Database to load.
        database: String,
    },
    /// Ask for a password, then continue with `then`.
    NeedPassword {
        /// Profile that needs a password.
        profile_id: ProfileId,
        /// What to do once it is entered.
        then: AfterPassword,
    },
    /// Open a script (from history) in a new tab, in the database it last ran in.
    OpenScript {
        /// Connection.
        profile_id: ProfileId,
        /// Database override, if any.
        database: Option<String>,
        /// SQL text.
        sql: String,
        /// Execute right away.
        run: bool,
    },
    /// Save the pending grid edits of a tab.
    SaveEdits {
        /// Tab id.
        tab_id: u64,
    },
    /// Save the result shown in the active tab as CSV.
    ExportActive,
    /// Open (or focus) the query history tab.
    ShowHistory,
    /// Reload history entries matching a filter.
    ReloadHistory(String),
    /// Ask for confirmation, then delete all history.
    ClearHistory,
    /// Ask where to save exported text, then write it.
    SaveExport {
        /// File contents.
        contents: String,
        /// Suggested file name, including the extension.
        file_name: String,
        /// Format, for the file-type filter.
        format: ExportFormat,
    },
    /// Open (or focus) a tool tab.
    OpenTool(crate::tools::ToolLaunch),
    /// Insert or update a backup schedule.
    SaveSchedule(rdbs_core::job::BackupSchedule),
    /// Delete a backup schedule.
    DeleteSchedule(String),
    /// Run a backup schedule now.
    RunSchedule(String),
    /// Run a one-off backup.
    RunBackup {
        /// Connection.
        profile_id: ProfileId,
        /// Database override.
        database: Option<String>,
        /// What to dump and where.
        options: rdbs_core::job::BackupOptions,
    },
    /// Show a message in the status bar.
    Status(StatusMessage),
    /// Toggle the explorer sidebar.
    ToggleSidebar,
    /// Close one tab, all others or all of them. Tabs with unsaved changes ask first.
    CloseTabs(TabClose),
    /// Ask for a `.sql` file and open it in a query tab.
    OpenQueryFile,
    /// Save a query tab (the active one when `None`) to its file, or ask for one.
    SaveQuery {
        /// Tab id.
        tab_id: Option<u64>,
        /// Always ask for the file name.
        save_as: bool,
    },
}

/// Which tabs a close request removes; tabs are identified by [`crate::workspace::Tab::key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabClose {
    /// This tab.
    One(u64),
    /// Every tab except this one.
    Others(u64),
    /// Every tab.
    All,
}

impl TabClose {
    /// Whether the tab with `key` is closed.
    pub fn closes(self, key: u64) -> bool {
        match self {
            TabClose::One(k) => key == k,
            TabClose::Others(k) => key != k,
            TabClose::All => true,
        }
    }
}

/// Backup schedules and the backup jobs in flight (owned by the app, shown by the tool tab).
#[derive(Debug, Default)]
pub struct BackupState {
    /// Saved schedules.
    pub schedules: Vec<rdbs_core::job::BackupSchedule>,
    /// Running jobs: request → schedule id (`""` for a one-off backup).
    pub running: std::collections::HashMap<rdbs_core::RequestId, String>,
    /// Progress text by schedule id (`""` for a one-off backup).
    pub status: std::collections::HashMap<String, String>,
    /// Outcome of the last one-off backup.
    pub adhoc_result: Option<Result<String, String>>,
    /// When schedules were last checked.
    pub last_check: Option<std::time::Instant>,
}

/// What to do after the password prompt is confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterPassword {
    /// Connect the profile (explorer).
    Connect,
    /// Run the query tab with this id.
    RunTab(u64),
}

/// Severity of a status bar message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Informational.
    Info,
    /// Something went wrong.
    Error,
}

/// A status bar message.
#[derive(Debug, Clone)]
pub struct StatusMessage {
    /// Severity.
    pub severity: Severity,
    /// Text.
    pub text: String,
}

impl StatusMessage {
    /// Informational message.
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            text: text.into(),
        }
    }

    /// Error message.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            text: text.into(),
        }
    }
}
