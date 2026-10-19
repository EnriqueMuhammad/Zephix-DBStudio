//! Application shell: menu bar, toolbar, explorer, dock workspace, status bar, dialogs.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{
    Button, CentralPanel, Color32, Frame, Id, Key, KeyboardShortcut, Margin, Modifiers, RichText,
    Stroke, Ui, Vec2,
};
use egui_dock::DockArea;
use egui_phosphor::regular as icon;
use rdbs_core::sql::StatementKind;
use rdbs_core::{
    AdapterFactory, Command, ConnectionProfile, CredentialStore, DbErrorKind, Environment, Event,
    ProfileId, SecretKind, dialect,
};
use rdbs_services::ServiceHandle;
use rdbs_storage::{ProfileStore, default_db_path};

use crate::dialog::{ConnectionDialog, DialogAction, TestState};
use crate::editor;
use crate::explorer::{self, ExplorerContext, ExplorerState};
use crate::grid::GridState;
use crate::model::{AfterPassword, ConnState, ConnStatus, Severity, StatusMessage, UiAction};
use crate::theme::{self, color};
use crate::widgets::{self, ToolbarButton};
use crate::workspace::{LastRun, Tab, TabContext, Workspace};

/// Status text shown while an object tree reloads.
const REFRESHING: &str = "Refreshing objects…";

/// Storage key of the persisted [`Workspace`] (separate keys keep demo tabs out of live mode).
const WORKSPACE_KEY: &str = "rdbs.workspace";
const DEMO_WORKSPACE_KEY: &str = "rdbs.workspace.demo";
/// Persisted theme choice (`true` = light).
const LIGHT_THEME_KEY: &str = "rdbs.light_theme";

const SC_NEW_CONNECTION: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const SC_NEW_QUERY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::T);
const SC_CLOSE_TAB: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::W);
const SC_SIDEBAR: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::B);
const SC_FILTER: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);
const SC_RUN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);
const SC_RUN_ALT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::R);
const SC_REFRESH: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F5);

/// Error type returned while creating the app.
pub type InitError = Box<dyn std::error::Error + Send + Sync>;

/// How the app connects and where it keeps its data; chosen by the binary.
pub struct AppConfig {
    /// Opens database sessions (live drivers or the mock engine).
    pub factory: Arc<dyn AdapterFactory>,
    /// Where saved passwords live (OS keychain in live mode).
    pub credentials: Arc<dyn CredentialStore>,
    /// `Some` runs in demo mode: profiles live in memory, seeded with these.
    pub demo_profiles: Option<Vec<ConnectionProfile>>,
}

/// The password prompt shown before connecting a profile without a saved password.
struct PasswordPrompt {
    profile_id: ProfileId,
    password: String,
    remember: bool,
    then: AfterPassword,
    /// Why the prompt appeared (e.g. the previous attempt was rejected).
    reason: Option<String>,
    focus: bool,
}

/// A pending confirmation dialog.
enum Confirmation {
    Execute {
        tab_id: u64,
        profile_id: ProfileId,
        sql: String,
        kind: StatementKind,
    },
    DeleteProfile(ProfileId),
    ClearHistory,
    SaveEdits {
        tab_id: u64,
        rows: usize,
    },
}

/// Finds a table node by database / schema / name in an explorer tree.
fn find_table<'a>(
    objects: &'a [rdbs_core::DbObject],
    database: Option<&str>,
    schema: Option<&str>,
    name: &str,
) -> Option<&'a rdbs_core::DbObject> {
    use rdbs_core::ObjectKind;
    fn walk<'a>(
        obj: &'a rdbs_core::DbObject,
        database: Option<&str>,
        schema: Option<&str>,
        name: &str,
    ) -> Option<&'a rdbs_core::DbObject> {
        match obj.kind {
            ObjectKind::Database if database.is_some_and(|d| d != obj.name) => None,
            ObjectKind::Schema if schema.is_some_and(|s| s != obj.name) => None,
            ObjectKind::Table if obj.name == name => Some(obj),
            _ => obj
                .children
                .iter()
                .find_map(|c| walk(c, database, schema, name)),
        }
    }
    objects.iter().find_map(|o| walk(o, database, schema, name))
}

/// History record for a finished execution.
fn history_entry(
    running: &crate::workspace::Running,
    elapsed: std::time::Duration,
    outcome: &Result<rdbs_core::QueryOutcome, rdbs_core::DbError>,
) -> rdbs_storage::NewHistoryEntry {
    use rdbs_storage::HistoryStatus;
    let (status, rows, error) = match outcome {
        Ok(o) => (
            HistoryStatus::Ok,
            Some(if o.results.is_empty() {
                o.rows_affected.unwrap_or(0)
            } else {
                o.total_rows() as u64
            }),
            None,
        ),
        Err(e) if e.kind == DbErrorKind::Cancelled => (HistoryStatus::Cancelled, None, None),
        Err(e) => (HistoryStatus::Error, None, Some(e.to_string())),
    };
    rdbs_storage::NewHistoryEntry {
        profile_id: running.profile_id,
        profile_name: running.profile_name.clone(),
        database: running.database.clone(),
        sql: running.sql.clone(),
        elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        status,
        rows,
        error,
    }
}

/// The Zephix application.
pub struct StudioApp {
    /// Keeps the window on its monitor.
    window_fit: crate::window::WindowFit,
    services: ServiceHandle,
    store: ProfileStore,
    profiles: Vec<ConnectionProfile>,
    connections: HashMap<ProfileId, ConnState>,
    explorer: ExplorerState,
    workspace: Workspace,
    dialog: Option<ConnectionDialog>,
    confirm: Option<Confirmation>,
    status: Option<StatusMessage>,
    show_about: bool,
    credentials: Arc<dyn CredentialStore>,
    /// Passwords typed for this run only (profiles that do not save their password).
    session_passwords: HashMap<ProfileId, String>,
    prompt: Option<PasswordPrompt>,
    demo: bool,
    /// Results of background "Save as…" exports.
    export_tx: std::sync::mpsc::Sender<StatusMessage>,
    export_rx: std::sync::mpsc::Receiver<StatusMessage>,
    egui_ctx: egui::Context,
    /// Cached history rows for the history tab, and the filter they match.
    history: Vec<rdbs_storage::HistoryEntry>,
    history_filter: String,
    /// DDL requests in flight.
    ddl_requests: HashMap<rdbs_core::RequestId, DdlRequest>,
    /// Routine parameter requests in flight, for the Execute dialog.
    routine_requests: HashMap<rdbs_core::RequestId, RoutineTarget>,
    /// Execute dialog of a function or procedure.
    exec: Option<ExecDialog>,
    /// Backup schedules and running backups.
    backups: crate::model::BackupState,
}

/// A DDL request in flight.
struct DdlRequest {
    profile_id: ProfileId,
    database: Option<String>,
    title: String,
    /// Set when the definition is opened for editing.
    edit: Option<RoutineTarget>,
}

/// A function or procedure being edited or executed.
#[derive(Clone)]
struct RoutineTarget {
    profile_id: ProfileId,
    database: Option<String>,
    kind: rdbs_core::DatabaseKind,
    qualified: String,
    name: String,
    routine: rdbs_core::ObjectKind,
}

/// Parameter values of a routine about to be executed.
struct ExecDialog {
    target: RoutineTarget,
    signature: rdbs_core::routine::RoutineSignature,
    values: Vec<String>,
    nulls: Vec<bool>,
}

impl StudioApp {
    /// Creates the app: applies the theme, starts the worker, opens local storage and restores
    /// the persisted workspace.
    pub fn new(cc: &eframe::CreationContext<'_>, config: AppConfig) -> Result<Self, InitError> {
        let light = cc
            .storage
            .and_then(|s| eframe::get_value::<bool>(s, LIGHT_THEME_KEY))
            .unwrap_or(false);
        theme::apply(&cc.egui_ctx, light);

        let repaint_ctx = cc.egui_ctx.clone();
        let services =
            ServiceHandle::spawn(config.factory, config.credentials.clone(), move || {
                repaint_ctx.request_repaint();
            })?;

        let mut status = None;
        let demo = config.demo_profiles.is_some();
        let store = if let Some(seed) = config.demo_profiles {
            let store = ProfileStore::open_in_memory()?;
            for profile in seed {
                store.upsert(&profile)?;
            }
            store
        } else {
            match default_db_path().and_then(|path| ProfileStore::open(&path)) {
                Ok(store) => store,
                Err(err) => {
                    tracing::warn!("falling back to in-memory profile store: {err}");
                    status = Some(StatusMessage::error(format!(
                        "Local storage unavailable ({err}); changes will not be saved"
                    )));
                    ProfileStore::open_in_memory()?
                }
            }
        };
        let profiles = store.list()?;
        let schedules = store.schedules().unwrap_or_else(|e| {
            tracing::warn!("could not load backup schedules: {e}");
            Vec::new()
        });
        let (export_tx, export_rx) = std::sync::mpsc::channel();

        let key = if demo {
            DEMO_WORKSPACE_KEY
        } else {
            WORKSPACE_KEY
        };
        let mut workspace: Workspace = cc
            .storage
            .and_then(|s| eframe::get_value(s, key))
            .unwrap_or_default();
        // Every start opens on the home page; the other restored tabs stay behind it.
        workspace.show_welcome();

        Ok(Self {
            window_fit: crate::window::WindowFit::default(),
            services,
            store,
            profiles,
            connections: HashMap::new(),
            explorer: ExplorerState::default(),
            workspace,
            dialog: None,
            confirm: None,
            status,
            show_about: false,
            credentials: config.credentials,
            session_passwords: HashMap::new(),
            prompt: None,
            demo,
            export_tx,
            export_rx,
            egui_ctx: cc.egui_ctx.clone(),
            history: Vec::new(),
            history_filter: String::new(),
            ddl_requests: HashMap::new(),
            routine_requests: HashMap::new(),
            exec: None,
            backups: crate::model::BackupState {
                schedules,
                ..crate::model::BackupState::default()
            },
        })
        .map(|mut app| {
            app.reload_history();
            app
        })
    }

