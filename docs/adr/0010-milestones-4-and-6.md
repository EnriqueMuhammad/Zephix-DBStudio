# ADR 0010 — Milestones 4 and 6: design tools, data movement, more engines

Status: accepted (2026-10)

## Context

Milestone 4 adds design and analysis tools (table designer, import, explain, schema diff,
SQLite); Milestone 6 adds ER diagrams, data transfer/sync, a visual query builder, scheduled
backups and Oracle, MongoDB and Redis. All of them need a structured view of tables that the
object tree (`DbObject`) does not provide.

## Decisions

1. **One table model.** `rdbs_core::table::TableSchema` (columns, PK, unique constraints,
   indexes, FKs) is produced by every SQL adapter's `describe_schema` and consumed by the
   designer, diff, ER diagram, transfer, builder and backups. Expression/partial indexes are
   skipped rather than misrepresented.
2. **DDL is generated, not parsed.** `ddlgen::create_table` / `alter_table` emit dialect SQL
   from two `TableSchema`s plus explicit renames. SQLite changes that need it rebuild the table.
   A live round trip (describe → create elsewhere → describe) must be lossless on every engine.
3. **Types travel through classes.** `types::classify` / `map_type` map native types across
   engines; equivalence (`character varying` = `varchar`, `((0))` = `0`) prevents false diffs.
4. **Tool tabs.** Each tool is a `Tab::Tool` that owns its request ids and consumes the matching
   worker events; only targets (scopes, query models, diagram positions) are persisted.
5. **Background jobs use their own connections.** Transfer and backup open dedicated sessions
   through the factory (passwords from session or keychain), report `JobProgress`, finish with
   `JobFinished`, and can be cancelled. Interactive sessions are never blocked by a job.
6. **Writes are transactional where the engine allows.** Imports and per-page transfer writes use
   `apply_changes`; `Change::ANY` marks DDL whose row count is not checked. Sync deletions run
   last, referencing tables first. Explain Analyze runs inside a rolled-back transaction.
7. **Backups are logical SQL dumps** written to a temporary file and renamed; schedules live in
   local SQLite (schema v4) and run only while the application is open (documented limitation).
8. **Non-SQL engines keep the editor, not the SQL tools.** MongoDB accepts shell syntax and Redis
   `redis-cli` syntax; their results are grids. `DatabaseKind::is_sql()` hides designer, diff,
   transfer, builder and backups for them.
9. **Oracle uses rust-oracle (ODPI-C).** It builds without Oracle software but needs Instant
   Client at run time; the missing-client error (DPI-1047) explains how to install it.

## Consequences

- A new SQL engine needs `describe_schema` plus dialect cases in `ddlgen`/`types`; every tool then
  works for it.
- Oracle live tests are opt-in until Instant Client is available on the test machine.
- Scheduled backups do not run when the application is closed (a service is a Milestone 5 topic).
