//! Query tab: toolbar, SQL editor, results area and status line.

use std::time::{Duration, Instant};

use egui::{
    Align, Button, Frame, Id, Layout, Margin, RichText, ScrollArea, Stroke, TextBuffer, TextEdit,
    Ui, Vec2,
};
use egui_phosphor::regular as icon;
use rdbs_core::sql;
use rdbs_core::{Command, ConnectionProfile, DbErrorKind};
use rdbs_services::ServiceHandle;

use crate::grid;
use crate::model::{AfterPassword, ConnState, ConnStatus, StatusMessage, UiAction};
use crate::theme::{self, color};
use crate::widgets::{self, format_count, format_duration};
use crate::workspace::{LastRun, QueryTab, RunState, Running, TabContext};

/// Renders a query tab.
pub fn show(ui: &mut Ui, q: &mut QueryTab, cx: &mut TabContext<'_>) {
    egui::Panel::top(Id::new(("q-toolbar", q.id)))
        .frame(
            Frame::new()
                .fill(color::PANEL)
                .inner_margin(Margin::symmetric(8, 5)),
        )
        .show_separator_line(false)
        .show(ui, |ui| toolbar(ui, q, cx));

    egui::Panel::bottom(Id::new(("q-status", q.id)))
        .exact_size(24.0)
        .frame(
            Frame::new()
                .fill(color::PANEL)
                .inner_margin(Margin::symmetric(8, 0)),
        )
        .show(ui, |ui| status_line(ui, q));

    let available = ui.available_height();
    egui::Panel::top(Id::new(("q-editor", q.id)))
        .resizable(true)
        .default_size((available * 0.42).max(120.0))
        .size_range(80.0..=(available - 90.0).max(90.0))
        .frame(Frame::new().fill(color::EDITOR))
        .show(ui, |ui| editor(ui, q, cx));

    egui::CentralPanel::no_frame().show(ui, |ui| results(ui, q, cx));
}

fn toolbar(ui: &mut Ui, q: &mut QueryTab, cx: &mut TabContext<'_>) {
    ui.horizontal(|ui| {
        let running = q.run.running.is_some();
        let run = Button::new(RichText::new(format!("{}  Run", icon::PLAY)).color(color::SUCCESS));
        if ui
            .add_enabled(!running, run)
            .on_hover_text("Run (Ctrl+Enter). Runs only the selection when text is selected.")
            .clicked()
        {
            request_run(q, cx);
        }
        // Until a session reports its capabilities, assume cancellation is possible.
        let can_cancel = match q.profile_id.and_then(|id| cx.connections.get(&id)) {
            Some(ConnState {
                status: ConnStatus::Connected { capabilities, .. },
                ..
            }) => capabilities.query_cancellation,
            _ => true,
        };
        let stop = Button::new(RichText::new(format!("{}  Stop", icon::STOP)).color(color::DANGER));
        if ui
            .add_enabled(running && can_cancel, stop)
            .on_hover_text("Cancel the running query")
            .on_disabled_hover_text("This connection does not support query cancellation")
            .clicked()
            && let Some(r) = &q.run.running
        {
            cx.services.send(Command::Cancel {
                request_id: r.request_id,
            });
        }
        ui.separator();

        let current = q.profile_id.and_then(|id| cx.profile(id));
        let selected = match current {
            Some(p) => format!("{}  {}", p.kind.badge(), p.name),
            None => format!("{}  Choose connection…", icon::PLUGS),
        };
        egui::ComboBox::from_id_salt(("conn-combo", q.id))
            .width(230.0)
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for p in cx.profiles {
                    let label = format!("{}  {}", p.kind.badge(), p.name);
                    ui.selectable_value(&mut q.profile_id, Some(p.id), label)
                        .on_hover_text(p.endpoint());
                }
            });
        if let Some(p) = current
            && p.kind.has_schemas()
        {
            database_combo(ui, q, cx);
        }
        if let Some(p) = current {
            widgets::env_pill(ui, p.environment);
        }
        ui.separator();
        let can_explain = match q.profile_id.and_then(|id| cx.connections.get(&id)) {
            Some(ConnState {
                status: ConnStatus::Connected { capabilities, .. },
                ..
            }) => capabilities.explain_plan,
            _ => true,
        };
        let busy = running || q.run.plan_request.is_some();
        if ui
            .add_enabled(
                !busy && can_explain,
                Button::new(format!("{}  Explain", icon::TREE_STRUCTURE)),
            )
            .on_hover_text(
                "Estimated execution plan of the statement at the caret (or the selection)",
            )
            .on_disabled_hover_text("Execution plans are not available for this connection")
            .clicked()
        {
            request_explain(q, cx, false);
        }
        ui.add_enabled_ui(!busy && can_explain, |ui| {
            ui.menu_button(icon::CARET_DOWN, |ui| {
                if ui
                    .button(format!("{}  Explain Analyze", icon::LIGHTNING))
                    .on_hover_text(
                        "Executes the statement to measure actual rows and time. \
                         Its changes are rolled back where the engine allows it.",
                    )
                    .clicked()
                {
                    request_explain(q, cx, true);
                    ui.close();
                }
            });
        });
        ui.add_enabled(false, Button::new(format!("{}  Format", icon::TEXT_INDENT)))
            .on_disabled_hover_text("SQL formatter — Milestone 3");
    });
}

