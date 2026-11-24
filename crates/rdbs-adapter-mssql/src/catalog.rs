//! Object tree from the SQL Server catalog views of the current database.
//!
//! Other databases on the server are listed as leaves; browsing them needs `USE` or another
//! session (Milestone 3).

use std::collections::{BTreeMap, HashMap};

use futures_util::TryStreamExt;
use rdbs_core::{DbError, DbObject, ObjectKind, SchemaContents, approx_rows_label};
use tiberius::QueryItem;

use crate::{MssqlClient, map_error, values};

/// Runs `sql` and returns all rows of all result sets as text.
pub async fn text_rows(
    client: &mut MssqlClient,
    sql: &str,
) -> Result<Vec<Vec<Option<String>>>, DbError> {
    let mut stream = client.simple_query(sql).await.map_err(map_error)?;
    let mut rows = Vec::new();
    while let Some(item) = stream.try_next().await.map_err(map_error)? {
        if let QueryItem::Row(row) = item {
            rows.push(row.into_iter().map(|d| values::text(&d)).collect());
        }
    }
    Ok(rows)
}

/// How long a catalog read waits for a lock before it gives up (milliseconds).
const CATALOG_LOCK_TIMEOUT_MS: u32 = 10_000;

/// Prepares the session for catalog reads and returns the batch that undoes it.
///
/// Under READ COMMITTED, catalog views wait behind any open transaction that creates,
/// drops or alters a table (a `SELECT INTO` in a running job is enough), so the explorer
/// would hang. READ UNCOMMITTED reads past those locks; `LOCK_TIMEOUT` bounds the locks
/// that are still taken. The session's own isolation level and lock timeout are restored
/// afterwards, because query tabs share the session.
pub async fn relax(client: &mut MssqlClient) -> Result<String, DbError> {
    let rows = text_rows(
        client,
        &format!(
            "SELECT @@LOCK_TIMEOUT, transaction_isolation_level FROM sys.dm_exec_sessions \
             WHERE session_id = @@SPID; \
             SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED; \
             SET LOCK_TIMEOUT {CATALOG_LOCK_TIMEOUT_MS}"
        ),
    )
    .await?;
    let row = rows.first().map(Vec::as_slice).unwrap_or_default();
    Ok(restore_sql(&col(row, 0), &col(row, 1)))
}

/// `SET` batch restoring a lock timeout and an isolation level as reported by
/// `@@LOCK_TIMEOUT` and `sys.dm_exec_sessions.transaction_isolation_level`.
fn restore_sql(lock_timeout: &str, isolation: &str) -> String {
    let lock_timeout = lock_timeout.trim().parse::<i64>().unwrap_or(-1);
    let isolation = match isolation.trim() {
        "1" => "READ UNCOMMITTED",
        "3" => "REPEATABLE READ",
        "4" => "SERIALIZABLE",
        "5" => "SNAPSHOT",
        _ => "READ COMMITTED",
    };
    format!("SET LOCK_TIMEOUT {lock_timeout}; SET TRANSACTION ISOLATION LEVEL {isolation}")
}

/// Runs the batch returned by [`relax`].
pub async fn restore(client: &mut MssqlClient, sql: &str) -> Result<(), DbError> {
    text_rows(client, sql).await.map(drop)
}

/// Explains a lock timeout (error 1222) of a catalog read.
pub fn lock_hint(mut error: DbError) -> DbError {
    if error.code.as_deref() == Some("1222") {
        error.message = format!(
            "the catalog is locked by another session (for example an open transaction that \
             creates or changes a table); waited {}s",
            CATALOG_LOCK_TIMEOUT_MS / 1000
        );
    }
    error
}

