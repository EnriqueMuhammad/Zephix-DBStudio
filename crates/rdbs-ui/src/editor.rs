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
use crate::workspace::{LastRun, QueryTab, RunState, Running, TabContext};

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

    egui::Panel::bottom(Id::new(("q-status", q.id)))
        .exact_size(24.0)
        .frame(
            Frame::new()
                .fill(color::panel())
                .inner_margin(Margin::symmetric(8, 0)),
        )
        .show(ui, |ui| status_line(ui, q));

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

fn toolbar(ui: &mut Ui, q: &mut QueryTab, cx: &mut TabContext<'_>) {
    ui.horizontal(|ui| {
        let running = q.run.running.is_some();
        let run =
            Button::new(RichText::new(format!("{}  Run", icon::PLAY)).color(color::success()));
        if ui
            .add_enabled(!running, run)
            .on_hover_text("Run (Ctrl+Enter). Runs only the selection when text is selected.")
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
            if let Some(p) = q.profile_id.and_then(|id| cx.profile(id)) {
                ui.separator();
                widgets::env_pill(ui, p.environment);
                ui.label(
                    RichText::new(format!("{}  {}", p.kind.badge(), p.name))
                        .color(color::text_dim()),
                )
                .on_hover_text(p.endpoint());
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

    let mut layouter = |ui: &Ui, buf: &dyn TextBuffer, wrap_width: f32| {
        let mut job = if white_space {
            widgets::highlight_sql_whitespace(buf.as_str(), &font)
        } else {
            widgets::highlight_sql(buf.as_str(), &font)
        };
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

/// First key of a two-key shortcut (Ctrl+K, Ctrl+C), waiting for the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chord {
    CtrlK,
    CtrlE,
    ShiftAltL,
}

fn chord_id() -> Id {
    Id::new("editor-chord")
}

/// Whether an editor waits for the second key of a shortcut; the global shortcuts then leave
/// the keyboard to it (Ctrl+E, Ctrl+W toggles word wrap instead of closing the tab).
pub fn chord_pending(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<(Id, Chord)>(chord_id()).is_some())
}

/// Forgets a chord started in `editor_id` once that editor lost focus.
fn clear_chord(ctx: &egui::Context, editor_id: Id) {
    ctx.data_mut(|d| {
        if d.get_temp::<(Id, Chord)>(chord_id())
            .is_some_and(|(owner, _)| owner == editor_id)
        {
            d.remove::<(Id, Chord)>(chord_id());
        }
    });
}

/// Removes a key press with exactly these modifiers from this frame's input.
fn take_key(i: &mut egui::InputState, modifiers: Modifiers, key: Key) -> bool {
    let found = i.events.iter().position(|e| {
        matches!(e, Event::Key { key: k, pressed: true, modifiers: m, .. }
            if *k == key && m.matches_exact(modifiers))
    });
    found.map(|p| i.events.remove(p)).is_some()
}

/// Editor shortcuts, read before the text editor so it does not act on the same keys (it
/// binds Ctrl+K and Ctrl+U to line deletions).
fn shortcut_action(ui: &mut Ui, editor_id: Id, text: &str) -> Option<EditorAction> {
    use EditorAction as A;
    const CTRL: Modifiers = Modifiers::COMMAND;
    let ctrl_shift = Modifiers::COMMAND.plus(Modifiers::SHIFT);
    let ctrl_alt = Modifiers::COMMAND.plus(Modifiers::ALT);
    let shift_alt = Modifiers::SHIFT.plus(Modifiers::ALT);
    let ctx = ui.ctx().clone();
    let chord = ctx
        .data(|d| d.get_temp::<(Id, Chord)>(chord_id()))
        .filter(|(owner, _)| *owner == editor_id)
        .map(|(_, c)| c);
    let multiline = egui::text_edit::TextEditState::load(&ctx, editor_id)
        .and_then(|s| s.cursor.char_range())
        .is_some_and(|r| {
            let r = r.as_sorted_char_range();
            text.chars()
                .skip(r.start.0)
                .take(r.end.0 - r.start.0)
                .any(|c| c == '\n')
        });
    let (action, next) = ui.input_mut(|i| {
        if let Some(chord) = chord {
            let action = match chord {
                Chord::CtrlK => {
                    // Ctrl+C arrives as a copy request on most platforms.
                    if let Some(p) = i.events.iter().position(|e| matches!(e, Event::Copy)) {
                        i.events.remove(p);
                        Some(A::Comment)
                    } else if take_key(i, CTRL, Key::C) {
                        Some(A::Comment)
                    } else if take_key(i, CTRL, Key::U) {
                        Some(A::Uncomment)
                    } else if take_key(i, CTRL, Key::Backslash) {
                        Some(A::DeleteWhiteSpace)
                    } else {
                        None
                    }
                }
                Chord::CtrlE => {
                    if take_key(i, CTRL, Key::W) {
                        Some(A::ToggleWordWrap)
                    } else if take_key(i, CTRL, Key::S) {
                        Some(A::ToggleWhiteSpace)
                    } else {
                        None
                    }
                }
                Chord::ShiftAltL => {
                    if take_key(i, shift_alt, Key::S) {
                        Some(A::SortLines)
                    } else if take_key(i, shift_alt, Key::J) {
                        Some(A::JoinLines)
                    } else {
                        None
                    }
                }
            };
            // Any other key ends the chord and is handled as usual.
            let other_key = i.events.iter().any(|e| {
                matches!(
                    e,
                    Event::Key { pressed: true, .. }
                        | Event::Text(_)
                        | Event::Copy
                        | Event::Cut
                        | Event::Paste(_)
                )
            });
            let waiting = action.is_none() && !other_key;
            return (action, waiting.then_some(chord));
        }
        if take_key(i, CTRL, Key::K) {
            return (None, Some(Chord::CtrlK));
        }
        if take_key(i, CTRL, Key::E) {
            return (None, Some(Chord::CtrlE));
        }
        if take_key(i, shift_alt, Key::L) {
            return (None, Some(Chord::ShiftAltL));
        }
        let shift_ctrl_alt = ctrl_alt.plus(Modifiers::SHIFT);
        let action = if take_key(i, ctrl_shift, Key::U) {
            Some(A::Uppercase)
        } else if take_key(i, ctrl_shift, Key::L) {
            Some(A::Lowercase)
        } else if take_key(i, Modifiers::ALT, Key::ArrowUp) {
            Some(A::MoveLinesUp)
        } else if take_key(i, Modifiers::ALT, Key::ArrowDown) {
            Some(A::MoveLinesDown)
        } else if take_key(i, ctrl_alt, Key::ArrowRight) {
            Some(A::Subword {
                forward: true,
                extend: false,
            })
        } else if take_key(i, shift_ctrl_alt, Key::ArrowRight) {
            Some(A::Subword {
                forward: true,
                extend: true,
            })
        } else if take_key(i, ctrl_alt, Key::ArrowLeft) {
            Some(A::Subword {
                forward: false,
                extend: false,
            })
        } else if take_key(i, shift_ctrl_alt, Key::ArrowLeft) {
            Some(A::Subword {
                forward: false,
                extend: true,
            })
        } else if take_key(i, shift_alt, Key::E) {
            Some(A::ExpandToLine)
        } else if take_key(i, Modifiers::SHIFT, Key::Tab) {
            Some(A::DecreaseIndent)
        } else if multiline && take_key(i, Modifiers::NONE, Key::Tab) {
            Some(A::IncreaseIndent)
        } else {
            None
        };
        (action, None)
    });
    ctx.data_mut(|d| match next {
        Some(chord) => {
            d.insert_temp(chord_id(), (editor_id, chord));
        }
        None => {
            d.remove::<(Id, Chord)>(chord_id());
        }
    });
    action
}

/// Closes brackets and quotes as they are typed and deletes an empty pair with Backspace
/// ([`textops::type_char`], [`textops::backspace`]). Only the first edit of a frame is taken
/// over, so keys typed after it in the same frame still apply in order.
fn auto_close(ui: &mut Ui, editor_id: Id, q: &mut QueryTab) {
    let ctx = ui.ctx().clone();
    let Some(sel) = current_selection(&ctx, editor_id) else {
        return;
    };
    let edit = ui.input_mut(|i| {
        let first = i.events.iter().position(|e| {
            matches!(
                e,
                Event::Text(_) | Event::Paste(_) | Event::Ime(_) | Event::Key { pressed: true, .. }
            )
        })?;
        let edit = match &i.events[first] {
            Event::Text(t) => {
                let mut chars = t.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => textops::type_char(&q.sql, sel, c),
                    _ => None,
                }
            }
            Event::Key {
                key: Key::Backspace,
                pressed: true,
                modifiers,
                ..
            } if modifiers.is_none() => textops::backspace(&q.sql, sel),
            _ => None,
        }?;
        i.events.remove(first);
        Some(edit)
    });
    if let Some(edit) = edit {
        apply_edit(&ctx, editor_id, q, edit);
    }
}

/// The editor's selection in characters.
fn current_selection(ctx: &egui::Context, editor_id: Id) -> Option<Selection> {
    let range = egui::text_edit::TextEditState::load(ctx, editor_id)?
        .cursor
        .char_range()?;
    Some(Selection::new(
        range.secondary.index.0,
        range.primary.index.0,
    ))
}

fn set_selection(ctx: &egui::Context, editor_id: Id, sel: Selection) {
    let mut state = egui::text_edit::TextEditState::load(ctx, editor_id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(sel.anchor),
            egui::text::CCursor::new(sel.caret),
        )));
    state.store(ctx, editor_id);
}

