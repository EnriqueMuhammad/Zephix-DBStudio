# Milestones, acceptance criteria and testing strategy

## Milestone 1 — Application shell (mock data only)

| # | Deliverable | Acceptance criteria | Verified by |
|---|---|---|---|
| 1 | Native startup | `cargo run -p zephix` opens a window titled "Zephix - Database Management Studio" on Windows/macOS/Linux. | Manual launch |
| 2 | Main shell | Menu bar, toolbar, explorer, dock workspace and status bar are visible. | Manual |
| 3 | Dark theme & typography | Custom palette applied; monospace font in editor/grid; Phosphor icons render. | Manual |
| 4 | Explorer (mock) | Seeded profiles appear; expanding one connects through the worker and shows a database/schema/table tree; filter box narrows the tree; right-click menus work. | Manual + `rdbs-services` tests |
| 5 | Resizable sidebar | Drag edge resizes; Ctrl+B toggles; width persists. | Manual |
| 6 | Dockable tabs | Tabs open (Ctrl+T), close (Ctrl+W / ×), drag to split and undock. | Manual |
| 7 | SQL editor | Highlighting for keywords/strings/numbers/comments; Ctrl+Enter runs selection or whole text; Stop cancels. | Lexer unit tests + manual |
| 8 | Result grid | `SELECT ... LIMIT 100000` scrolls smoothly; NULLs styled; types shown; cell copy works. | Manual + mock adapter tests |
| 9 | Connection dialog | Engine picker sets default port; validation messages; Test connection reports result; Save persists to SQLite. | `rdbs-core` + `rdbs-storage` tests + manual |
| 10 | Persistence | Restart restores window geometry, sidebar width, tabs and SQL text. | Manual |
| — | Quality gate | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` pass. | CI / local |

## Milestone 2 — Live connectivity

| # | Deliverable | Acceptance criteria | Verified by |
|---|---|---|---|
| 1 | PostgreSQL adapter | Connect (TLS modes), typed results, SQLSTATE errors, cancel via cancel request, object tree of current DB. | Unit tests + `tests/live.rs` (RDBS_IT) |
| 2 | MySQL/MariaDB adapter | Connect, multi-statement scripts incl. compound statements, errno errors, `KILL QUERY` cancel, object tree. | Unit tests + `tests/live.rs` |
| 3 | SQL Server adapter | Connect, `GO` batches, row counts for plain DML, error numbers, cancel by reset + reconnect, object tree. | Unit tests + `tests/live.rs` |
| 4 | Routing factory | Profile kind selects adapter; SQLite reported as unsupported. | `rdbs-drivers` tests |
| 5 | Credentials | Saved passwords in OS keychain only; unsaved ones prompted per run; wrong password re-prompts. | `rdbs-services` + manual |
| 6 | Modes | `--demo` keeps M1 mock behaviour; default is live. | Manual |

Run the live suite:

```
docker compose -f dev/docker-compose.yml up -d
RDBS_IT=1 cargo test --workspace
```

## Milestone 3a — Daily driver (done)

| # | Deliverable | Acceptance criteria | Verified by |
|---|---|---|---|
| 1 | Multiple result sets | Every SELECT of a script gets its own result tab; PostgreSQL NOTICEs appear under Messages. | Live tests (all engines) |
| 2 | Export | Copy/Save as CSV, JSON, SQL INSERT from the result header; toolbar Export saves CSV. | `rdbs-core` export tests + manual |
| 3 | Query history | Executions recorded with outcome, rows, duration and database; searchable; reopen / run again. | `rdbs-storage` tests + manual |
| 4 | Autocomplete | Keywords, schemas, tables, columns, aliases; Ctrl+Space; dialect quoting. | `rdbs-core` complete tests |
| 5 | Other databases | Expanding another PostgreSQL/SQL Server database loads it in its own session; tabs can target a database. | `rdbs-services` test + manual |

## Milestone 3b — Editing, DDL, SSH (done)

| # | Deliverable | Acceptance criteria | Verified by |
|---|---|---|---|
| 1 | Grid editing | Edit cells, NULL/default, add/delete rows on tables with a primary key; Save applies all changes in one transaction; 0-row matches roll back. | `rdbs-core` edit tests, `apply_changes_is_transactional` live tests (3 engines) |
| 2 | DDL viewer | Show DDL for tables, views, routines; PostgreSQL DDL re-executes. | Live tests (3 engines) |
| 3 | SSH tunnel | Password and key auth, TOFU host keys, refused changed key, probe errors. | `rdbs-drivers` `ssh_live` tests (bastion in compose) |
| 4 | Secrets | SSH secret only in keychain (`<id>:ssh`), never in SQLite or logs. | `rdbs-core` profile tests, services test |

## Milestone 4 — Design and analysis tools (done)

| # | Deliverable | Acceptance criteria | Verified by |
|---|---|---|---|
| 1 | Table definitions | `describe_schema` returns columns, defaults, identity, PK, unique constraints, indexes, FKs; `ddlgen` recreates them identically. | `schema_round_trip` live test on PostgreSQL, MySQL, SQL Server, SQLite |
| 2 | SQLite adapter | Files (not created implicitly), scripts, cancellation, grid edits, DDL, rebuild-based ALTER. | `rdbs-adapter-sqlite` unit + file tests |
| 3 | Explain plan | Estimated and analyzed plans as a tree; Analyze changes rolled back. | `explain_*` live tests (4 engines), parser unit tests |
| 4 | Table designer | Create/alter with renames, keys, indexes, FKs; production confirmation. | designer unit tests, ddlgen tests |
| 5 | Import | CSV/Excel into new or existing tables in one transaction; offending cell reported. | `rdbs-transfer` import tests (xlsx, SQLite), SQL Server identity insert live test |
| 6 | Schema diff | Cross-engine comparison, dependency-ordered script. | `rdbs-core` diff tests |

## Milestone 6 — Productivity and more engines (done; Oracle live tests pending)

| # | Deliverable | Acceptance criteria | Verified by |
|---|---|---|---|
| 1 | ER diagram | Layered auto layout, pan/zoom/drag, FK connectors, SVG export. | layout/SVG unit tests + manual |
| 2 | Data transfer / sync | Any engine pair; FK order; created tables get FKs afterwards; sync inserts/updates/deletes by key; sequences/identity continue. | `transfer_live` (SQLite, PG→MySQL/SQL Server/SQLite, PG schema copy), services test |
| 3 | Query builder | Joins from FKs, aggregates + GROUP BY, sort, filters, dialect limits. | `rdbs-core` builder tests |
| 4 | Scheduled backups | Logical dumps restore to identical schemas; schedules persisted (storage v4) and run while the app is open; retention. | `backup_live` (4 engines), schedule/retention unit tests |
| 5 | MongoDB | Shell syntax (find/aggregate/CRUD/indexes/runCommand), documents as rows, collections tree. | `rdbs-adapter-mongodb` unit + live tests |
| 6 | Redis | redis-cli syntax, `INSPECT key`, db0…dbN tree with keys. | `rdbs-adapter-redis` unit + live tests |
| 7 | Oracle | Queries, PL/SQL blocks, catalog, describe, DDL, explain; needs Instant Client. | unit tests; live tests need `RDBS_ORACLE=1` + Instant Client (not run yet) |

## Milestone 5 — Release (next)

- Installers (MSI / dmg / AppImage), code signing, CI with the integration matrix, crash
  reporting, auto-update.

## Testing strategy

| Layer | Type | Notes |
|---|---|---|
| `rdbs-core` | Unit | SQL lexer, statement classifier, profile validation, value display. Pure, fast. |
| `rdbs-storage` | Unit (in-memory SQLite) | CRUD round-trip, ordering, schema migration idempotence. |
| `rdbs-adapter-mock` | Async unit | Result generation, LIMIT parsing, error path, cancellation. |
| `rdbs-services` | Integration | Worker with mock factory: connect → objects loaded, execute → finished, cancel → cancelled. |
| `rdbs-transfer` | Unit + integration | Import, transfer/sync and backup→restore round trips (SQLite always; servers with `RDBS_IT=1`). |
| `rdbs-adapter-{sqlite,redis,mongodb,oracle}` | Unit + integration | SQLite tests use temp files; Redis/MongoDB containers in the compose file; Oracle opt-in (`--profile oracle`, `RDBS_ORACLE=1`). |
| `rdbs-adapter-{postgres,mysql,mssql}` | Unit + integration (opt-in) | Value/type mapping and script handling are unit-tested offline. `tests/live.rs` runs against `dev/docker-compose.yml` (`postgres:17`, `mysql:8.4`, `mssql/server:2022` with `MSSQL_AGENT_ENABLED`) only when `RDBS_IT=1`. The SQL Server suite includes a read-only Agent job round trip. |
| `rdbs-ui` | Logic unit tests + manual checklist | Pure helpers (formatting, selection → byte range) unit-tested; visual behaviour verified with the checklist above. egui_kittest snapshot tests are a later option. |

## Dependency & licence review (M1)

| Crate | Version | Licence | Note |
|---|---|---|---|
| egui / eframe / egui_extras | 0.36.x | MIT OR Apache-2.0 | Pinned to match egui_dock. |
| egui_dock | 0.21.x | MIT | Latest; targets egui 0.36. |
| egui-phosphor | 0.14.x | MIT (icons: MIT) | Icon font. |
| tokio | 1.x | MIT | |
| rusqlite (bundled) | 0.40.x | MIT; SQLite is public domain | Needs a C compiler (MSVC present). |
| serde / serde_json / thiserror / tracing / uuid / directories | latest | MIT / Apache-2.0 | |
| M2: tokio-postgres, mysql_async, tiberius, keyring | — | MIT / Apache-2.0 | tiberius optionally pulls native-tls / rustls — choose rustls for portability. |

No copyleft dependencies. Add `cargo-deny` with a licence allow-list before M2.

## Known technical risks

| Risk | Mitigation |
|---|---|
| egui `TextEdit` slows on very large scripts; no built-in autocomplete. | Keep editor isolated behind `editor.rs`; evaluate a custom rope-based editor widget in M3. |
| Tiberius has limited query cancellation. | Drop and reconnect the session on cancel; surface this via `Capabilities::query_cancellation`. |
| egui/egui_dock version coupling. | Pin both; upgrade together. |
| Huge result sets exhaust memory. | M2: stream rows in pages from the worker, cap in-memory rows with "fetch more". |
| Accidental writes on production. | Environment tagging + statement classifier + confirmation (ADR 0006). |
