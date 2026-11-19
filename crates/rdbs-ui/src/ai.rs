//! AI assistant panel and its provider settings (ADR 0014).
//!
//! The panel is a chat on the right of the workspace. The app gives it the context of the
//! focused query tab; the worker sends the request and streams the answer back. Code blocks
//! in answers can be inserted into the editor, but nothing is ever run from here.

use std::collections::HashMap;

use egui::{Color32, Frame, Id, Key, Margin, Modifiers, RichText, Ui};
use egui_phosphor::regular as icon;
use rdbs_core::RequestId;
use rdbs_core::ai::{
    AiMessage, AiPreset, AiProtocol, AiProvider, AiRole, PRESETS, QuickAsk, Segment,
    split_code_blocks,
};
use uuid::Uuid;

use crate::theme::{self, color};

/// Persisted: configured providers.
pub(crate) const PROVIDERS_KEY: &str = "rdbs.ai.providers";
/// Persisted: id of the provider the panel uses.
pub(crate) const ACTIVE_KEY: &str = "rdbs.ai.active";
/// Persisted: the panel is open.
pub(crate) const VISIBLE_KEY: &str = "rdbs.ai.visible";
/// Persisted: table and column names are sent with each question.
pub(crate) const SEND_SCHEMA_KEY: &str = "rdbs.ai.send_schema";

/// Earlier turns sent with a question (the oldest are dropped first).
const HISTORY_TURNS: usize = 20;

/// Where a code block from an answer goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InsertMode {
    /// Replace the selection, or insert at the cursor.
    AtCursor,
    /// Replace the whole editor text.
    ReplaceAll,
    /// Open a new query tab on the same connection.
    NewTab,
}

/// Something the assistant asks the app to do.
pub(crate) enum AiAction {
    /// Ask this question with the context of the focused query tab.
    Ask(String),
    /// Stop the answer being written.
    Cancel(RequestId),
    /// Put SQL into the editor.
    Insert {
        /// The code.
        sql: String,
        /// Where.
        mode: InsertMode,
    },
    /// Save (`Some`) or delete (`None`) API keys in the keychain.
    StoreKeys(Vec<(Uuid, Option<String>)>),
    /// List the models of a provider being edited.
    ListModels {
        /// The provider as edited (may be unsaved).
        provider: AiProvider,
        /// Key typed in the dialog, if any.
        api_key: Option<String>,
    },
}

/// What the app tells the panel about the focused query tab.
#[derive(Debug, Default, Clone)]
pub(crate) struct PanelContext {
    /// A query tab with text is focused.
    pub has_sql: bool,
    /// Its last run failed.
    pub has_error: bool,
    /// One line describing what will be sent, e.g. "PostgreSQL · shop · selection".
    pub summary: String,
}

struct Entry {
    role: AiRole,
    text: String,
    /// The request failed or was cancelled (shown under the text).
    error: Option<String>,
}

/// A key typed in the settings dialog.
#[derive(Default)]
struct KeyEdit {
    text: String,
    /// Delete the saved key.
    clear: bool,
}

/// Model list of one provider: its id, the request in flight, and the last answer.
type ModelList = (Uuid, Option<RequestId>, Result<Vec<String>, String>);

struct Settings {
    providers: Vec<AiProvider>,
    selected: Option<usize>,
    active: Option<Uuid>,
    saved_keys: HashMap<Uuid, bool>,
    keys: HashMap<Uuid, KeyEdit>,
    show_key: bool,
    models: Option<ModelList>,
    /// Providers removed in this dialog (their keys are deleted on save).
    removed: Vec<Uuid>,
}

/// Assistant state owned by the app.
pub(crate) struct AiState {
    /// Configured providers.
    pub providers: Vec<AiProvider>,
    /// Provider in use.
    pub active: Option<Uuid>,
    /// The panel is open.
    pub visible: bool,
    /// Send table and column names as context.
    pub send_schema: bool,
    /// Whether a provider has an API key in the keychain (read once, updated on save).
    pub key_saved: HashMap<Uuid, bool>,
    chat: Vec<Entry>,
    input: String,
    pending: Option<RequestId>,
    focus_input: bool,
    settings: Option<Settings>,
}

