//! Table designer: edit columns, primary key, indexes and foreign keys of a new or existing
//! table, preview the generated `CREATE`/`ALTER` script and apply it.

use egui::{Button, Grid, RichText, ScrollArea, TextEdit, Ui};
use egui_phosphor::regular as icon;
use rdbs_core::ddlgen::{self, alter_table, create_table, rename_table};
use rdbs_core::{ColumnDef, DatabaseKind, Event, ForeignKeyDef, IndexDef, RequestId, TableSchema};
use serde::{Deserialize, Serialize};

use super::{Scope, error_box, error_text, header};
use crate::model::{StatusMessage, UiAction};
use crate::theme::{self, color};
use crate::workspace::TabContext;

/// Table designer state. Only the target is persisted; the definition is reloaded.
#[derive(Serialize, Deserialize)]
pub struct Designer {
    /// Where the table lives.
    pub scope: Scope,
    /// Existing table being edited (`None` while designing a new one).
    pub table: Option<String>,
    #[serde(skip)]
    rt: Runtime,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Section {
    #[default]
    Fields,
    Indexes,
    ForeignKeys,
    Sql,
}

#[derive(Default)]
struct Runtime {
    started: bool,
    describe: Option<RequestId>,
    save: Option<RequestId>,
    /// Definition as loaded from the server (`None` for a new table).
    loaded: Option<TableSchema>,
    draft: TableSchema,
    /// Name each draft column had when loaded (`None` = added in the designer).
    originals: Vec<Option<String>>,
    /// Tables of the scope, offered as foreign key targets.
    tables: Vec<TableSchema>,
    section: Section,
    selected: Option<usize>,
    error: Option<String>,
    notice: Option<String>,
    confirm: bool,
    /// Reload after a successful save (needs the tab context, so done in `show`).
    reload: bool,
    refresh_explorer: bool,
}

impl Designer {
    /// Designer for `table`, or for a new table when `None`.
    pub fn new(scope: Scope, table: Option<String>) -> Self {
        Self {
            scope,
            table,
            rt: Runtime::default(),
        }
    }

    /// Tab title.
    pub fn title(&self) -> String {
        match &self.table {
            Some(t) => format!("Design {t}"),
            None => "New table".to_owned(),
        }
    }

    /// Consumes the events of this designer's requests.
    pub fn on_event(&mut self, event: &Event) -> bool {
        match event {
            Event::SchemaDescribed { request_id, result }
                if self.rt.describe == Some(*request_id) =>
            {
                self.rt.describe = None;
                match result {
                    Ok(tables) => {
                        self.rt.tables = tables.clone();
                        if let Some(name) = &self.table {
                            match tables.iter().find(|t| &t.name == name) {
                                Some(t) => self.load(t.clone()),
                                None => {
                                    self.rt.error = Some(format!("Table {name} was not found"));
                                }
                            }
                        }
                    }
                    Err(e) => self.rt.error = Some(error_text(e)),
                }
                true
            }
            Event::ScriptFinished { request_id, result } if self.rt.save == Some(*request_id) => {
                self.rt.save = None;
                match result {
                    Ok(n) => {
                        self.rt.error = None;
                        self.rt.notice = Some(format!("Saved ({n} statement(s) executed)"));
                        self.table = Some(self.rt.draft.name.clone());
                        self.rt.reload = true;
                    }
                    Err(e) => {
                        self.rt.error = Some(format!(
                            "{}\nStatements before the failing one may already be applied; \
                             use Reload to see the current definition.",
                            error_text(e)
                        ));
                    }
                }
                self.rt.refresh_explorer = true;
                true
            }
            _ => false,
        }
    }

    fn load(&mut self, table: TableSchema) {
        self.rt.originals = table.columns.iter().map(|c| Some(c.name.clone())).collect();
        self.rt.draft = table.clone();
        self.rt.loaded = Some(table);
        self.rt.error = None;
        self.rt.selected = None;
    }

