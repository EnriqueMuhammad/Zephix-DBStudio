//! Connection / database / schema picker shared by the diff, transfer and backup tools.

use egui::{Popup, PopupCloseBehavior, RichText, ScrollArea, SetOpenCommand, TextEdit, Ui};
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

/// Choices shown for `typed`: all of them, or those containing it while it is not one of them.
fn matching<'a>(choices: &'a [String], typed: &str) -> Vec<&'a String> {
    let needle = typed.trim().to_lowercase();
    if needle.is_empty() || choices.iter().any(|c| c.eq_ignore_ascii_case(typed.trim())) {
        return choices.iter().collect();
    }
    choices
        .iter()
        .filter(|c| c.to_lowercase().contains(&needle))
        .collect()
}

/// Text field with a dropdown of known values. Clicking the text or the arrow opens the list;
/// typing filters it, and any other name can still be typed.
fn text_with_choices(
    ui: &mut Ui,
    id: &str,
    value: &mut String,
    choices: &[String],
    hint: &str,
) -> bool {
    let mut changed = false;
    ui.push_id(id, |ui| {
        let text = ui.add(
            TextEdit::singleline(value)
                .hint_text(hint)
                .desired_width(130.0),
        );
        changed |= text.changed();
        let arrow = ui.button(icon::CARET_DOWN);
        let command = if arrow.clicked() {
            Some(SetOpenCommand::Toggle)
        } else if text.clicked() || text.gained_focus() || text.changed() {
            Some(SetOpenCommand::Bool(true))
        } else {
            None
        };
        let anchor = text.rect.union(arrow.rect);
        let popup_id = ui.id().with("choices");
        // Closed by hand: a click into the text (to place the cursor) keeps the list open.
        let popup = Popup::new(popup_id, ui.ctx().clone(), anchor, ui.layer_id())
            .open_memory(command)
            .close_behavior(PopupCloseBehavior::IgnoreClicks)
            .width(anchor.width())
            .show(|ui| {
                let shown = matching(choices, value);
                if shown.is_empty() {
                    let note = if choices.is_empty() {
                        "Nothing loaded yet; type a name"
                    } else {
                        "No match; the typed name is used"
                    };
                    ui.label(RichText::new(note).color(color::text_dim()));
                    return;
                }
                ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                    for c in shown {
                        if ui.selectable_label(value == c, c).clicked() {
                            value.clone_from(c);
                            changed = true;
                            Popup::close_id(ui.ctx(), popup_id);
                        }
                    }
                });
            });
        if let Some(popup) = popup {
            let clicked_elsewhere = ui.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer
                        .interact_pos()
                        .is_some_and(|p| !anchor.contains(p) && !popup.response.rect.contains(p))
            });
            let tabbed_away = text.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Tab));
            if clicked_elsewhere || tabbed_away {
                Popup::close_id(ui.ctx(), popup_id);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_filters_the_choices() {
        let choices: Vec<String> = ["app_dev", "app_test", "shop"].map(String::from).into();
        let names = |typed: &str| -> Vec<&str> {
            matching(&choices, typed)
                .into_iter()
                .map(String::as_str)
                .collect()
        };
        assert_eq!(names(""), ["app_dev", "app_test", "shop"]);
        assert_eq!(names("APP"), ["app_dev", "app_test"]);
        // A complete name still lists everything, so another one can be picked.
        assert_eq!(names("shop"), ["app_dev", "app_test", "shop"]);
        assert!(names("nope").is_empty());
    }
}
