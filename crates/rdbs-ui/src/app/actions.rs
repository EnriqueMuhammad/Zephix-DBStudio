//! Applies UI actions: connect, open tabs, routines, DDL, edits, exports and profiles.

use super::*;

impl StudioApp {
    pub(super) fn apply(&mut self, actions: Vec<UiAction>) {
        for action in actions {
            tracing::debug!(?action, "ui action");
            match action {
                UiAction::NewQuery {
                    profile_id,
                    database,
                    sql,
                    title,
                    run,
                } => {
                    let (profile_id, database) = match profile_id {
                        Some(id) => (Some(id), database),
                        None => (
                            self.explorer.selected_profile,
                            self.explorer.selected_database.clone(),
                        ),
                    };
                    let database = profile_id.and_then(|id| self.tab_database(id, database));
                    let id =
                        self.workspace
                            .open_query_tab(profile_id, sql.unwrap_or_default(), title);
                    if let Some(q) = self.workspace.query_tab_mut(id) {
                        q.database = database;
                    }
                    if run {
                        let follow_up = self.run_tab(id);
                        self.apply(follow_up);
                    }
                }
                UiAction::OpenObject(obj) => {
                    let Some(kind) = self.profile(obj.profile_id).map(|p| p.kind) else {
                        continue;
                    };
                    if kind.is_sql() {
                        self.open_data_tab(&obj, kind);
                        continue;
                    }
                    let qualified = dialect::qualified_name(
                        kind,
                        obj.database.as_deref(),
                        obj.schema.as_deref(),
                        &obj.name,
                    );
                    let sql = dialect::select_preview(kind, &qualified, 1000);
                    // Objects of a non-default database run in a session for that database.
                    let database = obj.database.clone().filter(|db| {
                        kind.has_schemas()
                            && self
                                .connections
                                .get(&obj.profile_id)
                                .and_then(|c| c.default_database.as_ref())
                                != Some(db)
                    });
                    let table = self.table_target(&obj, kind);
                    let id = self.workspace.open_query_tab(
                        Some(obj.profile_id),
                        sql,
                        Some(obj.name.clone()),
                    );
                    if let Some(q) = self.workspace.query_tab_mut(id) {
                        q.database = database;
                        q.table = table;
                    }
                    let follow_up = self.run_tab(id);
                    self.apply(follow_up);
                }
                UiAction::ShowDdl(obj) => self.request_ddl(obj, false),
                UiAction::RunAgentJob { obj, action } => self.request_agent_job(obj, action),
                UiAction::ViewJobHistory(obj) => {
                    self.apply(vec![UiAction::NewQuery {
                        profile_id: Some(obj.profile_id),
                        database: None,
                        sql: Some(rdbs_core::agent::history_sql(&obj.name)),
                        title: Some(format!("History · {}", obj.name)),
                        run: true,
                    }]);
                }
                UiAction::EditRoutine(obj) => self.request_ddl(obj, true),
                UiAction::MoveConnection { id, target, below } => {
                    self.move_connection(id, target, below);
                }
                UiAction::ExecuteRoutine(obj) => self.request_routine(obj),
                UiAction::SelectTop { obj, rows } => self.open_select(&obj, Some(rows)),
                UiAction::OpenTool(launch) => self.workspace.open_tool(launch),
                UiAction::SaveSchedule(schedule) => match self.store.save_schedule(&schedule) {
                    Ok(()) => {
                        match self
                            .backups
                            .schedules
                            .iter_mut()
                            .find(|s| s.id == schedule.id)
                        {
                            Some(existing) => *existing = schedule,
                            None => self.backups.schedules.push(schedule),
                        }
                        self.status = Some(StatusMessage::info("Backup schedule saved"));
                    }
                    Err(e) => {
                        self.status = Some(StatusMessage::error(format!(
                            "Could not save the schedule: {e}"
                        )));
                    }
                },
                UiAction::DeleteSchedule(id) => {
                    if let Err(e) = self.store.delete_schedule(&id) {
                        self.status = Some(StatusMessage::error(format!(
                            "Could not delete the schedule: {e}"
                        )));
                    } else {
                        self.backups.schedules.retain(|s| s.id != id);
                    }
                }
                UiAction::RunSchedule(id) => self.run_schedule(&id),
                UiAction::RunBackup {
                    profile_id,
                    database,
                    options,
                } => {
                    if let Err(e) = self.start_backup(String::new(), profile_id, database, options)
                    {
                        self.backups.adhoc_result = Some(Err(e));
                    }
                }
                UiAction::Connect(id) => self.connect(id),
                UiAction::Disconnect(id) => {
                    self.services.send(Command::Disconnect { profile_id: id })
                }
                UiAction::Reconnect {
                    profile_id,
                    database,
                    retry_tab,
                } => self.begin_reconnect(profile_id, database, retry_tab),
                UiAction::Refresh(id) => {
                    if self
                        .connections
                        .get(&id)
                        .is_some_and(ConnState::is_connected)
                    {
                        self.services
                            .send(Command::RefreshObjects { profile_id: id });
                        self.status = Some(StatusMessage::info(REFRESHING));
                    } else {
                        self.connect(id);
                    }
                }
                UiAction::NewConnection => self.dialog = Some(ConnectionDialog::new_profile()),
                UiAction::EditConnection(id) => {
                    if let Some(p) = self.profile(id) {
                        self.dialog = Some(ConnectionDialog::edit(p));
                    }
                }
                UiAction::DuplicateConnection(id) => {
                    if let Some(p) = self.profile(id) {
                        let mut copy = p.clone();
                        copy.id = ProfileId::new();
                        copy.name = format!("{} (copy)", p.name);
                        let password = if p.save_password {
                            self.credentials
                                .get(id, SecretKind::Database)
                                .ok()
                                .flatten()
                        } else {
                            None
                        };
                        self.save_profile(copy, password);
                    }
                }
                UiAction::DeleteConnection(id) => {
                    self.confirm = Some(Confirmation::DeleteProfile(id));
                }
                UiAction::ConfirmExecute {
                    tab_id,
                    profile_id,
                    sql,
                    kind,
                } => {
                    self.confirm = Some(Confirmation::Execute {
                        tab_id,
                        profile_id,
                        sql,
                        kind,
                    });
                }
                UiAction::LoadDatabase {
                    profile_id,
                    database,
                } => {
                    let Some(profile) = self.profile(profile_id).cloned() else {
                        continue;
                    };
                    if let Some(conn) = self.connections.get_mut(&profile_id) {
                        conn.loading_databases.insert(database.clone());
                        conn.database_errors.remove(&database);
                    }
                    self.services.send(Command::LoadDatabase {
                        profile,
                        database,
                        password: self.session_passwords.get(&profile_id).cloned(),
                    });
                }
                UiAction::NeedPassword { profile_id, then } => {
                    self.open_prompt(profile_id, then, None);
                }
                UiAction::OpenScript {
                    profile_id,
                    database,
                    sql,
                    run,
                } => {
                    let id = self.workspace.open_query_tab(Some(profile_id), sql, None);
                    if let Some(q) = self.workspace.query_tab_mut(id) {
                        q.database = database;
                    }
                    if run {
                        let follow_up = self.run_tab(id);
                        self.apply(follow_up);
                    }
                }
                UiAction::SaveEdits { tab_id } => self.save_edits(tab_id, false),
                UiAction::ExportActive => self.export_active(),
                UiAction::ShowHistory => {
                    self.reload_history();
                    self.workspace.show_history();
                }
                UiAction::ReloadHistory(filter) => {
                    self.history_filter = filter;
                    self.reload_history();
                }
                UiAction::ClearHistory => self.confirm = Some(Confirmation::ClearHistory),
                UiAction::SaveExport {
                    contents,
                    file_name,
                    format,
                } => self.save_export(contents, file_name, format),
                UiAction::SaveExcel {
                    result,
                    file_name,
                    sheet,
                } => self.save_excel(result, file_name, sheet),
                UiAction::Status(message) => self.status = Some(message),
                UiAction::ToggleSidebar => {
                    self.workspace.sidebar_visible = !self.workspace.sidebar_visible;
                }
                UiAction::CloseTabs(which) => self.request_close(which),
                UiAction::OpenQueryFile => {
                    let ctx = self.egui_ctx.clone();
                    if let Err(e) = self.scripts.pick_open(&ctx) {
                        self.status = Some(StatusMessage::error(e));
                    }
                }
                UiAction::SaveQuery { tab_id, save_as } => {
                    let tab_id = tab_id.or_else(|| self.workspace.active_query_tab().map(|q| q.id));
                    if let Some(id) = tab_id {
                        self.save_query(id, save_as, false);
                    }
                }
            }
        }
    }

