//! Query tab: toolbar, SQL editor, results area and status line.

use std::time::{Duration, Instant};

use egui::{
    Align, Button, Event, Frame, Id, Key, Layout, Margin, Modifiers, RichText, ScrollArea, Stroke,
    TextBuffer, TextEdit, Ui, Vec2,
};
use egui_phosphor::regular as icon;
use rdbs_core::sql;
use rdbs_core::{Command, ConnectionProfile, DbErrorKind, DbObject, ObjectKind};
use rdbs_services::ServiceHandle;

use crate::grid;
use crate::model::{AfterPassword, ConnState, ConnStatus, StatusMessage, UiAction};
use crate::textops::{self, Selection};
use crate::theme::{self, color};
use crate::widgets::{self, format_count, format_duration};
use crate::workspace::{LastRun, QueryTab, RunPhase, RunState, Running, TabContext};

mod find;
mod keys;
mod menu;
mod results;
mod run;

use keys::*;
use menu::*;
use results::*;
use run::*;

pub(crate) use find::FindState;
pub(crate) use keys::chord_pending;
pub(crate) use menu::{EditorAction, apply_editor_action, editor_preferences, show_edit_menu};
pub(crate) use results::export_table_name;
pub(crate) use run::{needs_password, request_run, start_query};

/// Renders a query tab.
pub fn show(ui: &mut Ui, q: &mut QueryTab, cx: &mut TabContext<'_>) {
    let data_tab = q.data_view.is_some();
    egui::Panel::top(Id::new(("q-toolbar", q.id)))
        .frame(
            Frame::new()
                .fill(color::panel())
                .inner_margin(Margin::symmetric(8, 5)),
        )
        .show_separator_line(false)
        .show(ui, |ui| {
            if data_tab {
                data_toolbar(ui, q, cx);
            } else {
                toolbar(ui, q, cx);
            }
        });

    let session_dropped = tab_session_dropped(q, cx);
    egui::Panel::bottom(Id::new(("q-status", q.id)))
        .exact_size(24.0)
        .frame(
            Frame::new()
                .fill(color::panel())
                .inner_margin(Margin::symmetric(8, 0)),
        )
        .show(ui, |ui| status_line(ui, q, session_dropped));

    if data_tab {
        egui::CentralPanel::no_frame().show(ui, |ui| results(ui, q, cx));
        return;
    }
    let available = ui.available_height();
    egui::Panel::top(Id::new(("q-editor", q.id)))
        .resizable(true)
        .default_size((available * 0.42).max(120.0))
        .size_range(80.0..=(available - 90.0).max(90.0))
        .frame(Frame::new().fill(color::editor()))
        .show(ui, |ui| editor(ui, q, cx));

    egui::CentralPanel::no_frame().show(ui, |ui| results(ui, q, cx));
}

/// Session key for this tab: `None` when it uses the profile's own database.
fn tab_session_key(q: &QueryTab, cx: &TabContext<'_>) -> Option<Option<String>> {
    let profile = q.profile_id.and_then(|id| cx.profile(id))?;
    Some(profile.session_database(q.database.clone()))
}

fn tab_session_dropped(q: &QueryTab, cx: &TabContext<'_>) -> bool {
    let Some(key) = tab_session_key(q, cx) else {
        return false;
    };
    q.profile_id
        .and_then(|id| cx.connections.get(&id))
        .is_some_and(|conn| conn.session_dropped(&key))
}

/// Coloured mark next to the connection name: green when the session is up, amber when it
/// was dropped, red when the last connect failed.
fn session_dot(ui: &mut Ui, q: &QueryTab, cx: &TabContext<'_>) {
    if q.run
        .running
        .as_ref()
        .is_some_and(|running| running.phase == RunPhase::Connecting)
    {
        ui.label(RichText::new("●").color(color::text_faint()))
            .on_hover_text("Connecting…");
        return;
    }
    let conn = q.profile_id.and_then(|id| cx.connections.get(&id));
    let key = tab_session_key(q, cx).unwrap_or(None);
    let (paint, tip) = match conn.map(|c| &c.status) {
        Some(ConnStatus::Connecting) => (color::text_faint(), "Connecting…"),
        Some(ConnStatus::Failed(_)) => (color::danger(), "Connection failed — use Reconnect"),
        Some(ConnStatus::Connected { .. }) if conn.is_some_and(|c| c.session_dropped(&key)) => (
            color::warning(),
            "Session dropped — Reconnect opens a new one. This tab stays open.",
        ),
        Some(ConnStatus::Connected { .. }) => (color::success(), "Connected"),
        _ => (color::text_faint(), "Not connected"),
    };
    ui.label(RichText::new("●").color(paint)).on_hover_text(tip);
}