impl AiState {
    pub fn new(
        providers: Vec<AiProvider>,
        active: Option<Uuid>,
        visible: bool,
        send_schema: bool,
    ) -> Self {
        let active = active
            .filter(|id| providers.iter().any(|p| p.id == *id))
            .or_else(|| providers.first().map(|p| p.id));
        Self {
            providers,
            active,
            visible,
            send_schema,
            key_saved: HashMap::new(),
            chat: Vec::new(),
            input: String::new(),
            pending: None,
            focus_input: false,
            settings: None,
        }
    }

    /// The provider in use.
    pub fn provider(&self) -> Option<&AiProvider> {
        let id = self.active?;
        self.providers.iter().find(|p| p.id == id)
    }

    /// Opens the panel with the cursor in the question box.
    pub fn open(&mut self) {
        self.visible = true;
        self.focus_input = true;
    }

    /// Shows or hides the panel.
    pub fn toggle(&mut self) {
        if self.visible {
            self.visible = false;
        } else {
            self.open();
        }
    }

    /// An answer is being written.
    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// The settings dialog is open (it owns the keyboard).
    pub fn dialog_open(&self) -> bool {
        self.settings.is_some()
    }

    /// Opens the settings dialog. `has_key` says whether a provider has a saved key.
    pub fn open_settings(&mut self) {
        let saved_keys = self.key_saved.clone();
        self.settings = Some(Settings {
            selected: self
                .active
                .and_then(|id| self.providers.iter().position(|p| p.id == id))
                .or((!self.providers.is_empty()).then_some(0)),
            providers: self.providers.clone(),
            active: self.active,
            saved_keys,
            keys: HashMap::new(),
            show_key: false,
            models: None,
            removed: Vec::new(),
        });
    }

    /// The conversation to send for a new question: earlier turns plus `question`. Records
    /// the question and an empty answer, and marks `request_id` as pending.
    pub fn begin(&mut self, request_id: RequestId, question: String) -> Vec<AiMessage> {
        // Earlier complete turns only; failed answers without text are left out.
        let mut messages: Vec<AiMessage> = self
            .chat
            .iter()
            .filter(|e| !e.text.trim().is_empty())
            .map(|e| AiMessage {
                role: e.role,
                content: e.text.clone(),
            })
            .collect();
        // The conversation must alternate and start with the user; drop a dangling turn.
        let mut cleaned: Vec<AiMessage> = Vec::new();
        for m in messages.drain(..) {
            if cleaned.last().is_some_and(|l| l.role == m.role) {
                cleaned.pop();
            }
            cleaned.push(m);
        }
        if cleaned.last().is_some_and(|m| m.role == AiRole::User) {
            cleaned.pop();
        }
        let skip = cleaned.len().saturating_sub(HISTORY_TURNS);
        let mut messages: Vec<AiMessage> = cleaned.into_iter().skip(skip).collect();
        if messages
            .first()
            .is_some_and(|m| m.role == AiRole::Assistant)
        {
            messages.remove(0);
        }
        messages.push(AiMessage::user(question.clone()));
        self.chat.push(Entry {
            role: AiRole::User,
            text: question,
            error: None,
        });
        self.chat.push(Entry {
            role: AiRole::Assistant,
            text: String::new(),
            error: None,
        });
        self.pending = Some(request_id);
        messages
    }

    /// Shows an error as the answer without sending anything (no provider, bad settings).
    pub fn refuse(&mut self, question: String, reason: &str) {
        self.chat.push(Entry {
            role: AiRole::User,
            text: question,
            error: None,
        });
        self.chat.push(Entry {
            role: AiRole::Assistant,
            text: String::new(),
            error: Some(reason.to_owned()),
        });
    }

    /// Handles `Event::AiChunk`.
    pub fn chunk(&mut self, request_id: RequestId, text: &str) {
        if self.pending == Some(request_id)
            && let Some(last) = self.chat.last_mut()
        {
            last.text.push_str(text);
        }
    }

