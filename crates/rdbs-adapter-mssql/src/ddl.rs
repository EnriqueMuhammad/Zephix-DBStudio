//! DDL reconstruction from SQL Server catalog views (`sys.*`) and `OBJECT_DEFINITION`.

use rdbs_core::dialect::{qualified_name, quote_identifier};
use rdbs_core::schema::ObjectPath;
use rdbs_core::{DatabaseKind, DbError, DbErrorKind, ObjectKind};

use crate::MssqlClient;
use crate::catalog::{col, column_type, text_rows};

const MS: DatabaseKind = DatabaseKind::SqlServer;

/// `N'…'` literal.
fn lit(s: &str) -> String {
    format!("N'{}'", s.replace('\'', "''"))
}

/// `CREATE …` statement(s) for `object` in the current database.
pub async fn object_ddl(client: &mut MssqlClient, object: &ObjectPath) -> Result<String, DbError> {
    let schema = object.schema.as_deref().unwrap_or("dbo");
    let two_part = qualified_name(MS, None, Some(schema), &object.name);
    let id = format!("OBJECT_ID({})", lit(&two_part));
    match object.kind {
        ObjectKind::Table => table_ddl(client, &two_part, &id).await,
        ObjectKind::View | ObjectKind::Procedure | ObjectKind::Function => {
            let rows = text_rows(client, &format!("SELECT OBJECT_DEFINITION({id})")).await?;
            let ddl = rows.first().map(|r| col(r, 0)).unwrap_or_default();
            if ddl.is_empty() {
                return Err(DbError::new(
                    DbErrorKind::Query,
                    format!("no definition for {} (missing or encrypted)", object.name),
                ));
            }
            Ok(ddl.trim().to_owned())
        }
        _ => Err(DbError::new(
            DbErrorKind::Unsupported,
            "DDL is available for tables, views and routines",
        )),
    }
}

async fn table_ddl(client: &mut MssqlClient, two_part: &str, id: &str) -> Result<String, DbError> {
    let columns = text_rows(
        client,
        &format!(
            "SELECT c.name, t.name, c.max_length, c.precision, c.scale, c.is_nullable, \
                    c.is_identity, CAST(ic.seed_value AS nvarchar(40)), \
                    CAST(ic.increment_value AS nvarchar(40)), dc.definition, cc.definition \
             FROM sys.columns c \
             JOIN sys.types t ON t.user_type_id = c.user_type_id \
             LEFT JOIN sys.identity_columns ic \
                    ON ic.object_id = c.object_id AND ic.column_id = c.column_id \
             LEFT JOIN sys.default_constraints dc \
                    ON dc.parent_object_id = c.object_id AND dc.parent_column_id = c.column_id \
             LEFT JOIN sys.computed_columns cc \
                    ON cc.object_id = c.object_id AND cc.column_id = c.column_id \
             WHERE c.object_id = {id} ORDER BY c.column_id"
        ),
    )
    .await?;
    if columns.is_empty() {
        return Err(DbError::new(
            DbErrorKind::Query,
            format!("table {two_part} was not found"),
        ));
    }
    let keys = text_rows(
        client,
        &format!(
            "SELECT kc.name, kc.type, i.type_desc, \
                    STRING_AGG(QUOTENAME(c.name) + CASE WHEN ic.is_descending_key = 1 \
                        THEN ' DESC' ELSE '' END, ', ') WITHIN GROUP (ORDER BY ic.key_ordinal) \
             FROM sys.key_constraints kc \
             JOIN sys.indexes i ON i.object_id = kc.parent_object_id AND i.index_id = kc.unique_index_id \
             JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
             JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
             WHERE kc.parent_object_id = {id} \
             GROUP BY kc.name, kc.type, i.type_desc ORDER BY kc.type, kc.name"
        ),
    )
    .await?;
    let foreign = text_rows(
        client,
        &format!(
            "SELECT fk.name, \
                    STRING_AGG(QUOTENAME(pc.name), ', ') WITHIN GROUP (ORDER BY fkc.constraint_column_id), \
                    QUOTENAME(SCHEMA_NAME(rt.schema_id)) + '.' + QUOTENAME(rt.name), \
                    STRING_AGG(QUOTENAME(rc.name), ', ') WITHIN GROUP (ORDER BY fkc.constraint_column_id) \
             FROM sys.foreign_keys fk \
             JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
             JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
             JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
             JOIN sys.tables rt ON rt.object_id = fk.referenced_object_id \
             WHERE fk.parent_object_id = {id} \
             GROUP BY fk.name, rt.schema_id, rt.name ORDER BY fk.name"
        ),
    )
    .await?;
    let indexes = text_rows(
        client,
        &format!(
            "SELECT i.name, i.is_unique, i.type_desc, \
                    STRING_AGG(QUOTENAME(c.name) + CASE WHEN ic.is_descending_key = 1 \
                        THEN ' DESC' ELSE '' END, ', ') WITHIN GROUP (ORDER BY ic.key_ordinal) \
             FROM sys.indexes i \
             JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
             JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
             WHERE i.object_id = {id} AND i.is_primary_key = 0 AND i.is_unique_constraint = 0 \
               AND i.type > 0 AND ic.is_included_column = 0 \
             GROUP BY i.name, i.is_unique, i.type_desc ORDER BY i.name"
        ),
    )
    .await?;

    let mut lines: Vec<String> = columns
        .iter()
        .map(|r| {
            let name = quote_identifier(MS, &col(r, 0));
            let computed = col(r, 10);
            if !computed.is_empty() {
                return format!("    {name} AS {computed}");
            }
            let max_length = col(r, 2).parse().unwrap_or(0);
            let mut line = format!(
                "    {name} {}",
                column_type(&col(r, 1), max_length, &col(r, 3), &col(r, 4))
            );
            if col(r, 6) == "true" {
                line.push_str(&format!(" IDENTITY({},{})", col(r, 7), col(r, 8)));
            }
            line.push_str(if col(r, 5) == "true" {
                " NULL"
            } else {
                " NOT NULL"
            });
            let default = col(r, 9);
            if !default.is_empty() {
                line.push_str(&format!(" DEFAULT {default}"));
            }
            line
        })
        .collect();
    lines.extend(keys.iter().map(|r| {
        let kind = if col(r, 1) == "PK" {
            "PRIMARY KEY"
        } else {
            "UNIQUE"
        };
        format!(
            "    CONSTRAINT {} {kind} {} ({})",
            quote_identifier(MS, &col(r, 0)),
            col(r, 2),
            col(r, 3)
        )
    }));
    lines.extend(foreign.iter().map(|r| {
        format!(
            "    CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
            quote_identifier(MS, &col(r, 0)),
            col(r, 1),
            col(r, 2),
            col(r, 3)
        )
    }));

    let mut out = format!("CREATE TABLE {two_part} (\n{}\n);", lines.join(",\n"));
    for r in &indexes {
        let unique = if col(r, 1) == "true" { "UNIQUE " } else { "" };
        out.push_str(&format!(
            "\n\nCREATE {unique}{} INDEX {} ON {two_part} ({});",
            col(r, 2),
            quote_identifier(MS, &col(r, 0)),
            col(r, 3)
        ));
    }
    Ok(out)
}
