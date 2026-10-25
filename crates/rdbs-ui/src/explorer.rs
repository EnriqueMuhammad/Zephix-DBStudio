//! Connection explorer sidebar: profiles and their object trees.

use crate::tools::{self, ToolLaunch};
use std::collections::{HashMap, HashSet};

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Response, RichText, Sense, Ui, Vec2};
use egui_phosphor::regular as icon;
use rdbs_core::{ConnectionProfile, DbObject, ObjectKind, ProfileId};

use crate::model::{ConnState, ConnStatus, ObjectRef, UiAction};
use crate::theme::{self, color};
use crate::widgets;

const ROW_HEIGHT: f32 = 22.0;
const INDENT: f32 = 14.0;

/// Explorer view state.
#[derive(Debug, Default)]
pub struct ExplorerState {
    /// Filter text typed by the user.
    pub filter: String,
    /// Keys of expanded nodes.
    expanded: HashSet<String>,
    /// Key of the selected node.
    selected: Option<String>,
    /// Object of the selected node, when it is a table or view.
    pub selected_object: Option<ObjectRef>,
    /// Profile of the selected node.
    pub selected_profile: Option<ProfileId>,
    /// Request keyboard focus for the filter box next frame.
    pub focus_filter: bool,
}

impl ExplorerState {
    /// Expands the node of a connection (used after connecting from elsewhere).
    pub fn expand_profile(&mut self, id: ProfileId) {
        self.expanded.insert(profile_key(id));
    }

    /// Collapses a connection node.
    pub fn collapse_profile(&mut self, id: ProfileId) {
        self.expanded.remove(&profile_key(id));
    }
}

fn profile_key(id: ProfileId) -> String {
    id.to_string()
}

/// Inputs the explorer needs for one frame.
pub struct ExplorerContext<'a> {
    /// All profiles.
    pub profiles: &'a [ConnectionProfile],
    /// Runtime connection state.
    pub connections: &'a HashMap<ProfileId, ConnState>,
    /// Actions requested during this frame.
    pub actions: &'a mut Vec<UiAction>,
}

/// Renders the explorer.
pub fn show(ui: &mut Ui, state: &mut ExplorerState, cx: &mut ExplorerContext<'_>) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("CONNECTIONS")
                .size(11.0)
                .strong()
                .color(color::text_dim()),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button(icon::PLUS)
                .on_hover_text("New connection (Ctrl+N)")
                .clicked()
            {
                cx.actions.push(UiAction::NewConnection);
            }
            if ui
                .small_button(icon::ARROWS_CLOCKWISE)
                .on_hover_text("Refresh selected connection (F5)")
                .clicked()
                && let Some(id) = state.selected_profile
            {
                cx.actions.push(UiAction::Refresh(id));
            }
        });
    });

    let filter_id = egui::Id::new("explorer-filter");
    if std::mem::take(&mut state.focus_filter) {
        ui.memory_mut(|m| m.request_focus(filter_id));
    }
    ui.add(
        egui::TextEdit::singleline(&mut state.filter)
            .id(filter_id)
            .hint_text(format!(
                "{}  Filter objects (Ctrl+F)",
                icon::MAGNIFYING_GLASS
            ))
            .desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);

    let needle = state.filter.trim().to_lowercase();
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if cx.profiles.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("No connections yet").color(color::text_dim()));
                    if ui
                        .button(format!("{}  New connection", icon::PLUS))
                        .clicked()
                    {
                        cx.actions.push(UiAction::NewConnection);
                    }
                });
            }
            for profile in cx.profiles {
                profile_node(ui, state, cx, profile, &needle);
            }
        });
}