/// Replaces this tab's database session. The editor text and the tab stay.
fn reconnect_button(ui: &mut Ui, q: &QueryTab, cx: &mut TabContext<'_>) {
    let profile = q.profile_id.and_then(|id| cx.profile(id));
    let running = q.run.running.is_some();
    let dropped = tab_session_dropped(q, cx);
    let label = RichText::new(format!("{}  Reconnect", icon::ARROWS_CLOCKWISE));
    let label = if dropped {
        label.color(color::warning())
    } else {
        label
    };
    let enabled = profile.is_some() && !running;
    let mut response = ui.add_enabled(enabled, Button::new(label)).on_hover_text(
        "Close this tab's database session and open a new one. The editor text stays. \
             Temporary tables, open transactions, and an in-memory SQLite database are lost.",
    );
    if profile.is_none() {
        response = response.on_disabled_hover_text("Choose a connection first");
    } else if running {
        response = response.on_disabled_hover_text("Wait for the running query to finish");
    }
    if response.clicked()
        && let Some(profile) = profile
    {
        cx.actions.push(UiAction::Reconnect {
            profile_id: profile.id,
            database: q.database.clone(),
            retry_tab: None,
        });
    }
}

fn toolbar(ui: &mut Ui, q: &mut QueryTab, cx: &mut TabContext<'_>) {
    ui.horizontal(|ui| {
        let running = q.run.running.is_some();
        let run =
            Button::new(RichText::new(format!("{}  Run", icon::PLAY)).color(color::success()));
        if ui
            .add_enabled(!running, run)
            .on_hover_text("Run (Ctrl+Enter or F5). Runs only the selection when text is selected.")
            .clicked()
        {
            request_run(q, cx);
        }
        // Until a session reports its capabilities, assume cancellation is possible.
        let can_cancel = match q.profile_id.and_then(|id| cx.connections.get(&id)) {
            Some(ConnState {
                status: ConnStatus::Connected { capabilities, .. },
                ..
            }) => capabilities.query_cancellation,
            _ => true,
        };
        let stop =
            Button::new(RichText::new(format!("{}  Stop", icon::STOP)).color(color::danger()));
        if ui
            .add_enabled(running && can_cancel, stop)
            .on_hover_text("Cancel the running query")
            .on_disabled_hover_text("This connection does not support query cancellation")
            .clicked()
            && let Some(r) = &q.run.running
        {
            cx.services.send(Command::Cancel {
                request_id: r.request_id,
            });
        }
        ui.separator();

        let current = q.profile_id.and_then(|id| cx.profile(id));
        let selected = match current {
            Some(p) => format!("{}  {}", p.kind.badge(), p.name),
            None => format!("{}  Choose connection…", icon::PLUGS),
        };
        egui::ComboBox::from_id_salt(("conn-combo", q.id))
            .width(230.0)
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for p in cx.profiles {
                    let label = format!("{}  {}", p.kind.badge(), p.name);
                    ui.selectable_value(&mut q.profile_id, Some(p.id), label)
                        .on_hover_text(p.endpoint());
                }
            });
        if let Some(p) = current
            && p.kind.has_schemas()
        {
            database_combo(ui, q, cx);
        }
        if let Some(p) = current {
            widgets::env_pill(ui, p.environment);
        }
        ui.separator();
        session_dot(ui, q, cx);
        reconnect_button(ui, q, cx);
        ui.separator();
        let can_explain = match q.profile_id.and_then(|id| cx.connections.get(&id)) {
            Some(ConnState {
                status: ConnStatus::Connected { capabilities, .. },
                ..
            }) => capabilities.explain_plan,
            _ => true,
        };
        let busy = running || q.run.plan_request.is_some();
        if ui
            .add_enabled(
                !busy && can_explain,
                Button::new(format!("{}  Explain", icon::TREE_STRUCTURE)),
            )
            .on_hover_text(
                "Estimated execution plan of the statement at the caret (or the selection)",
            )
            .on_disabled_hover_text("Execution plans are not available for this connection")
            .clicked()
        {
            request_explain(q, cx, false);
        }
        ui.add_enabled_ui(!busy && can_explain, |ui| {
            ui.menu_button(icon::CARET_DOWN, |ui| {
                if ui
                    .button(format!("{}  Explain Analyze", icon::LIGHTNING))
                    .on_hover_text(
                        "Executes the statement to measure actual rows and time. \
                         Its changes are rolled back where the engine allows it.",
                    )
                    .clicked()
                {
                    request_explain(q, cx, true);
                    ui.close();
                }
            });
        });
    });
}

