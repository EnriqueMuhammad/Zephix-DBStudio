//! Import wizard: CSV/TSV or spreadsheet file → existing or new table, with column mapping.
//! Everything (create table, optional delete, inserts) runs in one transaction where the
//! engine supports transactional DDL.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use egui::{Button, Grid, RichText, ScrollArea, TextEdit, Ui};
use egui_phosphor::regular as icon;
use rdbs_core::dialect::qualified_name;
use rdbs_core::edit::Change;
use rdbs_core::{Command, DatabaseKind, Event, RequestId, TableSchema};
use rdbs_transfer::import::{
    self, ImportOptions, ReadOptions, Sheet, auto_map, build_inserts, table_for_sheet,
};
use serde::{Deserialize, Serialize};

use super::{Scope, error_box, error_text, header};
use crate::model::UiAction;
use crate::theme::color;
use crate::workspace::TabContext;

/// Import tool state; only the target is persisted.
#[derive(Serialize, Deserialize)]
pub struct Import {
    /// Where to import.
    pub scope: Scope,
    /// Table chosen when the tool was opened from a table.
    pub table: Option<String>,
    #[serde(skip)]
    rt: Runtime,
}

type Loaded = Result<(Sheet, Vec<String>), String>;

/// Target table, source column per target column, statements to run before the inserts.
type Plan = (TableSchema, Vec<Option<usize>>, Vec<Change>);

enum Phase {
    Idle,
    Building(Receiver<Result<Vec<Change>, String>>),
    Running(RequestId, usize),
    Done(String),
    Failed(String),
}

struct Runtime {
    started: bool,
    path: String,
    options: ReadOptions,
    sheets: Vec<String>,
    loading: Option<Receiver<Loaded>>,
    sheet: Option<Sheet>,
    file_error: Option<String>,
    tables: Vec<TableSchema>,
    describe: Option<RequestId>,
    describe_error: Option<String>,
    new_table: bool,
    target: Option<String>,
    /// Definition for a new table (editable names and types).
    new_def: TableSchema,
    /// Per new-table column: imported or not.
    include: Vec<bool>,
    /// Existing table: source column per target column.
    mapping: Vec<Option<usize>>,
    mapped_for: Option<String>,
    empty_is_null: bool,
    replace: bool,
    phase: Phase,
    confirm: bool,
    /// Explorer and table list were refreshed after the last import.
    refreshed: bool,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            started: false,
            path: String::new(),
            options: ReadOptions::default(),
            sheets: Vec::new(),
            loading: None,
            sheet: None,
            file_error: None,
            tables: Vec::new(),
            describe: None,
            describe_error: None,
            new_table: false,
            target: None,
            new_def: TableSchema::default(),
            include: Vec::new(),
            mapping: Vec::new(),
            mapped_for: None,
            empty_is_null: true,
            replace: false,
            phase: Phase::Idle,
            confirm: false,
            refreshed: true,
        }
    }
}

impl Import {
    /// Import into `table` (or let the user choose / create one).
    pub fn new(scope: Scope, table: Option<String>) -> Self {
        let rt = Runtime {
            target: table.clone(),
            new_table: false,
            ..Runtime::default()
        };
        Self { scope, table, rt }
    }

