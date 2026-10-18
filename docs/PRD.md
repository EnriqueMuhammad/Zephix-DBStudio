# Zephix — Product Requirements Document

Status: Draft v0.1 · Owner: Zephix · Last updated: 2026-10-16

## 1. Problem

Developers and DBAs who work across SQL Server, PostgreSQL and MySQL juggle several
vendor tools (SSMS, pgAdmin, Workbench) or pay for a commercial multi-engine client.
We want one fast, native, cross-platform database IDE that we own and can extend.

Zephix is an **original clean-room implementation**. It takes inspiration from the
*workflow* of professional database IDEs (connection explorer, tabbed query workspace,
data grid) but contains no proprietary code, assets, icons or branding from any vendor.

## 2. Target users

| Persona | Needs |
|---|---|
| Application developer | Browse schemas, write and run queries, inspect and edit rows quickly. |
| DBA / ops | Work against production safely, see execution info, cancel long queries. |
| Data analyst | Run read queries, export results. |

## 3. Goals

1. Native desktop app (Windows, macOS, Linux) with a single binary, no WebView.
2. Multi-engine: SQL Server, PostgreSQL, MySQL/MariaDB first; SQLite next; Oracle later.
3. The UI never freezes on database I/O.
4. Safe by default on production connections.
5. Large result sets stay responsive (streaming + virtualised grid).

## 4. Non-goals (v1)

- Cloud sync / team collaboration.
- Visual query builder, data/structure sync, ER modelling (v3 candidates).
- Web or mobile versions.

## 5. Feature roadmap

| Milestone | Scope |
|---|---|
| **M1 — Shell (this milestone)** | App shell, dark theme, explorer with mock data, resizable sidebar, dock tabs, SQL editor, result grid with mock data, connection dialog UI, persisted window + layout. |
| M2 — Live connectivity | PostgreSQL (`tokio-postgres`), MySQL (`mysql_async`), SQL Server (`tiberius`); OS keychain credentials; real object explorer; query cancel; streaming results. |
| M3 — Daily driver | TLS + SSH tunnel, autocomplete, multiple result sets, table data editing with transactions, export CSV/JSON/SQL, DDL viewer, query history. |
| M4 — Power tools | Table designer, import, explain-plan viewer, schema diff, SQLite adapter. |

## 6. Functional requirements (M1)

| ID | Requirement |
|---|---|
| F1 | App starts natively and shows the main shell within 1 s on a typical dev machine. |
| F2 | Menu bar, toolbar, left explorer, central dock workspace, bottom status bar. |
| F3 | Dark theme with consistent palette, typography and icon set. |
| F4 | Explorer lists connection profiles; expanding one "connects" (mock) and shows databases → schemas → tables / views / routines. |
| F5 | Explorer has search filter and context menus (connection and object level). |
| F6 | Sidebar is resizable and toggleable (Ctrl+B). |
| F7 | Workspace tabs can be opened, closed, reordered, split and undocked. |
| F8 | SQL editor with syntax highlighting, connection selector, Run (Ctrl+Enter), Stop. |
| F9 | Results render in a virtualised grid (tested with 100 000 rows), NULLs distinguished, column types shown. |
| F10 | Status line per query: rows, elapsed time, connection; errors displayed with vendor code. |
| F11 | Connection dialog: engine picker, General/SSL/SSH/Advanced sections, validation, Test, Save. |
| F12 | Window size/position, sidebar width, open tabs and their SQL text survive restart. |
| F13 | Destructive statements on a Production profile require explicit confirmation. |

## 7. Non-functional requirements

- No database or network I/O on the UI thread (enforced by architecture: typed commands → worker).
- `cargo fmt`, `cargo clippy -D warnings`, `cargo test` clean on every merge.
- No `unwrap()` / `expect()` in recoverable runtime paths (clippy lint enabled).
- Passwords are never written to the local SQLite file (OS keychain from M2).