    /// Re-reads history for the current filter.
    fn reload_history(&mut self) {
        match self.store.recent_queries(&self.history_filter, 1_000) {
            Ok(rows) => self.history = rows,
            Err(e) => tracing::warn!("could not load history: {e}"),
        }
    }

    /// Stores a finished execution in the history and refreshes the history tab.
    fn record_history(&mut self, entry: rdbs_storage::NewHistoryEntry) {
        if let Err(e) = self.store.record_query(&entry) {
            tracing::warn!("could not record history: {e}");
        }
        self.reload_history();
    }

    fn profile(&self, id: ProfileId) -> Option<&ConnectionProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    fn reload_profiles(&mut self) {
        match self.store.list() {
            Ok(list) => self.profiles = list,
            Err(e) => {
                self.status = Some(StatusMessage::error(format!(
                    "Could not load connections: {e}"
                )))
            }
        }
    }

    // ----------------------------------------------------------------------------------
    // Worker events

    fn drain_events(&mut self) {
        while let Some(event) = self.services.try_recv() {
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: Event) {
        tracing::debug!(?event, "worker event");
        if self.handle_backup_event(&event) {
            return;
        }
        // Tool tabs consume the results of their own requests.
        if matches!(
            event,
            Event::ChangesApplied { .. } | Event::QueryFinished { .. }
        ) && self.workspace.dispatch_tool_event(&event)
        {
            return;
        }
        let name_of = |profiles: &[ConnectionProfile], id: ProfileId| {
            profiles
                .iter()
                .find(|p| p.id == id)
                .map_or_else(|| "connection".to_owned(), |p| p.name.clone())
        };
        match event {
            Event::Connecting { profile_id } => {
                self.connections.entry(profile_id).or_default().status = ConnStatus::Connecting;
            }
            Event::Connected {
                profile_id,
                server_version,
                capabilities,
            } => {
                self.status = Some(StatusMessage::info(format!(
                    "Connected to {} · {server_version}",
                    name_of(&self.profiles, profile_id)
                )));
                self.connections.entry(profile_id).or_default().status = ConnStatus::Connected {
                    server_version,
                    capabilities,
                };
            }
            Event::ConnectionFailed { profile_id, error } => {
                self.status = Some(StatusMessage::error(format!(
                    "{}: {error}",
                    name_of(&self.profiles, profile_id)
                )));
                if error.kind == DbErrorKind::Authentication {
                    // A rejected password must not be retried silently.
                    self.session_passwords.remove(&profile_id);
                    if self.prompt.is_none() && self.dialog.is_none() {
                        self.open_prompt(
                            profile_id,
                            AfterPassword::Connect,
                            Some(error.message.clone()),
                        );
                    }
                }
                let conn = self.connections.entry(profile_id).or_default();
                conn.status = ConnStatus::Failed(error);
                conn.objects.clear();
            }
            Event::Disconnected { profile_id } => {
                if let Some(conn) = self.connections.get_mut(&profile_id) {
                    *conn = ConnState::default();
                }
                self.explorer.collapse_profile(profile_id);
                self.status = Some(StatusMessage::info(format!(
                    "Disconnected from {}",
                    name_of(&self.profiles, profile_id)
                )));
            }
            Event::ObjectsLoaded {
                profile_id,
                objects,
            } => {
                let conn = self.connections.entry(profile_id).or_default();
                conn.default_database = objects
                    .iter()
                    .find(|o| o.kind == rdbs_core::ObjectKind::Database && !o.children.is_empty())
                    .map(|o| o.name.clone());
                conn.loading_databases.clear();
                conn.objects = objects;
                if self.status.as_ref().is_some_and(|s| s.text == REFRESHING) {
                    self.status = Some(StatusMessage::info("Objects refreshed"));
                }
            }
            Event::ObjectsFailed { profile_id, error } => {
                self.status = Some(StatusMessage::error(format!(
                    "Could not load objects of {}: {error}",
                    name_of(&self.profiles, profile_id)
                )));
            }
            Event::DatabaseObjectsLoaded {
                profile_id,
                database,
                objects,
            } => {
                if let Some(conn) = self.connections.get_mut(&profile_id) {
                    conn.loading_databases.remove(&database);
                    if let Some(node) = conn.objects.iter_mut().find(|o| o.name == database) {
                        node.children = objects;
                        node.detail = None;
                    }
                }
            }
            Event::DatabaseObjectsFailed {
                profile_id,
                database,
                error,
            } => {
                if let Some(conn) = self.connections.get_mut(&profile_id) {
                    conn.loading_databases.remove(&database);
                }
                self.status = Some(StatusMessage::error(format!(
                    "Could not open database {database}: {error}"
                )));
            }
            Event::ChangesApplied { request_id, result } => {
                let mut rerun = None;
                if let Some(q) = self.workspace.tab_for_save(request_id) {
                    q.run.saving = None;
                    match result {
                        Ok(rows) => {
                            q.run.edits = rdbs_core::edit::PendingEdits::default();
                            q.run.editing = None;
                            q.run.edit_error = None;
                            self.status = Some(StatusMessage::info(format!(
                                "Saved changes to {rows} row(s)"
                            )));
                            rerun = Some(q.id);
                        }
                        Err(e) => {
                            q.run.edit_error = Some(e.to_string());
                            self.status =
                                Some(StatusMessage::error(format!("Changes were not saved: {e}")));
                        }
                    }
                }
                if let Some(tab_id) = rerun {
                    let follow_up = self.run_tab(tab_id);
                    self.apply(follow_up);
                }
            }
            Event::DdlLoaded { request_id, result } => {
                if let Some(request) = self.ddl_requests.remove(&request_id) {
                    match result {
                        Ok(ddl) => {
                            let text = match &request.edit {
                                Some(t) => rdbs_core::routine::edit_script(
                                    t.kind,
                                    &t.qualified,
                                    t.routine,
                                    &ddl,
                                ),
                                None => ddl,
                            };
                            let id = self.workspace.open_query_tab(
                                Some(request.profile_id),
                                text,
                                Some(request.title),
                            );
                            if let Some(q) = self.workspace.query_tab_mut(id) {
                                q.database = request.database;
                            }
                            self.status = None;
                        }
                        Err(e) => {
                            self.status = Some(StatusMessage::error(format!(
                                "Could not load the definition: {e}"
                            )));
                        }
                    }
                }
            }
            Event::RoutineLoaded { request_id, result } => {
                if let Some(target) = self.routine_requests.remove(&request_id) {
                    match result {
                        Ok(signature) => {
                            self.status = None;
                            if signature.params.iter().any(|p| p.mode.takes_value()) {
                                let n = signature.params.len();
                                self.exec = Some(ExecDialog {
                                    target,
                                    signature,
                                    values: vec![String::new(); n],
                                    nulls: vec![false; n],
                                });
                            } else {
                                let sql = rdbs_core::routine::call_statement(
                                    target.kind,
                                    &target.qualified,
                                    target.routine,
                                    &signature,
                                    &[],
                                );
                                self.run_routine(&target, sql);
                            }
                        }
                        Err(e) => {
                            self.status = Some(StatusMessage::error(format!(
                                "Could not load the parameters of {}: {e}",
                                target.name
                            )));
                        }
                    }
                }
            }
            Event::PlanReady { request_id, result } => {
                if let Some(q) = self.workspace.tab_for_plan(request_id) {
                    q.run.plan_request = None;
                    q.run.plan = Some(result);
                    q.run.view = crate::workspace::ResultView::Plan;
                }
            }
            Event::SchemaDescribed { .. }
            | Event::ScriptFinished { .. }
            | Event::JobProgress { .. } => {
                self.dispatch_tool_event(event);
            }
            Event::JobFinished { .. } => {
                // The tool tab shows the details; the status bar replaces "... started".
                if self.workspace.dispatch_tool_event(&event)
                    && let Event::JobFinished { result, .. } = &event
                {
                    self.status = Some(match result {
                        Ok(summary) => StatusMessage::info(format!("Finished: {summary}")),
                        Err(e) => {
                            StatusMessage::error(format!("Failed: {}", crate::tools::error_text(e)))
                        }
                    });
                } else {
                    tracing::debug!("no tab waits for this tool result (closed?)");
                }
            }
            Event::QueryStarted { .. } => {
                // A new run supersedes an earlier error such as "choose a connection first".
                if self
                    .status
                    .as_ref()
                    .is_some_and(|s| s.severity == Severity::Error)
                {
                    self.status = None;
                }
            }
            Event::QueryFinished {
                request_id,
                elapsed,
                result,
            } => {
                let mut finished = None;
                if let Some(q) = self.workspace.tab_for_request(request_id) {
                    let running = q.run.running.take();
                    let profile_name = running
                        .as_ref()
                        .map(|r| r.profile_name.clone())
                        .unwrap_or_default();
                    let has_results = result.as_ref().is_ok_and(|o| !o.results.is_empty());
                    let last = LastRun {
                        elapsed,
                        outcome: result,
                        profile_name,
                    };
                    q.run.grid = GridState::default();
                    q.run.stacked_grids.clear();
                    q.run.show_messages = !has_results;
                    q.run.active_result = 0;
                    if let Some(running) = &running {
                        finished = Some(history_entry(running, last.elapsed, &last.outcome));
                    }
                    q.run.last = Some(last);
                }
                if let Some(entry) = finished {
                    self.record_history(entry);
                }
            }
            Event::TestFinished { request_id, result } => {
                if let Some(dialog) = &mut self.dialog
                    && matches!(dialog.test, TestState::Running(id) if id == request_id)
                {
                    dialog.test = match result {
                        Ok(version) => TestState::Succeeded(version),
                        Err(e) => TestState::Failed(e),
                    };
                }
            }
        }
    }

    /// Progress and results of backup jobs started by the app.
    fn handle_backup_event(&mut self, event: &Event) -> bool {
        match event {
            Event::JobProgress {
                request_id,
                message,
                ..
            } => match self.backups.running.get(request_id) {
                Some(id) => {
                    self.backups.status.insert(id.clone(), message.clone());
                    true
                }
                None => false,
            },
            Event::JobFinished { request_id, result } => {
                let Some(id) = self.backups.running.remove(request_id) else {
                    return false;
                };
                self.backups.status.remove(&id);
                let outcome = result.clone().map_err(|e| crate::tools::error_text(&e));
                self.status = Some(match &outcome {
                    Ok(summary) => StatusMessage::info(format!("Backup finished: {summary}")),
                    Err(e) => StatusMessage::error(format!("Backup failed: {e}")),
                });
                if id.is_empty() {
                    self.backups.adhoc_result = Some(outcome);
                } else if let Some(s) = self.backups.schedules.iter_mut().find(|s| s.id == id) {
                    s.last_result = Some(outcome);
                    let saved = s.clone();
                    if let Err(e) = self.store.save_schedule(&saved) {
                        tracing::warn!("could not save backup schedule: {e}");
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Starts a backup job; `schedule` is `""` for a one-off backup.
    fn start_backup(
        &mut self,
        schedule: String,
        profile_id: ProfileId,
        database: Option<String>,
        options: rdbs_core::job::BackupOptions,
    ) -> Result<(), String> {
        let profile = self
            .profile(profile_id)
            .cloned()
            .ok_or_else(|| "the connection no longer exists".to_owned())?;
        let password = self.session_passwords.get(&profile_id).cloned();
        if profile.kind.is_networked() && !profile.save_password && password.is_none() {
            return Err(format!(
                "{} does not save its password; connect once in this session or save the password",
                profile.name
            ));
        }
        let request_id = self.services.next_request_id();
        self.services.send(Command::Backup {
            request_id,
            profile,
            database,
            password,
            options,
        });
        self.backups
            .status
            .insert(schedule.clone(), "Starting…".into());
        if schedule.is_empty() {
            self.backups.adhoc_result = None;
        }
        self.backups.running.insert(request_id, schedule);
        Ok(())
    }

    /// Runs due schedules (checked every 20 seconds).
    fn tick_schedules(&mut self, ctx: &egui::Context) {
        if !self.backups.schedules.iter().any(|s| s.enabled) {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(20));
        if self
            .backups
            .last_check
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(20))
        {
            return;
        }
        self.backups.last_check = Some(std::time::Instant::now());
        let now = chrono::Local::now();
        let due: Vec<String> = self
            .backups
            .schedules
            .iter()
            .filter(|s| s.enabled && !self.backups.status.contains_key(&s.id))
            .filter(|s| rdbs_transfer::backup::is_due(s.frequency, s.last_run, now))
            .map(|s| s.id.clone())
            .collect();
        for id in due {
            self.run_schedule(&id);
        }
    }

    fn run_schedule(&mut self, id: &str) {
        let Some(s) = self.backups.schedules.iter_mut().find(|s| s.id == id) else {
            return;
        };
        s.last_run = chrono::Local::now().timestamp();
        let (profile_id, database, options) = (s.profile_id, s.database.clone(), s.options.clone());
        let started = self.start_backup(id.to_owned(), profile_id, database, options);
        if let Some(s) = self.backups.schedules.iter_mut().find(|s| s.id == id) {
            if let Err(e) = started {
                s.last_result = Some(Err(e));
            }
            let saved = s.clone();
            if let Err(e) = self.store.save_schedule(&saved) {
                tracing::warn!("could not save backup schedule: {e}");
            }
        }
    }

    /// Routes results of tool requests (designer, diff, …) to the tab that sent them.
    fn dispatch_tool_event(&mut self, event: Event) {
        if !self.workspace.dispatch_tool_event(&event) {
            tracing::debug!("no tab waits for this tool result (closed?)");
        }
    }

    // ----------------------------------------------------------------------------------
    // Keyboard

    fn shortcuts(&mut self, ctx: &egui::Context, actions: &mut Vec<UiAction>) {
        if self.dialog.is_some()
            || self.confirm.is_some()
            || self.show_about
            || self.prompt.is_some()
            || self.exec.is_some()
        {
            return; // Modals own the keyboard.
        }
        let pressed = |sc: &KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(sc));
        let new_connection = pressed(&SC_NEW_CONNECTION);
        let new_query = pressed(&SC_NEW_QUERY);
        let close_tab = pressed(&SC_CLOSE_TAB);
        let sidebar = pressed(&SC_SIDEBAR);
        let filter = pressed(&SC_FILTER);
        let run = pressed(&SC_RUN) | pressed(&SC_RUN_ALT);
        let refresh = pressed(&SC_REFRESH);

        if new_connection {
            actions.push(UiAction::NewConnection);
        }
        if new_query {
            actions.push(UiAction::NewQuery {
                profile_id: None,
                sql: None,
                title: None,
                run: false,
            });
        }
        if close_tab {
            self.close_active_tab();
        }
        if sidebar {
            actions.push(UiAction::ToggleSidebar);
        }
        if filter {
            self.workspace.sidebar_visible = true;
            self.explorer.focus_filter = true;
        }
        if run {
            self.run_active_tab(actions);
        }
        if refresh && let Some(id) = self.explorer.selected_profile {
            actions.push(UiAction::Refresh(id));
        }
    }

    fn close_active_tab(&mut self) {
        if let Some(Tab::Query(q)) = self.workspace.close_active_tab()
            && let Some(r) = q.run.running
        {
            self.services.send(Command::Cancel {
                request_id: r.request_id,
            });
        }
    }

    fn run_active_tab(&mut self, actions: &mut Vec<UiAction>) {
        let Self {
            workspace,
            profiles,
            connections,
            services,
            session_passwords,
            history,
            backups,
            ..
        } = self;
        if let Some(q) = workspace.active_query_tab() {
            let mut cx = TabContext {
                profiles,
                connections,
                services,
                passwords: session_passwords,
                history,
                backups,
                actions,
            };
            editor::request_run(q, &mut cx);
        }
    }

    fn run_tab(&mut self, tab_id: u64) -> Vec<UiAction> {
        let mut actions = Vec::new();
        let Self {
            workspace,
            profiles,
            connections,
            services,
            session_passwords,
            history,
            backups,
            ..
        } = self;
        if let Some(q) = workspace.query_tab_mut(tab_id) {
            let mut cx = TabContext {
                profiles,
                connections,
                services,
                passwords: session_passwords,
                history,
                backups,
                actions: &mut actions,
            };
            editor::request_run(q, &mut cx);
        }
        actions
    }

    // ----------------------------------------------------------------------------------
    // Actions

    fn apply(&mut self, actions: Vec<UiAction>) {
        for action in actions {
            tracing::debug!(?action, "ui action");
            match action {
                UiAction::NewQuery {
                    profile_id,
                    sql,
                    title,
                    run,
                } => {
                    let profile_id = profile_id.or(self.explorer.selected_profile);
                    let id =
                        self.workspace
                            .open_query_tab(profile_id, sql.unwrap_or_default(), title);
                    if run {
                        let follow_up = self.run_tab(id);
                        self.apply(follow_up);
                    }
                }
                UiAction::OpenObject(obj) => {
                    let Some(kind) = self.profile(obj.profile_id).map(|p| p.kind) else {
                        continue;
                    };
                    if kind.is_sql() {
                        self.open_data_tab(&obj, kind);
                        continue;
                    }
                    let qualified = dialect::qualified_name(
                        kind,
                        obj.database.as_deref(),
                        obj.schema.as_deref(),
                        &obj.name,
                    );
                    let sql = dialect::select_preview(kind, &qualified, 1000);
                    // Objects of a non-default database run in a session for that database.
                    let database = obj.database.clone().filter(|db| {
                        kind.has_schemas()
                            && self
                                .connections
                                .get(&obj.profile_id)
                                .and_then(|c| c.default_database.as_ref())
                                != Some(db)
                    });
                    let table = self.table_target(&obj, kind);
                    let id = self.workspace.open_query_tab(
                        Some(obj.profile_id),
                        sql,
                        Some(obj.name.clone()),
                    );
                    if let Some(q) = self.workspace.query_tab_mut(id) {
                        q.database = database;
                        q.table = table;
                    }
                    let follow_up = self.run_tab(id);
                    self.apply(follow_up);
                }
                UiAction::ShowDdl(obj) => self.request_ddl(obj, false),
                UiAction::EditRoutine(obj) => self.request_ddl(obj, true),
                UiAction::MoveConnection { id, target, below } => {
                    self.move_connection(id, target, below);
                }
                UiAction::ExecuteRoutine(obj) => self.request_routine(obj),
                UiAction::SelectTop { obj, rows } => self.open_select(&obj, Some(rows)),
                UiAction::OpenTool(launch) => self.workspace.open_tool(launch),
                UiAction::SaveSchedule(schedule) => match self.store.save_schedule(&schedule) {
                    Ok(()) => {
                        match self
                            .backups
                            .schedules
                            .iter_mut()
                            .find(|s| s.id == schedule.id)
                        {
                            Some(existing) => *existing = schedule,
                            None => self.backups.schedules.push(schedule),
                        }
                        self.status = Some(StatusMessage::info("Backup schedule saved"));
                    }
                    Err(e) => {
                        self.status = Some(StatusMessage::error(format!(
                            "Could not save the schedule: {e}"
                        )));
                    }
                },
                UiAction::DeleteSchedule(id) => {
                    if let Err(e) = self.store.delete_schedule(&id) {
                        self.status = Some(StatusMessage::error(format!(
                            "Could not delete the schedule: {e}"
                        )));
                    } else {
                        self.backups.schedules.retain(|s| s.id != id);
                    }
                }
                UiAction::RunSchedule(id) => self.run_schedule(&id),
                UiAction::RunBackup {
                    profile_id,
                    database,
                    options,
                } => {
                    if let Err(e) = self.start_backup(String::new(), profile_id, database, options)
                    {
                        self.backups.adhoc_result = Some(Err(e));
                    }
                }
                UiAction::Connect(id) => self.connect(id),
                UiAction::Disconnect(id) => {
                    self.services.send(Command::Disconnect { profile_id: id })
                }
                UiAction::Refresh(id) => {
                    if self
                        .connections
                        .get(&id)
                        .is_some_and(ConnState::is_connected)
                    {
                        self.services
                            .send(Command::RefreshObjects { profile_id: id });
                        self.status = Some(StatusMessage::info(REFRESHING));
                    } else {
                        self.connect(id);
                    }
                }
                UiAction::NewConnection => self.dialog = Some(ConnectionDialog::new_profile()),
                UiAction::EditConnection(id) => {
                    if let Some(p) = self.profile(id) {
                        self.dialog = Some(ConnectionDialog::edit(p));
                    }
                }
                UiAction::DuplicateConnection(id) => {
                    if let Some(p) = self.profile(id) {
                        let mut copy = p.clone();
                        copy.id = ProfileId::new();
                        copy.name = format!("{} (copy)", p.name);
                        let password = if p.save_password {
                            self.credentials
                                .get(id, SecretKind::Database)
                                .ok()
                                .flatten()
                        } else {
                            None
                        };
                        self.save_profile(copy, password);
                    }
                }
                UiAction::DeleteConnection(id) => {
                    self.confirm = Some(Confirmation::DeleteProfile(id));
                }
                UiAction::ConfirmExecute {
                    tab_id,
                    profile_id,
                    sql,
                    kind,
                } => {
                    self.confirm = Some(Confirmation::Execute {
                        tab_id,
                        profile_id,
                        sql,
                        kind,
                    });
                }
                UiAction::LoadDatabase {
                    profile_id,
                    database,
                } => {
                    let Some(profile) = self.profile(profile_id).cloned() else {
                        continue;
                    };
                    if let Some(conn) = self.connections.get_mut(&profile_id) {
                        conn.loading_databases.insert(database.clone());
                    }
                    self.services.send(Command::LoadDatabase {
                        profile,
                        database,
                        password: self.session_passwords.get(&profile_id).cloned(),
                    });
                }
                UiAction::NeedPassword { profile_id, then } => {
                    self.open_prompt(profile_id, then, None);
                }
                UiAction::OpenScript {
                    profile_id,
                    database,
                    sql,
                    run,
                } => {
                    let id = self.workspace.open_query_tab(Some(profile_id), sql, None);
                    if let Some(q) = self.workspace.query_tab_mut(id) {
                        q.database = database;
                    }
                    if run {
                        let follow_up = self.run_tab(id);
                        self.apply(follow_up);
                    }
                }
                UiAction::SaveEdits { tab_id } => self.save_edits(tab_id, false),
                UiAction::ExportActive => self.export_active(),
                UiAction::ShowHistory => {
                    self.reload_history();
                    self.workspace.show_history();
                }
                UiAction::ReloadHistory(filter) => {
                    self.history_filter = filter;
                    self.reload_history();
                }
                UiAction::ClearHistory => self.confirm = Some(Confirmation::ClearHistory),
                UiAction::SaveExport {
                    contents,
                    file_name,
                    format,
                } => self.save_export(contents, file_name, format),
                UiAction::Status(message) => self.status = Some(message),
                UiAction::ToggleSidebar => {
                    self.workspace.sidebar_visible = !self.workspace.sidebar_visible;
                }
            }
        }
    }

    fn connect(&mut self, id: ProfileId) {
        let Some(profile) = self.profile(id).cloned() else {
            return;
        };
        if editor::needs_password(&profile, &self.connections, &self.session_passwords) {
            self.open_prompt(id, AfterPassword::Connect, None);
            return;
        }
        self.connections.entry(id).or_default().status = ConnStatus::Connecting;
        self.explorer.expand_profile(id);
        self.services.send(Command::Connect {
            profile,
            password: self.session_passwords.get(&id).cloned(),
        });
    }

    /// Edit target for a table opened from the explorer, with its primary key read from the
    /// object tree (columns marked "PK"). Views are not editable.
    fn table_target(
        &self,
        obj: &crate::model::ObjectRef,
        kind: rdbs_core::DatabaseKind,
    ) -> Option<rdbs_core::edit::TableTarget> {
        if obj.kind != rdbs_core::ObjectKind::Table {
            return None;
        }
        let objects = &self.connections.get(&obj.profile_id)?.objects;
        let node = find_table(
            objects,
            obj.database.as_deref(),
            obj.schema.as_deref(),
            &obj.name,
        )?;
        let primary_key = node
            .children
            .iter()
            .filter(|c| {
                c.kind == rdbs_core::ObjectKind::Column
                    && c.detail.as_deref().is_some_and(|d| d.contains("· PK"))
            })
            .map(|c| c.name.clone())
            .collect();
        Some(rdbs_core::edit::TableTarget {
            kind,
            database: obj.database.clone(),
            schema: obj.schema.clone(),
            table: obj.name.clone(),
            primary_key,
        })
    }

    /// Database override for an object of a non-default database (PostgreSQL / SQL Server).
    fn database_override(
        &self,
        obj: &crate::model::ObjectRef,
        kind: rdbs_core::DatabaseKind,
    ) -> Option<String> {
        obj.database.clone().filter(|db| {
            kind.has_schemas()
                && self
                    .connections
                    .get(&obj.profile_id)
                    .and_then(|c| c.default_database.as_ref())
                    != Some(db)
        })
    }

    /// Opens the rows of a table or view in a data tab (or focuses the one already open).
    fn open_data_tab(&mut self, obj: &crate::model::ObjectRef, kind: rdbs_core::DatabaseKind) {
        let qualified = dialect::qualified_name(
            kind,
            obj.database.as_deref(),
            obj.schema.as_deref(),
            &obj.name,
        );
        let existing = self.workspace.dock.find_tab_from(|t| {
            matches!(t, crate::workspace::Tab::Query(q)
                if q.profile_id == Some(obj.profile_id)
                    && q.data_view.as_ref().is_some_and(|v| v.qualified == qualified))
        });
        if let Some(path) = existing {
            let _ = self.workspace.dock.set_active_tab(path);
            return;
        }
        let database = self.database_override(obj, kind);
        let table = self.table_target(obj, kind);
        let view = crate::workspace::DataView {
            kind,
            qualified,
            filter: String::new(),
            order: String::new(),
            limit: Some(1000),
        };
        let id =
            self.workspace
                .open_query_tab(Some(obj.profile_id), view.sql(), Some(obj.name.clone()));
        if let Some(q) = self.workspace.query_tab_mut(id) {
            q.database = database;
            q.table = table;
            q.data_view = Some(view);
        }
        let follow_up = self.run_tab(id);
        self.apply(follow_up);
    }

    /// Opens a query tab selecting the first `rows` rows of a table or view and runs it.
    fn open_select(&mut self, obj: &crate::model::ObjectRef, rows: Option<u32>) {
        let Some(kind) = self.profile(obj.profile_id).map(|p| p.kind) else {
            return;
        };
        let qualified = dialect::qualified_name(
            kind,
            obj.database.as_deref(),
            obj.schema.as_deref(),
            &obj.name,
        );
        let sql = dialect::select_preview(kind, &qualified, rows.unwrap_or(1000));
        let database = self.database_override(obj, kind);
        let table = self.table_target(obj, kind);
        let id = self
            .workspace
            .open_query_tab(Some(obj.profile_id), sql, Some(obj.name.clone()));
        if let Some(q) = self.workspace.query_tab_mut(id) {
            q.database = database;
            q.table = table;
        }
        let follow_up = self.run_tab(id);
        self.apply(follow_up);
    }

    /// Moves a connection above or below another one and stores the new order.
    fn move_connection(&mut self, id: ProfileId, target: ProfileId, below: bool) {
        let Some(from) = self.profiles.iter().position(|p| p.id == id) else {
            return;
        };
        let moved = self.profiles.remove(from);
        let Some(to) = self.profiles.iter().position(|p| p.id == target) else {
            self.profiles.insert(from, moved);
            return;
        };
        self.profiles.insert(if below { to + 1 } else { to }, moved);
        let ids: Vec<ProfileId> = self.profiles.iter().map(|p| p.id).collect();
        if let Err(e) = self.store.reorder(&ids) {
            self.status = Some(StatusMessage::error(format!(
                "Could not save the connection order: {e}"
            )));
        }
    }

    /// Everything needed to edit or execute a routine.
    fn routine_target(
        &self,
        obj: &crate::model::ObjectRef,
        kind: rdbs_core::DatabaseKind,
    ) -> RoutineTarget {
        RoutineTarget {
            profile_id: obj.profile_id,
            database: self.database_override(obj, kind),
            kind,
            qualified: dialect::qualified_name(
                kind,
                obj.database.as_deref(),
                obj.schema.as_deref(),
                &obj.name,
            ),
            name: obj.name.clone(),
            routine: obj.kind,
        }
    }

    /// Fetches the parameters of a routine for the Execute dialog.
    fn request_routine(&mut self, obj: crate::model::ObjectRef) {
        let Some(profile) = self.profile(obj.profile_id).cloned() else {
            return;
        };
        let target = self.routine_target(&obj, profile.kind);
        let request_id = self.services.next_request_id();
        self.services.send(Command::GetRoutine {
            request_id,
            profile,
            database: target.database.clone(),
            object: rdbs_core::schema::ObjectPath {
                database: obj.database,
                schema: obj.schema,
                name: obj.name.clone(),
                kind: obj.kind,
            },
            password: self.session_passwords.get(&obj.profile_id).cloned(),
        });
        self.status = Some(StatusMessage::info(format!(
            "Loading the parameters of {}…",
            obj.name
        )));
        self.routine_requests.insert(request_id, target);
    }

    /// Opens a query tab with `sql` for a routine and runs it.
    fn run_routine(&mut self, target: &RoutineTarget, sql: String) {
        let id = self.workspace.open_query_tab(
            Some(target.profile_id),
            sql,
            Some(format!("Exec {}", target.name)),
        );
        if let Some(q) = self.workspace.query_tab_mut(id) {
            q.database = target.database.clone();
        }
        let follow_up = self.run_tab(id);
        self.apply(follow_up);
    }

    fn request_ddl(&mut self, obj: crate::model::ObjectRef, edit: bool) {
        let Some(profile) = self.profile(obj.profile_id).cloned() else {
            return;
        };
        let database = self.database_override(&obj, profile.kind);
        let request_id = self.services.next_request_id();
        let edit = edit.then(|| self.routine_target(&obj, profile.kind));
        let title = if edit.is_some() {
            format!("Edit {}", obj.name)
        } else {
            format!("DDL · {}", obj.name)
        };
        self.ddl_requests.insert(
            request_id,
            DdlRequest {
                profile_id: obj.profile_id,
                database: database.clone(),
                title,
                edit,
            },
        );
        self.status = Some(StatusMessage::info(format!(
            "Loading the definition of {}…",
            obj.name
        )));
        self.services.send(Command::GetDdl {
            request_id,
            profile,
            database,
            object: rdbs_core::schema::ObjectPath {
                database: obj.database,
                schema: obj.schema,
                name: obj.name,
                kind: obj.kind,
            },
            password: self.session_passwords.get(&obj.profile_id).cloned(),
        });
    }

    /// Writes the pending edits of a tab in one transaction (confirming on production).
    fn save_edits(&mut self, tab_id: u64, confirmed: bool) {
        let Self {
            workspace,
            profiles,
            services,
            session_passwords,
            confirm,
            ..
        } = self;
        let Some(q) = workspace.query_tab_mut(tab_id) else {
            return;
        };
        let (Some(table), Some(profile)) = (
            q.table.clone(),
            q.profile_id
                .and_then(|id| profiles.iter().find(|p| p.id == id)),
        ) else {
            return;
        };
        let Some(rs) = q.run.active_result_set() else {
            return;
        };
        let changes = match rdbs_core::edit::build_changes(&table, rs, &q.run.edits) {
            Ok(changes) => changes,
            Err(e) => {
                q.run.edit_error = Some(e.to_string());
                return;
            }
        };
        if changes.is_empty() {
            return;
        }
        if profile.environment.requires_write_confirmation() && !confirmed {
            *confirm = Some(Confirmation::SaveEdits {
                tab_id,
                rows: q.run.edits.row_count(),
            });
            return;
        }
        let request_id = services.next_request_id();
        q.run.saving = Some(request_id);
        services.send(Command::ApplyChanges {
            request_id,
            profile: profile.clone(),
            database: q.database.clone(),
            changes,
            password: session_passwords.get(&profile.id).cloned(),
        });
    }

    /// Saves the result shown in the active query tab as CSV.
    fn export_active(&mut self) {
        use rdbs_core::export::{ExportFormat, export};
        let Self {
            workspace,
            profiles,
            ..
        } = self;
        let Some(q) = workspace.active_query_tab() else {
            return;
        };
        let kind = q
            .profile_id
            .and_then(|id| profiles.iter().find(|p| p.id == id))
            .map_or(rdbs_core::DatabaseKind::PostgreSql, |p| p.kind);
        let table = editor::export_table_name(&q.title);
        let Some(rs) = q.run.active_result_set() else {
            return;
        };
        let contents = export(rs, ExportFormat::Csv, kind, &table);
        self.save_export(contents, format!("{table}.csv"), ExportFormat::Csv);
    }

    /// Asks for a destination on a helper thread (the native dialog is modal and blocking)
    /// and writes the file there; the outcome comes back through `export_rx`.
    fn save_export(
        &mut self,
        contents: String,
        file_name: String,
        format: rdbs_core::export::ExportFormat,
    ) {
        let tx = self.export_tx.clone();
        let ctx = self.egui_ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("rdbs-export".into())
            .spawn(move || {
                let Some(path) = rfd::FileDialog::new()
                    .set_file_name(&file_name)
                    .add_filter(format.label(), &[format.extension()])
                    .save_file()
                else {
                    return;
                };
                let message = match std::fs::write(&path, contents.as_bytes()) {
                    Ok(()) => StatusMessage::info(format!(
                        "Exported {} to {}",
                        format.label(),
                        path.display()
                    )),
                    Err(e) => {
                        StatusMessage::error(format!("Could not write {}: {e}", path.display()))
                    }
                };
                let _ = tx.send(message);
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.status = Some(StatusMessage::error(format!("Could not start export: {e}")));
        }
    }

    fn open_prompt(&mut self, profile_id: ProfileId, then: AfterPassword, reason: Option<String>) {
        self.prompt = Some(PasswordPrompt {
            profile_id,
            password: String::new(),
            remember: false,
            then,
            reason,
            focus: true,
        });
    }

    /// Saves a profile and, when given, its password: into the keychain if the profile saves
    /// passwords, otherwise for this session only.
    fn save_profile(&mut self, mut profile: ConnectionProfile, password: Option<String>) {
        let previous = self.profile(profile.id).cloned();
        // The SSH secret goes to the keychain, never into the stored profile.
        let ssh_secret = profile.ssh.as_mut().and_then(|s| s.secret.take());
        let ssh_result = match (&profile.ssh, ssh_secret) {
            (Some(_), Some(secret)) => self.credentials.set(profile.id, SecretKind::Ssh, &secret),
            (Some(_), None) => Ok(()),
            (None, _) => self.credentials.delete(profile.id, SecretKind::Ssh),
        };
        if let Err(e) = ssh_result {
            tracing::warn!("could not store the SSH secret: {e}");
        }
        if let Err(e) = self.store.upsert(&profile) {
            self.status = Some(StatusMessage::error(format!(
                "Could not save connection: {e}"
            )));
            return;
        }
        let secret_result = if profile.save_password {
            self.session_passwords.remove(&profile.id);
            match &password {
                Some(pw) => self.credentials.set(profile.id, SecretKind::Database, pw),
                None => Ok(()),
            }
        } else {
            if let Some(pw) = password {
                self.session_passwords.insert(profile.id, pw);
            }
            self.credentials.delete(profile.id, SecretKind::Database)
        };
        self.status = Some(match secret_result {
            Ok(()) => StatusMessage::info(format!("Saved connection “{}”", profile.name)),
            Err(e) => StatusMessage::error(format!(
                "Saved “{}”, but the password could not be stored: {e}",
                profile.name
            )),
        });
        // Changed connection settings take effect on the next connect.
        let settings_changed = previous.as_ref().is_some_and(|old| {
            (
                &old.kind,
                &old.host,
                old.port,
                &old.database,
                &old.username,
                old.ssl_mode,
                &old.ssh,
            ) != (
                &profile.kind,
                &profile.host,
                profile.port,
                &profile.database,
                &profile.username,
                profile.ssl_mode,
                &profile.ssh,
            )
        });
        if settings_changed
            && self
                .connections
                .get(&profile.id)
                .is_some_and(ConnState::is_connected)
        {
            self.services.send(Command::Disconnect {
                profile_id: profile.id,
            });
        }
        self.reload_profiles();
        self.dialog = None;
    }

    fn delete_profile(&mut self, id: ProfileId) {
        if self.connections.remove(&id).is_some() {
            self.services.send(Command::Disconnect { profile_id: id });
        }
        self.session_passwords.remove(&id);
        for kind in [SecretKind::Database, SecretKind::Ssh] {
            if let Err(e) = self.credentials.delete(id, kind) {
                tracing::warn!("could not remove saved secret: {e}");
            }
        }
        match self.store.delete(id) {
            Ok(_) => {
                self.status = Some(StatusMessage::info("Connection deleted"));
                self.reload_profiles();
            }
            Err(e) => {
                self.status = Some(StatusMessage::error(format!(
                    "Could not delete connection: {e}"
                )));
            }
        }
    }

    // ----------------------------------------------------------------------------------
    // Chrome

    fn menu_bar(&mut self, ui: &mut Ui, actions: &mut Vec<UiAction>) {
        let ctx = ui.ctx().clone();
        let item = |label: String, sc: &KeyboardShortcut| {
            Button::new(label).shortcut_text(ctx.format_shortcut(sc))
        };
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui
                    .add(item(
                        format!("{}  New Connection…", icon::PLUS),
                        &SC_NEW_CONNECTION,
                    ))
                    .clicked()
                {
                    actions.push(UiAction::NewConnection);
                }
                if ui
                    .add(item(
                        format!("{}  New Query", icon::FILE_SQL),
                        &SC_NEW_QUERY,
                    ))
                    .clicked()
                {
                    actions.push(UiAction::NewQuery {
                        profile_id: None,
                        sql: None,
                        title: None,
                        run: false,
                    });
                }
                ui.separator();
                if ui
                    .add(item(format!("{}  Close Tab", icon::X), &SC_CLOSE_TAB))
                    .clicked()
                {
                    self.close_active_tab();
                }
                ui.separator();
                if ui.button(format!("{}  Exit", icon::SIGN_OUT)).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("View", |ui| {
                if ui
                    .add(item(
                        format!("{}  Toggle Sidebar", icon::SIDEBAR),
                        &SC_SIDEBAR,
                    ))
                    .clicked()
                {
                    actions.push(UiAction::ToggleSidebar);
                }
                if ui
                    .add(item(
                        format!("{}  Filter Explorer", icon::MAGNIFYING_GLASS),
                        &SC_FILTER,
                    ))
                    .clicked()
                {
                    self.workspace.sidebar_visible = true;
                    self.explorer.focus_filter = true;
                }
                if ui
                    .button(format!("{}  Welcome Page", icon::HOUSE))
                    .clicked()
                {
                    self.workspace.show_welcome();
                }
                ui.separator();
                let mut light = theme::is_light();
                if ui.checkbox(&mut light, "Light Theme").clicked() {
                    theme::set_light(ui.ctx(), light);
                }
                ui.separator();
                if ui
                    .button(format!("{}  Reset Layout", icon::ARROWS_CLOCKWISE))
                    .clicked()
                {
                    self.workspace = Workspace::default();
                }
            });
            ui.menu_button("Query", |ui| {
                if ui
                    .add(item(format!("{}  Run", icon::PLAY), &SC_RUN))
                    .clicked()
                {
                    self.run_active_tab(actions);
                }
            });
            ui.menu_button("Tools", |ui| {
                self.tools_menu(ui, actions);
            });
            ui.menu_button("Help", |ui| {
                if ui
                    .button(format!("{}  Keyboard Shortcuts", icon::KEYBOARD))
                    .clicked()
                {
                    self.workspace.show_welcome();
                }
                if ui
                    .button(format!(
                        "{}  About {}",
                        icon::INFO,
                        rdbs_core::brand::APP_NAME
                    ))
                    .clicked()
                {
                    self.show_about = true;
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let light = theme::is_light();
                let (glyph, tip) = if light {
                    (icon::MOON, "Switch to the dark theme")
                } else {
                    (icon::SUN, "Switch to the light theme")
                };
                if ui
                    .add(Button::new(glyph).frame(false))
                    .on_hover_text(tip)
                    .clicked()
                {
                    theme::set_light(ui.ctx(), !light);
                }
            });
        });
    }

    /// Entries of the Tools menu (scope = explorer selection).
    fn tools_menu(&self, ui: &mut Ui, actions: &mut Vec<UiAction>) {
        use crate::tools::ToolLaunch;
        let selected = self.selected_scope();
        let needs_selection = "Select a connection in the explorer first";
        if ui
            .button(format!("{}  Schema Diff…", icon::GIT_DIFF))
            .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Diff {
                source: selected.clone(),
            }));
        }
        if ui.button(format!("{}  Backups…", icon::ARCHIVE)).clicked() {
            actions.push(UiAction::OpenTool(ToolLaunch::Backup { scope: None }));
        }
        if ui
            .add_enabled(
                selected.is_some(),
                Button::new(format!("{}  Query Builder", icon::PUZZLE_PIECE)),
            )
            .on_disabled_hover_text(needs_selection)
            .clicked()
            && let Some(scope) = selected.clone()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Builder { scope }));
        }
        if ui
            .button(format!("{}  Data Transfer…", icon::FLOW_ARROW))
            .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Transfer {
                source: selected.clone(),
            }));
        }
        if ui
            .add_enabled(
                selected.is_some(),
                Button::new(format!("{}  ER Diagram", icon::GRAPH)),
            )
            .on_disabled_hover_text(needs_selection)
            .clicked()
            && let Some(scope) = selected.clone()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Er { scope }));
        }
        ui.separator();
        if ui
            .add_enabled(
                selected.is_some(),
                Button::new(format!("{}  New Table…", icon::PLUS_SQUARE)),
            )
            .on_disabled_hover_text(needs_selection)
            .clicked()
            && let Some(scope) = selected.clone()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Designer {
                scope,
                table: None,
            }));
        }
        if ui
            .add_enabled(
                selected.is_some(),
                Button::new(format!("{}  Import Data…", icon::DOWNLOAD_SIMPLE)),
            )
            .on_disabled_hover_text(needs_selection)
            .clicked()
            && let Some(scope) = selected.clone()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Import {
                scope,
                table: None,
            }));
        }
        ui.separator();
        if ui
            .button(format!("{}  Query History", icon::CLOCK))
            .clicked()
        {
            actions.push(UiAction::ShowHistory);
        }
    }

    /// Scope of the explorer selection: the selected object's database/schema, or the
    /// selected connection's default database.
    fn selected_scope(&self) -> Option<crate::tools::Scope> {
        let profile = self
            .profile(self.explorer.selected_profile?)
            .filter(|p| p.kind.is_sql())?;
        let (database, schema) = match &self.explorer.selected_object {
            Some(o) if o.profile_id == profile.id => (o.database.clone(), o.schema.clone()),
            _ => (
                self.connections
                    .get(&profile.id)
                    .and_then(|c| c.default_database.clone())
                    .or_else(|| (!profile.database.is_empty()).then(|| profile.database.clone())),
                crate::tools::picker::default_schema(profile.kind),
            ),
        };
        Some(crate::tools::scope(
            profile.kind,
            profile.id,
            database,
            schema,
        ))
    }

    fn toolbar(&mut self, ui: &mut Ui, actions: &mut Vec<UiAction>) {
        let can_export = self
            .workspace
            .active_query_tab()
            .is_some_and(|q| q.run.active_result_set().is_some());
        let selected_profile = self.explorer.selected_profile;
        let selected_connected = selected_profile
            .and_then(|id| self.connections.get(&id))
            .is_some_and(ConnState::is_connected);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if ui
                .add(ToolbarButton::new(icon::PLUGS_CONNECTED, "Connection"))
                .on_hover_text("New connection (Ctrl+N)")
                .clicked()
            {
                actions.push(UiAction::NewConnection);
            }
            if ui
                .add(ToolbarButton::new(icon::FILE_SQL, "New Query"))
                .on_hover_text("New query tab (Ctrl+T)")
                .clicked()
            {
                actions.push(UiAction::NewQuery {
                    profile_id: None,
                    sql: None,
                    title: None,
                    run: false,
                });
            }
            ui.separator();
            let selected_object = self.explorer.selected_object.clone();
            if ui
                .add(
                    ToolbarButton::new(icon::TABLE, "Open Table")
                        .enabled(selected_object.is_some()),
                )
                .on_hover_text("Open the selected table or view")
                .clicked()
                && let Some(obj) = selected_object
            {
                actions.push(UiAction::OpenObject(obj));
            }
            if ui
                .add(
                    ToolbarButton::new(icon::ARROWS_CLOCKWISE, "Refresh")
                        .enabled(selected_profile.is_some()),
                )
                .on_hover_text("Refresh selected connection (F5)")
                .clicked()
                && let Some(id) = selected_profile
            {
                actions.push(UiAction::Refresh(id));
            }
            if ui
                .add(ToolbarButton::new(icon::PLUGS, "Disconnect").enabled(selected_connected))
                .clicked()
                && let Some(id) = selected_profile
            {
                actions.push(UiAction::Disconnect(id));
            }
            ui.separator();
            if ui
                .add(ToolbarButton::new(icon::EXPORT, "Export").enabled(can_export))
                .on_hover_text("Save the visible result as CSV (more formats in the result header)")
                .clicked()
            {
                actions.push(UiAction::ExportActive);
            }
            let scope = self.selected_scope();
            if ui
                .add(ToolbarButton::new(icon::GRAPH, "Model").enabled(scope.is_some()))
                .on_hover_text("ER diagram of the selected database / schema")
                .clicked()
                && let Some(scope) = scope
            {
                actions.push(UiAction::OpenTool(crate::tools::ToolLaunch::Er { scope }));
            }
            if ui
                .add(ToolbarButton::new(icon::CLOCK, "History"))
                .on_hover_text("Query history")
                .clicked()
            {
                actions.push(UiAction::ShowHistory);
            }
            ui.separator();
            if ui
                .add(ToolbarButton::new(icon::SIDEBAR, "Sidebar"))
                .on_hover_text("Toggle sidebar (Ctrl+B)")
                .clicked()
            {
                actions.push(UiAction::ToggleSidebar);
            }
        });
    }

    fn status_bar(&self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            // The summary on the right gets its room first; the message takes what is left.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let connected = self
                    .connections
                    .values()
                    .filter(|c| c.is_connected())
                    .count();
                ui.label(
                    RichText::new(format!(
                        "v{}  ·  {}  ·  {connected}/{} connected",
                        env!("CARGO_PKG_VERSION"),
                        if self.demo {
                            "Demo mode (mock data)"
                        } else {
                            "Live"
                        },
                        self.profiles.len()
                    ))
                    .size(12.0)
                    .color(color::text_faint()),
                );
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let Some(msg) = &self.status else {
                        ui.label(RichText::new("Ready").size(12.0).color(color::text_dim()));
                        return;
                    };
                    let (glyph, c) = match msg.severity {
                        Severity::Info => (icon::INFO, color::text_dim()),
                        Severity::Error => (icon::WARNING, color::danger()),
                    };
                    // Server versions and errors can span several lines: one line here, all of
                    // it on hover.
                    let one_line = msg.text.split_whitespace().collect::<Vec<_>>().join(" ");
                    let label = ui.add(
                        egui::Label::new(
                            RichText::new(format!("{glyph}  {one_line}"))
                                .size(12.0)
                                .color(c),
                        )
                        .truncate(),
                    );
                    if one_line != msg.text || label.rect.width() >= ui.available_width() {
                        label.on_hover_text(msg.text.as_str());
                    }
                });
            });
        });
    }

    // ----------------------------------------------------------------------------------
    // Modals

    fn show_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        match dialog.show(ctx) {
            DialogAction::None => {}
            DialogAction::Cancel => self.dialog = None,
            DialogAction::Save(profile, password) => self.save_profile(profile, password),
            DialogAction::Test(profile, password) => {
                let request_id = self.services.next_request_id();
                self.services.send(Command::TestConnection {
                    request_id,
                    profile,
                    password,
                });
                dialog.test = TestState::Running(request_id);
            }
        }
    }

    fn show_password_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let Some(profile) = self
            .profiles
            .iter()
            .find(|p| p.id == prompt.profile_id)
            .cloned()
        else {
            self.prompt = None;
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        let response = egui::Modal::new(Id::new("password-prompt")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.horizontal(|ui| {
                widgets::kind_badge(ui, profile.kind, 22.0);
                ui.label(
                    RichText::new(format!("Connect to {}", profile.name))
                        .size(16.0)
                        .strong(),
                );
            });
            ui.label(
                RichText::new(format!("{}@{}", profile.username, profile.endpoint()))
                    .color(color::text_dim()),
            );
            if let Some(reason) = &prompt.reason {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("{}  {reason}", icon::WARNING)).color(color::danger()),
                );
            }
            ui.add_space(8.0);
            let field = ui.add(
                egui::TextEdit::singleline(&mut prompt.password)
                    .password(true)
                    .hint_text("Password")
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(&mut prompt.focus) {
                field.request_focus();
            }
            if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                submit = true;
            }
            ui.checkbox(&mut prompt.remember, "Save password in OS keychain");
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let connect = Button::new(RichText::new("Connect").color(Color32::WHITE).strong())
                    .fill(color::accent())
                    .min_size(Vec2::new(96.0, 26.0));
                if ui.add(connect).clicked() {
                    submit = true;
                }
                if ui
                    .add(Button::new("Cancel").min_size(Vec2::new(84.0, 26.0)))
                    .clicked()
                {
                    cancel = true;
                }
            });
        });
        if cancel || (!submit && response.should_close()) {
            self.prompt = None;
            return;
        }
        if !submit {
            return;
        }
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        if prompt.remember {
            let mut updated = profile.clone();
            updated.save_password = true;
            self.save_profile(updated, Some(prompt.password));
        } else {
            self.session_passwords.insert(profile.id, prompt.password);
        }
        match prompt.then {
            AfterPassword::Connect => self.connect(profile.id),
            AfterPassword::RunTab(tab_id) => {
                let follow_up = self.run_tab(tab_id);
                self.apply(follow_up);
            }
        }
    }

    fn show_exec_dialog(&mut self, ctx: &egui::Context) {
        let Some(exec) = &mut self.exec else {
            return;
        };
        let mut run = false;
        let mut cancel = false;
        let what = if exec.target.routine == rdbs_core::ObjectKind::Function {
            "function"
        } else {
            "procedure"
        };
        let response = egui::Modal::new(Id::new("exec-routine")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.label(
                RichText::new(format!(
                    "{}  Execute {what} {}",
                    icon::PLAY,
                    exec.target.name
                ))
                .size(16.0)
                .strong(),
            );
            ui.label(
                RichText::new("Text is quoted for you; numbers are passed as they are.")
                    .color(color::text_dim()),
            );
            ui.add_space(8.0);
            egui::Grid::new("exec-params")
                .num_columns(4)
                .spacing([10.0, 6.0])
                .striped(true)
                .show(ui, |ui| {
                    for h in ["Parameter", "Type", "Value", "NULL"] {
                        ui.label(RichText::new(h).color(color::text_dim()));
                    }
                    ui.end_row();
                    for (i, p) in exec.signature.params.iter().enumerate() {
                        let name = if p.name.is_empty() {
                            format!("${}", i + 1)
                        } else {
                            p.name.clone()
                        };
                        ui.label(RichText::new(name).strong());
                        ui.label(
                            RichText::new(format!("{} · {}", p.data_type, p.mode.label()))
                                .color(color::text_dim()),
                        );
                        if p.mode.takes_value() {
                            let null = exec.nulls[i];
                            ui.add_enabled_ui(!null, |ui| {
                                ui.add_sized(
                                    [240.0, ui.spacing().interact_size.y],
                                    egui::TextEdit::singleline(&mut exec.values[i])
                                        .hint_text(if null { "NULL" } else { "" }),
                                )
                            });
                            ui.checkbox(&mut exec.nulls[i], "");
                        } else {
                            ui.label(RichText::new("returned").color(color::text_faint()));
                            ui.label("");
                        }
                        ui.end_row();
                    }
                });
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let execute = Button::new(
                    RichText::new(format!("{}  Execute", icon::PLAY))
                        .color(Color32::WHITE)
                        .strong(),
                )
                .fill(color::accent())
                .min_size(Vec2::new(110.0, 26.0));
                if ui.add(execute).clicked() {
                    run = true;
                }
                if ui
                    .add(Button::new("Cancel").min_size(Vec2::new(84.0, 26.0)))
                    .clicked()
                {
                    cancel = true;
                }
            });
        });
        if cancel || (!run && response.should_close()) {
            self.exec = None;
            return;
        }
        if !run {
            return;
        }
        let Some(exec) = self.exec.take() else {
            return;
        };
        let args: Vec<rdbs_core::routine::ArgValue> = exec
            .values
            .into_iter()
            .zip(exec.nulls)
            .map(|(value, null)| {
                if null {
                    rdbs_core::routine::ArgValue::Null
                } else {
                    rdbs_core::routine::ArgValue::Text(value)
                }
            })
            .collect();
        let sql = rdbs_core::routine::call_statement(
            exec.target.kind,
            &exec.target.qualified,
            exec.target.routine,
            &exec.signature,
            &args,
        );
        self.run_routine(&exec.target, sql);
    }

    fn show_confirmation(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &self.confirm else { return };
        let mut decision: Option<bool> = None;
        let profiles = &self.profiles;
        let name_of = |id: ProfileId| {
            profiles
                .iter()
                .find(|p| p.id == id)
                .map_or_else(String::new, |p| p.name.clone())
        };

        let response = egui::Modal::new(Id::new("confirm-dialog")).show(ctx, |ui| {
            ui.set_width(480.0);
            let (title, body, confirm_label, preview) = match confirm {
                Confirmation::Execute {
                    profile_id,
                    sql,
                    kind,
                    ..
                } => (
                    format!("{}  Run on production?", icon::SHIELD_WARNING),
                    format!(
                        "This script contains a {} statement and will run on “{}” ({}).",
                        kind.label(),
                        name_of(*profile_id),
                        Environment::Production.label()
                    ),
                    "Run on production",
                    Some(sql.as_str()),
                ),
                Confirmation::SaveEdits { rows, .. } => (
                    format!("{}  Save changes on production?", icon::SHIELD_WARNING),
                    format!(
                        "{rows} row(s) will be changed in a single transaction on a {} connection.",
                        Environment::Production.label()
                    ),
                    "Save on production",
                    None,
                ),
                Confirmation::ClearHistory => (
                    format!("{}  Clear query history?", icon::TRASH),
                    "All recorded queries will be deleted. This cannot be undone.".to_owned(),
                    "Clear history",
                    None,
                ),
                Confirmation::DeleteProfile(id) => (
                    format!("{}  Delete connection?", icon::TRASH),
                    format!(
                        "“{}” will be removed from your saved connections. This cannot be undone.",
                        name_of(*id)
                    ),
                    "Delete",
                    None,
                ),
            };
            ui.label(
                RichText::new(title)
                    .size(16.0)
                    .strong()
                    .color(color::warning()),
            );
            ui.add_space(6.0);
            ui.label(body);
            if let Some(sql) = preview {
                ui.add_space(6.0);
                Frame::new()
                    .fill(color::editor())
                    .stroke(Stroke::new(1.0, color::border()))
                    .corner_radius(4)
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(160.0)
                            .show(ui, |ui| {
                                let shown: String = sql.chars().take(2_000).collect();
                                ui.label(widgets::highlight_sql(&shown, &theme::mono()));
                            });
                    });
            }
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let danger =
                    Button::new(RichText::new(confirm_label).color(Color32::WHITE).strong())
                        .fill(color::danger())
                        .min_size(Vec2::new(120.0, 26.0));
                if ui.add(danger).clicked() {
                    decision = Some(true);
                }
                if ui
                    .add(Button::new("Cancel").min_size(Vec2::new(84.0, 26.0)))
                    .clicked()
                {
                    decision = Some(false);
                }
            });
        });
        if decision.is_none() && response.should_close() {
            decision = Some(false);
        }

        let Some(accepted) = decision else { return };
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        if !accepted {
            return;
        }
        match confirm {
            Confirmation::Execute {
                tab_id,
                profile_id,
                sql,
                ..
            } => {
                let Some(profile) = self.profile(profile_id).cloned() else {
                    return;
                };
                let password = self.session_passwords.get(&profile_id).cloned();
                if let Some(q) = self.workspace.query_tab_mut(tab_id) {
                    editor::start_query(q, &profile, sql, &self.services, password);
                }
            }
            Confirmation::DeleteProfile(id) => self.delete_profile(id),
            Confirmation::SaveEdits { tab_id, .. } => self.save_edits(tab_id, true),
            Confirmation::ClearHistory => {
                if let Err(e) = self.store.clear_history() {
                    self.status = Some(StatusMessage::error(format!(
                        "Could not clear history: {e}"
                    )));
                }
                self.reload_history();
            }
        }
    }

    fn about_modal(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        let response = egui::Modal::new(Id::new("about-dialog")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(icon::DATABASE)
                        .size(36.0)
                        .color(color::accent()),
                );
                ui.label(
                    RichText::new(rdbs_core::brand::APP_NAME)
                        .size(22.0)
                        .strong(),
                );
                ui.label(RichText::new(rdbs_core::brand::APP_TAGLINE).color(color::text()));
                ui.label(
                    RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                        .color(color::text_dim()),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Author & creator")
                        .small()
                        .color(color::text_faint()),
                );
                ui.label(RichText::new(rdbs_core::brand::APP_AUTHOR).strong());
                ui.add_space(6.0);
                ui.label("Native, clean-room database IDE written in Rust with egui.");
                ui.label(
                    RichText::new(format!(
                        "© 2026 {}. All rights reserved.",
                        rdbs_core::brand::APP_AUTHOR
                    ))
                    .small()
                    .color(color::text_faint()),
                );
                ui.add_space(8.0);
                ui.button("Close").clicked()
            })
            .inner
        });
        if response.inner || response.should_close() {
            self.show_about = false;
        }
    }
}