    /// Handles `Event::AiFinished`.
    pub fn finished(&mut self, request_id: RequestId, result: Result<(), String>) {
        if self.pending != Some(request_id) {
            return; // Cancelled earlier, or a cleared chat.
        }
        self.pending = None;
        if let (Err(message), Some(last)) = (result, self.chat.last_mut()) {
            last.error = Some(message);
        }
    }

    /// Stops waiting for the pending answer (the worker is asked to cancel it too).
    fn cancel_pending(&mut self) -> Option<RequestId> {
        let id = self.pending.take()?;
        if let Some(last) = self.chat.last_mut() {
            last.error = Some("Stopped.".to_owned());
        }
        Some(id)
    }

    /// Records that a model list was asked for.
    pub fn models_requested(&mut self, provider: Uuid, request_id: RequestId) {
        if let Some(s) = &mut self.settings {
            s.models = Some((provider, Some(request_id), Ok(Vec::new())));
        }
    }

    /// Handles `Event::AiModelsListed`.
    pub fn models_listed(&mut self, request_id: RequestId, result: Result<Vec<String>, String>) {
        if let Some(Settings {
            models: Some((_, pending, slot)),
            ..
        }) = &mut self.settings
            && *pending == Some(request_id)
        {
            *pending = None;
            *slot = result;
        }
    }