/// Replaces the editor text with `edit`, as one undo step.
fn apply_edit(ctx: &egui::Context, editor_id: Id, q: &mut QueryTab, edit: textops::Edit) {
    let mut state = egui::text_edit::TextEditState::load(ctx, editor_id).unwrap_or_default();
    if let Some(range) = state.cursor.char_range() {
        let mut undoer = state.undoer();
        undoer.add_undo(&(range, q.sql.clone()));
        state.set_undoer(undoer);
    }
    q.sql = edit.text;
    state.store(ctx, editor_id);
    set_selection(ctx, editor_id, edit.selection);
}

/// While a selection drag is held outside the editor, scroll toward the pointer and
/// keep extending the selection. `TextEdit` only follows the pointer while it is over
/// the widget, so the scroll offset and the cursor are written back after it returns.
fn extend_selection_past_editor(
    ui: &Ui,
    q: &mut QueryTab,
    editor_id: Id,
    scrolled: &egui::containers::scroll_area::ScrollAreaOutput<
        egui::widgets::text_edit::TextEditOutput,
    >,
) {
    if !ui.ctx().is_being_dragged(editor_id) {
        return;
    }
    let Some(pointer) = ui.input(|input| input.pointer.hover_pos().or(input.pointer.latest_pos()))
    else {
        return;
    };
    let view = scrolled.inner_rect;
    if view.width() <= 1.0 || view.height() <= 1.0 || view.contains(pointer) {
        return;
    }
    let delta = Vec2::new(
        outside_speed(pointer.x, view.left(), view.right()),
        outside_speed(pointer.y, view.top(), view.bottom()),
    );
    if delta == Vec2::ZERO {
        return;
    }

    let max_offset = (scrolled.content_size - view.size()).max(Vec2::ZERO);
    let mut scroll = scrolled.state;
    let next_offset = (scroll.offset + delta).clamp(Vec2::ZERO, max_offset);
    let moved = next_offset != scroll.offset;
    scroll.offset = next_offset;
    scroll.store(ui.ctx(), scrolled.id);

    let edge = egui::pos2(
        pointer.x.clamp(view.left(), view.right()),
        pointer.y.clamp(view.top(), view.bottom()),
    );
    let local = (edge - scrolled.inner.galley_pos) + delta;
    let primary = scrolled.inner.galley.cursor_from_pos(local);
    let mut text = scrolled.inner.state.clone();
    let secondary = text
        .cursor
        .char_range()
        .map(|range| range.secondary)
        .unwrap_or(primary);
    text.cursor.set_char_range(Some(egui::text::CCursorRange {
        primary,
        secondary,
        h_pos: None,
    }));
    if let Some(range) = text.cursor.char_range() {
        q.run.cursor = Some(
            widgets::char_range_to_bytes(&q.sql, range.primary.index.0, range.primary.index.0)
                .start,
        );
        let chars = range.as_sorted_char_range();
        q.run.selection = (chars.start.0 != chars.end.0)
            .then(|| widgets::char_range_to_bytes(&q.sql, chars.start.0, chars.end.0));
    }
    text.store(ui.ctx(), editor_id);
    if moved {
        ui.ctx().request_repaint();
    }
}