fn dock_style(style: &mut egui_dock::Style) {
    style.main_surface_border_stroke = Stroke::NONE;
    style.tab_bar.bg_fill = color::panel();
    style.tab_bar.height = 30.0;
    style.tab_bar.hline_color = color::border();

    style.tab.active.bg_fill = color::editor();
    style.tab.active.text_color = color::text();
    style.tab.active.outline_color = color::border();
    style.tab.focused.bg_fill = color::editor();
    style.tab.focused.text_color = color::text_strong();
    style.tab.focused.outline_color = color::accent();
    style.tab.inactive.bg_fill = color::panel();
    style.tab.inactive.text_color = color::text_dim();
    style.tab.inactive.outline_color = Color32::TRANSPARENT;
    style.tab.hovered.bg_fill = color::surface();
    style.tab.hovered.text_color = color::text();
    style.tab.tab_body.bg_fill = color::editor();
    style.tab.tab_body.inner_margin = Margin::ZERO;
    style.tab.tab_body.stroke = Stroke::new(1.0, color::border());
    style.tab.hline_below_active_tab_name = true;

    style.separator.color_idle = color::border();
    style.separator.color_hovered = color::accent();
    style.separator.color_dragged = color::accent();
    style.overlay.selection_color = color::accent().gamma_multiply(0.35);
}

