//! Visual query builder: pick tables (joins follow foreign keys), tick columns, add
//! aggregates, sorting and conditions; the SQL is generated live and can be opened or run.

use egui::{Button, ComboBox, Grid, RichText, ScrollArea, TextEdit, Ui};
use egui_phosphor::regular as icon;
use rdbs_core::builder::{Aggregate, ColRef, Filter, Join, JoinKind, Operator, QueryModel, Sort};
use rdbs_core::{DatabaseKind, Event, RequestId, TableSchema};
use serde::{Deserialize, Serialize};

use super::{Scope, error_box, error_text, header};
use crate::model::UiAction;
use crate::theme::{self, color};
use crate::workspace::TabContext;

/// Builder state; the query model is persisted.
#[derive(Serialize, Deserialize)]
pub struct Builder {
    /// Database/schema the query runs in.
    pub scope: Scope,
    /// The query being built.
    #[serde(default)]
    pub model: QueryModel,
    #[serde(skip)]
    rt: Runtime,
}

#[derive(Default)]
struct Runtime {
    started: bool,
    describe: Option<RequestId>,
    tables: Vec<TableSchema>,
    filter: String,
    error: Option<String>,
}

fn label(c: &ColRef) -> String {
    format!("{}.{}", c.table, c.column)
}

/// Combo box choosing one of `options`.
fn col_combo(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    value: &mut ColRef,
    options: &[ColRef],
) {
    ComboBox::from_id_salt(id)
        .width(180.0)
        .selected_text(label(value))
        .show_ui(ui, |ui| {
            for o in options {
                if ui.selectable_label(o == value, label(o)).clicked() {
                    *value = o.clone();
                }
            }
        });
}