/// Pixels to scroll this frame when the pointer is outside one edge of the editor.
fn outside_speed(pointer: f32, min: f32, max: f32) -> f32 {
    if pointer > max {
        let over = (pointer - max).clamp(0.0, 48.0);
        12.0 + over * 0.5
    } else if pointer < min {
        let over = (min - pointer).clamp(0.0, 48.0);
        -(12.0 + over * 0.5)
    } else {
        0.0
    }
}

/// Commands of the editor's context menu and shortcuts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
    Run,
    Tabify,
    Untabify,
    Uppercase,
    Lowercase,
    MoveLinesUp,
    MoveLinesDown,
    DeleteWhiteSpace,
    ToggleWhiteSpace,
    ToggleWordWrap,
    SortLines,
    JoinLines,
    Subword { forward: bool, extend: bool },
    ExpandToLine,
    Comment,
    Uncomment,
    IncreaseIndent,
    DecreaseIndent,
}

fn editor_menu(
    ui: &mut Ui,
    has_selection: bool,
    word_wrap: bool,
    white_space: bool,
) -> Option<EditorAction> {
    use EditorAction as A;
    let mut action = None;
    let mut item = |ui: &mut Ui, enabled: bool, glyph: &str, label: &str, keys: &str, a| {
        let button = Button::new(format!("{glyph}  {label}")).shortcut_text(keys);
        if ui.add_enabled(enabled, button).clicked() {
            action = Some(a);
        }
    };
    item(ui, has_selection, icon::SCISSORS, "Cut", "Ctrl+X", A::Cut);
    item(ui, has_selection, icon::COPY, "Copy", "Ctrl+C", A::Copy);
    item(ui, true, icon::CLIPBOARD_TEXT, "Paste", "Ctrl+V", A::Paste);
    ui.separator();
    item(
        ui,
        true,
        icon::SELECTION_ALL,
        "Select All",
        "Ctrl+A",
        A::SelectAll,
    );
    ui.separator();
    item(
        ui,
        true,
        icon::CHAT_CENTERED_SLASH,
        "Comment Selection",
        "Ctrl+K, Ctrl+C",
        A::Comment,
    );
    item(
        ui,
        true,
        icon::CHAT_CENTERED,
        "Uncomment Selection",
        "Ctrl+K, Ctrl+U",
        A::Uncomment,
    );
    ui.menu_button(format!("{}  Advanced", icon::TEXT_AA), |ui| {
        let blank = " ";
        item(
            ui,
            true,
            icon::ARROW_LINE_RIGHT,
            "Tabify Selected Lines",
            "",
            A::Tabify,
        );
        item(
            ui,
            true,
            icon::ARROW_LINE_LEFT,
            "Untabify Selected Lines",
            "",
            A::Untabify,
        );
        item(
            ui,
            true,
            icon::TEXT_AA,
            "Make Uppercase",
            "Ctrl+Shift+U",
            A::Uppercase,
        );
        item(
            ui,
            true,
            blank,
            "Make Lowercase",
            "Ctrl+Shift+L",
            A::Lowercase,
        );
        item(
            ui,
            true,
            icon::ARROW_UP,
            "Move Selected Lines Up",
            "Alt+Up Arrow",
            A::MoveLinesUp,
        );
        item(
            ui,
            true,
            icon::ARROW_DOWN,
            "Move Selected Lines Down",
            "Alt+Down Arrow",
            A::MoveLinesDown,
        );
        item(
            ui,
            true,
            blank,
            "Delete Horizontal White Space",
            "Ctrl+K, Ctrl+\\",
            A::DeleteWhiteSpace,
        );
        let check = |on: bool| if on { icon::CHECK } else { " " };
        item(
            ui,
            true,
            check(white_space),
            "View White Space",
            "Ctrl+E, Ctrl+S",
            A::ToggleWhiteSpace,
        );
        item(
            ui,
            true,
            check(word_wrap),
            "Word Wrap",
            "Ctrl+E, Ctrl+W",
            A::ToggleWordWrap,
        );
        ui.separator();
        item(
            ui,
            true,
            icon::SORT_ASCENDING,
            "Sort Lines",
            "Shift+Alt+L, Shift+Alt+S",
            A::SortLines,
        );
        item(
            ui,
            true,
            blank,
            "Join Lines",
            "Shift+Alt+L, Shift+Alt+J",
            A::JoinLines,
        );
        item(
            ui,
            true,
            blank,
            "Next Subword",
            "Ctrl+Alt+Right Arrow",
            A::Subword {
                forward: true,
                extend: false,
            },
        );
        item(
            ui,
            true,
            blank,
            "Previous Subword",
            "Ctrl+Alt+Left Arrow",
            A::Subword {
                forward: false,
                extend: false,
            },
        );
        item(
            ui,
            true,
            blank,
            "Expand Selection to Line",
            "Shift+Alt+E",
            A::ExpandToLine,
        );
        ui.separator();
        item(
            ui,
            true,
            icon::TEXT_INDENT,
            "Increase Line Indent",
            "Tab",
            A::IncreaseIndent,
        );
        item(
            ui,
            true,
            icon::TEXT_OUTDENT,
            "Decrease Line Indent",
            "Shift+Tab",
            A::DecreaseIndent,
        );
    });
    ui.separator();
    let run = if has_selection {
        "Run Selection"
    } else {
        "Run"
    };
    item(ui, true, icon::PLAY, run, "Ctrl+Enter", A::Run);
    action
}