/// Database selector for engines where a session is bound to one database.
fn database_combo(ui: &mut Ui, q: &mut QueryTab, cx: &TabContext<'_>) {
    let conn = q.profile_id.and_then(|id| cx.connections.get(&id));
    let default = conn
        .and_then(|c| c.default_database.clone())
        .unwrap_or_else(|| "default".to_owned());
    let databases: Vec<String> = conn
        .map(|c| {
            c.objects
                .iter()
                .filter(|o| o.kind == rdbs_core::ObjectKind::Database)
                .map(|o| o.name.clone())
                .collect()
        })
        .unwrap_or_default();
    let selected = q.database.clone().unwrap_or_else(|| default.clone());
    egui::ComboBox::from_id_salt(("db-combo", q.id))
        .width(150.0)
        .selected_text(format!("{}  {selected}", icon::DATABASE))
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(q.database.is_none(), format!("{default} (default)"))
                .clicked()
            {
                q.database = None;
            }
            for db in databases.iter().filter(|d| **d != default) {
                if ui
                    .selectable_label(q.database.as_deref() == Some(db.as_str()), db)
                    .clicked()
                {
                    q.database = Some(db.clone());
                }
            }
        })
        .response
        .on_hover_text("Database this tab runs in (a separate session is opened when needed)");
}