    fn start(&mut self, cx: &mut TabContext<'_>, kind: DatabaseKind) {
        self.rt.started = true;
        if self.table.is_none() && self.rt.draft.columns.is_empty() {
            self.rt.draft = TableSchema {
                schema: self.scope.describe_schema(kind),
                name: "new_table".into(),
                columns: vec![ColumnDef {
                    nullable: false,
                    identity: true,
                    ..ColumnDef::new("id", ddlgen::common_types(kind)[0])
                }],
                primary_key: vec!["id".into()],
                ..TableSchema::default()
            };
            self.rt.originals = vec![None];
        }
        self.rt.describe = super::describe(cx, &self.scope);
    }

    /// Renames applied to existing columns.
    fn renames(&self) -> Vec<(String, String)> {
        self.rt
            .originals
            .iter()
            .zip(&self.rt.draft.columns)
            .filter_map(|(o, c)| {
                o.as_ref()
                    .filter(|o| **o != c.name)
                    .map(|o| (o.clone(), c.name.clone()))
            })
            .collect()
    }

    /// The script that turns the loaded definition into the draft.
    fn statements(&self, kind: DatabaseKind) -> Result<Vec<String>, String> {
        validate(&self.rt.draft)?;
        let Some(old) = &self.rt.loaded else {
            return Ok(create_table(kind, &self.rt.draft));
        };
        let mut out = Vec::new();
        let mut base = old.clone();
        if old.name != self.rt.draft.name {
            out.push(rename_table(kind, old, &self.rt.draft.name));
            base.name = self.rt.draft.name.clone();
        }
        out.extend(alter_table(kind, &base, &self.rt.draft, &self.renames()).map_err(|e| e.0)?);
        Ok(out)
    }

