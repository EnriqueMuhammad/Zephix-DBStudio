//! Editing table data from the result grid.
//!
//! The UI collects [`PendingEdits`] against a result set that came from a single table
//! ([`TableTarget`]). [`build_changes`] turns them into parameter-free DML statements, each
//! expected to touch exactly one row, which adapters apply in one transaction (see
//! `DatabaseAdapter::apply_changes`). A statement that touches no row means the row was changed
//! or deleted concurrently, and the whole transaction is rolled back.
//!
//! Rows are found by their primary key. A table without one is matched by the values of all
//! its comparable columns ([`RowMatch::AllColumns`]); when the grid shows identical rows, an
//! `UPDATE` / `DELETE` changes only one of them (`TOP (1)`, `LIMIT 1`, `ROWNUM = 1`, or by
//! `ctid` / `rowid`), since such rows cannot be told apart anyway.

use std::collections::{BTreeMap, BTreeSet};

use crate::dialect::{qualified_name, quote_identifier};
use crate::export::{sql_literal, sql_string};
use crate::profile::DatabaseKind;
use crate::value::{CellValue, ResultSet};

/// The table a result set was read from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TableTarget {
    /// Engine (for quoting and literals).
    pub kind: DatabaseKind,
    /// Database, used in qualified names where idiomatic.
    pub database: Option<String>,
    /// Schema, if any.
    pub schema: Option<String>,
    /// Table name.
    pub table: String,
    /// Primary key columns; rows are found by these when there are any.
    pub primary_key: Vec<String>,
    /// All columns of the table in ordinal order (empty when unknown). Rows of a table without
    /// a primary key are found by the values of these.
    #[serde(default)]
    pub columns: Vec<String>,
}

impl TableTarget {
    /// Quoted, qualified table name.
    pub fn qualified(&self) -> String {
        qualified_name(
            self.kind,
            self.database.as_deref(),
            self.schema.as_deref(),
            &self.table,
        )
    }
}

/// A value typed into the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditValue {
    /// SQL NULL.
    Null,
    /// Text as typed; converted to a literal matching the column.
    Text(String),
    /// The column default (new rows only).
    Default,
}

impl EditValue {
    /// Display text in the grid.
    pub fn display(&self) -> &str {
        match self {
            EditValue::Null => "NULL",
            EditValue::Text(t) => t,
            EditValue::Default => "(default)",
        }
    }
}

/// Changes not yet written to the database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingEdits {
    /// Row index → column index → new value.
    pub updates: BTreeMap<usize, BTreeMap<usize, EditValue>>,
    /// Indices of rows to delete.
    pub deleted: BTreeSet<usize>,
    /// New rows, one value per column.
    pub inserted: Vec<Vec<EditValue>>,
}

impl PendingEdits {
    /// Whether nothing is pending.
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.deleted.is_empty() && self.inserted.is_empty()
    }

    /// Number of rows that will change.
    pub fn row_count(&self) -> usize {
        self.updates
            .keys()
            .filter(|r| !self.deleted.contains(r))
            .count()
            + self.deleted.len()
            + self.inserted.len()
    }

    /// Sets a cell of an existing row; an edit back to the original value is dropped.
    pub fn set_cell(&mut self, row: usize, col: usize, value: EditValue, original: &CellValue) {
        let unchanged = match (&value, original) {
            (EditValue::Null, CellValue::Null) => true,
            (EditValue::Text(t), o) if !o.is_null() => *t == o.to_string(),
            _ => false,
        };
        let row_edits = self.updates.entry(row).or_default();
        if unchanged {
            row_edits.remove(&col);
        } else {
            row_edits.insert(col, value);
        }
        if row_edits.is_empty() {
            self.updates.remove(&row);
        }
    }

    /// Pending value of a cell of an existing row.
    pub fn cell(&self, row: usize, col: usize) -> Option<&EditValue> {
        self.updates.get(&row).and_then(|r| r.get(&col))
    }

    /// Marks a row for deletion, or unmarks it.
    pub fn toggle_delete(&mut self, row: usize) {
        if !self.deleted.remove(&row) {
            self.deleted.insert(row);
        }
    }

    /// Appends a new row whose columns use their defaults.
    pub fn add_row(&mut self, columns: usize) {
        self.inserted.push(vec![EditValue::Default; columns]);
    }
}