/// Width of the row-limit box in the data toolbar.
const LIMIT_WIDTH: f32 = 110.0;

/// Toolbar of a data tab: refresh, filter, sort order and row limit of one table or view on the
/// left (the text fields share the free width), the connection and "Open as Query" on the right.
fn data_toolbar(ui: &mut Ui, q: &mut QueryTab, cx: &mut TabContext<'_>) {
    let running = q.run.running.is_some();
    let mut apply = false;
    ui.horizontal(|ui| {
        // The right side is laid out first so that the fields can take what is left.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .button(format!("{}  Open as Query", icon::FILE_SQL))
                .on_hover_text("Continue in a query tab with this SELECT")
                .clicked()
                && let Some(view) = q.data_view.take()
            {
                q.sql = view.sql();
                // A new script starts out unchanged.
                q.saved = q.sql.clone();
                q.run.focus_editor = true;
            }
            if q.profile_id.and_then(|id| cx.profile(id)).is_some() {
                ui.separator();
                reconnect_button(ui, q, cx);
                session_dot(ui, q, cx);
                if let Some(p) = q.profile_id.and_then(|id| cx.profile(id)) {
                    widgets::env_pill(ui, p.environment);
                    ui.label(
                        RichText::new(format!("{}  {}", p.kind.badge(), p.name))
                            .color(color::text_dim()),
                    )
                    .on_hover_text(p.endpoint());
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let refresh = Button::new(
                    RichText::new(format!("{}  Refresh", icon::ARROWS_CLOCKWISE))
                        .color(color::success()),
                );
                if ui
                    .add_enabled(!running, refresh)
                    .on_hover_text("Load the rows again (Ctrl+Enter)")
                    .clicked()
                {
                    apply = true;
                }
                let stop = Button::new(
                    RichText::new(format!("{}  Stop", icon::STOP)).color(color::danger()),
                );
                if ui.add_enabled(running, stop).clicked()
                    && let Some(r) = &q.run.running
                {
                    cx.services.send(Command::Cancel {
                        request_id: r.request_id,
                    });
                }
                ui.separator();
                let Some(view) = q.data_view.as_mut() else {
                    return;
                };
                let (filter_width, order_width) = field_widths(ui);
                ui.label(RichText::new(icon::FUNNEL).color(color::text_dim()))
                    .on_hover_text("Filter (WHERE)");
                let filter = ui.add(
                    TextEdit::singleline(&mut view.filter)
                        .hint_text("Filter, e.g. country = 'Indonesia'")
                        .desired_width(filter_width),
                );
                ui.label(RichText::new(icon::SORT_ASCENDING).color(color::text_dim()))
                    .on_hover_text("Sort (ORDER BY)");
                let order = ui.add(
                    TextEdit::singleline(&mut view.order)
                        .hint_text("Sort, e.g. created_at DESC")
                        .desired_width(order_width),
                );
                let entered = |r: &egui::Response| {
                    r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                };
                if entered(&filter) || entered(&order) {
                    apply = true;
                }
                let label = |limit: Option<u32>| match limit {
                    Some(n) => format!("{} rows", format_count(u64::from(n))),
                    None => "All rows".to_owned(),
                };
                egui::ComboBox::from_id_salt(("data-limit", q.id))
                    .width(LIMIT_WIDTH)
                    .selected_text(label(view.limit))
                    .show_ui(ui, |ui| {
                        for limit in crate::workspace::DataView::LIMITS {
                            if ui
                                .selectable_value(&mut view.limit, limit, label(limit))
                                .changed()
                            {
                                apply = true;
                            }
                        }
                    });
            });
        });
    });
    if apply {
        request_run(q, cx);
    }
}

