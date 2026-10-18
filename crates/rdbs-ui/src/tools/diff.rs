//! Schema diff: compare the tables of two connections/databases/schemas (possibly different
//! engines) and generate or apply the script that makes the target match the source.

use std::collections::HashSet;

use egui::{Button, RichText, ScrollArea, TextEdit, Ui};
use egui_phosphor::regular as icon;
use rdbs_core::ddlgen::{create_table, script};
use rdbs_core::diff::{DiffStatus, TableDiff, compare, migration_script};
use rdbs_core::{DatabaseKind, Event, RequestId, TableSchema};
use serde::{Deserialize, Serialize};

use super::{Scope, error_box, error_text, header, picker};
use crate::model::UiAction;
use crate::theme::{self, color};
use crate::workspace::TabContext;

/// Schema diff state; the compared scopes are persisted.
#[derive(Serialize, Deserialize, Default)]
pub struct Diff {
    /// Reference side.
    pub source: Option<Scope>,
    /// Side to be changed.
    pub target: Option<Scope>,
    #[serde(skip)]
    rt: Runtime,
}

#[derive(Default)]
struct Runtime {
    source_req: Option<RequestId>,
    target_req: Option<RequestId>,
    source_tables: Option<Vec<TableSchema>>,
    target_tables: Option<Vec<TableSchema>>,
    kinds: Option<(DatabaseKind, DatabaseKind)>,
    diffs: Vec<TableDiff>,
    checked: HashSet<String>,
    focus: Option<String>,
    show_identical: bool,
    error: Option<String>,
    apply: Option<RequestId>,
    notice: Option<String>,
    confirm: bool,
}

fn status_icon(status: DiffStatus) -> (&'static str, egui::Color32) {
    match status {
        DiffStatus::Added => (icon::PLUS_CIRCLE, color::SUCCESS),
        DiffStatus::Removed => (icon::MINUS_CIRCLE, color::DANGER),
        DiffStatus::Changed => (icon::PENCIL_CIRCLE, color::WARNING),
        DiffStatus::Identical => (icon::CHECK_CIRCLE, color::TEXT_FAINT),
    }
}

impl Diff {
    /// Diff starting from `scope` as the source.
    pub fn new(source: Option<Scope>) -> Self {
        Self {
            source,
            ..Self::default()
        }
    }

    /// Tab title.
    pub fn title(&self) -> String {
        "Schema diff".to_owned()
    }

    /// Consumes this tool's events.
    pub fn on_event(&mut self, event: &Event) -> bool {
        match event {
            Event::SchemaDescribed { request_id, result }
                if self.rt.source_req == Some(*request_id)
                    || self.rt.target_req == Some(*request_id) =>
            {
                let is_source = self.rt.source_req == Some(*request_id);
                match result {
                    Ok(tables) => {
                        if is_source {
                            self.rt.source_tables = Some(tables.clone());
                        } else {
                            self.rt.target_tables = Some(tables.clone());
                        }
                    }
                    Err(e) => {
                        let side = if is_source { "source" } else { "target" };
                        self.rt.error =
                            Some(format!("Could not read the {side}: {}", error_text(e)));
                    }
                }
                if is_source {
                    self.rt.source_req = None;
                } else {
                    self.rt.target_req = None;
                }
                self.recompute();
                true
            }
            Event::ScriptFinished { request_id, result } if self.rt.apply == Some(*request_id) => {
                self.rt.apply = None;
                match result {
                    Ok(n) => {
                        self.rt.notice = Some(format!(
                            "Applied {n} statement(s) to the target. Compare again to verify."
                        ));
                    }
                    Err(e) => self.rt.error = Some(error_text(e)),
                }
                true
            }
            _ => false,
        }
    }

    fn recompute(&mut self) {
        let (Some(source), Some(target), Some((sk, tk))) = (
            &self.rt.source_tables,
            &self.rt.target_tables,
            self.rt.kinds,
        ) else {
            return;
        };
        let target_schema = self.target.as_ref().and_then(|t| {
            if tk.has_schemas() {
                t.schema.clone().or_else(|| picker::default_schema(tk))
            } else {
                None
            }
        });
        self.rt.diffs = compare(sk, source, tk, target, target_schema.as_deref());
        self.rt.checked = self
            .rt
            .diffs
            .iter()
            .filter(|d| {
                matches!(d.status, DiffStatus::Added | DiffStatus::Changed) && d.error.is_none()
            })
            .map(|d| d.name.clone())
            .collect();
        self.rt.focus = self
            .rt
            .diffs
            .iter()
            .find(|d| d.status != DiffStatus::Identical)
            .map(|d| d.name.clone());
    }