    /// Session key for a tab database override. `None` is the profile's own database.
    pub(super) fn normalized_session(
        &self,
        profile_id: ProfileId,
        database: &Option<String>,
    ) -> Option<String> {
        match self.profile(profile_id) {
            Some(profile) => profile.session_database(database.clone()),
            None => database.clone().filter(|db| !db.is_empty()),
        }
    }

    /// Clears a matching reconnect and returns the tab to run again, if one was requested.
    pub(super) fn take_reconnect_retry(
        &mut self,
        profile_id: ProfileId,
        database: &Option<String>,
    ) -> Option<u64> {
        let intent = self.reconnect.clone()?;
        if intent.profile_id != profile_id
            || self.normalized_session(profile_id, &intent.database) != *database
        {
            return None;
        }
        self.reconnect = None;
        intent.retry_tab
    }

    pub(super) fn begin_reconnect(
        &mut self,
        profile_id: ProfileId,
        database: Option<String>,
        retry_tab: Option<u64>,
    ) {
        let Some(profile) = self.profile(profile_id).cloned() else {
            return;
        };
        self.reconnect = Some(ReconnectIntent {
            profile_id,
            database,
            retry_tab,
        });
        if editor::needs_password(&profile, &self.connections, &self.session_passwords) {
            self.open_prompt(profile_id, AfterPassword::Reconnect, None);
            return;
        }
        self.send_reconnect(&profile);
    }