impl Builder {
    /// Builder for `scope`.
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            model: QueryModel::default(),
            rt: Runtime::default(),
        }
    }

    /// Tab title.
    pub fn title(&self) -> String {
        "Query builder".to_owned()
    }

    /// Consumes this tool's events.
    pub fn on_event(&mut self, event: &Event) -> bool {
        match event {
            Event::SchemaDescribed { request_id, result }
                if self.rt.describe == Some(*request_id) =>
            {
                self.rt.describe = None;
                match result {
                    Ok(t) => {
                        self.rt.tables = t.clone();
                        self.rt.error = None;
                    }
                    Err(e) => self.rt.error = Some(error_text(e)),
                }
                true
            }
            _ => false,
        }
    }

    fn definition(&self, alias: &str) -> Option<&TableSchema> {
        let r = self.model.tables.iter().find(|t| t.alias == alias)?;
        self.rt
            .tables
            .iter()
            .find(|t| t.name == r.name && t.schema == r.schema)
    }

    /// Every column of every table in the query.
    fn all_columns(&self) -> Vec<ColRef> {
        self.model
            .tables
            .iter()
            .flat_map(|t| {
                self.definition(&t.alias)
                    .map(|d| {
                        d.columns
                            .iter()
                            .map(|c| ColRef {
                                table: t.alias.clone(),
                                column: c.name.clone(),
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Renders the tool.
    pub fn show(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        let kind = self.scope.kind(cx).unwrap_or(DatabaseKind::PostgreSql);
        if !self.rt.started {
            self.rt.started = true;
            self.rt.describe = super::describe(cx, &self.scope);
        }
        let sql = self.model.to_sql(kind);
        egui::Frame::new()
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    header(
                        ui,
                        icon::PUZZLE_PIECE,
                        "Query builder",
                        &self.scope.label(cx),
                    );
                    ui.separator();
                    ui.checkbox(&mut self.model.distinct, "Distinct");
                    let mut limited = self.model.limit.is_some();
                    if ui.checkbox(&mut limited, "Limit").changed() {
                        self.model.limit = limited.then_some(100);
                    }
                    if let Some(n) = &mut self.model.limit {
                        ui.add(egui::DragValue::new(n).range(1..=1_000_000));
                    }
                    ui.separator();
                    let has_sql = !sql.is_empty();
                    for (run, glyph, text) in [
                        (false, icon::FILE_SQL, "Open in Editor"),
                        (true, icon::PLAY, "Run"),
                    ] {
                        if ui
                            .add_enabled(has_sql, Button::new(format!("{glyph}  {text}")))
                            .clicked()
                        {
                            cx.actions.push(UiAction::OpenScript {
                                profile_id: self.scope.profile_id,
                                database: self.scope.database.clone(),
                                sql: format!("{sql};"),
                                run,
                            });
                        }
                    }
                    if ui.button(format!("{}  Clear", icon::ERASER)).clicked() {
                        self.model = QueryModel::default();
                    }
                });
                if let Some(e) = &self.rt.error {
                    error_box(ui, e);
                }
                ui.separator();
                let sql_height = 150.0;
                let body_height = (ui.available_height() - sql_height - 20.0).max(200.0);
                // Bound the row: its vertical separator would otherwise take all remaining height
                // and push the SQL preview out of view.
                ui.allocate_ui(egui::vec2(ui.available_width(), body_height), |ui| {
                    ui.horizontal_top(|ui| {
                        ui.vertical(|ui| {
                            ui.set_width(210.0);
                            ui.set_height(body_height);
                            self.table_list(ui, cx);
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_height(body_height);
                            ScrollArea::vertical().id_salt("qb-body").show(ui, |ui| {
                                self.cards(ui);
                                ui.add_space(6.0);
                                self.joins(ui);
                                ui.add_space(6.0);
                                self.output(ui);
                                ui.add_space(6.0);
                                self.filters(ui);
                            });
                        });
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SQL").strong());
                    if ui.small_button(format!("{}  Copy", icon::COPY)).clicked() {
                        ui.ctx().copy_text(sql.clone());
                    }
                });
                let mut shown = if sql.is_empty() {
                    "-- add a table on the left".to_owned()
                } else {
                    sql.clone()
                };
                ScrollArea::vertical()
                    .id_salt("qb-sql")
                    .max_height(sql_height)
                    .show(ui, |ui| {
                        ui.add(
                            TextEdit::multiline(&mut shown)
                                .font(theme::mono())
                                .desired_width(f32::INFINITY)
                                .interactive(false),
                        );
                    });
            });
    }

    fn table_list(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Tables").strong());
            if ui
                .small_button(icon::ARROWS_CLOCKWISE)
                .on_hover_text("Reload")
                .clicked()
            {
                self.rt.describe = super::describe(cx, &self.scope);
            }
            if self.rt.describe.is_some() {
                ui.spinner();
            }
        });
        ui.add(TextEdit::singleline(&mut self.rt.filter).hint_text("filter…"));
        let needle = self.rt.filter.to_lowercase();
        let mut add = None;
        ScrollArea::vertical().id_salt("qb-tables").show(ui, |ui| {
            for (i, t) in self.rt.tables.iter().enumerate() {
                if !needle.is_empty() && !t.name.to_lowercase().contains(&needle) {
                    continue;
                }
                if ui
                    .button(format!("{}  {}", icon::PLUS, t.name))
                    .on_hover_text("Add to the query")
                    .clicked()
                {
                    add = Some(i);
                }
            }
        });
        if let Some(i) = add {
            let table = self.rt.tables[i].clone();
            self.model.add_table(&table, &self.rt.tables);
        }
    }

    fn cards(&mut self, ui: &mut Ui) {
        if self.model.tables.is_empty() {
            ui.label(RichText::new("Add tables from the list on the left.").color(color::TEXT_DIM));
            return;
        }
        let mut remove = None;
        let mut toggle = None;
        ui.horizontal_wrapped(|ui| {
            for t in &self.model.tables {
                let Some(def) = self.definition(&t.alias) else {
                    continue;
                };
                // The card stacks its rows; without `vertical` it inherits the wrapped row layout
                // and the column checkboxes line up sideways out of view.
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.set_width(190.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{}  {}", icon::TABLE, t.alias)).strong(),
                            );
                            if ui.small_button(icon::X).on_hover_text("Remove").clicked() {
                                remove = Some(t.alias.clone());
                            }
                        });
                        ScrollArea::vertical()
                            .id_salt(("qb-card", &t.alias))
                            .max_height(160.0)
                            .min_scrolled_height(160.0)
                            .show(ui, |ui| {
                                for c in &def.columns {
                                    let col = ColRef {
                                        table: t.alias.clone(),
                                        column: c.name.clone(),
                                    };
                                    let mut on = self.model.columns.iter().any(|s| s.col == col);
                                    let key = if def.primary_key.contains(&c.name) {
                                        format!("{} ", icon::KEY)
                                    } else {
                                        String::new()
                                    };
                                    if ui.checkbox(&mut on, format!("{key}{}", c.name)).changed() {
                                        toggle = Some(col);
                                    }
                                }
                            });
                    })
                });
            }
        });
        if let Some(a) = remove {
            self.model.remove_table(&a);
        }
        if let Some(c) = toggle {
            self.model.toggle_column(c);
        }
    }

    fn joins(&mut self, ui: &mut Ui) {
        if self.model.tables.len() < 2 {
            return;
        }
        let columns = self.all_columns();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Joins").strong());
            if ui.small_button(format!("{}  Add", icon::PLUS)).clicked() && columns.len() >= 2 {
                let last = self
                    .model
                    .tables
                    .last()
                    .map(|t| t.alias.clone())
                    .unwrap_or_default();
                let left = columns.iter().find(|c| c.table != last).cloned();
                let right = columns.iter().find(|c| c.table == last).cloned();
                if let (Some(left), Some(right)) = (left, right) {
                    self.model.joins.push(Join {
                        kind: JoinKind::Inner,
                        left,
                        right,
                    });
                }
            }
        });
        let mut remove = None;
        Grid::new("qb-joins").spacing([8.0, 4.0]).show(ui, |ui| {
            for (i, j) in self.model.joins.iter_mut().enumerate() {
                col_combo(ui, ("qb-jl", i), &mut j.left, &columns);
                ComboBox::from_id_salt(("qb-jk", i))
                    .selected_text(j.kind.sql())
                    .show_ui(ui, |ui| {
                        for k in JoinKind::ALL {
                            ui.selectable_value(&mut j.kind, k, k.sql());
                        }
                    });
                col_combo(ui, ("qb-jr", i), &mut j.right, &columns);
                if ui.small_button(icon::TRASH).clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = remove {
            self.model.joins.remove(i);
        }
    }

    fn output(&mut self, ui: &mut Ui) {
        if self.model.columns.is_empty() {
            return;
        }
        ui.label(RichText::new("Output columns").strong());
        let mut remove = None;
        let mut swap = None;
        let n = self.model.columns.len();
        Grid::new("qb-out")
            .striped(true)
            .spacing([8.0, 3.0])
            .show(ui, |ui| {
                for h in ["Column", "Alias", "Aggregate", "Sort", ""] {
                    ui.label(RichText::new(h).color(color::TEXT_DIM));
                }
                ui.end_row();
                for (i, c) in self.model.columns.iter_mut().enumerate() {
                    ui.label(label(&c.col));
                    ui.add_sized(
                        [120.0, ui.spacing().interact_size.y],
                        TextEdit::singleline(&mut c.alias).hint_text("alias"),
                    );
                    ComboBox::from_id_salt(("qb-agg", i))
                        .selected_text(c.aggregate.label())
                        .show_ui(ui, |ui| {
                            for a in Aggregate::ALL {
                                ui.selectable_value(&mut c.aggregate, a, a.label());
                            }
                        });
                    ComboBox::from_id_salt(("qb-sort", i))
                        .selected_text(match c.sort {
                            Sort::None => "—",
                            Sort::Asc => "ASC",
                            Sort::Desc => "DESC",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut c.sort, Sort::None, "—");
                            ui.selectable_value(&mut c.sort, Sort::Asc, "ASC");
                            ui.selectable_value(&mut c.sort, Sort::Desc, "DESC");
                        });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(i > 0, Button::new(icon::ARROW_UP).small())
                            .clicked()
                        {
                            swap = Some((i, i - 1));
                        }
                        if ui
                            .add_enabled(i + 1 < n, Button::new(icon::ARROW_DOWN).small())
                            .clicked()
                        {
                            swap = Some((i, i + 1));
                        }
                        if ui.small_button(icon::TRASH).clicked() {
                            remove = Some(i);
                        }
                    });
                    ui.end_row();
                }
            });
        if let Some((a, b)) = swap {
            self.model.columns.swap(a, b);
        }
        if let Some(i) = remove {
            self.model.columns.remove(i);
        }
    }

    fn filters(&mut self, ui: &mut Ui) {
        let columns = self.all_columns();
        if columns.is_empty() {
            return;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("Conditions").strong());
            if ui.small_button(format!("{}  Add", icon::PLUS)).clicked() {
                self.model.filters.push(Filter {
                    col: columns[0].clone(),
                    op: Operator::Eq,
                    value: String::new(),
                    or: false,
                });
            }
        });
        let mut remove = None;
        Grid::new("qb-filters").spacing([8.0, 4.0]).show(ui, |ui| {
            for (i, f) in self.model.filters.iter_mut().enumerate() {
                if i == 0 {
                    ui.label("WHERE");
                } else {
                    ComboBox::from_id_salt(("qb-or", i))
                        .width(60.0)
                        .selected_text(if f.or { "OR" } else { "AND" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut f.or, false, "AND");
                            ui.selectable_value(&mut f.or, true, "OR");
                        });
                }
                col_combo(ui, ("qb-fc", i), &mut f.col, &columns);
                ComboBox::from_id_salt(("qb-op", i))
                    .width(90.0)
                    .selected_text(f.op.sql())
                    .show_ui(ui, |ui| {
                        for op in Operator::ALL {
                            ui.selectable_value(&mut f.op, op, op.sql());
                        }
                    });
                let hint = if f.op == Operator::In {
                    "a, b, c"
                } else {
                    "value"
                };
                ui.add_enabled_ui(f.op.has_value(), |ui| {
                    ui.add_sized(
                        [160.0, ui.spacing().interact_size.y],
                        TextEdit::singleline(&mut f.value).hint_text(hint),
                    )
                });
                if ui.small_button(icon::TRASH).clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = remove {
            self.model.filters.remove(i);
        }
    }
}
