//! Backups: one-off SQL dumps and schedules that run while the application is open.

use egui::{Button, DragValue, Grid, RichText, ScrollArea, TextEdit, Ui};
use egui_phosphor::regular as icon;
use rdbs_core::job::{BackupOptions, BackupSchedule, Frequency};
use rdbs_core::{DatabaseKind, Event};
use serde::{Deserialize, Serialize};

use super::{Scope, header, picker};
use crate::model::UiAction;
use crate::theme::color;
use crate::workspace::TabContext;

/// Backup tool state.
#[derive(Serialize, Deserialize, Default)]
pub struct Backup {
    /// Scope to prefill a new backup with.
    pub scope: Option<Scope>,
    #[serde(skip)]
    rt: Runtime,
}

#[derive(Default)]
struct Runtime {
    /// Schedule being edited (new or existing).
    form: Option<Form>,
    initialized: bool,
}

struct Form {
    id: Option<String>,
    name: String,
    scope: Option<Scope>,
    folder: String,
    file: String,
    include_schema: bool,
    include_data: bool,
    keep: usize,
    frequency: Frequency,
    enabled: bool,
}

impl Form {
    fn new(scope: Option<Scope>, cx: &TabContext<'_>) -> Self {
        let base = scope
            .as_ref()
            .and_then(|s| {
                s.database
                    .clone()
                    .or_else(|| s.profile(cx).map(|p| p.name.clone()))
            })
            .unwrap_or_else(|| "backup".into())
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let folder = directories::UserDirs::new()
            .and_then(|d| d.document_dir().map(|p| p.join("Zephix Backups")))
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        Self {
            id: None,
            // "orders backup" for a chosen database; plain "Backup" before one is picked.
            name: if base == "backup" {
                "Backup".to_owned()
            } else {
                format!("{base} backup")
            },
            scope,
            folder,
            file: format!("{base}_{{date}}_{{time}}.sql"),
            include_schema: true,
            include_data: true,
            keep: 7,
            frequency: Frequency::default(),
            enabled: true,
        }
    }

    fn from_schedule(s: &BackupSchedule, cx: &TabContext<'_>) -> Self {
        let kind = cx
            .profile(s.profile_id)
            .map_or(DatabaseKind::PostgreSql, |p| p.kind);
        Self {
            id: Some(s.id.clone()),
            name: s.name.clone(),
            scope: Some(super::scope(
                kind,
                s.profile_id,
                s.database.clone(),
                s.options.schema.clone(),
            )),
            folder: s
                .options
                .path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            file: s
                .options
                .path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default(),
            include_schema: s.options.include_schema,
            include_data: s.options.include_data,
            keep: s.options.keep,
            frequency: s.frequency,
            enabled: s.enabled,
        }
    }

    fn options(&self) -> BackupOptions {
        BackupOptions {
            schema: self.scope.as_ref().and_then(|s| s.schema.clone()),
            tables: Vec::new(),
            include_schema: self.include_schema,
            include_data: self.include_data,
            path: std::path::Path::new(self.folder.trim()).join(self.file.trim()),
            keep: self.keep,
        }
    }

