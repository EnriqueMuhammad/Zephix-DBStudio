# ADR 0015 — Installed fonts and font settings

Status: accepted (2026-11)

## Context

Zephix used egui's built-in fonts at fixed sizes. Users coming from desktop database tools
expect the system UI font, a fixed-width editor font such as Consolas, and separate sizes for
the interface, the editor and the results grid. egui needs font bytes, not a family name, and
does not list installed fonts.

## Decision

- **Tools > Fonts…** has three items, chosen independently: Environment (menus, dialogs,
  explorer, tabs), Text Editor (SQL editor, messages, DDL, plans) and Grid Results. Sizes are in
  typographic points (96 dpi). Changes preview immediately; Cancel restores the settings the
  dialog opened with. Settings are stored in `app.ron` (`rdbs.fonts`).
- Defaults: Segoe UI 10 pt (interface, grid) and Consolas 10 pt (editor) on Windows, Menlo
  for the editor on macOS, built-in fonts elsewhere. 10 pt keeps the interface at the size the
  layout was designed for.
- Installed fonts are listed with **`fontdb`** (pure Rust; reads the OS font folders and the
  name tables). The scan runs on a background thread at start-up; until it finishes the
  built-in fonts are shown, then the chosen families are applied. Chosen families are put in
  front of egui's lists, so missing glyphs (icons, symbols) still come from the built-in fonts
  and a family that is not installed silently falls back.
- Fixed sizes in the UI code are scaled by the interface size (`theme::ui_font`), explorer and
  grid row heights follow their font, and the grid uses its own font family (`grid`).
- Tests start the app with `system_fonts: false`, so layouts do not depend on the machine.

## Consequences

- One small dependency (`fontdb`, which adds `memmap2`, `slotmap` and `tinyvec`); no C
  libraries.
- Very large sizes can still crowd fixed-size chrome (toolbar, tab bar); sizes are limited to
  7–28 pt.
