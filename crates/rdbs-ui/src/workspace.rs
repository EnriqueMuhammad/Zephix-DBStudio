//! Dockable tab workspace: tab model, persisted layout and the `egui_dock` tab viewer.

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use egui::{Id, Ui, WidgetText};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockState, TabViewer};
use egui_phosphor::regular as icon;
use rdbs_core::{Command, ConnectionProfile, DbError, ProfileId, QueryOutcome, RequestId};
use rdbs_services::ServiceHandle;
use serde::{Deserialize, Serialize};

use crate::grid::GridState;
use crate::model::{ConnState, TabClose, UiAction};
use crate::{editor, welcome};

/// Initial SQL in a fresh workspace.
const SAMPLE_SQL: &str = "-- Choose a connection in the toolbar above, then press Ctrl+Enter.\n\
-- Select part of the script first to run only that part.\n\
SELECT 1 AS hello;\n";

/// Everything about the workspace that survives a restart.
#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    /// Dock layout and open tabs.
    pub dock: DockState<Tab>,
    /// Next id handed to a new query tab.
    pub next_tab_id: u64,
    /// Whether the explorer sidebar is shown.
    pub sidebar_visible: bool,
}

impl Default for Workspace {
    fn default() -> Self {
        let first = QueryTab::new(1, None, SAMPLE_SQL.to_owned(), None);
        Self {
            dock: DockState::new(vec![Tab::Welcome, Tab::Query(Box::new(first))]),
            next_tab_id: 2,
            sidebar_visible: true,
        }
    }
}

impl Workspace {
    /// Opens a query tab in the focused leaf and returns its id.
    pub fn open_query_tab(
        &mut self,
        profile_id: Option<ProfileId>,
        sql: String,
        title: Option<String>,
    ) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.dock
            .push_to_focused_leaf(Tab::Query(Box::new(QueryTab::new(
                id, profile_id, sql, title,
            ))));
        id
    }

    /// Finds a query tab by id.
    pub fn query_tab_mut(&mut self, id: u64) -> Option<&mut QueryTab> {
        self.dock
            .iter_all_tabs_mut()
            .find_map(|(_, tab)| match tab {
                Tab::Query(q) if q.id == id => Some(q.as_mut()),
                _ => None,
            })
    }

    /// Finds the query tab waiting for `request_id`.
    pub fn tab_for_request(&mut self, request_id: RequestId) -> Option<&mut QueryTab> {
        self.dock
            .iter_all_tabs_mut()
            .find_map(|(_, tab)| match tab {
                Tab::Query(q)
                    if q.run.running.as_ref().map(|r| r.request_id) == Some(request_id) =>
                {
                    Some(q.as_mut())
                }
                _ => None,
            })
    }

    /// Finds the query tab waiting for the plan `request_id`.
    pub fn tab_for_plan(&mut self, request_id: RequestId) -> Option<&mut QueryTab> {
        self.dock
            .iter_all_tabs_mut()
            .find_map(|(_, tab)| match tab {
                Tab::Query(q) if q.run.plan_request.as_ref().map(|p| p.0) == Some(request_id) => {
                    Some(q.as_mut())
                }
                _ => None,
            })
    }

    /// Finds the query tab whose edits are being saved by `request_id`.
    pub fn tab_for_save(&mut self, request_id: RequestId) -> Option<&mut QueryTab> {
        self.dock
            .iter_all_tabs_mut()
            .find_map(|(_, tab)| match tab {
                Tab::Query(q) if q.run.saving == Some(request_id) => Some(q.as_mut()),
                _ => None,
            })
    }

    /// The active tab of the focused leaf, if it is a query tab.
    pub fn active_query_tab(&mut self) -> Option<&mut QueryTab> {
        match self.dock.find_active_focused() {
            Some((_, Tab::Query(q))) => Some(q.as_mut()),
            _ => None,
        }
    }

    /// Closes the active tab of the focused leaf and returns it.
    pub fn close_active_tab(&mut self) -> Option<Tab> {
        let key = self.dock.find_active_focused().map(|(_, t)| t.key())?;
        let path = self.dock.find_tab_from(|t| t.key() == key)?;
        self.dock.remove_tab(path)
    }

    /// Closes the tabs `which` selects and returns them. After "close others" the kept tab is
    /// active and focused.
    pub fn close_tabs(&mut self, which: TabClose) -> Vec<Tab> {
        let mut closed = Vec::new();
        while let Some(path) = self.dock.find_tab_from(|t| which.closes(t.key())) {
            match self.dock.remove_tab(path) {
                Some(tab) => closed.push(tab),
                None => break,
            }
        }
        if let TabClose::Others(key) = which
            && let Some(path) = self.dock.find_tab_from(|t| t.key() == key)
        {
            let _ = self.dock.set_active_tab(path);
            self.dock.set_focused_node_and_surface(path.node_path());
        }
        closed
    }

    /// Focuses the history tab, opening it if needed.
    pub fn show_history(&mut self) {
        match self
            .dock
            .find_tab_from(|t| matches!(t, Tab::History { .. }))
        {
            Some(path) => {
                let _ = self.dock.set_active_tab(path);
            }
            None => self.dock.push_to_focused_leaf(Tab::History {
                filter: String::new(),
            }),
        }
    }

    /// Opens a tool tab, or focuses an existing one that edits the same object.
    pub fn open_tool(&mut self, launch: crate::tools::ToolLaunch) {
        if let Some(path) = self
            .dock
            .find_tab_from(|t| matches!(t, Tab::Tool(tool) if tool.matches(&launch)))
        {
            let _ = self.dock.set_active_tab(path);
            return;
        }
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.dock
            .push_to_focused_leaf(Tab::Tool(Box::new(crate::tools::ToolTab::new(id, launch))));
    }

    /// Offers a worker event to every tool tab until one consumes it.
    pub fn dispatch_tool_event(&mut self, event: &rdbs_core::Event) -> bool {
        self.dock.iter_all_tabs_mut().any(|(_, tab)| match tab {
            Tab::Tool(t) => t.on_event(event),
            _ => false,
        })
    }

    /// Focuses the welcome tab, re-opening it if it was closed.
    pub fn show_welcome(&mut self) {
        if self
            .dock
            .find_tab_from(|t| matches!(t, Tab::Welcome))
            .is_none()
        {
            self.dock.push_to_focused_leaf(Tab::Welcome);
        }
        if let Some(path) = self.dock.find_tab_from(|t| matches!(t, Tab::Welcome)) {
            let _ = self.dock.set_active_tab(path);
            // Also focus its panel: a restored layout may have no focused panel at all.
            self.dock.set_focused_node_and_surface(path.node_path());
        }
    }
}