/// Carries out a context-menu or shortcut command. Clipboard commands go through the platform
/// clipboard (the backend turns them into the same events as the keyboard shortcuts).
fn apply_editor_action(
    ctx: &egui::Context,
    editor_id: Id,
    action: EditorAction,
    q: &mut QueryTab,
    cx: &mut TabContext<'_>,
) {
    use EditorAction as A;
    ctx.memory_mut(|m| m.request_focus(editor_id));
    let sel = current_selection(ctx, editor_id).unwrap_or(Selection::caret(0));
    let text = q.sql.as_str();
    let edit = match action {
        A::Cut => {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestCut);
            None
        }
        A::Copy => {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy);
            None
        }
        A::Paste => {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            None
        }
        A::SelectAll => {
            set_selection(ctx, editor_id, Selection::new(0, text.chars().count()));
            None
        }
        A::Run => {
            request_run(q, cx);
            None
        }
        A::ToggleWhiteSpace => {
            toggle_pref(ctx, WHITE_SPACE);
            None
        }
        A::ToggleWordWrap => {
            toggle_pref(ctx, WORD_WRAP);
            None
        }
        A::Subword { forward, extend } => {
            let caret = textops::subword(text, sel.caret, forward);
            let anchor = if extend { sel.anchor } else { caret };
            set_selection(ctx, editor_id, Selection::new(anchor, caret));
            None
        }
        A::ExpandToLine => {
            set_selection(ctx, editor_id, textops::expand_to_line(text, sel));
            None
        }
        A::Tabify => textops::retab(text, sel, true),
        A::Untabify => textops::retab(text, sel, false),
        A::Uppercase => textops::change_case(text, sel, true),
        A::Lowercase => textops::change_case(text, sel, false),
        A::MoveLinesUp => textops::move_lines(text, sel, true),
        A::MoveLinesDown => textops::move_lines(text, sel, false),
        A::DeleteWhiteSpace => textops::delete_horizontal_whitespace(text, sel),
        A::SortLines => textops::sort_lines(text, sel),
        A::JoinLines => textops::join_lines(text, sel),
        A::Comment => textops::toggle_comment(text, sel, true),
        A::Uncomment => textops::toggle_comment(text, sel, false),
        A::IncreaseIndent => textops::indent_lines(text, sel, true),
        A::DecreaseIndent => textops::indent_lines(text, sel, false),
    };
    if let Some(edit) = edit {
        apply_edit(ctx, editor_id, q, edit);
    }
}

/// Text that "Run" would execute: the selection if non-blank, else the whole editor.
pub fn sql_to_run(q: &QueryTab) -> String {
    if let Some(r) = &q.run.selection
        && let Some(selected) = q.sql.get(r.clone())
        && !selected.trim().is_empty()
    {
        return selected.to_owned();
    }
    q.sql.clone()
}

/// Statement to explain: the selection if non-blank, else the statement around the caret,
/// else the first statement.
pub fn sql_to_explain(q: &QueryTab) -> String {
    if let Some(r) = &q.run.selection
        && let Some(selected) = q.sql.get(r.clone())
        && !selected.trim().is_empty()
    {
        return selected.trim().to_owned();
    }
    let ranges = sql::split_statements(&q.sql);
    let at_cursor = q.run.cursor.and_then(|c| {
        ranges
            .iter()
            .find(|r| r.start <= c && c <= r.end)
            .or_else(|| ranges.iter().rev().find(|r| r.end <= c))
    });
    at_cursor
        .or(ranges.first())
        .and_then(|r| q.sql.get(r.clone()))
        .map(|s| s.trim().trim_end_matches(';').trim().to_owned())
        .unwrap_or_default()
}

/// Requests the execution plan of [`sql_to_explain`].
pub fn request_explain(q: &mut QueryTab, cx: &mut TabContext<'_>, analyze: bool) {
    let Some(profile) = q.profile_id.and_then(|id| cx.profile(id)).cloned() else {
        cx.actions.push(UiAction::Status(StatusMessage::error(
            "Choose a connection for this query first",
        )));
        return;
    };
    if needs_password(&profile, cx.connections, cx.passwords) {
        cx.actions.push(UiAction::NeedPassword {
            profile_id: profile.id,
            then: AfterPassword::Connect,
        });
        return;
    }
    let statement = sql_to_explain(q);
    let kind = sql::classify(&statement);
    if kind == sql::StatementKind::Empty {
        cx.actions
            .push(UiAction::Status(StatusMessage::info("Nothing to explain")));
        return;
    }
    if analyze && profile.environment.requires_write_confirmation() && !kind.is_read_only() {
        cx.actions.push(UiAction::Status(StatusMessage::error(
            "Explain Analyze executes the statement; it is only allowed for read-only \
             statements on production connections",
        )));
        return;
    }
    let request_id = cx.services.next_request_id();
    cx.services.send(Command::Explain {
        request_id,
        profile: profile.clone(),
        database: q.database.clone(),
        sql: statement,
        analyze,
        password: cx.passwords.get(&profile.id).cloned(),
    });
    q.run.plan_request = Some((request_id, Instant::now(), analyze));
    q.run.view = crate::workspace::ResultView::Plan;
    q.run.plan_view.selected = None;
}

/// Validates and dispatches a query, asking for confirmation on production when needed.
pub fn request_run(q: &mut QueryTab, cx: &mut TabContext<'_>) {
    if q.run.running.is_some() || q.run.saving.is_some() {
        return;
    }
    if !q.run.edits.is_empty() {
        cx.actions
            .push(UiAction::Status(StatusMessage::error(format!(
                "Save or discard the {} pending row change(s) before running again",
                q.run.edits.row_count()
            ))));
        return;
    }
    let Some(profile) = q.profile_id.and_then(|id| cx.profile(id)).cloned() else {
        cx.actions.push(UiAction::Status(StatusMessage::error(
            "Choose a connection for this query first",
        )));
        return;
    };
    if needs_password(&profile, cx.connections, cx.passwords) {
        cx.actions.push(UiAction::NeedPassword {
            profile_id: profile.id,
            then: AfterPassword::RunTab(q.id),
        });
        return;
    }
    if let Some(view) = &q.data_view {
        q.sql = view.sql();
        q.run.selection = None;
    }
    let sql_text = sql_to_run(q);
    let kind = sql::classify(&sql_text);
    if kind == sql::StatementKind::Empty {
        cx.actions
            .push(UiAction::Status(StatusMessage::info("Nothing to execute")));
        return;
    }
    if profile.environment.requires_write_confirmation() && !kind.is_read_only() {
        cx.actions.push(UiAction::ConfirmExecute {
            tab_id: q.id,
            profile_id: profile.id,
            sql: sql_text,
            kind,
        });
        return;
    }
    let password = cx.passwords.get(&profile.id).cloned();
    start_query(q, &profile, sql_text, cx.services, password);
}