/// One statement to apply and the number of rows it must affect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The DML statement.
    pub sql: String,
    /// Required affected-row count (1 for all generated statements, [`Change::ANY`] for
    /// statements whose count is not checked, such as DDL).
    pub expected_rows: u64,
}

impl Change {
    /// `expected_rows` value that accepts any affected-row count.
    pub const ANY: u64 = u64::MAX;

    /// A statement whose affected-row count is not checked.
    pub fn unchecked(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            expected_rows: Self::ANY,
        }
    }

    /// Whether `affected` rows satisfy this change.
    pub fn accepts(&self, affected: u64) -> bool {
        self.expected_rows == Self::ANY || affected == self.expected_rows
    }
}

/// How rows of a result are identified in `UPDATE` / `DELETE` statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowMatch {
    /// By the primary key.
    PrimaryKey,
    /// By the values of all comparable columns (the table has no primary key).
    AllColumns,
}

/// Why edits cannot be turned into statements.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The table has no primary key and its columns are not known.
    #[error("the table has no primary key, so rows cannot be identified safely")]
    NoPrimaryKey,
    /// A primary key column is not part of the result.
    #[error("primary key column {0} is not in the result; include it in the SELECT to edit")]
    MissingKeyColumn(String),
    /// A column of a table without a primary key is not part of the result.
    #[error(
        "column {0} is not in the result; a table without a primary key is edited with all its \
         columns selected"
    )]
    MissingColumn(String),
    /// No column of a table without a primary key can be compared in a `WHERE` clause.
    #[error("the table has no primary key and none of its columns can be compared")]
    NoComparableColumn,
    /// A primary key value of a row is NULL or binary.
    #[error("row {0} has an unusable primary key value")]
    UnusableKey(usize),
}

/// Literal for a typed value, using the original cell (if any) to pick the literal form.
fn edit_literal(kind: DatabaseKind, value: &EditValue, hint: Option<&CellValue>) -> String {
    match value {
        EditValue::Null => "NULL".to_owned(),
        EditValue::Default => "DEFAULT".to_owned(),
        EditValue::Text(text) => match hint {
            Some(CellValue::Int(_) | CellValue::Float(_) | CellValue::Decimal(_))
                if text.trim().parse::<f64>().is_ok() =>
            {
                text.trim().to_owned()
            }
            Some(CellValue::Bool(_)) => match text.trim().to_ascii_lowercase().as_str() {
                "true" | "t" | "1" | "yes" => sql_literal(&CellValue::Bool(true), kind),
                "false" | "f" | "0" | "no" => sql_literal(&CellValue::Bool(false), kind),
                _ => sql_string(text, kind),
            },
            _ => sql_string(text, kind),
        },
    }
}

/// Columns that identify a row, by result column index.
struct RowKey {
    mode: RowMatch,
    columns: Vec<usize>,
    /// Whether the key covers every column, so rows equal on it are indistinguishable.
    complete: bool,
}

