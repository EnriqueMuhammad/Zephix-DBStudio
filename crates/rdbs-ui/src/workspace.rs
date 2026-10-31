//! Dockable tab workspace: tab model, persisted layout and the `egui_dock` tab viewer.

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Id, Rect, Ui, WidgetText};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockState, TabViewer};
use egui_phosphor::regular as icon;
use rdbs_core::{Command, ConnectionProfile, DbError, ProfileId, QueryOutcome, RequestId};
use rdbs_services::ServiceHandle;
use serde::{Deserialize, Serialize};

use crate::grid::GridState;
use crate::model::{ConnState, TabClose, TabLink, UiAction};
use crate::theme::color;
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
        let title = title.unwrap_or_else(|| format!("Query {}", self.free_query_number()));
        self.dock
            .push_to_focused_leaf(Tab::Query(Box::new(QueryTab::new(
                id,
                profile_id,
                sql,
                Some(title),
            ))));
        id
    }

    /// Lowest number not used by an open "Query N" tab, so numbering starts at 1 again once
    /// those tabs are closed (also across restarts). Tab ids stay unique regardless.
    fn free_query_number(&self) -> u64 {
        let used: std::collections::HashSet<u64> = self
            .dock
            .iter_all_tabs()
            .filter_map(|(_, tab)| match tab {
                Tab::Query(q) => q.title.strip_prefix("Query ")?.parse().ok(),
                _ => None,
            })
            .collect();
        (1..).find(|n| !used.contains(n)).unwrap_or(1)
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

    /// Moves tabs out of floating windows back into the main area. Tabs cannot be dragged out
    /// any more, but workspaces saved by earlier versions may still have such windows.
    pub fn dock_floating_tabs(&mut self) {
        let windows: Vec<egui_dock::SurfaceIndex> = self
            .dock
            .iter_surfaces_indexed()
            .map(|(index, _)| index)
            .filter(|index| !index.is_main())
            .collect();
        for index in windows.into_iter().rev() {
            let Some(mut surface) = self.dock.remove_surface(index) else {
                continue;
            };
            let tabs: Vec<Tab> = surface
                .iter_nodes_mut()
                .filter_map(|node| node.get_leaf_mut())
                .flat_map(|leaf| std::mem::take(&mut leaf.tabs))
                .collect();
            for tab in tabs {
                self.dock.push_to_first_leaf(tab);
            }
        }
    }

    /// Ids and titles of the query tabs with unsaved changes, in tab order.
    pub fn unsaved_tabs(&self) -> Vec<(u64, String)> {
        self.dock
            .iter_all_tabs()
            .filter_map(|(_, tab)| match tab {
                Tab::Query(q) if q.is_dirty() => Some((q.id, q.title.clone())),
                _ => None,
            })
            .collect()
    }

    /// The open query tab for a file, if any.
    pub fn tab_for_path(&self, path: &std::path::Path) -> Option<u64> {
        self.dock.iter_all_tabs().find_map(|(_, tab)| match tab {
            Tab::Query(q) if q.path.as_deref() == Some(path) => Some(q.id),
            _ => None,
        })
    }

    /// Makes a tab active and focuses its panel.
    pub fn focus_tab(&mut self, key: u64) {
        if let Some(path) = self.dock.find_tab_from(|t| t.key() == key) {
            let _ = self.dock.set_active_tab(path);
            self.dock.set_focused_node_and_surface(path.node_path());
        }
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

    /// Closes the tabs `which` selects and returns them. After "close others" the kept tab is
    /// active and focused.
    pub fn close_tabs(&mut self, which: TabClose) -> Vec<Tab> {
        self.close_where(which, |_| true)
    }

    /// Closes the tabs `which` selects for which `close` holds (unsaved scripts wait for a
    /// decision instead), and returns them.
    pub fn close_where(&mut self, which: TabClose, close: impl Fn(&Tab) -> bool) -> Vec<Tab> {
        let mut closed = Vec::new();
        while let Some(path) = self
            .dock
            .find_tab_from(|t| which.closes(t.key()) && close(t))
        {
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
            let dot = measure("●  ");
            let widths = leaf
                .tabs
                .iter()
                .map(|tab| {
                    let text = measure(&tab.heading());
                    if matches!(tab, Tab::Query(_)) {
                        text + dot
                    } else {
                        text
                    }
                })
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
                let dirty = if q.is_dirty() { "*" } else { "" };
                format!("{glyph}  {}{dirty}", q.title)
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
/// A title wider than this is ellipsized so one long name cannot swallow the bar.
const TAB_MAX: f32 = 240.0;

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
    // Start near the real title width. Searching from the whole bar downward
    // would accept one tab stretched across the strip and scroll every other tab away.
    let widest = leaves
        .iter()
        .flat_map(|(widths, _)| widths.iter().copied())
        .map(|width| (width + TAB_CHROME).min(TAB_MAX))
        .fold(TAB_MIN_SLOT, f32::max);
    let narrowest = clips
        .iter()
        .map(|(clip, _)| clip - 4.0)
        .fold(f32::MAX, f32::min);
    let max_slot = (widest + 36.0).min(narrowest).max(8.0);
    let mut floor = TAB_MIN_SLOT.min(max_slot);
    loop {
        let mut slot_px = max_slot.floor() as i32;
        let stop_px = floor.ceil() as i32;
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
    /// Table the tab was opened from; enables editing its rows.
    #[serde(default)]
    pub table: Option<rdbs_core::edit::TableTarget>,
    /// Set for a data tab: the rows of one table or view, without an SQL editor.
    #[serde(default)]
    pub data_view: Option<DataView>,
    /// Editor contents.
    pub sql: String,
    /// File the script was opened from or last saved to.
    #[serde(default)]
    pub path: Option<std::path::PathBuf>,
    /// Contents as last opened, saved or generated; the tab has unsaved changes when `sql`
    /// differs. Tabs restored from older versions have none, so their text counts as unsaved.
    #[serde(default)]
    pub saved: String,
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
            saved: sql.clone(),
            sql,
            path: None,
            run: RunState {
                focus_editor: true,
                ..RunState::default()
            },
        }
    }

    /// Whether the script differs from what was last opened, saved or generated. Data tabs
    /// have no script of their own.
    pub fn is_dirty(&self) -> bool {
        self.data_view.is_none() && self.sql != self.saved
    }

    /// Records that the script now lives in `path` with the current contents.
    pub fn mark_saved(&mut self, path: std::path::PathBuf) {
        if let Some(name) = path.file_name() {
            self.title = name.to_string_lossy().into_owned();
        }
        self.path = Some(path);
        self.saved = self.sql.clone();
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
    /// Database whose objects this tab asked to load for completion (asked once).
    pub catalog_requested: Option<String>,
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

/// Whether an in-flight query is still opening its session or already executing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhase {
    /// The worker is opening the session. The statement has not started.
    Connecting,
    /// The session is open and the statement is executing.
    Running,
}

impl RunPhase {
    /// Status-line and query-tab wording.
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting…",
            Self::Running => "Running…",
        }
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
    /// Connecting until [`rdbs_core::Event::QueryStarted`], then running.
    pub phase: RunPhase,
}

/// A finished execution.
pub struct LastRun {
    /// Wall-clock duration.
    pub elapsed: Duration,
    /// Result or error.
    pub outcome: Result<QueryOutcome, DbError>,
    /// Connection name, for the status line.
    pub profile_name: String,
    /// When the execution finished, for the Messages view.
    pub finished: chrono::DateTime<chrono::Local>,
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
        let shown = match (
            self.tab_text_px,
            self.egui.as_ref(),
            self.button_font.as_ref(),
        ) {
            (Some(max_width), Some(ctx), Some(font)) => {
                let room = if matches!(tab, Tab::Query(_)) {
                    (max_width - dot_width(ctx, font)).max(4.0)
                } else {
                    max_width
                };
                ellipsize_title(ctx, font, &full, room)
            }
            _ => full,
        };
        match tab {
            Tab::Query(query) => session_title(self, query, &shown),
            _ => shown.into(),
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
        // Unsaved scripts go through the application, which asks whether to save.
        if let Tab::Query(q) = tab
            && q.is_dirty()
        {
            self.actions.push(UiAction::CloseTabs(TabClose::One(q.id)));
            return OnCloseResponse::Ignore;
        }
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
        let shown = match (
            self.tab_text_px,
            self.egui.as_ref(),
            self.button_font.as_ref(),
        ) {
            (Some(max_width), Some(ctx), Some(font)) => {
                let room = if matches!(tab, Tab::Query(_)) {
                    (max_width - dot_width(ctx, font)).max(4.0)
                } else {
                    max_width
                };
                ellipsize_title(ctx, font, &full, room)
            }
            _ => full.clone(),
        };
        let query = match tab {
            Tab::Query(query) => Some((
                query.profile_id,
                query.database.clone(),
                query.run.running.is_some(),
                query.path.clone(),
                session_tip(self, query),
            )),
            _ => None,
        };
        if let Some((profile_id, database, running, path, tip)) = &query {
            let mut lines = Vec::new();
            if shown != full {
                lines.push(full.clone());
            }
            if let Some(path) = path {
                lines.push(path.display().to_string());
            }
            lines.push(tip.clone());
            response.clone().on_hover_text(lines.join("\n"));
            if response.clicked()
                && !*running
                && dot_clicked(response, &shown)
                && let Some(profile_id) = *profile_id
            {
                self.actions.push(UiAction::Reconnect {
                    profile_id,
                    database: database.clone(),
                    retry_tab: None,
                });
            }
        } else if shown != full {
            response.clone().on_hover_text(full);
        }
        // Replaces egui_dock's own tab menu (see `DockArea::tab_context_menus`).
        let key = tab.key();
        let script = matches!(tab, Tab::Query(q) if q.data_view.is_none());
        let reconnect = query.as_ref().map(|q| (q.0, q.1.clone(), q.2));
        response.context_menu(|ui| {
            if let Some((profile_id, database, running)) = &reconnect {
                let enabled = profile_id.is_some() && !running;
                if ui
                    .add_enabled(enabled, egui::Button::new("Reconnect"))
                    .on_hover_text(
                        "Close this tab's database session and open a new one. The text stays.",
                    )
                    .on_disabled_hover_text(if profile_id.is_none() {
                        "Choose a connection first"
                    } else {
                        "Wait for the running query to finish"
                    })
                    .clicked()
                    && let Some(profile_id) = *profile_id
                {
                    self.actions.push(UiAction::Reconnect {
                        profile_id,
                        database: database.clone(),
                        retry_tab: None,
                    });
                    ui.close();
                }
                ui.separator();
            }
            if script {
                for (label, save_as) in [("Save", false), ("Save As…", true)] {
                    if ui.button(label).clicked() {
                        self.actions.push(UiAction::SaveQuery {
                            tab_id: Some(key),
                            save_as,
                        });
                        ui.close();
                    }
                }
                ui.separator();
            }
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

    fn allowed_in_windows(&self, _tab: &mut Tab) -> bool {
        // A floating window has no toolbar, explorer or menus of its own; tabs stay docked.
        false
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

/// Width of the status bullet and the spaces that follow it.
fn dot_width(ctx: &egui::Context, font: &FontId) -> f32 {
    ctx.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap("●  ".into(), font.clone(), Color32::WHITE)
            .size()
            .x
    })
}

fn link_color(link: TabLink) -> Color32 {
    match link {
        TabLink::Up => color::success(),
        TabLink::Down => color::warning(),
        TabLink::Off => color::text_faint(),
    }
}

fn query_link(cx: &TabContext<'_>, query: &QueryTab) -> (TabLink, &'static str) {
    let Some(profile) = query.profile_id.and_then(|id| cx.profile(id)) else {
        return (TabLink::Off, "No connection chosen");
    };
    let key = profile.session_database(query.database.clone());
    query_session_link(
        query.run.running.as_ref().map(|running| running.phase),
        cx.connections.get(&profile.id),
        &key,
    )
}

/// Dot for one query tab. A query that is still connecting stays grey, including when the
/// profile was already connected or the previous session had been dropped.
pub fn query_session_link(
    phase: Option<RunPhase>,
    conn: Option<&ConnState>,
    database: &Option<String>,
) -> (TabLink, &'static str) {
    if phase == Some(RunPhase::Connecting) {
        return (TabLink::Off, "Connecting…");
    }
    match conn {
        Some(conn) => (conn.link(database), conn.link_label(database)),
        None => (TabLink::Off, "Not connected"),
    }
}

fn session_tip(cx: &TabContext<'_>, query: &QueryTab) -> String {
    let Some(profile) = query.profile_id.and_then(|id| cx.profile(id)) else {
        return "No connection chosen\nClick the dot after choosing a connection".into();
    };
    let (_, label) = query_link(cx, query);
    let db = query
        .database
        .as_deref()
        .filter(|db| !db.is_empty())
        .unwrap_or(profile.database.as_str());
    let database = if db.is_empty() {
        String::new()
    } else {
        format!(" · {db}")
    };
    format!(
        "{}{database}\n{label}\nClick the dot, or right-click and choose Reconnect",
        profile.name
    )
}

/// Coloured bullet plus the tab title. The title itself keeps the tab-bar colour.
fn session_title(cx: &TabContext<'_>, query: &QueryTab, shown: &str) -> WidgetText {
    let font = cx
        .button_font
        .clone()
        .unwrap_or_else(|| FontId::proportional(13.5));
    let (link, _) = query_link(cx, query);
    let mut job = LayoutJob::default();
    job.append(
        "●",
        0.0,
        TextFormat {
            font_id: font.clone(),
            color: link_color(link),
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job.append(
        &format!("  {shown}"),
        0.0,
        TextFormat {
            font_id: font,
            color: Color32::PLACEHOLDER,
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job.into()
}

/// Whether the click landed on the status bullet, which sits at the start of the centred title.
fn dot_clicked(response: &egui::Response, shown: &str) -> bool {
    let Some(pos) = response.interact_pointer_pos() else {
        return false;
    };
    let font = egui::TextStyle::Button.resolve(response.ctx.global_style().as_ref());
    let width = |text: &str| -> f32 {
        response.ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
    };
    let text_w = width(&format!("●  {shown}"));
    let dot_w = width("●") + 8.0;
    let mut text_rect = response.rect;
    text_rect.set_width((text_rect.width() - 24.0).max(0.0));
    let inner = text_rect.shrink2(egui::vec2(8.0, 0.0));
    let left = inner.center().x - text_w / 2.0;
    let dot = Rect::from_min_size(
        egui::pos2(left - 1.0, response.rect.top()),
        egui::vec2(dot_w, response.rect.height()),
    );
    dot.contains(pos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ConnStatus;

    #[test]
    fn connecting_query_keeps_the_tab_dot_grey() {
        let mut conn = ConnState {
            status: ConnStatus::Connected {
                server_version: "PostgreSQL".into(),
                capabilities: rdbs_core::Capabilities::default(),
            },
            ..ConnState::default()
        };
        let db = None;
        assert_eq!(
            query_session_link(Some(RunPhase::Running), Some(&conn), &db).0,
            TabLink::Up
        );
        assert_eq!(
            query_session_link(Some(RunPhase::Connecting), Some(&conn), &db),
            (TabLink::Off, "Connecting…")
        );
        conn.dropped.insert(None);
        assert_eq!(
            query_session_link(Some(RunPhase::Connecting), Some(&conn), &db),
            (TabLink::Off, "Connecting…")
        );
        assert_eq!(query_session_link(None, Some(&conn), &db).0, TabLink::Down);
        assert_eq!(
            query_session_link(None, None, &db),
            (TabLink::Off, "Not connected")
        );
    }

    fn active_is_welcome(w: &mut Workspace) -> bool {
        matches!(w.dock.find_active_focused(), Some((_, Tab::Welcome)))
    }

    fn keys(w: &Workspace) -> Vec<u64> {
        w.dock.iter_all_tabs().map(|(_, t)| t.key()).collect()
    }

    fn query_titles(w: &Workspace) -> Vec<String> {
        w.dock
            .iter_all_tabs()
            .filter_map(|(_, t)| match t {
                Tab::Query(q) => Some(q.title.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn unsaved_changes_are_tracked_per_tab() {
        let mut w = Workspace::default(); // Welcome and Query 1 with the sample script
        let scratch = w.open_query_tab(None, String::new(), None);
        let ddl = w.open_query_tab(None, "CREATE TABLE t (id int)".into(), Some("t".into()));
        let tab = |w: &mut Workspace, id| w.query_tab_mut(id).expect("tab").is_dirty();
        // Generated or empty scripts start out unchanged.
        assert!(!tab(&mut w, 1) && !tab(&mut w, scratch) && !tab(&mut w, ddl));

        w.query_tab_mut(scratch).expect("tab").sql = "SELECT 1".into();
        assert!(tab(&mut w, scratch));
        let heading = w
            .dock
            .iter_all_tabs()
            .find(|(_, t)| t.key() == scratch)
            .map(|(_, t)| t.heading())
            .expect("heading");
        assert!(heading.ends_with("Query 2*"), "{heading}");

        let q = w.query_tab_mut(scratch).expect("tab");
        q.mark_saved(std::path::PathBuf::from("C:/sql/report.sql"));
        assert!(!q.is_dirty());
        assert_eq!(q.title, "report.sql");
        assert_eq!(
            w.tab_for_path(std::path::Path::new("C:/sql/report.sql")),
            Some(scratch)
        );

        assert_eq!(w.unsaved_tabs(), Vec::<(u64, String)>::new());
        w.query_tab_mut(1).expect("tab").sql.push(' ');
        assert_eq!(w.unsaved_tabs(), [(1, "Query 1".to_owned())]);
        w.query_tab_mut(1).expect("tab").sql.pop();

        // Closing everything leaves the changed tab open.
        w.query_tab_mut(ddl)
            .expect("tab")
            .sql
            .push_str(", name text");
        let closed = w.close_where(
            TabClose::All,
            |t| !matches!(t, Tab::Query(q) if q.is_dirty()),
        );
        assert_eq!(closed.len(), 3);
        assert_eq!(keys(&w), [ddl]);
    }

    #[test]
    fn floating_windows_are_docked_again() {
        let mut w = Workspace::default(); // Welcome and Query 1
        let q = QueryTab::new(7, None, "SELECT 7".into(), None);
        w.dock.add_window(vec![Tab::Query(Box::new(q))]);
        assert_eq!(w.dock.iter_surfaces().count(), 2);
        w.dock_floating_tabs();
        let main: Vec<u64> = w.dock.main_surface().tabs().map(Tab::key).collect();
        assert_eq!(main, [0, 1, 7]);
        assert!(
            w.dock
                .iter_surfaces()
                .all(|s| !matches!(s, egui_dock::Surface::Window(..)))
        );
    }

    #[test]
    fn query_numbers_restart_at_one() {
        // Welcome and Query 1, after a long session.
        let mut w = Workspace {
            next_tab_id: 57,
            ..Workspace::default()
        };
        let a = w.open_query_tab(None, String::new(), None);
        let b = w.open_query_tab(None, String::new(), None);
        assert_eq!(query_titles(&w), ["Query 1", "Query 2", "Query 3"]);
        assert_eq!((a, b), (57, 58), "ids stay unique");

        // A gap is filled first.
        w.close_tabs(TabClose::One(a));
        w.open_query_tab(None, String::new(), None);
        assert_eq!(query_titles(&w), ["Query 1", "Query 3", "Query 2"]);

        // Everything closed: back to 1. Named tabs do not take a number.
        w.close_tabs(TabClose::All);
        w.open_query_tab(None, String::new(), Some("customers".into()));
        w.open_query_tab(None, String::new(), None);
        assert_eq!(query_titles(&w), ["customers", "Query 1"]);
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
    fn short_titles_stay_readable_when_the_bar_scrolls() {
        let widths = vec![80.0; 12];
        let bar = 800.0;
        let plan = plan_strip(&[(&widths, bar)]);
        let slot = plan.slot.expect("the strip overflows");
        let clip = bar - TAB_ADD;
        let visible = (clip / slot).floor() as usize;
        assert!(
            visible >= 3,
            "slot {slot} only leaves {visible} tabs on screen"
        );
        assert!(clips_cleanly(clip, widths.len(), slot));
        // The slot stays near the title width, so the label is not crushed to a fragment.
        assert!(slot + 0.5 >= 80.0 + TAB_CHROME - 36.0, "slot {slot}");
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
