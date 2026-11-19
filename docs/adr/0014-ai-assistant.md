# ADR 0014 — AI assistant with user-chosen providers

Status: accepted (2026-11)

## Context

Users want help writing, explaining and fixing queries without leaving the editor, using the
AI service they already pay for or a model running on their own machine. There is no Zephix
server, account or bundled model, and the app must keep working with no AI configured.

## Decision

- **Two wire protocols cover the providers.** *OpenAI-compatible* (`POST {base}/chat/completions`,
  bearer token) is spoken by OpenAI, Google Gemini's compatibility endpoint, OpenRouter, Groq,
  DeepSeek, gateways and local servers (Ollama, LM Studio, vLLM, llama.cpp). *Anthropic*
  (`POST {base}/messages`, `x-api-key`, `anthropic-version: 2026-10-21`) is the Messages API.
  A "Custom" provider is any base URL with either protocol. Presets only fill in the base URL
  (and a default model where one is stable); **Load Models** asks `GET {base}/models` for the
  rest, so the app does not hard-code model lists that go stale.
- **Requests run on the worker** (ADR 0003): `Command::AiChat` → `Event::AiChunk`* →
  `Event::AiFinished`, `Command::AiListModels` → `Event::AiModelsListed`. HTTP uses the
  `ureq` + rustls/ring client of the update check (ADR 0013) with OS roots, on Tokio's
  blocking pool. Answers stream as server-sent events; a server that ignores `"stream": true`
  and answers with one JSON document is accepted too. `Command::Cancel` stops a stream between
  two events. Building requests and parsing events are pure functions in `rdbs_core::ai`,
  unit-tested without a network.
- **Secrets:** API keys live in the OS keychain (`SecretKind::AiApiKey`, account
  `<provider-id>:ai`), never in `app.ron`. The worker reads the key itself; the UI only holds a
  key while it is typed in the settings dialog. Debug output of a request hides its headers.
  Providers (name, protocol, base URL, model, limits) are stored with the UI preferences.
- **Context is explicit and minimal.** Each question carries a system prompt with the engine
  and server version, the database, the editor text or its selection, and the error of the
  last run of the focused query tab. Table and column names from the explorer tree are added
  when "Send table and column names" is ticked (on by default; untick it per need). Result
  rows are never sent. The panel shows a one-line summary of what will be sent. Production
  connections add an instruction to prefer read-only queries and spell out the effect of writes.
- **Nothing runs by itself.** Code blocks in answers have Insert (at the cursor / replacing
  the selection), Replace, New Tab and Copy. Running is the user's normal Run, so the
  production confirmation (ADR 0006) still applies.
- **UI:** a resizable panel on the right (View > AI Assistant, toolbar **AI**, Ctrl+Shift+I),
  quick requests (Explain, Fix Error, Optimize, Format & Comment) in the panel and in
  Query > Ask AI, and a settings dialog for providers and keys. The conversation lives for the
  session only; the last 20 turns are sent as history.

## Consequences

- Questions go straight from the user's machine to the provider they configured; Zephix sees
  nothing. Hosted providers may log what they receive, which is why the context is listed in
  the panel and the schema can be left out.
- No tool calling: the model cannot query the database. Adding it later means a separate,
  read-only, confirmed path, not the assistant running SQL.
- New providers that speak one of the two protocols need no code change.