fn editor(ui: &mut Ui, q: &mut QueryTab, cx: &mut TabContext<'_>) {
    let font = theme::mono();
    let row_height = ui.ctx().fonts_mut(|f| f.row_height(&font));
    let editor_id = q.editor_id();
    if std::mem::take(&mut q.run.focus_editor) {
        ui.memory_mut(|m| m.request_focus(editor_id));
    }

    // Completion keys must be consumed before the text editor handles them.
    let has_focus = ui.memory(|m| m.has_focus(editor_id));
    if let Some(accepted) = q.run.autocomplete.handle_keys(ui, has_focus) {
        crate::autocomplete::apply(accepted, &mut q.sql, ui.ctx(), editor_id);
    }
    let kind = q.profile_id.and_then(|id| cx.profile(id)).map(|p| p.kind);
    let objects = q
        .profile_id
        .and_then(|id| cx.connections.get(&id))
        .map_or(&[][..], |c| c.objects.as_slice());

    let mut layouter = |ui: &Ui, buf: &dyn TextBuffer, _wrap_width: f32| {
        let job = widgets::highlight_sql(buf.as_str(), &font);
        ui.ctx().fonts_mut(|f| f.layout_job(job))
    };

    ScrollArea::both()
        .id_salt(("editor-scroll", q.id))
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let lines = q.sql.split('\n').count().max(1);
                let digits = lines.to_string().len().max(2) as f32;
                let gutter_width = 16.0 + digits * 8.0;
                let numbers = (1..=lines)
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                ui.allocate_ui_with_layout(
                    Vec2::new(gutter_width, lines as f32 * row_height),
                    Layout::top_down(Align::Max),
                    |ui| {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(numbers)
                                .font(font.clone())
                                .color(color::TEXT_FAINT),
                        );
                    },
                );
                ui.add_space(8.0);

                let min_height = ui.available_height();
                let output = TextEdit::multiline(&mut q.sql)
                    .id(editor_id)
                    .font(font.clone())
                    .code_editor()
                    .frame(Frame::NONE)
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .min_size(Vec2::new(0.0, min_height))
                    .layouter(&mut layouter)
                    .show(ui);

                q.run.cursor = output.cursor_range.map(|range| {
                    widgets::char_range_to_bytes(
                        &q.sql,
                        range.primary.index.0,
                        range.primary.index.0,
                    )
                    .start
                });
                q.run.selection = output.cursor_range.and_then(|range| {
                    let chars = range.as_sorted_char_range();
                    (chars.start.0 != chars.end.0)
                        .then(|| widgets::char_range_to_bytes(&q.sql, chars.start.0, chars.end.0))
                });
                if has_focus || q.run.autocomplete.is_open() {
                    q.run.autocomplete.update(
                        &output,
                        &q.sql,
                        objects,
                        kind.unwrap_or(rdbs_core::DatabaseKind::PostgreSql),
                    );
                }
            });
        });

    if let Some(accepted) = q.run.autocomplete.show(ui, Id::new(("autocomplete", q.id))) {
        crate::autocomplete::apply(accepted, &mut q.sql, ui.ctx(), editor_id);
    }
}

/// Text that "Run" would execute: the selection if non-blank, else the whole editor.
pub fn sql_to_run(q: &QueryTab) -> String {
    if let Some(r) = &q.run.selection
        && let Some(selected) = q.sql.get(r.clone())
        && !selected.trim().is_empty()
    {
        return selected.to_owned();
    }
    q.sql.clone()
}

/// Statement to explain: the selection if non-blank, else the statement around the caret,
/// else the first statement.
pub fn sql_to_explain(q: &QueryTab) -> String {
    if let Some(r) = &q.run.selection
        && let Some(selected) = q.sql.get(r.clone())
        && !selected.trim().is_empty()
    {
        return selected.trim().to_owned();
    }
    let ranges = sql::split_statements(&q.sql);
    let at_cursor = q.run.cursor.and_then(|c| {
        ranges
            .iter()
            .find(|r| r.start <= c && c <= r.end)
            .or_else(|| ranges.iter().rev().find(|r| r.end <= c))
    });
    at_cursor
        .or(ranges.first())
        .and_then(|r| q.sql.get(r.clone()))
        .map(|s| s.trim().trim_end_matches(';').trim().to_owned())
        .unwrap_or_default()
}

/// Requests the execution plan of [`sql_to_explain`].
pub fn request_explain(q: &mut QueryTab, cx: &mut TabContext<'_>, analyze: bool) {
    let Some(profile) = q.profile_id.and_then(|id| cx.profile(id)).cloned() else {
        cx.actions.push(UiAction::Status(StatusMessage::error(
            "Choose a connection for this query first",
        )));
        return;
    };
    if needs_password(&profile, cx.connections, cx.passwords) {
        cx.actions.push(UiAction::NeedPassword {
            profile_id: profile.id,
            then: AfterPassword::Connect,
        });
        return;
    }
    let statement = sql_to_explain(q);
    let kind = sql::classify(&statement);
    if kind == sql::StatementKind::Empty {
        cx.actions
            .push(UiAction::Status(StatusMessage::info("Nothing to explain")));
        return;
    }
    if analyze && profile.environment.requires_write_confirmation() && !kind.is_read_only() {
        cx.actions.push(UiAction::Status(StatusMessage::error(
            "Explain Analyze executes the statement; it is only allowed for read-only \
             statements on production connections",
        )));
        return;
    }
    let request_id = cx.services.next_request_id();
    cx.services.send(Command::Explain {
        request_id,
        profile: profile.clone(),
        database: q.database.clone(),
        sql: statement,
        analyze,
        password: cx.passwords.get(&profile.id).cloned(),
    });
    q.run.plan_request = Some((request_id, Instant::now(), analyze));
    q.run.view = crate::workspace::ResultView::Plan;
    q.run.plan_view.selected = None;
}

