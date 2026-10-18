# Zephix - Database Management Studio

Native desktop database IDE for PostgreSQL, MySQL/MariaDB, SQL Server, SQLite, Oracle, MongoDB
and Redis. Written in Rust (egui); runs on Windows, macOS and Linux as a single executable.

## Features

- **Connections**: saved profiles, TLS modes, SSH tunnels (password or key, host key check),
  passwords in the OS keychain (Windows Credential Manager, macOS Keychain, Linux Secret Service)
  or asked once per run, confirmation before writes on production connections.
- **Query editor**: syntax highlighting, autocomplete, run selection or script, multiple result
  sets, stop a running query, query history, explain plan (and explain analyze).
  MongoDB uses shell syntax, Redis uses redis-cli syntax.
- **Results**: fast grid for large result sets, editing applied in one transaction, export to
  CSV, JSON or SQL.
- **Explorer**: every database of a server, DDL viewer, table designer (fields, keys, indexes,
  foreign keys).
- **Tools**: CSV/Excel import, schema comparison with a migration script, ER diagram (SVG
  export), data transfer and sync between servers, visual query builder, backups on demand or on
  a schedule.

## Install and run

Windows: install `Zephix-<version>-x64.msi` (per-user, no administrator rights; built with
`.\scripts\release.ps1`, see [docs/RELEASING.md](docs/RELEASING.md)) or run the portable
`zephix.exe`. macOS/Linux: build from source (below). The executable is self-contained; Oracle
connections additionally need Oracle Instant Client (64-bit) on `PATH`.

```bash
zephix --demo     # built-in sample engine and connections, nothing is saved
```

Profiles are stored in `%APPDATA%\Zephix\RustDB Studio\data\studio.db` on Windows (the platform's
app-data folder elsewhere), window layout in `%APPDATA%\Zephix\data\app.ron`.

## Build from source

Requires Rust 1.95+. Windows: Visual Studio Build Tools (MSVC). Linux: X11/Wayland and OpenGL
development packages.

```bash
cargo run -p zephix                      # run
cargo build --release -p zephix          # target/release/zephix(.exe)
.\scripts\release.ps1                     # Windows: dist\ MSI, portable exe, SHA256SUMS.txt
```

Checks:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Integration tests run against throw-away servers in Docker. Create the SSH test key once (see
[dev/ssh/README.md](dev/ssh/README.md)), then:

```bash
ssh-keygen -t ed25519 -N "" -C rdbs-it -f dev/ssh/test_key
docker compose -f dev/docker-compose.yml up -d --build
RDBS_IT=1 cargo test --workspace -- --test-threads=1
```

Without `RDBS_IT=1` the integration tests are skipped. Oracle runs separately
(`--profile oracle`, `RDBS_ORACLE=1`, Oracle Instant Client). Design notes: [docs/](docs/).

## License

Created by **Enrique Muhammad Ilham**. © 2026 Enrique Muhammad Ilham.

Source-available under the [PolyForm Noncommercial License 1.0.0](LICENSE.md). Free for personal,
non-commercial use (hobby, study, private use), and for the non-profit organizations the license
lists (charities, schools, public research, government institutions). **Commercial use, including
use inside a company, is not permitted** without a separate license from the author; get in touch
for one. Donations to support development are welcome.

Third-party crates used by the app keep their own licenses (mostly MIT / Apache-2.0).
