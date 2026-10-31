# Changelog

All notable changes to Zephix. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project uses [Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- **Reconnect** on a query tab (and on a connection in the explorer). It closes that tab's
  database session and opens a new one. The tab stays open and the editor text stays. A query
  that fails because the server dropped the connection offers **Reconnect** and **Reconnect
  and retry**. The dropped session is discarded, so the next statement is not sent down the
  same dead connection.
- An **Edit** menu in the top menu bar with the editor commands that lived under the
  right-click **Advanced** submenu (word wrap, white space, case, line moves, sort, indent
  and the rest), plus cut, copy, paste and comment. The right-click menu is unchanged.
- Each query tab shows a connection dot on its title: green when that tab's session is
  open, amber when it was dropped or the last connect failed, grey when it is not connected
  yet or still connecting. The tooltip names the connection and the state. Clicking the dot,
  or **Reconnect** in the tab's right-click menu, opens a new session.
- Running a query that still needs a session shows **Connecting…** on the query tab and
  its status line, and the tab dot stays grey, until the session is open. It switches to
  **Running…** when the statement actually starts. A query on a session that is already
  open stays on **Running…**.
- Idle sessions are pinged about every 45 seconds, off the UI thread, and skipped while a
  statement is running. A failed ping marks the session dropped (the tab dot turns amber)
  without waiting for the next query. The ping also keeps a quiet networked connection from
  sitting idle. SQLite answers with a local `SELECT 1`; the dot stays green while the file
  is open.

## 0.3.1 - 2026-10-30

### Added

- Tables without a primary key can be edited in result grids and data tabs. Rows are found by
  the values of all their columns (`NULL` included; large objects, XML, JSON and binary values
  are left out of the match), and the edit bar shows "no primary key". Each change must still
  affect exactly one row or the whole save is rolled back; of identical rows, one is changed.
  The query must return every column of the table, otherwise the grid stays read-only and says
  which column is missing.
- **Messages** lists every statement in execution order, `(1 row affected)` after a data change
  and `(n rows affected)` after a query, then the completion time; a script without any shows
  "Commands completed successfully.". On SQL Server this includes batches that mix transaction
  control, data changes and queries.
- The SQL editor closes `(`, `[`, `'` and `"` as they are typed, wraps a selection in them,
  steps over a closer that is already there and removes an empty pair with Backspace.
- Typing `begin` offers a transaction template (`begin transaction`, a `--Query` line, a
  commented `--commit` and `rollback`), with the caret on the line for the work.
- An **Advanced** submenu in the editor's right-click menu: Tabify / Untabify Selected Lines,
  Make Uppercase (Ctrl+Shift+U) / Lowercase (Ctrl+Shift+L), Move Selected Lines Up / Down
  (Alt+Up / Alt+Down), Delete Horizontal White Space (Ctrl+K, Ctrl+\\), View White Space
  (Ctrl+E, Ctrl+S), **Word Wrap** (Ctrl+E, Ctrl+W), Sort Lines, Join Lines, Next / Previous
  Subword (Ctrl+Alt+Right / Left), Expand Selection to Line (Shift+Alt+E) and Increase /
  Decrease Line Indent (Tab / Shift+Tab). Comment and Uncomment Selection (Ctrl+K, Ctrl+C /
  Ctrl+K, Ctrl+U) sit in the menu itself. Word wrap and white space are remembered.

### Changed

- Autocomplete suggestions are left-aligned, with the schema or table and the kind of item in
  a right column behind a divider.

### Fixed

- Table names were sometimes missing from autocomplete (also with Ctrl+Space) when the tab
  ran in a database that had not been expanded in the explorer yet; its objects now load when
  the editor gets focus, and only that database's tables are suggested.

## 0.3.0 - 2026-10-27

### Added

- SQL Server databases show **Programmability** (Stored Procedures, Functions, Database
  Triggers, Assemblies, Types, Rules, Defaults, Plan Guides, Sequences) and **Security**
  (Users, Roles, Schemas, Asymmetric Keys, Certificates, Symmetric Keys, Always Encrypted Keys,
  Database Audit Specifications, Security Policies) after their schemas. Entries show as
  `schema.name`; procedures and functions there can be edited, executed and scripted. A
  category the server or the login cannot read explains why instead of failing the refresh.
- Query scripts can be opened and saved as `.sql` files: **File > Open Query…** (Ctrl+O),
  **Save Query** (Ctrl+S) and **Save Query As…** (Ctrl+Shift+S), the Open and Save buttons of
  the toolbar, and Save / Save As in a tab's right-click menu. The dialogs start in the folder
  used last, or `Documents\Zephix`. A file tab is named after the file and shows its path on
  hover; opening a file that is already open switches to its tab.
- A tab whose script changed since it was opened, saved or generated shows `*` after its title.
  Closing it (its close button, Ctrl+W, Close Other Tabs or Close All Tabs) asks to **Save**,
  **Don't Save** or **Cancel**; a query that was never saved asks whether to save it to a
  file first. Tabs restored from an earlier version count as unsaved.
- Closing Zephix while queries have unsaved changes lists them and asks to **Save All** (a query
  without a file asks for a file name), **Don't Save** (they are dropped and not reopened next
  time) or **Cancel**.
- Folders in the explorer (Tables, Views, Procedures, …) have **Filter**: only names
  containing the text are listed, and the folder shows the filter. Folders, schemas and
  databases have **Refresh**, which reads that database again.

### Changed

- Development connections show a green **DEV** label, like STG and PROD.
- Tabs can no longer be dragged out of the main window into a floating window, which had no
  toolbar or explorer of its own. Tabs left in such a window by an earlier version return to the
  main area on start. Tabs can still be arranged side by side inside the window.
- New query tabs take the lowest free number, so numbering starts at Query 1 again once the
  numbered tabs are closed, also after a restart.

### Fixed

- With every tab closed the workspace was black; it now shows an empty page with New Query and
  Welcome.
- The close button of a tab no longer paints over the tab's border when hovered; the cross
  turns red instead.
- **View History** of a SQL Server Agent job failed with "Invalid column name 'retries'".
- Resizing the window no longer fights back: Zephix stopped shrinking a window it judged too
  tall for the screen, which undid snapping and dragging an edge.
- On monitors with different scale factors the window could open off screen and at twice its
  size, so its edges could not be reached. The window now opens centered on the primary
  monitor with its last size (or maximized); the position is no longer remembered. The bad
  position saved by earlier versions is removed on the first start.
- A window too large for the screen opens maximized and restores to a centered size whose
  title bar is on screen.

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
