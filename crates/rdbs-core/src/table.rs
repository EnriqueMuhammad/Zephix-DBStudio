//! Structured table definitions (columns, keys, indexes, foreign keys).
//!
//! Adapters fill [`TableSchema`] from their catalogs (`DatabaseAdapter::describe_schema`); the
//! table designer edits it, schema diff compares two of them, the ER diagram draws them and data
//! transfer recreates them on another server ([`crate::ddlgen`]).

use serde::{Deserialize, Serialize};

/// One column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDef {
    /// Column name.
    pub name: String,
    /// Native type as written in DDL, e.g. `varchar(100)`, `numeric(12,2)`, `INT UNSIGNED`.
    pub data_type: String,
    /// Whether NULL is allowed.
    pub nullable: bool,
    /// Default expression exactly as SQL (literals quoted), if any.
    pub default: Option<String>,
    /// Auto-generated value (identity / serial / auto_increment).
    pub identity: bool,
}

impl ColumnDef {
    /// A nullable column without default.
    pub fn new(name: impl Into<String>, data_type: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            data_type: data_type.into(),
            nullable: true,
            default: None,
            identity: false,
        }
    }
}

/// A secondary index (unique constraints are represented as unique indexes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDef {
    /// Index name.
    pub name: String,
    /// Indexed columns in order.
    pub columns: Vec<String>,
    /// Whether values must be unique.
    pub unique: bool,
    /// Backs a `UNIQUE` constraint (created/dropped as a constraint, not with `CREATE INDEX`).
    #[serde(default)]
    pub constraint: bool,
}

impl IndexDef {
    /// A plain (non-constraint) index.
    pub fn new(name: impl Into<String>, columns: Vec<String>, unique: bool) -> Self {
        Self {
            name: name.into(),
            columns,
            unique,
            constraint: false,
        }
    }
}

/// A foreign key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKeyDef {
    /// Constraint name.
    pub name: String,
    /// Referencing columns.
    pub columns: Vec<String>,
    /// Schema of the referenced table (None = same schema / MySQL database).
    pub ref_schema: Option<String>,
    /// Referenced table.
    pub ref_table: String,
    /// Referenced columns, aligned with `columns`.
    pub ref_columns: Vec<String>,
}

/// Full definition of one table.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TableSchema {
    /// Schema (PostgreSQL / SQL Server) — `None` for MySQL and SQLite.
    pub schema: Option<String>,
    /// Table name.
    pub name: String,
    /// Columns in ordinal order.
    pub columns: Vec<ColumnDef>,
    /// Primary key columns in key order (empty = no primary key).
    pub primary_key: Vec<String>,
    /// Name of the primary key constraint, when the engine names it.
    pub primary_key_name: Option<String>,
    /// Secondary indexes.
    pub indexes: Vec<IndexDef>,
    /// Foreign keys.
    pub foreign_keys: Vec<ForeignKeyDef>,
}

impl TableSchema {
    /// Column by name (exact match).
    pub fn column(&self, name: &str) -> Option<&ColumnDef> {
        self.columns.iter().find(|c| c.name == name)
    }
}

/// Groups rows of `(table, …)` into per-table vectors while keeping first-seen table order.
pub fn group_by_table<T>(rows: impl IntoIterator<Item = (String, T)>) -> Vec<(String, Vec<T>)> {
    let mut out: Vec<(String, Vec<T>)> = Vec::new();
    for (table, item) in rows {
        match out.iter_mut().find(|(t, _)| *t == table) {
            Some((_, items)) => items.push(item),
            None => out.push((table, vec![item])),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_preserving_order() {
        let grouped = group_by_table(vec![
            ("b".to_owned(), 1),
            ("a".to_owned(), 2),
            ("b".to_owned(), 3),
        ]);
        assert_eq!(
            grouped,
            vec![("b".into(), vec![1, 3]), ("a".into(), vec![2])]
        );
    }
}