    /// The panel on the right of the workspace.
    pub fn panel(&mut self, ui: &mut Ui, cx: &PanelContext) -> Vec<AiAction> {
        let mut actions = Vec::new();
        let mut open_settings = false;
        // Header.
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(icon::SPARKLE)
                    .color(color::accent())
                    .size(16.0),
            );
            ui.label(RichText::new("AI Assistant").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(icon::X).frame(false))
                    .on_hover_text("Close (Ctrl+Shift+I)")
                    .clicked()
                {
                    self.visible = false;
                }
                if ui
                    .add(egui::Button::new(icon::GEAR).frame(false))
                    .on_hover_text("Providers and API keys")
                    .clicked()
                {
                    open_settings = true;
                }
                if ui
                    .add_enabled(
                        !self.chat.is_empty(),
                        egui::Button::new(icon::TRASH).frame(false),
                    )
                    .on_hover_text("Clear the conversation")
                    .clicked()
                {
                    if let Some(id) = self.cancel_pending() {
                        actions.push(AiAction::Cancel(id));
                    }
                    self.chat.clear();
                }
            });
        });
        // Provider picker.
        if self.providers.is_empty() {
            ui.add_space(12.0);
            ui.label("Connect an AI provider to get help with queries, errors and schemas.");
            ui.label(
                RichText::new(
                    "Hosted services (Anthropic, OpenAI, Gemini, OpenRouter, …), local servers \
                     (Ollama, LM Studio) or any OpenAI-compatible endpoint.",
                )
                .small()
                .color(color::text_dim()),
            );
            ui.add_space(6.0);
            if ui
                .button(format!("{}  Set Up a Provider…", icon::PLUS))
                .clicked()
            {
                open_settings = true;
            }
        } else {
            ui.horizontal(|ui| {
                let current = self
                    .provider()
                    .map(|p| format!("{} · {}", p.name, p.model))
                    .unwrap_or_else(|| "Choose a provider".to_owned());
                egui::ComboBox::from_id_salt("ai-provider")
                    .width(ui.available_width())
                    .truncate()
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for p in &self.providers {
                            ui.selectable_value(
                                &mut self.active,
                                Some(p.id),
                                format!("{} · {}", p.name, p.model),
                            );
                        }
                    });
            });
            if let Some(p) = self.provider() {
                let problem = p.problem().map(str::to_owned).or_else(|| {
                    (p.needs_key && !self.key_saved.get(&p.id).copied().unwrap_or(false))
                        .then(|| "No API key is saved.".to_owned())
                });
                if let Some(problem) = problem {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::WARNING).color(color::warning()));
                        ui.label(RichText::new(problem).small().color(color::text_dim()));
                        if ui.small_button("Settings…").clicked() {
                            open_settings = true;
                        }
                    });
                }
            }
        }
        ui.add_space(4.0);
        // Context and quick requests.
        ui.label(
            RichText::new(format!("Context: {}", cx.summary))
                .small()
                .color(color::text_faint()),
        )
        .on_hover_text(
            "Sent with every question: the engine, the editor text (or its selection), the \
             error of the last run and, if ticked below, table and column names. Result rows \
             are never sent.",
        );
        ui.checkbox(&mut self.send_schema, "Send table and column names")
            .on_hover_text("From the explorer tree of the connection (no data)");
        let ready = self.provider().is_some() && !self.is_busy();
        ui.horizontal_wrapped(|ui| {
            for q in QuickAsk::ALL {
                let enabled = ready && cx.has_sql && (!q.needs_error() || cx.has_error);
                let hint = if q.needs_error() && !cx.has_error {
                    "The last run in this tab did not fail"
                } else {
                    "Focus a query tab with some SQL first"
                };
                if ui
                    .add_enabled(enabled, egui::Button::new(q.label()).small())
                    .on_disabled_hover_text(hint)
                    .on_hover_text(q.prompt())
                    .clicked()
                {
                    actions.push(AiAction::Ask(q.prompt().to_owned()));
                }
            }
        });
        ui.separator();

        // Question box at the bottom; the conversation takes the rest.
        let input_id = Id::new("ai-input");
        let mut send = false;
        egui::Panel::bottom(Id::new("ai-input-panel"))
            .frame(Frame::new().inner_margin(Margin::symmetric(0, 6)))
            .show(ui, |ui| {
                if ui.memory(|m| m.has_focus(input_id))
                    && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
                {
                    send = true;
                }
                let edit = ui.add(
                    egui::TextEdit::multiline(&mut self.input)
                        .id(input_id)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text("Ask about this query, an error or the schema…  (Enter sends, Shift+Enter for a new line)"),
                );
                if self.focus_input {
                    edit.request_focus();
                    self.focus_input = false;
                }
                ui.horizontal(|ui| {
                    if let Some(id) = self.pending {
                        ui.add(egui::Spinner::new().size(12.0));
                        ui.label(RichText::new("Writing…").small().color(color::text_dim()));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button(format!("{}  Stop", icon::STOP)).clicked() {
                                self.cancel_pending();
                                actions.push(AiAction::Cancel(id));
                            }
                        });
                    } else {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            send |= ui
                                .add_enabled(
                                    ready && !self.input.trim().is_empty(),
                                    egui::Button::new(format!("{}  Send", icon::PAPER_PLANE_RIGHT)),
                                )
                                .clicked();
                        });
                    }
                });
            });
        if send && ready && !self.input.trim().is_empty() {
            actions.push(AiAction::Ask(
                std::mem::take(&mut self.input).trim().to_owned(),
            ));
            self.focus_input = true;
        }

        egui::ScrollArea::vertical()
            .id_salt("ai-chat")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.chat.is_empty() && !self.providers.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Ask a question, or use a button above on the focused query tab. \
                             Suggested SQL can be inserted into the editor; it is never run \
                             automatically.",
                        )
                        .color(color::text_dim()),
                    );
                }
                let last = self.chat.len().saturating_sub(1);
                for (i, entry) in self.chat.iter().enumerate() {
                    let writing = i == last && self.pending.is_some();
                    show_entry(ui, i, entry, writing, &mut actions);
                    ui.add_space(6.0);
                }
            });

        if open_settings {
            self.open_settings();
        }
        actions
    }

    /// The provider settings dialog.
    pub fn settings_dialog(&mut self, ctx: &egui::Context) -> Vec<AiAction> {
        let mut actions = Vec::new();
        let Some(s) = &mut self.settings else {
            return actions;
        };
        let mut save = false;
        let mut close = false;
        let response = egui::Modal::new(Id::new("ai-settings")).show(ctx, |ui| {
            ui.set_width(620.0);
            ui.heading("AI Assistant Providers");
            ui.label(
                RichText::new(
                    "Questions go straight from this computer to the provider you choose, \
                     with the context shown in the panel. API keys are kept in the OS \
                     keychain.",
                )
                .small()
                .color(color::text_dim()),
            );
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                // Provider list.
                ui.vertical(|ui| {
                    ui.set_width(180.0);
                    Frame::new()
                        .stroke(egui::Stroke::new(1.0, color::border()))
                        .corner_radius(4)
                        .inner_margin(Margin::same(4))
                        .show(ui, |ui| {
                            ui.set_width(170.0);
                            ui.set_min_height(120.0);
                            egui::ScrollArea::vertical()
                                .id_salt("ai-provider-list")
                                .max_height(300.0)
                                .show(ui, |ui| {
                                    for (i, p) in s.providers.iter().enumerate() {
                                        let mark = if s.active == Some(p.id) {
                                            format!("{}  {}", icon::CHECK, p.name)
                                        } else {
                                            format!("     {}", p.name)
                                        };
                                        if ui
                                            .selectable_label(s.selected == Some(i), mark)
                                            .clicked()
                                        {
                                            s.selected = Some(i);
                                            s.show_key = false;
                                        }
                                    }
                                });
                        });
                    ui.add_space(4.0);
                    ui.menu_button(format!("{}  Add Provider", icon::PLUS), |ui| {
                        for preset in PRESETS {
                            if ui.button(preset.name).clicked() {
                                add_preset(s, preset);
                                ui.close();
                            }
                        }
                    });
                });
                // No vertical separator here: in a modal it takes the whole screen height
                // and the dialog grows past the window, showing only an empty middle.
                ui.add_space(12.0);
                // Editor of the selected provider.
                ui.vertical(|ui| match s.selected.filter(|i| *i < s.providers.len()) {
                    None => {
                        ui.label(
                            RichText::new("Add a provider to start.").color(color::text_dim()),
                        );
                    }
                    Some(i) => {
                        if let Some(a) = provider_editor(ui, s, i) {
                            actions.push(a);
                        }
                    }
                });
            });
            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                save = ui.button(format!("{}  Save", icon::CHECK)).clicked();
                close = ui.button("Cancel").clicked();
            });
        });
        if save {
            let keys = key_changes(s);
            if !keys.is_empty() {
                actions.push(AiAction::StoreKeys(keys));
            }
            self.providers = std::mem::take(&mut s.providers);
            self.active = s
                .active
                .filter(|id| self.providers.iter().any(|p| p.id == *id))
                .or_else(|| self.providers.first().map(|p| p.id));
            self.settings = None;
            self.open();
        } else if close || response.should_close() {
            self.settings = None;
        }
        actions
    }
}

