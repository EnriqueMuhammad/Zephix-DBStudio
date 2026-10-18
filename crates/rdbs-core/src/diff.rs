//! Schema comparison: which tables differ between a source and a target, and the script that
//! makes the target match the source (possibly on another engine).

use crate::ddlgen::{alter_table, create_table, drop_table};
use crate::dialect::qualified_name;
use crate::profile::DatabaseKind;
use crate::table::{ForeignKeyDef, TableSchema};
use crate::types::map_type;

/// How a table differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffStatus {
    /// Only in the source: create it in the target.
    Added,
    /// Only in the target: drop it (opt-in).
    Removed,
    /// In both, with differences.
    Changed,
    /// In both, identical.
    Identical,
}

impl DiffStatus {
    /// Label for lists.
    pub fn label(self) -> &'static str {
        match self {
            DiffStatus::Added => "Only in source",
            DiffStatus::Removed => "Only in target",
            DiffStatus::Changed => "Different",
            DiffStatus::Identical => "Identical",
        }
    }
}

/// Comparison result for one table.
#[derive(Debug, Clone, PartialEq)]
pub struct TableDiff {
    /// Table name.
    pub name: String,
    /// Kind of difference.
    pub status: DiffStatus,
    /// Source definition, converted to the target engine and schema.
    pub source: Option<TableSchema>,
    /// Target definition.
    pub target: Option<TableSchema>,
    /// Statements for this table.
    pub statements: Vec<String>,
    /// Statements that must run after all tables exist (foreign keys of new tables).
    pub deferred: Vec<String>,
    /// Why no script could be generated, if so.
    pub error: Option<String>,
}

/// Converts a table definition to another engine and schema: types are mapped and defaults
/// that are not plain literals are dropped when the engines differ.
pub fn convert_table(
    from: DatabaseKind,
    table: &TableSchema,
    to: DatabaseKind,
    schema: Option<&str>,
) -> TableSchema {
    let same = family(from) == family(to);
    let mut out = table.clone();
    out.schema = schema.map(str::to_owned);
    for c in &mut out.columns {
        c.data_type = map_type(from, &c.data_type, to);
        if !same {
            c.default = c
                .default
                .take()
                .map(|d| crate::types::strip_default_decoration(&d))
                .filter(|d| is_portable_literal(d));
        }
    }
    if !same {
        // Constraint names may clash across engines' naming rules; keep them, they are plain.
        out.primary_key_name = out
            .primary_key_name
            .take()
            .or_else(|| (!out.primary_key.is_empty()).then(|| format!("pk_{}", out.name)));
    }
    for fk in &mut out.foreign_keys {
        if fk.ref_schema.as_deref() == table.schema.as_deref() {
            fk.ref_schema = None;
        }
    }
    out
}

fn family(kind: DatabaseKind) -> DatabaseKind {
    match kind {
        DatabaseKind::MariaDb => DatabaseKind::MySql,
        other => other,
    }
}

/// Numbers and simple quoted strings work on every engine.
fn is_portable_literal(e: &str) -> bool {
    e.parse::<f64>().is_ok()
        || (e.starts_with('\'')
            && e.ends_with('\'')
            && e.len() >= 2
            && !e[1..e.len() - 1].contains('\''))
        || e.eq_ignore_ascii_case("null")
}