/// Whether a password must be asked for before using `profile`: it does not save its
/// password, none was entered this session, and no session is open yet.
pub fn needs_password(
    profile: &ConnectionProfile,
    connections: &std::collections::HashMap<rdbs_core::ProfileId, ConnState>,
    passwords: &std::collections::HashMap<rdbs_core::ProfileId, String>,
) -> bool {
    profile.kind.is_networked()
        && !profile.save_password
        && !passwords.contains_key(&profile.id)
        && !connections
            .get(&profile.id)
            .is_some_and(ConnState::is_connected)
}

/// Sends the query to the worker and marks the tab as running.
pub fn start_query(
    q: &mut QueryTab,
    profile: &ConnectionProfile,
    sql_text: String,
    services: &ServiceHandle,
    password: Option<String>,
) {
    let request_id = services.next_request_id();
    let executed = sql_text.clone();
    services.send(Command::Execute {
        request_id,
        profile: profile.clone(),
        sql: sql_text,
        password,
        database: q.database.clone(),
    });
    q.run.running = Some(Running {
        request_id,
        started: Instant::now(),
        profile_name: profile.name.clone(),
        profile_id: profile.id,
        sql: executed,
        database: q.database.clone(),
    });
    q.run.show_messages = false;
    q.run.view = crate::workspace::ResultView::Results;
}

