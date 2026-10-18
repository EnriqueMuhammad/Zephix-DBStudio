# ADR 0006 — Production safeguards

Status: Accepted · 2026-10-16

## Decision
- Every profile has an `Environment` (Development, Staging, Production) that is shown as a
  coloured marker in the explorer, tab and status bar.
- Before execution, SQL is classified by `rdbs_core::sql::classify` into Read / Write / Ddl /
  Transaction / Other. On Production, anything other than Read requires an explicit
  confirmation dialog that names the connection.
- Future (M3): optional read-only sessions, statement allow-lists, and auto-wrapping data
  edits in a transaction with explicit commit.
