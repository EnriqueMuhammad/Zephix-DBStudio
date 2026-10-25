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
    /// Active tab last scrolled fully into the tab bar (not persisted).
    #[serde(skip)]
    tab_reveal: Option<u64>,
}

impl Default for Workspace {
    fn default() -> Self {
        let first = QueryTab::new(1, None, SAMPLE_SQL.to_owned(), None);
        Self {
            dock: DockState::new(vec![Tab::Welcome, Tab::Query(Box::new(first))]),
            next_tab_id: 2,
            sidebar_visible: true,
            tab_reveal: None,
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

    /// Gives overflowing tab bars a width that lands on a tab boundary, and scrolls the
    /// active tab into view when it changes. `fallback_bar` is the dock width on the first
    /// frame, before a leaf has a rectangle.
    pub fn layout_tabs(
        &mut self,
        ctx: &egui::Context,
        font: egui::FontId,
        fallback_bar: f32,
    ) -> TabStripPlan {
        let measure = |text: &str| -> f32 {
            ctx.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
                    .size()
                    .x
            })
        };

        struct LeafMeasure {
            path: egui_dock::NodePath,
            bar: f32,
            widths: Vec<f32>,
            active: usize,
            scroll: f32,
            key: u64,
        }

        let mut leaves = Vec::new();
        for (path, node) in self.dock.iter_all_nodes() {
            let Some(leaf) = node.get_leaf() else {
                continue;
            };
            if leaf.tabs.is_empty() || leaf.collapsed || leaf.tab_bar_hidden {
                continue;
            }
            let width = leaf.rect.width();
            let bar = if width.is_finite() && width > 32.0 {
                width
            } else {
                fallback_bar
            };
            let widths = leaf
                .tabs
                .iter()
                .map(|tab| measure(&tab.heading()))
                .collect();
            let key = leaf.tabs.get(leaf.active.0).map(Tab::key).unwrap_or(0);
            leaves.push(LeafMeasure {
                path,
                bar,
                widths,
                active: leaf.active.0,
                scroll: leaf.scroll,
                key,
            });
        }

        let plan = plan_strip(
            &leaves
                .iter()
                .map(|leaf| (leaf.widths.as_slice(), leaf.bar))
                .collect::<Vec<_>>(),
        );

        let focused_key = self.dock.focused_leaf().and_then(|path| {
            leaves
                .iter()
                .find(|leaf| leaf.path == path)
                .map(|leaf| leaf.key)
        });
        let changed = focused_key != self.tab_reveal;
        if changed {
            self.tab_reveal = focused_key;
        }
        if changed
            && let Some(slot) = plan.slot
            && let Some(path) = self.dock.focused_leaf()
            && let Some(leaf) = leaves.iter().find(|leaf| leaf.path == path)
        {
            let available = (leaf.bar - TAB_ADD).max(0.0);
            let widths = vec![slot; leaf.widths.len()];
            let scroll = reveal_scroll(&widths, leaf.active, available, leaf.scroll);
            if let Ok(node) = self.dock.leaf_mut(leaf.path) {
                node.scroll = scroll;
            }
        }
        plan
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

    /// Full tab title, including the icon. This is what the tooltip shows.
    pub fn heading(&self) -> String {
        match self {
            Tab::Welcome => format!("{}  Welcome", icon::HOUSE),
            Tab::History { .. } => format!("{}  History", icon::CLOCK),
            Tab::Tool(t) => t.title(),
            Tab::Query(q) => {
                let glyph = if q.run.running.is_some() {
                    icon::CIRCLE_NOTCH
                } else if q.data_view.is_some() {
                    icon::TABLE
                } else {
                    icon::FILE_SQL
                };
                format!("{glyph}  {}", q.title)
            }
        }
    }
}

/// How wide tabs should be when the strip runs out of room.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabStripPlan {
    /// Width of every tab, when titles no longer fit whole. `None` when they do.
    pub slot: Option<f32>,
    /// Longest title galley that still fits inside `slot`.
    pub text_px: Option<f32>,
}

