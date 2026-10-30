//! Virtualised result grid built on `egui_extras::TableBuilder`.
//!
//! Only visible rows are laid out each frame, so result sets with hundreds of thousands of
//! rows scroll smoothly. In edit mode (a table opened from the explorer) cells can be changed
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
/// Row and header heights.
const ROW_HEIGHT: f32 = 22.0;
const HEADER_HEIGHT: f32 = 24.0;
/// Space between a cell's border and its text.
const CELL_PAD: f32 = 6.0;
/// Rows measured to size the columns.
const MEASURED_ROWS: usize = 200;

/// Font of cell and header text.
fn cell_font() -> egui::FontId {
    egui::FontId::proportional(13.0)
}

/// Grid interaction state.
#[derive(Debug, Default, Clone)]
pub struct GridState {
    /// Selected cell as (row, column); rows past the result are pending new rows.
    pub selected: Option<(usize, usize)>,
    /// Whole rows selected with the row numbers (Ctrl toggles, Shift extends, "#" selects all).
    pub rows: std::collections::BTreeSet<usize>,
    /// Row a Shift-click extends from.
    pub anchor: Option<usize>,
    /// Whether the grid owns keyboard shortcuts (last click was inside it).
    pub active: bool,
    /// Column widths fitted to the content, for the result identified by the key.
    widths: Option<(u64, Vec<f32>)>,
}

/// Identity of a result's shape: columns and row count.
fn shape_key(rs: &ResultSet) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for c in &rs.columns {
        c.name.hash(&mut h);
        c.type_name.hash(&mut h);
    }
    rs.rows.len().hash(&mut h);
    h.finish()
}

/// Widths that fit each column's name and its first rows, like SQL Server's grid.
fn fit_widths(ctx: &egui::Context, rs: &ResultSet) -> Vec<f32> {
    let font = cell_font();
    ctx.fonts_mut(|f| {
        let mut measure = |text: &str| {
            f.layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
        };
        rs.columns
            .iter()
            .enumerate()
            .map(|(c, col)| {
                let mut width = measure(&col.name);
                for row in rs.rows.iter().take(MEASURED_ROWS) {
                    if let Some(v) = row.get(c) {
                        width = width.max(measure(&display_text(v)));
                    }
                }
                (width + 2.0 * CELL_PAD + 4.0).clamp(40.0, 420.0)
            })
            .collect()
    })
}

/// Height that shows every row of `rs` (plus room for a horizontal scroll bar).
#[allow(clippy::cast_precision_loss)]
pub fn fitted_height(rs: &ResultSet) -> f32 {
    HEADER_HEIGHT + ROW_HEIGHT * rs.rows.len() as f32 + 14.0
}

/// Height of a grid without rows: the header and a little space.
pub fn empty_height() -> f32 {
    HEADER_HEIGHT + ROW_HEIGHT + 14.0
}