/// Base type name in lower case: `nvarchar(50)` → `nvarchar`.
fn base_type(type_name: &str) -> String {
    type_name
        .split('(')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// Whether `=` works on a column of this type (large objects, XML, JSON and spatial types
/// cannot be compared, or not reliably).
fn comparable_type(kind: DatabaseKind, type_name: &str) -> bool {
    let excluded: &[&str] = match kind {
        DatabaseKind::SqlServer => &[
            "text",
            "ntext",
            "image",
            "xml",
            "udt",
            "sql_variant",
            "geography",
            "geometry",
        ],
        DatabaseKind::PostgreSql => &[
            "json", "xml", "point", "line", "lseg", "box", "path", "polygon", "circle",
        ],
        DatabaseKind::MySql | DatabaseKind::MariaDb => &["json", "geometry"],
        DatabaseKind::Oracle => &[
            "clob", "nclob", "blob", "bfile", "long", "long raw", "xmltype",
        ],
        DatabaseKind::Sqlite | DatabaseKind::MongoDb | DatabaseKind::Redis => &[],
    };
    !excluded.contains(&base_type(type_name).as_str())
}

fn row_key(target: &TableTarget, rs: &ResultSet) -> Result<RowKey, EditError> {
    let column_index = |name: &str| {
        rs.columns.iter().position(|c| c.name == name).or_else(|| {
            rs.columns
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(name))
        })
    };
    if !target.primary_key.is_empty() {
        let columns = target
            .primary_key
            .iter()
            .map(|k| column_index(k).ok_or_else(|| EditError::MissingKeyColumn(k.clone())))
            .collect::<Result<_, _>>()?;
        return Ok(RowKey {
            mode: RowMatch::PrimaryKey,
            columns,
            complete: true,
        });
    }
    if target.columns.is_empty() {
        return Err(EditError::NoPrimaryKey);
    }
    let mut columns = Vec::new();
    let mut complete = true;
    for name in &target.columns {
        let i = column_index(name).ok_or_else(|| EditError::MissingColumn(name.clone()))?;
        let binary = rs
            .rows
            .iter()
            .any(|r| matches!(r.get(i), Some(CellValue::Bytes(_))));
        if !binary && comparable_type(target.kind, &rs.columns[i].type_name) {
            columns.push(i);
        } else {
            complete = false;
        }
    }
    if columns.is_empty() {
        return Err(EditError::NoComparableColumn);
    }
    Ok(RowKey {
        mode: RowMatch::AllColumns,
        columns,
        complete,
    })
}

/// How rows of `rs` (read from `target`) are identified, or why they cannot be edited.
pub fn row_match(target: &TableTarget, rs: &ResultSet) -> Result<RowMatch, EditError> {
    row_key(target, rs).map(|k| k.mode)
}

/// SQL Server literal for a date/time read back from the server, for comparisons. `datetime`
/// stores 1/300 s ticks that are read back with more digits than its literals accept, so they
/// are rounded to milliseconds; other types keep at most the 7 digits they store. `datetime`
/// and `smalldatetime` use the ISO 8601 `T` form, which does not depend on `DATEFORMAT`.
fn mssql_temporal(text: &str, type_name: &str) -> String {
    let legacy = matches!(base_type(type_name).as_str(), "datetime" | "smalldatetime");
    let mut out = text.to_owned();
    if let Some(dot) = text.find('.') {
        let after = &text[dot + 1..];
        let len = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        let (digits, rest) = after.split_at(len);
        let digits = if legacy && !digits.is_empty() {
            let nanos: u64 = format!("{digits:0<9}")[..9].parse().unwrap_or(0);
            format!("{:03}", ((nanos + 500_000) / 1_000_000).min(999))
        } else {
            digits.trim_end_matches('0').to_owned()
        };
        out = text[..dot].to_owned();
        if !digits.is_empty() {
            out.push('.');
            out.push_str(&digits);
        }
        out.push_str(rest);
    }
    if legacy && out.as_bytes().get(10) == Some(&b' ') {
        out.replace_range(10..11, "T");
    }
    sql_string(&out, DatabaseKind::SqlServer)
}

/// Literal of a value read from the server, for a `WHERE` comparison with its column.
fn key_literal(kind: DatabaseKind, type_name: &str, value: &CellValue) -> String {
    match value {
        CellValue::Temporal(t) if kind == DatabaseKind::SqlServer => mssql_temporal(t, type_name),
        other => sql_literal(other, kind),
    }
}