fn profile_node(
    ui: &mut Ui,
    state: &mut ExplorerState,
    cx: &mut ExplorerContext<'_>,
    profile: &ConnectionProfile,
    needle: &str,
) {
    let conn = cx.connections.get(&profile.id);
    let status = conn.map(|c| &c.status);
    let objects = conn.map_or(&[][..], |c| c.objects.as_slice());
    let name_matches = profile.name.to_lowercase().contains(needle);
    if !needle.is_empty() && !name_matches && !objects.iter().any(|o| o.matches_filter(needle)) {
        return;
    }

    let key = profile_key(profile.id);
    let connected = matches!(status, Some(ConnStatus::Connected { .. }));
    let open = state.expanded.contains(&key) || (!needle.is_empty() && connected);

    let row = TreeRow {
        depth: 0,
        expandable: true,
        open,
        selected: state.selected.as_deref() == Some(key.as_str()),
        // Reordering a filtered list would be confusing.
        draggable: needle.is_empty(),
    };
    let (response, toggled) = row.show(ui, |ui, rect| {
        let badge = Rect::from_center_size(
            egui::pos2(rect.left() + 9.0, rect.center().y),
            Vec2::splat(16.0),
        );
        widgets::paint_kind_badge(ui, badge, profile.kind);
        let mut x = badge.right() + 7.0;
        let text_color = if connected {
            color::text()
        } else {
            color::text_dim()
        };
        let shows_env = profile.environment != rdbs_core::Environment::Development;
        let right_limit = rect.right() - 22.0;
        let name_width = (right_limit - if shows_env { 36.0 } else { 0.0 } - x).max(8.0);
        let name = fit_label(
            ui,
            &profile.name,
            FontId::proportional(13.5),
            text_color,
            name_width,
        );
        let painter = ui.painter_at(rect);
        let painted = name.size();
        painter.galley(
            egui::pos2(x, rect.center().y - painted.y / 2.0),
            name,
            text_color,
        );
        x += painted.x + 6.0;
        if shows_env && x + 28.0 <= right_limit {
            let c = theme::env_color(profile.environment);
            let label = if profile.environment == rdbs_core::Environment::Production {
                "PROD"
            } else {
                "STG"
            };
            painter.text(
                egui::pos2(x, rect.center().y),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(9.5),
                c,
            );
        }
        let right = egui::pos2(rect.right() - 10.0, rect.center().y);
        match status {
            Some(ConnStatus::Connecting) => {
                let r = Rect::from_center_size(right, Vec2::splat(12.0));
                egui::Spinner::new().size(12.0).paint_at(ui, r);
            }
            Some(ConnStatus::Connected { .. }) => {
                ui.painter().circle_filled(right, 3.5, color::success());
            }
            Some(ConnStatus::Failed(_)) => {
                ui.painter().text(
                    right,
                    Align2::CENTER_CENTER,
                    icon::WARNING,
                    FontId::proportional(13.0),
                    color::danger(),
                );
            }
            _ => {}
        }
    });

    let server = match status {
        Some(ConnStatus::Connected { server_version, .. }) => {
            format!("\nConnected · {server_version}")
        }
        _ => String::new(),
    };
    let response = response.on_hover_text(format!(
        "{}\n{} · {}\n{}{server}\nDrag to reorder",
        profile.name,
        profile.kind,
        profile.environment.label(),
        profile.endpoint()
    ));

    // Drag a connection onto another one to move it above or below it.
    if needle.is_empty() {
        response.dnd_set_drag_payload(profile.id);
        if response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        let below = ui
            .input(|i| i.pointer.latest_pos())
            .is_some_and(|p| p.y > response.rect.center().y);
        if let Some(dragged) = response.dnd_hover_payload::<ProfileId>()
            && *dragged != profile.id
        {
            let y = if below {
                response.rect.bottom()
            } else {
                response.rect.top()
            };
            ui.painter().hline(
                response.rect.x_range(),
                y,
                egui::Stroke::new(2.0, color::accent()),
            );
        }
        if let Some(dragged) = response.dnd_release_payload::<ProfileId>()
            && *dragged != profile.id
        {
            cx.actions.push(UiAction::MoveConnection {
                id: *dragged,
                target: profile.id,
                below,
            });
        }
    }

    if response.clicked() {
        state.selected = Some(key.clone());
        state.selected_profile = Some(profile.id);
        state.selected_object = None;
    }
    let wants_open = toggled || response.double_clicked();
    if wants_open {
        if open {
            state.expanded.remove(&key);
        } else {
            state.expanded.insert(key.clone());
            if matches!(
                status,
                None | Some(ConnStatus::Disconnected | ConnStatus::Failed(_))
            ) {
                cx.actions.push(UiAction::Connect(profile.id));
            } else if connected && objects.is_empty() {
                // Opened by a tool tab (schema diff, transfer, ...) without loading the tree.
                cx.actions.push(UiAction::Refresh(profile.id));
            }
        }
    }

    response.context_menu(|ui| {
        state.selected = Some(key.clone());
        state.selected_profile = Some(profile.id);
        profile_menu(ui, profile, connected, cx.actions);
    });

    if !open {
        return;
    }
    match status {
        Some(ConnStatus::Connecting) => hint_row(ui, 1, "Connecting…", color::text_dim()),
        Some(ConnStatus::Failed(e)) => {
            hint_row(ui, 1, &e.to_string(), color::danger());
        }
        Some(ConnStatus::Connected { .. }) => {
            let path = PathCtx {
                profile_id: profile.id,
                kind: profile.kind,
                database: None,
                schema: None,
            };
            if objects.is_empty() {
                hint_row(
                    ui,
                    1,
                    "No objects loaded (F5 refreshes)",
                    color::text_faint(),
                );
            }
            for obj in objects {
                let loading = conn.map(|c| &c.loading_databases);
                object_node(ui, state, cx.actions, obj, &key, 1, &path, needle, loading);
            }
        }
        _ => hint_row(ui, 1, "Not connected", color::text_faint()),
    }
}