/// Compares `source` tables (of `source_kind`) with `target` tables (of `target_kind`, in
/// `target_schema`). Results are sorted by name.
pub fn compare(
    source_kind: DatabaseKind,
    source: &[TableSchema],
    target_kind: DatabaseKind,
    target: &[TableSchema],
    target_schema: Option<&str>,
) -> Vec<TableDiff> {
    let find = |list: &[TableSchema], name: &str| {
        list.iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .cloned()
    };
    let mut out = Vec::new();
    for s in source {
        let converted = convert_table(source_kind, s, target_kind, target_schema);
        match find(target, &s.name) {
            None => {
                let without_fks = TableSchema {
                    foreign_keys: Vec::new(),
                    ..converted.clone()
                };
                let deferred = converted
                    .foreign_keys
                    .iter()
                    .map(|fk| add_foreign_key(target_kind, &converted, fk))
                    .collect();
                out.push(TableDiff {
                    name: s.name.clone(),
                    status: DiffStatus::Added,
                    statements: create_table(target_kind, &without_fks),
                    deferred,
                    source: Some(converted),
                    target: None,
                    error: None,
                });
            }
            Some(t) => {
                // Compare against the target's own name (case may differ).
                let wanted = TableSchema {
                    name: t.name.clone(),
                    primary_key_name: t
                        .primary_key_name
                        .clone()
                        .or(converted.primary_key_name.clone()),
                    ..converted.clone()
                };
                let (statements, error) = match alter_table(target_kind, &t, &wanted, &[]) {
                    Ok(s) => (s, None),
                    Err(e) => (Vec::new(), Some(e.0)),
                };
                let status = if statements.is_empty() && error.is_none() {
                    DiffStatus::Identical
                } else {
                    DiffStatus::Changed
                };
                out.push(TableDiff {
                    name: s.name.clone(),
                    status,
                    source: Some(wanted),
                    target: Some(t),
                    statements,
                    deferred: Vec::new(),
                    error,
                });
            }
        }
    }
    for t in target {
        if find(source, &t.name).is_none() {
            out.push(TableDiff {
                name: t.name.clone(),
                status: DiffStatus::Removed,
                source: None,
                target: Some(t.clone()),
                statements: vec![drop_table(target_kind, t)],
                deferred: Vec::new(),
                error: None,
            });
        }
    }
    out.sort_by_key(|d| d.name.to_lowercase());
    out
}

fn add_foreign_key(kind: DatabaseKind, t: &TableSchema, fk: &ForeignKeyDef) -> String {
    // Reuse create_table's constraint syntax by rendering a one-FK table and extracting it.
    let q = |s: &str| crate::dialect::quote_identifier(kind, s);
    let cols = |c: &[String]| c.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
    format!(
        "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
        qualified_name(kind, None, t.schema.as_deref(), &t.name),
        q(&fk.name),
        cols(&fk.columns),
        qualified_name(
            kind,
            None,
            fk.ref_schema.as_deref().or(t.schema.as_deref()),
            &fk.ref_table
        ),
        cols(&fk.ref_columns)
    )
}

