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
use crate::model::{
    AfterPassword, ConnState, ConnStatus, Severity, StatusMessage, TabClose, UiAction,
};
use crate::theme::{self, color};
use crate::widgets::{self, ToolbarButton};
use crate::workspace::{LastRun, RunPhase, Tab, TabContext, Workspace};

mod actions;
mod ai;
mod chrome;
mod events;
mod keyboard;
mod modals;

/// Status text shown while an object tree reloads.
const REFRESHING: &str = "Refreshing objects…";

/// Storage key of the persisted [`Workspace`] (separate keys keep demo tabs out of live mode).
const WORKSPACE_KEY: &str = "rdbs.workspace";
const DEMO_WORKSPACE_KEY: &str = "rdbs.workspace.demo";
/// Persisted theme choice of 0.3.2 and earlier (`true` = light), still written for them.
const LIGHT_THEME_KEY: &str = "rdbs.light_theme";
/// Persisted [`theme::ThemeMode`].
const THEME_MODE_KEY: &str = "rdbs.theme_mode";

const SC_NEW_CONNECTION: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const SC_NEW_QUERY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::T);
const SC_CLOSE_TAB: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::W);
const SC_CLOSE_ALL_TABS: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::W);
const SC_OPEN_QUERY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SC_SAVE_QUERY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SC_SAVE_QUERY_AS: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
const SC_SIDEBAR: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::B);
const SC_FILTER: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);
const SC_REPLACE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::H);
const SC_FIND_NEXT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F3);
const SC_FIND_PREVIOUS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::SHIFT, Key::F3);
const SC_RUN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);
const SC_RUN_ALT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::R);
/// Runs the active query tab; with no query tab active it refreshes the selected connection.
const SC_F5: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F5);
/// Shows or hides the AI assistant panel.
const SC_AI_PANEL: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::I);

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
    /// Use installed fonts (listed on a background thread). Tests turn it off so layouts do
    /// not depend on the machine.
    pub system_fonts: bool,
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
    AgentJob {
        profile_id: ProfileId,
        job_name: String,
        action: rdbs_core::agent::AgentJobAction,
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
    /// Opens the window at a usable size and remembers it.
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
    /// Script file dialogs, the last folder and tabs waiting for a save decision.
    scripts: crate::scripts::Scripts,
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
    /// Reconnect in flight, including one that is waiting for a password or a retry.
    reconnect: Option<ReconnectIntent>,
    /// Update check: menu, start-up toast and status-bar notice.
    update: crate::update::UpdateState,
    /// AI assistant panel, its providers and the conversation.
    ai: crate::ai::AiState,
    /// Interface, editor and grid fonts, and Tools > Fonts.
    fonts: crate::fonts::FontState,
}

/// A reconnect the user asked for. Kept until the worker reports success or failure so a
/// "retry" can run the same tab once the new session is open.
#[derive(Clone)]
struct ReconnectIntent {
    profile_id: ProfileId,
    /// Database override as the tab stores it (not yet reduced to a session key).
    database: Option<String>,
    retry_tab: Option<u64>,
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
        let mode = cc
            .storage
            .and_then(|s| {
                eframe::get_value::<theme::ThemeMode>(s, THEME_MODE_KEY).or_else(|| {
                    eframe::get_value::<bool>(s, LIGHT_THEME_KEY).map(|light| {
                        if light {
                            theme::ThemeMode::Light
                        } else {
                            theme::ThemeMode::Dark
                        }
                    })
                })
            })
            .unwrap_or_default();
        let fonts = crate::fonts::FontState::new(
            cc.storage
                .and_then(|s| eframe::get_value(s, crate::fonts::FONTS_KEY))
                .unwrap_or_default(),
            config.system_fonts,
        );
        theme::apply(&cc.egui_ctx, mode, &fonts.settings);

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
        workspace.dock_floating_tabs();
        // Every start opens on the home page; the other restored tabs stay behind it.
        workspace.show_welcome();
        let script_dir: Option<std::path::PathBuf> = cc
            .storage
            .and_then(|s| eframe::get_value(s, crate::scripts::SCRIPT_DIR_KEY));
        let update = crate::update::UpdateState::new(
            cc.storage
                .and_then(|s| eframe::get_value(s, crate::update::CHECK_ON_START_KEY))
                .unwrap_or(true),
            cc.storage
                .and_then(|s| {
                    eframe::get_value::<Option<String>>(s, crate::update::SKIPPED_VERSION_KEY)
                })
                .flatten(),
        );

