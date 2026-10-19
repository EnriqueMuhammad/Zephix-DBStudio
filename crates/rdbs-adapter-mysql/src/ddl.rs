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

/// Single-quoted string literal.
fn lit(s: &str) -> String {
    rdbs_core::export::sql_string(s, DatabaseKind::MySql)
}

/// Parameters of a function or procedure.
pub async fn routine_signature(
    conn: &mut Conn,
    object: &ObjectPath,
) -> Result<rdbs_core::routine::RoutineSignature, DbError> {
    use rdbs_core::routine::{ParamMode, RoutineParam, RoutineSignature};
    let schema = match &object.database {
        Some(db) => lit(db),
        None => "DATABASE()".to_owned(),
    };
    let routine_type = if object.kind == ObjectKind::Function {
        "FUNCTION"
    } else {
        "PROCEDURE"
    };
    // Position 0 is a function's return value.
    let rows = text_rows(
        conn,
        &format!(
            "SELECT PARAMETER_MODE, PARAMETER_NAME, DTD_IDENTIFIER \
             FROM information_schema.PARAMETERS \
             WHERE SPECIFIC_SCHEMA = {schema} AND SPECIFIC_NAME = {} \
               AND ROUTINE_TYPE = '{routine_type}' AND ORDINAL_POSITION > 0 \
             ORDER BY ORDINAL_POSITION",
            lit(&object.name)
        ),
    )
    .await?;
    let params = rows
        .iter()
        .map(|r| RoutineParam {
            name: col(r, 1),
            data_type: col(r, 2),
            mode: match col(r, 0).as_str() {
                "OUT" => ParamMode::Out,
                "INOUT" => ParamMode::InOut,
                _ => ParamMode::In,
            },
        })
        .collect();
    Ok(RoutineSignature {
        params,
        returns_table: false,
    })
}
