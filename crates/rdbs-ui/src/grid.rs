//! Virtualised result grid built on `egui_extras::TableBuilder`.
//!
//! Only visible rows are laid out each frame, so result sets with hundreds of thousands of
//! rows scroll smoothly. In edit mode (a table with a primary key) cells can be changed
//! inline, rows added or marked for deletion; changes stay pending until saved.

use egui::{Align, Color32, Event, Key, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use rdbs_core::edit::{EditValue, PendingEdits};
use rdbs_core::{CellValue, ResultSet};

use crate::theme::{self, color};
use crate::widgets::format_count;

/// Maximum number of characters rendered per cell (full value is still copyable).
const MAX_CELL_CHARS: usize = 120;

/// Grid interaction state.
#[derive(Debug, Default, Clone)]
pub struct GridState {
    /// Selected cell as (row, column); rows past the result are pending new rows.
    pub selected: Option<(usize, usize)>,
    /// Whether the grid owns keyboard shortcuts (last click was inside it).
    pub active: bool,
}

/// A row of the grid: one from the result, or a pending new row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowRef {
    /// Index into the result set.
    Existing(usize),
    /// Index into [`PendingEdits::inserted`].
    New(usize),
}

/// The cell being edited inline.
#[derive(Debug, Clone)]
pub struct EditingCell {
    /// Row.
    pub row: RowRef,
    /// Column.
    pub col: usize,
    /// Current text.
    pub text: String,
    /// Grab keyboard focus on the next frame.
    pub focus: bool,
}

/// Edit-mode state borrowed from the query tab.
pub struct EditMode<'a> {
    /// Pending changes.
    pub edits: &'a mut PendingEdits,
    /// Cell being edited, if any.
    pub editing: &'a mut Option<EditingCell>,
}

/// Mutations requested while drawing; applied after the table is laid out.
enum GridOp {
    StartEdit(RowRef, usize, String),
    Set(RowRef, usize, EditValue),
    Revert(usize, usize),
    ToggleDelete(usize),
    RemoveNew(usize),
    Commit,
    Cancel,
}

/// Text used when copying a cell: NULL becomes an empty string.
pub fn copy_text(value: &CellValue) -> String {
    match value {
        CellValue::Null => String::new(),
        other => other.to_string(),
    }
}

/// Tab-separated text for one row.
pub fn row_as_tsv(rs: &ResultSet, row: usize) -> String {
    rs.rows
        .get(row)
        .map(|cells| cells.iter().map(copy_text).collect::<Vec<_>>().join("\t"))
        .unwrap_or_default()
}

fn display_text(value: &CellValue) -> String {
    truncate_line(&value.to_string())
}

fn truncate_line(text: &str) -> String {
    let single_line = if text.contains(['\n', '\r']) {
        text.replace(['\n', '\r'], " ")
    } else {
        text.to_owned()
    };
    if single_line.chars().count() > MAX_CELL_CHARS {
        let truncated: String = single_line.chars().take(MAX_CELL_CHARS).collect();
        format!("{truncated}…")
    } else {
        single_line
    }
}

