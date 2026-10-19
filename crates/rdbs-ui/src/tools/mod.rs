//! Tool tabs: table designer, schema diff, ER diagram, data transfer, query builder, backups
//! and import. Each tool keeps its own request ids and consumes the worker events for them.

pub mod backup;
pub mod builder;
pub mod designer;
pub mod diff;
pub mod er;
pub mod import;
pub mod picker;
pub mod transfer;

use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use rdbs_core::{Command, ConnectionProfile, DatabaseKind, Event, ProfileId, RequestId};
use serde::{Deserialize, Serialize};

use crate::model::{AfterPassword, StatusMessage, UiAction};
use crate::theme::color;
use crate::workspace::TabContext;

/// Where a tool works: a connection, optionally a database and schema inside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// Connection.
    pub profile_id: ProfileId,
    /// Database (catalog); for SQLite always `None`.
    pub database: Option<String>,
    /// Schema (PostgreSQL / SQL Server).
    pub schema: Option<String>,
}

impl Scope {
    /// The connection profile, if it still exists.
    pub fn profile<'a>(&self, cx: &'a TabContext<'_>) -> Option<&'a ConnectionProfile> {
        cx.profile(self.profile_id)
    }

    /// Engine of the connection.
    pub fn kind(&self, cx: &TabContext<'_>) -> Option<DatabaseKind> {
        self.profile(cx).map(|p| p.kind)
    }

    /// `connection · database · schema` for headers.
    pub fn label(&self, cx: &TabContext<'_>) -> String {
        let mut parts = vec![
            self.profile(cx)
                .map_or_else(|| "(deleted connection)".to_owned(), |p| p.name.clone()),
        ];
        parts.extend(self.database.iter().cloned());
        parts.extend(self.schema.iter().cloned());
        parts.join(" · ")
    }

    /// Schema argument for `DescribeSchema` (only engines with schemas use it).
    pub fn describe_schema(&self, kind: DatabaseKind) -> Option<String> {
        kind.has_schemas().then(|| self.schema.clone()).flatten()
    }
}

/// Builds the scope of an explorer path, normalising engine differences.
pub fn scope(
    kind: DatabaseKind,
    profile_id: ProfileId,
    database: Option<String>,
    schema: Option<String>,
) -> Scope {
    Scope {
        profile_id,
        database: (kind != DatabaseKind::Sqlite).then_some(database).flatten(),
        schema: kind.has_schemas().then_some(schema).flatten(),
    }
}

/// Sends a command for `scope`, or asks for the password first. `make` receives the request
/// id, profile, session database and password.
pub fn send(
    cx: &mut TabContext<'_>,
    scope: &Scope,
    make: impl FnOnce(RequestId, ConnectionProfile, Option<String>, Option<String>) -> Command,
) -> Option<RequestId> {
    let Some(profile) = scope.profile(cx).cloned() else {
        cx.actions.push(UiAction::Status(StatusMessage::error(
            "The connection of this tab no longer exists",
        )));
        return None;
    };
    if crate::editor::needs_password(&profile, cx.connections, cx.passwords) {
        cx.actions.push(UiAction::NeedPassword {
            profile_id: profile.id,
            then: AfterPassword::Connect,
        });
        return None;
    }
    let request_id = cx.services.next_request_id();
    let password = cx.passwords.get(&profile.id).cloned();
    cx.services
        .send(make(request_id, profile, scope.database.clone(), password));
    Some(request_id)
}

/// Sends `DescribeSchema` for `scope`.
pub fn describe(cx: &mut TabContext<'_>, scope: &Scope) -> Option<RequestId> {
    let schema = scope.kind(cx).and_then(|k| scope.describe_schema(k));
    send(cx, scope, |request_id, profile, database, password| {
        Command::DescribeSchema {
            request_id,
            profile,
            database,
            schema,
            password,
        }
    })
}

/// Sends `RunScript` for `scope`.
pub fn run_script(
    cx: &mut TabContext<'_>,
    scope: &Scope,
    statements: Vec<String>,
) -> Option<RequestId> {
    send(cx, scope, |request_id, profile, database, password| {
        Command::RunScript {
            request_id,
            profile,
            database,
            statements,
            password,
        }
    })
}

/// Request to open a tool tab.
#[derive(Debug, Clone)]
pub enum ToolLaunch {
    /// Create (`table: None`) or alter a table.
    Designer {
        /// Where the table lives.
        scope: Scope,
        /// Existing table.
        table: Option<String>,
    },
    /// Import a CSV / spreadsheet file.
    Import {
        /// Target database/schema.
        scope: Scope,
        /// Preselected target table.
        table: Option<String>,
    },
    /// Compare two schemas.
    Diff {
        /// Preselected source.
        source: Option<Scope>,
    },
    /// ER diagram of a database/schema.
    Er {
        /// What to draw.
        scope: Scope,
    },
    /// Copy / synchronise data between connections.
    Transfer {
        /// Preselected source.
        source: Option<Scope>,
    },
    /// Visual query builder.
    Builder {
        /// Where the query runs.
        scope: Scope,
    },
    /// Backups and schedules.
    Backup {
        /// Prefill a new backup for this database.
        scope: Option<Scope>,
    },
}

