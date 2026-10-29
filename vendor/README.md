# Vendored dependencies

Crates patched for Zephix and wired in with `[patch.crates-io]` in the workspace
`Cargo.toml`. They are excluded from the workspace, so `cargo clippy --workspace` and
`cargo test --workspace` do not build their tests.

## tiberius 0.13.0

Upstream: <https://crates.io/crates/tiberius/0.13.0> (MIT OR Apache-2.0, licenses kept in
`tiberius/`). Only `src/`, the manifest, the licenses and the README are copied; the
`[[example]]` and `[[test]]` targets were removed from the manifest because their sources are
not vendored, and `dead_code` is allowed for the Windows SSPI paths this workspace does not
enable.

The patch (`tiberius-done-counts.patch`, see ADR 0012) lets a `QueryStream` report the row
count of every statement in a batch. Upstream drops the TDS `DONE` / `DONEINPROC` tokens in
query streams, so a SQL Server batch such as `BEGIN TRAN; UPDATE …; SELECT …` could not show
"(n rows affected)" per statement:

- `TokenDone::count()` returns the row count when the `DONE_COUNT` status bit is set.
- `QueryStream::take_done_counts()` returns the counts of the statements completed since the
  last call, in order, including those skipped before the first result set.

To upgrade tiberius: copy the new release the same way, reapply the patch
(`git apply --directory=vendor/tiberius vendor/tiberius-done-counts.patch`, or by hand), and
run the SQL Server live tests. Drop the vendored copy once upstream exposes the counts.
