//! Structured table definitions ([`TableSchema`]) from `information_schema`.
//!
//! MySQL has no schemas inside a database, so [`TableSchema::schema`] stays `None` and the
//! definitions refer to the session's database. Functional indexes are skipped.

use std::collections::HashMap;

use mysql_async::Conn;
use rdbs_core::export::sql_string;
use rdbs_core::{ColumnDef, DatabaseKind, DbError, ForeignKeyDef, IndexDef, TableSchema};

use crate::catalog::{col, text_rows};

fn lit(s: &str) -> String {
    sql_string(s, DatabaseKind::MySql)
}

/// Turns `COLUMN_DEFAULT` into a DDL expression.
///
/// MariaDB already reports SQL (`'text'`, `NULL`, `current_timestamp()`); MySQL reports the raw
/// value and flags expressions with `DEFAULT_GENERATED` in `EXTRA`.
pub(crate) fn default_expr(
    raw: Option<String>,
    extra: &str,
    data_type: &str,
    mariadb: bool,
) -> Option<String> {
    let raw = raw?;
    if mariadb {
        return (!raw.eq_ignore_ascii_case("NULL")).then_some(raw);
    }
    let upper = raw.to_ascii_uppercase();
    if extra.to_ascii_uppercase().contains("DEFAULT_GENERATED") {
        if upper.starts_with("CURRENT_TIMESTAMP") || upper.starts_with("NOW(") {
            return Some(raw);
        }
        return Some(format!("({raw})"));
    }
    let numeric = matches!(
        data_type.to_ascii_lowercase().as_str(),
        "tinyint"
            | "smallint"
            | "mediumint"
            | "int"
            | "integer"
            | "bigint"
            | "decimal"
            | "numeric"
            | "float"
            | "double"
            | "real"
    );
    let bit_literal = data_type.eq_ignore_ascii_case("bit") && raw.starts_with("b'");
    if (numeric && raw.parse::<f64>().is_ok()) || bit_literal {
        Some(raw)
    } else {
        Some(lit(&raw))
    }
}

/// Base tables of `database` (the session's current database when `None`).
pub async fn describe(
    conn: &mut Conn,
    database: Option<&str>,
    mariadb: bool,
) -> Result<Vec<TableSchema>, DbError> {
    let db = match database {
        Some(d) => lit(d),
        None => "DATABASE()".to_owned(),
    };
    let current = text_rows(conn, &format!("SELECT {db}"))
        .await?
        .first()
        .map(|r| col(r, 0))
        .unwrap_or_default();
    let tables = text_rows(
        conn,
        &format!(
            "SELECT TABLE_NAME FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = {db} AND TABLE_TYPE = 'BASE TABLE' ORDER BY TABLE_NAME"
        ),
    )
    .await?;
    let mut out: Vec<TableSchema> = tables
        .iter()
        .map(|r| TableSchema {
            name: col(r, 0),
            ..TableSchema::default()
        })
        .collect();
    let index: HashMap<String, usize> = out
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.clone(), i))
        .collect();

    let columns = text_rows(
        conn,
        &format!(
            "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, EXTRA, \
                    DATA_TYPE \
             FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {db} \
             ORDER BY TABLE_NAME, ORDINAL_POSITION"
        ),
    )
    .await?;
    for r in &columns {
        let Some(&t) = index.get(&col(r, 0)) else {
            continue;
        };
        let extra = col(r, 5);
        let lower = extra.to_ascii_lowercase();
        let identity = lower.contains("auto_increment");
        let generated = lower.contains("generated") && !lower.contains("default_generated");
        out[t].columns.push(ColumnDef {
            name: col(r, 1),
            data_type: col(r, 2),
            nullable: col(r, 3) == "YES",
            default: default_expr(r.get(4).cloned().flatten(), &extra, &col(r, 6), mariadb)
                .filter(|_| !identity && !generated),
            identity,
        });
    }

    let fks = text_rows(
        conn,
        &format!(
            "SELECT TABLE_NAME, CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, \
                    REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
             FROM information_schema.KEY_COLUMN_USAGE \
             WHERE TABLE_SCHEMA = {db} AND REFERENCED_TABLE_NAME IS NOT NULL \
             ORDER BY TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION"
        ),
    )
    .await?;
    for r in &fks {
        let Some(&t) = index.get(&col(r, 0)) else {
            continue;
        };
        let name = col(r, 1);
        let position = match out[t].foreign_keys.iter().position(|f| f.name == name) {
            Some(i) => i,
            None => {
                let ref_db = col(r, 3);
                out[t].foreign_keys.push(ForeignKeyDef {
                    name,
                    columns: Vec::new(),
                    ref_schema: (ref_db != current).then_some(ref_db),
                    ref_table: col(r, 4),
                    ref_columns: Vec::new(),
                });
                out[t].foreign_keys.len() - 1
            }
        };
        let fk = &mut out[t].foreign_keys[position];
        fk.columns.push(col(r, 2));
        fk.ref_columns.push(col(r, 5));
    }

    let stats = text_rows(
        conn,
        &format!(
            "SELECT TABLE_NAME, INDEX_NAME, NON_UNIQUE, COLUMN_NAME \
             FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = {db} \
             ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX"
        ),
    )
    .await?;
    let mut functional: Vec<(usize, String)> = Vec::new();
    for r in &stats {
        let Some(&t) = index.get(&col(r, 0)) else {
            continue;
        };
        let name = col(r, 1);
        let Some(column) = r.get(3).cloned().flatten() else {
            functional.push((t, name));
            continue;
        };
        if name == "PRIMARY" {
            out[t].primary_key.push(column);
            continue;
        }
        // The index MySQL creates implicitly for a foreign key comes back with the key.
        if out[t].foreign_keys.iter().any(|f| f.name == name) {
            continue;
        }
        match out[t].indexes.iter_mut().find(|i| i.name == name) {
            Some(ix) => ix.columns.push(column),
            None => out[t]
                .indexes
                .push(IndexDef::new(name, vec![column], col(r, 2) == "0")),
        }
    }
    for (t, name) in functional {
        out[t].indexes.retain(|i| i.name != name);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_defaults_become_sql() {
        assert_eq!(default_expr(None, "", "int", false), None);
        assert_eq!(
            default_expr(Some("0".into()), "", "int", false).as_deref(),
            Some("0")
        );
        assert_eq!(
            default_expr(Some("abc".into()), "", "varchar", false).as_deref(),
            Some("'abc'")
        );
        assert_eq!(
            default_expr(Some("it's".into()), "", "text", false).as_deref(),
            Some("'it''s'")
        );
        assert_eq!(
            default_expr(
                Some("CURRENT_TIMESTAMP".into()),
                "DEFAULT_GENERATED",
                "datetime",
                false
            )
            .as_deref(),
            Some("CURRENT_TIMESTAMP")
        );
        assert_eq!(
            default_expr(Some("uuid()".into()), "DEFAULT_GENERATED", "char", false).as_deref(),
            Some("(uuid())")
        );
    }

    #[test]
    fn mariadb_defaults_are_already_sql() {
        assert_eq!(default_expr(Some("NULL".into()), "", "int", true), None);
        assert_eq!(
            default_expr(Some("'x'".into()), "", "varchar", true).as_deref(),
            Some("'x'")
        );
    }
}