/// Text widths of the filter and sort fields: the free width of the toolbar row (after the
/// icons, the row-limit box and the gaps), split 3:2, within sensible bounds.
fn field_widths(ui: &Ui) -> (f32, f32) {
    let gap = ui.spacing().item_spacing.x;
    let icon = ui.spacing().interact_size.y;
    // Two icons, the limit box with its arrow, the fields' own margins and the gaps between.
    let fixed = 2.0 * icon + LIMIT_WIDTH + ui.spacing().icon_width + 4.0 * 8.0 + 6.0 * gap;
    let free = (ui.available_width() - fixed).max(0.0);
    let filter = (free * 0.6).clamp(160.0, 560.0);
    let order = (free - filter).clamp(120.0, 360.0);
    (filter, order)
}

/// Database selector for engines where a session is bound to one database.
fn database_combo(ui: &mut Ui, q: &mut QueryTab, cx: &TabContext<'_>) {
    let conn = q.profile_id.and_then(|id| cx.connections.get(&id));
    let default = conn
        .and_then(|c| c.default_database.clone())
        .unwrap_or_else(|| "default".to_owned());
    let databases: Vec<String> = conn
        .map(|c| {
            c.objects
                .iter()
                .filter(|o| o.kind == rdbs_core::ObjectKind::Database)
                .map(|o| o.name.clone())
                .collect()
        })
        .unwrap_or_default();
    let selected = q.database.clone().unwrap_or_else(|| default.clone());
    egui::ComboBox::from_id_salt(("db-combo", q.id))
        .width(150.0)
        .selected_text(format!("{}  {selected}", icon::DATABASE))
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(q.database.is_none(), format!("{default} (default)"))
                .clicked()
            {
                q.database = None;
            }
            for db in databases.iter().filter(|d| **d != default) {
                if ui
                    .selectable_label(q.database.as_deref() == Some(db.as_str()), db)
                    .clicked()
                {
                    q.database = Some(db.clone());
                }
            }
        })
        .response
        .on_hover_text("Database this tab runs in (a separate session is opened when needed)");
}