/// Keychain writes for Save: typed keys, removed keys, and keys of removed providers.
fn key_changes(s: &mut Settings) -> Vec<(Uuid, Option<String>)> {
    let mut keys = Vec::new();
    for (id, edit) in s.keys.drain() {
        let present = s.providers.iter().find(|p| p.id == id);
        let Some(provider) = present else {
            continue;
        };
        if edit.clear || !provider.needs_key {
            if s.saved_keys.get(&id).copied().unwrap_or(false) {
                keys.push((id, None));
            }
        } else if !edit.text.trim().is_empty() {
            keys.push((id, Some(edit.text.trim().to_owned())));
        }
    }
    keys.extend(s.removed.drain(..).map(|id| (id, None)));
    keys
}

fn add_preset(s: &mut Settings, preset: &AiPreset) {
    let mut p = AiProvider::from_preset(preset);
    // Number duplicates: "OpenAI 2".
    let taken = |name: &str| s.providers.iter().any(|q| q.name == name);
    if taken(&p.name) {
        let base = p.name.clone();
        let mut n = 2;
        while taken(&format!("{base} {n}")) {
            n += 1;
        }
        p.name = format!("{base} {n}");
    }
    if s.active.is_none() {
        s.active = Some(p.id);
    }
    s.saved_keys.insert(p.id, false);
    s.providers.push(p);
    s.selected = Some(s.providers.len() - 1);
    s.show_key = false;
}