fn profile_menu(
    ui: &mut Ui,
    profile: &ConnectionProfile,
    connected: bool,
    actions: &mut Vec<UiAction>,
) {
    if connected {
        if ui
            .button(format!("{}  Close Connection", icon::PLUGS))
            .clicked()
        {
            actions.push(UiAction::Disconnect(profile.id));
        }
    } else if ui
        .button(format!("{}  Open Connection", icon::PLUGS_CONNECTED))
        .clicked()
    {
        actions.push(UiAction::Connect(profile.id));
    }
    if ui
        .button(format!("{}  New Query", icon::FILE_SQL))
        .clicked()
    {
        actions.push(UiAction::NewQuery {
            profile_id: Some(profile.id),
            sql: None,
            title: None,
            run: false,
        });
    }
    if ui
        .add_enabled(
            connected,
            egui::Button::new(format!("{}  Refresh", icon::ARROWS_CLOCKWISE)),
        )
        .clicked()
    {
        actions.push(UiAction::Refresh(profile.id));
    }
    ui.separator();
    if ui
        .button(format!("{}  Edit Connection…", icon::PENCIL_SIMPLE))
        .clicked()
    {
        actions.push(UiAction::EditConnection(profile.id));
    }
    if ui
        .button(format!("{}  Duplicate Connection", icon::COPY))
        .clicked()
    {
        actions.push(UiAction::DuplicateConnection(profile.id));
    }
    if ui
        .button(
            RichText::new(format!("{}  Delete Connection…", icon::TRASH)).color(color::danger()),
        )
        .clicked()
    {
        actions.push(UiAction::DeleteConnection(profile.id));
    }
}