/// Validates and dispatches a query, asking for confirmation on production when needed.
pub fn request_run(q: &mut QueryTab, cx: &mut TabContext<'_>) {
    if q.run.running.is_some() || q.run.saving.is_some() {
        return;
    }
    if !q.run.edits.is_empty() {
        cx.actions
            .push(UiAction::Status(StatusMessage::error(format!(
                "Save or discard the {} pending row change(s) before running again",
                q.run.edits.row_count()
            ))));
        return;
    }
    let Some(profile) = q.profile_id.and_then(|id| cx.profile(id)).cloned() else {
        cx.actions.push(UiAction::Status(StatusMessage::error(
            "Choose a connection for this query first",
        )));
        return;
    };
    if needs_password(&profile, cx.connections, cx.passwords) {
        cx.actions.push(UiAction::NeedPassword {
            profile_id: profile.id,
            then: AfterPassword::RunTab(q.id),
        });
        return;
    }
    let sql_text = sql_to_run(q);
    let kind = sql::classify(&sql_text);
    if kind == sql::StatementKind::Empty {
        cx.actions
            .push(UiAction::Status(StatusMessage::info("Nothing to execute")));
        return;
    }
    if profile.environment.requires_write_confirmation() && !kind.is_read_only() {
        cx.actions.push(UiAction::ConfirmExecute {
            tab_id: q.id,
            profile_id: profile.id,
            sql: sql_text,
            kind,
        });
        return;
    }
    let password = cx.passwords.get(&profile.id).cloned();
    start_query(q, &profile, sql_text, cx.services, password);
}

/// Whether a password must be asked for before using `profile`: it does not save its
/// password, none was entered this session, and no session is open yet.
pub fn needs_password(
    profile: &ConnectionProfile,
    connections: &std::collections::HashMap<rdbs_core::ProfileId, ConnState>,
    passwords: &std::collections::HashMap<rdbs_core::ProfileId, String>,
) -> bool {
    profile.kind.is_networked()
        && !profile.save_password
        && !passwords.contains_key(&profile.id)
        && !connections
            .get(&profile.id)
            .is_some_and(ConnState::is_connected)
}

/// Sends the query to the worker and marks the tab as running.
pub fn start_query(
    q: &mut QueryTab,
    profile: &ConnectionProfile,
    sql_text: String,
    services: &ServiceHandle,
    password: Option<String>,
) {
    let request_id = services.next_request_id();
    let executed = sql_text.clone();
    services.send(Command::Execute {
        request_id,
        profile: profile.clone(),
        sql: sql_text,
        password,
        database: q.database.clone(),
    });
    q.run.running = Some(Running {
        request_id,
        started: Instant::now(),
        profile_name: profile.name.clone(),
        profile_id: profile.id,
        sql: executed,
        database: q.database.clone(),
    });
    q.run.show_messages = false;
    q.run.view = crate::workspace::ResultView::Results;
}

