//! AI assistant (ADR 0014): provider settings, HTTP request bodies, streamed answers, the
//! system prompt built from the editor and the explorer, and code blocks in replies.
//!
//! Pure functions only. The worker (`rdbs-services`) sends the requests built here and feeds
//! the server-sent events back through [`parse_stream_data`]; the UI renders the result.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::profile::ProfileId;
use crate::schema::{DbObject, ObjectKind};

/// Anthropic Messages API version sent with every request.
pub const ANTHROPIC_VERSION: &str = "2026-10-21";
/// `max_tokens` for the Anthropic protocol when a provider sets none (it is required there).
pub const DEFAULT_ANTHROPIC_MAX_TOKENS: u32 = 16_000;
/// The editor text sent as context is cut after this many bytes.
pub const MAX_SQL_CONTEXT: usize = 24_000;
/// The schema outline sent as context is cut after this many bytes.
pub const MAX_SCHEMA_CONTEXT: usize = 16_000;

/// Wire format of a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum AiProtocol {
    /// `POST {base}/chat/completions` with a bearer token. OpenAI and most other hosted
    /// services, gateways and local servers (Ollama, LM Studio, vLLM, llama.cpp) speak it.
    #[default]
    OpenAiCompatible,
    /// `POST {base}/messages` with `x-api-key` (Anthropic Messages API).
    Anthropic,
}

impl AiProtocol {
    /// Both protocols, for a picker.
    pub const ALL: [AiProtocol; 2] = [AiProtocol::OpenAiCompatible, AiProtocol::Anthropic];

    /// Name shown in the settings dialog.
    pub fn label(self) -> &'static str {
        match self {
            AiProtocol::OpenAiCompatible => "OpenAI-compatible (chat/completions)",
            AiProtocol::Anthropic => "Anthropic (messages)",
        }
    }
}

/// A ready-made provider the settings dialog offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiPreset {
    /// Name of the provider.
    pub name: &'static str,
    /// Wire format.
    pub protocol: AiProtocol,
    /// API base URL (the endpoint path is appended).
    pub base_url: &'static str,
    /// Suggested model; empty when the user picks one (Load Models lists them).
    pub model: &'static str,
    /// Whether the service needs an API key.
    pub needs_key: bool,
}

/// Providers offered by "Add provider". Anything else is "Custom" with its own base URL.
pub const PRESETS: &[AiPreset] = &[
    AiPreset {
        name: "Anthropic",
        protocol: AiProtocol::Anthropic,
        base_url: "https://api.anthropic.com/v1",
        model: "claude-opus-5-5",
        needs_key: true,
    },
    AiPreset {
        name: "OpenAI",
        protocol: AiProtocol::OpenAiCompatible,
        base_url: "https://api.openai.com/v1",
        model: "",
        needs_key: true,
    },
    AiPreset {
        name: "Google Gemini",
        protocol: AiProtocol::OpenAiCompatible,
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        model: "",
        needs_key: true,
    },
    AiPreset {
        name: "OpenRouter",
        protocol: AiProtocol::OpenAiCompatible,
        base_url: "https://openrouter.ai/api/v1",
        model: "",
        needs_key: true,
    },
    AiPreset {
        name: "Groq",
        protocol: AiProtocol::OpenAiCompatible,
        base_url: "https://api.groq.com/openai/v1",
        model: "",
        needs_key: true,
    },
    AiPreset {
        name: "DeepSeek",
        protocol: AiProtocol::OpenAiCompatible,
        base_url: "https://api.deepseek.com/v1",
        model: "",
        needs_key: true,
    },
    AiPreset {
        name: "Ollama (local)",
        protocol: AiProtocol::OpenAiCompatible,
        base_url: "http://localhost:11434/v1",
        model: "",
        needs_key: false,
    },
    AiPreset {
        name: "LM Studio (local)",
        protocol: AiProtocol::OpenAiCompatible,
        base_url: "http://localhost:1234/v1",
        model: "",
        needs_key: false,
    },
    AiPreset {
        name: "Custom",
        protocol: AiProtocol::OpenAiCompatible,
        base_url: "",
        model: "",
        needs_key: true,
    },
];

