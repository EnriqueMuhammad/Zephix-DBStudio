# ADR 0009 — Milestone 3b: grid editing, DDL viewer, SSH tunnels

Status: Accepted · 2026-10-16

## Editing table data
- Only tabs opened from a **table** with a **primary key** (read from the explorer tree) are
  editable, and only while the single result contains every key column. Views, multi-result
  scripts and key-less tables are read-only with an explanation in the edit bar.
- Pending changes (`rdbs_core::edit::PendingEdits`) are kept per tab: edited cells (amber),
  rows to delete (red, struck through), new rows (green, `DEFAULT` until typed). Running the
  query again is blocked until changes are saved or discarded.
- `build_changes` produces one `UPDATE`/`DELETE`/`INSERT` per row, identified by primary-key
  literals; typed text becomes a literal matching the original column (numbers unquoted,
  booleans per dialect, everything else a string the server coerces).
- `DatabaseAdapter::apply_changes` runs them in **one transaction** and requires each statement
  to affect exactly one row; otherwise it rolls back (optimistic concurrency: a row changed or
  deleted by someone else is detected). Per engine: tokio-postgres `Transaction`; mysql_async
  `Transaction` with explicit rollback; SQL Server `SET XACT_ABORT ON; BEGIN TRANSACTION` as plain
  batches with DML through `sp_executesql`.
- Saving on a Production connection asks for confirmation. "Copy SQL" shows exactly what Save
  will run. After a successful save the tab re-runs its query.

## DDL viewer
`DatabaseAdapter::object_ddl` returns `CREATE` statements, opened in a new query tab:
MySQL `SHOW CREATE …`; PostgreSQL reconstructed from `pg_attribute`/`pg_constraint`
(`pg_get_constraintdef`), `pg_get_indexdef`, `pg_get_viewdef`, `pg_get_functiondef` (identity
and generated columns included; the integration test re-executes the generated DDL);
SQL Server from `sys.columns`/`sys.key_constraints`/`sys.foreign_keys`/`sys.indexes` and
`OBJECT_DEFINITION` for views and routines.

## SSH tunnels
- `rdbs-ssh` (russh, `ring` crypto): password or private-key authentication, local listener on
  an ephemeral `127.0.0.1` port, one `direct-tcpip` channel per TCP connection. A probe channel
  is opened at connect time so "forwarding disabled" or "target unreachable from the bastion"
  is reported clearly instead of as a broken database handshake.
- Host keys: trust on first use, recorded in the app's own `known_hosts`
  (`%APPDATA%\Zephix\RustDB Studio\data\known_hosts`); a changed key is refused with a
  man-in-the-middle warning.
- `LiveFactory` wraps the adapter so the tunnel lives exactly as long as the session.
- The SSH secret is a transient `SshTunnel::secret` (`#[serde(skip)]`, redacted in `Debug`):
  filled from the connection dialog for "Test", otherwise from the keychain by the worker.
  Keychain accounts: `<profile id>` (database) and `<profile id>:ssh` (tunnel).

## Logging
The worker logs only command names; command payloads contain passwords and are never logged.

## Not done yet
SSH agent / certificates / jump chains; prompting for an SSH secret that is not saved;
editing in non-default-schema-qualified views; SQL Server PRINT messages.