fn editor(ui: &mut Ui, q: &mut QueryTab, cx: &mut TabContext<'_>) {
    let font = theme::mono();
    let row_height = ui.ctx().fonts_mut(|f| f.row_height(&font));
    let editor_id = q.editor_id();
    if std::mem::take(&mut q.run.focus_editor) {
        ui.memory_mut(|m| m.request_focus(editor_id));
    }
    let word_wrap = pref(ui.ctx(), WORD_WRAP);
    let white_space = pref(ui.ctx(), WHITE_SPACE);

    // Completion keys must be consumed before the text editor handles them.
    let has_focus = ui.memory(|m| m.has_focus(editor_id));
    if let Some(accepted) = q.run.autocomplete.handle_keys(ui, has_focus) {
        crate::autocomplete::apply(accepted, &mut q.sql, ui.ctx(), editor_id);
    }
    // So are the editor's own shortcuts and the keys that close brackets and quotes.
    let mut menu_action: Option<EditorAction> = None;
    if has_focus {
        menu_action = shortcut_action(ui, editor_id, &q.sql);
        auto_close(ui, editor_id, q);
    } else {
        clear_chord(ui.ctx(), editor_id);
    }
    let connections = cx.connections;
    let conn = q.profile_id.and_then(|id| connections.get(&id));
    let kind = q.profile_id.and_then(|id| cx.profile(id)).map(|p| p.kind);
    // Engines with several databases per server complete from the tab's database.
    let database = kind.filter(|k| k.has_schemas()).and_then(|_| {
        q.database
            .clone()
            .or_else(|| conn?.default_database.clone())
    });
    if has_focus {
        load_tab_database(q, conn, database.as_deref(), cx.actions);
    }
    let objects = conn.map_or(&[][..], |c| {
        completion_objects(&c.objects, database.as_deref())
    });

    if q.run.find.is_open() {
        if has_focus {
            q.run.find.follow_editor(ui.ctx(), editor_id);
        }
        find::bar(ui, q, editor_id);
    }
    let highlights = q.run.find.highlights(q.sql.len());

    let mut layouter = |ui: &Ui, buf: &dyn TextBuffer, wrap_width: f32| {
        let mut job = if white_space {
            widgets::highlight_sql_whitespace(buf.as_str(), &font)
        } else {
            widgets::highlight_sql(buf.as_str(), &font)
        };
        highlights.paint(&mut job);
        job.wrap.max_width = if word_wrap { wrap_width } else { f32::INFINITY };
        ui.ctx().fonts_mut(|f| f.layout_job(job))
    };

    let scroll_style = ui.spacing().scroll;
    // Solid bars stay visible. The default floating bars fade out, so a long line
    // looks clipped even though it can be scrolled.
    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
    let scroll = if word_wrap {
        ScrollArea::vertical()
    } else {
        ScrollArea::both()
    };
    let scrolled = scroll
        .id_salt(("editor-scroll", q.id))
        .auto_shrink(false)
        .stick_to_bottom(false)
        .stick_to_right(false)
        .show(ui, |ui| {
            let lines = q.sql.split('\n').count().max(1);
            let digits = lines.to_string().len().max(2) as f32;
            let gutter_width = 16.0 + digits * 8.0;
            let min_width = if word_wrap {
                0.0
            } else {
                let width_id = Id::new(("editor-width", q.id));
                let remembered = ui
                    .ctx()
                    .data(|data| data.get_temp::<f32>(width_id).unwrap_or(0.0));
                let estimated = q
                    .sql
                    .lines()
                    .map(|line| line.chars().count())
                    .max()
                    .unwrap_or(0) as f32
                    * 8.0;
                remembered.max(estimated)
            };
            // egui's scroll area asks its content to fit the viewport. A minimum wider
            // than that is what makes the horizontal bar appear for a long line.
            ui.set_min_width(gutter_width + 8.0 + min_width);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                // The numbers are painted once the rows are known: a wrapped line spans
                // several rows.
                let (gutter, _) = ui.allocate_exact_size(
                    Vec2::new(gutter_width + 8.0, row_height),
                    egui::Sense::hover(),
                );

                let min_height = ui.available_height();
                let output = TextEdit::multiline(&mut q.sql)
                    .id(editor_id)
                    .font(font.clone())
                    .code_editor()
                    .frame(Frame::NONE)
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .min_size(Vec2::new(min_width, min_height))
                    .layouter(&mut layouter)
                    .show(ui);
                paint_line_numbers(ui, gutter.right() - 8.0, &output, &font);
                if let Some(at) = q.run.find.scroll_to.take() {
                    let rect = output
                        .galley
                        .pos_from_cursor(egui::text::CCursor::new(at))
                        .translate(output.galley_pos.to_vec2());
                    ui.scroll_to_rect(rect.expand2(Vec2::new(48.0, row_height)), None);
                }
                let has_selection = output
                    .cursor_range
                    .is_some_and(|r| r.primary.index != r.secondary.index);
                output.response.context_menu(|ui| {
                    if let Some(action) = editor_menu(ui, has_selection, word_wrap, white_space) {
                        menu_action = Some(action);
                    }
                });

                q.run.cursor = output.cursor_range.map(|range| {
                    widgets::char_range_to_bytes(
                        &q.sql,
                        range.primary.index.0,
                        range.primary.index.0,
                    )
                    .start
                });
                q.run.selection = output.cursor_range.and_then(|range| {
                    let chars = range.as_sorted_char_range();
                    (chars.start.0 != chars.end.0)
                        .then(|| widgets::char_range_to_bytes(&q.sql, chars.start.0, chars.end.0))
                });
                if has_focus || q.run.autocomplete.is_open() {
                    q.run.autocomplete.update(
                        &output,
                        &q.sql,
                        objects,
                        kind.unwrap_or(rdbs_core::DatabaseKind::PostgreSql),
                    );
                }
                output
            })
            .inner
        });
    ui.spacing_mut().scroll = scroll_style;
    let line_width = scrolled.inner.galley.size().x + 4.0;
    ui.ctx()
        .data_mut(|data| data.insert_temp(Id::new(("editor-width", q.id)), line_width));
    extend_selection_past_editor(ui, q, editor_id, &scrolled);

    if let Some(accepted) = q.run.autocomplete.show(ui, Id::new(("autocomplete", q.id))) {
        crate::autocomplete::apply(accepted, &mut q.sql, ui.ctx(), editor_id);
    }
    if let Some(action) = menu_action {
        apply_editor_action(ui.ctx(), editor_id, action, q, cx);
    }
}

