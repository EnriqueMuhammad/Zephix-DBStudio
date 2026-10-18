//! Data transfer / synchronisation between two connections (any engines).

use std::collections::BTreeSet;

use egui::{Button, ProgressBar, RichText, ScrollArea, Ui};
use egui_phosphor::regular as icon;
use rdbs_core::job::{TransferMode, TransferOptions};
use rdbs_core::{Command, Event, RequestId, TableSchema};
use serde::{Deserialize, Serialize};

use super::{Scope, error_box, error_text, header, picker};
use crate::model::{AfterPassword, StatusMessage, UiAction};
use crate::theme::color;
use crate::workspace::TabContext;

/// Transfer tool state; the scopes and options are persisted.
#[derive(Serialize, Deserialize)]
pub struct Transfer {
    /// Where rows come from.
    pub source: Option<Scope>,
    /// Where rows go.
    pub target: Option<Scope>,
    /// Row handling.
    #[serde(default)]
    pub mode: TransferMode,
    /// Create tables missing in the target.
    #[serde(default = "yes")]
    pub create_missing: bool,
    #[serde(skip)]
    rt: Runtime,
}

fn yes() -> bool {
    true
}

#[derive(Default)]
struct Runtime {
    describe: Option<RequestId>,
    tables: Vec<TableSchema>,
    selected: BTreeSet<String>,
    filter: String,
    job: Option<RequestId>,
    progress: f32,
    log: Vec<String>,
    result: Option<Result<String, String>>,
    error: Option<String>,
    confirm: bool,
}

impl Transfer {
    /// Transfer starting from `source`.
    pub fn new(source: Option<Scope>) -> Self {
        Self {
            source,
            target: None,
            mode: TransferMode::Append,
            create_missing: true,
            rt: Runtime::default(),
        }
    }

    /// Tab title.
    pub fn title(&self) -> String {
        "Data transfer".to_owned()
    }

    /// Consumes this tool's events.
    pub fn on_event(&mut self, event: &Event) -> bool {
        match event {
            Event::SchemaDescribed { request_id, result }
                if self.rt.describe == Some(*request_id) =>
            {
                self.rt.describe = None;
                match result {
                    Ok(tables) => {
                        self.rt.selected = tables.iter().map(|t| t.name.clone()).collect();
                        self.rt.tables = tables.clone();
                        self.rt.error = None;
                    }
                    Err(e) => self.rt.error = Some(error_text(e)),
                }
                true
            }
            Event::JobProgress {
                request_id,
                message,
                fraction,
            } if self.rt.job == Some(*request_id) => {
                if let Some(f) = fraction {
                    self.rt.progress = *f;
                }
                // Consecutive updates of the same table replace each other.
                let table = message.split(':').next().unwrap_or_default();
                if let Some(last) = self.rt.log.last_mut()
                    && last.split(':').next() == Some(table)
                    && last.contains(':')
                    && message.contains(':')
                {
                    last.clone_from(message);
                } else {
                    self.rt.log.push(message.clone());
                }
                true
            }
            Event::JobFinished { request_id, result } if self.rt.job == Some(*request_id) => {
                self.rt.job = None;
                self.rt.result = Some(result.clone().map_err(|e| error_text(&e)));
                if result.is_ok() {
                    self.rt.progress = 1.0;
                }
                true
            }
            _ => false,
        }
    }

    fn options(&self) -> TransferOptions {
        let schema = |s: &Option<Scope>| s.as_ref().and_then(|s| s.schema.clone());
        TransferOptions {
            source_schema: schema(&self.source),
            target_schema: schema(&self.target),
            tables: if self.rt.selected.len() == self.rt.tables.len() {
                Vec::new()
            } else {
                self.rt.selected.iter().cloned().collect()
            },
            create_missing: self.create_missing,
            mode: self.mode,
            ..TransferOptions::default()
        }
    }

    fn start(&mut self, cx: &mut TabContext<'_>) {
        let (Some(source), Some(target)) = (self.source.clone(), self.target.clone()) else {
            return;
        };
        let (Some(sp), Some(tp)) = (source.profile(cx).cloned(), target.profile(cx).cloned())
        else {
            return;
        };
        for p in [&sp, &tp] {
            if crate::editor::needs_password(p, cx.connections, cx.passwords) {
                cx.actions.push(UiAction::NeedPassword {
                    profile_id: p.id,
                    then: AfterPassword::Connect,
                });
                return;
            }
        }
        let request_id = cx.services.next_request_id();
        cx.services.send(Command::Transfer {
            request_id,
            source_password: cx.passwords.get(&sp.id).cloned(),
            target_password: cx.passwords.get(&tp.id).cloned(),
            source: sp,
            source_database: source.database.clone(),
            target: tp,
            target_database: target.database.clone(),
            options: self.options(),
        });
        self.rt.job = Some(request_id);
        self.rt.progress = 0.0;
        self.rt.log.clear();
        self.rt.result = None;
        cx.actions.push(UiAction::Status(StatusMessage::info(
            "Data transfer started",
        )));
    }