fn results(ui: &mut Ui, q: &mut QueryTab, cx: &mut TabContext<'_>) {
    use crate::workspace::ResultView;
    let has_plan = q.run.plan.is_some() || q.run.plan_request.is_some();
    if has_plan && q.run.running.is_none() {
        Frame::new()
            .fill(color::panel())
            .inner_margin(Margin::symmetric(8, 3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let view = &mut q.run.view;
                    ui.selectable_value(
                        view,
                        ResultView::Results,
                        format!("{}  Results", icon::TABLE),
                    );
                    ui.selectable_value(
                        view,
                        ResultView::Plan,
                        format!("{}  Plan", icon::TREE_STRUCTURE),
                    );
                });
            });
    }
    if q.run.view == ResultView::Plan && has_plan && q.run.running.is_none() {
        if let Some((request_id, started, analyze)) = q.run.plan_request {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.35);
                ui.add(egui::Spinner::new().size(22.0));
                ui.label(
                    RichText::new(format!(
                        "{} … {}",
                        if analyze {
                            "Running Explain Analyze"
                        } else {
                            "Fetching plan"
                        },
                        format_duration(started.elapsed())
                    ))
                    .color(color::text_dim()),
                );
                let _ = request_id;
            });
            return;
        }
        match &q.run.plan {
            Some(Ok(plan)) => crate::plan::show(ui, plan, &mut q.run.plan_view),
            Some(Err(e)) => error_panel(ui, e),
            None => {}
        }
        return;
    }
    let kind = q.profile_id.and_then(|id| cx.profile(id)).map(|p| p.kind);
    let data_tab = q.data_view.is_some();
    let export_table = export_table_name(&q.title);
    let table = q.table.clone();
    let RunState {
        running,
        last,
        grid: grid_state,
        stacked_grids,
        show_messages,
        active_result,
        edits,
        editing,
        saving,
        edit_error,
        ..
    } = &mut q.run;

    if let Some(r) = running {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.35);
                ui.add(egui::Spinner::new().size(22.0));
                ui.label(
                    RichText::new(format!(
                        "Executing on {} … {}",
                        r.profile_name,
                        format_duration(r.started.elapsed())
                    ))
                    .color(color::text_dim()),
                );
                if ui.button(format!("{}  Cancel", icon::STOP)).clicked() {
                    cx.services.send(Command::Cancel {
                        request_id: r.request_id,
                    });
                }
            });
        });
        return;
    }

    let Some(last) = last else {
        placeholder(
            ui,
            icon::TABLE,
            "Run a query to see results here",
            "Ctrl+Enter runs the editor contents or the current selection",
        );
        return;
    };

    match &last.outcome {
        Err(e) if e.kind == DbErrorKind::Cancelled => {
            placeholder(
                ui,
                icon::PROHIBIT,
                "Query cancelled",
                "No rows were returned",
            );
        }
        Err(e) => error_panel(ui, e),
        Ok(outcome) => {
            let count = outcome.results.len();
            if *active_result >= count {
                *active_result = count.saturating_sub(1);
            }
            // A data tab shows one result set: its row buttons, Messages and Export share one
            // bar under the toolbar.
            if data_tab && count <= 1 {
                let rs = outcome.results.first();
                let edit_state = match (&table, rs) {
                    (Some(t), Some(rs)) if !*show_messages => Some(editability(t, rs, count)),
                    _ => None,
                };
                Frame::new()
                    .fill(color::panel())
                    .inner_margin(Margin::symmetric(8, 3))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add_enabled_ui(rs.is_some(), |ui| {
                                    ui.menu_button(format!("{}  Export", icon::EXPORT), |ui| {
                                        if let Some(rs) = rs {
                                            export_menu(ui, rs, kind, &export_table, cx.actions);
                                        }
                                    });
                                });
                                let messages_label = if outcome.messages.is_empty() {
                                    format!("{}  Messages", icon::CHAT_TEXT)
                                } else {
                                    format!(
                                        "{}  Messages ({})",
                                        icon::CHAT_TEXT,
                                        outcome.messages.len()
                                    )
                                };
                                if ui
                                    .selectable_label(
                                        *show_messages || rs.is_none(),
                                        messages_label,
                                    )
                                    .on_hover_text("Server messages (click again for the rows)")
                                    .clicked()
                                {
                                    *show_messages = !*show_messages;
                                }
                                let (Some(state), Some(rs)) = (&edit_state, rs) else {
                                    return;
                                };
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    if edit_actions(
                                        ui,
                                        state,
                                        rs,
                                        edits,
                                        grid_state,
                                        saving.is_some(),
                                    ) {
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                ui.add_space(8.0);
                                                save_actions(
                                                    ui,
                                                    q.id,
                                                    rs,
                                                    table.as_ref(),
                                                    edits,
                                                    saving.is_some(),
                                                    edit_error,
                                                    cx.actions,
                                                );
                                            },
                                        );
                                    }
                                });
                            });
                        });
                        edit_error_label(ui, edit_error);
                    });
                match rs {
                    Some(rs) if !*show_messages => {
                        let mode = matches!(edit_state, Some(Editability::Editable(_)))
                            .then_some(grid::EditMode { edits, editing });
                        grid::show(ui, Id::new(("grid", q.id, 0)), rs, grid_state, mode);
                    }
                    _ => messages(ui, last),
                }
                return;
            }
            // Several result sets: stacked one above the other (remembered across runs) or as
            // tabs.
            let stacked_id = Id::new("results-stacked");
            let mut stacked = ui
                .ctx()
                .data_mut(|d| *d.get_persisted_mut_or_insert_with(stacked_id, || true));
            let stacked_view = stacked && count > 1;
            Frame::new()
                .fill(color::panel())
                .inner_margin(Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if stacked_view {
                            let label = format!("{}  Results ({count})", icon::TABLE);
                            if ui.selectable_label(!*show_messages, label).clicked() {
                                *show_messages = false;
                            }
                        }
                        for i in (0..count).filter(|_| !stacked_view) {
                            let label = if count == 1 {
                                format!("{}  Result", icon::TABLE)
                            } else {
                                format!("{}  Result {}", icon::TABLE, i + 1)
                            };
                            let selected = !*show_messages && *active_result == i;
                            if ui.selectable_label(selected, label).clicked() {
                                *show_messages = false;
                                if *active_result != i {
                                    *active_result = i;
                                    *grid_state = grid::GridState::default();
                                }
                            }
                        }
                        let messages_label = if outcome.messages.is_empty() {
                            format!("{}  Messages", icon::CHAT_TEXT)
                        } else {
                            format!("{}  Messages ({})", icon::CHAT_TEXT, outcome.messages.len())
                        };
                        if ui
                            .selectable_label(*show_messages || count == 0, messages_label)
                            .clicked()
                        {
                            *show_messages = true;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let rs = outcome.results.get(*active_result);
                            ui.add_enabled_ui(rs.is_some(), |ui| {
                                ui.menu_button(format!("{}  Export", icon::EXPORT), |ui| {
                                    if stacked_view {
                                        for (i, rs) in outcome.results.iter().enumerate() {
                                            ui.menu_button(format!("Result {}", i + 1), |ui| {
                                                export_menu(
                                                    ui,
                                                    rs,
                                                    kind,
                                                    &export_table,
                                                    cx.actions,
                                                );
                                            });
                                        }
                                    } else if let Some(rs) = rs {
                                        export_menu(ui, rs, kind, &export_table, cx.actions);
                                    }
                                });
                            });
                            if count > 1
                                && ui
                                    .selectable_label(stacked, format!("{}  Stacked", icon::ROWS))
                                    .on_hover_text(
                                        "Show all result sets one above the other instead of as tabs",
                                    )
                                    .clicked()
                            {
                                stacked = !stacked;
                                ui.ctx().data_mut(|d| d.insert_persisted(stacked_id, stacked));
                            }
                        });
                    });
                });
            if stacked_view && !*show_messages {
                stacked_results(ui, q.id, &outcome.results, stacked_grids);
                return;
            }
            match outcome.results.get(*active_result) {
                Some(rs) if !*show_messages => {
                    let edit_state = table
                        .as_ref()
                        .map(|t| editability(t, rs, outcome.results.len()));
                    if let Some(state) = &edit_state {
                        edit_bar(
                            ui,
                            q.id,
                            state,
                            rs,
                            table.as_ref(),
                            edits,
                            grid_state,
                            saving.is_some(),
                            edit_error,
                            cx.actions,
                        );
                    }
                    let mode = matches!(edit_state, Some(Editability::Editable(_)))
                        .then_some(grid::EditMode { edits, editing });
                    grid::show(
                        ui,
                        Id::new(("grid", q.id, *active_result)),
                        rs,
                        grid_state,
                        mode,
                    );
                }
                _ => messages(ui, last),
            }
        }
    }
}

/// All result sets one above the other, each grid with its own scrolling and as tall as its
/// rows need (up to an equal share of the space), separated by a divider.
fn stacked_results(
    ui: &mut Ui,
    tab_id: u64,
    results: &[rdbs_core::ResultSet],
    states: &mut Vec<grid::GridState>,
) {
    const DIVIDER: f32 = 4.0;
    states.resize_with(results.len(), grid::GridState::default);
    #[allow(clippy::cast_precision_loss)]
    let share = (ui.available_height() / results.len() as f32 - DIVIDER).max(120.0);
    ScrollArea::vertical()
        .id_salt(("stacked-results", tab_id))
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, (rs, state)) in results.iter().zip(states.iter_mut()).enumerate() {
                if i > 0 {
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), DIVIDER),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(rect, 0, color::surface_active());
                }
                let height = grid::fitted_height(rs).clamp(grid::empty_height(), share);
                let width = ui.available_width();
                ui.allocate_ui(Vec2::new(width, height), |ui| {
                    ui.set_min_height(height);
                    grid::show(ui, Id::new(("grid-stacked", tab_id, i)), rs, state, None);
                });
            }
        });
}

