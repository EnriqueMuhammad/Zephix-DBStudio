//! Results pane: grids, row editing, export, messages, errors and the status line.

use super::*;

pub(super) fn results(ui: &mut Ui, q: &mut QueryTab, cx: &mut TabContext<'_>) {
    use crate::workspace::ResultView;
    let has_plan = q.run.plan.is_some() || q.run.plan_request.is_some();
    if has_plan && q.run.running.is_none() {
        Frame::new()
            .fill(color::panel())
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
                    .color(color::text_dim()),
                );
                let _ = request_id;
            });
            return;
        }
        match &q.run.plan {
            Some(Ok(plan)) => crate::plan::show(ui, plan, &mut q.run.plan_view),
            Some(Err(e)) => error_panel(ui, e, reconnect_target(q), cx.actions),
            None => {}
        }
        return;
    }
    let kind = q.profile_id.and_then(|id| cx.profile(id)).map(|p| p.kind);
    let data_tab = q.data_view.is_some();
    let export_table = export_table_name(&q.title);
    let table = q.table.clone();
    let reconnect = reconnect_target(q);
    let RunState {
        running,
        last,
        grid: grid_state,
        stacked_grids,
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
                    RichText::new(in_flight_banner(
                        r.phase,
                        &r.profile_name,
                        r.started.elapsed(),
                    ))
                    .color(color::text_dim()),
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
        Err(e) => error_panel(ui, e, reconnect, cx.actions),
        Ok(outcome) => {
            let count = outcome.results.len();
            if *active_result >= count {
                *active_result = count.saturating_sub(1);
            }
            // A data tab shows one result set: its row buttons, Messages and Export share one
            // bar under the toolbar.
            if data_tab && count <= 1 {
                let rs = outcome.results.first();
                let edit_state = match (&table, rs) {
                    (Some(t), Some(rs)) if !*show_messages => Some(editability(t, rs, count)),
                    _ => None,
                };
                Frame::new()
                    .fill(color::panel())
                    .inner_margin(Margin::symmetric(8, 3))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add_enabled_ui(rs.is_some(), |ui| {
                                    ui.menu_button(format!("{}  Export", icon::EXPORT), |ui| {
                                        if let Some(rs) = rs {
                                            export_menu(ui, rs, kind, &export_table, cx.actions);
                                        }
                                    });
                                });
                                let messages_label = if outcome.messages.is_empty() {
                                    format!("{}  Messages", icon::CHAT_TEXT)
                                } else {
                                    format!(
                                        "{}  Messages ({})",
                                        icon::CHAT_TEXT,
                                        outcome.messages.len()
                                    )
                                };
                                if ui
                                    .selectable_label(
                                        *show_messages || rs.is_none(),
                                        messages_label,
                                    )
                                    .on_hover_text("Server messages (click again for the rows)")
                                    .clicked()
                                {
                                    *show_messages = !*show_messages;
                                }
                                let (Some(state), Some(rs)) = (&edit_state, rs) else {
                                    return;
                                };
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    if edit_actions(
                                        ui,
                                        state,
                                        rs,
                                        edits,
                                        grid_state,
                                        saving.is_some(),
                                    ) {
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                ui.add_space(8.0);
                                                save_actions(
                                                    ui,
                                                    q.id,
                                                    rs,
                                                    table.as_ref(),
                                                    edits,
                                                    saving.is_some(),
                                                    edit_error,
                                                    cx.actions,
                                                );
                                            },
                                        );
                                    }
                                });
                            });
                        });
                        edit_error_label(ui, edit_error);
                    });
                match rs {
                    Some(rs) if !*show_messages => {
                        let mode = matches!(edit_state, Some(Editability::Editable(_)))
                            .then_some(grid::EditMode { edits, editing });
                        grid::show(ui, Id::new(("grid", q.id, 0)), rs, grid_state, mode);
                    }
                    _ => messages(ui, last),
                }
                return;
            }
            // Several result sets: stacked one above the other (remembered across runs) or as
            // tabs.
            let stacked_id = Id::new("results-stacked");
            let mut stacked = ui
                .ctx()
                .data_mut(|d| *d.get_persisted_mut_or_insert_with(stacked_id, || true));
            let stacked_view = stacked && count > 1;
            Frame::new()
                .fill(color::panel())
                .inner_margin(Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if stacked_view {
                            let label = format!("{}  Results ({count})", icon::TABLE);
                            if ui.selectable_label(!*show_messages, label).clicked() {
                                *show_messages = false;
                            }
                        }
                        for i in (0..count).filter(|_| !stacked_view) {
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
                                    if stacked_view {
                                        for (i, rs) in outcome.results.iter().enumerate() {
                                            ui.menu_button(format!("Result {}", i + 1), |ui| {
                                                export_menu(
                                                    ui,
                                                    rs,
                                                    kind,
                                                    &export_table,
                                                    cx.actions,
                                                );
                                            });
                                        }
                                    } else if let Some(rs) = rs {
                                        export_menu(ui, rs, kind, &export_table, cx.actions);
                                    }
                                });
                            });
                            if count > 1
                                && ui
                                    .selectable_label(stacked, format!("{}  Stacked", icon::ROWS))
                                    .on_hover_text(
                                        "Show all result sets one above the other instead of as tabs",
                                    )
                                    .clicked()
                            {
                                stacked = !stacked;
                                ui.ctx().data_mut(|d| d.insert_persisted(stacked_id, stacked));
                            }
                        });
                    });
                });
            if stacked_view && !*show_messages {
                stacked_results(ui, q.id, &outcome.results, stacked_grids);
                return;
            }
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
                    let mode = matches!(edit_state, Some(Editability::Editable(_)))
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