/// A workspace tab.
#[derive(Serialize, Deserialize)]
pub enum Tab {
    /// Start page with quick actions.
    Welcome,
    /// SQL editor with results.
    Query(Box<QueryTab>),
    /// Executed-query history.
    History {
        /// Search text.
        filter: String,
    },
    /// Designer, diff, ER diagram and other tools.
    Tool(Box<crate::tools::ToolTab>),
}

impl Tab {
    /// Stable identity used to locate the tab in the dock tree.
    pub fn key(&self) -> u64 {
        match self {
            Tab::Welcome => 0,
            Tab::History { .. } => u64::MAX,
            Tab::Query(q) => q.id,
            Tab::Tool(t) => t.id,
        }
    }
}

/// A SQL editor tab. Only the SQL text and target connection are persisted.
#[derive(Serialize, Deserialize)]
pub struct QueryTab {
    /// Unique id within the workspace.
    pub id: u64,
    /// Tab title.
    pub title: String,
    /// Target connection.
    pub profile_id: Option<ProfileId>,
    /// Database to run in instead of the profile's default (PostgreSQL / SQL Server).
    #[serde(default)]
    pub database: Option<String>,
    /// Table the tab was opened from; enables editing when it has a primary key.
    #[serde(default)]
    pub table: Option<rdbs_core::edit::TableTarget>,
    /// Set for a data tab: the rows of one table or view, without an SQL editor.
    #[serde(default)]
    pub data_view: Option<DataView>,
    /// Editor contents.
    pub sql: String,
    /// Execution state (not persisted).
    #[serde(skip)]
    pub run: RunState,
}

impl QueryTab {
    /// Creates a tab; the editor grabs keyboard focus on first display.
    pub fn new(id: u64, profile_id: Option<ProfileId>, sql: String, title: Option<String>) -> Self {
        Self {
            id,
            title: title.unwrap_or_else(|| format!("Query {id}")),
            profile_id,
            database: None,
            table: None,
            data_view: None,
            sql,
            run: RunState {
                focus_editor: true,
                ..RunState::default()
            },
        }
    }

    /// Egui id of this tab's text editor.
    pub fn editor_id(&self) -> Id {
        Id::new(("sql-editor", self.id))
    }
}

