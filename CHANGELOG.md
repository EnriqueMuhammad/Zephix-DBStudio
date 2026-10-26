# Changelog

All notable changes to Zephix. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project uses [Semantic Versioning](https://semver.org/).

## 0.2.0 - 2026-10-26

### Added

- SQL Server connections show **SQL Server Agent → Jobs** in the explorer: each job's name,
  whether it is enabled, its owner and category, the last run outcome and time, and the next
  scheduled run. A job that is running shows a running indicator and, when known, the current
  step. **View History** (or a double-click) opens each step of each run, including the
  message, in a result tab. **Script Job as CREATE** opens a T-SQL script of the job, its steps
  and its schedules, without running it. **Start Job**, **Start Job at Step**, **Stop Job**,
  **Enable Job** and **Disable Job** call `sp_start_job`, `sp_stop_job` and `sp_update_job`.
  Every action asks for confirmation on a production connection, and Stop asks on every
  connection. When Agent is missing (SQL Server Express), `msdb` is offline, or the login
  cannot read it, the folder explains that instead of failing the refresh. A refused action
  (permission, Agent stopped, already running, disabled) is reported in the status bar.
  Editing a job's steps or schedules is not in this version.
- **Table Sync…** (Tools menu, and the explorer menu of a database or schema) makes the tables
  of a target equal to the source, on the same or another engine. It synchronizes the
  **tables and columns** (creates missing tables; adds, changes and optionally drops columns,
  keys and indexes; rows are not touched), the **data only** (by primary key, as before), or
  **tables, columns and data**. The Data transfer tool offers the same choices under
  "Synchronize".
- Right-clicking a tab offers **Close Tab**, **Close Other Tabs** and **Close All Tabs**;
  **File > Close All Tabs** (Ctrl+Shift+W) closes every tab.

### Changed

- Data tabs have two tidy bars instead of three: refresh, filter, sort order and row limit (the
  filter and sort fields share the width) with the connection on the right; below, the row
  buttons next to Messages and Export.
- The Database and Schema fields of the transfer, sync, schema comparison and backup tools open
  their list when the text or the arrow is clicked, and typing filters the list.

### Fixed

- Tab titles stay whole when the tab bar runs out of room. Titles shrink with an ellipsis, the
  rest scroll, and hovering a shortened title shows the full name. Tabs no longer slide under
  the new-tab button.
- Dragging a selection past the SQL editor scrolls the editor and keeps extending the selection.
- A window the user restores larger than the default size stays that size. The window is pulled
  back only when it spills past the screen, and the default size is used on the first launch.
- Explorer names and the detail beside them end with an ellipsis when the sidebar is narrow.
  Hovering the row shows the full text.
- The SQL editor shows a horizontal scrollbar when a line is wider than the editor.
- Synchronizing data into MySQL or MariaDB could stop with "affected 0 rows" on date/time
  columns: values such as `23:59:59` and `23:59:59.000000` now count as equal. MySQL and
  MariaDB connections also count matched rows as affected, so an `UPDATE` that writes the
  values a row already has no longer looks like a conflict.
- SQL Server `datetime2` and `datetime2(7)` columns are no longer reported as different by
  schema comparison.

## 0.1.2 - 2026-10-19

### Added

- Light theme: View > Light Theme or the sun/moon button at the right of the menu bar; the
  choice is remembered.
- Double-clicking a table or view opens its rows in a data tab: a grid without an SQL editor,
  with a filter (`WHERE`), a sort order (`ORDER BY`), a row limit (100 / 1,000 / 10,000 / all),
  Refresh and "Open as Query". Grid editing works as in query tabs. The columns of a table
  still expand with the arrow next to it.
- **Select Top 100 Rows** on tables and views: a query tab with the engine's `SELECT`
  (`TOP`, `LIMIT` or `FETCH FIRST`), already run.
- Functions and procedures can be edited: **Edit** (or a double-click) opens the definition as a
  script that saves the changes when run (`CREATE OR ALTER` on SQL Server, `CREATE OR REPLACE`
  on PostgreSQL, MariaDB and Oracle, `DROP` + `CREATE` on MySQL).
- Functions and procedures can be executed: **Execute…** asks for the parameter values (or
  `NULL`) and runs the matching call (`EXEC`, `CALL`, `SELECT`, an anonymous block on Oracle),
  showing output parameters as a result. Routines without parameters run right away.
- The installer shows a welcome page with the license, progress, and a finish page with a
  "Launch Zephix" option; running it again offers Repair and Remove. Its pictures use the
  colors and icon of Zephix.
- Demo mode (`--demo`) has definitions and parameters for its sample tables, views and routines.
- Right-click menu in the SQL editor: Cut, Copy, Paste, Select All and Run (or Run Selection).
- Rows can be selected in result grids: click a row number, Ctrl-click to add, Shift-click for a
  range, the blank top-left cell or Ctrl+A for all. **Copy** (Ctrl+C) copies the rows, **Copy with headers**
  (Ctrl+Shift+C) adds the column names; both paste as a table into spreadsheets.
- Several result sets can be shown **stacked**, one grid above the other (the default, toggle
  "Stacked"), instead of as tabs; Export offers each result set.
- Connections in the explorer can be reordered by dragging them; the order is saved.

### Changed

- Result grids look like a spreadsheet: cell borders, a one-line header (the column type shows
  on hover), columns sized to their content, and a compact row-number column. Stacked result
  sets are as tall as their rows and separated by a divider.
- Zephix opens on the home page (Welcome tab), also when it was closed last time; the other
  tabs of the last session are restored behind it.
- A window that does not fit the screen (small or highly scaled displays, or a size remembered
  from a larger monitor) opens maximized, and the restore button gives it a size that fits.
- Installing a newer build of the same version replaces the installed copy instead of adding a
  second one.

### Fixed

- The status bar showed only the end of multi-line messages (such as the SQL Server version);
  it now shows one line and the full text on hover.

### Removed

- The disabled "Format" button of query tabs and the inactive Truncate / Drop menu items.

## 0.1.0 - 2026-10-18

First release: PostgreSQL, MySQL, MariaDB, SQL Server, SQLite, Oracle, MongoDB and Redis;
connection profiles with SSH tunnels and keychain passwords; query editor with autocomplete,
history and explain plans; result grid with editing and export; table designer, import,
schema diff, ER diagram, data transfer and sync, query builder and scheduled backups;
per-user MSI installer.
