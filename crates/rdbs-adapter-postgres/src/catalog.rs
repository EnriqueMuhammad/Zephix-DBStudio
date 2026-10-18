//! Object tree from the PostgreSQL system catalogs.
//!
//! A PostgreSQL connection is bound to one database, so only the current database is expanded;
//! other databases are listed as leaves (browsing them needs another session — Milestone 3).

use std::collections::{BTreeMap, HashMap};

use rdbs_core::{DbError, DbObject, ObjectKind, SchemaContents, approx_rows_label};
use tokio_postgres::{Client, SimpleQueryMessage};

use crate::query_error;

/// Excludes system schemas; shared by all catalog queries.
pub(crate) const USER_SCHEMAS: &str =
    "n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema'";

/// Runs `sql` and returns all rows as text.
pub async fn text_rows(client: &Client, sql: &str) -> Result<Vec<Vec<Option<String>>>, DbError> {
    let messages = client.simple_query(sql).await.map_err(query_error)?;
    Ok(messages
        .into_iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::Row(row) => Some(
                (0..row.len())
                    .map(|i| row.get(i).map(str::to_owned))
                    .collect(),
            ),
            _ => None,
        })
        .collect())
}

pub(crate) fn col(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

/// Loads databases, and schemas/tables/views/routines/columns of the current database.
pub async fn load(client: &Client) -> Result<Vec<DbObject>, DbError> {
    let current = text_rows(client, "SELECT current_database()")
        .await?
        .first()
        .map(|r| col(r, 0))
        .unwrap_or_default();
    let databases = text_rows(
        client,
        "SELECT datname FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY datname",
    )
    .await?;
    let schemas = text_rows(
        client,
        &format!("SELECT n.nspname FROM pg_namespace n WHERE {USER_SCHEMAS} ORDER BY 1"),
    )
    .await?;
    let relations = text_rows(
        client,
        &format!(
            "SELECT n.nspname, c.relname, c.relkind::text, c.reltuples::bigint \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('r','p','v','m','f') AND {USER_SCHEMAS} ORDER BY 1, 2"
        ),
    )
    .await?;
    let columns = text_rows(
        client,
        &format!(
            "SELECT n.nspname, c.relname, a.attname, format_type(a.atttypid, a.atttypmod), \
                    a.attnotnull::text, EXISTS (SELECT 1 FROM pg_index i \
                        WHERE i.indrelid = c.oid AND i.indisprimary \
                          AND a.attnum = ANY(i.indkey))::text \
             FROM pg_attribute a \
             JOIN pg_class c ON c.oid = a.attrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE a.attnum > 0 AND NOT a.attisdropped \
               AND c.relkind IN ('r','p','v','m','f') AND {USER_SCHEMAS} \
             ORDER BY 1, 2, a.attnum"
        ),
    )
    .await?;
    let routines = text_rows(
        client,
        &format!(
            "SELECT n.nspname, p.proname, p.prokind::text, pg_get_function_result(p.oid) \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE p.prokind IN ('f','p') AND {USER_SCHEMAS} ORDER BY 1, 2"
        ),
    )
    .await?;

    let mut by_relation: HashMap<(String, String), Vec<DbObject>> = HashMap::new();
    for row in &columns {
        let mut detail = col(row, 3);
        if col(row, 5) == "true" {
            detail.push_str(" · PK");
        } else if col(row, 4) == "true" {
            detail.push_str(" · NOT NULL");
        }
        by_relation
            .entry((col(row, 0), col(row, 1)))
            .or_default()
            .push(DbObject::new(col(row, 2), ObjectKind::Column).with_detail(detail));
    }

    let mut contents: BTreeMap<String, SchemaContents> = schemas
        .iter()
        .map(|r| (col(r, 0), SchemaContents::default()))
        .collect();
    for row in &relations {
        let (schema, name, kind) = (col(row, 0), col(row, 1), col(row, 2));
        let children = by_relation
            .remove(&(schema.clone(), name.clone()))
            .unwrap_or_default();
        let mut obj = DbObject::new(
            name,
            match kind.as_str() {
                "v" => ObjectKind::View,
                "m" => ObjectKind::MaterializedView,
                _ => ObjectKind::Table,
            },
        )
        .with_children(children);
        if matches!(kind.as_str(), "r" | "p" | "m")
            && let Some(label) = col(row, 3).parse().ok().and_then(approx_rows_label)
        {
            obj = obj.with_detail(label);
        }
        let group = contents.entry(schema).or_default();
        match obj.kind {
            ObjectKind::View => group.views.push(obj),
            ObjectKind::MaterializedView => group.materialized_views.push(obj),
            _ => group.tables.push(obj),
        }
    }
    for row in &routines {
        let group = contents.entry(col(row, 0)).or_default();
        if col(row, 2) == "p" {
            group
                .procedures
                .push(DbObject::new(col(row, 1), ObjectKind::Procedure));
        } else {
            group
                .functions
                .push(DbObject::new(col(row, 1), ObjectKind::Function).with_detail(col(row, 3)));
        }
    }

    let schema_nodes: Vec<DbObject> = contents
        .into_iter()
        .map(|(name, c)| DbObject::new(name, ObjectKind::Schema).with_children(c.into_folders()))
        .collect();

    let mut schema_nodes = Some(schema_nodes);
    Ok(databases
        .iter()
        .map(|r| {
            let name = col(r, 0);
            if name == current {
                DbObject::new(name, ObjectKind::Database)
                    .with_children(schema_nodes.take().unwrap_or_default())
            } else {
                DbObject::new(name, ObjectKind::Database).with_detail(rdbs_core::schema::NOT_LOADED)
            }
        })
        .collect())
}