/// A tool tab.
#[derive(Serialize, Deserialize)]
pub struct ToolTab {
    /// Unique id within the workspace (shared counter with query tabs).
    pub id: u64,
    /// The tool and its state.
    pub tool: Tool,
}

/// Tool state.
#[derive(Serialize, Deserialize)]
pub enum Tool {
    /// Table designer.
    Designer(designer::Designer),
    /// File import.
    Import(import::Import),
    /// Schema diff.
    Diff(diff::Diff),
    /// ER diagram.
    Er(er::Er),
    /// Data transfer.
    Transfer(transfer::Transfer),
    /// Query builder.
    Builder(builder::Builder),
    /// Backups.
    Backup(backup::Backup),
}

impl ToolTab {
    /// Creates the tab for a launch request.
    pub fn new(id: u64, launch: ToolLaunch) -> Self {
        let tool = match launch {
            ToolLaunch::Designer { scope, table } => {
                Tool::Designer(designer::Designer::new(scope, table))
            }
            ToolLaunch::Import { scope, table } => Tool::Import(import::Import::new(scope, table)),
            ToolLaunch::Diff { source } => Tool::Diff(diff::Diff::new(source)),
            ToolLaunch::Er { scope } => Tool::Er(er::Er::new(scope)),
            ToolLaunch::Transfer { source } => Tool::Transfer(transfer::Transfer::new(source)),
            ToolLaunch::Builder { scope } => Tool::Builder(builder::Builder::new(scope)),
            ToolLaunch::Backup { scope } => Tool::Backup(backup::Backup::new(scope)),
        };
        Self { id, tool }
    }

    /// Tab title.
    pub fn title(&self) -> String {
        match &self.tool {
            Tool::Designer(d) => format!("{}  {}", icon::WRENCH, d.title()),
            Tool::Import(t) => format!("{}  {}", icon::DOWNLOAD_SIMPLE, t.title()),
            Tool::Diff(t) => format!("{}  {}", icon::GIT_DIFF, t.title()),
            Tool::Er(t) => format!("{}  {}", icon::GRAPH, t.title()),
            Tool::Transfer(t) => format!("{}  {}", icon::FLOW_ARROW, t.title()),
            Tool::Builder(t) => format!("{}  {}", icon::PUZZLE_PIECE, t.title()),
            Tool::Backup(t) => format!("{}  {}", icon::ARCHIVE, t.title()),
        }
    }

    /// Renders the tab.
    pub fn show(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        match &mut self.tool {
            Tool::Designer(d) => d.show(ui, cx),
            Tool::Import(t) => t.show(ui, cx),
            Tool::Diff(t) => t.show(ui, cx),
            Tool::Er(t) => t.show(ui, cx),
            Tool::Transfer(t) => t.show(ui, cx),
            Tool::Builder(t) => t.show(ui, cx),
            Tool::Backup(t) => t.show(ui, cx),
        }
    }

    /// Offers a worker event; returns true when this tab consumed it.
    pub fn on_event(&mut self, event: &Event) -> bool {
        match &mut self.tool {
            Tool::Designer(d) => d.on_event(event),
            Tool::Import(t) => t.on_event(event),
            Tool::Diff(t) => t.on_event(event),
            Tool::Er(t) => t.on_event(event),
            Tool::Transfer(t) => t.on_event(event),
            Tool::Builder(t) => t.on_event(event),
            Tool::Backup(t) => t.on_event(event),
        }
    }

    /// Whether this tool edits the same thing as `launch` (to focus instead of duplicating).
    pub fn matches(&self, launch: &ToolLaunch) -> bool {
        match (&self.tool, launch) {
            (
                Tool::Designer(d),
                ToolLaunch::Designer {
                    scope,
                    table: Some(table),
                },
            ) => d.scope == *scope && d.table.as_ref() == Some(table),
            (Tool::Er(e), ToolLaunch::Er { scope }) => e.scope == *scope,
            (Tool::Backup(_), ToolLaunch::Backup { scope: None }) => true,
            _ => false,
        }
    }
}

/// Header line shared by tools: icon, title and scope.
pub fn header(ui: &mut Ui, glyph: &str, title: &str, scope: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(glyph).size(18.0).color(color::accent()));
        ui.label(RichText::new(title).strong().size(15.0));
        ui.label(RichText::new(scope).color(color::text_dim()));
    });
}

/// Error box shared by tools.
pub fn error_box(ui: &mut Ui, text: &str) {
    egui::Frame::new()
        .fill(color::danger().gamma_multiply(0.08))
        .stroke(egui::Stroke::new(1.0, color::danger().gamma_multiply(0.4)))
        .corner_radius(6)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{}  {text}", icon::WARNING)).color(color::danger()),
                )
                .selectable(true),
            );
        });
}

/// Full text of a worker error, including its detail.
pub fn error_text(e: &rdbs_core::DbError) -> String {
    match &e.detail {
        Some(detail) => format!("{e}\n{detail}"),
        None => e.to_string(),
    }
}