    /// Renders the designer.
    pub fn show(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        let Some(profile) = self.scope.profile(cx).cloned() else {
            error_box(ui, "The connection of this tab no longer exists");
            return;
        };
        let kind = profile.kind;
        if !self.rt.started {
            self.start(cx, kind);
        }
        if std::mem::take(&mut self.rt.reload) {
            self.rt.describe = super::describe(cx, &self.scope);
        }
        if std::mem::take(&mut self.rt.refresh_explorer) {
            let other_db = kind.has_schemas()
                && self
                    .scope
                    .database
                    .as_ref()
                    .is_some_and(|d| *d != profile.database && !profile.database.is_empty());
            cx.actions.push(match (&self.scope.database, other_db) {
                (Some(db), true) => UiAction::LoadDatabase {
                    profile_id: profile.id,
                    database: db.clone(),
                },
                _ => UiAction::Refresh(profile.id),
            });
        }

        egui::Frame::new()
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                let title = if self.rt.loaded.is_some() {
                    "Table designer"
                } else {
                    "New table"
                };
                header(ui, icon::WRENCH, title, &self.scope.label(cx));
                ui.add_space(6.0);
                self.toolbar(ui, cx, kind, &profile);
                ui.add_space(4.0);
                if let Some(e) = &self.rt.error {
                    error_box(ui, e);
                    ui.add_space(4.0);
                }
                if let Some(n) = &self.rt.notice {
                    ui.label(
                        RichText::new(format!("{}  {n}", icon::CHECK_CIRCLE)).color(color::SUCCESS),
                    );
                }
                if self.rt.describe.is_some() && self.table.is_some() && self.rt.loaded.is_none() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading table definition…");
                    });
                    return;
                }
                ui.horizontal(|ui| {
                    for (section, label) in [
                        (Section::Fields, format!("{}  Fields", icon::COLUMNS)),
                        (Section::Indexes, format!("{}  Indexes", icon::LIST_NUMBERS)),
                        (
                            Section::ForeignKeys,
                            format!("{}  Foreign Keys", icon::LINK),
                        ),
                        (Section::Sql, format!("{}  SQL Preview", icon::CODE)),
                    ] {
                        ui.selectable_value(&mut self.rt.section, section, label);
                    }
                });
                ui.separator();
                match self.rt.section {
                    Section::Fields => self.fields(ui, kind),
                    Section::Indexes => self.indexes(ui, kind),
                    Section::ForeignKeys => self.foreign_keys(ui),
                    Section::Sql => self.sql_preview(ui, kind),
                }
            });
    }

    fn toolbar(
        &mut self,
        ui: &mut Ui,
        cx: &mut TabContext<'_>,
        kind: DatabaseKind,
        profile: &rdbs_core::ConnectionProfile,
    ) {
        ui.horizontal(|ui| {
            ui.label("Table name");
            ui.add(TextEdit::singleline(&mut self.rt.draft.name).desired_width(220.0));
            ui.separator();
            let busy = self.rt.save.is_some() || self.rt.describe.is_some();
            let statements = self.statements(kind);
            let has_changes = statements.as_ref().is_ok_and(|s| !s.is_empty());
            let save = Button::new(
                RichText::new(format!("{}  Save", icon::FLOPPY_DISK)).color(color::SUCCESS),
            );
            let response = ui
                .add_enabled(!busy && has_changes, save)
                .on_disabled_hover_text(match &statements {
                    Err(e) => e.clone(),
                    Ok(s) if s.is_empty() => "No changes".to_owned(),
                    Ok(_) => "Busy…".to_owned(),
                });
            if response.clicked()
                && let Ok(statements) = statements.clone()
            {
                if profile.environment.requires_write_confirmation() {
                    self.rt.confirm = true;
                } else {
                    self.apply(cx, statements);
                }
            }
            if ui
                .add_enabled(
                    !busy && self.table.is_some(),
                    Button::new(format!("{}  Reload", icon::ARROWS_CLOCKWISE)),
                )
                .on_hover_text("Discard changes and reload the definition from the server")
                .clicked()
            {
                self.rt.notice = None;
                self.rt.error = None;
                self.rt.describe = super::describe(cx, &self.scope);
            }
            if self.rt.save.is_some() {
                ui.spinner();
                ui.label("Applying…");
            }
            if let Err(problem) = &statements {
                ui.label(
                    RichText::new(format!("{}  {problem}", icon::WARNING)).color(color::WARNING),
                );
            }
        });
        if self.rt.confirm {
            let statements = self.statements(kind).unwrap_or_default();
            egui::Frame::new()
                .fill(color::WARNING.gamma_multiply(0.1))
                .stroke(egui::Stroke::new(1.0, color::WARNING.gamma_multiply(0.5)))
                .corner_radius(6)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{}  {} is a production connection. Apply {} statement(s)?",
                            icon::WARNING,
                            profile.name,
                            statements.len()
                        ))
                        .color(color::WARNING),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .button(RichText::new("Apply changes").color(color::DANGER))
                            .clicked()
                        {
                            self.rt.confirm = false;
                            self.apply(cx, statements.clone());
                        }
                        if ui.button("Cancel").clicked() {
                            self.rt.confirm = false;
                        }
                    });
                });
        }
    }

    fn apply(&mut self, cx: &mut TabContext<'_>, statements: Vec<String>) {
        self.rt.notice = None;
        self.rt.error = None;
        self.rt.save = super::run_script(cx, &self.scope, statements);
        if self.rt.save.is_some() {
            cx.actions
                .push(UiAction::Status(StatusMessage::info(format!(
                    "Applying changes to {}…",
                    self.rt.draft.name
                ))));
        }
    }

    fn fields(&mut self, ui: &mut Ui, kind: DatabaseKind) {
        let count = self.rt.draft.columns.len();
        ui.horizontal(|ui| {
            if ui.button(format!("{}  Add Field", icon::PLUS)).clicked() {
                self.insert_column(count, kind);
            }
            let sel = self.rt.selected.filter(|s| *s < count);
            if ui
                .add_enabled(
                    sel.is_some(),
                    Button::new(format!("{}  Insert", icon::ROWS_PLUS_TOP)),
                )
                .clicked()
                && let Some(i) = sel
            {
                self.insert_column(i, kind);
            }
            if ui
                .add_enabled(
                    sel.is_some(),
                    Button::new(format!("{}  Delete", icon::TRASH)),
                )
                .clicked()
                && let Some(i) = sel
            {
                self.delete_column(i);
            }
            if ui
                .add_enabled(sel.is_some_and(|i| i > 0), Button::new(icon::ARROW_UP))
                .on_hover_text("Move up")
                .clicked()
                && let Some(i) = sel
            {
                self.move_column(i, i - 1);
            }
            if ui
                .add_enabled(
                    sel.is_some_and(|i| i + 1 < count),
                    Button::new(icon::ARROW_DOWN),
                )
                .on_hover_text("Move down")
                .clicked()
                && let Some(i) = sel
            {
                self.move_column(i, i + 1);
            }
            if self.rt.loaded.is_some() {
                ui.label(
                    RichText::new("Column order of existing tables is kept by most engines")
                        .small()
                        .color(color::TEXT_FAINT),
                );
            }
        });
        ui.add_space(4.0);
        let types = ddlgen::common_types(kind);
        let mut renamed: Option<(String, String)> = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            Grid::new("designer-fields")
                .striped(true)
                .num_columns(7)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    for h in ["#", "Name", "Type", "Not null", "Key", "Auto", "Default"] {
                        ui.label(RichText::new(h).strong().color(color::TEXT_DIM));
                    }
                    ui.end_row();
                    let draft = &mut self.rt.draft;
                    for i in 0..draft.columns.len() {
                        let selected = self.rt.selected == Some(i);
                        if ui
                            .selectable_label(selected, format!("{}", i + 1))
                            .clicked()
                        {
                            self.rt.selected = Some(i);
                        }
                        let before = draft.columns[i].name.clone();
                        let response = ui.add_sized(
                            [170.0, ui.spacing().interact_size.y],
                            TextEdit::singleline(&mut draft.columns[i].name),
                        );
                        if response.gained_focus() {
                            self.rt.selected = Some(i);
                        }
                        if draft.columns[i].name != before {
                            renamed = Some((before, draft.columns[i].name.clone()));
                        }
                        ui.horizontal(|ui| {
                            ui.add(
                                TextEdit::singleline(&mut draft.columns[i].data_type)
                                    .desired_width(150.0),
                            );
                            ui.menu_button(icon::CARET_DOWN, |ui| {
                                for t in types {
                                    if ui.button(*t).clicked() {
                                        draft.columns[i].data_type = (*t).to_owned();
                                        ui.close();
                                    }
                                }
                            });
                        });
                        let mut not_null = !draft.columns[i].nullable;
                        if ui.checkbox(&mut not_null, "").changed() {
                            draft.columns[i].nullable = !not_null;
                        }
                        let name = draft.columns[i].name.clone();
                        let position = draft.primary_key.iter().position(|k| *k == name);
                        let mut in_key = position.is_some();
                        let label = position.map_or(String::new(), |p| {
                            if draft.primary_key.len() > 1 {
                                format!("{}", p + 1)
                            } else {
                                icon::KEY.to_owned()
                            }
                        });
                        if ui.checkbox(&mut in_key, label).changed() {
                            if in_key {
                                draft.primary_key.push(name.clone());
                                draft.columns[i].nullable = false;
                            } else {
                                draft.primary_key.retain(|k| *k != name);
                            }
                        }
                        ui.checkbox(&mut draft.columns[i].identity, "")
                            .on_hover_text("Auto-increment / identity");
                        let mut default = draft.columns[i].default.clone().unwrap_or_default();
                        if ui
                            .add(
                                TextEdit::singleline(&mut default)
                                    .hint_text("none")
                                    .desired_width(160.0),
                            )
                            .on_hover_text("SQL expression, e.g. 0, 'text', CURRENT_TIMESTAMP")
                            .changed()
                        {
                            draft.columns[i].default =
                                (!default.trim().is_empty()).then_some(default);
                        }
                        ui.end_row();
                    }
                });
        });
        if let Some((from, to)) = renamed {
            rename_references(&mut self.rt.draft, &from, &to);
        }
    }

    fn insert_column(&mut self, at: usize, kind: DatabaseKind) {
        let mut n = self.rt.draft.columns.len() + 1;
        let name = loop {
            let candidate = format!("column{n}");
            if self.rt.draft.column(&candidate).is_none() {
                break candidate;
            }
            n += 1;
        };
        let data_type = match kind {
            DatabaseKind::Sqlite => "TEXT",
            DatabaseKind::SqlServer => "nvarchar(255)",
            _ => "varchar(255)",
        };
        self.rt
            .draft
            .columns
            .insert(at, ColumnDef::new(name, data_type));
        self.rt.originals.insert(at, None);
        self.rt.selected = Some(at);
    }

    fn delete_column(&mut self, i: usize) {
        let removed = self.rt.draft.columns.remove(i);
        self.rt.originals.remove(i);
        let draft = &mut self.rt.draft;
        draft.primary_key.retain(|k| *k != removed.name);
        for ix in &mut draft.indexes {
            ix.columns.retain(|c| *c != removed.name);
        }
        draft.indexes.retain(|ix| !ix.columns.is_empty());
        draft
            .foreign_keys
            .retain(|fk| !fk.columns.contains(&removed.name));
        self.rt.selected = None;
    }

    fn move_column(&mut self, from: usize, to: usize) {
        self.rt.draft.columns.swap(from, to);
        self.rt.originals.swap(from, to);
        self.rt.selected = Some(to);
    }

    fn indexes(&mut self, ui: &mut Ui, kind: DatabaseKind) {
        let table = self.rt.draft.name.clone();
        let columns: Vec<String> = self
            .rt
            .draft
            .columns
            .iter()
            .map(|c| c.name.clone())
            .collect();
        if ui.button(format!("{}  Add Index", icon::PLUS)).clicked() {
            let n = self.rt.draft.indexes.len() + 1;
            self.rt.draft.indexes.push(IndexDef::new(
                format!("ix_{table}_{n}"),
                columns.first().cloned().into_iter().collect(),
                false,
            ));
        }
        ui.add_space(4.0);
        let mut remove = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            Grid::new("designer-indexes")
                .striped(true)
                .num_columns(5)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    for h in ["Name", "Columns", "Unique", "Constraint", ""] {
                        ui.label(RichText::new(h).strong().color(color::TEXT_DIM));
                    }
                    ui.end_row();
                    for (i, ix) in self.rt.draft.indexes.iter_mut().enumerate() {
                        ui.add_sized(
                            [200.0, ui.spacing().interact_size.y],
                            TextEdit::singleline(&mut ix.name),
                        );
                        column_list_editor(
                            ui,
                            egui::Id::new(("ix-cols", i)),
                            &mut ix.columns,
                            &columns,
                        );
                        ui.checkbox(&mut ix.unique, "");
                        ui.add_enabled_ui(ix.unique, |ui| {
                            ui.checkbox(&mut ix.constraint, "").on_hover_text(
                                "Create as a UNIQUE constraint instead of a unique index",
                            );
                        });
                        if ui.button(icon::TRASH).on_hover_text("Remove").clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                        if !ix.unique {
                            ix.constraint = false;
                        }
                    }
                });
        });
        if let Some(i) = remove {
            self.rt.draft.indexes.remove(i);
        }
        if kind == DatabaseKind::Sqlite {
            ui.label(
                RichText::new("SQLite rebuilds the table to change unique constraints.")
                    .small()
                    .color(color::TEXT_FAINT),
            );
        }
    }

    fn foreign_keys(&mut self, ui: &mut Ui) {
        let table = self.rt.draft.name.clone();
        let columns: Vec<String> = self
            .rt
            .draft
            .columns
            .iter()
            .map(|c| c.name.clone())
            .collect();
        let mut targets: Vec<(String, Vec<String>)> = self
            .rt
            .tables
            .iter()
            .filter(|t| t.name != table)
            .map(|t| {
                (
                    t.name.clone(),
                    t.columns.iter().map(|c| c.name.clone()).collect(),
                )
            })
            .collect();
        targets.push((table.clone(), columns.clone()));
        if ui
            .button(format!("{}  Add Foreign Key", icon::PLUS))
            .clicked()
        {
            let n = self.rt.draft.foreign_keys.len() + 1;
            self.rt.draft.foreign_keys.push(ForeignKeyDef {
                name: format!("fk_{table}_{n}"),
                columns: Vec::new(),
                ref_schema: None,
                ref_table: String::new(),
                ref_columns: Vec::new(),
            });
        }
        ui.add_space(4.0);
        let mut remove = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            Grid::new("designer-fks")
                .striped(true)
                .num_columns(5)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    for h in [
                        "Name",
                        "Columns",
                        "References table",
                        "Referenced columns",
                        "",
                    ] {
                        ui.label(RichText::new(h).strong().color(color::TEXT_DIM));
                    }
                    ui.end_row();
                    for (i, fk) in self.rt.draft.foreign_keys.iter_mut().enumerate() {
                        ui.add_sized(
                            [180.0, ui.spacing().interact_size.y],
                            TextEdit::singleline(&mut fk.name),
                        );
                        column_list_editor(
                            ui,
                            egui::Id::new(("fk-cols", i)),
                            &mut fk.columns,
                            &columns,
                        );
                        egui::ComboBox::from_id_salt(("fk-ref", i))
                            .width(170.0)
                            .selected_text(if fk.ref_table.is_empty() {
                                "choose…".to_owned()
                            } else {
                                fk.ref_table.clone()
                            })
                            .show_ui(ui, |ui| {
                                for (name, _) in &targets {
                                    if ui.selectable_label(fk.ref_table == *name, name).clicked() {
                                        fk.ref_table = name.clone();
                                        fk.ref_columns.clear();
                                    }
                                }
                            });
                        let ref_cols = targets
                            .iter()
                            .find(|(n, _)| *n == fk.ref_table)
                            .map(|(_, c)| c.clone())
                            .unwrap_or_default();
                        column_list_editor(
                            ui,
                            egui::Id::new(("fk-ref-cols", i)),
                            &mut fk.ref_columns,
                            &ref_cols,
                        );
                        if ui.button(icon::TRASH).on_hover_text("Remove").clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
        });
        if let Some(i) = remove {
            self.rt.draft.foreign_keys.remove(i);
        }
    }

    fn sql_preview(&self, ui: &mut Ui, kind: DatabaseKind) {
        let text = match self.statements(kind) {
            Ok(s) if s.is_empty() => "-- No changes".to_owned(),
            Ok(s) => ddlgen::script(&s),
            Err(e) => format!("-- {e}"),
        };
        ui.horizontal(|ui| {
            if ui.button(format!("{}  Copy", icon::COPY)).clicked() {
                ui.ctx().copy_text(text.clone());
            }
        });
        let mut shown = text.as_str();
        ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.add(
                TextEdit::multiline(&mut shown)
                    .font(theme::mono())
                    .desired_width(f32::INFINITY),
            );
        });
    }
}