    fn compare_now(&mut self, cx: &mut TabContext<'_>) {
        let (Some(source), Some(target)) = (self.source.clone(), self.target.clone()) else {
            return;
        };
        let (Some(sk), Some(tk)) = (source.kind(cx), target.kind(cx)) else {
            return;
        };
        self.rt = Runtime {
            show_identical: self.rt.show_identical,
            kinds: Some((sk, tk)),
            ..Runtime::default()
        };
        self.rt.source_req = super::describe(cx, &source);
        self.rt.target_req = super::describe(cx, &target);
    }

    fn script(&self) -> Vec<String> {
        let selected: Vec<&TableDiff> = self
            .rt
            .diffs
            .iter()
            .filter(|d| self.rt.checked.contains(&d.name))
            .collect();
        migration_script(&selected)
    }

    /// Renders the tool.
    pub fn show(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                header(
                    ui,
                    icon::GIT_DIFF,
                    "Schema diff",
                    "make the target match the source",
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.group(|ui| {
                        ui.label(RichText::new("Source").strong());
                        picker::show(ui, "diff-source", &mut self.source, cx);
                    });
                    ui.label(RichText::new(icon::ARROW_RIGHT).size(20.0));
                    ui.group(|ui| {
                        ui.label(RichText::new("Target").strong());
                        picker::show(ui, "diff-target", &mut self.target, cx);
                    });
                    ui.vertical(|ui| {
                        let busy = self.rt.source_req.is_some() || self.rt.target_req.is_some();
                        let ready = self.source.is_some() && self.target.is_some();
                        if ui
                            .add_enabled(
                                ready && !busy,
                                Button::new(format!("{}  Compare", icon::GIT_DIFF)),
                            )
                            .clicked()
                        {
                            self.compare_now(cx);
                        }
                        if busy {
                            ui.spinner();
                        }
                        ui.checkbox(&mut self.rt.show_identical, "Show identical");
                    });
                });
                if let Some(e) = &self.rt.error {
                    error_box(ui, e);
                }
                if let Some(n) = &self.rt.notice {
                    ui.label(
                        RichText::new(format!("{}  {n}", icon::CHECK_CIRCLE)).color(color::SUCCESS),
                    );
                }
                if self.rt.diffs.is_empty() {
                    if self.rt.source_tables.is_some() && self.rt.target_tables.is_some() {
                        ui.label("Both sides have no tables.");
                    }
                    return;
                }
                ui.separator();
                self.results(ui, cx);
            });
    }

    fn results(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        let counts = |s: DiffStatus| self.rt.diffs.iter().filter(|d| d.status == s).count();
        ui.label(
            RichText::new(format!(
                "{} only in source  ·  {} different  ·  {} only in target  ·  {} identical",
                counts(DiffStatus::Added),
                counts(DiffStatus::Changed),
                counts(DiffStatus::Removed),
                counts(DiffStatus::Identical)
            ))
            .color(color::TEXT_DIM),
        );
        let script_lines = self.script();
        // Room for the action bar (and the confirmation row) below the list and details: the
        // vertical separator would otherwise take all remaining height.
        let reserve = if self.rt.confirm { 76.0 } else { 44.0 };
        let height = (ui.available_height() - reserve).max(120.0);
        ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
            ui.horizontal_top(|ui| {
                // Table list.
                ui.vertical(|ui| {
                    ui.set_width(300.0);
                    ScrollArea::vertical()
                        .id_salt("diff-list")
                        .max_height(height)
                        .show(ui, |ui| {
                            for d in &self.rt.diffs {
                                if d.status == DiffStatus::Identical && !self.rt.show_identical {
                                    continue;
                                }
                                ui.horizontal(|ui| {
                                    let mut on = self.rt.checked.contains(&d.name);
                                    let selectable =
                                        d.status != DiffStatus::Identical && d.error.is_none();
                                    if ui
                                        .add_enabled(
                                            selectable,
                                            egui::Checkbox::without_text(&mut on),
                                        )
                                        .changed()
                                    {
                                        if on {
                                            self.rt.checked.insert(d.name.clone());
                                        } else {
                                            self.rt.checked.remove(&d.name);
                                        }
                                    }
                                    let (glyph, c) = status_icon(d.status);
                                    ui.label(RichText::new(glyph).color(c));
                                    let focused = self.rt.focus.as_ref() == Some(&d.name);
                                    if ui
                                        .selectable_label(focused, &d.name)
                                        .on_hover_text(d.status.label())
                                        .clicked()
                                    {
                                        self.rt.focus = Some(d.name.clone());
                                    }
                                    if d.error.is_some() {
                                        ui.label(RichText::new(icon::WARNING).color(color::DANGER));
                                    }
                                });
                            }
                        });
                });
                ui.separator();
                // Details of the focused table.
                ui.vertical(|ui| {
                    let Some(d) = self
                        .rt
                        .focus
                        .as_ref()
                        .and_then(|f| self.rt.diffs.iter().find(|d| &d.name == f))
                    else {
                        ui.label("Select a table to see its differences.");
                        return;
                    };
                    let kind = self
                        .rt
                        .kinds
                        .map(|k| k.1)
                        .unwrap_or(DatabaseKind::PostgreSql);
                    ui.label(
                        RichText::new(format!("{}  ·  {}", d.name, d.status.label())).strong(),
                    );
                    if let Some(e) = &d.error {
                        error_box(ui, e);
                    }
                    let ddl = |t: &Option<TableSchema>| {
                        t.as_ref().map_or_else(
                            || "-- (does not exist)".to_owned(),
                            |t| script(&create_table(kind, t)),
                        )
                    };
                    let half = (ui.available_width() / 2.0 - 8.0).max(200.0);
                    ui.horizontal_top(|ui| {
                        for (title, text) in [
                            ("Source (as target DDL)", ddl(&d.source)),
                            ("Target", ddl(&d.target)),
                        ] {
                            ui.vertical(|ui| {
                                ui.set_width(half);
                                ui.label(RichText::new(title).color(color::TEXT_DIM));
                                let mut shown = text.as_str();
                                ScrollArea::both()
                                    .id_salt(("diff-ddl", title))
                                    .max_height(220.0)
                                    .show(ui, |ui| {
                                        ui.add(
                                            TextEdit::multiline(&mut shown)
                                                .font(theme::mono())
                                                .desired_width(half),
                                        );
                                    });
                            });
                        }
                    });
                    ui.label(RichText::new("Statements for this table").color(color::TEXT_DIM));
                    let mut own = script(&[d.statements.clone(), d.deferred.clone()].concat());
                    if own.is_empty() {
                        own = "-- none".into();
                    }
                    let mut shown = own.as_str();
                    ScrollArea::both()
                        .id_salt("diff-own")
                        .max_height(120.0)
                        .show(ui, |ui| {
                            ui.add(
                                TextEdit::multiline(&mut shown)
                                    .font(theme::mono())
                                    .desired_width(f32::INFINITY),
                            );
                        });
                });
            });
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} statement(s) for {} selected table(s)",
                script_lines.len(),
                self.rt.checked.len()
            ));
            let text = script(&script_lines);
            if ui
                .add_enabled(
                    !script_lines.is_empty(),
                    Button::new(format!("{}  Open in Query Editor", icon::FILE_SQL)),
                )
                .clicked()
                && let Some(t) = &self.target
            {
                cx.actions.push(UiAction::OpenScript {
                    profile_id: t.profile_id,
                    database: t.database.clone(),
                    sql: text.clone(),
                    run: false,
                });
            }
            if ui
                .add_enabled(
                    !script_lines.is_empty(),
                    Button::new(format!("{}  Copy Script", icon::COPY)),
                )
                .clicked()
            {
                ui.ctx().copy_text(text);
            }
            let busy = self.rt.apply.is_some();
            if ui
                .add_enabled(
                    !script_lines.is_empty() && !busy,
                    Button::new(
                        RichText::new(format!("{}  Apply to Target", icon::PLAY))
                            .color(color::WARNING),
                    ),
                )
                .clicked()
            {
                self.rt.confirm = true;
            }
            if busy {
                ui.spinner();
            }
        });
        if self.rt.confirm {
            let target_name = self
                .target
                .as_ref()
                .map(|t| t.label(cx))
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{}  Run {} statement(s) on {target_name}? Dropped tables lose their data.",
                        icon::WARNING,
                        script_lines.len()
                    ))
                    .color(color::WARNING),
                );
                if ui
                    .button(RichText::new("Apply").color(color::DANGER))
                    .clicked()
                    && let Some(t) = self.target.clone()
                {
                    self.rt.confirm = false;
                    self.rt.notice = None;
                    self.rt.error = None;
                    self.rt.apply = super::run_script(cx, &t, script_lines.clone());
                    cx.actions.push(UiAction::Refresh(t.profile_id));
                }
                if ui.button("Cancel").clicked() {
                    self.rt.confirm = false;
                }
            });
        }
    }
}
