//! Worker events: query results, object trees, backups and tool results.

use super::*;

impl StudioApp {
    pub(super) fn drain_events(&mut self) {
        while let Some(event) = self.services.try_recv() {
            self.handle_event(event);
        }
    }

    pub(super) fn handle_event(&mut self, event: Event) {
        tracing::debug!(?event, "worker event");
        if self.handle_backup_event(&event) {
            return;
        }
        // Tool tabs consume the results of their own requests.
        if matches!(
            event,
            Event::ChangesApplied { .. } | Event::QueryFinished { .. }
        ) && self.workspace.dispatch_tool_event(&event)
        {
            return;
        }
        let name_of = |profiles: &[ConnectionProfile], id: ProfileId| {
            profiles
                .iter()
                .find(|p| p.id == id)
                .map_or_else(|| "connection".to_owned(), |p| p.name.clone())
        };
        match event {
            Event::Connecting { profile_id } => {
                self.connections.entry(profile_id).or_default().status = ConnStatus::Connecting;
            }
            Event::Connected {
                profile_id,
                server_version,
                capabilities,
            } => {
                self.status = Some(StatusMessage::info(format!(
                    "Connected to {} · {server_version}",
                    name_of(&self.profiles, profile_id)
                )));
                let conn = self.connections.entry(profile_id).or_default();
                // A fresh main session replaces one that was dropped.
                conn.dropped.remove(&None);
                conn.status = ConnStatus::Connected {
                    server_version,
                    capabilities,
                };
            }
            Event::ConnectionFailed { profile_id, error } => {
                if self
                    .reconnect
                    .as_ref()
                    .is_some_and(|intent| intent.profile_id == profile_id)
                {
                    self.reconnect = None;
                }
                self.status = Some(StatusMessage::error(format!(
                    "{}: {error}",
                    name_of(&self.profiles, profile_id)
                )));
                if error.kind == DbErrorKind::Authentication {
                    // A rejected password must not be retried silently.
                    self.session_passwords.remove(&profile_id);
                    if self.prompt.is_none() && self.dialog.is_none() {
                        self.open_prompt(
                            profile_id,
                            AfterPassword::Connect,
                            Some(error.message.clone()),
                        );
                    }
                }
                let conn = self.connections.entry(profile_id).or_default();
                conn.status = ConnStatus::Failed(error);
                conn.objects.clear();
            }
            Event::Disconnected { profile_id } => {
                if let Some(conn) = self.connections.get_mut(&profile_id) {
                    *conn = ConnState::default();
                }
                if self
                    .reconnect
                    .as_ref()
                    .is_some_and(|intent| intent.profile_id == profile_id)
                {
                    self.reconnect = None;
                }
                self.explorer.collapse_profile(profile_id);
                self.status = Some(StatusMessage::info(format!(
                    "Disconnected from {}",
                    name_of(&self.profiles, profile_id)
                )));
            }
            Event::ConnectionLost {
                profile_id,
                database,
                ..
            } => {
                let name = name_of(&self.profiles, profile_id);
                let which = database
                    .as_ref()
                    .map(|db| format!(" ({db})"))
                    .unwrap_or_default();
                self.status = Some(StatusMessage::error(format!(
                    "Connection to {name}{which} was lost. Reconnect keeps this tab and its text."
                )));
                self.connections
                    .entry(profile_id)
                    .or_default()
                    .dropped
                    .insert(database);
            }
            Event::Reconnected {
                profile_id,
                database,
            } => {
                if let Some(conn) = self.connections.get_mut(&profile_id) {
                    conn.dropped.remove(&database);
                }
                let retry = self.take_reconnect_retry(profile_id, &database);
                let name = name_of(&self.profiles, profile_id);
                self.status = Some(StatusMessage::info(format!("Reconnected to {name}")));
                if let Some(tab_id) = retry {
                    let follow_up = self.run_tab(tab_id);
                    self.apply(follow_up);
                }
            }
            Event::SessionsProbed { .. } => {}
            Event::ReconnectFailed {
                profile_id,
                database,
                error,
            } => {
                let _ = self.take_reconnect_retry(profile_id, &database);
                self.status = Some(StatusMessage::error(format!(
                    "Could not reconnect: {error}"
                )));
            }
            Event::ObjectsLoaded {
                profile_id,
                objects,
            } => {
                let conn = self.connections.entry(profile_id).or_default();
                conn.default_database = objects
                    .iter()
                    .find(|o| o.kind == rdbs_core::ObjectKind::Database && !o.children.is_empty())
                    .map(|o| o.name.clone());
                conn.loading_databases.clear();
                conn.database_errors.clear();
                conn.objects = objects;
                if self.status.as_ref().is_some_and(|s| s.text == REFRESHING) {
                    self.status = Some(StatusMessage::info("Objects refreshed"));
                }
            }
            Event::ObjectsFailed { profile_id, error } => {
                self.status = Some(StatusMessage::error(format!(
                    "Could not load objects of {}: {error}",
                    name_of(&self.profiles, profile_id)
                )));
            }
            Event::DatabaseObjectsLoaded {
                profile_id,
                database,
                objects,
            } => {
                if let Some(conn) = self.connections.get_mut(&profile_id) {
                    conn.loading_databases.remove(&database);
                    conn.database_errors.remove(&database);
                    if let Some(node) = conn.objects.iter_mut().find(|o| o.name == database) {
                        node.children = objects;
                        node.detail = None;
                    }
                }
            }
            Event::DatabaseObjectsFailed {
                profile_id,
                database,
                error,
            } => {
                if let Some(conn) = self.connections.get_mut(&profile_id) {
                    conn.loading_databases.remove(&database);
                    // Shown under the expanded node; collapsing and expanding retries.
                    conn.database_errors.insert(
                        database.clone(),
                        format!("Could not load: {} (expand again to retry)", error.message),
                    );
                }
                self.status = Some(StatusMessage::error(format!(
                    "Could not open database {database}: {error}"
                )));
            }
            Event::ChangesApplied { request_id, result } => {
                let mut rerun = None;
                if let Some(q) = self.workspace.tab_for_save(request_id) {
                    q.run.saving = None;
                    match result {
                        Ok(rows) => {
                            q.run.edits = rdbs_core::edit::PendingEdits::default();
                            q.run.editing = None;
                            q.run.edit_error = None;
                            self.status = Some(StatusMessage::info(format!(
                                "Saved changes to {rows} row(s)"
                            )));
                            rerun = Some(q.id);
                        }
                        Err(e) => {
                            q.run.edit_error = Some(e.to_string());
                            self.status =
                                Some(StatusMessage::error(format!("Changes were not saved: {e}")));
                        }
                    }
                }
                if let Some(tab_id) = rerun {
                    let follow_up = self.run_tab(tab_id);
                    self.apply(follow_up);
                }
            }
            Event::DdlLoaded { request_id, result } => {
                if let Some(request) = self.ddl_requests.remove(&request_id) {
                    match result {
                        Ok(ddl) => {
                            let text = match &request.edit {
                                Some(t) => rdbs_core::routine::edit_script(
                                    t.kind,
                                    &t.qualified,
                                    t.routine,
                                    &ddl,
                                ),
                                None => ddl,
                            };
                            let id = self.workspace.open_query_tab(
                                Some(request.profile_id),
                                text,
                                Some(request.title),
                            );
                            if let Some(q) = self.workspace.query_tab_mut(id) {
                                q.database = request.database;
                            }
                            self.status = None;
                        }
                        Err(e) => {
                            self.status = Some(StatusMessage::error(format!(
                                "Could not load the definition: {e}"
                            )));
                        }
                    }
                }
            }
            Event::RoutineLoaded { request_id, result } => {
                if let Some(target) = self.routine_requests.remove(&request_id) {
                    match result {
                        Ok(signature) => {
                            self.status = None;
                            if signature.params.iter().any(|p| p.mode.takes_value()) {
                                let n = signature.params.len();
                                self.exec = Some(ExecDialog {
                                    target,
                                    signature,
                                    values: vec![String::new(); n],
                                    nulls: vec![false; n],
                                });
                            } else {
                                let sql = rdbs_core::routine::call_statement(
                                    target.kind,
                                    &target.qualified,
                                    target.routine,
                                    &signature,
                                    &[],
                                );
                                self.run_routine(&target, sql);
                            }
                        }
                        Err(e) => {
                            self.status = Some(StatusMessage::error(format!(
                                "Could not load the parameters of {}: {e}",
                                target.name
                            )));
                        }
                    }
                }
            }
            Event::PlanReady { request_id, result } => {
                if let Some(q) = self.workspace.tab_for_plan(request_id) {
                    q.run.plan_request = None;
                    q.run.plan = Some(result);
                    q.run.view = crate::workspace::ResultView::Plan;
                }
            }
            Event::SchemaDescribed { .. }
            | Event::ScriptFinished { .. }
            | Event::JobProgress { .. } => {
                self.dispatch_tool_event(event);
            }
            Event::JobFinished { .. } => {
                // The tool tab shows the details; the status bar replaces "... started".
                if self.workspace.dispatch_tool_event(&event)
                    && let Event::JobFinished { result, .. } = &event
                {
                    self.status = Some(match result {
                        Ok(summary) => StatusMessage::info(format!("Finished: {summary}")),
                        Err(e) => {
                            StatusMessage::error(format!("Failed: {}", crate::tools::error_text(e)))
                        }
                    });
                } else {
                    tracing::debug!("no tab waits for this tool result (closed?)");
                }
            }
            Event::QueryConnecting { request_id } => {
                if let Some(running) = self
                    .workspace
                    .tab_for_request(request_id)
                    .and_then(|query| query.run.running.as_mut())
                {
                    running.phase = RunPhase::Connecting;
                }
            }
            Event::QueryStarted { request_id } => {
                // A new run supersedes an earlier error such as "choose a connection first".
                if self
                    .status
                    .as_ref()
                    .is_some_and(|s| s.severity == Severity::Error)
                {
                    self.status = None;
                }
                if let Some(running) = self
                    .workspace
                    .tab_for_request(request_id)
                    .and_then(|query| query.run.running.as_mut())
                {
                    running.phase = RunPhase::Running;
                }
            }
            Event::QueryFinished {
                request_id,
                elapsed,
                result,
            } => {
                let mut finished = None;
                let mut recovered = None;
                if let Some(q) = self.workspace.tab_for_request(request_id) {
                    let running = q.run.running.take();
                    let profile_name = running
                        .as_ref()
                        .map(|r| r.profile_name.clone())
                        .unwrap_or_default();
                    let has_results = result.as_ref().is_ok_and(|o| !o.results.is_empty());
                    // A syntax error still means the session accepted the statement. Only a
                    // lost connection leaves the "session dropped" mark in place.
                    let lost = result
                        .as_ref()
                        .is_err_and(|error| error.is_connection_lost());
                    if !lost && let Some(running) = &running {
                        recovered = Some((running.profile_id, running.database.clone()));
                    }
                    let last = LastRun {
                        elapsed,
                        outcome: result,
                        profile_name,
                        finished: chrono::Local::now(),
                    };
                    q.run.grid = GridState::default();
                    q.run.stacked_grids.clear();
                    q.run.show_messages = !has_results;
                    q.run.active_result = 0;
                    if let Some(running) = &running {
                        finished = Some(history_entry(running, last.elapsed, &last.outcome));
                    }
                    q.run.last = Some(last);
                }
                if let Some(entry) = finished {
                    self.record_history(entry);
                }
                if let Some((profile_id, database)) = recovered {
                    let key = self.normalized_session(profile_id, &database);
                    if let Some(conn) = self.connections.get_mut(&profile_id) {
                        conn.dropped.remove(&key);
                    }
                }
            }
            Event::AgentJobFinished { result, .. } => {
                self.status = Some(match result {
                    Ok(message) => StatusMessage::info(message),
                    Err(error) => StatusMessage::error(error.message),
                });
            }
            Event::TestFinished { request_id, result } => {
                if let Some(dialog) = &mut self.dialog
                    && matches!(dialog.test, TestState::Running(id) if id == request_id)
                {
                    dialog.test = match result {
                        Ok(version) => TestState::Succeeded(version),
                        Err(e) => TestState::Failed(e),
                    };
                }
            }
            Event::UpdateChecked { manual, result } => self.update.finished(manual, result),
            Event::AiChunk { request_id, text } => self.ai.chunk(request_id, &text),
            Event::AiFinished { request_id, result } => self.ai.finished(request_id, result),
            Event::AiModelsListed { request_id, result } => {
                self.ai.models_listed(request_id, result);
            }
        }
    }