/// Whether the grid of a table tab can be edited.
enum Editability {
    Editable(rdbs_core::edit::RowMatch),
    ReadOnly(String),
}

fn editability(
    table: &rdbs_core::edit::TableTarget,
    rs: &rdbs_core::ResultSet,
    sets: usize,
) -> Editability {
    use rdbs_core::edit::EditError;
    if sets != 1 {
        return Editability::ReadOnly("read-only: the script returned several result sets".into());
    }
    match rdbs_core::edit::row_match(table, rs) {
        Ok(m) => Editability::Editable(m),
        Err(e) => Editability::ReadOnly(match e {
            EditError::NoPrimaryKey => "read-only: the table has no primary key".into(),
            EditError::MissingKeyColumn(k) => {
                format!("read-only: include primary key column {k} to edit")
            }
            EditError::MissingColumn(c) => {
                format!("read-only: no primary key; include all columns ({c} is missing) to edit")
            }
            EditError::NoComparableColumn => {
                "read-only: no primary key and no column that can be compared".into()
            }
            EditError::UnusableKey(_) => format!("read-only: {e}"),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn edit_bar(
    ui: &mut Ui,
    tab_id: u64,
    state: &Editability,
    rs: &rdbs_core::ResultSet,
    table: Option<&rdbs_core::edit::TableTarget>,
    edits: &mut rdbs_core::edit::PendingEdits,
    grid_state: &mut grid::GridState,
    saving: bool,
    edit_error: &mut Option<String>,
    actions: &mut Vec<UiAction>,
) {
    Frame::new()
        .fill(color::editor())
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if edit_actions(ui, state, rs, edits, grid_state, saving) {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        save_actions(ui, tab_id, rs, table, edits, saving, edit_error, actions);
                    });
                }
            });
            edit_error_label(ui, edit_error);
        });
}

/// Left part of the edit controls: why the grid is read-only, or the row buttons followed by
/// a hint or the number of pending changes. Returns whether there are changes to save.
fn edit_actions(
    ui: &mut Ui,
    state: &Editability,
    rs: &rdbs_core::ResultSet,
    edits: &mut rdbs_core::edit::PendingEdits,
    grid_state: &mut grid::GridState,
    saving: bool,
) -> bool {
    let row_match = match state {
        Editability::Editable(m) => *m,
        Editability::ReadOnly(reason) => {
            ui.label(
                RichText::new(format!("{}  {reason}", icon::LOCK))
                    .size(12.0)
                    .color(color::text_faint()),
            );
            return false;
        }
    };
    ui.add_enabled_ui(!saving, |ui| {
        if ui
            .button(format!("{}  Row", icon::PLUS))
            .on_hover_text("Add a new row")
            .clicked()
        {
            edits.add_row(rs.columns.len());
            grid_state.selected = Some((rs.rows.len() + edits.inserted.len() - 1, 0));
        }
        let selected_row = grid_state
            .selected
            .map(|(r, _)| r)
            .filter(|r| *r < rs.rows.len());
        if ui
            .add_enabled(
                selected_row.is_some(),
                Button::new(format!("{}  Delete", icon::TRASH)),
            )
            .on_hover_text("Mark the selected row for deletion (toggle)")
            .clicked()
            && let Some(r) = selected_row
        {
            edits.toggle_delete(r);
        }
    });
    if row_match == rdbs_core::edit::RowMatch::AllColumns {
        // Icon only while changes are pending, to leave room for the count and save buttons.
        let text = if edits.is_empty() {
            format!("{}  no primary key", icon::WARNING)
        } else {
            icon::WARNING.to_owned()
        };
        ui.label(RichText::new(text).size(12.0).color(color::warning()))
            .on_hover_text(
                "Rows are found by the values of all their columns. A change that would touch \
             more than one row is rolled back; of identical rows shown in the grid, one is \
             changed.",
            );
    }
    if edits.is_empty() {
        ui.label(
            RichText::new("Double-click a cell to edit")
                .size(12.0)
                .color(color::text_faint()),
        );
        return false;
    }
    ui.separator();
    ui.label(
        RichText::new(format!("{} pending", edits.row_count()))
            .size(12.0)
            .color(color::warning()),
    );
    true
}

/// Save, Discard and Copy SQL, for a right-to-left layout.
#[allow(clippy::too_many_arguments)]
fn save_actions(
    ui: &mut Ui,
    tab_id: u64,
    rs: &rdbs_core::ResultSet,
    table: Option<&rdbs_core::edit::TableTarget>,
    edits: &mut rdbs_core::edit::PendingEdits,
    saving: bool,
    edit_error: &mut Option<String>,
    actions: &mut Vec<UiAction>,
) {
    ui.add_enabled_ui(!saving, |ui| {
        let save = Button::new(
            RichText::new(format!("{}  Save changes", icon::FLOPPY_DISK))
                .color(egui::Color32::WHITE),
        )
        .fill(color::accent());
        if ui.add(save).clicked() {
            *edit_error = None;
            actions.push(UiAction::SaveEdits { tab_id });
        }
        if ui
            .button(format!("{}  Discard", icon::ARROW_CLOCKWISE))
            .clicked()
        {
            *edits = rdbs_core::edit::PendingEdits::default();
            *edit_error = None;
        }
        if ui
            .button(format!("{}  Copy SQL", icon::COPY))
            .on_hover_text("Copy the statements that Save will run")
            .clicked()
            && let Some(t) = table
        {
            match rdbs_core::edit::build_changes(t, rs, edits) {
                Ok(changes) => {
                    let script = changes
                        .iter()
                        .map(|c| format!("{};", c.sql))
                        .collect::<Vec<_>>()
                        .join(
                            "
",
                        );
                    ui.ctx().copy_text(script);
                }
                Err(e) => *edit_error = Some(e.to_string()),
            }
        }
    });
    if saving {
        ui.add(egui::Spinner::new().size(14.0));
    }
}

fn edit_error_label(ui: &mut Ui, edit_error: &Option<String>) {
    if let Some(err) = edit_error {
        ui.add(
            egui::Label::new(
                RichText::new(format!("{}  {err}", icon::WARNING)).color(color::danger()),
            )
            .wrap(),
        );
    }
}

