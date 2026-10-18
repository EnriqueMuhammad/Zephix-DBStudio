//! Query history tab: searchable list of executed scripts.

use egui::{Align, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use rdbs_storage::{HistoryEntry, HistoryStatus};

use crate::model::UiAction;
use crate::theme::{self, color};
use crate::widgets::{format_count, format_duration};
use crate::workspace::TabContext;

/// One-line preview of a script for the list.
pub fn preview(sql: &str, max_chars: usize) -> String {
    let flat = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max_chars {
        let cut: String = flat.chars().take(max_chars).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

/// Renders the history tab. `filter` is the tab's search text.
pub fn show(ui: &mut Ui, filter: &mut String, cx: &mut TabContext<'_>) {
    egui::Frame::new()
        .fill(color::PANEL)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let search = ui.add(
                    egui::TextEdit::singleline(filter)
                        .hint_text(format!(
                            "{}  Search SQL or connection",
                            icon::MAGNIFYING_GLASS
                        ))
                        .desired_width(320.0),
                );
                if search.changed() {
                    cx.actions.push(UiAction::ReloadHistory(filter.clone()));
                }
                ui.label(
                    RichText::new(match cx.history.len() {
                        1 => "1 entry".to_owned(),
                        n => format!("{} entries", format_count(n as u64)),
                    })
                    .small()
                    .color(color::TEXT_FAINT),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            !cx.history.is_empty(),
                            egui::Button::new(format!("{}  Clear history", icon::TRASH)),
                        )
                        .clicked()
                    {
                        cx.actions.push(UiAction::ClearHistory);
                    }
                });
            });
        });

    if cx.history.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.label(
                RichText::new(icon::CLOCK)
                    .size(30.0)
                    .color(color::TEXT_FAINT),
            );
            ui.label(RichText::new("No queries yet").color(color::TEXT_DIM));
            ui.label(
                RichText::new("Executed scripts appear here; double-click one to reopen it")
                    .small()
                    .color(color::TEXT_FAINT),
            );
        });
        return;
    }

    let mono = theme::mono();
    let history = cx.history;
    TableBuilder::new(ui)
        .id_salt("history-table")
        .striped(true)
        .resizable(true)
        .sense(Sense::click())
        .auto_shrink(false)
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(158.0))
        .column(Column::initial(170.0).clip(true))
        .column(Column::exact(28.0))
        .column(Column::exact(80.0))
        .column(Column::exact(90.0))
        .column(Column::remainder().clip(true))
        .header(24.0, |mut header| {
            for title in ["Executed", "Connection", "", "Time", "Rows", "SQL"] {
                header.col(|ui| {
                    ui.label(RichText::new(title).strong());
                });
            }
        })
        .body(|body| {
            body.rows(24.0, history.len(), |mut row| {
                let Some(h) = history.get(row.index()) else {
                    return;
                };
                let e = &h.entry;
                row.col(|ui| {
                    ui.label(RichText::new(&h.executed_at).color(color::TEXT_DIM));
                });
                row.col(|ui| {
                    match &e.database {
                        Some(db) => ui.label(format!("{} · {db}", e.profile_name)),
                        None => ui.label(&e.profile_name),
                    };
                });
                row.col(|ui| {
                    let (glyph, c) = match e.status {
                        HistoryStatus::Ok => (icon::CHECK_CIRCLE, color::SUCCESS),
                        HistoryStatus::Error => (icon::X_CIRCLE, color::DANGER),
                        HistoryStatus::Cancelled => (icon::PROHIBIT, color::WARNING),
                    };
                    let label = ui.label(RichText::new(glyph).color(c));
                    if let Some(err) = &e.error {
                        label.on_hover_text(err);
                    }
                });
                row.col(|ui| {
                    ui.label(
                        RichText::new(format_duration(std::time::Duration::from_millis(
                            e.elapsed_ms,
                        )))
                        .color(color::TEXT_DIM),
                    );
                });
                row.col(|ui| {
                    ui.label(
                        RichText::new(e.rows.map(format_count).unwrap_or_default())
                            .color(color::TEXT_DIM),
                    );
                });
                row.col(|ui| {
                    ui.add(
                        egui::Label::new(RichText::new(preview(&e.sql, 300)).font(mono.clone()))
                            .truncate()
                            .selectable(false),
                    );
                });
                let response = row.response();
                if response.double_clicked() {
                    cx.actions.push(reopen(h, false));
                }
                response
                    .on_hover_text(preview(&e.sql, 2_000))
                    .context_menu(|ui| {
                        if ui
                            .button(format!("{}  Open in new tab", icon::FILE_SQL))
                            .clicked()
                        {
                            cx.actions.push(reopen(h, false));
                        }
                        if ui.button(format!("{}  Run again", icon::PLAY)).clicked() {
                            cx.actions.push(reopen(h, true));
                        }
                        if ui.button(format!("{}  Copy SQL", icon::COPY)).clicked() {
                            ui.ctx().copy_text(e.sql.clone());
                        }
                    });
            });
        });
}

fn reopen(h: &HistoryEntry, run: bool) -> UiAction {
    UiAction::OpenScript {
        profile_id: h.entry.profile_id,
        database: h.entry.database.clone(),
        sql: h.entry.sql.clone(),
        run,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_flattens_and_truncates() {
        assert_eq!(
            preview("select *\n  from t\twhere a = 1", 100),
            "select * from t where a = 1"
        );
        assert_eq!(preview("abcdef", 3), "abc…");
    }
}
