//! Structured table definitions ([`TableSchema`]) from the SQL Server catalog views.
//!
//! Filtered indexes and included columns are skipped: they cannot be represented as plain
//! column lists.

use std::collections::HashMap;

use rdbs_core::{ColumnDef, DbError, ForeignKeyDef, IndexDef, TableSchema};

use crate::MssqlClient;
use crate::catalog::{col, column_type, text_rows};

fn lit(s: &str) -> String {
    format!("N'{}'", s.replace('\'', "''"))
}

/// `((0))` → `0`, `(getdate())` → `getdate()`: SQL Server stores defaults wrapped in parens.
pub(crate) fn strip_outer_parens(expr: &str) -> String {
    let mut s = expr.trim();
    while s.starts_with('(') && s.ends_with(')') {
        // Only strip when the first paren closes at the very end.
        let mut depth = 0i32;
        let mut closes_at_end = true;
        for (i, ch) in s.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 && i != s.len() - 1 {
                        closes_at_end = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !closes_at_end {
            break;
        }
        s = s[1..s.len() - 1].trim();
    }
    s.to_owned()
}

/// Base tables of the current database (optionally of one schema).
pub async fn describe(
    client: &mut MssqlClient,
    schema: Option<&str>,
) -> Result<Vec<TableSchema>, DbError> {
    let filter = match schema {
        Some(s) => format!("t.is_ms_shipped = 0 AND s.name = {}", lit(s)),
        None => "t.is_ms_shipped = 0".to_owned(),
    };
    let from = "sys.tables t JOIN sys.schemas s ON s.schema_id = t.schema_id";

    let tables = text_rows(
        client,
        &format!("SELECT s.name, t.name FROM {from} WHERE {filter} ORDER BY 1, 2"),
    )
    .await?;
    let mut out: Vec<TableSchema> = tables
        .iter()
        .map(|r| TableSchema {
            schema: Some(col(r, 0)),
            name: col(r, 1),
            ..TableSchema::default()
        })
        .collect();
    let index: HashMap<(String, String), usize> = out
        .iter()
        .enumerate()
        .map(|(i, t)| ((t.schema.clone().unwrap_or_default(), t.name.clone()), i))
        .collect();
    let table = |r: &[Option<String>]| index.get(&(col(r, 0), col(r, 1))).copied();

    let columns = text_rows(
        client,
        &format!(
            "SELECT s.name, t.name, c.name, ty.name, c.max_length, c.precision, c.scale, \
                    c.is_nullable, dc.definition, c.is_identity, c.is_computed \
             FROM sys.columns c JOIN {from} ON t.object_id = c.object_id \
             JOIN sys.types ty ON ty.user_type_id = c.user_type_id \
             LEFT JOIN sys.default_constraints dc ON dc.object_id = c.default_object_id \
             WHERE {filter} ORDER BY s.name, t.name, c.column_id"
        ),
    )
    .await?;
    for r in &columns {
        let Some(t) = table(r) else { continue };
        let max_length = col(r, 4).parse().unwrap_or(0);
        out[t].columns.push(ColumnDef {
            name: col(r, 2),
            data_type: column_type(&col(r, 3), max_length, &col(r, 5), &col(r, 6)),
            nullable: col(r, 7) == "true",
            default: r
                .get(8)
                .cloned()
                .flatten()
                .map(|d| strip_outer_parens(&d))
                .filter(|_| col(r, 10) != "true"),
            identity: col(r, 9) == "true",
        });
    }

    let keys = text_rows(
        client,
        &format!(
            "SELECT s.name, t.name, kc.name, kc.type, c.name \
             FROM sys.key_constraints kc JOIN {from} ON t.object_id = kc.parent_object_id \
             JOIN sys.index_columns ic \
               ON ic.object_id = kc.parent_object_id AND ic.index_id = kc.unique_index_id \
             JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
             WHERE {filter} ORDER BY s.name, t.name, kc.name, ic.key_ordinal"
        ),
    )
    .await?;
    for r in &keys {
        let Some(t) = table(r) else { continue };
        let name = col(r, 2);
        let column = col(r, 4);
        if col(r, 3).trim() == "PK" {
            out[t].primary_key_name = Some(name);
            out[t].primary_key.push(column);
        } else {
            match out[t].indexes.iter_mut().find(|i| i.name == name) {
                Some(ix) => ix.columns.push(column),
                None => out[t].indexes.push(IndexDef {
                    constraint: true,
                    ..IndexDef::new(name, vec![column], true)
                }),
            }
        }
    }

    let indexes = text_rows(
        client,
        &format!(
            "SELECT s.name, t.name, i.name, i.is_unique, c.name \
             FROM sys.indexes i JOIN {from} ON t.object_id = i.object_id \
             JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
             JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
             WHERE {filter} AND i.type > 0 AND i.is_primary_key = 0 \
               AND i.is_unique_constraint = 0 AND i.has_filter = 0 \
               AND ic.is_included_column = 0 \
             ORDER BY s.name, t.name, i.name, ic.key_ordinal"
        ),
    )
    .await?;
    for r in &indexes {
        let Some(t) = table(r) else { continue };
        let name = col(r, 2);
        let column = col(r, 4);
        match out[t].indexes.iter_mut().find(|i| i.name == name) {
            Some(ix) => ix.columns.push(column),
            None => out[t]
                .indexes
                .push(IndexDef::new(name, vec![column], col(r, 3) == "true")),
        }
    }

    let fks = text_rows(
        client,
        &format!(
            "SELECT s.name, t.name, fk.name, pc.name, rs.name, rt.name, rc.name \
             FROM sys.foreign_keys fk JOIN {from} ON t.object_id = fk.parent_object_id \
             JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
             JOIN sys.columns pc \
               ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
             JOIN sys.tables rt ON rt.object_id = fkc.referenced_object_id \
             JOIN sys.schemas rs ON rs.schema_id = rt.schema_id \
             JOIN sys.columns rc \
               ON rc.object_id = fkc.referenced_object_id \
              AND rc.column_id = fkc.referenced_column_id \
             WHERE {filter} ORDER BY s.name, t.name, fk.name, fkc.constraint_column_id"
        ),
    )
    .await?;
    for r in &fks {
        let Some(t) = table(r) else { continue };
        let name = col(r, 2);
        let position = match out[t].foreign_keys.iter().position(|f| f.name == name) {
            Some(i) => i,
            None => {
                let ref_schema = col(r, 4);
                let ref_schema =
                    (Some(&ref_schema) != out[t].schema.as_ref()).then_some(ref_schema);
                out[t].foreign_keys.push(ForeignKeyDef {
                    name,
                    columns: Vec::new(),
                    ref_schema,
                    ref_table: col(r, 5),
                    ref_columns: Vec::new(),
                });
                out[t].foreign_keys.len() - 1
            }
        };
        let fk = &mut out[t].foreign_keys[position];
        fk.columns.push(col(r, 3));
        fk.ref_columns.push(col(r, 6));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_wrapping_parens_only() {
        assert_eq!(strip_outer_parens("((0))"), "0");
        assert_eq!(strip_outer_parens("(getdate())"), "getdate()");
        assert_eq!(strip_outer_parens("(N'a')"), "N'a'");
        assert_eq!(strip_outer_parens("(1)+(2)"), "(1)+(2)");
    }
}
