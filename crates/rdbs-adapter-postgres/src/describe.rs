//! Structured table definitions ([`TableSchema`]) from the PostgreSQL catalogs.
//!
//! Expression and partial indexes are skipped: they cannot be represented as column lists.

use std::collections::HashMap;

use rdbs_core::{ColumnDef, DbError, ForeignKeyDef, IndexDef, TableSchema};
use tokio_postgres::Client;

use crate::catalog::{USER_SCHEMAS, col, text_rows};

/// Separator for aggregated column lists (cannot appear in identifiers in practice).
const SEP: char = '\u{1f}';

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn split(list: &str) -> Vec<String> {
    if list.is_empty() {
        Vec::new()
    } else {
        list.split(SEP).map(str::to_owned).collect()
    }
}

/// Column-name list of `keys` (an attnum array) of relation `rel`, in key order.
fn attnames(keys: &str, rel: &str) -> String {
    format!(
        "(SELECT string_agg(a.attname, chr(31) ORDER BY k.ord) \
         FROM unnest({keys}) WITH ORDINALITY k(attnum, ord) \
         JOIN pg_attribute a ON a.attrelid = {rel} AND a.attnum = k.attnum)"
    )
}

/// Base tables of the current database (optionally of one schema).
pub async fn describe(client: &Client, schema: Option<&str>) -> Result<Vec<TableSchema>, DbError> {
    let filter = match schema {
        Some(s) => format!("n.nspname = {}", lit(s)),
        None => USER_SCHEMAS.to_owned(),
    };
    let tables_where = format!("c.relkind IN ('r','p') AND NOT c.relispartition AND {filter}");

    let tables = text_rows(
        client,
        &format!(
            "SELECT n.nspname, c.relname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace WHERE {tables_where} ORDER BY 1, 2"
        ),
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
            "SELECT n.nspname, c.relname, a.attname, format_type(a.atttypid, a.atttypmod), \
                    a.attnotnull, pg_get_expr(d.adbin, d.adrelid), a.attidentity, a.attgenerated \
             FROM pg_attribute a \
             JOIN pg_class c ON c.oid = a.attrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attnum > 0 AND NOT a.attisdropped AND {tables_where} \
             ORDER BY n.nspname, c.relname, a.attnum"
        ),
    )
    .await?;
    for r in &columns {
        let Some(t) = table(r) else { continue };
        let default = r.get(5).cloned().flatten();
        // `serial` columns are identities whose default is a sequence call.
        let serial = default
            .as_deref()
            .is_some_and(|d| d.starts_with("nextval("));
        let identity = !col(r, 6).is_empty() || serial;
        let generated = !col(r, 7).is_empty();
        out[t].columns.push(ColumnDef {
            name: col(r, 2),
            data_type: col(r, 3),
            nullable: col(r, 4) != "t",
            default: default.filter(|_| !identity && !generated),
            identity,
        });
    }

    let constraints = text_rows(
        client,
        &format!(
            "SELECT n.nspname, c.relname, con.conname, con.contype::text, {}, fn.nspname, \
                    fc.relname, {} \
             FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_class fc ON fc.oid = con.confrelid \
             LEFT JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
             WHERE con.contype IN ('p', 'u', 'f') AND {tables_where} ORDER BY 1, 2, 3",
            attnames("con.conkey", "con.conrelid"),
            attnames("con.confkey", "con.confrelid"),
        ),
    )
    .await?;
    for r in &constraints {
        let Some(t) = table(r) else { continue };
        let name = col(r, 2);
        let columns = split(&col(r, 4));
        match col(r, 3).as_str() {
            "p" => {
                out[t].primary_key = columns;
                out[t].primary_key_name = Some(name);
            }
            "u" => out[t].indexes.push(IndexDef {
                constraint: true,
                ..IndexDef::new(name, columns, true)
            }),
            _ => {
                let ref_schema = col(r, 5);
                let ref_schema =
                    (Some(&ref_schema) != out[t].schema.as_ref()).then_some(ref_schema);
                out[t].foreign_keys.push(ForeignKeyDef {
                    name,
                    columns,
                    ref_schema,
                    ref_table: col(r, 6),
                    ref_columns: split(&col(r, 7)),
                });
            }
        }
    }

    let indexes = text_rows(
        client,
        &format!(
            "SELECT n.nspname, c.relname, i.relname, x.indisunique, {} \
             FROM pg_index x \
             JOIN pg_class i ON i.oid = x.indexrelid \
             JOIN pg_class c ON c.oid = x.indrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE NOT x.indisprimary AND x.indexprs IS NULL AND x.indpred IS NULL \
               AND NOT EXISTS (SELECT 1 FROM pg_constraint con WHERE con.conindid = x.indexrelid) \
               AND {tables_where} \
             ORDER BY 1, 2, 3",
            attnames("x.indkey::int2[]", "x.indrelid"),
        ),
    )
    .await?;
    for r in &indexes {
        let Some(t) = table(r) else { continue };
        out[t].indexes.push(IndexDef::new(
            col(r, 2),
            split(&col(r, 4)),
            col(r, 3) == "t",
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_aggregated_lists() {
        assert!(split("").is_empty());
        assert_eq!(split("a\u{1f}b c"), ["a", "b c"]);
    }
}