/// A configured AI provider. Stored with the UI preferences; the API key lives in the OS
/// keychain under [`AiProvider::keychain_id`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiProvider {
    /// Stable identifier (also the keychain account).
    pub id: Uuid,
    /// Name shown in the assistant panel.
    pub name: String,
    /// Wire format.
    pub protocol: AiProtocol,
    /// API base URL, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    /// Model id sent with every request.
    pub model: String,
    /// Whether an API key is sent (local servers usually need none).
    pub needs_key: bool,
    /// Output token limit. `None` lets an OpenAI-compatible server decide; the Anthropic
    /// protocol uses [`DEFAULT_ANTHROPIC_MAX_TOKENS`].
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Sampling temperature for OpenAI-compatible servers. Not sent to the Anthropic
    /// protocol, whose current models reject it.
    #[serde(default)]
    pub temperature: Option<f32>,
}

impl AiProvider {
    /// A new provider from a preset.
    pub fn from_preset(preset: &AiPreset) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: preset.name.to_owned(),
            protocol: preset.protocol,
            base_url: preset.base_url.to_owned(),
            model: preset.model.to_owned(),
            needs_key: preset.needs_key,
            max_tokens: None,
            temperature: None,
        }
    }

    /// Keychain slot of the API key (a profile id that no connection uses).
    pub fn keychain_id(&self) -> ProfileId {
        ProfileId(self.id)
    }

    /// Why the provider cannot be used yet, if it cannot.
    pub fn problem(&self) -> Option<&'static str> {
        let base = self.base_url.trim();
        if base.is_empty() {
            Some("Enter the API base URL.")
        } else if !(base.starts_with("https://") || base.starts_with("http://")) {
            Some("The base URL must start with https:// or http://.")
        } else if self.model.trim().is_empty() {
            Some("Choose a model (Load Models lists what the server offers).")
        } else {
            None
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{path}", self.base_url.trim().trim_end_matches('/'))
    }
}

/// Who wrote a chat message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiRole {
    /// The user.
    User,
    /// The model.
    Assistant,
}

/// One turn of the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiMessage {
    /// Author.
    pub role: AiRole,
    /// Plain text (Markdown from the model).
    pub content: String,
}

impl AiMessage {
    /// A user turn.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: AiRole::User,
            content: content.into(),
        }
    }

    /// A model turn.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: AiRole::Assistant,
            content: content.into(),
        }
    }
}

/// An HTTP request to send. `body` is empty for a GET.
#[derive(Clone, PartialEq)]
pub struct HttpRequest {
    /// Full URL.
    pub url: String,
    /// Header names and values (may contain the API key).
    pub headers: Vec<(&'static str, String)>,
    /// JSON body; empty for GET.
    pub body: String,
}

impl std::fmt::Debug for HttpRequest {
    // Headers carry the API key: never print them.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("url", &self.url)
            .field("headers", &self.headers.len())
            .finish_non_exhaustive()
    }
}

fn auth_headers(provider: &AiProvider, api_key: Option<&str>) -> Vec<(&'static str, String)> {
    let key = api_key.map(str::trim).filter(|k| !k.is_empty());
    let mut headers = Vec::new();
    match provider.protocol {
        AiProtocol::OpenAiCompatible => {
            if let Some(key) = key {
                headers.push(("authorization", format!("Bearer {key}")));
            }
        }
        AiProtocol::Anthropic => {
            if let Some(key) = key {
                headers.push(("x-api-key", key.to_owned()));
            }
            headers.push(("anthropic-version", ANTHROPIC_VERSION.to_owned()));
        }
    }
    headers
}

fn role_name(role: AiRole) -> &'static str {
    match role {
        AiRole::User => "user",
        AiRole::Assistant => "assistant",
    }
}

/// The streaming chat request for `messages` with `system` as the instructions.
pub fn chat_request(
    provider: &AiProvider,
    api_key: Option<&str>,
    system: &str,
    messages: &[AiMessage],
) -> HttpRequest {
    let turns = messages
        .iter()
        .map(|m| json!({ "role": role_name(m.role), "content": m.content }));
    let body = match provider.protocol {
        AiProtocol::OpenAiCompatible => {
            let mut all = vec![json!({ "role": "system", "content": system })];
            all.extend(turns);
            let mut body = json!({
                "model": provider.model.trim(),
                "messages": all,
                "stream": true,
            });
            if let Some(max) = provider.max_tokens {
                body["max_tokens"] = json!(max);
            }
            if let Some(t) = provider.temperature {
                body["temperature"] = json!(t);
            }
            body
        }
        AiProtocol::Anthropic => json!({
            "model": provider.model.trim(),
            "system": system,
            "messages": turns.collect::<Vec<_>>(),
            "max_tokens": provider.max_tokens.unwrap_or(DEFAULT_ANTHROPIC_MAX_TOKENS),
            "stream": true,
        }),
    };
    let mut headers = auth_headers(provider, api_key);
    headers.push(("content-type", "application/json".to_owned()));
    headers.push(("accept", "text/event-stream".to_owned()));
    let path = match provider.protocol {
        AiProtocol::OpenAiCompatible => "chat/completions",
        AiProtocol::Anthropic => "messages",
    };
    HttpRequest {
        url: provider.endpoint(path),
        headers,
        body: body.to_string(),
    }
}