pub(crate) fn col(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

/// `nvarchar` + max_length 100 → `nvarchar(50)`; `decimal` + precision/scale → `decimal(12,2)`.
pub(crate) fn column_type(name: &str, max_length: i64, precision: &str, scale: &str) -> String {
    let length = |divisor: i64| {
        if max_length < 0 {
            "max".to_owned()
        } else {
            (max_length / divisor).to_string()
        }
    };
    match name {
        "varchar" | "char" | "varbinary" | "binary" => format!("{name}({})", length(1)),
        "nvarchar" | "nchar" => format!("{name}({})", length(2)),
        "decimal" | "numeric" => format!("{name}({precision},{scale})"),
        "datetime2" | "time" | "datetimeoffset" => format!("{name}({scale})"),
        _ => name.to_owned(),
    }
}

/// Loads databases and the schemas/tables/views/routines/columns of the current database.
pub async fn load(client: &mut MssqlClient) -> Result<Vec<DbObject>, DbError> {
    let current = text_rows(client, "SELECT DB_NAME()")
        .await?
        .first()
        .map(|r| col(r, 0))
        .unwrap_or_default();
    let databases = text_rows(
        client,
        "SELECT name FROM sys.databases WHERE state = 0 ORDER BY name",
    )
    .await?;
    // Partition metadata of a table being created or altered stays locked even under
    // READ UNCOMMITTED; READPAST skips it, so that table shows no row estimate.
    let objects = text_rows(
        client,
        "SELECT s.name, o.name, RTRIM(o.type), \
                (SELECT SUM(p.rows) FROM sys.partitions p WITH (READCOMMITTED, READPAST) \
                  WHERE p.object_id = o.object_id AND p.index_id IN (0, 1)) \
         FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id \
         WHERE o.is_ms_shipped = 0 AND o.type IN ('U','V','P','FN','IF','TF') \
         ORDER BY s.name, o.name",
    )
    .await?;
    let columns = text_rows(
        client,
        "SELECT s.name, o.name, c.name, t.name, c.max_length, c.precision, c.scale, \
                c.is_nullable, \
                CASE WHEN EXISTS (SELECT 1 FROM sys.index_columns ic \
                     JOIN sys.indexes i ON i.object_id = ic.object_id AND i.index_id = ic.index_id \
                     WHERE i.is_primary_key = 1 AND ic.object_id = c.object_id \
                       AND ic.column_id = c.column_id) THEN 1 ELSE 0 END \
         FROM sys.columns c \
         JOIN sys.objects o ON o.object_id = c.object_id \
         JOIN sys.schemas s ON s.schema_id = o.schema_id \
         JOIN sys.types t ON t.user_type_id = c.user_type_id \
         WHERE o.type IN ('U','V') AND o.is_ms_shipped = 0 \
         ORDER BY s.name, o.name, c.column_id",
    )
    .await?;

    let mut by_object: HashMap<(String, String), Vec<DbObject>> = HashMap::new();
    for row in &columns {
        let max_length = col(row, 4).parse().unwrap_or(0);
        let mut detail = column_type(&col(row, 3), max_length, &col(row, 5), &col(row, 6));
        if col(row, 8) == "1" {
            detail.push_str(" · PK");
        } else if col(row, 7) == "false" {
            detail.push_str(" · NOT NULL");
        }
        by_object
            .entry((col(row, 0), col(row, 1)))
            .or_default()
            .push(DbObject::new(col(row, 2), ObjectKind::Column).with_detail(detail));
    }

    let mut contents: BTreeMap<String, SchemaContents> = BTreeMap::new();
    contents.entry("dbo".to_owned()).or_default();
    for row in &objects {
        let (schema, name, ty) = (col(row, 0), col(row, 1), col(row, 2));
        let group = contents.entry(schema.clone()).or_default();
        match ty.as_str() {
            "U" | "V" => {
                let children = by_object
                    .remove(&(schema, name.clone()))
                    .unwrap_or_default();
                let kind = if ty == "U" {
                    ObjectKind::Table
                } else {
                    ObjectKind::View
                };
                let mut obj = DbObject::new(name, kind).with_children(children);
                if ty == "U"
                    && let Some(label) = col(row, 3).parse().ok().and_then(approx_rows_label)
                {
                    obj = obj.with_detail(label);
                }
                if ty == "U" {
                    group.tables.push(obj);
                } else {
                    group.views.push(obj);
                }
            }
            "P" => group
                .procedures
                .push(DbObject::new(name, ObjectKind::Procedure)),
            _ => group
                .functions
                .push(DbObject::new(name, ObjectKind::Function)),
        }
    }

    let mut schema_nodes: Vec<DbObject> = contents
        .into_iter()
        .map(|(name, c)| DbObject::new(name, ObjectKind::Schema).with_children(c.into_folders()))
        .collect();
    // Programmability and Security follow the schemas, as database-level folders.
    schema_nodes.extend(crate::folders::load(client).await);
    let mut schema_nodes = Some(schema_nodes);
    let mut tree: Vec<DbObject> = databases
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
        .collect();
    // Server-level, after the databases. A failure here is a notice, not a failed refresh.
    tree.push(crate::agent::load_node(client).await);
    Ok(tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_column_types() {
        assert_eq!(column_type("nvarchar", 100, "0", "0"), "nvarchar(50)");
        assert_eq!(column_type("varchar", -1, "0", "0"), "varchar(max)");
        assert_eq!(column_type("decimal", 9, "12", "2"), "decimal(12,2)");
        assert_eq!(column_type("int", 4, "10", "0"), "int");
    }

    #[test]
    fn restores_session_settings() {
        assert_eq!(
            restore_sql("-1", "2"),
            "SET LOCK_TIMEOUT -1; SET TRANSACTION ISOLATION LEVEL READ COMMITTED"
        );
        assert_eq!(
            restore_sql("5000", "4"),
            "SET LOCK_TIMEOUT 5000; SET TRANSACTION ISOLATION LEVEL SERIALIZABLE"
        );
        assert_eq!(
            restore_sql("", ""),
            "SET LOCK_TIMEOUT -1; SET TRANSACTION ISOLATION LEVEL READ COMMITTED"
        );
    }

    #[test]
    fn explains_lock_timeouts() {
        let e = lock_hint(
            DbError::new(rdbs_core::DbErrorKind::Query, "Lock request time out").with_code("1222"),
        );
        assert!(
            e.message
                .starts_with("the catalog is locked by another session")
        );
    }
}