/// Thin border at the right and bottom of a cell.
fn cell_lines(ui: &Ui) {
    let r = ui.max_rect();
    let stroke = egui::Stroke::new(1.0, color::border());
    ui.painter().hline(r.x_range(), r.bottom() - 0.5, stroke);
    ui.painter().vline(r.right() - 0.5, r.y_range(), stroke);
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

/// Tab-separated rows (in result order), optionally preceded by the column names; pastes as a
/// table into spreadsheets.
pub fn rows_as_tsv(rs: &ResultSet, rows: impl IntoIterator<Item = usize>, headers: bool) -> String {
    let mut lines = Vec::new();
    if headers {
        lines.push(
            rs.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join("\t"),
        );
    }
    lines.extend(
        rows.into_iter()
            .filter(|r| *r < rs.rows.len())
            .map(|r| row_as_tsv(rs, r)),
    );
    lines.join("\n")
}

/// Text Ctrl+C copies: the selected rows, else the selected cell (with its column name when
/// `headers`).
fn selection_text(rs: &ResultSet, state: &GridState, headers: bool) -> Option<String> {
    if !state.rows.is_empty() {
        return Some(rows_as_tsv(rs, state.rows.iter().copied(), headers));
    }
    let (r, c) = state.selected?;
    let value = copy_text(rs.cell(r, c)?);
    Some(match (headers, rs.columns.get(c)) {
        (true, Some(col)) => format!("{}\n{value}", col.name),
        _ => value,
    })
}

/// Updates the row selection for a click on row `r` with the given modifiers.
fn click_row(state: &mut GridState, r: usize, modifiers: egui::Modifiers) {
    state.selected = None;
    match (modifiers.shift, state.anchor) {
        (true, Some(anchor)) => {
            if !modifiers.command {
                state.rows.clear();
            }
            state.rows.extend(anchor.min(r)..=anchor.max(r));
        }
        _ if modifiers.command => {
            if !state.rows.remove(&r) {
                state.rows.insert(r);
            }
            state.anchor = Some(r);
        }
        _ => {
            state.rows.clear();
            state.rows.insert(r);
            state.anchor = Some(r);
        }
    }
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
            RichText::new("NULL").italics().color(color::text_faint())
        }
        Shown::Pending(EditValue::Default) => RichText::new("(default)")
            .italics()
            .color(color::text_faint()),
        Shown::Pending(EditValue::Text(t)) => RichText::new(truncate_line(t)).color(color::text()),
        Shown::Original(value) => {
            let fg = match value {
                CellValue::Bool(_) => crate::theme::palette().syntax[5],
                v if v.is_numeric() => color::text(),
                _ if crate::theme::is_light() => color::text(),
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
    let existing = rs.rows.len();
    let added = edit.as_ref().map_or(0, |e| e.edits.inserted.len());
    let n_rows = existing + added;
    #[allow(clippy::cast_precision_loss)]
    let gutter = (2.0 * CELL_PAD + 7.5 * format_count(n_rows as u64).len() as f32).max(30.0);
    let mono = theme::mono();
    let font = cell_font();
    let key = shape_key(rs);
    if state.widths.as_ref().is_none_or(|(k, _)| *k != key) {
        state.widths = Some((key, fit_widths(&ctx, rs)));
    }
    let widths = state
        .widths
        .as_ref()
        .map(|(_, w)| w.clone())
        .unwrap_or_default();
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
            // Cells touch, so their borders form one grid; text gets its own padding.
            ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
            let mut table = TableBuilder::new(ui)
                // A new shape of result gets fresh (fitted) column widths.
                .id_salt(id.with(("table", key)))
                .striped(false)
                .resizable(true)
                .sense(Sense::click())
                .auto_shrink(false)
                .min_scrolled_height(0.0)
                .max_scroll_height(f32::INFINITY)
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(gutter));
            for w in &widths {
                table = table.column(Column::initial(*w).at_least(32.0).clip(true));
            }
            table
                .header(HEADER_HEIGHT, |mut header| {
                    header.col(|ui| {
                        // The blank corner selects every row; right-click to copy them.
                        let rect = ui.max_rect();
                        let corner = ui.interact(rect, id.with("corner"), Sense::click());
                        let fill = if corner.hovered() {
                            color::surface_hover()
                        } else {
                            color::surface()
                        };
                        ui.painter().rect_filled(rect, 0, fill);
                        cell_lines(ui);
                        let corner = corner.on_hover_text("Select all rows");
                        if corner.clicked() || corner.secondary_clicked() {
                            state.selected = None;
                            state.rows = (0..existing).collect();
                            clicked_inside = true;
                        }
                        corner.context_menu(|ui| rows_menu(ui, &ctx, rs, state));
                    });
                    for col in &rs.columns {
                        header.col(|ui| {
                            ui.painter().rect_filled(ui.max_rect(), 0, color::surface());
                            cell_lines(ui);
                            ui.add_space(CELL_PAD);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&col.name)
                                        .font(font.clone())
                                        .color(color::text()),
                                )
                                .truncate(),
                            )
                            .on_hover_text(format!("{}\n{}", col.name, col.type_name));
                        });
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, n_rows, |mut row| {
                        let r = row.index();
                        let row_ref = if r < existing {
                            RowRef::Existing(r)
                        } else {
                            RowRef::New(r - existing)
                        };
                        let deleted = matches!(row_ref, RowRef::Existing(i)
                            if edits.is_some_and(|e| e.deleted.contains(&i)));
                        row.set_selected(
                            state.rows.contains(&r)
                                || state.selected.is_some_and(|(sr, _)| sr == r),
                        );
                        let (_, gutter_response) = row.col(|ui| {
                            ui.painter().rect_filled(
                                ui.max_rect(),
                                0,
                                color::surface().gamma_multiply(0.6),
                            );
                            cell_lines(ui);
                            let label = match row_ref {
                                RowRef::Existing(i) => (i + 1).to_string(),
                                RowRef::New(_) => "+".to_owned(),
                            };
                            ui.add_space(CELL_PAD);
                            ui.label(
                                RichText::new(label)
                                    .font(font.clone())
                                    .color(color::text_dim()),
                            );
                        });
                        if let RowRef::Existing(i) = row_ref {
                            if gutter_response.clicked() {
                                let modifiers = ui_modifiers(&ctx);
                                click_row(state, i, modifiers);
                                clicked_inside = true;
                            }
                            if gutter_response.secondary_clicked() {
                                if !state.rows.contains(&i) {
                                    click_row(state, i, egui::Modifiers::NONE);
                                }
                                clicked_inside = true;
                            }
                            gutter_response.context_menu(|ui| {
                                rows_menu(ui, &ctx, rs, state);
                            });
                        }
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
                                    Some(color::danger().gamma_multiply(0.16))
                                } else if matches!(row_ref, RowRef::New(_)) {
                                    Some(color::success().gamma_multiply(0.12))
                                } else if pending.is_some() {
                                    Some(color::warning().gamma_multiply(0.20))
                                } else {
                                    None
                                };
                                if let Some(tint) = tint {
                                    ui.painter().rect_filled(ui.max_rect(), 0, tint);
                                }
                                if is_selected {
                                    let rect = ui.max_rect();
                                    ui.painter().rect_filled(
                                        rect,
                                        0,
                                        color::accent().gamma_multiply(0.35),
                                    );
                                    ui.painter().rect_stroke(
                                        rect.shrink(0.5),
                                        0,
                                        egui::Stroke::new(1.0, color::accent()),
                                        egui::StrokeKind::Inside,
                                    );
                                }
                                cell_lines(ui);
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
                                let mut text = rich(&shown).font(font.clone());
                                if deleted {
                                    text = text.strikethrough().color(color::text_faint());
                                }
                                let label = egui::Label::new(text).selectable(false).truncate();
                                let right = matches!(shown, Shown::Original(v) if v.is_numeric());
                                if right {
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.add_space(CELL_PAD);
                                        ui.add(label)
                                    });
                                } else {
                                    ui.add_space(CELL_PAD);
                                    ui.add(label);
                                }
                            });
                            let in_selected_rows =
                                matches!(row_ref, RowRef::Existing(i) if state.rows.contains(&i));
                            if response.clicked()
                                || (response.secondary_clicked() && !in_selected_rows)
                            {
                                state.selected = Some((r, c));
                                state.rows.clear();
                                state.anchor = match row_ref {
                                    RowRef::Existing(i) => Some(i),
                                    RowRef::New(_) => None,
                                };
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
                                if in_selected_rows {
                                    rows_menu(ui, &ctx, rs, state);
                                } else if let RowRef::Existing(i) = row_ref {
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
    if state.active && !typing {
        // Ctrl+C copies the selection; with Shift (Ctrl+Shift+C) the column names come first.
        let copy = ui.input(|i| {
            i.events
                .iter()
                .any(|e| matches!(e, Event::Copy))
                .then_some(i.modifiers.shift)
        });
        if let Some(headers) = copy
            && let Some(text) = selection_text(rs, state, headers)
        {
            ctx.copy_text(text);
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::A)) {
            state.selected = None;
            state.rows = (0..existing).collect();
        }
    }
}