/// Padding and the close button egui_dock 0.21 adds around a tab title.
const TAB_CHROME: f32 = 40.0;
/// The "+" button reserved at the end of the tab bar.
const TAB_ADD: f32 = 24.0;
/// Shortest slot we try before shrinking further: chrome plus a readable fragment.
const TAB_MIN_SLOT: f32 = TAB_CHROME + 28.0;

/// Picks one tab width so every leaf shows whole tabs and the rest scroll.
fn plan_strip(leaves: &[(&[f32], f32)]) -> TabStripPlan {
    let overflowing = leaves.iter().any(|(widths, bar)| overflows(widths, *bar));
    let Some(slot) = overflowing.then(|| choose_slot(leaves)).flatten() else {
        return TabStripPlan {
            slot: None,
            text_px: None,
        };
    };
    TabStripPlan {
        slot: Some(slot),
        // Two extra points so a font-hint difference cannot push the title under the close button.
        text_px: Some((slot - TAB_CHROME - 2.0).max(4.0)),
    }
}

fn overflows(widths: &[f32], bar: f32) -> bool {
    if widths.is_empty() || !bar.is_finite() {
        return false;
    }
    let clip = bar - TAB_ADD;
    widths.iter().map(|width| width + TAB_CHROME).sum::<f32>() > clip + 0.5
}

fn choose_slot(leaves: &[(&[f32], f32)]) -> Option<f32> {
    let clips: Vec<(f32, usize)> = leaves
        .iter()
        .filter_map(|(widths, bar)| {
            if widths.is_empty() || !bar.is_finite() {
                return None;
            }
            let clip = bar - TAB_ADD;
            (clip >= 16.0).then_some((clip, widths.len()))
        })
        .collect();
    if clips.is_empty() {
        return None;
    }
    let max_slot = clips
        .iter()
        .map(|(clip, _)| clip - 4.0)
        .fold(f32::MAX, f32::min);
    let mut floor = TAB_MIN_SLOT;
    loop {
        let mut slot_px = max_slot.floor() as i32;
        let stop_px = floor.min(max_slot).ceil() as i32;
        while slot_px >= stop_px {
            let slot = slot_px as f32;
            if clips
                .iter()
                .all(|(clip, count)| clips_cleanly(*clip, *count, slot))
            {
                return Some(slot);
            }
            slot_px -= 1;
        }
        if floor <= 20.0 {
            return Some(max_slot.max(8.0));
        }
        floor = (floor * 0.5).max(20.0);
    }
}

/// `slot` shows `count` tabs in `clip` points without cutting a title.
/// A gap of at most 8 points is only the tab's left padding, so no glyph is sliced.
fn clips_cleanly(clip: f32, count: usize, slot: f32) -> bool {
    if !(slot.is_finite() && slot > 1.0 && clip.is_finite()) || count == 0 {
        return false;
    }
    let fits = (clip / slot).floor() as usize;
    let visible = fits.clamp(1, count);
    let used = visible as f32 * slot;
    if used > clip - 3.5 {
        return false;
    }
    visible >= count || clip - used <= 8.0
}

/// Scroll offset that puts tab `index` fully inside a bar of `available` points.
/// Offsets are negative, matching egui_dock's tab-bar scroll.
fn reveal_scroll(widths: &[f32], index: usize, available: f32, current: f32) -> f32 {
    if widths.is_empty() || !available.is_finite() || available <= 0.0 {
        return current;
    }
    let index = index.min(widths.len() - 1);
    let left: f32 = widths.iter().take(index).sum();
    let right = left + widths[index];
    let view_left = -current;
    let mut new_left = view_left;
    if right > view_left + available {
        new_left = right - available;
    }
    if left < new_left {
        new_left = left;
    }
    let total: f32 = widths.iter().sum();
    let max_scroll = (total - available).max(0.0);
    (-new_left).clamp(-max_scroll, 0.0)
}