/// All result sets one above the other, each grid with its own scrolling and as tall as its
/// rows need (up to an equal share of the space), separated by a divider.
pub(super) fn stacked_results(
    ui: &mut Ui,
    tab_id: u64,
    results: &[rdbs_core::ResultSet],
    states: &mut Vec<grid::GridState>,
) {
    const DIVIDER: f32 = 4.0;
    states.resize_with(results.len(), grid::GridState::default);
    #[allow(clippy::cast_precision_loss)]
    let share = (ui.available_height() / results.len() as f32 - DIVIDER).max(120.0);
    ScrollArea::vertical()
        .id_salt(("stacked-results", tab_id))
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, (rs, state)) in results.iter().zip(states.iter_mut()).enumerate() {
                if i > 0 {
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), DIVIDER),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(rect, 0, color::surface_active());
                }
                let height = grid::fitted_height(rs).clamp(grid::empty_height(), share);
                let width = ui.available_width();
                ui.allocate_ui(Vec2::new(width, height), |ui| {
                    ui.set_min_height(height);
                    grid::show(ui, Id::new(("grid-stacked", tab_id, i)), rs, state, None);
                });
            }
        });
}

/// Whether the grid of a table tab can be edited.
pub(super) enum Editability {
    Editable(rdbs_core::edit::RowMatch),
    ReadOnly(String),
}

pub(super) fn editability(
    table: &rdbs_core::edit::TableTarget,
    rs: &rdbs_core::ResultSet,
    sets: usize,
) -> Editability {
    use rdbs_core::edit::EditError;
    if sets != 1 {
        return Editability::ReadOnly("read-only: the script returned several result sets".into());
    }
    match rdbs_core::edit::row_match(table, rs) {
        Ok(m) => Editability::Editable(m),
        Err(e) => Editability::ReadOnly(match e {
            EditError::NoPrimaryKey => "read-only: the table has no primary key".into(),
            EditError::MissingKeyColumn(k) => {
                format!("read-only: include primary key column {k} to edit")
            }
            EditError::MissingColumn(c) => {
                format!("read-only: no primary key; include all columns ({c} is missing) to edit")
            }
            EditError::NoComparableColumn => {
                "read-only: no primary key and no column that can be compared".into()
            }
            EditError::UnusableKey(_) => format!("read-only: {e}"),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn edit_bar(
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
        .fill(color::editor())
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if edit_actions(ui, state, rs, edits, grid_state, saving) {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        save_actions(ui, tab_id, rs, table, edits, saving, edit_error, actions);
                    });
                }
            });
            edit_error_label(ui, edit_error);
        });
}