/// Database/schema context accumulated while walking down the tree.
#[derive(Clone)]
struct PathCtx {
    profile_id: ProfileId,
    kind: rdbs_core::DatabaseKind,
    database: Option<String>,
    schema: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn object_node(
    ui: &mut Ui,
    state: &mut ExplorerState,
    actions: &mut Vec<UiAction>,
    obj: &DbObject,
    parent_key: &str,
    depth: usize,
    path: &PathCtx,
    needle: &str,
    loading: Option<&HashSet<String>>,
) {
    if !obj.matches_filter(needle) {
        return;
    }
    // Other databases of a PostgreSQL / SQL Server instance load when first expanded.
    let lazy = obj.kind == ObjectKind::Database
        && obj.children.is_empty()
        && obj.detail.as_deref() == Some(rdbs_core::schema::NOT_LOADED);
    let is_loading = lazy && loading.is_some_and(|l| l.contains(&obj.name));
    let key = format!("{parent_key}/{}", obj.name);
    let expandable = !obj.children.is_empty() || lazy;
    let open = expandable && (state.expanded.contains(&key) || (!needle.is_empty() && !lazy));
    let (glyph, glyph_color) = object_icon(obj);

    let row = TreeRow {
        depth,
        expandable,
        open,
        selected: state.selected.as_deref() == Some(key.as_str()),
        draggable: false,
    };
    let (response, toggled) = row.show(ui, |ui, rect| {
        let painter = ui.painter();
        painter.text(
            egui::pos2(rect.left() + 8.0, rect.center().y),
            Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(14.0),
            glyph_color,
        );
        let name_x = rect.left() + 20.0;
        let available = (rect.right() - 4.0 - name_x).max(0.0);
        let name_font = FontId::proportional(13.0);
        let detail_font = FontId::proportional(11.0);
        let detail = obj.detail.as_deref().filter(|text| !text.is_empty());
        let name_natural = label_width(ui, &obj.name, name_font.clone());
        let detail_natural = detail
            .map(|text| label_width(ui, text, detail_font.clone()))
            .unwrap_or(0.0);
        let gap = if detail.is_some() { 8.0 } else { 0.0 };
        let name_budget = if detail.is_some() && name_natural + gap + detail_natural > available {
            (available * 0.62).clamp(available.min(24.0), available)
        } else {
            available
        };
        let name = fit_label(ui, &obj.name, name_font, color::text(), name_budget);
        let name_pos = egui::pos2(name_x, rect.center().y - name.size().y / 2.0);
        let painted_name = name.size().x;
        painter.galley(name_pos, name, color::text());
        if let Some(detail) = detail {
            let detail_budget = (available - painted_name - gap).max(0.0);
            if detail_budget >= 12.0 {
                let detail_galley =
                    fit_label(ui, detail, detail_font, color::text_faint(), detail_budget);
                painter.galley(
                    egui::pos2(
                        name_pos.x + painted_name + gap,
                        rect.center().y - detail_galley.size().y / 2.0,
                    ),
                    detail_galley,
                    color::text_faint(),
                );
            }
        }
    });

    let mut here = path.clone();
    match obj.kind {
        ObjectKind::Database => here.database = Some(obj.name.clone()),
        ObjectKind::Schema => here.schema = Some(obj.name.clone()),
        _ => {}
    }
    let reference = ObjectRef {
        profile_id: path.profile_id,
        database: path.database.clone(),
        schema: path.schema.clone(),
        name: obj.name.clone(),
        kind: obj.kind,
    };
    let has_ddl = matches!(
        obj.kind,
        ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::MaterializedView
            | ObjectKind::Function
            | ObjectKind::Procedure
    );
    let ddl_ref = has_ddl.then(|| reference.clone());
    let routine_ref =
        matches!(obj.kind, ObjectKind::Function | ObjectKind::Procedure).then(|| reference.clone());
    let job_ref = (obj.kind == ObjectKind::AgentJob).then(|| reference.clone());
    let object_ref = obj.kind.is_queryable().then_some(reference);

    let hover = match obj.detail.as_deref() {
        Some(detail) if !detail.is_empty() => format!("{}\n{detail}", obj.name),
        _ => obj.name.clone(),
    };
    let response = response.on_hover_text(hover);
    if response.clicked() || response.secondary_clicked() {
        state.selected = Some(key.clone());
        state.selected_profile = Some(path.profile_id);
        state.selected_object = object_ref.clone();
    }
    // Double-click opens tables, views, routines and Agent job history.
    // Columns and folders expand with the caret.
    let opens = object_ref.is_some() || routine_ref.is_some() || job_ref.is_some();
    if toggled || (expandable && !opens && response.double_clicked()) {
        if !state.expanded.remove(&key) {
            state.expanded.insert(key.clone());
            if lazy && !is_loading {
                actions.push(UiAction::LoadDatabase {
                    profile_id: path.profile_id,
                    database: obj.name.clone(),
                });
            }
        }
    } else if response.double_clicked() {
        if let Some(r) = &object_ref {
            actions.push(UiAction::OpenObject(r.clone()));
        } else if let Some(r) = &job_ref {
            actions.push(UiAction::ViewJobHistory(r.clone()));
        } else if let Some(r) = &routine_ref
            && path.kind.is_sql()
        {
            actions.push(UiAction::EditRoutine(r.clone()));
        }
    }

    response.context_menu(|ui| {
        object_menu(
            ui,
            obj,
            object_ref.as_ref(),
            ddl_ref.as_ref(),
            routine_ref.as_ref(),
            job_ref.as_ref(),
            path,
            actions,
        );
    });

    if open {
        if is_loading {
            hint_row(ui, depth + 1, "Loading…", color::text_dim());
        }
        for child in &obj.children {
            object_node(
                ui,
                state,
                actions,
                child,
                &key,
                depth + 1,
                &here,
                needle,
                loading,
            );
        }
    }
}

/// Start, stop, enable and disable. Hidden or disabled when the job's state makes
/// the action meaningless. The application confirms before anything is sent.
fn job_actions(ui: &mut Ui, obj: &DbObject, job: &ObjectRef, actions: &mut Vec<UiAction>) {
    use rdbs_core::agent::{AgentJobAction, job_controls};
    let controls = job_controls(&obj.notes);
    let start = ui.add_enabled(
        controls.can_start,
        egui::Button::new(format!("{}  Start Job", icon::PLAY)),
    );
    let start = if controls.can_start {
        start.on_hover_text("Run the job from its start step")
    } else {
        start.on_disabled_hover_text(controls.start_reason)
    };
    if start.clicked() {
        actions.push(UiAction::RunAgentJob {
            obj: job.clone(),
            action: AgentJobAction::Start { step_name: None },
        });
    }
    if controls.can_start && controls.steps.len() > 1 {
        ui.menu_button(format!("{}  Start Job at Step", icon::PLAY), |ui| {
            for step in &controls.steps {
                if ui.button(format!("{}. {}", step.id, step.name)).clicked() {
                    actions.push(UiAction::RunAgentJob {
                        obj: job.clone(),
                        action: AgentJobAction::Start {
                            step_name: Some(step.name.clone()),
                        },
                    });
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("Run the job beginning at one of its steps");
    }
    if controls.show_stop {
        let stop = ui.add_enabled(
            controls.can_stop,
            egui::Button::new(format!("{}  Stop Job", icon::STOP)),
        );
        let stop = if controls.can_stop {
            stop.on_hover_text("Interrupt the running job. You will be asked to confirm.")
        } else {
            stop.on_disabled_hover_text(controls.stop_reason)
        };
        if stop.clicked() {
            actions.push(UiAction::RunAgentJob {
                obj: job.clone(),
                action: AgentJobAction::Stop,
            });
        }
    }
    if let Some(enable) = controls.set_enabled {
        let (label, hover) = if enable {
            (
                format!("{}  Enable Job", icon::CHECK),
                "Allow the job to be started and to run on its schedule",
            )
        } else {
            (
                format!("{}  Disable Job", icon::PROHIBIT),
                "Keep the job from being started. A job that is already running is left running.",
            )
        };
        if ui.button(label).on_hover_text(hover).clicked() {
            actions.push(UiAction::RunAgentJob {
                obj: job.clone(),
                action: AgentJobAction::SetEnabled(enable),
            });
        }
    }
    ui.separator();
}

#[allow(clippy::too_many_arguments)]
fn object_menu(
    ui: &mut Ui,
    obj: &DbObject,
    object_ref: Option<&ObjectRef>,
    ddl_ref: Option<&ObjectRef>,
    routine_ref: Option<&ObjectRef>,
    job_ref: Option<&ObjectRef>,
    path: &PathCtx,
    actions: &mut Vec<UiAction>,
) {
    if obj.kind == ObjectKind::Notice {
        ui.label(obj.name.clone());
        if ui.button(format!("{}  Copy", icon::COPY)).clicked() {
            ui.ctx().copy_text(obj.name.clone());
        }
        return;
    }
    // Start, stop and enable/disable go through the application, which confirms them.
    // Steps and schedules are not edited here.
    if let Some(r) = job_ref {
        job_actions(ui, obj, r, actions);
        if ui
            .button(format!("{}  View History", icon::CLOCK))
            .on_hover_text(
                "Each step of each run, including the message (double-click does the same)",
            )
            .clicked()
        {
            actions.push(UiAction::ViewJobHistory(r.clone()));
        }
        if ui
            .button(format!("{}  Script Job as CREATE", icon::CODE))
            .on_hover_text(
                "Open a T-SQL script for the job, its steps and its schedules. Nothing is run.",
            )
            .clicked()
        {
            actions.push(UiAction::ShowDdl(r.clone()));
        }
        ui.separator();
    }
    if let Some(r) = routine_ref
        && path.kind.is_sql()
    {
        let what = if obj.kind == ObjectKind::Function {
            "Function"
        } else {
            "Procedure"
        };
        if ui
            .button(format!("{}  Edit {what}", icon::PENCIL_SIMPLE))
            .on_hover_text("Open the definition as a script; running it saves your changes")
            .clicked()
        {
            actions.push(UiAction::EditRoutine(r.clone()));
        }
        if ui
            .button(format!("{}  Execute {what}…", icon::PLAY))
            .on_hover_text("Enter the parameters and run it")
            .clicked()
        {
            actions.push(UiAction::ExecuteRoutine(r.clone()));
        }
    }
    if let Some(r) = ddl_ref
        && ui.button(format!("{}  Show DDL", icon::CODE)).clicked()
    {
        actions.push(UiAction::ShowDdl(r.clone()));
    }
    if let Some(r) = object_ref {
        if ui
            .button(format!("{}  Open {}", icon::TABLE, kind_label(obj.kind)))
            .on_hover_text("Rows in a data tab (double-click does the same)")
            .clicked()
        {
            actions.push(UiAction::OpenObject(r.clone()));
        }
        let sql = path.kind.is_sql();
        if sql
            && ui
                .button(format!("{}  Select Top 100 Rows", icon::LIST_NUMBERS))
                .on_hover_text("A query tab with the SELECT, already run")
                .clicked()
        {
            actions.push(UiAction::SelectTop {
                obj: r.clone(),
                rows: 100,
            });
        }
        if sql
            && obj.kind == ObjectKind::Table
            && ui
                .button(format!("{}  Design Table", icon::WRENCH))
                .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Designer {
                scope: tools::scope(
                    path.kind,
                    path.profile_id,
                    r.database.clone(),
                    r.schema.clone(),
                ),
                table: Some(r.name.clone()),
            }));
        }
        if sql
            && obj.kind == ObjectKind::Table
            && ui
                .button(format!("{}  Import Data…", icon::DOWNLOAD_SIMPLE))
                .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Import {
                scope: tools::scope(
                    path.kind,
                    path.profile_id,
                    r.database.clone(),
                    r.schema.clone(),
                ),
                table: Some(r.name.clone()),
            }));
        }
        ui.separator();
    }
    // Containers of tables: the Tables folder, a schema, or a database without schemas.
    // Table tools need SQL; MongoDB and Redis have none of them.
    let container = if !path.kind.is_sql() {
        None
    } else {
        match obj.kind {
            ObjectKind::Folder if obj.name == "Tables" => {
                Some((path.database.clone(), path.schema.clone()))
            }
            ObjectKind::Schema => Some((path.database.clone(), Some(obj.name.clone()))),
            ObjectKind::Database if !obj.children.is_empty() && !path.kind.has_schemas() => {
                Some((Some(obj.name.clone()), None))
            }
            _ => None,
        }
    };
    if let Some((database, schema)) = container {
        let scope = tools::scope(path.kind, path.profile_id, database, schema);
        if ui
            .button(format!("{}  New Table…", icon::PLUS_SQUARE))
            .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Designer {
                scope: scope.clone(),
                table: None,
            }));
        }
        if ui
            .button(format!("{}  Import Data…", icon::DOWNLOAD_SIMPLE))
            .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Import {
                scope: scope.clone(),
                table: None,
            }));
        }
        ui.separator();
        if ui
            .button(format!("{}  Query Builder", icon::PUZZLE_PIECE))
            .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Builder {
                scope: scope.clone(),
            }));
        }
        if ui.button(format!("{}  ER Diagram", icon::GRAPH)).clicked() {
            actions.push(UiAction::OpenTool(ToolLaunch::Er {
                scope: scope.clone(),
            }));
        }
        if ui
            .button(format!("{}  Compare Schema…", icon::GIT_DIFF))
            .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Diff {
                source: Some(scope.clone()),
            }));
        }
        if ui
            .button(format!("{}  Data Transfer…", icon::FLOW_ARROW))
            .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Transfer {
                source: Some(scope.clone()),
            }));
        }
        if ui
            .button(format!("{}  Table Sync…", icon::ARROWS_LEFT_RIGHT))
            .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Sync {
                source: Some(scope.clone()),
            }));
        }
        if ui.button(format!("{}  Backup…", icon::ARCHIVE)).clicked() {
            actions.push(UiAction::OpenTool(ToolLaunch::Backup {
                scope: Some(scope.clone()),
            }));
        }
        ui.separator();
    }
    if ui
        .button(format!("{}  New Query", icon::FILE_SQL))
        .clicked()
    {
        actions.push(UiAction::NewQuery {
            profile_id: Some(path.profile_id),
            sql: None,
            title: None,
            run: false,
        });
    }
    if ui.button(format!("{}  Copy Name", icon::COPY)).clicked() {
        ui.ctx().copy_text(obj.name.clone());
    }
}