/// Edits an ordered list of column names: chips with remove buttons plus an "add" menu.
fn column_list_editor(ui: &mut Ui, id: egui::Id, list: &mut Vec<String>, available: &[String]) {
    ui.push_id(id, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.set_max_width(260.0);
            let mut remove = None;
            for (i, c) in list.iter().enumerate() {
                if ui
                    .small_button(format!("{c} {}", icon::X))
                    .on_hover_text("Remove")
                    .clicked()
                {
                    remove = Some(i);
                }
            }
            if let Some(i) = remove {
                list.remove(i);
            }
            ui.menu_button(icon::PLUS, |ui| {
                let missing: Vec<&String> =
                    available.iter().filter(|c| !list.contains(c)).collect();
                for c in missing {
                    if ui.button(c).clicked() {
                        list.push(c.clone());
                        ui.close();
                    }
                }
            });
        });
    });
}

/// Keeps key, index and foreign key column lists in step with a column rename.
fn rename_references(t: &mut TableSchema, from: &str, to: &str) {
    let rename = |cols: &mut Vec<String>| {
        for c in cols.iter_mut() {
            if c == from {
                *c = to.to_owned();
            }
        }
    };
    rename(&mut t.primary_key);
    for ix in &mut t.indexes {
        rename(&mut ix.columns);
    }
    for fk in &mut t.foreign_keys {
        rename(&mut fk.columns);
        if fk.ref_table == t.name {
            rename(&mut fk.ref_columns);
        }
    }
}