/// What a cell shows: the original value or a pending one.
enum Shown<'a> {
    Original(&'a CellValue),
    Pending(&'a EditValue),
}

fn rich(shown: &Shown<'_>) -> RichText {
    match shown {
        Shown::Original(CellValue::Null) | Shown::Pending(EditValue::Null) => {
            RichText::new("NULL").italics().color(color::TEXT_FAINT)
        }
        Shown::Pending(EditValue::Default) => RichText::new("(default)")
            .italics()
            .color(color::TEXT_FAINT),
        Shown::Pending(EditValue::Text(t)) => RichText::new(truncate_line(t)).color(color::TEXT),
        Shown::Original(value) => {
            let fg = match value {
                CellValue::Bool(_) => Color32::from_rgb(0xC0, 0x9B, 0xF7),
                v if v.is_numeric() => color::TEXT,
                _ => Color32::from_rgb(0xC8, 0xCD, 0xD6),
            };
            RichText::new(display_text(value)).color(fg)
        }
    }
}

/// Text placed in the inline editor when editing starts.
fn edit_text(shown: &Shown<'_>) -> String {
    match shown {
        Shown::Original(v) => copy_text(v),
        Shown::Pending(EditValue::Text(t)) => t.clone(),
        Shown::Pending(_) => String::new(),
    }
}

/// Shows `rs` as a grid. `id` must be unique per grid. Pass `edit` to allow editing.
pub fn show(
    ui: &mut Ui,
    id: egui::Id,
    rs: &ResultSet,
    state: &mut GridState,
    mut edit: Option<EditMode<'_>>,
) {
    let ctx = ui.ctx().clone();
    let row_height = 22.0;
    let existing = rs.rows.len();
    let added = edit.as_ref().map_or(0, |e| e.edits.inserted.len());
    let n_rows = existing + added;
    let gutter = 12.0 + 8.0 * format_count(n_rows as u64).len() as f32;
    let mono = theme::mono();
    let mut clicked_inside = false;
    let mut ops: Vec<GridOp> = Vec::new();
    let editable = edit.is_some();
    let (edits, mut editing) = match edit.as_mut() {
        Some(e) => (Some(&*e.edits), Some(&mut *e.editing)),
        None => (None, None),
    };

    let outer = egui::ScrollArea::horizontal()
        .id_salt(id.with("h"))
        .auto_shrink(false)
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt(id.with("table"))
                .striped(true)
                .resizable(true)
                .sense(Sense::click())
                .auto_shrink(false)
                .min_scrolled_height(0.0)
                .max_scroll_height(f32::INFINITY)
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(gutter))
                .columns(
                    Column::initial(150.0).at_least(48.0).clip(true),
                    rs.columns.len(),
                )
                .header(30.0, |mut header| {
                    header.col(|ui| {
                        ui.label(RichText::new("#").color(color::TEXT_FAINT));
                    });
                    for col in &rs.columns {
                        header.col(|ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.label(RichText::new(&col.name).strong().color(color::TEXT));
                                ui.label(
                                    RichText::new(&col.type_name)
                                        .size(10.5)
                                        .color(color::TEXT_FAINT),
                                );
                            });
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_height, n_rows, |mut row| {
                        let r = row.index();
                        let row_ref = if r < existing {
                            RowRef::Existing(r)
                        } else {
                            RowRef::New(r - existing)
                        };
                        let deleted = matches!(row_ref, RowRef::Existing(i)
                            if edits.is_some_and(|e| e.deleted.contains(&i)));
                        row.set_selected(state.selected.is_some_and(|(sr, _)| sr == r));
                        row.col(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let label = match row_ref {
                                    RowRef::Existing(i) => (i + 1).to_string(),
                                    RowRef::New(_) => "+".to_owned(),
                                };
                                ui.label(
                                    RichText::new(label)
                                        .font(mono.clone())
                                        .color(color::TEXT_FAINT),
                                );
                            });
                        });
                        for c in 0..rs.columns.len() {
                            let original = match row_ref {
                                RowRef::Existing(i) => rs.cell(i, c),
                                RowRef::New(_) => None,
                            };
                            let pending = match (row_ref, edits) {
                                (RowRef::Existing(i), Some(e)) => e.cell(i, c),
                                (RowRef::New(i), Some(e)) => {
                                    e.inserted.get(i).and_then(|v| v.get(c))
                                }
                                _ => None,
                            };
                            let shown = match (pending, original) {
                                (Some(p), _) => Shown::Pending(p),
                                (None, Some(o)) => Shown::Original(o),
                                (None, None) => Shown::Original(&CellValue::Null),
                            };
                            let binary = matches!(original, Some(CellValue::Bytes(_)));
                            let is_selected = state.selected == Some((r, c));
                            let is_editing = editing
                                .as_deref()
                                .and_then(Option::as_ref)
                                .is_some_and(|e| e.row == row_ref && e.col == c);

                            let (_, response) = row.col(|ui| {
                                let tint = if deleted {
                                    Some(color::DANGER.gamma_multiply(0.16))
                                } else if matches!(row_ref, RowRef::New(_)) {
                                    Some(color::SUCCESS.gamma_multiply(0.12))
                                } else if pending.is_some() {
                                    Some(color::WARNING.gamma_multiply(0.20))
                                } else {
                                    None
                                };
                                if let Some(tint) = tint {
                                    ui.painter().rect_filled(ui.max_rect(), 0, tint);
                                }
                                if is_selected {
                                    ui.painter().rect_stroke(
                                        ui.max_rect().shrink(1.0),
                                        2,
                                        egui::Stroke::new(1.5, color::ACCENT),
                                        egui::StrokeKind::Inside,
                                    );
                                }
                                if is_editing
                                    && let Some(cell) =
                                        editing.as_deref_mut().and_then(Option::as_mut)
                                {
                                    let field = ui.add(
                                        egui::TextEdit::singleline(&mut cell.text)
                                            .font(mono.clone())
                                            .desired_width(f32::INFINITY)
                                            .margin(egui::Margin::symmetric(2, 0)),
                                    );
                                    if std::mem::take(&mut cell.focus) {
                                        field.request_focus();
                                    }
                                    if field.lost_focus() {
                                        if ui.input(|i| i.key_pressed(Key::Escape)) {
                                            ops.push(GridOp::Cancel);
                                        } else {
                                            ops.push(GridOp::Commit);
                                        }
                                    }
                                    return;
                                }
                                let mut text = rich(&shown).font(mono.clone());
                                if deleted {
                                    text = text.strikethrough().color(color::TEXT_FAINT);
                                }
                                let label = egui::Label::new(text).selectable(false).truncate();
                                let right = matches!(shown, Shown::Original(v) if v.is_numeric());
                                if right {
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.add(label)
                                    });
                                } else {
                                    ui.add(label);
                                }
                            });
                            if response.clicked() || response.secondary_clicked() {
                                state.selected = Some((r, c));
                                clicked_inside = true;
                            }
                            let can_edit = editable && !deleted && !binary;
                            if can_edit && response.double_clicked() {
                                ops.push(GridOp::StartEdit(row_ref, c, edit_text(&shown)));
                            }
                            response.context_menu(|ui| {
                                if editable {
                                    edit_menu(
                                        ui,
                                        row_ref,
                                        c,
                                        &shown,
                                        pending.is_some(),
                                        can_edit,
                                        deleted,
                                        &mut ops,
                                    );
                                    ui.separator();
                                }
                                if let RowRef::Existing(i) = row_ref {
                                    cell_menu(ui, &ctx, rs, i, c);
                                }
                            });
                        }
                    });
                });
        });

    // Apply mutations collected while drawing.
    if let Some(mode) = edit {
        for op in ops {
            apply_op(op, rs, mode.edits, mode.editing);
        }
    }

    // The grid owns Ctrl+C after a click inside it, until the user clicks elsewhere.
    let grid_rect = outer.inner_rect;
    if clicked_inside {
        state.active = true;
    } else if ui.input(|i| {
        i.pointer.primary_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| !grid_rect.contains(p))
    }) {
        state.active = false;
    }
    let typing = ctx.memory(|m| m.focused().is_some());
    if state.active
        && !typing
        && let Some((r, c)) = state.selected
        && ui.input(|i| i.events.iter().any(|e| matches!(e, Event::Copy)))
        && let Some(value) = rs.cell(r, c)
    {
        ctx.copy_text(copy_text(value));
    }
}

