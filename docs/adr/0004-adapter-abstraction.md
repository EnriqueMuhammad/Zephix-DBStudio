# ADR 0004 — Capability-based adapter abstraction

Status: Accepted · 2026-10-16

## Decision
- `AdapterFactory::connect(profile, secret) -> Box<dyn DatabaseAdapter>` creates a session.
- `DatabaseAdapter` exposes `capabilities()`, `server_version()`, `load_objects()`,
  `execute(sql, cancel)`. Methods return boxed futures so the traits stay object-safe
  without an `async-trait` dependency.
- `Capabilities` is a plain struct of booleans (cancellation, streaming, transactions,
  schemas, multiple databases, explain). The UI enables/disables features from it instead of
  matching on the engine.
- Values are `CellValue` (typed variants + `Other { type_name, display }`) and every column
  keeps its **native type name**, so nothing is lossy-converted to a generic string type.
- Errors are `DbError { kind, message, code, detail }` where `code` is the vendor code
  (SQLSTATE, MySQL errno, SQL Server error number).

## Driver choice (supersedes the original SQLx plan)
| Engine | Crate | Why |
|---|---|---|
| PostgreSQL | `tokio-postgres` | Raw type access, `simple_query`, `CancelToken`, COPY. |
| MySQL / MariaDB | `mysql_async` | Dynamic row decoding; cancellation via `KILL QUERY` on a side connection. |
| SQL Server | `tiberius` | Only mature pure-Rust TDS driver. Weak cancel support → drop connection fallback. |

SQLx was rejected: it is optimised for compile-time-checked queries against known schemas,
its dynamic decoding is awkward for an ad-hoc SQL client, and it has no query cancellation.