/// Problems that would make the generated DDL fail.
fn validate(t: &TableSchema) -> Result<(), String> {
    if t.name.trim().is_empty() {
        return Err("The table needs a name".into());
    }
    if t.columns.is_empty() {
        return Err("The table needs at least one field".into());
    }
    let mut seen = std::collections::HashSet::new();
    for c in &t.columns {
        if c.name.trim().is_empty() {
            return Err("Every field needs a name".into());
        }
        if !seen.insert(c.name.to_lowercase()) {
            return Err(format!("Field {} appears twice", c.name));
        }
        if c.data_type.trim().is_empty() {
            return Err(format!("Field {} needs a type", c.name));
        }
    }
    for ix in &t.indexes {
        if ix.name.trim().is_empty() || ix.columns.is_empty() {
            return Err("Every index needs a name and at least one column".into());
        }
    }
    for fk in &t.foreign_keys {
        if fk.name.trim().is_empty() || fk.columns.is_empty() || fk.ref_table.is_empty() {
            return Err("Every foreign key needs a name, columns and a referenced table".into());
        }
        if fk.columns.len() != fk.ref_columns.len() {
            return Err(format!(
                "Foreign key {} needs as many referenced columns as columns",
                fk.name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdbs_core::ProfileId;

    fn designer_with(table: TableSchema) -> Designer {
        let mut d = Designer::new(
            Scope {
                profile_id: ProfileId::new(),
                database: None,
                schema: Some("public".into()),
            },
            Some(table.name.clone()),
        );
        d.load(table);
        d
    }

    fn sample() -> TableSchema {
        TableSchema {
            schema: Some("public".into()),
            name: "items".into(),
            columns: vec![
                ColumnDef {
                    nullable: false,
                    ..ColumnDef::new("id", "integer")
                },
                ColumnDef::new("label", "text"),
            ],
            primary_key: vec!["id".into()],
            primary_key_name: Some("items_pkey".into()),
            indexes: vec![IndexDef::new("items_label", vec!["label".into()], false)],
            foreign_keys: Vec::new(),
        }
    }

    #[test]
    fn unchanged_table_has_no_statements() {
        let d = designer_with(sample());
        assert_eq!(
            d.statements(DatabaseKind::PostgreSql).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn renames_are_tracked_and_references_follow() {
        let mut d = designer_with(sample());
        d.rt.draft.columns[1].name = "title".into();
        rename_references(&mut d.rt.draft, "label", "title");
        assert_eq!(d.rt.draft.indexes[0].columns, ["title"]);
        let sql = d.statements(DatabaseKind::PostgreSql).unwrap();
        assert_eq!(
            sql,
            ["ALTER TABLE \"public\".\"items\" RENAME COLUMN \"label\" TO \"title\""]
        );
    }

    #[test]
    fn table_rename_comes_first() {
        let mut d = designer_with(sample());
        d.rt.draft.name = "products".into();
        d.rt.draft
            .columns
            .push(ColumnDef::new("price", "numeric(10,2)"));
        let sql = d.statements(DatabaseKind::PostgreSql).unwrap();
        assert_eq!(
            sql[0],
            "ALTER TABLE \"public\".\"items\" RENAME TO \"products\""
        );
        assert_eq!(
            sql[1],
            "ALTER TABLE \"public\".\"products\" ADD COLUMN \"price\" numeric(10,2)"
        );
    }

    #[test]
    fn deleting_a_column_cleans_up_keys() {
        let mut d = designer_with(sample());
        d.delete_column(1);
        assert!(d.rt.draft.indexes.is_empty());
        let sql = d.statements(DatabaseKind::PostgreSql).unwrap();
        assert!(sql.iter().any(|s| s.contains("DROP INDEX")));
        assert!(sql.iter().any(|s| s.contains("DROP COLUMN \"label\"")));
    }

    #[test]
    fn validation_catches_duplicates_and_bad_keys() {
        let mut t = sample();
        t.columns[1].name = "ID".into();
        assert!(validate(&t).unwrap_err().contains("twice"));
        let mut t = sample();
        t.foreign_keys.push(ForeignKeyDef {
            name: "fk".into(),
            columns: vec!["id".into()],
            ref_schema: None,
            ref_table: "other".into(),
            ref_columns: Vec::new(),
        });
        assert!(validate(&t).is_err());
    }
}
