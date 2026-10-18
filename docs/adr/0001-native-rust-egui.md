# ADR 0001 — Native Rust UI with egui + eframe

Status: Accepted · 2026-10-16

## Context
We need a single native binary on Windows, macOS and Linux, no WebView, and a UI that can
render very large result grids without stutter.

## Decision
- Rust + `egui`/`eframe` 0.36, `egui_dock` 0.21 for the tab workspace, `egui_extras` tables
  for the result grid, `egui-phosphor` (MIT) for icons.
- `eframe` uses the **glow** (OpenGL) backend instead of the default wgpu: smaller binary,
  shorter compile times, sufficient for a 2D IDE. Switching to wgpu is a one-line feature change.
- egui is pinned to 0.36 because `egui_dock` 0.21.1 (latest) targets egui 0.36. Upgrade
  egui only together with an `egui_dock` release.

## Consequences
- Immediate-mode rendering makes virtualised grids simple and fast.
- The SQL editor is built on `TextEdit` with a custom layouter for highlighting; autocomplete
  popups and very large scripts (> ~1 MB) will need custom work (risk tracked in milestones).
- Native look is "IDE-like" rather than OS-native widgets; acceptable for a developer tool.