    pub(super) fn send_reconnect(&mut self, profile: &ConnectionProfile) {
        let Some(intent) = self.reconnect.clone() else {
            return;
        };
        self.services.send(Command::Reconnect {
            profile: profile.clone(),
            password: self.session_passwords.get(&profile.id).cloned(),
            database: intent.database,
        });
        self.status = Some(StatusMessage::info(format!(
            "Reconnecting to {}…",
            profile.name
        )));
    }

    pub(super) fn connect(&mut self, id: ProfileId) {
        let Some(profile) = self.profile(id).cloned() else {
            return;
        };
        if editor::needs_password(&profile, &self.connections, &self.session_passwords) {
            self.open_prompt(id, AfterPassword::Connect, None);
            return;
        }
        self.connections.entry(id).or_default().status = ConnStatus::Connecting;
        self.explorer.expand_profile(id);
        self.services.send(Command::Connect {
            profile,
            password: self.session_passwords.get(&id).cloned(),
        });
    }

    /// Edit target for a table opened from the explorer, with its columns and primary key read
    /// from the object tree (columns marked "PK"). Views are not editable.
    pub(super) fn table_target(
        &self,
        obj: &crate::model::ObjectRef,
        kind: rdbs_core::DatabaseKind,
    ) -> Option<rdbs_core::edit::TableTarget> {
        if obj.kind != rdbs_core::ObjectKind::Table {
            return None;
        }
        let objects = &self.connections.get(&obj.profile_id)?.objects;
        let node = find_table(
            objects,
            obj.database.as_deref(),
            obj.schema.as_deref(),
            &obj.name,
        )?;
        let columns: Vec<&rdbs_core::DbObject> = node
            .children
            .iter()
            .filter(|c| c.kind == rdbs_core::ObjectKind::Column)
            .collect();
        let primary_key = columns
            .iter()
            .filter(|c| c.detail.as_deref().is_some_and(|d| d.contains("· PK")))
            .map(|c| c.name.clone())
            .collect();
        Some(rdbs_core::edit::TableTarget {
            kind,
            database: obj.database.clone(),
            schema: obj.schema.clone(),
            table: obj.name.clone(),
            primary_key,
            columns: columns.iter().map(|c| c.name.clone()).collect(),
        })
    }

    /// Database override for an object of a non-default database (PostgreSQL / SQL Server).
    pub(super) fn database_override(
        &self,
        obj: &crate::model::ObjectRef,
        kind: rdbs_core::DatabaseKind,
    ) -> Option<String> {
        obj.database.clone().filter(|db| {
            kind.has_schemas()
                && self
                    .connections
                    .get(&obj.profile_id)
                    .and_then(|c| c.default_database.as_ref())
                    != Some(db)
        })
    }

