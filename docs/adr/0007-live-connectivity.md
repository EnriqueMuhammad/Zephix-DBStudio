# ADR 0007 — Live connectivity: execution model, cancellation, credentials, modes

Status: Accepted · 2026-10-16 (Milestone 2)

## Execution model per engine
| Engine | How a script runs | Result shown | Native types |
|---|---|---|---|
| PostgreSQL | One simple-query round trip (server runs every statement; implicit transaction unless the script manages its own). Values arrive in PostgreSQL's text format. | Last row-returning statement | `prepare` of that statement after execution (best effort; falls back to `unknown`) |
| MySQL / MariaDB | One multi-statement `COM_QUERY`; compound statements need no `DELIMITER`. | Last result set | From column metadata (type + charset + flags) |
| SQL Server | Split on `GO` lines; a batch that is one plain `INSERT/UPDATE/DELETE/MERGE` (no `OUTPUT`) runs via `sp_executesql` to report affected rows, everything else as a SQL batch. | Last result set | From TDS column metadata |

Results are capped at `MAX_RESULT_ROWS` (200 000); extra rows are drained and dropped and the
outcome is flagged `truncated`.

## Cancellation
`CancelHandle` became a waker-based future, so adapters race their I/O against it without
polling:
- PostgreSQL: protocol cancel request via `CancelToken` (same TLS settings); session stays usable.
- MySQL: `KILL QUERY <connection id>` from a short-lived side connection; session stays usable.
- SQL Server: tiberius does not expose TDS attention, so the connection is dropped and re-opened
  on next use. The error detail tells the user that temp tables / transactions were discarded.

A query failing with a connection-class error discards the session so the next run reconnects.

## Credentials
- `CredentialStore` trait in `rdbs-core`; `KeyringCredentials` (OS keychain via `keyring` 4,
  service "RustDB Studio", account = profile id) in `rdbs-storage`; `MemoryCredentials` for demo
  mode and tests. The worker reads secrets on a blocking thread.
- Profiles with "save password" read the keychain; others prompt once per run and keep the
  password in memory only. A rejected password (authentication error) clears the in-memory copy
  and re-opens the prompt.

## Modes
- `zephix` — live drivers, profiles in `studio.db`, keychain secrets.
- `zephix --demo` (or `RDBS_DEMO=1`) — mock engine, sample profiles in memory, nothing
  written except window layout (stored under a separate key).

## TLS
rustls with the `ring` provider for all three drivers (avoids the CMake/NASM toolchain that
`aws-lc-rs` needs on Windows). `Prefer`/`Require` encrypt without verifying the certificate
(libpq semantics); `VerifyFull` verifies against the OS trust store (PostgreSQL) or webpki roots
(MySQL, SQL Server). MySQL `Prefer` falls back to plaintext only for non-authentication failures.

## Known limitations (tracked for Milestone 3)
- One connection per profile; PostgreSQL/SQL Server browse only the connected database
  (other databases are listed as leaves).
- Object trees load eagerly; very large catalogs should move to lazy per-node loading.
- Server messages (NOTICE / PRINT) are not surfaced yet.