fn apply_op(
    op: GridOp,
    rs: &ResultSet,
    edits: &mut PendingEdits,
    editing: &mut Option<EditingCell>,
) {
    let set = |edits: &mut PendingEdits, row: RowRef, col: usize, value: EditValue| match row {
        RowRef::Existing(i) => {
            if let Some(original) = rs.cell(i, col) {
                edits.set_cell(i, col, value, original);
            }
        }
        RowRef::New(i) => {
            if let Some(cell) = edits.inserted.get_mut(i).and_then(|r| r.get_mut(col)) {
                *cell = value;
            }
        }
    };
    match op {
        GridOp::StartEdit(row, col, text) => {
            *editing = Some(EditingCell {
                row,
                col,
                text,
                focus: true,
            });
        }
        GridOp::Commit => {
            if let Some(cell) = editing.take() {
                set(edits, cell.row, cell.col, EditValue::Text(cell.text));
            }
        }
        GridOp::Cancel => *editing = None,
        GridOp::Set(row, col, value) => set(edits, row, col, value),
        GridOp::Revert(row, col) => {
            if let Some(cols) = edits.updates.get_mut(&row) {
                cols.remove(&col);
                if cols.is_empty() {
                    edits.updates.remove(&row);
                }
            }
        }
        GridOp::ToggleDelete(row) => edits.toggle_delete(row),
        GridOp::RemoveNew(i) => {
            if i < edits.inserted.len() {
                edits.inserted.remove(i);
            }
            *editing = None;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn edit_menu(
    ui: &mut Ui,
    row: RowRef,
    col: usize,
    shown: &Shown<'_>,
    has_pending: bool,
    can_edit: bool,
    deleted: bool,
    ops: &mut Vec<GridOp>,
) {
    if ui
        .add_enabled(
            can_edit,
            egui::Button::new(format!("{}  Edit cell", icon::PENCIL_SIMPLE)),
        )
        .clicked()
    {
        ops.push(GridOp::StartEdit(row, col, edit_text(shown)));
    }
    if ui
        .add_enabled(
            can_edit,
            egui::Button::new(format!("{}  Set NULL", icon::PROHIBIT)),
        )
        .clicked()
    {
        ops.push(GridOp::Set(row, col, EditValue::Null));
    }
    match row {
        RowRef::New(i) => {
            if ui
                .button(format!("{}  Use column default", icon::ARROW_CLOCKWISE))
                .clicked()
            {
                ops.push(GridOp::Set(row, col, EditValue::Default));
            }
            if ui
                .button(format!("{}  Remove new row", icon::TRASH))
                .clicked()
            {
                ops.push(GridOp::RemoveNew(i));
            }
        }
        RowRef::Existing(i) => {
            if ui
                .add_enabled(
                    has_pending,
                    egui::Button::new(format!("{}  Revert cell", icon::ARROW_CLOCKWISE)),
                )
                .clicked()
            {
                ops.push(GridOp::Revert(i, col));
            }
            let label = if deleted {
                format!("{}  Restore row", icon::ARROW_CLOCKWISE)
            } else {
                format!("{}  Delete row", icon::TRASH)
            };
            if ui.button(label).clicked() {
                ops.push(GridOp::ToggleDelete(i));
            }
        }
    }
}

fn cell_menu(ui: &mut Ui, ctx: &egui::Context, rs: &ResultSet, r: usize, c: usize) {
    if ui.button(format!("{}  Copy value", icon::COPY)).clicked()
        && let Some(v) = rs.cell(r, c)
    {
        ctx.copy_text(copy_text(v));
    }
    if ui
        .button(format!("{}  Copy row (TSV)", icon::ROWS))
        .clicked()
    {
        ctx.copy_text(row_as_tsv(rs, r));
    }
    if ui
        .button(format!("{}  Copy column name", icon::COLUMNS))
        .clicked()
        && let Some(col) = rs.columns.get(c)
    {
        ctx.copy_text(col.name.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdbs_core::ColumnMeta;

    fn rs() -> ResultSet {
        ResultSet {
            columns: vec![ColumnMeta::new("a", "int"), ColumnMeta::new("b", "text")],
            rows: vec![vec![CellValue::Int(7), CellValue::Null]],
        }
    }

    #[test]
    fn display_truncates_and_flattens() {
        let v = CellValue::Text(format!("a\nb{}", "x".repeat(300)));
        let shown = display_text(&v);
        assert!(!shown.contains('\n'));
        assert_eq!(shown.chars().count(), MAX_CELL_CHARS + 1);
    }

    #[test]
    fn tsv_copies_nulls_as_empty() {
        assert_eq!(row_as_tsv(&rs(), 0), "7\t");
        assert_eq!(row_as_tsv(&rs(), 5), "");
    }

    #[test]
    fn inline_edit_commit_and_cancel() {
        let data = rs();
        let mut edits = PendingEdits::default();
        let mut editing = None;
        apply_op(
            GridOp::StartEdit(RowRef::Existing(0), 1, String::new()),
            &data,
            &mut edits,
            &mut editing,
        );
        if let Some(cell) = editing.as_mut() {
            cell.text = "hello".into();
        }
        apply_op(GridOp::Commit, &data, &mut edits, &mut editing);
        assert!(editing.is_none());
        assert_eq!(edits.cell(0, 1), Some(&EditValue::Text("hello".into())));

        apply_op(
            GridOp::StartEdit(RowRef::Existing(0), 0, "7".into()),
            &data,
            &mut edits,
            &mut editing,
        );
        apply_op(GridOp::Cancel, &data, &mut edits, &mut editing);
        assert_eq!(edits.cell(0, 0), None);

        apply_op(GridOp::Revert(0, 1), &data, &mut edits, &mut editing);
        assert!(edits.is_empty());
    }

    #[test]
    fn new_rows_take_values_and_can_be_removed() {
        let data = rs();
        let mut edits = PendingEdits::default();
        let mut editing = None;
        edits.add_row(2);
        apply_op(
            GridOp::Set(RowRef::New(0), 0, EditValue::Text("9".into())),
            &data,
            &mut edits,
            &mut editing,
        );
        assert_eq!(edits.inserted[0][0], EditValue::Text("9".into()));
        assert_eq!(edits.inserted[0][1], EditValue::Default);
        apply_op(GridOp::RemoveNew(0), &data, &mut edits, &mut editing);
        assert!(edits.inserted.is_empty());
    }
}