fn results(ui: &mut Ui, q: &mut QueryTab, cx: &mut TabContext<'_>) {
    use crate::workspace::ResultView;
    let has_plan = q.run.plan.is_some() || q.run.plan_request.is_some();
    if has_plan && q.run.running.is_none() {
        Frame::new()
            .fill(color::PANEL)
            .inner_margin(Margin::symmetric(8, 3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let view = &mut q.run.view;
                    ui.selectable_value(
                        view,
                        ResultView::Results,
                        format!("{}  Results", icon::TABLE),
                    );
                    ui.selectable_value(
                        view,
                        ResultView::Plan,
                        format!("{}  Plan", icon::TREE_STRUCTURE),
                    );
                });
            });
    }
    if q.run.view == ResultView::Plan && has_plan && q.run.running.is_none() {
        if let Some((request_id, started, analyze)) = q.run.plan_request {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.35);
                ui.add(egui::Spinner::new().size(22.0));
                ui.label(
                    RichText::new(format!(
                        "{} … {}",
                        if analyze {
                            "Running Explain Analyze"
                        } else {
                            "Fetching plan"
                        },
                        format_duration(started.elapsed())
                    ))
                    .color(color::TEXT_DIM),
                );
                let _ = request_id;
            });
            return;
        }
        match &q.run.plan {
            Some(Ok(plan)) => crate::plan::show(ui, plan, &mut q.run.plan_view),
            Some(Err(e)) => error_panel(ui, e),
            None => {}
        }
        return;
    }
    let kind = q.profile_id.and_then(|id| cx.profile(id)).map(|p| p.kind);
    let export_table = export_table_name(&q.title);
    let table = q.table.clone();
    let RunState {
        running,
        last,
        grid: grid_state,
        show_messages,
        active_result,
        edits,
        editing,
        saving,
        edit_error,
        ..
    } = &mut q.run;

    if let Some(r) = running {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.35);
                ui.add(egui::Spinner::new().size(22.0));
                ui.label(
                    RichText::new(format!(
                        "Executing on {} … {}",
                        r.profile_name,
                        format_duration(r.started.elapsed())
                    ))
                    .color(color::TEXT_DIM),
                );
                if ui.button(format!("{}  Cancel", icon::STOP)).clicked() {
                    cx.services.send(Command::Cancel {
                        request_id: r.request_id,
                    });
                }
            });
        });
        return;
    }

    let Some(last) = last else {
        placeholder(
            ui,
            icon::TABLE,
            "Run a query to see results here",
            "Ctrl+Enter runs the editor contents or the current selection",
        );
        return;
    };

    match &last.outcome {
        Err(e) if e.kind == DbErrorKind::Cancelled => {
            placeholder(
                ui,
                icon::PROHIBIT,
                "Query cancelled",
                "No rows were returned",
            );
        }
        Err(e) => error_panel(ui, e),
        Ok(outcome) => {
            let count = outcome.results.len();
            if *active_result >= count {
                *active_result = count.saturating_sub(1);
            }
            Frame::new()
                .fill(color::PANEL)
                .inner_margin(Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for i in 0..count {
                            let label = if count == 1 {
                                format!("{}  Result", icon::TABLE)
                            } else {
                                format!("{}  Result {}", icon::TABLE, i + 1)
                            };
                            let selected = !*show_messages && *active_result == i;
                            if ui.selectable_label(selected, label).clicked() {
                                *show_messages = false;
                                if *active_result != i {
                                    *active_result = i;
                                    *grid_state = grid::GridState::default();
                                }
                            }
                        }
                        let messages_label = if outcome.messages.is_empty() {
                            format!("{}  Messages", icon::CHAT_TEXT)
                        } else {
                            format!("{}  Messages ({})", icon::CHAT_TEXT, outcome.messages.len())
                        };
                        if ui
                            .selectable_label(*show_messages || count == 0, messages_label)
                            .clicked()
                        {
                            *show_messages = true;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let rs = outcome.results.get(*active_result);
                            ui.add_enabled_ui(rs.is_some(), |ui| {
                                ui.menu_button(format!("{}  Export", icon::EXPORT), |ui| {
                                    if let Some(rs) = rs {
                                        export_menu(ui, rs, kind, &export_table, cx.actions);
                                    }
                                });
                            });
                        });
                    });
                });
            match outcome.results.get(*active_result) {
                Some(rs) if !*show_messages => {
                    let edit_state = table
                        .as_ref()
                        .map(|t| editability(t, rs, outcome.results.len()));
                    if let Some(state) = &edit_state {
                        edit_bar(
                            ui,
                            q.id,
                            state,
                            rs,
                            table.as_ref(),
                            edits,
                            grid_state,
                            saving.is_some(),
                            edit_error,
                            cx.actions,
                        );
                    }
                    let mode = matches!(edit_state, Some(Editability::Editable))
                        .then_some(grid::EditMode { edits, editing });
                    grid::show(
                        ui,
                        Id::new(("grid", q.id, *active_result)),
                        rs,
                        grid_state,
                        mode,
                    );
                }
                _ => messages(ui, last),
            }
        }
    }
}

