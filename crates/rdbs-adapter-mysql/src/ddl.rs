//! DDL via MySQL's `SHOW CREATE …` statements.

use mysql_async::Conn;
use rdbs_core::dialect::qualified_name;
use rdbs_core::schema::ObjectPath;
use rdbs_core::{DatabaseKind, DbError, DbErrorKind, ObjectKind};

use crate::catalog::{col, text_rows};

/// `CREATE …` statement for `object`.
pub async fn object_ddl(conn: &mut Conn, object: &ObjectPath) -> Result<String, DbError> {
    let name = qualified_name(
        DatabaseKind::MySql,
        object.database.as_deref(),
        None,
        &object.name,
    );
    // (statement, column holding the DDL)
    let (sql, column) = match object.kind {
        ObjectKind::Table => (format!("SHOW CREATE TABLE {name}"), 1),
        ObjectKind::View => (format!("SHOW CREATE VIEW {name}"), 1),
        ObjectKind::Procedure => (format!("SHOW CREATE PROCEDURE {name}"), 2),
        ObjectKind::Function => (format!("SHOW CREATE FUNCTION {name}"), 2),
        _ => {
            return Err(DbError::new(
                DbErrorKind::Unsupported,
                "DDL is available for tables, views and routines",
            ));
        }
    };
    let rows = text_rows(conn, &sql).await?;
    let ddl = rows.first().map(|r| col(r, column)).unwrap_or_default();
    if ddl.is_empty() {
        return Err(DbError::new(
            DbErrorKind::Query,
            format!(
                "no definition returned for {} (missing privileges?)",
                object.name
            ),
        ));
    }
    Ok(format!("{ddl};"))
}
