//! Editing table data from the result grid.
//!
//! The UI collects [`PendingEdits`] against a result set that came from a single table with a
//! primary key ([`TableTarget`]). [`build_changes`] turns them into parameter-free DML
//! statements, each expected to touch exactly one row, which adapters apply in one transaction
//! (see `DatabaseAdapter::apply_changes`). A statement that touches no row means the row was
//! changed or deleted concurrently, and the whole transaction is rolled back.

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
    /// Primary key columns. Editing is only offered when this is not empty.
    pub primary_key: Vec<String>,
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

/// Why edits cannot be turned into statements.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The table has no primary key.
    #[error("the table has no primary key, so rows cannot be identified safely")]
    NoPrimaryKey,
    /// A primary key column is not part of the result.
    #[error("primary key column {0} is not in the result; include it in the SELECT to edit")]
    MissingKeyColumn(String),
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

/// Builds the statements for `edits` against rows of `rs` read from `target`.
pub fn build_changes(
    target: &TableTarget,
    rs: &ResultSet,
    edits: &PendingEdits,
) -> Result<Vec<Change>, EditError> {
    if target.primary_key.is_empty() {
        return Err(EditError::NoPrimaryKey);
    }
    let column_index = |name: &str| {
        rs.columns.iter().position(|c| c.name == name).or_else(|| {
            rs.columns
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(name))
        })
    };
    let keys: Vec<(String, usize)> = target
        .primary_key
        .iter()
        .map(|k| {
            column_index(k)
                .map(|i| (k.clone(), i))
                .ok_or_else(|| EditError::MissingKeyColumn(k.clone()))
        })
        .collect::<Result<_, _>>()?;
    let kind = target.kind;
    let table = target.qualified();
    let col = |i: usize| quote_identifier(kind, &rs.columns[i].name);

    let where_clause = |row: usize| -> Result<String, EditError> {
        let cells = rs.rows.get(row).ok_or(EditError::UnusableKey(row))?;
        let parts = keys
            .iter()
            .map(|(name, i)| match cells.get(*i) {
                Some(CellValue::Null | CellValue::Bytes(_)) | None => {
                    Err(EditError::UnusableKey(row))
                }
                Some(v) => Ok(format!(
                    "{} = {}",
                    quote_identifier(kind, name),
                    sql_literal(v, kind)
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(parts.join(" AND "))
    };

    let mut changes = Vec::new();
    for &row in &edits.deleted {
        changes.push(Change {
            sql: format!("DELETE FROM {table} WHERE {}", where_clause(row)?),
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
            sql: format!(
                "UPDATE {table} SET {} WHERE {}",
                sets.join(", "),
                where_clause(row)?
            ),
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
        }
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
