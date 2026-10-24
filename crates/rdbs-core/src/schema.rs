//! Database objects displayed in the explorer tree.

use serde::{Deserialize, Serialize};

/// Detail text adapters put on databases whose objects are not loaded yet (other databases
/// of a PostgreSQL or SQL Server instance). The explorer loads them on demand.
pub const NOT_LOADED: &str = "not connected";

/// Kind of a database object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectKind {
    /// A database (catalog).
    Database,
    /// A schema/namespace inside a database.
    Schema,
    /// A grouping folder such as "Tables" or "Views".
    Folder,
    /// A base table.
    Table,
    /// A view.
    View,
    /// A materialized view.
    MaterializedView,
    /// A scalar or table-valued function.
    Function,
    /// A stored procedure.
    Procedure,
    /// A table column.
    Column,
    /// An index.
    Index,
    /// A SQL Server Agent job (`msdb.dbo.sysjobs`). Read-only in the explorer.
    AgentJob,
    /// An informational leaf, such as why SQL Server Agent could not be read.
    Notice,
}

impl ObjectKind {
    /// Whether rows can be selected from objects of this kind.
    pub fn is_queryable(self) -> bool {
        matches!(
            self,
            ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
        )
    }
}

/// A node of the object tree. Children are loaded eagerly by the mock adapter; real adapters
/// will populate them lazily in Milestone 2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DbObject {
    /// Object name as reported by the server (unquoted).
    pub name: String,
    /// Object kind.
    pub kind: ObjectKind,
    /// Optional detail shown dimmed next to the name, e.g. a column type or row estimate.
    pub detail: Option<String>,
    /// Child objects.
    pub children: Vec<DbObject>,
}

impl DbObject {
    /// Creates a leaf object.
    pub fn new(name: impl Into<String>, kind: ObjectKind) -> Self {
        Self {
            name: name.into(),
            kind,
            detail: None,
            children: Vec::new(),
        }
    }

    /// Builder-style detail setter.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Builder-style children setter.
    #[must_use]
    pub fn with_children(mut self, children: Vec<DbObject>) -> Self {
        self.children = children;
        self
    }

    /// Returns `true` if this node or any descendant name contains `needle`
    /// (case-insensitive). `needle` must already be lowercase.
    pub fn matches_filter(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.name.to_lowercase().contains(needle)
            || self.children.iter().any(|c| c.matches_filter(needle))
    }
}

/// Location of one object, e.g. for fetching its DDL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectPath {
    /// Database (catalog), if any.
    pub database: Option<String>,
    /// Schema, if the engine has schemas.
    pub schema: Option<String>,
    /// Object name.
    pub name: String,
    /// Object kind.
    pub kind: ObjectKind,
}

/// Objects of one schema (or of a MySQL database), grouped the way the explorer shows them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SchemaContents {
    /// Base tables.
    pub tables: Vec<DbObject>,
    /// Views.
    pub views: Vec<DbObject>,
    /// Materialized views.
    pub materialized_views: Vec<DbObject>,
    /// Functions.
    pub functions: Vec<DbObject>,
    /// Stored procedures.
    pub procedures: Vec<DbObject>,
}

impl SchemaContents {
    /// Converts into explorer folders, omitting empty groups except "Tables".
    pub fn into_folders(self) -> Vec<DbObject> {
        let folder = |name: &str, items: Vec<DbObject>| {
            DbObject::new(name, ObjectKind::Folder).with_children(items)
        };
        let mut out = vec![folder("Tables", self.tables)];
        for (name, items) in [
            ("Views", self.views),
            ("Materialized Views", self.materialized_views),
            ("Functions", self.functions),
            ("Procedures", self.procedures),
        ] {
            if !items.is_empty() {
                out.push(folder(name, items));
            }
        }
        out
    }
}

/// Compact row-count estimate for the explorer, e.g. `~1.2M rows`. Negative values (unknown
/// statistics) yield `None`.
pub fn approx_rows_label(rows: i64) -> Option<String> {
    let n = u64::try_from(rows).ok()?;
    let (value, unit) = match n {
        0..1_000 => return Some(format!("~{n} rows")),
        1_000..1_000_000 => (n as f64 / 1e3, "K"),
        1_000_000..1_000_000_000 => (n as f64 / 1e6, "M"),
        _ => (n as f64 / 1e9, "B"),
    };
    let text = if value < 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };
    Some(format!("~{}{unit} rows", text.trim_end_matches(".0")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_labels_are_compact() {
        assert_eq!(approx_rows_label(-1), None);
        assert_eq!(approx_rows_label(42).as_deref(), Some("~42 rows"));
        assert_eq!(approx_rows_label(3_100).as_deref(), Some("~3.1K rows"));
        assert_eq!(approx_rows_label(640_000).as_deref(), Some("~640K rows"));
        assert_eq!(approx_rows_label(1_000_000).as_deref(), Some("~1M rows"));
        assert_eq!(approx_rows_label(21_400_000).as_deref(), Some("~21M rows"));
    }

    #[test]
    fn empty_groups_are_omitted_except_tables() {
        let folders = SchemaContents {
            views: vec![DbObject::new("v", ObjectKind::View)],
            ..SchemaContents::default()
        }
        .into_folders();
        let names: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Tables", "Views"]);
    }

    #[test]
    fn filter_matches_descendants() {
        let tree = DbObject::new("public", ObjectKind::Schema).with_children(vec![
            DbObject::new("Tables", ObjectKind::Folder)
                .with_children(vec![DbObject::new("Customers", ObjectKind::Table)]),
        ]);
        assert!(tree.matches_filter("cust"));
        assert!(tree.matches_filter(""));
        assert!(!tree.matches_filter("orders"));
    }
}