/// Left part of the edit controls: why the grid is read-only, or the row buttons followed by
/// a hint or the number of pending changes. Returns whether there are changes to save.
pub(super) fn edit_actions(
    ui: &mut Ui,
    state: &Editability,
    rs: &rdbs_core::ResultSet,
    edits: &mut rdbs_core::edit::PendingEdits,
    grid_state: &mut grid::GridState,
    saving: bool,
) -> bool {
    let row_match = match state {
        Editability::Editable(m) => *m,
        Editability::ReadOnly(reason) => {
            ui.label(
                RichText::new(format!("{}  {reason}", icon::LOCK))
                    .size(12.0)
                    .color(color::text_faint()),
            );
            return false;
        }
    };
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
    if row_match == rdbs_core::edit::RowMatch::AllColumns {
        // Icon only while changes are pending, to leave room for the count and save buttons.
        let text = if edits.is_empty() {
            format!("{}  no primary key", icon::WARNING)
        } else {
            icon::WARNING.to_owned()
        };
        ui.label(RichText::new(text).size(12.0).color(color::warning()))
            .on_hover_text(
                "Rows are found by the values of all their columns. A change that would touch \
             more than one row is rolled back; of identical rows shown in the grid, one is \
             changed.",
            );
    }
    if edits.is_empty() {
        ui.label(
            RichText::new("Double-click a cell to edit")
                .size(12.0)
                .color(color::text_faint()),
        );
        return false;
    }
    ui.separator();
    ui.label(
        RichText::new(format!("{} pending", edits.row_count()))
            .size(12.0)
            .color(color::warning()),
    );
    true
}

/// Save, Discard and Copy SQL, for a right-to-left layout.
#[allow(clippy::too_many_arguments)]
pub(super) fn save_actions(
    ui: &mut Ui,
    tab_id: u64,
    rs: &rdbs_core::ResultSet,
    table: Option<&rdbs_core::edit::TableTarget>,
    edits: &mut rdbs_core::edit::PendingEdits,
    saving: bool,
    edit_error: &mut Option<String>,
    actions: &mut Vec<UiAction>,
) {
    ui.add_enabled_ui(!saving, |ui| {
        let save = Button::new(
            RichText::new(format!("{}  Save changes", icon::FLOPPY_DISK))
                .color(egui::Color32::WHITE),
        )
        .fill(color::accent());
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
                        .join(
                            "
",
                        );
                    ui.ctx().copy_text(script);
                }
                Err(e) => *edit_error = Some(e.to_string()),
            }
        }
    });
    if saving {
        ui.add(egui::Spinner::new().size(14.0));
    }
}

pub(super) fn edit_error_label(ui: &mut Ui, edit_error: &Option<String>) {
    if let Some(err) = edit_error {
        ui.add(
            egui::Label::new(
                RichText::new(format!("{}  {err}", icon::WARNING)).color(color::danger()),
            )
            .wrap(),
        );
    }
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

pub(super) fn export_menu(
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
            .color(color::text_faint()),
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
    if ui
        .button(format!("{}  Save as Excel…", icon::FILE_XLS))
        .on_hover_text("An .xlsx workbook with numbers and dates as Excel values")
        .clicked()
    {
        actions.push(UiAction::SaveExcel {
            result: rs.clone(),
            file_name: format!("{table}.xlsx"),
            sheet: table.to_owned(),
        });
    }
}

/// Messages view: one line per statement in execution order ("(3 rows affected)"), server
/// messages, then when and how long the script ran.
pub(super) fn messages(ui: &mut Ui, last: &LastRun) {
    let Ok(outcome) = &last.outcome else { return };
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        Frame::new().inner_margin(Margin::same(12)).show(ui, |ui| {
            let mono = theme::mono();
            let mut line = |text: String, c| {
                ui.add(
                    egui::Label::new(RichText::new(text).font(mono.clone()).color(c))
                        .selectable(true),
                );
            };
            if outcome.messages.is_empty() {
                line("Commands completed successfully.".to_owned(), color::text());
            }
            for m in &outcome.messages {
                let c = if m.starts_with('(') {
                    color::text()
                } else {
                    color::text_dim()
                };
                line(m.clone(), c);
            }
            line(String::new(), color::text());
            line(
                format!(
                    "Completion time: {}  ·  {}  ·  {}",
                    last.finished.format("%Y-%m-%d %H:%M:%S"),
                    format_duration(last.elapsed),
                    last.profile_name
                ),
                color::text_faint(),
            );
        });
    });
}