    /// Database override of a new query tab for `database` picked in the explorer. `None`
    /// for the connection's own database, and for engines whose explorer databases are not
    /// separate sessions (SQLite's `main`, Oracle's service, Redis' numbered databases).
    pub(super) fn tab_database(
        &self,
        profile_id: ProfileId,
        database: Option<String>,
    ) -> Option<String> {
        use rdbs_core::DatabaseKind as K;
        let profile = self.profile(profile_id)?;
        if !matches!(
            profile.kind,
            K::SqlServer | K::PostgreSql | K::MySql | K::MariaDb | K::MongoDb
        ) {
            return None;
        }
        let default = self
            .connections
            .get(&profile_id)
            .and_then(|c| c.default_database.as_ref());
        profile
            .session_database(database)
            .filter(|db| Some(db) != default)
    }

    /// Opens the rows of a table or view in a data tab (or focuses the one already open).
    pub(super) fn open_data_tab(
        &mut self,
        obj: &crate::model::ObjectRef,
        kind: rdbs_core::DatabaseKind,
    ) {
        let qualified = dialect::qualified_name(
            kind,
            obj.database.as_deref(),
            obj.schema.as_deref(),
            &obj.name,
        );
        let existing = self.workspace.dock.find_tab_from(|t| {
            matches!(t, crate::workspace::Tab::Query(q)
                if q.profile_id == Some(obj.profile_id)
                    && q.data_view.as_ref().is_some_and(|v| v.qualified == qualified))
        });
        if let Some(path) = existing {
            let _ = self.workspace.dock.set_active_tab(path);
            return;
        }
        let database = self.database_override(obj, kind);
        let table = self.table_target(obj, kind);
        let view = crate::workspace::DataView {
            kind,
            qualified,
            filter: String::new(),
            order: String::new(),
            limit: Some(1000),
        };
        let id =
            self.workspace
                .open_query_tab(Some(obj.profile_id), view.sql(), Some(obj.name.clone()));
        if let Some(q) = self.workspace.query_tab_mut(id) {
            q.database = database;
            q.table = table;
            q.data_view = Some(view);
        }
        let follow_up = self.run_tab(id);
        self.apply(follow_up);
    }

    /// Opens a query tab selecting the first `rows` rows of a table or view and runs it.
    pub(super) fn open_select(&mut self, obj: &crate::model::ObjectRef, rows: Option<u32>) {
        let Some(kind) = self.profile(obj.profile_id).map(|p| p.kind) else {
            return;
        };
        let qualified = dialect::qualified_name(
            kind,
            obj.database.as_deref(),
            obj.schema.as_deref(),
            &obj.name,
        );
        let sql = dialect::select_preview(kind, &qualified, rows.unwrap_or(1000));
        let database = self.database_override(obj, kind);
        let table = self.table_target(obj, kind);
        let id = self
            .workspace
            .open_query_tab(Some(obj.profile_id), sql, Some(obj.name.clone()));
        if let Some(q) = self.workspace.query_tab_mut(id) {
            q.database = database;
            q.table = table;
        }
        let follow_up = self.run_tab(id);
        self.apply(follow_up);
    }

    /// Moves a connection above or below another one and stores the new order.
    pub(super) fn move_connection(&mut self, id: ProfileId, target: ProfileId, below: bool) {
        let Some(from) = self.profiles.iter().position(|p| p.id == id) else {
            return;
        };
        let moved = self.profiles.remove(from);
        let Some(to) = self.profiles.iter().position(|p| p.id == target) else {
            self.profiles.insert(from, moved);
            return;
        };
        self.profiles.insert(if below { to + 1 } else { to }, moved);
        let ids: Vec<ProfileId> = self.profiles.iter().map(|p| p.id).collect();
        if let Err(e) = self.store.reorder(&ids) {
            self.status = Some(StatusMessage::error(format!(
                "Could not save the connection order: {e}"
            )));
        }
    }

