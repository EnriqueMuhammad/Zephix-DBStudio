# ADR 0002 — Cargo workspace and crate boundaries

Status: Accepted · 2026-10-16

## Decision
```
apps/zephix             binary: logging + eframe bootstrap only
crates/rdbs-core        domain types, SQL lexer, commands/events, adapter traits (no I/O deps)
crates/rdbs-storage     local SQLite store (connection profiles, later history)
crates/rdbs-services    background worker: owns Tokio runtime + live sessions
crates/rdbs-adapter-*   one crate per engine (mock in M1; postgres/mysql/mssql in M2)
crates/rdbs-ui          egui views; talks to services only via typed commands/events
```
Dependency direction: `app → ui → {services, storage, core}`, `services → core`,
`adapter-* → core`. Adapters never depend on UI; UI never depends on a concrete driver crate
except through the factory it hands to the worker.

## Consequences
- Driver crates (and their compile time / native deps) are isolated and can be feature-gated.
- `rdbs-core` stays dependency-light, so it is fast to test.