fn provider_editor(ui: &mut Ui, s: &mut Settings, i: usize) -> Option<AiAction> {
    let mut action = None;
    let mut remove = false;
    let id = s.providers[i].id;
    egui::Grid::new("ai-provider-form")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            let p = &mut s.providers[i];
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut p.name).desired_width(300.0));
            ui.end_row();
            ui.label("Protocol");
            egui::ComboBox::from_id_salt("ai-protocol")
                .width(300.0)
                .selected_text(p.protocol.label())
                .show_ui(ui, |ui| {
                    for proto in AiProtocol::ALL {
                        ui.selectable_value(&mut p.protocol, proto, proto.label());
                    }
                });
            ui.end_row();
            ui.label("Base URL");
            ui.add(
                egui::TextEdit::singleline(&mut p.base_url)
                    .desired_width(300.0)
                    .hint_text("https://api.example.com/v1"),
            );
            ui.end_row();
            ui.label("Model");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut p.model)
                        .desired_width(200.0)
                        .hint_text("model id"),
                );
                let loading = matches!(&s.models, Some((m, Some(_), _)) if *m == id);
                if ui
                    .add_enabled(!loading, egui::Button::new("Load Models"))
                    .on_hover_text("Ask the server which models it offers")
                    .clicked()
                {
                    let typed = s
                        .keys
                        .get(&id)
                        .filter(|k| !k.clear && !k.text.trim().is_empty())
                        .map(|k| k.text.trim().to_owned());
                    action = Some(AiAction::ListModels {
                        provider: p.clone(),
                        api_key: typed,
                    });
                }
                if loading {
                    ui.add(egui::Spinner::new().size(12.0));
                }
            });
            ui.end_row();
            if let Some((m, None, result)) = &s.models
                && *m == id
            {
                ui.label("");
                match result {
                    Ok(list) if list.is_empty() => {
                        ui.label(RichText::new("The server lists no models.").small());
                    }
                    Ok(list) => {
                        egui::ComboBox::from_id_salt("ai-models")
                            .width(300.0)
                            .height(320.0)
                            .selected_text(format!("Pick one of {} models", list.len()))
                            .show_ui(ui, |ui| {
                                for model in list {
                                    ui.selectable_value(&mut p.model, model.clone(), model);
                                }
                            });
                    }
                    Err(e) => {
                        ui.label(RichText::new(e).small().color(color::danger()));
                    }
                }
                ui.end_row();
            }
            ui.label("API key");
            ui.vertical(|ui| {
                ui.checkbox(&mut p.needs_key, "Send an API key")
                    .on_hover_text("Local servers usually need none");
                if p.needs_key {
                    let saved = s.saved_keys.get(&id).copied().unwrap_or(false);
                    let edit = s.keys.entry(id).or_default();
                    ui.horizontal(|ui| {
                        let hint = if saved && !edit.clear {
                            "saved in the keychain — type to replace"
                        } else {
                            "paste the key"
                        };
                        ui.add(
                            egui::TextEdit::singleline(&mut edit.text)
                                .password(!s.show_key)
                                .desired_width(230.0)
                                .hint_text(hint),
                        );
                        let eye = if s.show_key {
                            icon::EYE_SLASH
                        } else {
                            icon::EYE
                        };
                        if ui
                            .add(egui::Button::new(eye).frame(false))
                            .on_hover_text("Show or hide what you typed")
                            .clicked()
                        {
                            s.show_key = !s.show_key;
                        }
                    });
                    if saved {
                        ui.checkbox(&mut edit.clear, "Remove the saved key");
                    }
                }
            });
            ui.end_row();
            ui.label("Max tokens");
            ui.horizontal(|ui| {
                let mut limit = p.max_tokens.is_some();
                if ui.checkbox(&mut limit, "").changed() {
                    p.max_tokens = limit.then_some(4_096);
                }
                if let Some(max) = &mut p.max_tokens {
                    ui.add(egui::DragValue::new(max).range(64..=128_000).speed(64));
                } else {
                    ui.label(
                        RichText::new("server default")
                            .small()
                            .color(color::text_dim()),
                    );
                }
            });
            ui.end_row();
            if p.protocol == AiProtocol::OpenAiCompatible {
                ui.label("Temperature");
                ui.horizontal(|ui| {
                    let mut set = p.temperature.is_some();
                    if ui.checkbox(&mut set, "").changed() {
                        p.temperature = set.then_some(0.2);
                    }
                    if let Some(t) = &mut p.temperature {
                        ui.add(egui::Slider::new(t, 0.0..=2.0).step_by(0.05));
                    } else {
                        ui.label(
                            RichText::new("server default")
                                .small()
                                .color(color::text_dim()),
                        );
                    }
                });
                ui.end_row();
            }
        });
    if let Some(problem) = s.providers[i].problem() {
        ui.label(RichText::new(problem).small().color(color::warning()));
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let is_active = s.active == Some(id);
        if ui
            .add_enabled(!is_active, egui::Button::new("Use This Provider"))
            .clicked()
        {
            s.active = Some(id);
        }
        if ui
            .button(RichText::new(format!("{}  Remove", icon::TRASH)).color(color::danger()))
            .clicked()
        {
            remove = true;
        }
    });
    if remove {
        s.providers.remove(i);
        s.keys.remove(&id);
        if s.saved_keys.get(&id).copied().unwrap_or(false) {
            s.removed.push(id);
        }
        if s.active == Some(id) {
            s.active = s.providers.first().map(|p| p.id);
        }
        s.selected = (!s.providers.is_empty()).then(|| i.min(s.providers.len() - 1));
    }
    action
}