    /// Everything needed to edit or execute a routine.
    pub(super) fn routine_target(
        &self,
        obj: &crate::model::ObjectRef,
        kind: rdbs_core::DatabaseKind,
    ) -> RoutineTarget {
        RoutineTarget {
            profile_id: obj.profile_id,
            database: self.database_override(obj, kind),
            kind,
            qualified: dialect::qualified_name(
                kind,
                obj.database.as_deref(),
                obj.schema.as_deref(),
                &obj.name,
            ),
            name: obj.name.clone(),
            routine: obj.kind,
        }
    }

    /// Fetches the parameters of a routine for the Execute dialog.
    pub(super) fn request_routine(&mut self, obj: crate::model::ObjectRef) {
        let Some(profile) = self.profile(obj.profile_id).cloned() else {
            return;
        };
        let target = self.routine_target(&obj, profile.kind);
        let request_id = self.services.next_request_id();
        self.services.send(Command::GetRoutine {
            request_id,
            profile,
            database: target.database.clone(),
            object: rdbs_core::schema::ObjectPath {
                database: obj.database,
                schema: obj.schema,
                name: obj.name.clone(),
                kind: obj.kind,
            },
            password: self.session_passwords.get(&obj.profile_id).cloned(),
        });
        self.status = Some(StatusMessage::info(format!(
            "Loading the parameters of {}…",
            obj.name
        )));
        self.routine_requests.insert(request_id, target);
    }

    /// Opens a query tab with `sql` for a routine and runs it.
    pub(super) fn run_routine(&mut self, target: &RoutineTarget, sql: String) {
        let id = self.workspace.open_query_tab(
            Some(target.profile_id),
            sql,
            Some(format!("Exec {}", target.name)),
        );
        if let Some(q) = self.workspace.query_tab_mut(id) {
            q.database = target.database.clone();
        }
        let follow_up = self.run_tab(id);
        self.apply(follow_up);
    }

    /// Confirms when the action requires it, then asks the worker to run the procedure.
    pub(super) fn request_agent_job(
        &mut self,
        obj: crate::model::ObjectRef,
        action: rdbs_core::agent::AgentJobAction,
    ) {
        let Some(profile) = self.profile(obj.profile_id) else {
            return;
        };
        if rdbs_core::agent::action_requires_confirmation(&action, profile.environment) {
            self.confirm = Some(Confirmation::AgentJob {
                profile_id: obj.profile_id,
                job_name: obj.name,
                action,
            });
            return;
        }
        self.send_agent_job(obj.profile_id, obj.name, action);
    }

    pub(super) fn send_agent_job(
        &mut self,
        profile_id: ProfileId,
        job_name: String,
        action: rdbs_core::agent::AgentJobAction,
    ) {
        let Some(profile) = self.profile(profile_id).cloned() else {
            return;
        };
        let progress = match &action {
            rdbs_core::agent::AgentJobAction::Start {
                step_name: Some(step),
            } if !step.is_empty() => {
                format!("Starting “{job_name}” at step “{step}”…")
            }
            rdbs_core::agent::AgentJobAction::Start { .. } => format!("Starting “{job_name}”…"),
            rdbs_core::agent::AgentJobAction::Stop => format!("Stopping “{job_name}”…"),
            rdbs_core::agent::AgentJobAction::SetEnabled(true) => {
                format!("Enabling “{job_name}”…")
            }
            rdbs_core::agent::AgentJobAction::SetEnabled(false) => {
                format!("Disabling “{job_name}”…")
            }
        };
        self.status = Some(StatusMessage::info(progress));
        self.services.send(Command::RunAgentJob {
            request_id: self.services.next_request_id(),
            profile,
            password: self.session_passwords.get(&profile_id).cloned(),
            job_name,
            action,
        });
    }

    pub(super) fn request_ddl(&mut self, obj: crate::model::ObjectRef, edit: bool) {
        let Some(profile) = self.profile(obj.profile_id).cloned() else {
            return;
        };
        let database = self.database_override(&obj, profile.kind);
        let request_id = self.services.next_request_id();
        let edit = edit.then(|| self.routine_target(&obj, profile.kind));
        let title = if obj.kind == rdbs_core::ObjectKind::AgentJob {
            format!("Script · {}", obj.name)
        } else if edit.is_some() {
            format!("Edit {}", obj.name)
        } else {
            format!("DDL · {}", obj.name)
        };
        self.ddl_requests.insert(
            request_id,
            DdlRequest {
                profile_id: obj.profile_id,
                database: database.clone(),
                title,
                edit,
            },
        );
        self.status = Some(StatusMessage::info(format!(
            "Loading the definition of {}…",
            obj.name
        )));
        self.services.send(Command::GetDdl {
            request_id,
            profile,
            database,
            object: rdbs_core::schema::ObjectPath {
                database: obj.database,
                schema: obj.schema,
                name: obj.name,
                kind: obj.kind,
            },
            password: self.session_passwords.get(&obj.profile_id).cloned(),
        });
    }

