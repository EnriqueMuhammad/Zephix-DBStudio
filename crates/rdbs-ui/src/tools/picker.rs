//! Connection / database / schema picker shared by the diff, transfer and backup tools.

use egui::{RichText, TextEdit, Ui};
use egui_phosphor::regular as icon;
use rdbs_core::{DatabaseKind, ObjectKind};

use super::Scope;
use crate::model::{ConnState, UiAction};
use crate::theme::color;
use crate::workspace::TabContext;

/// Default schema of an engine.
pub fn default_schema(kind: DatabaseKind) -> Option<String> {
    match kind {
        DatabaseKind::PostgreSql => Some("public".into()),
        DatabaseKind::SqlServer => Some("dbo".into()),
        _ => None,
    }
}

fn databases(conn: Option<&ConnState>) -> Vec<String> {
    conn.map(|c| {
        c.objects
            .iter()
            .filter(|o| o.kind == ObjectKind::Database)
            .map(|o| o.name.clone())
            .collect()
    })
    .unwrap_or_default()
}

fn schemas(conn: Option<&ConnState>, database: Option<&str>) -> Vec<String> {
    let Some(conn) = conn else {
        return Vec::new();
    };
    conn.objects
        .iter()
        .filter(|o| o.kind == ObjectKind::Database)
        .find(|o| match database {
            Some(d) => o.name == d,
            None => !o.children.is_empty(),
        })
        .map(|db| {
            db.children
                .iter()
                .filter(|c| c.kind == ObjectKind::Schema)
                .map(|c| c.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Text field with a dropdown of known values.
fn text_with_choices(
    ui: &mut Ui,
    id: &str,
    value: &mut String,
    choices: &[String],
    hint: &str,
) -> bool {
    let mut changed = ui
        .add(
            TextEdit::singleline(value)
                .hint_text(hint)
                .desired_width(130.0),
        )
        .changed();
    ui.add_enabled_ui(!choices.is_empty(), |ui| {
        ui.push_id(id, |ui| {
            ui.menu_button(icon::CARET_DOWN, |ui| {
                for c in choices {
                    if ui.selectable_label(value == c, c).clicked() {
                        value.clone_from(c);
                        changed = true;
                        ui.close();
                    }
                }
            });
        });
    });
    changed
}

/// Renders the picker; returns true when the scope changed.
pub fn show(ui: &mut Ui, id: &str, scope: &mut Option<Scope>, cx: &mut TabContext<'_>) -> bool {
    let mut changed = false;
    ui.vertical(|ui| {
        let current = scope.as_ref().and_then(|s| cx.profile(s.profile_id));
        let selected = current.map_or_else(
            || format!("{}  Choose connection…", icon::PLUGS),
            |p| format!("{}  {}", p.kind.badge(), p.name),
        );
        egui::ComboBox::from_id_salt((id, "conn"))
            .width(260.0)
            .selected_text(selected)
            .show_ui(ui, |ui| {
                // The tools using this picker work on SQL databases only.
                for p in cx.profiles.iter().filter(|p| p.kind.is_sql()) {
                    let is = scope.as_ref().is_some_and(|s| s.profile_id == p.id);
                    if ui
                        .selectable_label(is, format!("{}  {}", p.kind.badge(), p.name))
                        .on_hover_text(p.endpoint())
                        .clicked()
                        && !is
                    {
                        let conn = cx.connections.get(&p.id);
                        let database = (p.kind != DatabaseKind::Sqlite).then(|| {
                            conn.and_then(|c| c.default_database.clone())
                                .unwrap_or_else(|| p.database.clone())
                        });
                        *scope = Some(Scope {
                            profile_id: p.id,
                            database: database.filter(|d| !d.is_empty()),
                            schema: default_schema(p.kind),
                        });
                        if !conn.is_some_and(ConnState::is_connected) {
                            cx.actions.push(UiAction::Connect(p.id));
                        }
                        changed = true;
                    }
                }
            });
        let Some(s) = scope.as_mut() else { return };
        let Some(profile) = cx.profile(s.profile_id) else {
            ui.label(RichText::new("connection deleted").color(color::danger()));
            return;
        };
        let kind = profile.kind;
        let conn = cx.connections.get(&s.profile_id);
        ui.horizontal(|ui| {
            if kind != DatabaseKind::Sqlite {
                ui.label(RichText::new("Database").color(color::text_dim()));
                let mut db = s.database.clone().unwrap_or_default();
                if text_with_choices(
                    ui,
                    &format!("{id}-db"),
                    &mut db,
                    &databases(conn),
                    "default",
                ) {
                    s.database = (!db.trim().is_empty()).then(|| db.trim().to_owned());
                    changed = true;
                }
            }
            if kind.has_schemas() {
                ui.label(RichText::new("Schema").color(color::text_dim()));
                let mut schema = s.schema.clone().unwrap_or_default();
                let known = schemas(conn, s.database.as_deref());
                if text_with_choices(ui, &format!("{id}-schema"), &mut schema, &known, "all") {
                    s.schema = (!schema.trim().is_empty()).then(|| schema.trim().to_owned());
                    changed = true;
                }
            }
        });
    });
    changed
}
