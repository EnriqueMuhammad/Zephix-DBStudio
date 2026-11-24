//! The AI assistant in the app shell: context of the focused query tab, sending questions,
//! inserting answers and storing API keys (ADR 0014).

use rdbs_core::ai::{AiContext, MAX_SCHEMA_CONTEXT, schema_outline, system_prompt};

use super::*;
use crate::ai::{AiAction, InsertMode, PanelContext};

impl StudioApp {
    /// Reads which providers have a saved key (once at start-up and after a save).
    pub(super) fn refresh_ai_keys(&mut self) {
        let saved = self
            .ai
            .providers
            .iter()
            .map(|p| {
                let has = self
                    .credentials
                    .get(p.keychain_id(), SecretKind::AiApiKey)
                    .map(|k| k.is_some())
                    .unwrap_or_else(|e| {
                        tracing::warn!("could not read an AI API key: {e}");
                        false
                    });
                (p.id, has)
            })
            .collect();
        self.ai.key_saved = saved;
    }

    /// Context of the focused query tab, and its one-line summary for the panel.
    /// `outline` builds the schema outline (only needed when a question is sent).
    pub(super) fn ai_context(&mut self, outline: bool) -> (AiContext, PanelContext) {
        let send_schema = self.ai.send_schema;
        let Self {
            workspace,
            profiles,
            connections,
            ..
        } = self;
        let Some(q) = workspace.active_query_tab() else {
            return (
                AiContext::default(),
                PanelContext {
                    summary: "no query tab focused".to_owned(),
                    ..PanelContext::default()
                },
            );
        };
        let profile = q
            .profile_id
            .and_then(|id| profiles.iter().find(|p| p.id == id));
        let database = q
            .database
            .clone()
            .or_else(|| profile.map(|p| p.database.clone()))
            .filter(|d| !d.is_empty());
        let conn = q.profile_id.and_then(|id| connections.get(&id));
        let server_version = conn.and_then(|c| match &c.status {
            crate::model::ConnStatus::Connected { server_version, .. } => {
                Some(server_version.clone())
            }
            _ => None,
        });
        let selection = q
            .run
            .selection
            .clone()
            .and_then(|r| q.sql.get(r))
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned);
        let sql_is_selection = selection.is_some();
        let sql = selection.or_else(|| Some(q.sql.clone()).filter(|s| !s.trim().is_empty()));
        let last_error = q
            .run
            .last
            .as_ref()
            .and_then(|l| l.outcome.as_ref().err())
            .filter(|e| e.kind != DbErrorKind::Cancelled)
            .map(|e| match &e.detail {
                Some(detail) => format!("{e}\n{detail}"),
                None => e.to_string(),
            });
        let has_schema = send_schema && conn.is_some_and(|c| !c.objects.is_empty());
        let schema = if has_schema && outline {
            conn.map(|c| schema_outline(&c.objects, database.as_deref(), MAX_SCHEMA_CONTEXT))
        } else {
            None
        };
        let mut parts = vec![
            profile
                .map(|p| p.kind.display_name().to_owned())
                .unwrap_or_else(|| "no connection".to_owned()),
        ];
        if let Some(db) = &database {
            parts.push(db.clone());
        }
        parts.push(match (&sql, sql_is_selection) {
            (None, _) => "empty editor".to_owned(),
            (Some(_), true) => "selection".to_owned(),
            (Some(_), false) => "editor text".to_owned(),
        });
        if last_error.is_some() {
            parts.push("last error".to_owned());
        }
        if has_schema {
            parts.push("schema".to_owned());
        }
        let panel = PanelContext {
            has_sql: sql.is_some(),
            has_error: last_error.is_some(),
            summary: parts.join(" · "),
        };
        let cx = AiContext {
            engine: profile.map(|p| p.kind.display_name().to_owned()),
            server_version,
            database,
            production: profile.is_some_and(|p| p.environment == Environment::Production),
            schema,
            sql,
            sql_is_selection,
            last_error,
        };
        (cx, panel)
    }

    /// Draws the assistant panel (right of the workspace) and applies what it asks for.
    pub(super) fn ai_panel(&mut self, ui: &mut Ui, actions: &mut Vec<UiAction>) {
        if !self.ai.visible {
            return;
        }
        let (_, panel_cx) = self.ai_context(false);
        let mut asked = Vec::new();
        egui::Panel::right(Id::new("ai-panel"))
            .resizable(true)
            .default_size(380.0)
            .size_range(280.0..=720.0)
            .frame(
                Frame::new()
                    .fill(color::sidebar())
                    .inner_margin(Margin::same(8)),
            )
            .show(ui, |ui| {
                asked = self.ai.panel(ui, &panel_cx);
            });
        self.apply_ai(asked, actions);
    }

    /// The provider settings dialog.
    pub(super) fn ai_dialog(&mut self, ctx: &egui::Context, actions: &mut Vec<UiAction>) {
        let asked = self.ai.settings_dialog(ctx);
        self.apply_ai(asked, actions);
    }

    /// Sends a question with the context of the focused query tab.
    pub(super) fn ai_ask(&mut self, question: String) {
        self.ai.open();
        let Some(provider) = self.ai.provider().cloned() else {
            self.ai
                .refuse(question, "Set up a provider first (gear button above).");
            return;
        };
        if let Some(problem) = provider.problem() {
            self.ai.refuse(question, problem);
            return;
        }
        if self.ai.is_busy() {
            return;
        }
        let (cx, _) = self.ai_context(true);
        let request_id = self.services.next_request_id();
        let messages = self.ai.begin(request_id, question);
        self.services.send(Command::AiChat {
            request_id,
            provider,
            api_key: None,
            system: system_prompt(&cx),
            messages,
        });
    }

    fn apply_ai(&mut self, asked: Vec<AiAction>, actions: &mut Vec<UiAction>) {
        for action in asked {
            match action {
                AiAction::Ask(question) => self.ai_ask(question),
                AiAction::Cancel(request_id) => self.services.send(Command::Cancel { request_id }),
                AiAction::Insert { sql, mode } => self.ai_insert(sql, mode, actions),
                AiAction::StoreKeys(keys) => {
                    for (id, key) in keys {
                        let slot = ProfileId(id);
                        let result = match &key {
                            Some(key) => self.credentials.set(slot, SecretKind::AiApiKey, key),
                            None => self.credentials.delete(slot, SecretKind::AiApiKey),
                        };
                        if let Err(e) = result {
                            self.status = Some(StatusMessage::error(format!(
                                "Could not save the API key in the keychain: {e}"
                            )));
                        }
                    }
                    self.refresh_ai_keys();
                }
                AiAction::ListModels { provider, api_key } => {
                    let request_id = self.services.next_request_id();
                    self.ai.models_requested(provider.id, request_id);
                    self.services.send(Command::AiListModels {
                        request_id,
                        provider,
                        api_key,
                    });
                }
            }
        }
    }

    /// Puts SQL from an answer into the focused query tab (never runs it).
    fn ai_insert(&mut self, sql: String, mode: InsertMode, actions: &mut Vec<UiAction>) {
        let target = self
            .workspace
            .active_query_tab()
            .filter(|q| q.data_view.is_none());
        let Some(q) = target else {
            // No editor to put it in: a new tab is the only sensible place.
            actions.push(UiAction::NewQuery {
                profile_id: None,
                database: None,
                sql: Some(sql),
                title: Some("AI Suggestion".to_owned()),
                run: false,
            });
            return;
        };
        match mode {
            InsertMode::NewTab => actions.push(UiAction::NewQuery {
                profile_id: q.profile_id,
                database: q.database.clone(),
                sql: Some(sql),
                title: Some("AI Suggestion".to_owned()),
                run: false,
            }),
            InsertMode::ReplaceAll => {
                q.sql = sql;
                q.run.selection = None;
                q.run.cursor = None;
                q.run.focus_editor = true;
            }
            InsertMode::AtCursor => {
                let range = insert_range(&q.sql, q.run.selection.clone(), q.run.cursor);
                q.sql.replace_range(range, &sql);
                q.run.selection = None;
                q.run.cursor = None;
                q.run.focus_editor = true;
            }
        }
    }
}

/// Byte range of the editor text an inserted snippet replaces: a valid selection, else the
/// cursor, else the end.
fn insert_range(
    text: &str,
    selection: Option<std::ops::Range<usize>>,
    cursor: Option<usize>,
) -> std::ops::Range<usize> {
    let valid = |i: usize| i <= text.len() && text.is_char_boundary(i);
    if let Some(r) = selection
        && r.start < r.end
        && valid(r.start)
        && valid(r.end)
    {
        return r;
    }
    match cursor {
        Some(c) if valid(c) => c..c,
        _ => text.len()..text.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::insert_range;

    #[test]
    fn insert_prefers_selection_then_cursor_then_end() {
        let t = "select 1;\nselect 2;";
        assert_eq!(insert_range(t, Some(0..8), Some(3)), 0..8);
        assert_eq!(insert_range(t, Some(4..4), Some(3)), 3..3);
        assert_eq!(insert_range(t, Some(100..200), None), t.len()..t.len());
        assert_eq!(
            insert_range("é", None, Some(1)),
            2..2,
            "not a char boundary"
        );
    }
}