    /// Writes the pending edits of a tab in one transaction (confirming on production).
    pub(super) fn save_edits(&mut self, tab_id: u64, confirmed: bool) {
        let Self {
            workspace,
            profiles,
            services,
            session_passwords,
            confirm,
            ..
        } = self;
        let Some(q) = workspace.query_tab_mut(tab_id) else {
            return;
        };
        let (Some(table), Some(profile)) = (
            q.table.clone(),
            q.profile_id
                .and_then(|id| profiles.iter().find(|p| p.id == id)),
        ) else {
            return;
        };
        let Some(rs) = q.run.active_result_set() else {
            return;
        };
        let changes = match rdbs_core::edit::build_changes(&table, rs, &q.run.edits) {
            Ok(changes) => changes,
            Err(e) => {
                q.run.edit_error = Some(e.to_string());
                return;
            }
        };
        if changes.is_empty() {
            return;
        }
        if profile.environment.requires_write_confirmation() && !confirmed {
            *confirm = Some(Confirmation::SaveEdits {
                tab_id,
                rows: q.run.edits.row_count(),
            });
            return;
        }
        let request_id = services.next_request_id();
        q.run.saving = Some(request_id);
        services.send(Command::ApplyChanges {
            request_id,
            profile: profile.clone(),
            database: q.database.clone(),
            changes,
            password: session_passwords.get(&profile.id).cloned(),
        });
    }

    /// Saves the result shown in the active query tab as CSV.
    pub(super) fn export_active(&mut self) {
        use rdbs_core::export::{ExportFormat, export};
        let Self {
            workspace,
            profiles,
            ..
        } = self;
        let Some(q) = workspace.active_query_tab() else {
            return;
        };
        let kind = q
            .profile_id
            .and_then(|id| profiles.iter().find(|p| p.id == id))
            .map_or(rdbs_core::DatabaseKind::PostgreSql, |p| p.kind);
        let table = editor::export_table_name(&q.title);
        let Some(rs) = q.run.active_result_set() else {
            return;
        };
        let contents = export(rs, ExportFormat::Csv, kind, &table);
        self.save_export(contents, format!("{table}.csv"), ExportFormat::Csv);
    }