/// Table name used for SQL `INSERT` exports: the tab title when it is a plain identifier
/// (tabs opened from the explorer are titled after their table).
pub(crate) fn export_table_name(title: &str) -> String {
    let plain = !title.is_empty()
        && title.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !title.starts_with(|c: char| c.is_ascii_digit());
    if plain && !title.starts_with("Query ") {
        title.to_owned()
    } else {
        "exported_rows".to_owned()
    }
}

fn export_menu(
    ui: &mut Ui,
    rs: &rdbs_core::ResultSet,
    kind: Option<rdbs_core::DatabaseKind>,
    table: &str,
    actions: &mut Vec<UiAction>,
) {
    use rdbs_core::export::{ExportFormat, export};
    let kind = kind.unwrap_or(rdbs_core::DatabaseKind::PostgreSql);
    let rows = format_count(rs.rows.len() as u64);
    ui.label(
        RichText::new(format!("{rows} rows"))
            .small()
            .color(color::text_faint()),
    );
    for format in ExportFormat::ALL {
        if ui
            .button(format!("{}  Copy as {}", icon::COPY, format.label()))
            .clicked()
        {
            ui.ctx().copy_text(export(rs, format, kind, table));
            actions.push(UiAction::Status(StatusMessage::info(format!(
                "Copied {rows} rows as {}",
                format.label()
            ))));
        }
    }
    ui.separator();
    for format in ExportFormat::ALL {
        if ui
            .button(format!(
                "{}  Save as {}…",
                icon::FLOPPY_DISK,
                format.label()
            ))
            .clicked()
        {
            actions.push(UiAction::SaveExport {
                contents: export(rs, format, kind, table),
                file_name: format!("{table}.{}", format.extension()),
                format,
            });
        }
    }
}

/// Messages view: one line per statement in execution order ("(3 rows affected)"), server
/// messages, then when and how long the script ran.
fn messages(ui: &mut Ui, last: &LastRun) {
    let Ok(outcome) = &last.outcome else { return };
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        Frame::new().inner_margin(Margin::same(12)).show(ui, |ui| {
            let mono = theme::mono();
            let mut line = |text: String, c| {
                ui.add(
                    egui::Label::new(RichText::new(text).font(mono.clone()).color(c))
                        .selectable(true),
                );
            };
            if outcome.messages.is_empty() {
                line("Commands completed successfully.".to_owned(), color::text());
            }
            for m in &outcome.messages {
                let c = if m.starts_with('(') {
                    color::text()
                } else {
                    color::text_dim()
                };
                line(m.clone(), c);
            }
            line(String::new(), color::text());
            line(
                format!(
                    "Completion time: {}  ·  {}  ·  {}",
                    last.finished.format("%Y-%m-%d %H:%M:%S"),
                    format_duration(last.elapsed),
                    last.profile_name
                ),
                color::text_faint(),
            );
        });
    });
}

fn error_panel(ui: &mut Ui, e: &rdbs_core::DbError) {
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        Frame::new()
            .fill(color::danger().gamma_multiply(0.08))
            .stroke(Stroke::new(1.0, color::danger().gamma_multiply(0.4)))
            .corner_radius(6)
            .inner_margin(Margin::same(12))
            .outer_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{}  Query failed", icon::WARNING))
                            .strong()
                            .color(color::danger()),
                    );
                    if let Some(code) = &e.code {
                        ui.label(
                            RichText::new(format!("code {code}"))
                                .font(theme::mono())
                                .color(color::text_dim()),
                        );
                    }
                });
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(RichText::new(&e.message).font(theme::mono()))
                        .selectable(true),
                );
                if let Some(detail) = &e.detail {
                    ui.label(RichText::new(detail).color(color::text_dim()));
                }
            });
    });
}

fn placeholder(ui: &mut Ui, glyph: &str, title: &str, hint: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.3).max(12.0));
        ui.label(RichText::new(glyph).size(30.0).color(color::text_faint()));
        ui.label(RichText::new(title).color(color::text_dim()));
        ui.label(RichText::new(hint).small().color(color::text_faint()));
    });
}

fn status_line(ui: &mut Ui, q: &QueryTab) {
    ui.horizontal_centered(|ui| {
        let small = |t: String, c| RichText::new(t).size(12.0).color(c);
        if let Some(r) = &q.run.running {
            ui.add(egui::Spinner::new().size(12.0));
            ui.label(small(
                format!(
                    "Running on {}  ·  {}",
                    r.profile_name,
                    format_duration(r.started.elapsed())
                ),
                color::text_dim(),
            ));
        } else if let Some(last) = &q.run.last {
            let (text, c) = match &last.outcome {
                Ok(o) => {
                    let shown = q.run.active_result_set();
                    let mut what = match (shown, o.rows_affected) {
                        (Some(rs), _) => format!(
                            "{} rows{}",
                            format_count(rs.rows.len() as u64),
                            if o.truncated { " (truncated)" } else { "" }
                        ),
                        (None, Some(n)) => format!("{} rows affected", format_count(n)),
                        (None, None) => "OK".to_owned(),
                    };
                    if o.results.len() > 1 {
                        what.push_str(&format!("  ·  {} result sets", o.results.len()));
                    }
                    (format!("{}  {what}", icon::CHECK_CIRCLE), color::success())
                }
                Err(e) if e.kind == DbErrorKind::Cancelled => {
                    (format!("{}  Cancelled", icon::PROHIBIT), color::warning())
                }
                Err(_) => (format!("{}  Error", icon::X_CIRCLE), color::danger()),
            };
            ui.label(small(text, c));
            ui.label(small(
                format!(
                    "·  {}  ·  {}",
                    format_duration(last.elapsed),
                    last.profile_name
                ),
                color::text_dim(),
            ));
        } else {
            ui.label(small("Ready".to_owned(), color::text_dim()));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(r) = &q.run.selection {
                ui.label(small(
                    format!(
                        "Selection: {} chars",
                        q.sql.get(r.clone()).map_or(0, |s| s.chars().count())
                    ),
                    color::text_dim(),
                ));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