        let ai = crate::ai::AiState::new(
            cc.storage
                .and_then(|s| eframe::get_value(s, crate::ai::PROVIDERS_KEY))
                .unwrap_or_default(),
            cc.storage
                .and_then(|s| eframe::get_value::<Option<uuid::Uuid>>(s, crate::ai::ACTIVE_KEY))
                .flatten(),
            cc.storage
                .and_then(|s| eframe::get_value(s, crate::ai::VISIBLE_KEY))
                .unwrap_or(false),
            cc.storage
                .and_then(|s| eframe::get_value(s, crate::ai::SEND_SCHEMA_KEY))
                .unwrap_or(true),
        );

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
            scripts: crate::scripts::Scripts::new(script_dir),
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
            reconnect: None,
            update,
            ai,
            fonts,
        })
        .map(|mut app| {
            app.reload_history();
            app.refresh_ai_keys();
            if app.update.check_on_start {
                app.check_for_updates(false);
            }
            app
        })
    }

    /// Asks the worker for the latest release; the answer is [`Event::UpdateChecked`].
    fn check_for_updates(&mut self, manual: bool) {
        if self.update.is_checking() {
            if manual {
                self.update.started(true);
            }
            return;
        }
        self.update.started(manual);
        self.services.send(Command::CheckForUpdates { manual });
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
}

/// The workspace with every tab closed: a page in the editor colour with New Query and the
/// home page. Returns whether the home page was asked for.
fn empty_workspace(ui: &mut Ui, actions: &mut Vec<UiAction>) -> bool {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0, color::editor());
    let mut welcome = false;
    ui.vertical_centered(|ui| {
        ui.add_space((rect.height() * 0.32).max(12.0));
        ui.label(
            RichText::new(icon::FOLDER_OPEN)
                .size(34.0)
                .color(color::text_faint()),
        );
        ui.label(RichText::new("No open tabs").color(color::text_dim()));
        ui.add_space(8.0);
        let buttons = ui
            .horizontal(|ui| {
                let new_query = ui
                    .button(format!("{}  New Query", icon::FILE_SQL))
                    .on_hover_text("Ctrl+T")
                    .clicked();
                let home = ui.button(format!("{}  Welcome", icon::HOUSE)).clicked();
                (new_query, home)
            })
            .inner;
        if buttons.0 {
            actions.push(UiAction::NewQuery {
                profile_id: None,
                database: None,
                sql: None,
                title: None,
                run: false,
            });
        }
        welcome = buttons.1;
    });
    welcome
}

/// A query tab whose script has unsaved changes.
fn tab_is_dirty(tab: &Tab) -> bool {
    matches!(tab, Tab::Query(q) if q.is_dirty())
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
    // egui_dock fills the hovered close button edge to edge, over the tab's right border;
    // the cross turns red instead.
    style.buttons.close_tab_bg_fill = Color32::TRANSPARENT;
    style.buttons.close_tab_color = color::text_dim();
    style.buttons.close_tab_active_color = color::danger();
    style.tab_bar.show_scroll_bar_on_overflow = true;

    style.separator.color_idle = color::border();
    style.separator.color_hovered = color::accent();
    style.separator.color_dragged = color::accent();
    style.overlay.selection_color = color::accent().gamma_multiply(0.35);
}