fn show_entry(
    ui: &mut Ui,
    index: usize,
    entry: &Entry,
    writing: bool,
    actions: &mut Vec<AiAction>,
) {
    match entry.role {
        AiRole::User => {
            Frame::new()
                .fill(color::surface())
                .corner_radius(6)
                .inner_margin(Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(&entry.text).color(color::text_strong()));
                });
        }
        AiRole::Assistant => {
            let segments = split_code_blocks(&entry.text);
            if segments.is_empty() && writing {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0));
                    ui.label(RichText::new("Thinking…").color(color::text_dim()));
                });
            }
            for (j, segment) in segments.iter().enumerate() {
                match segment {
                    Segment::Text(text) => show_prose(ui, text),
                    Segment::Code { lang, code, closed } => {
                        show_code(
                            ui,
                            Id::new(("ai-code", index, j)),
                            lang,
                            code,
                            *closed || !writing,
                            actions,
                        );
                    }
                }
            }
            if let Some(error) = &entry.error {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(icon::WARNING).color(color::danger()));
                    ui.label(RichText::new(error).color(color::danger()));
                });
            }
        }
    }
}

/// Prose with the lightest Markdown: headings and `**bold**` lines become strong text.
fn show_prose(ui: &mut Ui, text: &str) {
    for para in text.split("\n\n") {
        let para = para.trim_matches('\n');
        if para.is_empty() {
            continue;
        }
        let heading = para.trim_start_matches('#');
        if heading.len() != para.len() && !para.contains('\n') {
            ui.label(RichText::new(heading.trim()).strong());
        } else {
            ui.label(RichText::new(para.replace("**", "")).color(color::text()));
        }
    }
}

fn show_code(ui: &mut Ui, id: Id, lang: &str, code: &str, done: bool, actions: &mut Vec<AiAction>) {
    Frame::new()
        .fill(color::editor())
        .stroke(egui::Stroke::new(1.0, color::border()))
        .corner_radius(4)
        .inner_margin(Margin::same(6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let tag = if lang.is_empty() { "code" } else { lang };
                ui.label(RichText::new(tag).small().color(color::text_faint()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(done, |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let small = |text: String| egui::Button::new(text).small();
                        if ui
                            .add(small(format!("{} Copy", icon::COPY)))
                            .on_hover_text("Copy to the clipboard")
                            .clicked()
                        {
                            ui.ctx().copy_text(code.to_owned());
                        }
                        let mut insert = |mode| {
                            actions.push(AiAction::Insert {
                                sql: code.to_owned(),
                                mode,
                            });
                        };
                        if ui
                            .add(small(format!("{} New Tab", icon::PLUS_SQUARE)))
                            .on_hover_text("Open in a new query tab")
                            .clicked()
                        {
                            insert(InsertMode::NewTab);
                        }
                        if ui
                            .add(small(format!("{} Replace", icon::ARROWS_CLOCKWISE)))
                            .on_hover_text("Replace the whole editor text")
                            .clicked()
                        {
                            insert(InsertMode::ReplaceAll);
                        }
                        if ui
                            .add(small(format!("{} Insert", icon::ARROW_LINE_DOWN)))
                            .on_hover_text("Insert at the cursor (replaces the selection)")
                            .clicked()
                        {
                            insert(InsertMode::AtCursor);
                        }
                    });
                });
            });
            ui.push_id(id, |ui| {
                ui.add(
                    egui::Label::new(RichText::new(code).font(theme::mono()).color(code_color()))
                        .wrap(),
                );
            });
        });
}