/// Line numbers right-aligned at `right`, next to the first row of each line.
fn paint_line_numbers(
    ui: &Ui,
    right: f32,
    output: &egui::text_edit::TextEditOutput,
    font: &egui::FontId,
) {
    let clip = ui.clip_rect();
    let mut line = 1;
    let mut starts_line = true;
    for row in &output.galley.rows {
        let rect = row.rect().translate(output.galley_pos.to_vec2());
        if starts_line && rect.bottom() >= clip.top() && rect.top() <= clip.bottom() {
            ui.painter().text(
                egui::pos2(right, rect.top()),
                egui::Align2::RIGHT_TOP,
                line.to_string(),
                font.clone(),
                color::text_faint(),
            );
        }
        starts_line = row.ends_with_newline;
        if starts_line {
            line += 1;
        }
    }
}

/// Objects completion draws on: the tree of `database` when the explorer lists it (so other
/// databases' tables stay out), else the whole tree.
fn completion_objects<'o>(objects: &'o [DbObject], database: Option<&str>) -> &'o [DbObject] {
    database
        .and_then(|db| {
            objects
                .iter()
                .position(|o| o.kind == ObjectKind::Database && o.name == db)
        })
        .map_or(objects, |i| &objects[i..=i])
}

/// Loads the objects of the tab's database when the explorer has not yet (databases other
/// than the session's load when first expanded), so completion knows its tables. Asked once
/// per tab and database, also when loading fails.
fn load_tab_database(
    q: &mut QueryTab,
    conn: Option<&ConnState>,
    database: Option<&str>,
    actions: &mut Vec<UiAction>,
) {
    let (Some(conn), Some(db), Some(profile_id)) = (conn, database, q.profile_id) else {
        return;
    };
    let unloaded = conn.objects.iter().any(|o| {
        o.kind == ObjectKind::Database
            && o.name == db
            && o.children.is_empty()
            && o.detail.as_deref() == Some(rdbs_core::schema::NOT_LOADED)
    });
    if conn.is_connected()
        && unloaded
        && !conn.loading_databases.contains(db)
        && q.run.catalog_requested.as_deref() != Some(db)
    {
        q.run.catalog_requested = Some(db.to_owned());
        actions.push(UiAction::LoadDatabase {
            profile_id,
            database: db.to_owned(),
        });
    }
}

/// Persisted editor view options, shared by all query tabs.
const WORD_WRAP: &str = "editor-word-wrap";
const WHITE_SPACE: &str = "editor-white-space";

fn pref(ctx: &egui::Context, key: &str) -> bool {
    ctx.data_mut(|d| *d.get_persisted_mut_or_default::<bool>(Id::new(key)))
}