impl eframe::App for StudioApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        theme::follow_system(&ctx);
        self.fonts.poll(&ctx);
        self.window_fit.update(&ctx);
        self.drain_events();
        self.tick_schedules(&ctx);
        while let Ok(message) = self.export_rx.try_recv() {
            self.status = Some(message);
        }
        while let Some(picked) = self.scripts.poll() {
            self.apply_picked(picked);
        }
        self.guard_quit(&ctx);

        let mut actions = Vec::new();
        self.shortcuts(&ctx, &mut actions);

        let bar = |m: Margin| Frame::new().fill(color::panel()).inner_margin(m);
        let mut edit_action = None;
        egui::Panel::top(Id::new("menu-bar"))
            .frame(bar(Margin::symmetric(6, 3)))
            .show(ui, |ui| {
                edit_action = self.menu_bar(ui, &mut actions);
            });
        if let Some(action) = edit_action {
            self.apply_editor_menu(action, &mut actions);
        }
        egui::Panel::top(Id::new("toolbar"))
            .frame(bar(Margin::symmetric(8, 3)))
            .show(ui, |ui| self.toolbar(ui, &mut actions));
        egui::Panel::bottom(Id::new("status-bar"))
            .exact_size(24.0)
            .frame(bar(Margin::symmetric(10, 0)))
            .show(ui, |ui| self.status_bar(ui));

        self.ai_panel(ui, &mut actions);
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

        let mut open_welcome = false;
        CentralPanel::no_frame().show(ui, |ui| {
            // With every tab closed the dock paints nothing, which leaves the bare (black)
            // window background; show an empty page with a way back instead.
            if self.workspace.dock.iter_all_tabs().next().is_none() {
                open_welcome = empty_workspace(ui, &mut actions);
                return;
            }
            let mut style = egui_dock::Style::from_egui(ui.style());
            dock_style(&mut style);
            let button_font = egui::TextStyle::Button.resolve(ui.style());
            let tab_plan =
                self.workspace
                    .layout_tabs(ui.ctx(), button_font.clone(), ui.available_width());
            if let Some(slot) = tab_plan.slot {
                style.tab.minimum_width = Some(slot);
            }
            let mut cx = TabContext {
                profiles: &self.profiles,
                connections: &self.connections,
                services: &self.services,
                passwords: &self.session_passwords,
                history: &self.history,
                backups: &self.backups,
                actions: &mut actions,
                tab_text_px: tab_plan.text_px,
                egui: Some(ui.ctx().clone()),
                button_font: Some(button_font),
            };
            DockArea::new(&mut self.workspace.dock)
                .id(Id::new("workspace-dock"))
                .style(style)
                .show_add_buttons(true)
                .show_leaf_collapse_buttons(false)
                .show_leaf_close_all_buttons(false)
                // Tabs bring their own right-click menu (`TabViewer::on_tab_button`).
                .tab_context_menus(false)
                .show_inside(ui, &mut cx);
        });
        if open_welcome {
            self.workspace.show_welcome();
        }

        self.show_dialog(&ctx);
        self.show_password_prompt(&ctx);
        self.show_exec_dialog(&ctx);
        self.show_confirmation(&ctx);
        self.show_close_prompt(&ctx);
        self.show_quit_prompt(&ctx);
        self.about_modal(&ctx);
        if let Some(crate::update::UpdateAction::CheckNow) = self.update.dialog(&ctx) {
            self.check_for_updates(true);
        }
        self.update.toast(&ctx);
        self.ai_dialog(&ctx, &mut actions);
        self.fonts.dialog(&ctx);
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
        eframe::set_value(storage, THEME_MODE_KEY, &theme::mode());
        eframe::set_value(
            storage,
            crate::update::CHECK_ON_START_KEY,
            &self.update.check_on_start,
        );
        eframe::set_value(
            storage,
            crate::update::SKIPPED_VERSION_KEY,
            &self.update.skipped,
        );
        eframe::set_value(storage, crate::fonts::FONTS_KEY, &self.fonts.settings);
        eframe::set_value(storage, crate::ai::PROVIDERS_KEY, &self.ai.providers);
        eframe::set_value(storage, crate::ai::ACTIVE_KEY, &self.ai.active);
        eframe::set_value(storage, crate::ai::VISIBLE_KEY, &self.ai.visible);
        eframe::set_value(storage, crate::ai::SEND_SCHEMA_KEY, &self.ai.send_schema);
        if let Some(dir) = &self.scripts.last_dir {
            eframe::set_value(storage, crate::scripts::SCRIPT_DIR_KEY, dir);
        }
        if let Some(state) = self.window_fit.state() {
            eframe::set_value(storage, crate::window::EFRAME_WINDOW_KEY, &state);
        }
    }
}
