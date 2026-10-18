# ADR 0005 — Local storage (SQLite) and credential handling

Status: Accepted · 2026-10-16

## Decision
- Connection profiles live in a local SQLite file (`rusqlite`, bundled SQLite) in the
  platform data directory (`directories::ProjectDirs`). Each row stores the profile as JSON
  plus indexed columns, which keeps schema migrations cheap while the model evolves.
- Passwords are **never** stored in SQLite. From M2 they go to the OS credential store via the
  `keyring` crate (Windows Credential Manager, macOS Keychain, Secret Service on Linux).
  Implemented in Milestone 2 — see ADR 0007.
- UI layout (dock tree, open tabs, SQL text) is persisted by eframe's storage (RON file),
  window geometry by eframe's `persist_window`.