/// What a data tab shows: one object, filtered, sorted and limited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataView {
    /// Engine of the connection (decides the SQL dialect).
    pub kind: rdbs_core::DatabaseKind,
    /// Quoted, qualified object name.
    pub qualified: String,
    /// `WHERE` condition as typed (without the keyword).
    pub filter: String,
    /// `ORDER BY` list as typed (without the keywords).
    pub order: String,
    /// Row limit (`None` = all rows).
    pub limit: Option<u32>,
}

impl DataView {
    /// Row limits offered in the toolbar.
    pub const LIMITS: [Option<u32>; 4] = [Some(100), Some(1000), Some(10_000), None];

    /// The `SELECT` for the current filter, sort order and limit.
    pub fn sql(&self) -> String {
        rdbs_core::dialect::select_rows(
            self.kind,
            &self.qualified,
            &self.filter,
            &self.order,
            self.limit,
        )
    }
}

/// Transient execution state of a query tab.
#[derive(Default)]
pub struct RunState {
    /// Query in flight.
    pub running: Option<Running>,
    /// Most recent completed execution.
    pub last: Option<LastRun>,
    /// Grid selection.
    pub grid: GridState,
    /// Selection of each grid when result sets are stacked.
    pub stacked_grids: Vec<GridState>,
    /// Selected text in the editor as a byte range (empty selections are `None`).
    pub selection: Option<Range<usize>>,
    /// Request keyboard focus for the editor next frame.
    pub focus_editor: bool,
    /// Show server messages instead of the grid.
    pub show_messages: bool,
    /// Index of the result set shown in the grid.
    pub active_result: usize,
    /// Autocomplete popup state.
    pub autocomplete: crate::autocomplete::AutocompleteState,
    /// Grid edits not yet saved.
    pub edits: rdbs_core::edit::PendingEdits,
    /// Cell being edited inline.
    pub editing: Option<crate::grid::EditingCell>,
    /// Save request in flight.
    pub saving: Option<RequestId>,
    /// Why the last save failed.
    pub edit_error: Option<String>,
    /// Caret position in the editor (byte offset).
    pub cursor: Option<usize>,
    /// Whether the results area shows the result grid or the execution plan.
    pub view: ResultView,
    /// Plan request in flight, with its start time and whether it is analyzed.
    pub plan_request: Option<(RequestId, Instant, bool)>,
    /// Last plan (or why it could not be produced).
    pub plan: Option<Result<rdbs_core::plan::ExplainPlan, DbError>>,
    /// Plan viewer state.
    pub plan_view: crate::plan::PlanViewState,
}

/// What the results area of a query tab shows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ResultView {
    /// Result sets and messages.
    #[default]
    Results,
    /// Execution plan.
    Plan,
}

impl RunState {
    /// The result set currently shown, if the last run produced any.
    pub fn active_result_set(&self) -> Option<&rdbs_core::ResultSet> {
        let outcome = self.last.as_ref()?.outcome.as_ref().ok()?;
        outcome
            .results
            .get(self.active_result)
            .or_else(|| outcome.results.last())
    }
}

/// A query in flight.
pub struct Running {
    /// Correlation id.
    pub request_id: RequestId,
    /// When it was dispatched.
    pub started: Instant,
    /// Connection name, for the status line.
    pub profile_name: String,
    /// Connection the query runs on (for history).
    pub profile_id: ProfileId,
    /// Executed SQL (for history).
    pub sql: String,
    /// Database override the query ran in (for history).
    pub database: Option<String>,
}

/// A finished execution.
pub struct LastRun {
    /// Wall-clock duration.
    pub elapsed: Duration,
    /// Result or error.
    pub outcome: Result<QueryOutcome, DbError>,
    /// Connection name, for the status line.
    pub profile_name: String,
}

/// Borrowed application state handed to the tab viewer for one frame.
pub struct TabContext<'a> {
    /// All profiles.
    pub profiles: &'a [ConnectionProfile],
    /// Runtime connection state.
    pub connections: &'a HashMap<ProfileId, ConnState>,
    /// Worker handle.
    pub services: &'a ServiceHandle,
    /// Passwords entered for this session.
    pub passwords: &'a HashMap<ProfileId, String>,
    /// History entries for the history tab (newest first).
    pub history: &'a [rdbs_storage::HistoryEntry],
    /// Backup schedules and running backups.
    pub backups: &'a crate::model::BackupState,
    /// Actions requested during this frame.
    pub actions: &'a mut Vec<UiAction>,
}

impl TabContext<'_> {
    /// Looks up a profile by id.
    pub fn profile(&self, id: ProfileId) -> Option<&ConnectionProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }
}