/// `GET {base}/models`, which both protocols (and most local servers) answer.
pub fn models_request(provider: &AiProvider, api_key: Option<&str>) -> HttpRequest {
    HttpRequest {
        url: provider.endpoint("models"),
        headers: auth_headers(provider, api_key),
        body: String::new(),
    }
}

/// Model ids from a `/models` answer (`{"data": [{"id": …}]}`, or Ollama's `{"models": …}`),
/// sorted.
pub fn parse_models(body: &str) -> Result<Vec<String>, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|e| format!("The model list is not JSON: {e}"))?;
    let list = value
        .get("data")
        .or_else(|| value.get("models"))
        .and_then(Value::as_array)
        .ok_or_else(|| "The answer has no model list.".to_owned())?;
    let mut ids: Vec<String> = list
        .iter()
        .filter_map(|m| {
            m.get("id")
                .or_else(|| m.get("name"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// One `data:` payload of a streamed answer.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamPiece {
    /// More answer text.
    Text(String),
    /// The answer is complete.
    Done,
    /// The server reported an error mid-stream.
    Error(String),
    /// Nothing to show (pings, thinking, usage, role headers).
    Skip,
}

/// Interprets the payload of one `data:` line of a server-sent event stream.
pub fn parse_stream_data(protocol: AiProtocol, data: &str) -> StreamPiece {
    let data = data.trim();
    if data.is_empty() {
        return StreamPiece::Skip;
    }
    if data == "[DONE]" {
        return StreamPiece::Done;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return StreamPiece::Skip;
    };
    if let Some(message) = error_text(&value) {
        return StreamPiece::Error(message);
    }
    match protocol {
        AiProtocol::OpenAiCompatible => {
            let text = value
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if text.is_empty() {
                StreamPiece::Skip
            } else {
                StreamPiece::Text(text.to_owned())
            }
        }
        AiProtocol::Anthropic => match value.get("type").and_then(Value::as_str) {
            Some("content_block_delta") => {
                match value.pointer("/delta/text").and_then(Value::as_str) {
                    Some(text) if !text.is_empty() => StreamPiece::Text(text.to_owned()),
                    _ => StreamPiece::Skip,
                }
            }
            Some("message_stop") => StreamPiece::Done,
            _ => StreamPiece::Skip,
        },
    }
}

/// The answer text of a complete (not streamed) JSON response. Some servers ignore
/// `"stream": true` and answer with one JSON document.
pub fn parse_complete(protocol: AiProtocol, body: &str) -> Result<String, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|e| format!("The answer is not JSON: {e}"))?;
    if let Some(message) = error_text(&value) {
        return Err(message);
    }
    let text = match protocol {
        AiProtocol::OpenAiCompatible => value
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_owned),
        AiProtocol::Anthropic => value
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<String>()
            }),
    };
    text.ok_or_else(|| "The answer has no text.".to_owned())
}

/// `error.message` (or a string `error`) of an error document.
fn error_text(value: &Value) -> Option<String> {
    let error = value.get("error")?;
    error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .map(str::to_owned)
}

/// One sentence for an HTTP error status and its body.
pub fn describe_http_error(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| error_text(&v));
    let hint = match status {
        401 | 403 => " Check the API key.",
        404 => " Check the base URL and the model.",
        429 => " The rate limit or quota is used up; try again later.",
        _ => "",
    };
    match detail {
        Some(detail) => format!("The provider answered HTTP {status}: {detail}{hint}"),
        None => format!("The provider answered HTTP {status}.{hint}"),
    }
}

