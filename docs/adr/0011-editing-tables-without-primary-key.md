# ADR 0011 — Editing tables without a primary key

Status: Accepted · 2026-10-29

## Context
Grid editing found rows by their primary key, so tables without one (heaps such as shift or
log tables) were read-only. Such tables are common in legacy databases, and only some engines
expose a stable row address (SQLite `rowid`, Oracle `ROWID`); SQL Server's `%%physloc%%` is
undocumented and MySQL's internal row id is not reachable from SQL.

## Decision
- `rdbs_core::edit::row_match` decides how rows are identified: by the primary key when there
  is one, otherwise by the values of **all** columns (`RowMatch::AllColumns`). The table's
  columns come from the explorer tree (`TableTarget::columns`); every one must be in the
  result, or the grid stays read-only.
- `NULL` is matched with `IS NULL`. Columns that cannot be compared with `=` (large objects,
  XML, JSON, spatial, SQL Server `text` / `ntext` / `image` / `sql_variant`, binary values)
  are left out of the `WHERE` clause; with none left the grid stays read-only.
- Values read back from SQL Server `datetime` / `smalldatetime` are compared with ISO 8601
  literals rounded to milliseconds, independent of `DATEFORMAT`.
- When the loaded rows contain identical rows (equal on every column, none left out), an
  `UPDATE` / `DELETE` changes one of them: `TOP (1)` (SQL Server), `LIMIT 1` (MySQL,
  MariaDB), `ROWNUM = 1` (Oracle), a `ctid` (PostgreSQL) or `rowid` (SQLite) sub-select.
  Identical rows cannot be told apart, so either one is correct.
- Otherwise statements keep the existing guard: each must affect exactly one row, or the whole
  transaction is rolled back (for example when another row matches outside the loaded page,
  or a case-insensitive collation makes two rows equal).
- The edit bar shows a "no primary key" warning with an explanation; no extra confirmation is
  asked (production profiles still confirm every save, ADR 0006).