    /// Renders the tool.
    pub fn show(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                header(
                    ui,
                    icon::FLOW_ARROW,
                    "Data transfer",
                    "copy or synchronise tables between connections",
                );
                ui.add_space(6.0);
                let running = self.rt.job.is_some();
                ui.add_enabled_ui(!running, |ui| {
                    ui.horizontal(|ui| {
                        ui.group(|ui| {
                            ui.label(RichText::new("Source").strong());
                            if picker::show(ui, "xfer-source", &mut self.source, cx) {
                                self.rt.tables.clear();
                                self.rt.selected.clear();
                            }
                        });
                        ui.label(RichText::new(icon::ARROW_RIGHT).size(20.0));
                        ui.group(|ui| {
                            ui.label(RichText::new("Target").strong());
                            picker::show(ui, "xfer-target", &mut self.target, cx);
                        });
                    });
                });
                if let Some(e) = &self.rt.error {
                    error_box(ui, e);
                }
                ui.add_space(6.0);
                ui.columns(2, |cols| {
                    self.tables_panel(&mut cols[0], cx, running);
                    self.options_panel(&mut cols[1], cx, running);
                });
            });
    }

    fn tables_panel(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>, running: bool) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Tables").strong());
            if ui
                .add_enabled(
                    self.source.is_some() && self.rt.describe.is_none() && !running,
                    Button::new(format!("{}  Load", icon::ARROWS_CLOCKWISE)),
                )
                .clicked()
                && let Some(s) = self.source.clone()
            {
                self.rt.describe = super::describe(cx, &s);
            }
            if self.rt.describe.is_some() {
                ui.spinner();
            }
            if !self.rt.tables.is_empty() {
                if ui.small_button("All").clicked() {
                    self.rt.selected = self.rt.tables.iter().map(|t| t.name.clone()).collect();
                }
                if ui.small_button("None").clicked() {
                    self.rt.selected.clear();
                }
            }
        });
        if self.rt.tables.is_empty() {
            ui.label(RichText::new("Choose a source and press Load.").color(color::TEXT_DIM));
            return;
        }
        ui.add(egui::TextEdit::singleline(&mut self.rt.filter).hint_text("filter…"));
        let needle = self.rt.filter.to_lowercase();
        ScrollArea::vertical()
            .id_salt("xfer-tables")
            .max_height(ui.available_height() - 10.0)
            .show(ui, |ui| {
                for t in &self.rt.tables {
                    if !needle.is_empty() && !t.name.to_lowercase().contains(&needle) {
                        continue;
                    }
                    let mut on = self.rt.selected.contains(&t.name);
                    let label = format!("{}  ({} columns)", t.name, t.columns.len());
                    if ui
                        .add_enabled(!running, egui::Checkbox::new(&mut on, label))
                        .changed()
                    {
                        if on {
                            self.rt.selected.insert(t.name.clone());
                        } else {
                            self.rt.selected.remove(&t.name);
                        }
                    }
                }
            });
    }

    fn options_panel(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>, running: bool) {
        ui.label(RichText::new("Options").strong());
        ui.add_enabled_ui(!running, |ui| {
            let sync = matches!(self.mode, TransferMode::Sync { .. });
            for mode in [
                TransferMode::Append,
                TransferMode::Replace,
                TransferMode::Recreate,
            ] {
                ui.radio_value(&mut self.mode, mode, mode.label());
            }
            if ui
                .radio(
                    sync,
                    TransferMode::Sync {
                        delete_missing: false,
                    }
                    .label(),
                )
                .on_hover_text("Insert new rows, update changed rows (matched by primary key)")
                .clicked()
                && !sync
            {
                self.mode = TransferMode::Sync {
                    delete_missing: false,
                };
            }
            if let TransferMode::Sync { delete_missing } = &mut self.mode {
                ui.indent("sync-opts", |ui| {
                    ui.checkbox(
                        delete_missing,
                        "Delete target rows that are not in the source",
                    );
                });
            }
            ui.checkbox(
                &mut self.create_missing,
                "Create tables missing in the target",
            );
        });
        ui.add_space(8.0);
        let ready = self.source.is_some()
            && self.target.is_some()
            && !self.rt.selected.is_empty()
            && self.source != self.target;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    ready && !running,
                    Button::new(
                        RichText::new(format!("{}  Start", icon::PLAY)).color(color::SUCCESS),
                    ),
                )
                .on_disabled_hover_text(
                    "Choose different source and target, and at least one table",
                )
                .clicked()
            {
                self.rt.confirm = true;
            }
            if running
                && ui
                    .button(RichText::new(format!("{}  Cancel", icon::STOP)).color(color::DANGER))
                    .clicked()
                && let Some(id) = self.rt.job
            {
                cx.services.send(Command::Cancel { request_id: id });
            }
        });
        if self.rt.confirm {
            let destructive = matches!(
                self.mode,
                TransferMode::Replace
                    | TransferMode::Recreate
                    | TransferMode::Sync {
                        delete_missing: true
                    }
            );
            let target = self
                .target
                .as_ref()
                .map(|t| t.label(cx))
                .unwrap_or_default();
            ui.label(
                RichText::new(format!(
                    "{}  Transfer {} table(s) into {target}?{}",
                    icon::WARNING,
                    self.rt.selected.len(),
                    if destructive {
                        " Existing target rows will be deleted or replaced."
                    } else {
                        ""
                    }
                ))
                .color(color::WARNING),
            );
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new("Start transfer").color(color::DANGER))
                    .clicked()
                {
                    self.rt.confirm = false;
                    self.start(cx);
                }
                if ui.button("Cancel").clicked() {
                    self.rt.confirm = false;
                }
            });
        }
        if running || self.rt.result.is_some() || !self.rt.log.is_empty() {
            ui.add_space(6.0);
            ui.add(
                ProgressBar::new(self.rt.progress)
                    .show_percentage()
                    .animate(running),
            );
            ScrollArea::vertical()
                .id_salt("xfer-log")
                .max_height(200.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.rt.log {
                        ui.label(RichText::new(line).small().color(color::TEXT_DIM));
                    }
                });
        }
        match &self.rt.result {
            Some(Ok(summary)) => {
                ui.label(
                    RichText::new(format!("{}  {summary}", icon::CHECK_CIRCLE))
                        .color(color::SUCCESS),
                );
            }
            Some(Err(e)) => error_box(ui, e),
            None => {}
        }
    }
}