impl TabViewer for TabContext<'_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> Id {
        Id::new(("tab", tab.key()))
    }

    fn title(&mut self, tab: &mut Tab) -> WidgetText {
        match tab {
            Tab::Welcome => format!("{}  Welcome", icon::HOUSE).into(),
            Tab::History { .. } => format!("{}  History", icon::CLOCK).into(),
            Tab::Tool(t) => t.title().into(),
            Tab::Query(q) => {
                let glyph = if q.run.running.is_some() {
                    icon::CIRCLE_NOTCH
                } else if q.data_view.is_some() {
                    icon::TABLE
                } else {
                    icon::FILE_SQL
                };
                format!("{glyph}  {}", q.title).into()
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        match tab {
            Tab::Welcome => welcome::show(ui, self),
            Tab::History { filter } => crate::history::show(ui, filter, self),
            Tab::Query(q) => editor::show(ui, q, self),
            Tab::Tool(t) => t.show(ui, self),
        }
    }

    fn on_close(&mut self, tab: &mut Tab) -> OnCloseResponse {
        if let Tab::Query(q) = tab
            && let Some(running) = &q.run.running
        {
            self.services.send(Command::Cancel {
                request_id: running.request_id,
            });
        }
        OnCloseResponse::Close
    }

    fn on_tab_button(&mut self, tab: &mut Tab, response: &egui::Response) {
        // Replaces egui_dock's own tab menu (see `DockArea::tab_context_menus`).
        let key = tab.key();
        response.context_menu(|ui| {
            for (label, which) in [
                ("Close Tab", TabClose::One(key)),
                ("Close Other Tabs", TabClose::Others(key)),
                ("Close All Tabs", TabClose::All),
            ] {
                if ui.button(label).clicked() {
                    self.actions.push(UiAction::CloseTabs(which));
                    ui.close();
                }
            }
        });
    }

    fn on_add(&mut self, _path: egui_dock::NodePath) {
        self.actions.push(UiAction::NewQuery {
            profile_id: None,
            sql: None,
            title: None,
            run: false,
        });
    }

    fn scroll_bars(&self, _tab: &Tab) -> [bool; 2] {
        // Tabs manage their own scroll areas (editor, grid).
        [false, false]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_is_welcome(w: &mut Workspace) -> bool {
        matches!(w.dock.find_active_focused(), Some((_, Tab::Welcome)))
    }

    fn keys(w: &Workspace) -> Vec<u64> {
        w.dock.iter_all_tabs().map(|(_, t)| t.key()).collect()
    }

    #[test]
    fn closes_one_other_or_all_tabs() {
        let mut w = Workspace::default(); // Welcome (0) and Query 1
        let a = w.open_query_tab(None, String::new(), None);
        let b = w.open_query_tab(None, String::new(), None);
        w.show_history();

        let closed = w.close_tabs(TabClose::One(a));
        assert_eq!(closed.iter().map(Tab::key).collect::<Vec<_>>(), [a]);
        assert_eq!(keys(&w), [0, 1, b, u64::MAX]);

        let closed = w.close_tabs(TabClose::Others(b));
        assert_eq!(closed.len(), 3);
        assert_eq!(keys(&w), [b]);
        assert!(matches!(w.dock.find_active_focused(), Some((_, Tab::Query(q))) if q.id == b));

        assert_eq!(w.close_tabs(TabClose::All).len(), 1);
        assert!(keys(&w).is_empty());
        assert!(w.close_tabs(TabClose::All).is_empty());

        // New tabs still open after everything was closed.
        let c = w.open_query_tab(None, String::new(), None);
        assert_eq!(keys(&w), [c]);
    }

    #[test]
    fn show_welcome_reopens_and_focuses_the_home_page() {
        let mut w = Workspace::default();
        // Focus the query tab, then close the welcome tab.
        let id = w.open_query_tab(None, String::new(), None);
        assert!(!active_is_welcome(&mut w));
        let path = w
            .dock
            .find_tab_from(|t| matches!(t, Tab::Welcome))
            .expect("welcome");
        w.dock.remove_tab(path);
        assert!(
            w.dock
                .find_tab_from(|t| matches!(t, Tab::Welcome))
                .is_none()
        );

        w.show_welcome();
        assert!(active_is_welcome(&mut w));
        assert!(w.query_tab_mut(id).is_some(), "other tabs are kept");

        // Already open but behind another tab: brought to the front, not duplicated.
        w.open_query_tab(None, String::new(), None);
        w.show_welcome();
        assert!(active_is_welcome(&mut w));
        let count = w
            .dock
            .iter_all_tabs()
            .filter(|(_, t)| matches!(t, Tab::Welcome))
            .count();
        assert_eq!(count, 1);
    }
}
