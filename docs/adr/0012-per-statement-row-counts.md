# ADR 0012 — Per-statement row counts and a vendored tiberius

Status: accepted (2026-10)

## Context

The Messages view showed one summary ("3 rows affected") for a whole script. Users coming from
database consoles expect a line per statement in execution order — `(1 row affected)` after an
`UPDATE`, `(25 rows affected)` after a `SELECT` — including inside a transaction script such
as `BEGIN TRANSACTION; UPDATE …; SELECT …; ROLLBACK`.

PostgreSQL, MySQL, SQLite and Oracle report a count per statement through their drivers. For
SQL Server, tiberius reads the per-statement `DONE` tokens but drops them in `QueryStream`;
only `Client::execute` returns them, and it runs the batch through `sp_executesql` without
result sets. Running transaction control there fails (error 266, transaction count mismatch),
and temporary tables and `SET` options would not survive the batch.

## Decision

- `QueryOutcome::messages` lists every statement in order. Adapters call
  `record_rows_affected(n)` for data-modifying statements and `push_result(rs)` (or push the
  server's count) for result sets. `rdbs_core::sql::reports_row_count` decides which
  statements carry a meaningful count where the driver reports a zero for `BEGIN`, DDL and
  similar statements. `rows_affected` keeps the last count.
- tiberius 0.13.0 is vendored in `vendor/tiberius` with a small patch
  (`QueryStream::take_done_counts`, see `vendor/README.md`) and wired in with
  `[patch.crates-io]`. The SQL Server adapter runs every batch as a plain SQL batch and maps a
  count taken while a result set is open to that set, any other count to a data change.
  Statements without `DONE_COUNT` (transaction control, `SET`, `DECLARE`) produce no line.
- The Messages view prints "Commands completed successfully." when a script produced no
  messages, and the completion time.

## Consequences

- One code path for SQL Server batches; the `sp_executesql` special case for single DML
  statements is gone.
- tiberius upgrades need the patch reapplied until upstream exposes the counts.
- A statement the PostgreSQL/MySQL splitter does not see (it splits on top-level `;`) is
  reported with whatever count the server sends.