    /// Asks for a destination on a helper thread (the native dialog is modal and blocking)
    /// and writes the file there; the outcome comes back through `export_rx`.
    pub(super) fn save_export(
        &mut self,
        contents: String,
        file_name: String,
        format: rdbs_core::export::ExportFormat,
    ) {
        let tx = self.export_tx.clone();
        let ctx = self.egui_ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("rdbs-export".into())
            .spawn(move || {
                let Some(path) = rfd::FileDialog::new()
                    .set_file_name(&file_name)
                    .add_filter(format.label(), &[format.extension()])
                    .save_file()
                else {
                    return;
                };
                let message = match std::fs::write(&path, contents.as_bytes()) {
                    Ok(()) => StatusMessage::info(format!(
                        "Exported {} to {}",
                        format.label(),
                        path.display()
                    )),
                    Err(e) => {
                        StatusMessage::error(format!("Could not write {}: {e}", path.display()))
                    }
                };
                let _ = tx.send(message);
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.status = Some(StatusMessage::error(format!("Could not start export: {e}")));
        }
    }

    /// Asks for a file name, then builds the workbook and writes it, all on a helper thread:
    /// a large result takes a moment to encode.
    pub(super) fn save_excel(
        &mut self,
        result: rdbs_core::ResultSet,
        file_name: String,
        sheet: String,
    ) {
        let tx = self.export_tx.clone();
        let ctx = self.egui_ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("rdbs-export".into())
            .spawn(move || {
                let Some(path) = rfd::FileDialog::new()
                    .set_file_name(&file_name)
                    .add_filter("Excel workbook", &["xlsx"])
                    .save_file()
                else {
                    return;
                };
                let message = match rdbs_transfer::xlsx::result_to_xlsx(&result, &sheet) {
                    Ok(bytes) => match std::fs::write(&path, bytes) {
                        Ok(()) => StatusMessage::info(format!(
                            "Exported {} rows to {}",
                            crate::widgets::format_count(result.rows.len() as u64),
                            path.display()
                        )),
                        Err(e) => {
                            StatusMessage::error(format!("Could not write {}: {e}", path.display()))
                        }
                    },
                    Err(e) => StatusMessage::error(e),
                };
                let _ = tx.send(message);
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.status = Some(StatusMessage::error(format!("Could not start export: {e}")));
        }
    }

    pub(super) fn open_prompt(
        &mut self,
        profile_id: ProfileId,
        then: AfterPassword,
        reason: Option<String>,
    ) {
        self.prompt = Some(PasswordPrompt {
            profile_id,
            password: String::new(),
            remember: false,
            then,
            reason,
            focus: true,
        });
    }

    /// Saves a profile and, when given, its password: into the keychain if the profile saves
    /// passwords, otherwise for this session only.
    pub(super) fn save_profile(
        &mut self,
        mut profile: ConnectionProfile,
        password: Option<String>,
    ) {
        let previous = self.profile(profile.id).cloned();
        // The SSH secret goes to the keychain, never into the stored profile.
        let ssh_secret = profile.ssh.as_mut().and_then(|s| s.secret.take());
        let ssh_result = match (&profile.ssh, ssh_secret) {
            (Some(_), Some(secret)) => self.credentials.set(profile.id, SecretKind::Ssh, &secret),
            (Some(_), None) => Ok(()),
            (None, _) => self.credentials.delete(profile.id, SecretKind::Ssh),
        };
        if let Err(e) = ssh_result {
            tracing::warn!("could not store the SSH secret: {e}");
        }
        if let Err(e) = self.store.upsert(&profile) {
            self.status = Some(StatusMessage::error(format!(
                "Could not save connection: {e}"
            )));
            return;
        }
        let secret_result = if profile.save_password {
            self.session_passwords.remove(&profile.id);
            match &password {
                Some(pw) => self.credentials.set(profile.id, SecretKind::Database, pw),
                None => Ok(()),
            }
        } else {
            if let Some(pw) = password {
                self.session_passwords.insert(profile.id, pw);
            }
            self.credentials.delete(profile.id, SecretKind::Database)
        };
        self.status = Some(match secret_result {
            Ok(()) => StatusMessage::info(format!("Saved connection “{}”", profile.name)),
            Err(e) => StatusMessage::error(format!(
                "Saved “{}”, but the password could not be stored: {e}",
                profile.name
            )),
        });
        // Changed connection settings take effect on the next connect.
        let settings_changed = previous.as_ref().is_some_and(|old| {
            (
                &old.kind,
                &old.host,
                old.port,
                &old.database,
                &old.username,
                old.ssl_mode,
                &old.ssh,
            ) != (
                &profile.kind,
                &profile.host,
                profile.port,
                &profile.database,
                &profile.username,
                profile.ssl_mode,
                &profile.ssh,
            )
        });
        if settings_changed
            && self
                .connections
                .get(&profile.id)
                .is_some_and(ConnState::is_connected)
        {
            self.services.send(Command::Disconnect {
                profile_id: profile.id,
            });
        }
        self.reload_profiles();
        self.dialog = None;
    }

    pub(super) fn delete_profile(&mut self, id: ProfileId) {
        if self.connections.remove(&id).is_some() {
            self.services.send(Command::Disconnect { profile_id: id });
        }
        self.session_passwords.remove(&id);
        for kind in [SecretKind::Database, SecretKind::Ssh] {
            if let Err(e) = self.credentials.delete(id, kind) {
                tracing::warn!("could not remove saved secret: {e}");
            }
        }
        match self.store.delete(id) {
            Ok(_) => {
                self.status = Some(StatusMessage::info("Connection deleted"));
                self.reload_profiles();
            }
            Err(e) => {
                self.status = Some(StatusMessage::error(format!(
                    "Could not delete connection: {e}"
                )));
            }
        }
    }
}