/// What the assistant knows about the user's situation. Every field is optional.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AiContext {
    /// Database engine, e.g. "PostgreSQL".
    pub engine: Option<String>,
    /// Server version string reported on connect.
    pub server_version: Option<String>,
    /// Database the editor runs in.
    pub database: Option<String>,
    /// The connection is marked as production.
    pub production: bool,
    /// Outline of tables and columns ([`schema_outline`]).
    pub schema: Option<String>,
    /// Editor text (or the selection).
    pub sql: Option<String>,
    /// Whether [`Self::sql`] is a selection rather than the whole editor.
    pub sql_is_selection: bool,
    /// Error of the last run in that editor.
    pub last_error: Option<String>,
}

/// Cuts `text` to at most `max` bytes on a character boundary, marking the cut.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (cut, {} more bytes)", &text[..end], text.len() - end)
}

/// Instructions for the model, with the context appended.
pub fn system_prompt(cx: &AiContext) -> String {
    let mut s = String::from(
        "You are the assistant built into a database IDE. You help the user write, explain, \
         fix and optimise SQL and database commands, and understand errors and schemas.\n\
         - Answer in the language the user writes in. Be concise.\n\
         - Put every statement the user may run in a fenced code block tagged with its \
         language (```sql, or ```javascript for MongoDB shell, ```redis for redis-cli). \
         The IDE offers buttons to insert those blocks into the editor; it never runs them \
         by itself.\n\
         - Use the SQL dialect of the engine below and only tables and columns that exist in \
         the schema when one is given. Say so when you have to assume a name.\n\
         - Warn clearly before any statement that deletes, overwrites or locks data, and \
         prefer a transaction or a WHERE clause the user can check first.\n",
    );
    if cx.production {
        s.push_str(
            "- This connection is marked PRODUCTION. Prefer read-only queries; for any write, \
             spell out its effect and suggest testing it elsewhere first.\n",
        );
    }
    s.push_str("\n# Context\n");
    match (&cx.engine, &cx.server_version) {
        (Some(engine), Some(version)) => s.push_str(&format!("Engine: {engine} ({version})\n")),
        (Some(engine), None) => s.push_str(&format!("Engine: {engine}\n")),
        _ => s.push_str("Engine: unknown (no connection selected)\n"),
    }
    if let Some(db) = &cx.database {
        s.push_str(&format!("Database: {db}\n"));
    }
    if let Some(schema) = cx.schema.as_deref().filter(|t| !t.trim().is_empty()) {
        s.push_str("\n## Schema (tables and columns)\n");
        s.push_str(&clip(schema, MAX_SCHEMA_CONTEXT));
        s.push('\n');
    }
    if let Some(sql) = cx.sql.as_deref().filter(|t| !t.trim().is_empty()) {
        s.push_str(if cx.sql_is_selection {
            "\n## Selected text in the editor\n```\n"
        } else {
            "\n## Editor text\n```\n"
        });
        s.push_str(&clip(sql, MAX_SQL_CONTEXT));
        s.push_str("\n```\n");
    }
    if let Some(error) = cx.last_error.as_deref().filter(|t| !t.trim().is_empty()) {
        s.push_str("\n## Error of the last run\n");
        s.push_str(&clip(error, 4_000));
        s.push('\n');
    }
    s
}