/// The full migration for the selected diffs, in an order that respects dependencies:
/// new tables (without foreign keys), changes to existing tables, foreign keys of new tables,
/// then drops (referencing tables first).
pub fn migration_script(diffs: &[&TableDiff]) -> Vec<String> {
    let mut out = Vec::new();
    for d in diffs.iter().filter(|d| d.status == DiffStatus::Added) {
        out.extend(d.statements.iter().cloned());
    }
    for d in diffs.iter().filter(|d| d.status == DiffStatus::Changed) {
        out.extend(d.statements.iter().cloned());
    }
    for d in diffs.iter().filter(|d| d.status == DiffStatus::Added) {
        out.extend(d.deferred.iter().cloned());
    }
    let mut removed: Vec<&TableDiff> = diffs
        .iter()
        .copied()
        .filter(|d| d.status == DiffStatus::Removed)
        .collect();
    // Drop a table only after every removed table that references it.
    let references = |d: &TableDiff, other: &TableDiff| {
        d.target
            .as_ref()
            .is_some_and(|t| t.foreign_keys.iter().any(|fk| fk.ref_table == other.name))
    };
    let mut ordered = Vec::new();
    while !removed.is_empty() {
        let next = removed
            .iter()
            .position(|d| !removed.iter().any(|o| o.name != d.name && references(o, d)))
            .unwrap_or(0);
        ordered.push(removed.remove(next));
    }
    for d in ordered {
        out.extend(d.statements.iter().cloned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{ColumnDef, IndexDef};

    fn table(name: &str, cols: &[(&str, &str)]) -> TableSchema {
        TableSchema {
            schema: Some("public".into()),
            name: name.into(),
            columns: cols.iter().map(|(n, t)| ColumnDef::new(*n, *t)).collect(),
            primary_key: vec![cols[0].0.into()],
            primary_key_name: Some(format!("{name}_pkey")),
            ..TableSchema::default()
        }
    }

    #[test]
    fn classifies_and_scripts_differences() {
        let mut orders = table("orders", &[("id", "integer"), ("customer_id", "integer")]);
        orders.foreign_keys.push(ForeignKeyDef {
            name: "fk_orders_customer".into(),
            columns: vec!["customer_id".into()],
            ref_schema: None,
            ref_table: "customers".into(),
            ref_columns: vec!["id".into()],
        });
        let source = vec![
            table("customers", &[("id", "integer"), ("name", "text")]),
            orders,
            table("same", &[("id", "integer")]),
        ];
        let mut changed = table("customers", &[("id", "int4")]);
        changed
            .indexes
            .push(IndexDef::new("old_ix", vec!["id".into()], false));
        let target = vec![
            changed,
            table("same", &[("id", "int4")]),
            table("legacy", &[("id", "integer")]),
        ];
        let diffs = compare(
            DatabaseKind::PostgreSql,
            &source,
            DatabaseKind::PostgreSql,
            &target,
            Some("public"),
        );
        let status: Vec<(&str, DiffStatus)> =
            diffs.iter().map(|d| (d.name.as_str(), d.status)).collect();
        assert_eq!(
            status,
            [
                ("customers", DiffStatus::Changed),
                ("legacy", DiffStatus::Removed),
                ("orders", DiffStatus::Added),
                ("same", DiffStatus::Identical),
            ]
        );
        let all: Vec<&TableDiff> = diffs.iter().collect();
        let script = migration_script(&all);
        let pos = |needle: &str| {
            script
                .iter()
                .position(|s| s.contains(needle))
                .unwrap_or_else(|| panic!("{needle} missing in {script:#?}"))
        };
        assert!(pos("CREATE TABLE \"public\".\"orders\"") < pos("ADD COLUMN \"name\""));
        assert!(pos("ADD COLUMN \"name\"") < pos("ADD CONSTRAINT \"fk_orders_customer\""));
        assert!(
            pos("ADD CONSTRAINT \"fk_orders_customer\"") < pos("DROP TABLE \"public\".\"legacy\"")
        );
        assert!(!script[pos("CREATE TABLE \"public\".\"orders\"")].contains("FOREIGN KEY"));
    }

    #[test]
    fn converts_across_engines() {
        let mut t = table(
            "items",
            &[("id", "integer"), ("label", "character varying(40)")],
        );
        t.columns[1].default = Some("'x'::character varying".into());
        t.columns.push(ColumnDef {
            default: Some("now()".into()),
            ..ColumnDef::new("at", "timestamp with time zone")
        });
        let c = convert_table(
            DatabaseKind::PostgreSql,
            &t,
            DatabaseKind::SqlServer,
            Some("dbo"),
        );
        assert_eq!(c.schema.as_deref(), Some("dbo"));
        let types: Vec<&str> = c.columns.iter().map(|c| c.data_type.as_str()).collect();
        assert_eq!(types, ["int", "nvarchar(40)", "datetimeoffset"]);
        assert_eq!(c.columns[1].default.as_deref(), Some("'x'"));
        assert_eq!(c.columns[2].default, None, "functions are not portable");
    }

    #[test]
    fn drops_children_before_parents() {
        let parent = table("parent", &[("id", "integer")]);
        let mut child = table("child", &[("id", "integer"), ("parent_id", "integer")]);
        child.foreign_keys.push(ForeignKeyDef {
            name: "fk".into(),
            columns: vec!["parent_id".into()],
            ref_schema: None,
            ref_table: "parent".into(),
            ref_columns: vec!["id".into()],
        });
        let diffs = compare(
            DatabaseKind::PostgreSql,
            &[],
            DatabaseKind::PostgreSql,
            &[parent, child],
            Some("public"),
        );
        let all: Vec<&TableDiff> = diffs.iter().collect();
        let script = migration_script(&all);
        assert_eq!(
            script,
            [
                "DROP TABLE \"public\".\"child\"",
                "DROP TABLE \"public\".\"parent\""
            ]
        );
    }
}