/// Whether the grid of a table tab can be edited.
enum Editability {
    Editable,
    ReadOnly(String),
}

fn editability(
    table: &rdbs_core::edit::TableTarget,
    rs: &rdbs_core::ResultSet,
    sets: usize,
) -> Editability {
    if table.primary_key.is_empty() {
        return Editability::ReadOnly("read-only: the table has no primary key".into());
    }
    if sets != 1 {
        return Editability::ReadOnly("read-only: the script returned several result sets".into());
    }
    let missing: Vec<&String> = table
        .primary_key
        .iter()
        .filter(|k| !rs.columns.iter().any(|c| c.name.eq_ignore_ascii_case(k)))
        .collect();
    if !missing.is_empty() {
        return Editability::ReadOnly(format!(
            "read-only: include primary key column(s) {} to edit",
            missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Editability::Editable
}

#[allow(clippy::too_many_arguments)]
fn edit_bar(
    ui: &mut Ui,
    tab_id: u64,
    state: &Editability,
    rs: &rdbs_core::ResultSet,
    table: Option<&rdbs_core::edit::TableTarget>,
    edits: &mut rdbs_core::edit::PendingEdits,
    grid_state: &mut grid::GridState,
    saving: bool,
    edit_error: &mut Option<String>,
    actions: &mut Vec<UiAction>,
) {
    Frame::new()
        .fill(color::EDITOR)
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                match state {
                    Editability::ReadOnly(reason) => {
                        ui.label(
                            RichText::new(format!("{}  {reason}", icon::LOCK))
                                .size(12.0)
                                .color(color::TEXT_FAINT),
                        );
                        return;
                    }
                    Editability::Editable => {}
                }
                ui.add_enabled_ui(!saving, |ui| {
                    if ui
                        .button(format!("{}  Row", icon::PLUS))
                        .on_hover_text("Add a new row")
                        .clicked()
                    {
                        edits.add_row(rs.columns.len());
                        grid_state.selected = Some((rs.rows.len() + edits.inserted.len() - 1, 0));
                    }
                    let selected_row = grid_state
                        .selected
                        .map(|(r, _)| r)
                        .filter(|r| *r < rs.rows.len());
                    if ui
                        .add_enabled(
                            selected_row.is_some(),
                            Button::new(format!("{}  Delete", icon::TRASH)),
                        )
                        .on_hover_text("Mark the selected row for deletion (toggle)")
                        .clicked()
                        && let Some(r) = selected_row
                    {
                        edits.toggle_delete(r);
                    }
                });
                if edits.is_empty() {
                    ui.label(
                        RichText::new("Double-click a cell to edit")
                            .size(12.0)
                            .color(color::TEXT_FAINT),
                    );
                    return;
                }
                ui.separator();
                let count = edits.row_count();
                ui.label(
                    RichText::new(format!("{count} pending"))
                        .size(12.0)
                        .color(color::WARNING),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_enabled_ui(!saving, |ui| {
                        let save = Button::new(
                            RichText::new(format!("{}  Save changes", icon::FLOPPY_DISK))
                                .color(egui::Color32::WHITE),
                        )
                        .fill(color::ACCENT);
                        if ui.add(save).clicked() {
                            *edit_error = None;
                            actions.push(UiAction::SaveEdits { tab_id });
                        }
                        if ui
                            .button(format!("{}  Discard", icon::ARROW_CLOCKWISE))
                            .clicked()
                        {
                            *edits = rdbs_core::edit::PendingEdits::default();
                            *edit_error = None;
                        }
                        if ui
                            .button(format!("{}  Copy SQL", icon::COPY))
                            .on_hover_text("Copy the statements that Save will run")
                            .clicked()
                            && let Some(t) = table
                        {
                            match rdbs_core::edit::build_changes(t, rs, edits) {
                                Ok(changes) => {
                                    let script = changes
                                        .iter()
                                        .map(|c| format!("{};", c.sql))
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    ui.ctx().copy_text(script);
                                }
                                Err(e) => *edit_error = Some(e.to_string()),
                            }
                        }
                    });
                    if saving {
                        ui.add(egui::Spinner::new().size(14.0));
                    }
                });
            });
            if let Some(err) = edit_error {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{}  {err}", icon::WARNING)).color(color::DANGER),
                    )
                    .wrap(),
                );
            }
        });
}