/// `schema.table(column type, …)` lines for the tables and views of an explorer tree.
///
/// With `database`, only that database's subtree is described (if the tree has it). Stops
/// after about `max` bytes and says how many objects were left out.
pub fn schema_outline(objects: &[DbObject], database: Option<&str>, max: usize) -> String {
    fn columns(obj: &DbObject, out: &mut Vec<String>) {
        for child in &obj.children {
            match child.kind {
                ObjectKind::Column => out.push(match &child.detail {
                    Some(t) if !t.is_empty() => format!("{} {t}", child.name),
                    _ => child.name.clone(),
                }),
                ObjectKind::Folder => columns(child, out),
                _ => {}
            }
        }
    }
    fn walk(obj: &DbObject, schema: Option<&str>, lines: &mut Vec<String>) {
        match obj.kind {
            ObjectKind::Schema => {
                for c in &obj.children {
                    walk(c, Some(&obj.name), lines);
                }
            }
            ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView => {
                let name = match (obj.schema.as_deref().or(schema), &obj.schema) {
                    (_, Some(_)) => obj.display_name(),
                    (Some(s), None) => format!("{s}.{}", obj.name),
                    (None, None) => obj.name.clone(),
                };
                let tag = if obj.kind == ObjectKind::Table {
                    ""
                } else {
                    " [view]"
                };
                let mut cols = Vec::new();
                columns(obj, &mut cols);
                lines.push(format!("{name}{tag}({})", cols.join(", ")));
            }
            ObjectKind::Database | ObjectKind::Folder => {
                for c in &obj.children {
                    walk(c, schema, lines);
                }
            }
            _ => {}
        }
    }
    let scoped: Vec<&DbObject> = match database {
        Some(db) => {
            let hit: Vec<&DbObject> = objects
                .iter()
                .filter(|o| o.kind == ObjectKind::Database && o.name == db)
                .collect();
            if hit.is_empty() {
                objects.iter().collect()
            } else {
                hit
            }
        }
        None => objects.iter().collect(),
    };
    let mut lines = Vec::new();
    for obj in scoped {
        walk(obj, None, &mut lines);
    }
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if out.len() + line.len() + 1 > max {
            out.push_str(&format!("… ({} more objects)\n", lines.len() - i));
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Ready-made requests offered as buttons and menu items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickAsk {
    /// Explain what the editor text does.
    Explain,
    /// Fix the error of the last run.
    FixError,
    /// Suggest a faster version.
    Optimize,
    /// Add comments and format.
    Document,
}

impl QuickAsk {
    /// Every quick request, in menu order.
    pub const ALL: [QuickAsk; 4] = [
        QuickAsk::Explain,
        QuickAsk::FixError,
        QuickAsk::Optimize,
        QuickAsk::Document,
    ];

    /// Button label.
    pub fn label(self) -> &'static str {
        match self {
            QuickAsk::Explain => "Explain",
            QuickAsk::FixError => "Fix Error",
            QuickAsk::Optimize => "Optimize",
            QuickAsk::Document => "Format & Comment",
        }
    }

    /// The user message sent for it.
    pub fn prompt(self) -> &'static str {
        match self {
            QuickAsk::Explain => {
                "Explain what the SQL in the editor does, step by step, and point out anything \
                 surprising or risky."
            }
            QuickAsk::FixError => {
                "The last run of the editor text failed with the error in the context. Explain \
                 the cause briefly and give the corrected statement."
            }
            QuickAsk::Optimize => {
                "Review the SQL in the editor for performance. Suggest a faster equivalent and \
                 any indexes worth adding, and say what each change gains."
            }
            QuickAsk::Document => {
                "Reformat the SQL in the editor for readability and add short comments. Do not \
                 change what it does. Return the whole result in one code block."
            }
        }
    }

    /// Whether it needs an error from the last run.
    pub fn needs_error(self) -> bool {
        self == QuickAsk::FixError
    }
}

/// A piece of a Markdown reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Prose.
    Text(String),
    /// A fenced code block. `closed` is false while the block is still streaming.
    Code {
        /// Info string after the opening fence (`sql`, …), lower-case.
        lang: String,
        /// The code, without the fences.
        code: String,
        /// The closing fence has arrived.
        closed: bool,
    },
}