fn kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::View | ObjectKind::MaterializedView => "View",
        _ => "Table",
    }
}

fn object_icon(obj: &DbObject) -> (&'static str, Color32) {
    let (glyph, c) = object_icon_dark(obj);
    (glyph, crate::theme::icon_tint(c))
}

/// Icon and its colour on the dark theme.
fn object_icon_dark(obj: &DbObject) -> (&'static str, Color32) {
    match obj.kind {
        ObjectKind::Database => (icon::DATABASE, Color32::from_rgb(0xE5, 0xB5, 0x4A)),
        ObjectKind::Schema => (icon::SQUARES_FOUR, Color32::from_rgb(0xB3, 0x8B, 0xF2)),
        ObjectKind::Folder if obj.name == rdbs_core::agent::AGENT_FOLDER => {
            (icon::GEAR, Color32::from_rgb(0x7F, 0xA2, 0xC4))
        }
        ObjectKind::Folder => (icon::FOLDER_SIMPLE, Color32::from_rgb(0x9A, 0xA3, 0xB2)),
        ObjectKind::AgentJob
            if obj
                .detail
                .as_deref()
                .is_some_and(|d| d.starts_with("running")) =>
        {
            (icon::PLAY, Color32::from_rgb(0x3D, 0xDC, 0x97))
        }
        ObjectKind::AgentJob => (icon::CLOCK, Color32::from_rgb(0xE0, 0x8A, 0x1E)),
        ObjectKind::Notice => (icon::INFO, color::text_dim()),
        ObjectKind::Table => (icon::TABLE, Color32::from_rgb(0x5E, 0xA2, 0xFF)),
        ObjectKind::View => (icon::EYE, Color32::from_rgb(0x4F, 0xC1, 0xB0)),
        ObjectKind::MaterializedView => (icon::STACK, Color32::from_rgb(0x4F, 0xC1, 0xB0)),
        ObjectKind::Function => (icon::FUNCTION, Color32::from_rgb(0xF2, 0x8B, 0xC0)),
        ObjectKind::Procedure => (icon::LIGHTNING, Color32::from_rgb(0xF2, 0xA1, 0x5A)),
        ObjectKind::Index => (icon::LIST_BULLETS, color::text_dim()),
        ObjectKind::Column => {
            let is_pk = obj.detail.as_deref().is_some_and(|d| d.contains("PK"));
            if is_pk {
                (icon::KEY, Color32::from_rgb(0xE5, 0xB5, 0x4A))
            } else {
                (icon::COLUMNS, color::text_faint())
            }
        }
    }
}