fn toggle_pref(ctx: &egui::Context, key: &str) {
    ctx.data_mut(|d| {
        let value = d.get_persisted_mut_or_default::<bool>(Id::new(key));
        *value = !*value;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs one frame of [`auto_close`] on `text` with the caret (or selection) at `sel`.
    fn type_events(text: &str, sel: Selection, events: Vec<Event>) -> (String, Selection) {
        let ctx = egui::Context::default();
        let mut q = QueryTab::new(1, None, text.into(), None);
        let editor_id = q.editor_id();
        set_selection(&ctx, editor_id, sel);
        let input = egui::RawInput {
            events,
            ..egui::RawInput::default()
        };
        let mut output = ctx.run_ui(input, |ui| auto_close(ui, editor_id, &mut q));
        output.textures_delta.clear();
        let after = current_selection(&ctx, editor_id).unwrap();
        (q.sql, after)
    }

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// What the window sends for a printable key: its key press, then the character.
    fn typed(k: Key, modifiers: Modifiers, text: &str) -> Vec<Event> {
        vec![key(k, modifiers), Event::Text(text.into())]
    }

    #[test]
    fn typed_quote_and_parenthesis_close_themselves() {
        let caret = Selection::caret;
        let (none, shift) = (Modifiers::NONE, Modifiers::SHIFT);
        // The key press that comes before the character does not hide it.
        assert_eq!(
            type_events("select ", caret(7), typed(Key::Quote, none, "'")),
            ("select ''".into(), caret(8))
        );
        assert_eq!(
            type_events("select count", caret(12), typed(Key::Num9, shift, "(")),
            ("select count()".into(), caret(13))
        );
        assert_eq!(
            type_events("select ", caret(7), typed(Key::Backtick, none, "`")),
            ("select ``".into(), caret(8))
        );
        // Typing the closer steps over it.
        assert_eq!(
            type_events("x = ''", caret(5), typed(Key::Quote, none, "'")),
            ("x = ''".into(), caret(6))
        );
        assert_eq!(
            type_events("count()", caret(6), typed(Key::Num0, shift, ")")),
            ("count()".into(), caret(7))
        );
        // A selection is wrapped.
        assert_eq!(
            type_events(
                "select a",
                Selection::new(7, 8),
                typed(Key::Num9, shift, "(")
            ),
            ("select (a)".into(), Selection::new(8, 9))
        );
        // Backspace between an empty pair deletes both.
        assert_eq!(
            type_events("f()", caret(2), vec![key(Key::Backspace, none)]),
            ("f".into(), caret(1))
        );
        // A caret move first in the frame leaves the character to the text editor.
        let mut moved = vec![key(Key::ArrowLeft, none)];
        moved.extend(typed(Key::Quote, none, "'"));
        assert_eq!(
            type_events("select ", caret(7), moved),
            ("select ".into(), caret(7))
        );
    }

    #[test]
    fn printable_key_presses_are_not_edits() {
        assert!(!key_edits(Key::Quote, Modifiers::NONE));
        assert!(!key_edits(Key::Num9, Modifiers::SHIFT));
        assert!(!key_edits(Key::A, Modifiers::SHIFT));
        assert!(key_edits(Key::Backspace, Modifiers::NONE));
        assert!(key_edits(Key::ArrowLeft, Modifiers::SHIFT));
        assert!(key_edits(Key::Z, Modifiers::COMMAND));
    }

    #[test]
    fn export_table_name_uses_plain_titles_only() {
        assert_eq!(export_table_name("customers"), "customers");
        assert_eq!(export_table_name("Query 3"), "exported_rows");
        assert_eq!(export_table_name("my table"), "exported_rows");
        assert_eq!(export_table_name("1abc"), "exported_rows");
    }

    #[test]
    fn explains_statement_at_caret() {
        let mut q = QueryTab::new(
            1,
            None,
            "select 1;
select 2 from t;
"
            .into(),
            None,
        );
        assert_eq!(sql_to_explain(&q), "select 1");
        q.run.cursor = Some(12);
        assert_eq!(sql_to_explain(&q), "select 2 from t");
        q.run.cursor = Some(q.sql.len());
        assert_eq!(sql_to_explain(&q), "select 2 from t");
        q.run.selection = Some(0..8);
        assert_eq!(sql_to_explain(&q), "select 1");
    }

    #[test]
    fn runs_selection_when_present() {
        let mut q = QueryTab::new(1, None, "select 1;\nselect 2;".into(), None);
        assert_eq!(sql_to_run(&q), q.sql);
        q.run.selection = Some(10..19);
        assert_eq!(sql_to_run(&q), "select 2;");
        q.run.selection = Some(9..10); // whitespace-only selection falls back to all
        assert_eq!(sql_to_run(&q), q.sql);
        q.run.selection = Some(100..200); // stale range is ignored
        assert_eq!(sql_to_run(&q), q.sql);
    }

    #[test]
    fn disconnected_run_says_connecting_until_the_session_is_up() {
        use std::collections::HashMap;
        use std::time::Duration;

        let profile = ConnectionProfile::new(rdbs_core::DatabaseKind::PostgreSql);
        let connections = HashMap::new();
        assert_eq!(
            phase_for(&profile, &None, &connections),
            RunPhase::Connecting
        );

        let up = ConnState {
            status: ConnStatus::Connected {
                server_version: "PostgreSQL".into(),
                capabilities: rdbs_core::Capabilities::default(),
            },
            ..ConnState::default()
        };
        let mut connections = HashMap::new();
        connections.insert(profile.id, up);
        assert_eq!(phase_for(&profile, &None, &connections), RunPhase::Running);

        connections
            .get_mut(&profile.id)
            .unwrap()
            .dropped
            .insert(None);
        assert_eq!(
            phase_for(&profile, &None, &connections),
            RunPhase::Connecting
        );

        let elapsed = Duration::from_secs(2);
        let connecting = in_flight_status(RunPhase::Connecting, "prod", elapsed);
        let running = in_flight_status(RunPhase::Running, "prod", elapsed);
        assert!(connecting.starts_with("Connecting…"), "{connecting}");
        assert!(running.starts_with("Running…"), "{running}");
        assert_ne!(connecting, running);
        assert!(in_flight_banner(RunPhase::Connecting, "prod", elapsed).contains("Connecting…"));
        assert!(in_flight_banner(RunPhase::Running, "prod", elapsed).contains("Running…"));
    }
}