fn code_color() -> Color32 {
    color::text_strong()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AiState {
        let p = AiProvider::from_preset(&PRESETS[0]);
        AiState::new(vec![p], None, true, true)
    }

    #[test]
    fn first_provider_is_active_and_unknown_active_is_ignored() {
        let s = state();
        assert_eq!(s.active, Some(s.providers[0].id));
        let s = AiState::new(s.providers.clone(), Some(Uuid::nil()), false, false);
        assert_eq!(s.active, Some(s.providers[0].id));
        assert!(
            AiState::new(Vec::new(), None, false, false)
                .provider()
                .is_none()
        );
    }

    #[test]
    fn conversation_alternates_and_streams() {
        let mut s = state();
        let m = s.begin(RequestId(1), "q1".into());
        assert_eq!(m, vec![AiMessage::user("q1")]);
        s.chunk(RequestId(1), "a");
        s.chunk(RequestId(99), "ignored");
        s.chunk(RequestId(1), "1");
        s.finished(RequestId(1), Ok(()));
        assert!(!s.is_busy());
        let m = s.begin(RequestId(2), "q2".into());
        assert_eq!(
            m,
            vec![
                AiMessage::user("q1"),
                AiMessage::assistant("a1"),
                AiMessage::user("q2")
            ]
        );
        // A failed answer without text is left out of the next request.
        s.finished(RequestId(2), Err("HTTP 500".into()));
        assert_eq!(
            s.chat.last().and_then(|e| e.error.as_deref()),
            Some("HTTP 500")
        );
        let m = s.begin(RequestId(3), "q3".into());
        assert_eq!(
            m,
            vec![
                AiMessage::user("q1"),
                AiMessage::assistant("a1"),
                AiMessage::user("q3")
            ]
        );
    }

    #[test]
    fn cancel_ignores_late_events() {
        let mut s = state();
        s.begin(RequestId(5), "q".into());
        s.chunk(RequestId(5), "part");
        assert_eq!(s.cancel_pending(), Some(RequestId(5)));
        s.chunk(RequestId(5), " late");
        s.finished(RequestId(5), Err("Cancelled.".into()));
        let last = s.chat.last().map(|e| (e.text.as_str(), e.error.as_deref()));
        assert_eq!(last, Some(("part", Some("Stopped."))));
    }

    #[test]
    fn history_is_capped() {
        let mut s = state();
        for i in 0..40 {
            s.begin(RequestId(i), format!("q{i}"));
            s.chunk(RequestId(i), "a");
            s.finished(RequestId(i), Ok(()));
        }
        let m = s.begin(RequestId(100), "last".into());
        assert!(m.len() <= HISTORY_TURNS + 1);
        assert_eq!(m.first().map(|m| m.role), Some(AiRole::User));
        assert_eq!(m.last(), Some(&AiMessage::user("last")));
    }

    #[test]
    fn settings_save_returns_key_changes() {
        let mut s = state();
        let first = s.providers[0].id;
        s.key_saved.insert(first, true);
        s.open_settings();
        let settings = s.settings.as_mut().unwrap_or_else(|| unreachable!());
        add_preset(settings, &PRESETS[0]);
        assert_eq!(settings.providers[1].name, "Anthropic 2");
        let second = settings.providers[1].id;
        settings.keys.entry(second).or_default().text = " sk-new ".into();
        settings.keys.entry(first).or_default().clear = true;
        let keys = key_changes(settings);
        assert!(keys.contains(&(second, Some("sk-new".into()))));
        assert!(keys.contains(&(first, None)));
    }
}