/// Splits a reply into prose and fenced code blocks (```` ``` ```` or `~~~`).
pub fn split_code_blocks(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut prose = String::new();
    let mut code: Option<(String, &str, String)> = None; // (lang, fence, body)
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        match &mut code {
            None => {
                let fence = if trimmed.starts_with("```") {
                    Some("```")
                } else if trimmed.starts_with("~~~") {
                    Some("~~~")
                } else {
                    None
                };
                if let Some(fence) = fence {
                    if !prose.trim().is_empty() {
                        out.push(Segment::Text(prose.trim_end().to_owned()));
                    }
                    prose.clear();
                    let lang = trimmed[3..].trim().to_lowercase();
                    code = Some((lang, fence, String::new()));
                } else {
                    prose.push_str(line);
                }
            }
            Some((lang, fence, body)) => {
                if trimmed.trim_end() == *fence {
                    out.push(Segment::Code {
                        lang: std::mem::take(lang),
                        code: body.trim_end_matches(['\n', '\r']).to_owned(),
                        closed: true,
                    });
                    code = None;
                } else {
                    body.push_str(line);
                }
            }
        }
    }
    if let Some((lang, _, body)) = code {
        out.push(Segment::Code {
            lang,
            code: body.trim_end_matches(['\n', '\r']).to_owned(),
            closed: false,
        });
    } else if !prose.trim().is_empty() {
        out.push(Segment::Text(prose.trim_end().to_owned()));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn provider(protocol: AiProtocol) -> AiProvider {
        AiProvider {
            id: Uuid::nil(),
            name: "t".into(),
            protocol,
            base_url: "https://example.test/v1/".into(),
            model: " m-1 ".into(),
            needs_key: true,
            max_tokens: None,
            temperature: Some(0.2),
        }
    }

    fn header<'a>(r: &'a HttpRequest, name: &str) -> Option<&'a str> {
        r.headers
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn openai_request_has_system_turn_and_bearer_key() {
        let p = provider(AiProtocol::OpenAiCompatible);
        let r = chat_request(&p, Some(" sk-1 "), "SYS", &[AiMessage::user("hi")]);
        assert_eq!(r.url, "https://example.test/v1/chat/completions");
        assert_eq!(header(&r, "authorization"), Some("Bearer sk-1"));
        let body: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(body["model"], "m-1");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hi");
        assert!(body.get("max_tokens").is_none());
        assert!((body["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
        assert!(!format!("{r:?}").contains("sk-1"), "Debug hides the key");
    }

    #[test]
    fn local_provider_sends_no_authorization() {
        let p = provider(AiProtocol::OpenAiCompatible);
        let r = chat_request(&p, None, "S", &[]);
        assert_eq!(header(&r, "authorization"), None);
        let r = chat_request(&p, Some("  "), "S", &[]);
        assert_eq!(header(&r, "authorization"), None);
    }

    #[test]
    fn anthropic_request_uses_system_field_and_version_header() {
        let p = provider(AiProtocol::Anthropic);
        let r = chat_request(
            &p,
            Some("k"),
            "SYS",
            &[AiMessage::user("a"), AiMessage::assistant("b")],
        );
        assert_eq!(r.url, "https://example.test/v1/messages");
        assert_eq!(header(&r, "x-api-key"), Some("k"));
        assert_eq!(header(&r, "anthropic-version"), Some(ANTHROPIC_VERSION));
        let body: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(body["system"], "SYS");
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["max_tokens"], DEFAULT_ANTHROPIC_MAX_TOKENS);
        assert!(
            body.get("temperature").is_none(),
            "rejected by current models"
        );
    }

    #[test]
    fn stream_lines_of_both_protocols() {
        let o = AiProtocol::OpenAiCompatible;
        assert_eq!(
            parse_stream_data(o, r#"{"choices":[{"delta":{"content":"SEL"}}]}"#),
            StreamPiece::Text("SEL".into())
        );
        assert_eq!(
            parse_stream_data(o, r#"{"choices":[{"delta":{"role":"assistant"}}]}"#),
            StreamPiece::Skip
        );
        assert_eq!(parse_stream_data(o, " [DONE] "), StreamPiece::Done);
        assert_eq!(
            parse_stream_data(o, r#"{"error":{"message":"bad model"}}"#),
            StreamPiece::Error("bad model".into())
        );
        let a = AiProtocol::Anthropic;
        assert_eq!(
            parse_stream_data(
                a,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#
            ),
            StreamPiece::Text("Hi".into())
        );
        assert_eq!(
            parse_stream_data(
                a,
                r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"x"}}"#
            ),
            StreamPiece::Skip
        );
        assert_eq!(
            parse_stream_data(a, r#"{"type":"ping"}"#),
            StreamPiece::Skip
        );
        assert_eq!(
            parse_stream_data(a, r#"{"type":"message_stop"}"#),
            StreamPiece::Done
        );
        assert_eq!(
            parse_stream_data(
                a,
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
            ),
            StreamPiece::Error("Overloaded".into())
        );
    }

    #[test]
    fn complete_answers_and_errors() {
        assert_eq!(
            parse_complete(
                AiProtocol::OpenAiCompatible,
                r#"{"choices":[{"message":{"content":"ok"}}]}"#
            ),
            Ok("ok".into())
        );
        assert_eq!(
            parse_complete(
                AiProtocol::Anthropic,
                r#"{"content":[{"type":"thinking","thinking":""},{"type":"text","text":"a"},{"type":"text","text":"b"}]}"#
            ),
            Ok("ab".into())
        );
        let e = describe_http_error(401, r#"{"error":{"message":"invalid x-api-key"}}"#);
        assert!(e.contains("401") && e.contains("invalid x-api-key") && e.contains("API key"));
        assert_eq!(
            describe_http_error(500, "oops"),
            "The provider answered HTTP 500."
        );
    }

    #[test]
    fn model_lists() {
        let ids =
            parse_models(r#"{"object":"list","data":[{"id":"b"},{"id":"a"},{"id":"a"}]}"#).unwrap();
        assert_eq!(ids, ["a", "b"]);
        let ids = parse_models(r#"{"models":[{"name":"llama"}]}"#).unwrap();
        assert_eq!(ids, ["llama"]);
        assert!(parse_models("{}").is_err());
    }

    #[test]
    fn provider_problems() {
        let mut p = AiProvider::from_preset(&PRESETS[0]);
        assert_eq!(p.problem(), None);
        p.model.clear();
        assert!(p.problem().is_some());
        p.model = "m".into();
        p.base_url = "ftp://x".into();
        assert!(p.problem().is_some());
        assert_ne!(
            AiProvider::from_preset(&PRESETS[0]).id,
            AiProvider::from_preset(&PRESETS[0]).id
        );
    }

    #[test]
    fn prompt_carries_context() {
        let cx = AiContext {
            engine: Some("PostgreSQL".into()),
            server_version: Some("17.2".into()),
            database: Some("shop".into()),
            production: true,
            schema: Some("public.users(id int4)\n".into()),
            sql: Some("select * from users".into()),
            sql_is_selection: true,
            last_error: Some("relation \"user\" does not exist".into()),
        };
        let s = system_prompt(&cx);
        for part in [
            "PostgreSQL (17.2)",
            "Database: shop",
            "PRODUCTION",
            "public.users(id int4)",
            "Selected text",
            "select * from users",
            "does not exist",
        ] {
            assert!(s.contains(part), "missing {part:?}");
        }
        let bare = system_prompt(&AiContext::default());
        assert!(bare.contains("no connection selected") && !bare.contains("PRODUCTION"));
    }

    #[test]
    fn clip_respects_char_boundaries() {
        let s = "ééééé"; // 10 bytes
        let c = clip(s, 3);
        assert!(c.starts_with('é') && c.contains("cut, 8 more bytes"));
        assert_eq!(clip("abc", 3), "abc");
    }

    #[test]
    fn outline_lists_tables_with_columns() {
        let col = |n: &str, t: &str| DbObject::new(n, ObjectKind::Column).with_detail(t);
        let users = DbObject::new("users", ObjectKind::Table).with_children(vec![
            DbObject::new("Columns", ObjectKind::Folder)
                .with_children(vec![col("id", "int4"), col("email", "text")]),
            DbObject::new("users_pkey", ObjectKind::Index),
        ]);
        let v = DbObject::new("active_users", ObjectKind::View);
        let tree = vec![
            DbObject::new("shop", ObjectKind::Database).with_children(vec![
                DbObject::new("public", ObjectKind::Schema).with_children(vec![
                    DbObject::new("Tables", ObjectKind::Folder).with_children(vec![users]),
                    DbObject::new("Views", ObjectKind::Folder).with_children(vec![v]),
                ]),
            ]),
            DbObject::new("other", ObjectKind::Database)
                .with_children(vec![DbObject::new("t", ObjectKind::Table)]),
        ];
        let out = schema_outline(&tree, Some("shop"), 10_000);
        assert_eq!(
            out,
            "public.users(id int4, email text)\npublic.active_users [view]()\n"
        );
        let all = schema_outline(&tree, None, 10_000);
        assert!(all.ends_with("t()\n"));
        let short = schema_outline(&tree, None, 40);
        assert!(short.contains("more objects"), "{short}");
    }

    #[test]
    fn code_blocks_split_and_stream() {
        let text = "Try this:\n```SQL\nselect 1;\n```\nThen\n~~~\nx\n~~~\n";
        assert_eq!(
            split_code_blocks(text),
            vec![
                Segment::Text("Try this:".into()),
                Segment::Code {
                    lang: "sql".into(),
                    code: "select 1;".into(),
                    closed: true
                },
                Segment::Text("Then".into()),
                Segment::Code {
                    lang: String::new(),
                    code: "x".into(),
                    closed: true
                },
            ]
        );
        let partial = split_code_blocks("a\n```sql\nselect");
        assert_eq!(
            partial[1],
            Segment::Code {
                lang: "sql".into(),
                code: "select".into(),
                closed: false
            }
        );
        assert_eq!(split_code_blocks(""), vec![]);
    }
}
