//! Object tree from `information_schema` (MySQL and MariaDB share it).

use std::collections::{BTreeMap, HashMap};

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Row};
use rdbs_core::{DbError, DbObject, ObjectKind, SchemaContents, approx_rows_label};

use crate::{map_error, values};

/// Runs `sql` and returns all rows as text.
pub async fn text_rows(conn: &mut Conn, sql: &str) -> Result<Vec<Vec<Option<String>>>, DbError> {
    let rows: Vec<Row> = conn.query(sql).await.map_err(map_error)?;
    Ok(rows
        .into_iter()
        .map(|row| row.unwrap().into_iter().map(values::text).collect())
        .collect())
}

pub(crate) fn col(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

/// Loads every database with its tables, views, routines and columns.
pub async fn load(conn: &mut Conn) -> Result<Vec<DbObject>, DbError> {
    let databases = text_rows(
        conn,
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME",
    )
    .await?;
    let tables = text_rows(
        conn,
        "SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE, TABLE_ROWS \
         FROM information_schema.TABLES ORDER BY TABLE_SCHEMA, TABLE_NAME",
    )
    .await?;
    let columns = text_rows(
        conn,
        "SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, COLUMN_KEY, IS_NULLABLE \
         FROM information_schema.COLUMNS \
         ORDER BY TABLE_SCHEMA, TABLE_NAME, ORDINAL_POSITION",
    )
    .await?;
    let routines = text_rows(
        conn,
        "SELECT ROUTINE_SCHEMA, ROUTINE_NAME, ROUTINE_TYPE, DTD_IDENTIFIER \
         FROM information_schema.ROUTINES ORDER BY ROUTINE_SCHEMA, ROUTINE_NAME",
    )
    .await?;

    let mut by_table: HashMap<(String, String), Vec<DbObject>> = HashMap::new();
    for row in &columns {
        let mut detail = col(row, 3);
        if col(row, 4) == "PRI" {
            detail.push_str(" · PK");
        } else if col(row, 5) == "NO" {
            detail.push_str(" · NOT NULL");
        }
        by_table
            .entry((col(row, 0), col(row, 1)))
            .or_default()
            .push(DbObject::new(col(row, 2), ObjectKind::Column).with_detail(detail));
    }

    let mut contents: BTreeMap<String, SchemaContents> = databases
        .iter()
        .map(|r| (col(r, 0), SchemaContents::default()))
        .collect();
    for row in &tables {
        let (db, name, kind) = (col(row, 0), col(row, 1), col(row, 2));
        let children = by_table
            .remove(&(db.clone(), name.clone()))
            .unwrap_or_default();
        let is_view = kind.contains("VIEW");
        let mut obj = DbObject::new(
            name,
            if is_view {
                ObjectKind::View
            } else {
                ObjectKind::Table
            },
        )
        .with_children(children);
        if !is_view && let Some(label) = col(row, 3).parse().ok().and_then(approx_rows_label) {
            obj = obj.with_detail(label);
        }
        let group = contents.entry(db).or_default();
        if is_view {
            group.views.push(obj);
        } else {
            group.tables.push(obj);
        }
    }
    for row in &routines {
        let group = contents.entry(col(row, 0)).or_default();
        if col(row, 2) == "PROCEDURE" {
            group
                .procedures
                .push(DbObject::new(col(row, 1), ObjectKind::Procedure));
        } else {
            group
                .functions
                .push(DbObject::new(col(row, 1), ObjectKind::Function).with_detail(col(row, 3)));
        }
    }

    Ok(contents
        .into_iter()
        .map(|(name, c)| DbObject::new(name, ObjectKind::Database).with_children(c.into_folders()))
        .collect())
}
