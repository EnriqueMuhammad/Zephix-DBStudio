# ADR 0008 — Milestone 3a: results, export, history, autocomplete, multi-database

Status: Accepted · 2026-10-16

## Multiple result sets and server messages
`QueryOutcome.results: Vec<ResultSet>` replaces the single result; the UI shows one tab per
result set plus a Messages tab. PostgreSQL notices (`RAISE NOTICE`, warnings) are captured by
driving `Connection::poll_message` in the connection task and attached to the outcome. MySQL
reports the warning count. SQL Server `PRINT` output is not exposed by tiberius (open item).
Column types for every PostgreSQL result set come from preparing the matching statement.

## Export
`rdbs_core::export` is pure: CSV (RFC 4180, CRLF, NULL → empty), JSON (array of objects;
exact numerics stay strings to keep precision, JSON columns are embedded as JSON) and SQL
`INSERT` statements in the connection's dialect (quoting, bytea/`X''`/`0x` binary literals,
`N''` strings on SQL Server). "Copy as …" uses the clipboard; "Save as …" opens the native
dialog (`rfd`) on a helper thread so the UI never blocks.

## Query history
Stored in the local SQLite file (`query_history`, schema v2; v3 adds `database`). Every
finished execution is recorded with connection, database override, duration, row count and
outcome; the newest 5 000 entries are kept. The History tab filters by SQL text or connection
(LIKE wildcards are escaped) and reopens scripts in the database they ran in. Inserts happen on
the UI thread: the local file is small and WAL-backed, unlike remote database I/O.

## Autocomplete
`rdbs_core::complete` is a pure function of (script, cursor, catalog). The catalog is derived
from the explorer tree. Context comes from the lexer: no suggestions inside strings/comments;
tables after FROM/JOIN/UPDATE/INTO; columns of referenced tables after SELECT/WHERE/ON/…;
`alias.` and `table.` resolve to columns, `schema.` to its tables. Names are quoted when the
dialect requires it (e.g. mixed case on PostgreSQL). The popup opens while typing an
identifier or on Ctrl+Space; ↑/↓, Enter/Tab, Esc are consumed before the editor sees them.

## Other databases on the same server
PostgreSQL and SQL Server sessions are bound to one database. Sessions are now keyed by
(profile, database): expanding a "not connected" database opens a secondary session that only
loads objects (no Connected/Disconnected noise on the main node), and query tabs carry an
optional database override chosen in the toolbar (set automatically when a table of another
database is opened). Disconnect closes every session of the profile.

## Deferred to Milestone 3b
Editing table data in the grid (with transactions), SSH tunnels, SQL Server PRINT messages,
lazy per-node catalog loading for very large servers.