fn hint_row(ui: &mut Ui, depth: usize, text: &str, c: Color32) {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::hover());
    let x = rect.left() + 20.0 + depth as f32 * INDENT;
    let galley = fit_label(
        ui,
        text,
        FontId::proportional(12.0),
        c,
        (rect.right() - 6.0 - x).max(0.0),
    );
    ui.painter_at(rect).galley(
        egui::pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        c,
    );
    response.on_hover_text(text);
}

/// Width of `text` on one line.
fn label_width(ui: &Ui, text: &str, font: FontId) -> f32 {
    ui.ctx().fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(text.to_owned(), font, color::text())
            .size()
            .x
    })
}

/// Lays `text` out on one line, ending with an ellipsis when it is wider than `max_width`.
fn fit_label(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(0.0));
    ui.ctx().fonts_mut(|fonts| fonts.layout_job(job))
}

/// A full-width, selectable tree row with an optional disclosure caret.
struct TreeRow {
    depth: usize,
    expandable: bool,
    open: bool,
    selected: bool,
    /// The row can be dragged (connections, to reorder them).
    draggable: bool,
}

impl TreeRow {
    /// Paints the row and calls `content` with the area right of the caret.
    /// Returns the row response and whether the caret was clicked.
    fn show(self, ui: &mut Ui, content: impl FnOnce(&Ui, Rect)) -> (Response, bool) {
        let sense = if self.draggable {
            Sense::click_and_drag()
        } else {
            Sense::click()
        };
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), sense);
        let painter = ui.painter();
        if self.selected {
            painter.rect_filled(rect, CornerRadius::same(3), color::selection());
        } else if response.hovered() {
            painter.rect_filled(
                rect,
                CornerRadius::same(3),
                color::surface().gamma_multiply(0.7),
            );
        }

        let caret_x = rect.left() + 10.0 + self.depth as f32 * INDENT;
        let caret_rect = Rect::from_center_size(
            egui::pos2(caret_x, rect.center().y),
            Vec2::new(16.0, ROW_HEIGHT),
        );
        if self.expandable {
            painter.text(
                caret_rect.center(),
                Align2::CENTER_CENTER,
                if self.open {
                    icon::CARET_DOWN
                } else {
                    icon::CARET_RIGHT
                },
                FontId::proportional(11.0),
                color::text_dim(),
            );
        }
        // `interact_pointer_pos` can be empty on the release frame (press and release in
        // different frames), so fall back to the last known pointer position.
        let pointer = response
            .interact_pointer_pos()
            .or_else(|| response.hover_pos())
            .or_else(|| ui.input(|i| i.pointer.latest_pos()));
        let toggled = self.expandable
            && response.clicked()
            && pointer.is_some_and(|p| caret_rect.expand2(Vec2::new(3.0, 0.0)).contains(p));

        let content_rect = Rect::from_min_max(
            egui::pos2(caret_rect.right() + 2.0, rect.top()),
            rect.right_bottom(),
        );
        content(ui, content_rect);
        (response, toggled)
    }
}