    fn problem(&self) -> Option<&'static str> {
        if self.scope.is_none() {
            Some("Choose a connection")
        } else if self.folder.trim().is_empty() || self.file.trim().is_empty() {
            Some("Choose a folder and file name")
        } else if !self.include_schema && !self.include_data {
            Some("Include the schema, the data or both")
        } else {
            None
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn when(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .map_or_else(String::new, |t| t.format("%Y-%m-%d %H:%M").to_string())
}

impl Backup {
    /// Backup tool, with a new backup prefilled for `scope` when given.
    pub fn new(scope: Option<Scope>) -> Self {
        Self {
            scope,
            rt: Runtime::default(),
        }
    }

    /// Tab title.
    pub fn title(&self) -> String {
        "Backups".to_owned()
    }

    /// Backups report through the application (schedules outlive tabs).
    pub fn on_event(&mut self, _event: &Event) -> bool {
        false
    }

    /// Renders the tool.
    pub fn show(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        if !self.rt.initialized {
            self.rt.initialized = true;
            if self.scope.is_some() {
                self.rt.form = Some(Form::new(self.scope.clone(), cx));
            }
        }
        egui::Frame::new()
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    header(
                        ui,
                        icon::ARCHIVE,
                        "Backups",
                        "SQL dumps, on demand or on a schedule",
                    );
                    ui.separator();
                    if ui.button(format!("{}  New Backup", icon::PLUS)).clicked() {
                        self.rt.form = Some(Form::new(self.scope.clone(), cx));
                    }
                });
                ui.label(
                    RichText::new(
                        "Schedules run while Zephix is open. Connections must save their \
                         password in the OS keychain to run unattended.",
                    )
                    .small()
                    .color(color::TEXT_FAINT),
                );
                ui.add_space(6.0);
                self.schedule_list(ui, cx);
                if self.rt.form.is_some() {
                    ui.separator();
                    self.form(ui, cx);
                }
            });
    }

    fn schedule_list(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        let schedules = cx.backups.schedules.clone();
        if schedules.is_empty() {
            ui.label(RichText::new("No scheduled backups yet.").color(color::TEXT_DIM));
            return;
        }
        ScrollArea::vertical()
            .id_salt("bk-list")
            .max_height(260.0)
            .show(ui, |ui| {
                Grid::new("bk-grid")
                    .striped(true)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for h in [
                            "",
                            "Name",
                            "Connection",
                            "When",
                            "Next run",
                            "Last result",
                            "",
                        ] {
                            ui.label(RichText::new(h).strong().color(color::TEXT_DIM));
                        }
                        ui.end_row();
                        for s in &schedules {
                            let mut enabled = s.enabled;
                            if ui
                                .checkbox(&mut enabled, "")
                                .on_hover_text("Enabled")
                                .changed()
                            {
                                let mut updated = s.clone();
                                updated.enabled = enabled;
                                if enabled {
                                    updated.last_run = now();
                                }
                                cx.actions.push(UiAction::SaveSchedule(updated));
                            }
                            ui.label(&s.name);
                            let conn = cx
                                .profile(s.profile_id)
                                .map_or_else(|| "(deleted)".to_owned(), |p| p.name.clone());
                            ui.label(match &s.database {
                                Some(db) => format!("{conn} · {db}"),
                                None => conn,
                            });
                            ui.label(s.frequency.describe());
                            let next = {
                                use chrono::TimeZone;
                                chrono::Local
                                    .timestamp_opt(s.last_run, 0)
                                    .single()
                                    .map(|last| rdbs_transfer::backup::next_run(s.frequency, last))
                            };
                            ui.label(if s.enabled {
                                next.map_or_else(String::new, |t| {
                                    t.format("%Y-%m-%d %H:%M").to_string()
                                })
                            } else {
                                "disabled".into()
                            });
                            match (cx.backups.status.get(&s.id), &s.last_result) {
                                (Some(progress), _) => {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(RichText::new(progress).small());
                                    });
                                }
                                (None, Some(Ok(summary))) => {
                                    ui.label(
                                        RichText::new(format!(
                                            "{}  {}",
                                            icon::CHECK_CIRCLE,
                                            when(s.last_run)
                                        ))
                                        .color(color::SUCCESS),
                                    )
                                    .on_hover_text(summary);
                                }
                                (None, Some(Err(e))) => {
                                    ui.label(
                                        RichText::new(format!(
                                            "{}  {}",
                                            icon::WARNING,
                                            when(s.last_run)
                                        ))
                                        .color(color::DANGER),
                                    )
                                    .on_hover_text(e);
                                }
                                (None, None) => {
                                    ui.label(RichText::new("never run").color(color::TEXT_FAINT));
                                }
                            }
                            ui.horizontal(|ui| {
                                let running = cx.backups.status.contains_key(&s.id);
                                if ui
                                    .add_enabled(!running, Button::new(icon::PLAY).small())
                                    .on_hover_text("Run now")
                                    .clicked()
                                {
                                    cx.actions.push(UiAction::RunSchedule(s.id.clone()));
                                }
                                if ui
                                    .small_button(icon::PENCIL_SIMPLE)
                                    .on_hover_text("Edit")
                                    .clicked()
                                {
                                    self.rt.form = Some(Form::from_schedule(s, cx));
                                }
                                if ui
                                    .small_button(icon::TRASH)
                                    .on_hover_text("Delete")
                                    .clicked()
                                {
                                    cx.actions.push(UiAction::DeleteSchedule(s.id.clone()));
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
    }

    fn form(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        let Some(form) = self.rt.form.as_mut() else {
            return;
        };
        ui.label(
            RichText::new(if form.id.is_some() {
                "Edit backup"
            } else {
                "New backup"
            })
            .strong(),
        );
        Grid::new("bk-form")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Name");
                ui.add_sized(
                    [260.0, ui.spacing().interact_size.y],
                    TextEdit::singleline(&mut form.name),
                );
                ui.end_row();

                ui.label("Database");
                picker::show(ui, "bk-scope", &mut form.scope, cx);
                ui.end_row();

                ui.label("Folder");
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [320.0, ui.spacing().interact_size.y],
                        TextEdit::singleline(&mut form.folder),
                    );
                    if ui
                        .button(format!("{}  Browse…", icon::FOLDER_OPEN))
                        .clicked()
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        form.folder = dir.display().to_string();
                    }
                });
                ui.end_row();

                ui.label("File name");
                ui.vertical(|ui| {
                    ui.add_sized(
                        [320.0, ui.spacing().interact_size.y],
                        TextEdit::singleline(&mut form.file),
                    );
                    ui.label(
                        RichText::new("{date} and {time} are replaced by the start time")
                            .small()
                            .color(color::TEXT_FAINT),
                    );
                });
                ui.end_row();

                ui.label("Content");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut form.include_schema, "Table definitions");
                    ui.checkbox(&mut form.include_data, "Data");
                });
                ui.end_row();

                ui.label("Keep");
                ui.horizontal(|ui| {
                    ui.add(DragValue::new(&mut form.keep).range(0..=1000));
                    ui.label(RichText::new("newest files (0 = keep all)").color(color::TEXT_DIM));
                });
                ui.end_row();

                ui.label("Schedule");
                ui.horizontal(|ui| {
                    let kind = match form.frequency {
                        Frequency::EveryHours { .. } => 0,
                        Frequency::Daily { .. } => 1,
                        Frequency::Weekly { .. } => 2,
                    };
                    egui::ComboBox::from_id_salt("bk-freq")
                        .selected_text(["Every N hours", "Daily", "Weekly"][kind])
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(kind == 0, "Every N hours").clicked() {
                                form.frequency = Frequency::EveryHours {
                                    hours: 6,
                                    minute: 0,
                                };
                            }
                            if ui.selectable_label(kind == 1, "Daily").clicked() {
                                form.frequency = Frequency::Daily { hour: 2, minute: 0 };
                            }
                            if ui.selectable_label(kind == 2, "Weekly").clicked() {
                                form.frequency = Frequency::Weekly {
                                    weekday: 0,
                                    hour: 2,
                                    minute: 0,
                                };
                            }
                        });
                    match &mut form.frequency {
                        Frequency::EveryHours { hours, minute } => {
                            ui.label("every");
                            ui.add(DragValue::new(hours).range(1..=24));
                            ui.label("hours at minute");
                            ui.add(DragValue::new(minute).range(0..=59));
                        }
                        Frequency::Daily { hour, minute } => {
                            ui.label("at");
                            ui.add(DragValue::new(hour).range(0..=23));
                            ui.label(":");
                            ui.add(DragValue::new(minute).range(0..=59));
                        }
                        Frequency::Weekly {
                            weekday,
                            hour,
                            minute,
                        } => {
                            const DAYS: [&str; 7] =
                                ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
                            egui::ComboBox::from_id_salt("bk-day")
                                .selected_text(DAYS[(*weekday as usize) % 7])
                                .show_ui(ui, |ui| {
                                    for (i, d) in DAYS.iter().enumerate() {
                                        ui.selectable_value(weekday, i as u32, *d);
                                    }
                                });
                            ui.label("at");
                            ui.add(DragValue::new(hour).range(0..=23));
                            ui.label(":");
                            ui.add(DragValue::new(minute).range(0..=59));
                        }
                    }
                    ui.checkbox(&mut form.enabled, "Enabled");
                });
                ui.end_row();
            });
        let problem = form.problem();
        let mut close = false;
        ui.horizontal(|ui| {
            let ok = problem.is_none();
            if ui
                .add_enabled(
                    ok,
                    Button::new(format!("{}  Save Schedule", icon::FLOPPY_DISK)),
                )
                .on_disabled_hover_text(problem.unwrap_or_default())
                .clicked()
                && let Some(scope) = &form.scope
            {
                let existing = form
                    .id
                    .as_ref()
                    .and_then(|id| cx.backups.schedules.iter().find(|s| &s.id == id));
                cx.actions.push(UiAction::SaveSchedule(BackupSchedule {
                    id: form
                        .id
                        .clone()
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    name: form.name.trim().to_owned(),
                    profile_id: scope.profile_id,
                    database: scope.database.clone(),
                    options: form.options(),
                    frequency: form.frequency,
                    enabled: form.enabled,
                    last_run: existing.map_or_else(now, |s| s.last_run),
                    last_result: existing.and_then(|s| s.last_result.clone()),
                }));
                close = true;
            }
            if ui
                .add_enabled(ok, Button::new(format!("{}  Back Up Now", icon::PLAY)))
                .on_hover_text("Run once without saving a schedule")
                .on_disabled_hover_text(problem.unwrap_or_default())
                .clicked()
                && let Some(scope) = &form.scope
            {
                cx.actions.push(UiAction::RunBackup {
                    profile_id: scope.profile_id,
                    database: scope.database.clone(),
                    options: form.options(),
                });
            }
            if ui.button("Close").clicked() {
                close = true;
            }
        });
        if close {
            self.rt.form = None;
            return;
        }
        if let Some(status) = cx.backups.status.get("") {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(status);
            });
        }
        if let Some(result) = &cx.backups.adhoc_result {
            match result {
                Ok(s) => ui.label(
                    RichText::new(format!("{}  {s}", icon::CHECK_CIRCLE)).color(color::SUCCESS),
                ),
                Err(e) => {
                    ui.label(RichText::new(format!("{}  {e}", icon::WARNING)).color(color::DANGER))
                }
            };
        }
    }
}