/// Builds the statements for `edits` against rows of `rs` read from `target`.
pub fn build_changes(
    target: &TableTarget,
    rs: &ResultSet,
    edits: &PendingEdits,
) -> Result<Vec<Change>, EditError> {
    let key = row_key(target, rs)?;
    let kind = target.kind;
    let table = target.qualified();
    let col = |i: usize| quote_identifier(kind, &rs.columns[i].name);

    let where_clause = |row: usize| -> Result<String, EditError> {
        let cells = rs.rows.get(row).ok_or(EditError::UnusableKey(row))?;
        let parts = key
            .columns
            .iter()
            .map(|&i| match (cells.get(i), key.mode) {
                (None, _) | (Some(CellValue::Null | CellValue::Bytes(_)), RowMatch::PrimaryKey) => {
                    Err(EditError::UnusableKey(row))
                }
                (Some(CellValue::Null), _) => Ok(format!("{} IS NULL", col(i))),
                (Some(v), _) => Ok(format!(
                    "{} = {}",
                    col(i),
                    key_literal(kind, &rs.columns[i].type_name, v)
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(parts.join(" AND "))
    };
    // Rows identical on every column cannot be told apart: change just one of them.
    let one_of_several = |row: usize| {
        key.mode == RowMatch::AllColumns
            && key.complete
            && rs.rows.get(row).is_some_and(|cells| {
                rs.rows.iter().enumerate().any(|(j, other)| {
                    j != row && key.columns.iter().all(|&i| other.get(i) == cells.get(i))
                })
            })
    };
    // `UPDATE` (with `set` = ` SET …`) or `DELETE` (with an empty `set`) of one row.
    let statement = |set: &str, row: usize| -> Result<String, EditError> {
        let filter = where_clause(row)?;
        let verb = if set.is_empty() {
            "DELETE FROM"
        } else {
            "UPDATE"
        };
        if !one_of_several(row) {
            return Ok(format!("{verb} {table}{set} WHERE {filter}"));
        }
        Ok(match kind {
            DatabaseKind::SqlServer => {
                let verb = if set.is_empty() {
                    "DELETE TOP (1) FROM"
                } else {
                    "UPDATE TOP (1)"
                };
                format!("{verb} {table}{set} WHERE {filter}")
            }
            DatabaseKind::MySql | DatabaseKind::MariaDb => {
                format!("{verb} {table}{set} WHERE {filter} LIMIT 1")
            }
            DatabaseKind::Oracle => format!("{verb} {table}{set} WHERE {filter} AND ROWNUM = 1"),
            DatabaseKind::PostgreSql => format!(
                "{verb} {table}{set} WHERE {filter} AND ctid = \
                 (SELECT ctid FROM {table} WHERE {filter} LIMIT 1)"
            ),
            DatabaseKind::Sqlite => format!(
                "{verb} {table}{set} WHERE {filter} AND rowid = \
                 (SELECT rowid FROM {table} WHERE {filter} LIMIT 1)"
            ),
            DatabaseKind::MongoDb | DatabaseKind::Redis => {
                format!("{verb} {table}{set} WHERE {filter}")
            }
        })
    };

    let mut changes = Vec::new();
    for &row in &edits.deleted {
        changes.push(Change {
            sql: statement("", row)?,
            expected_rows: 1,
        });
    }
    for (&row, cols) in &edits.updates {
        if edits.deleted.contains(&row) || cols.is_empty() {
            continue;
        }
        let original = rs.rows.get(row);
        let sets: Vec<String> = cols
            .iter()
            .map(|(&c, v)| {
                let hint = original.and_then(|r| r.get(c));
                format!("{} = {}", col(c), edit_literal(kind, v, hint))
            })
            .collect();
        changes.push(Change {
            sql: statement(&format!(" SET {}", sets.join(", ")), row)?,
            expected_rows: 1,
        });
    }
    // A sample value per column (first non-null) chooses literal forms for new rows.
    let hints: Vec<Option<&CellValue>> = (0..rs.columns.len())
        .map(|c| {
            rs.rows
                .iter()
                .filter_map(|r| r.get(c))
                .find(|v| !v.is_null())
        })
        .collect();
    for new_row in &edits.inserted {
        let given: Vec<(usize, &EditValue)> = new_row
            .iter()
            .enumerate()
            .filter(|(_, v)| **v != EditValue::Default)
            .collect();
        let sql = if given.is_empty() {
            match kind {
                DatabaseKind::MySql | DatabaseKind::MariaDb => {
                    format!("INSERT INTO {table} () VALUES ()")
                }
                _ => format!("INSERT INTO {table} DEFAULT VALUES"),
            }
        } else {
            let cols: Vec<String> = given.iter().map(|(c, _)| col(*c)).collect();
            let values: Vec<String> = given
                .iter()
                .map(|(c, v)| edit_literal(kind, v, hints.get(*c).copied().flatten()))
                .collect();
            format!(
                "INSERT INTO {table} ({}) VALUES ({})",
                cols.join(", "),
                values.join(", ")
            )
        };
        changes.push(Change {
            sql,
            expected_rows: 1,
        });
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::ColumnMeta;

    fn rs() -> ResultSet {
        ResultSet {
            columns: vec![
                ColumnMeta::new("id", "int4"),
                ColumnMeta::new("name", "text"),
                ColumnMeta::new("active", "bool"),
                ColumnMeta::new("balance", "numeric"),
            ],
            rows: vec![
                vec![
                    CellValue::Int(1),
                    CellValue::Text("Ann".into()),
                    CellValue::Bool(true),
                    CellValue::Decimal("10.50".into()),
                ],
                vec![
                    CellValue::Int(2),
                    CellValue::Null,
                    CellValue::Bool(false),
                    CellValue::Null,
                ],
            ],
        }
    }

    fn target(kind: DatabaseKind) -> TableTarget {
        TableTarget {
            kind,
            database: Some("shop".into()),
            schema: Some("public".into()),
            table: "people".into(),
            primary_key: vec!["id".into()],
            columns: vec![
                "id".into(),
                "name".into(),
                "active".into(),
                "balance".into(),
            ],
        }
    }

    /// A table without a primary key, like a shift schedule: rows 0 and 2 are identical.
    fn schedule() -> (TableTarget, ResultSet) {
        let target = TableTarget {
            kind: DatabaseKind::SqlServer,
            database: None,
            schema: Some("dbo".into()),
            table: "schedule".into(),
            primary_key: Vec::new(),
            columns: vec!["code".into(), "starts".into(), "note".into()],
        };
        let row = |code: &str, starts: &str, note: Option<&str>| {
            vec![
                CellValue::Text(code.into()),
                CellValue::Temporal(starts.into()),
                note.map_or(CellValue::Null, |n| CellValue::Text(n.into())),
            ]
        };
        let rs = ResultSet {
            columns: vec![
                ColumnMeta::new("code", "varchar"),
                ColumnMeta::new("starts", "datetime"),
                ColumnMeta::new("note", "nvarchar"),
            ],
            rows: vec![
                row("0700-1500", "1900-01-01 07:00:00", None),
                row("test", "1900-01-01 15:00:00.996666666", Some("x")),
                row("0700-1500", "1900-01-01 07:00:00", None),
            ],
        };
        (target, rs)
    }

    #[test]
    fn rows_without_primary_key_match_all_columns() {
        let (target, data) = schedule();
        assert_eq!(row_match(&target, &data), Ok(RowMatch::AllColumns));
        let mut e = PendingEdits::default();
        e.set_cell(1, 2, EditValue::Text("y".into()), &data.rows[1][2]);
        let changes = build_changes(&target, &data, &e).unwrap();
        assert_eq!(
            changes[0].sql,
            "UPDATE [dbo].[schedule] SET [note] = N'y' WHERE [code] = N'test' \
             AND [starts] = N'1900-01-01T15:00:00.997' AND [note] = N'x'"
        );
        assert_eq!(changes[0].expected_rows, 1);
    }

    #[test]
    fn identical_rows_change_one_of_them() {
        let (mut target, data) = schedule();
        let mut e = PendingEdits::default();
        e.set_cell(0, 2, EditValue::Text("z".into()), &data.rows[0][2]);
        e.toggle_delete(2);
        let filter = "[code] = N'0700-1500' AND [starts] = N'1900-01-01T07:00:00' \
                      AND [note] IS NULL";
        let sql: Vec<String> = build_changes(&target, &data, &e)
            .unwrap()
            .into_iter()
            .map(|c| c.sql)
            .collect();
        assert_eq!(
            sql,
            [
                format!("DELETE TOP (1) FROM [dbo].[schedule] WHERE {filter}"),
                format!("UPDATE TOP (1) [dbo].[schedule] SET [note] = N'z' WHERE {filter}"),
            ]
        );

        let mut one = |kind| {
            target.kind = kind;
            let mut e = PendingEdits::default();
            e.toggle_delete(0);
            build_changes(&target, &data, &e).unwrap().remove(0).sql
        };
        assert!(one(DatabaseKind::MySql).ends_with(" LIMIT 1"));
        assert!(one(DatabaseKind::Oracle).ends_with(" AND ROWNUM = 1"));
        let pg = one(DatabaseKind::PostgreSql);
        assert!(
            pg.starts_with("DELETE FROM \"dbo\".\"schedule\" WHERE \"code\" = '0700-1500'")
                && pg.ends_with(" AND \"note\" IS NULL LIMIT 1)")
                && pg.contains(" AND ctid = (SELECT ctid FROM \"dbo\".\"schedule\" WHERE "),
            "{pg}"
        );
        assert!(one(DatabaseKind::Sqlite).contains(" AND rowid = (SELECT rowid FROM "));
    }

    #[test]
    fn incomparable_columns_are_left_out_without_limiting() {
        let (target, mut data) = schedule();
        data.columns[2].type_name = "xml".into();
        let mut e = PendingEdits::default();
        e.toggle_delete(0);
        assert_eq!(
            build_changes(&target, &data, &e).unwrap()[0].sql,
            "DELETE FROM [dbo].[schedule] WHERE [code] = N'0700-1500' \
             AND [starts] = N'1900-01-01T07:00:00'"
        );

        data.columns[0].type_name = "text".into();
        data.columns[1].type_name = "image".into();
        assert_eq!(
            row_match(&target, &data),
            Err(EditError::NoComparableColumn)
        );
    }

    #[test]
    fn tables_without_primary_key_need_all_columns() {
        let (mut target, mut data) = schedule();
        data.columns.truncate(2);
        assert_eq!(
            row_match(&target, &data),
            Err(EditError::MissingColumn("note".into()))
        );
        target.columns.clear();
        assert_eq!(row_match(&target, &data), Err(EditError::NoPrimaryKey));
    }

    #[test]
    fn sql_server_temporal_literals_round_trip() {
        assert_eq!(
            mssql_temporal("2026-10-29 07:00:00.996666666", "datetime"),
            "N'2026-10-29T07:00:00.997'"
        );
        assert_eq!(
            mssql_temporal("2026-10-29 07:00:00", "smalldatetime"),
            "N'2026-10-29T07:00:00'"
        );
        assert_eq!(
            mssql_temporal("2026-10-29 07:00:00.123456700", "datetime2"),
            "N'2026-10-29 07:00:00.1234567'"
        );
        assert_eq!(
            mssql_temporal("2026-10-29 07:00:00.500 +07:00", "datetimeoffset"),
            "N'2026-10-29 07:00:00.5 +07:00'"
        );
        assert_eq!(mssql_temporal("07:30:00", "time"), "N'07:30:00'");
    }

    #[test]
    fn edits_back_to_original_are_dropped() {
        let data = rs();
        let mut e = PendingEdits::default();
        e.set_cell(0, 1, EditValue::Text("Bob".into()), &data.rows[0][1]);
        assert_eq!(e.row_count(), 1);
        e.set_cell(0, 1, EditValue::Text("Ann".into()), &data.rows[0][1]);
        assert!(e.is_empty());
        e.set_cell(1, 1, EditValue::Null, &data.rows[1][1]);
        assert!(e.is_empty());
    }

    #[test]
    fn builds_update_delete_insert_for_postgres() {
        let data = rs();
        let mut e = PendingEdits::default();
        e.set_cell(0, 1, EditValue::Text("O'Neil".into()), &data.rows[0][1]);
        e.set_cell(0, 2, EditValue::Text("no".into()), &data.rows[0][2]);
        e.set_cell(0, 3, EditValue::Text("12".into()), &data.rows[0][3]);
        e.toggle_delete(1);
        e.add_row(4);
        e.inserted[0][1] = EditValue::Text("New".into());
        e.inserted[0][3] = EditValue::Null;
        e.add_row(4);

        let changes = build_changes(&target(DatabaseKind::PostgreSql), &data, &e).unwrap();
        let sql: Vec<&str> = changes.iter().map(|c| c.sql.as_str()).collect();
        assert_eq!(
            sql,
            [
                "DELETE FROM \"public\".\"people\" WHERE \"id\" = 2",
                "UPDATE \"public\".\"people\" SET \"name\" = 'O''Neil', \"active\" = FALSE, \"balance\" = 12 WHERE \"id\" = 1",
                "INSERT INTO \"public\".\"people\" (\"name\", \"balance\") VALUES ('New', NULL)",
                "INSERT INTO \"public\".\"people\" DEFAULT VALUES",
            ]
        );
        assert!(changes.iter().all(|c| c.expected_rows == 1));
    }

    #[test]
    fn dialects_quote_and_literal_differently() {
        let data = rs();
        let mut e = PendingEdits::default();
        e.set_cell(0, 2, EditValue::Text("false".into()), &data.rows[0][2]);
        e.add_row(4);
        let ms = build_changes(&target(DatabaseKind::SqlServer), &data, &e).unwrap();
        assert_eq!(
            ms[0].sql,
            "UPDATE [shop].[public].[people] SET [active] = 0 WHERE [id] = 1"
        );
        assert_eq!(
            ms[1].sql,
            "INSERT INTO [shop].[public].[people] DEFAULT VALUES"
        );
        let my = build_changes(&target(DatabaseKind::MySql), &data, &e).unwrap();
        assert_eq!(my[1].sql, "INSERT INTO `shop`.`people` () VALUES ()");
    }

    #[test]
    fn refuses_without_usable_key() {
        let data = rs();
        let mut no_pk = target(DatabaseKind::PostgreSql);
        no_pk.primary_key.clear();
        no_pk.columns.clear();
        let e = PendingEdits::default();
        assert_eq!(
            build_changes(&no_pk, &data, &e),
            Err(EditError::NoPrimaryKey)
        );

        let mut other = target(DatabaseKind::PostgreSql);
        other.primary_key = vec!["uuid".into()];
        assert_eq!(
            build_changes(&other, &data, &e),
            Err(EditError::MissingKeyColumn("uuid".into()))
        );

        let mut nullable = target(DatabaseKind::PostgreSql);
        nullable.primary_key = vec!["name".into()];
        let mut e = PendingEdits::default();
        e.toggle_delete(1);
        assert_eq!(
            build_changes(&nullable, &data, &e),
            Err(EditError::UnusableKey(1))
        );
    }
}