impl eframe::App for StudioApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.window_fit.update(&ctx);
        self.drain_events();
        self.tick_schedules(&ctx);
        while let Ok(message) = self.export_rx.try_recv() {
            self.status = Some(message);
        }

        let mut actions = Vec::new();
        self.shortcuts(&ctx, &mut actions);

        let bar = |m: Margin| Frame::new().fill(color::panel()).inner_margin(m);
        egui::Panel::top(Id::new("menu-bar"))
            .frame(bar(Margin::symmetric(6, 3)))
            .show(ui, |ui| self.menu_bar(ui, &mut actions));
        egui::Panel::top(Id::new("toolbar"))
            .frame(bar(Margin::symmetric(8, 3)))
            .show(ui, |ui| self.toolbar(ui, &mut actions));
        egui::Panel::bottom(Id::new("status-bar"))
            .exact_size(24.0)
            .frame(bar(Margin::symmetric(10, 0)))
            .show(ui, |ui| self.status_bar(ui));

        if self.workspace.sidebar_visible {
            egui::Panel::left(Id::new("explorer"))
                .resizable(true)
                .default_size(280.0)
                .size_range(180.0..=560.0)
                .frame(
                    Frame::new()
                        .fill(color::sidebar())
                        .inner_margin(Margin::same(8)),
                )
                .show(ui, |ui| {
                    let mut cx = ExplorerContext {
                        profiles: &self.profiles,
                        connections: &self.connections,
                        actions: &mut actions,
                    };
                    explorer::show(ui, &mut self.explorer, &mut cx);
                });
        }

        CentralPanel::no_frame().show(ui, |ui| {
            let mut style = egui_dock::Style::from_egui(ui.style());
            dock_style(&mut style);
            let mut cx = TabContext {
                profiles: &self.profiles,
                connections: &self.connections,
                services: &self.services,
                passwords: &self.session_passwords,
                history: &self.history,
                backups: &self.backups,
                actions: &mut actions,
            };
            DockArea::new(&mut self.workspace.dock)
                .id(Id::new("workspace-dock"))
                .style(style)
                .show_add_buttons(true)
                .show_leaf_collapse_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_inside(ui, &mut cx);
        });

        self.show_dialog(&ctx);
        self.show_password_prompt(&ctx);
        self.show_exec_dialog(&ctx);
        self.show_confirmation(&ctx);
        self.about_modal(&ctx);
        self.apply(actions);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let key = if self.demo {
            DEMO_WORKSPACE_KEY
        } else {
            WORKSPACE_KEY
        };
        eframe::set_value(storage, key, &self.workspace);
        eframe::set_value(storage, LIGHT_THEME_KEY, &theme::is_light());
    }
}