/// Table name used for SQL `INSERT` exports: the tab title when it is a plain identifier
/// (tabs opened from the explorer are titled after their table).
pub(crate) fn export_table_name(title: &str) -> String {
    let plain = !title.is_empty()
        && title.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !title.starts_with(|c: char| c.is_ascii_digit());
    if plain && !title.starts_with("Query ") {
        title.to_owned()
    } else {
        "exported_rows".to_owned()
    }
}

fn export_menu(
    ui: &mut Ui,
    rs: &rdbs_core::ResultSet,
    kind: Option<rdbs_core::DatabaseKind>,
    table: &str,
    actions: &mut Vec<UiAction>,
) {
    use rdbs_core::export::{ExportFormat, export};
    let kind = kind.unwrap_or(rdbs_core::DatabaseKind::PostgreSql);
    let rows = format_count(rs.rows.len() as u64);
    ui.label(
        RichText::new(format!("{rows} rows"))
            .small()
            .color(color::TEXT_FAINT),
    );
    for format in ExportFormat::ALL {
        if ui
            .button(format!("{}  Copy as {}", icon::COPY, format.label()))
            .clicked()
        {
            ui.ctx().copy_text(export(rs, format, kind, table));
            actions.push(UiAction::Status(StatusMessage::info(format!(
                "Copied {rows} rows as {}",
                format.label()
            ))));
        }
    }
    ui.separator();
    for format in ExportFormat::ALL {
        if ui
            .button(format!(
                "{}  Save as {}…",
                icon::FLOPPY_DISK,
                format.label()
            ))
            .clicked()
        {
            actions.push(UiAction::SaveExport {
                contents: export(rs, format, kind, table),
                file_name: format!("{table}.{}", format.extension()),
                format,
            });
        }
    }
}

fn messages(ui: &mut Ui, last: &LastRun) {
    let Ok(outcome) = &last.outcome else { return };
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        Frame::new().inner_margin(Margin::same(12)).show(ui, |ui| {
            let mono = theme::mono();
            let mut line = |text: String, c| {
                ui.label(RichText::new(text).font(mono.clone()).color(c));
            };
            line(format!("{}  Query OK", icon::CHECK_CIRCLE), color::SUCCESS);
            for (i, rs) in outcome.results.iter().enumerate() {
                let prefix = if outcome.results.len() > 1 {
                    format!("Result {}: ", i + 1)
                } else {
                    String::new()
                };
                line(
                    format!(
                        "{prefix}{} rows returned",
                        format_count(rs.rows.len() as u64)
                    ),
                    color::TEXT,
                );
            }
            if let Some(n) = outcome.rows_affected {
                line(format!("{} rows affected", format_count(n)), color::TEXT);
            }
            for m in &outcome.messages {
                line(m.clone(), color::TEXT_DIM);
            }
            line(
                format!(
                    "Time: {}  ·  Connection: {}",
                    format_duration(last.elapsed),
                    last.profile_name
                ),
                color::TEXT_FAINT,
            );
        });
    });
}

fn error_panel(ui: &mut Ui, e: &rdbs_core::DbError) {
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        Frame::new()
            .fill(color::DANGER.gamma_multiply(0.08))
            .stroke(Stroke::new(1.0, color::DANGER.gamma_multiply(0.4)))
            .corner_radius(6)
            .inner_margin(Margin::same(12))
            .outer_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{}  Query failed", icon::WARNING))
                            .strong()
                            .color(color::DANGER),
                    );
                    if let Some(code) = &e.code {
                        ui.label(
                            RichText::new(format!("code {code}"))
                                .font(theme::mono())
                                .color(color::TEXT_DIM),
                        );
                    }
                });
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(RichText::new(&e.message).font(theme::mono()))
                        .selectable(true),
                );
                if let Some(detail) = &e.detail {
                    ui.label(RichText::new(detail).color(color::TEXT_DIM));
                }
            });
    });
}