/// Shortens `text` with an ellipsis so it fits in `max_width` points of the button font.
fn ellipsize_title(ctx: &egui::Context, font: &egui::FontId, text: &str, max_width: f32) -> String {
    let width = |sample: &str| -> f32 {
        ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(sample.to_owned(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        })
    };
    if width(text) <= max_width {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut low = 0;
    let mut high = chars.len();
    while low + 1 < high {
        let mid = (low + high) / 2;
        let mut sample: String = chars[..mid].iter().collect();
        sample.push('…');
        if width(&sample) <= max_width {
            low = mid;
        } else {
            high = mid;
        }
    }
    if low == 0 {
        return "…".to_owned();
    }
    let mut out: String = chars[..low].iter().collect();
    out.push('…');
    out
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
    /// Maximum title width when the tab strip overflows. `None` when every title fits.
    pub tab_text_px: Option<f32>,
    /// Context used to measure titles. Absent when no tab is being drawn.
    pub egui: Option<egui::Context>,
    /// Button font the tab bar paints titles with.
    pub button_font: Option<egui::FontId>,
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
        let full = tab.heading();
        match (
            self.tab_text_px,
            self.egui.as_ref(),
            self.button_font.as_ref(),
        ) {
            (Some(max_width), Some(ctx), Some(font)) => {
                ellipsize_title(ctx, font, &full, max_width).into()
            }
            _ => full.into(),
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
        let full = tab.heading();
        if let (Some(max_width), Some(ctx), Some(font)) = (
            self.tab_text_px,
            self.egui.as_ref(),
            self.button_font.as_ref(),
        ) {
            let shown = ellipsize_title(ctx, font, &full, max_width);
            if shown != full {
                response.clone().on_hover_text(full.clone());
            }
        }
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

    #[test]
    fn short_titles_keep_their_own_width() {
        let plan = plan_strip(&[(&[40.0, 50.0], 500.0)]);
        assert_eq!(plan.slot, None);
        assert_eq!(plan.text_px, None);
    }

    #[test]
    fn overflowing_tabs_end_on_a_boundary() {
        let widths = vec![120.0; 8];
        let bar = 360.0;
        let plan = plan_strip(&[(&widths, bar)]);
        let slot = plan.slot.expect("the strip overflows");
        let clip = bar - TAB_ADD;
        assert!(
            clips_cleanly(clip, widths.len(), slot),
            "slot {slot} clips a title"
        );
        let text = plan.text_px.expect("text budget");
        assert!(text <= slot - TAB_CHROME);
        // Whole tabs fit in the space left beside the add button, and at least one scrolls away.
        let visible = (clip / slot).floor() as usize;
        assert!(visible >= 1 && visible < widths.len());
    }

    #[test]
    fn every_leaf_keeps_whole_tabs() {
        let wide = vec![90.0; 6];
        let narrow = vec![90.0; 6];
        let plan = plan_strip(&[(&wide, 640.0), (&narrow, 220.0)]);
        let slot = plan.slot.expect("the narrow strip overflows");
        assert!(clips_cleanly(640.0 - TAB_ADD, wide.len(), slot));
        assert!(clips_cleanly(220.0 - TAB_ADD, narrow.len(), slot));
    }

    #[test]
    fn reveal_scroll_brings_the_active_tab_into_view() {
        let widths = [100.0; 8];
        let scroll = reveal_scroll(&widths, 5, 240.0, 0.0);
        assert!((scroll + 360.0).abs() < 0.1, "scroll {scroll}");
        let again = reveal_scroll(&widths, 5, 240.0, scroll);
        assert!((again - scroll).abs() < 0.1);
        assert_eq!(reveal_scroll(&widths, 1, 240.0, 0.0), 0.0);
    }

    #[test]
    fn ellipsize_title_keeps_short_text_and_marks_long_text() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        let font = egui::TextStyle::Button.resolve(ctx.global_style().as_ref());
        assert_eq!(ellipsize_title(&ctx, &font, "Query 1", 400.0), "Query 1");
        let long = "Query 1 — a very long tab title that cannot fit";
        let short = ellipsize_title(&ctx, &font, long, 48.0);
        assert!(short.ends_with('…'), "{short}");
        assert!(short.chars().count() < long.chars().count());
        assert!(!short.contains("cannot fit"));
        output.textures_delta.clear();
    }
}