    /// Tab title.
    pub fn title(&self) -> String {
        match &self.table {
            Some(t) => format!("Import into {t}"),
            None => "Import data".to_owned(),
        }
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
                        self.rt.tables = tables.clone();
                        self.rt.describe_error = None;
                    }
                    Err(e) => self.rt.describe_error = Some(error_text(e)),
                }
                true
            }
            Event::ChangesApplied { request_id, result } => {
                let Phase::Running(id, rows) = &self.rt.phase else {
                    return false;
                };
                if id != request_id {
                    return false;
                }
                let rows = *rows;
                self.rt.phase = match result {
                    Ok(_) => Phase::Done(format!("Imported {rows} row(s)")),
                    Err(e) => Phase::Failed(format!("{}\nNothing was imported.", error_text(e))),
                };
                true
            }
            _ => false,
        }
    }

    fn load_file(&mut self) {
        let path = PathBuf::from(self.rt.path.trim());
        let options = self.rt.options.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let sheets = if import::is_spreadsheet(&path) {
                import::sheet_names(&path).unwrap_or_default()
            } else {
                Vec::new()
            };
            let _ = tx.send(import::read_file(&path, &options).map(|s| (s, sheets)));
        });
        self.rt.loading = Some(rx);
        self.rt.file_error = None;
    }

    fn poll(&mut self, kind: DatabaseKind) {
        if let Some(rx) = &self.rt.loading {
            match rx.try_recv() {
                Ok(Ok((sheet, sheets))) => {
                    self.rt.loading = None;
                    self.rt.sheets = sheets;
                    let stem = std::path::Path::new(self.rt.path.trim())
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("imported")
                        .chars()
                        .map(|c| {
                            if c.is_alphanumeric() {
                                c.to_ascii_lowercase()
                            } else {
                                '_'
                            }
                        })
                        .collect::<String>();
                    self.rt.new_def =
                        table_for_sheet(kind, self.scope.describe_schema(kind), &stem, &sheet);
                    self.rt.include = vec![true; sheet.headers.len()];
                    self.rt.mapped_for = None;
                    self.rt.sheet = Some(sheet);
                    self.rt.phase = Phase::Idle;
                }
                Ok(Err(e)) => {
                    self.rt.loading = None;
                    self.rt.file_error = Some(e);
                    self.rt.sheet = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.rt.loading = None,
            }
        }
    }

    fn target_table(&self) -> Option<&TableSchema> {
        let name = self.rt.target.as_ref()?;
        self.rt.tables.iter().find(|t| &t.name == name)
    }

    /// Renders the tool.
    pub fn show(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        let Some(profile) = self.scope.profile(cx).cloned() else {
            error_box(ui, "The connection of this tab no longer exists");
            return;
        };
        let kind = profile.kind;
        if !self.rt.started {
            self.rt.started = true;
            self.rt.describe = super::describe(cx, &self.scope);
        }
        self.poll(kind);
        if self.rt.loading.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        // Statements built on a background thread → send them.
        if let Phase::Building(rx) = &self.rt.phase {
            match rx.try_recv() {
                Ok(Ok(changes)) => {
                    let rows = self.rt.sheet.as_ref().map_or(0, |s| s.rows.len());
                    let id = super::send(
                        cx,
                        &self.scope,
                        |request_id, profile, database, password| Command::ApplyChanges {
                            request_id,
                            profile,
                            database,
                            changes,
                            password,
                        },
                    );
                    self.rt.phase = match id {
                        Some(id) => Phase::Running(id, rows),
                        None => Phase::Idle,
                    };
                }
                Ok(Err(e)) => self.rt.phase = Phase::Failed(e),
                Err(TryRecvError::Empty) => {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(TryRecvError::Disconnected) => self.rt.phase = Phase::Idle,
            }
        }
        if let Phase::Done(_) = self.rt.phase
            && !self.rt.refreshed
        {
            self.rt.refreshed = true;
            cx.actions.push(UiAction::Refresh(profile.id));
            self.rt.describe = super::describe(cx, &self.scope);
        }

        egui::Frame::new()
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                header(
                    ui,
                    icon::DOWNLOAD_SIMPLE,
                    "Import data",
                    &self.scope.label(cx),
                );
                ui.add_space(6.0);
                ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    self.file_section(ui);
                    if let Some(e) = &self.rt.describe_error {
                        error_box(ui, e);
                    }
                    if self.rt.sheet.is_some() {
                        ui.add_space(8.0);
                        self.preview(ui);
                        ui.add_space(8.0);
                        self.target_section(ui, kind);
                        ui.add_space(8.0);
                        self.run_section(ui, cx, kind, &profile);
                    }
                });
            });
    }

    fn file_section(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("1. Source file").strong());
        ui.horizontal(|ui| {
            ui.add(
                TextEdit::singleline(&mut self.rt.path)
                    .hint_text("C:\\data\\customers.csv")
                    .desired_width(420.0),
            );
            if ui
                .button(format!("{}  Browse…", icon::FOLDER_OPEN))
                .clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .add_filter(
                        "Data files",
                        &["csv", "tsv", "txt", "xlsx", "xlsm", "xls", "ods"],
                    )
                    .add_filter("All files", &["*"])
                    .pick_file()
            {
                self.rt.path = p.display().to_string();
                self.rt.options.sheet = None;
                self.load_file();
            }
            if ui
                .add_enabled(
                    !self.rt.path.trim().is_empty() && self.rt.loading.is_none(),
                    Button::new(format!("{}  Load", icon::ARROWS_CLOCKWISE)),
                )
                .clicked()
            {
                self.load_file();
            }
            if self.rt.loading.is_some() {
                ui.spinner();
            }
        });
        let mut reload = false;
        ui.horizontal(|ui| {
            reload |= ui
                .checkbox(
                    &mut self.rt.options.header,
                    "First row contains column names",
                )
                .changed();
            let spreadsheet = import::is_spreadsheet(std::path::Path::new(self.rt.path.trim()));
            if spreadsheet {
                let current = self
                    .rt
                    .options
                    .sheet
                    .clone()
                    .or_else(|| self.rt.sheets.first().cloned())
                    .unwrap_or_default();
                ui.label("Sheet");
                egui::ComboBox::from_id_salt("import-sheet")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for name in self.rt.sheets.clone() {
                            if ui
                                .selectable_label(
                                    self.rt.options.sheet.as_ref() == Some(&name),
                                    &name,
                                )
                                .clicked()
                            {
                                self.rt.options.sheet = Some(name);
                                reload = true;
                            }
                        }
                    });
            } else {
                let label = |d: Option<u8>| match d {
                    None => "Auto",
                    Some(b',') => "Comma",
                    Some(b';') => "Semicolon",
                    Some(b'\t') => "Tab",
                    Some(b'|') => "Pipe",
                    Some(_) => "Other",
                };
                ui.label("Delimiter");
                egui::ComboBox::from_id_salt("import-delimiter")
                    .selected_text(label(self.rt.options.delimiter))
                    .show_ui(ui, |ui| {
                        for d in [None, Some(b','), Some(b';'), Some(b'\t'), Some(b'|')] {
                            if ui
                                .selectable_label(self.rt.options.delimiter == d, label(d))
                                .clicked()
                            {
                                self.rt.options.delimiter = d;
                                reload = true;
                            }
                        }
                    });
            }
        });
        if reload && !self.rt.path.trim().is_empty() {
            self.load_file();
        }
        if let Some(e) = &self.rt.file_error {
            error_box(ui, e);
        }
    }

    fn preview(&self, ui: &mut Ui) {
        let Some(sheet) = &self.rt.sheet else { return };
        ui.label(
            RichText::new(format!(
                "2. Preview  ·  {} column(s), {} row(s)",
                sheet.headers.len(),
                sheet.rows.len()
            ))
            .strong(),
        );
        ScrollArea::horizontal()
            .id_salt("import-preview")
            .max_height(200.0)
            .show(ui, |ui| {
                Grid::new("import-preview-grid")
                    .striped(true)
                    .spacing([14.0, 2.0])
                    .show(ui, |ui| {
                        for h in &sheet.headers {
                            ui.label(RichText::new(h).strong().color(color::accent()));
                        }
                        ui.end_row();
                        for row in sheet.rows.iter().take(15) {
                            for cell in row {
                                let shown: String = cell.chars().take(40).collect();
                                ui.label(if cell.is_empty() {
                                    RichText::new("∅").color(color::text_faint())
                                } else {
                                    RichText::new(shown)
                                });
                            }
                            ui.end_row();
                        }
                    });
            });
    }

    fn target_section(&mut self, ui: &mut Ui, kind: DatabaseKind) {
        let Some(sheet) = self.rt.sheet.clone() else {
            return;
        };
        ui.label(RichText::new("3. Target").strong());
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.rt.new_table, false, "Existing table");
            ui.add_enabled_ui(!self.rt.new_table, |ui| {
                egui::ComboBox::from_id_salt("import-target")
                    .width(220.0)
                    .selected_text(self.rt.target.clone().unwrap_or_else(|| "choose…".into()))
                    .show_ui(ui, |ui| {
                        for t in &self.rt.tables {
                            if ui
                                .selectable_label(self.rt.target.as_ref() == Some(&t.name), &t.name)
                                .clicked()
                            {
                                self.rt.target = Some(t.name.clone());
                            }
                        }
                    });
            });
            if self.rt.describe.is_some() {
                ui.spinner();
            }
            ui.separator();
            ui.radio_value(&mut self.rt.new_table, true, "New table");
            ui.add_enabled(
                self.rt.new_table,
                TextEdit::singleline(&mut self.rt.new_def.name).desired_width(200.0),
            );
        });
        ui.add_space(4.0);
        if self.rt.new_table {
            Grid::new("import-new-cols")
                .striped(true)
                .spacing([12.0, 3.0])
                .show(ui, |ui| {
                    for h in ["Import", "Source column", "Column name", "Type"] {
                        ui.label(RichText::new(h).strong().color(color::text_dim()));
                    }
                    ui.end_row();
                    let types = rdbs_core::ddlgen::common_types(kind);
                    for (i, h) in sheet.headers.iter().enumerate() {
                        if let (Some(include), Some(col)) = (
                            self.rt.include.get_mut(i),
                            self.rt.new_def.columns.get_mut(i),
                        ) {
                            ui.checkbox(include, "");
                            ui.label(h);
                            ui.add_sized(
                                [180.0, ui.spacing().interact_size.y],
                                TextEdit::singleline(&mut col.name),
                            );
                            ui.horizontal(|ui| {
                                ui.add(
                                    TextEdit::singleline(&mut col.data_type).desired_width(150.0),
                                );
                                ui.menu_button(icon::CARET_DOWN, |ui| {
                                    for t in types {
                                        if ui.button(*t).clicked() {
                                            col.data_type = (*t).to_owned();
                                            ui.close();
                                        }
                                    }
                                });
                            });
                            ui.end_row();
                        }
                    }
                });
        } else if let Some(target) = self.target_table().cloned() {
            if self.rt.mapped_for.as_ref() != Some(&target.name) {
                self.rt.mapping = auto_map(&target, &sheet);
                self.rt.mapped_for = Some(target.name.clone());
            }
            Grid::new("import-map")
                .striped(true)
                .spacing([12.0, 3.0])
                .show(ui, |ui| {
                    for h in ["Target column", "Type", "", "Source column"] {
                        ui.label(RichText::new(h).strong().color(color::text_dim()));
                    }
                    ui.end_row();
                    for (i, c) in target.columns.iter().enumerate() {
                        ui.label(&c.name);
                        ui.label(RichText::new(&c.data_type).color(color::text_dim()));
                        ui.label(icon::ARROW_LEFT);
                        let current = self.rt.mapping.get(i).copied().flatten();
                        egui::ComboBox::from_id_salt(("import-map", i))
                            .width(200.0)
                            .selected_text(
                                current
                                    .and_then(|s| sheet.headers.get(s).cloned())
                                    .unwrap_or_else(|| "(skip)".into()),
                            )
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(current.is_none(), "(skip)").clicked()
                                    && let Some(m) = self.rt.mapping.get_mut(i)
                                {
                                    *m = None;
                                }
                                for (s, h) in sheet.headers.iter().enumerate() {
                                    if ui.selectable_label(current == Some(s), h).clicked()
                                        && let Some(m) = self.rt.mapping.get_mut(i)
                                    {
                                        *m = Some(s);
                                    }
                                }
                            });
                        ui.end_row();
                    }
                });
        }
    }

    /// Changes for the current settings, or why they cannot be built.
    fn plan(&self, kind: DatabaseKind) -> Result<Plan, String> {
        let sheet = self.rt.sheet.as_ref().ok_or("Load a file first")?;
        let mut prefix = Vec::new();
        let (table, mapping) = if self.rt.new_table {
            let mut def = self.rt.new_def.clone();
            let mut mapping = Vec::new();
            let mut columns = Vec::new();
            for (i, col) in def.columns.iter().enumerate() {
                if self.rt.include.get(i).copied().unwrap_or(false) {
                    columns.push(col.clone());
                    mapping.push(Some(i));
                }
            }
            def.columns = columns;
            if def.name.trim().is_empty() || def.columns.is_empty() {
                return Err("Name the new table and include at least one column".into());
            }
            if self
                .rt
                .tables
                .iter()
                .any(|t| t.name.eq_ignore_ascii_case(&def.name))
            {
                return Err(format!("A table named {} already exists", def.name));
            }
            prefix.extend(
                rdbs_core::ddlgen::create_table(kind, &def)
                    .into_iter()
                    .map(Change::unchecked),
            );
            (def, mapping)
        } else {
            let target = self
                .target_table()
                .ok_or("Choose the target table")?
                .clone();
            if self.rt.mapping.iter().all(Option::is_none) {
                return Err("Map at least one column".into());
            }
            if self.rt.replace {
                let name = qualified_name(kind, None, target.schema.as_deref(), &target.name);
                prefix.push(Change::unchecked(format!("DELETE FROM {name}")));
            }
            (target, self.rt.mapping.clone())
        };
        let _ = sheet;
        Ok((table, mapping, prefix))
    }

    fn run_section(
        &mut self,
        ui: &mut Ui,
        cx: &mut TabContext<'_>,
        kind: DatabaseKind,
        profile: &rdbs_core::ConnectionProfile,
    ) {
        ui.label(RichText::new("4. Options").strong());
        ui.checkbox(&mut self.rt.empty_is_null, "Empty cells become NULL");
        if !self.rt.new_table {
            let tint = if self.rt.replace {
                color::warning()
            } else {
                color::text()
            };
            ui.checkbox(
                &mut self.rt.replace,
                RichText::new("Delete all existing rows first").color(tint),
            );
        }
        ui.add_space(6.0);
        let plan = self.plan(kind);
        let busy = matches!(self.rt.phase, Phase::Building(_) | Phase::Running(..));
        ui.horizontal(|ui| {
            let button = Button::new(
                RichText::new(format!("{}  Import", icon::DOWNLOAD_SIMPLE)).color(color::success()),
            );
            let response = ui
                .add_enabled(plan.is_ok() && !busy, button)
                .on_disabled_hover_text(plan.as_ref().err().cloned().unwrap_or_default());
            if response.clicked() {
                if profile.environment.requires_write_confirmation() || self.rt.replace {
                    self.rt.confirm = true;
                } else {
                    self.start(kind);
                }
            }
            if let Err(e) = &plan {
                ui.label(RichText::new(e).color(color::text_dim()));
            }
            if busy {
                ui.spinner();
                ui.label("Importing…");
            }
        });
        if self.rt.confirm {
            let rows = self.rt.sheet.as_ref().map_or(0, |s| s.rows.len());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{}  Import {rows} row(s) into {}{}?",
                        icon::WARNING,
                        profile.name,
                        if self.rt.replace {
                            " after deleting all existing rows"
                        } else {
                            ""
                        }
                    ))
                    .color(color::warning()),
                );
                if ui
                    .button(RichText::new("Import").color(color::danger()))
                    .clicked()
                {
                    self.rt.confirm = false;
                    self.start(kind);
                }
                if ui.button("Cancel").clicked() {
                    self.rt.confirm = false;
                }
            });
        }
        match &self.rt.phase {
            Phase::Done(msg) => {
                ui.label(
                    RichText::new(format!("{}  {msg}", icon::CHECK_CIRCLE)).color(color::success()),
                );
            }
            Phase::Failed(e) => error_box(ui, e),
            _ => {}
        }
        let _ = cx;
    }

    fn start(&mut self, kind: DatabaseKind) {
        let Ok((table, mapping, prefix)) = self.plan(kind) else {
            return;
        };
        let Some(sheet) = self.rt.sheet.clone() else {
            return;
        };
        let options = ImportOptions {
            empty_is_null: self.rt.empty_is_null,
            ..ImportOptions::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = build_inserts(kind, &table, &sheet, &mapping, &options)
                .map(|inserts| prefix.into_iter().chain(inserts).collect())
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        self.rt.refreshed = false;
        self.rt.phase = Phase::Building(rx);
    }
}