fn ui_modifiers(ctx: &egui::Context) -> egui::Modifiers {
    ctx.input(|i| i.modifiers)
}

/// Menu for selected rows.
fn rows_menu(ui: &mut Ui, ctx: &egui::Context, rs: &ResultSet, state: &GridState) {
    let n = state.rows.len();
    let what = if n == 1 {
        "row".to_owned()
    } else {
        format!("{} rows", format_count(n as u64))
    };
    if ui
        .button(format!("{}  Copy {what}", icon::COPY))
        .on_hover_text("Ctrl+C")
        .clicked()
    {
        ctx.copy_text(rows_as_tsv(rs, state.rows.iter().copied(), false));
    }
    if ui
        .button(format!("{}  Copy {what} with headers", icon::TABLE))
        .on_hover_text("Ctrl+Shift+C")
        .clicked()
    {
        ctx.copy_text(rows_as_tsv(rs, state.rows.iter().copied(), true));
    }
    ui.separator();
    if ui
        .button(format!("{}  Copy all rows with headers", icon::ROWS))
        .clicked()
    {
        ctx.copy_text(rows_as_tsv(rs, 0..rs.rows.len(), true));
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
    if ui
        .button(format!("{}  Copy value", icon::COPY))
        .on_hover_text("Ctrl+C")
        .clicked()
        && let Some(v) = rs.cell(r, c)
    {
        ctx.copy_text(copy_text(v));
    }
    if ui.button(format!("{}  Copy row", icon::ROWS)).clicked() {
        ctx.copy_text(row_as_tsv(rs, r));
    }
    if ui
        .button(format!("{}  Copy row with headers", icon::TABLE))
        .clicked()
    {
        ctx.copy_text(rows_as_tsv(rs, [r], true));
    }
    if ui
        .button(format!("{}  Copy all rows with headers", icon::ROWS))
        .clicked()
    {
        ctx.copy_text(rows_as_tsv(rs, 0..rs.rows.len(), true));
    }
    ui.separator();
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
    fn copies_rows_with_and_without_headers() {
        let mut rs = rs();
        rs.rows
            .push(vec![CellValue::Int(8), CellValue::Text("x".into())]);
        assert_eq!(rows_as_tsv(&rs, [1, 0], false), "8\tx\n7\t");
        assert_eq!(rows_as_tsv(&rs, [0], true), "a\tb\n7\t");
        assert_eq!(rows_as_tsv(&rs, [5], true), "a\tb");

        let mut state = GridState::default();
        click_row(&mut state, 0, egui::Modifiers::NONE);
        click_row(&mut state, 1, egui::Modifiers::SHIFT);
        assert_eq!(state.rows.iter().copied().collect::<Vec<_>>(), [0, 1]);
        assert_eq!(
            selection_text(&rs, &state, true).as_deref(),
            Some("a\tb\n7\t\n8\tx")
        );
        click_row(&mut state, 0, egui::Modifiers::COMMAND);
        assert_eq!(state.rows.iter().copied().collect::<Vec<_>>(), [1]);

        state.rows.clear();
        state.selected = Some((1, 1));
        assert_eq!(selection_text(&rs, &state, false).as_deref(), Some("x"));
        assert_eq!(selection_text(&rs, &state, true).as_deref(), Some("b\nx"));
    }

    #[test]
    fn widths_are_refitted_when_the_result_changes() {
        let a = rs();
        let mut b = rs();
        assert_eq!(shape_key(&a), shape_key(&b));
        b.rows.push(vec![CellValue::Int(1), CellValue::Null]);
        assert_ne!(shape_key(&a), shape_key(&b));
        b.columns[1].name = "renamed".into();
        assert_ne!(shape_key(&a), shape_key(&b));
        // One header plus one row, and room for the scroll bar.
        assert!((fitted_height(&a) - (HEADER_HEIGHT + ROW_HEIGHT + 14.0)).abs() < f32::EPSILON);
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
