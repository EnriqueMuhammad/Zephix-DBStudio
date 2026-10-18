# ADR 0003 — UI ↔ worker communication via typed commands and events

Status: Accepted · 2026-10-16

## Decision
- The UI sends `rdbs_core::Command` values over an unbounded Tokio channel to a worker thread
  that owns a multi-threaded Tokio runtime.
- The worker replies with `rdbs_core::Event` values over a `std::sync::mpsc` channel and
  calls a repaint callback (`egui::Context::request_repaint`) after each event.
- The UI drains events at the start of every frame (`try_recv`, never blocking).
- Each query has a `RequestId`; cancellation is a `Command::CancelQuery(RequestId)` that flips
  a `CancelHandle`. Real adapters map it to a protocol-level cancel in M2
  (`tokio-postgres` cancel token, MySQL `KILL QUERY`, TDS attention / connection drop).

## Consequences
- The UI thread can never block on I/O by construction.
- All state mutations from I/O happen in one place (`StudioApp::drain_events`), which keeps the
  egui code free of locks.