fn placeholder(ui: &mut Ui, glyph: &str, title: &str, hint: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.3).max(12.0));
        ui.label(RichText::new(glyph).size(30.0).color(color::TEXT_FAINT));
        ui.label(RichText::new(title).color(color::TEXT_DIM));
        ui.label(RichText::new(hint).small().color(color::TEXT_FAINT));
    });
}

fn status_line(ui: &mut Ui, q: &QueryTab) {
    ui.horizontal_centered(|ui| {
        let small = |t: String, c| RichText::new(t).size(12.0).color(c);
        if let Some(r) = &q.run.running {
            ui.add(egui::Spinner::new().size(12.0));
            ui.label(small(
                format!(
                    "Running on {}  ·  {}",
                    r.profile_name,
                    format_duration(r.started.elapsed())
                ),
                color::TEXT_DIM,
            ));
        } else if let Some(last) = &q.run.last {
            let (text, c) = match &last.outcome {
                Ok(o) => {
                    let shown = q.run.active_result_set();
                    let mut what = match (shown, o.rows_affected) {
                        (Some(rs), _) => format!(
                            "{} rows{}",
                            format_count(rs.rows.len() as u64),
                            if o.truncated { " (truncated)" } else { "" }
                        ),
                        (None, Some(n)) => format!("{} rows affected", format_count(n)),
                        (None, None) => "OK".to_owned(),
                    };
                    if o.results.len() > 1 {
                        what.push_str(&format!("  ·  {} result sets", o.results.len()));
                    }
                    (format!("{}  {what}", icon::CHECK_CIRCLE), color::SUCCESS)
                }
                Err(e) if e.kind == DbErrorKind::Cancelled => {
                    (format!("{}  Cancelled", icon::PROHIBIT), color::WARNING)
                }
                Err(_) => (format!("{}  Error", icon::X_CIRCLE), color::DANGER),
            };
            ui.label(small(text, c));
            ui.label(small(
                format!(
                    "·  {}  ·  {}",
                    format_duration(last.elapsed),
                    last.profile_name
                ),
                color::TEXT_DIM,
            ));
        } else {
            ui.label(small("Ready".to_owned(), color::TEXT_DIM));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(r) = &q.run.selection {
                ui.label(small(
                    format!(
                        "Selection: {} chars",
                        q.sql.get(r.clone()).map_or(0, |s| s.chars().count())
                    ),
                    color::TEXT_DIM,
                ));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_table_name_uses_plain_titles_only() {
        assert_eq!(export_table_name("customers"), "customers");
        assert_eq!(export_table_name("Query 3"), "exported_rows");
        assert_eq!(export_table_name("my table"), "exported_rows");
        assert_eq!(export_table_name("1abc"), "exported_rows");
    }

    #[test]
    fn explains_statement_at_caret() {
        let mut q = QueryTab::new(
            1,
            None,
            "select 1;
select 2 from t;
"
            .into(),
            None,
        );
        assert_eq!(sql_to_explain(&q), "select 1");
        q.run.cursor = Some(12);
        assert_eq!(sql_to_explain(&q), "select 2 from t");
        q.run.cursor = Some(q.sql.len());
        assert_eq!(sql_to_explain(&q), "select 2 from t");
        q.run.selection = Some(0..8);
        assert_eq!(sql_to_explain(&q), "select 1");
    }

    #[test]
    fn runs_selection_when_present() {
        let mut q = QueryTab::new(1, None, "select 1;\nselect 2;".into(), None);
        assert_eq!(sql_to_run(&q), q.sql);
        q.run.selection = Some(10..19);
        assert_eq!(sql_to_run(&q), "select 2;");
        q.run.selection = Some(9..10); // whitespace-only selection falls back to all
        assert_eq!(sql_to_run(&q), q.sql);
        q.run.selection = Some(100..200); // stale range is ignored
        assert_eq!(sql_to_run(&q), q.sql);
    }
}