/// Profile, database override and tab id, when the failure can offer Reconnect.
pub(super) fn reconnect_target(
    q: &QueryTab,
) -> Option<(rdbs_core::ProfileId, Option<String>, u64)> {
    q.profile_id
        .map(|profile_id| (profile_id, q.database.clone(), q.id))
}

pub(super) fn error_panel(
    ui: &mut Ui,
    e: &rdbs_core::DbError,
    reconnect: Option<(rdbs_core::ProfileId, Option<String>, u64)>,
    actions: &mut Vec<UiAction>,
) {
    let lost = e.is_connection_lost();
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        Frame::new()
            .fill(color::danger().gamma_multiply(0.08))
            .stroke(Stroke::new(1.0, color::danger().gamma_multiply(0.4)))
            .corner_radius(6)
            .inner_margin(Margin::same(12))
            .outer_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let title = if lost {
                        "Connection lost"
                    } else {
                        "Query failed"
                    };
                    ui.label(
                        RichText::new(format!("{}  {title}", icon::WARNING))
                            .strong()
                            .color(color::danger()),
                    );
                    if let Some(code) = &e.code {
                        ui.label(
                            RichText::new(format!("code {code}"))
                                .font(theme::mono())
                                .color(color::text_dim()),
                        );
                    }
                });
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(RichText::new(&e.message).font(theme::mono()))
                        .selectable(true),
                );
                if let Some(detail) = &e.detail {
                    ui.label(RichText::new(detail).color(color::text_dim()));
                }
                if lost && let Some((profile_id, database, tab_id)) = reconnect {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Reconnect opens a new session for this tab. The text stays; \
                             temporary tables and open transactions are lost.",
                        )
                        .color(color::text_dim()),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .button(format!("{}  Reconnect", icon::ARROWS_CLOCKWISE))
                            .clicked()
                        {
                            actions.push(UiAction::Reconnect {
                                profile_id,
                                database: database.clone(),
                                retry_tab: None,
                            });
                        }
                        if ui
                            .button(format!("{}  Reconnect and retry", icon::PLAY))
                            .clicked()
                        {
                            actions.push(UiAction::Reconnect {
                                profile_id,
                                database,
                                retry_tab: Some(tab_id),
                            });
                        }
                    });
                }
            });
    });
}

pub(super) fn placeholder(ui: &mut Ui, glyph: &str, title: &str, hint: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.3).max(12.0));
        ui.label(RichText::new(glyph).size(30.0).color(color::text_faint()));
        ui.label(RichText::new(title).color(color::text_dim()));
        ui.label(RichText::new(hint).small().color(color::text_faint()));
    });
}

pub(super) fn status_line(ui: &mut Ui, q: &QueryTab, session_dropped: bool) {
    ui.horizontal_centered(|ui| {
        let small = |t: String, c| RichText::new(t).size(12.0).color(c);
        if session_dropped && q.run.running.is_none() {
            ui.label(small(
                format!("{}  Session dropped", icon::WARNING),
                color::warning(),
            ));
            ui.label(small("·".into(), color::text_faint()));
        }
        if let Some(r) = &q.run.running {
            ui.add(egui::Spinner::new().size(12.0));
            ui.label(small(
                in_flight_status(r.phase, &r.profile_name, r.started.elapsed()),
                color::text_dim(),
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
                    (format!("{}  {what}", icon::CHECK_CIRCLE), color::success())
                }
                Err(e) if e.kind == DbErrorKind::Cancelled => {
                    (format!("{}  Cancelled", icon::PROHIBIT), color::warning())
                }
                Err(_) => (format!("{}  Error", icon::X_CIRCLE), color::danger()),
            };
            ui.label(small(text, c));
            ui.label(small(
                format!(
                    "·  {}  ·  {}",
                    format_duration(last.elapsed),
                    last.profile_name
                ),
                color::text_dim(),
            ));
        } else {
            ui.label(small("Ready".to_owned(), color::text_dim()));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(r) = &q.run.selection {
                ui.label(small(
                    format!(
                        "Selection: {} chars",
                        q.sql.get(r.clone()).map_or(0, |s| s.chars().count())
                    ),
                    color::text_dim(),
                ));
            }
        });
    });
}
