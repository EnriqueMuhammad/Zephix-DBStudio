//! Running and explaining a query tab: the SQL to send, passwords and in-flight status.

use super::*;

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
    start_query(q, &profile, sql_text, cx.services, password, cx.connections);
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

/// Phase to show the moment the user runs a statement. An open session can say Running
/// immediately; anything else stays on Connecting until [`rdbs_core::Event::QueryStarted`].
pub fn phase_for(
    profile: &ConnectionProfile,
    database: &Option<String>,
    connections: &std::collections::HashMap<rdbs_core::ProfileId, ConnState>,
) -> RunPhase {
    let key = profile.session_database(database.clone());
    let open = connections
        .get(&profile.id)
        .is_some_and(|conn| conn.is_connected() && !conn.session_dropped(&key));
    if open {
        RunPhase::Running
    } else {
        RunPhase::Connecting
    }
}

/// Status-line text while a query is in flight.
pub fn in_flight_status(phase: RunPhase, name: &str, elapsed: Duration) -> String {
    format!(
        "{}  ·  {}  ·  {}",
        phase.label(),
        name,
        format_duration(elapsed)
    )
}

/// Spinner caption in the query tab while a query is in flight.
pub fn in_flight_banner(phase: RunPhase, name: &str, elapsed: Duration) -> String {
    format!(
        "{}  {}  ·  {}",
        phase.label(),
        name,
        format_duration(elapsed)
    )
}

/// Sends the query to the worker and marks the tab as in flight.
pub fn start_query(
    q: &mut QueryTab,
    profile: &ConnectionProfile,
    sql_text: String,
    services: &ServiceHandle,
    password: Option<String>,
    connections: &std::collections::HashMap<rdbs_core::ProfileId, ConnState>,
) {
    let request_id = services.next_request_id();
    let executed = sql_text.clone();
    let phase = phase_for(profile, &q.database, connections);
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
        phase,
    });
    q.run.show_messages = false;
    q.run.view = crate::workspace::ResultView::Results;
}