    /// Progress and results of backup jobs started by the app.
    pub(super) fn handle_backup_event(&mut self, event: &Event) -> bool {
        match event {
            Event::JobProgress {
                request_id,
                message,
                ..
            } => match self.backups.running.get(request_id) {
                Some(id) => {
                    self.backups.status.insert(id.clone(), message.clone());
                    true
                }
                None => false,
            },
            Event::JobFinished { request_id, result } => {
                let Some(id) = self.backups.running.remove(request_id) else {
                    return false;
                };
                self.backups.status.remove(&id);
                let outcome = result.clone().map_err(|e| crate::tools::error_text(&e));
                self.status = Some(match &outcome {
                    Ok(summary) => StatusMessage::info(format!("Backup finished: {summary}")),
                    Err(e) => StatusMessage::error(format!("Backup failed: {e}")),
                });
                if id.is_empty() {
                    self.backups.adhoc_result = Some(outcome);
                } else if let Some(s) = self.backups.schedules.iter_mut().find(|s| s.id == id) {
                    s.last_result = Some(outcome);
                    let saved = s.clone();
                    if let Err(e) = self.store.save_schedule(&saved) {
                        tracing::warn!("could not save backup schedule: {e}");
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Starts a backup job; `schedule` is `""` for a one-off backup.
    pub(super) fn start_backup(
        &mut self,
        schedule: String,
        profile_id: ProfileId,
        database: Option<String>,
        options: rdbs_core::job::BackupOptions,
    ) -> Result<(), String> {
        let profile = self
            .profile(profile_id)
            .cloned()
            .ok_or_else(|| "the connection no longer exists".to_owned())?;
        let password = self.session_passwords.get(&profile_id).cloned();
        if profile.kind.is_networked() && !profile.save_password && password.is_none() {
            return Err(format!(
                "{} does not save its password; connect once in this session or save the password",
                profile.name
            ));
        }
        let request_id = self.services.next_request_id();
        self.services.send(Command::Backup {
            request_id,
            profile,
            database,
            password,
            options,
        });
        self.backups
            .status
            .insert(schedule.clone(), "Starting…".into());
        if schedule.is_empty() {
            self.backups.adhoc_result = None;
        }
        self.backups.running.insert(request_id, schedule);
        Ok(())
    }

    /// Runs due schedules (checked every 20 seconds).
    pub(super) fn tick_schedules(&mut self, ctx: &egui::Context) {
        if !self.backups.schedules.iter().any(|s| s.enabled) {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(20));
        if self
            .backups
            .last_check
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(20))
        {
            return;
        }
        self.backups.last_check = Some(std::time::Instant::now());
        let now = chrono::Local::now();
        let due: Vec<String> = self
            .backups
            .schedules
            .iter()
            .filter(|s| s.enabled && !self.backups.status.contains_key(&s.id))
            .filter(|s| rdbs_transfer::backup::is_due(s.frequency, s.last_run, now))
            .map(|s| s.id.clone())
            .collect();
        for id in due {
            self.run_schedule(&id);
        }
    }

    pub(super) fn run_schedule(&mut self, id: &str) {
        let Some(s) = self.backups.schedules.iter_mut().find(|s| s.id == id) else {
            return;
        };
        s.last_run = chrono::Local::now().timestamp();
        let (profile_id, database, options) = (s.profile_id, s.database.clone(), s.options.clone());
        let started = self.start_backup(id.to_owned(), profile_id, database, options);
        if let Some(s) = self.backups.schedules.iter_mut().find(|s| s.id == id) {
            if let Err(e) = started {
                s.last_result = Some(Err(e));
            }
            let saved = s.clone();
            if let Err(e) = self.store.save_schedule(&saved) {
                tracing::warn!("could not save backup schedule: {e}");
            }
        }
    }

    /// Routes results of tool requests (designer, diff, …) to the tab that sent them.
    pub(super) fn dispatch_tool_event(&mut self, event: Event) {
        if !self.workspace.dispatch_tool_event(&event) {
            tracing::debug!("no tab waits for this tool result (closed?)");
        }
    }
}
