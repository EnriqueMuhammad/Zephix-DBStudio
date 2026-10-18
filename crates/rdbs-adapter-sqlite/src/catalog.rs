//! Object tree, table definitions and DDL from `sqlite_master` and the `pragma_*` functions.

use rdbs_core::schema::ObjectPath;
use rdbs_core::{
    ColumnDef, DbError, DbErrorKind, DbObject, ForeignKeyDef, IndexDef, ObjectKind, SchemaContents,
    TableSchema,
};
use rusqlite::{Connection, params};

use crate::map_error;

/// Tables and views created by the user (internal `sqlite_*` objects excluded).
const USER_OBJECTS: &str = "SELECT type, name, sql FROM sqlite_master \
     WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name";

/// One `pragma_table_info` row.
struct PragmaColumn {
    name: String,
    data_type: String,
    not_null: bool,
    default: Option<String>,
    /// Position in the primary key (1-based), 0 when not part of it.
    pk: i64,
}

fn table_info(conn: &Connection, table: &str) -> Result<Vec<PragmaColumn>, DbError> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1) ORDER BY cid",
        )
        .map_err(map_error)?;
    stmt.query_map(params![table], |r| {
        Ok(PragmaColumn {
            name: r.get(0)?,
            data_type: r.get(1)?,
            not_null: r.get(2)?,
            default: r.get(3)?,
            pk: r.get(4)?,
        })
    })
    .and_then(Iterator::collect)
    .map_err(map_error)
}

/// `(type, name, sql)` of user tables and views.
fn user_objects(conn: &Connection) -> Result<Vec<(String, String, Option<String>)>, DbError> {
    let mut stmt = conn.prepare(USER_OBJECTS).map_err(map_error)?;
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .and_then(Iterator::collect)
        .map_err(map_error)
}

/// The explorer tree: one `main` database with Tables / Views folders.
pub fn load(conn: &mut Connection) -> Result<Vec<DbObject>, DbError> {
    let mut contents = SchemaContents::default();
    for (kind, name, _) in user_objects(conn)? {
        let columns = table_info(conn, &name)?
            .into_iter()
            .map(|c| {
                let mut detail = if c.data_type.is_empty() {
                    "ANY".to_owned()
                } else {
                    c.data_type
                };
                if c.pk > 0 {
                    detail.push_str(" · PK");
                } else if c.not_null {
                    detail.push_str(" · NOT NULL");
                }
                DbObject::new(c.name, ObjectKind::Column).with_detail(detail)
            })
            .collect();
        if kind == "view" {
            contents
                .views
                .push(DbObject::new(name, ObjectKind::View).with_children(columns));
        } else {
            contents
                .tables
                .push(DbObject::new(name, ObjectKind::Table).with_children(columns));
        }
    }
    Ok(vec![
        DbObject::new("main", ObjectKind::Database).with_children(contents.into_folders()),
    ])
}

/// Structured definitions of all user tables.
pub fn describe(conn: &mut Connection) -> Result<Vec<TableSchema>, DbError> {
    let mut out = Vec::new();
    for (kind, name, _) in user_objects(conn)? {
        if kind != "table" {
            continue;
        }
        out.push(describe_table(conn, &name)?);
    }
    Ok(out)
}

fn describe_table(conn: &Connection, name: &str) -> Result<TableSchema, DbError> {
    let info = table_info(conn, name)?;
    let mut pk: Vec<&PragmaColumn> = info.iter().filter(|c| c.pk > 0).collect();
    pk.sort_by_key(|c| c.pk);
    // A single INTEGER PRIMARY KEY is the rowid: auto-assigned and never NULL.
    let rowid = (pk.len() == 1 && pk[0].data_type.eq_ignore_ascii_case("INTEGER"))
        .then(|| pk[0].name.clone());
    let primary_key = pk.iter().map(|c| c.name.clone()).collect();
    let columns = info
        .iter()
        .map(|c| {
            let is_rowid = rowid.as_deref() == Some(c.name.as_str());
            ColumnDef {
                name: c.name.clone(),
                data_type: c.data_type.clone(),
                nullable: !c.not_null && !is_rowid,
                default: c.default.clone().filter(|_| !is_rowid),
                identity: is_rowid,
            }
        })
        .collect();

    let mut indexes = Vec::new();
    let mut stmt = conn
        .prepare_cached(
            "SELECT name, \"unique\", origin, partial FROM pragma_index_list(?1) ORDER BY name",
        )
        .map_err(map_error)?;
    let list: Vec<(String, bool, String, bool)> = stmt
        .query_map(params![name], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .and_then(Iterator::collect)
        .map_err(map_error)?;
    for (index, unique, origin, partial) in list {
        if origin == "pk" || partial {
            continue;
        }
        let mut cols = conn
            .prepare_cached("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")
            .map_err(map_error)?;
        let columns: Vec<Option<String>> = cols
            .query_map(params![index], |r| r.get(0))
            .and_then(Iterator::collect)
            .map_err(map_error)?;
        // Expression indexes have unnamed columns.
        let Some(columns) = columns.into_iter().collect::<Option<Vec<String>>>() else {
            continue;
        };
        indexes.push(IndexDef {
            constraint: origin == "u",
            ..IndexDef::new(index, columns, unique)
        });
    }

    let mut stmt = conn
        .prepare_cached(
            "SELECT id, \"table\", \"from\", \"to\" FROM pragma_foreign_key_list(?1) \
             ORDER BY id, seq",
        )
        .map_err(map_error)?;
    let rows: Vec<(i64, String, String, Option<String>)> = stmt
        .query_map(params![name], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .and_then(Iterator::collect)
        .map_err(map_error)?;
    let mut foreign_keys: Vec<(i64, ForeignKeyDef)> = Vec::new();
    for (id, ref_table, from, to) in rows {
        let position = match foreign_keys.iter().position(|(i, _)| *i == id) {
            Some(p) => p,
            None => {
                foreign_keys.push((
                    id,
                    ForeignKeyDef {
                        // SQLite does not keep foreign key names.
                        name: format!("fk_{name}_{id}"),
                        columns: Vec::new(),
                        ref_schema: None,
                        ref_table,
                        ref_columns: Vec::new(),
                    },
                ));
                foreign_keys.len() - 1
            }
        };
        let fk = &mut foreign_keys[position].1;
        fk.columns.push(from);
        if let Some(to) = to {
            fk.ref_columns.push(to);
        }
    }
    let mut foreign_keys: Vec<ForeignKeyDef> = foreign_keys.into_iter().map(|(_, f)| f).collect();
    // `REFERENCES t` without columns points at t's primary key.
    for fk in &mut foreign_keys {
        if fk.ref_columns.is_empty() {
            let mut target: Vec<PragmaColumn> = table_info(conn, &fk.ref_table)?
                .into_iter()
                .filter(|c| c.pk > 0)
                .collect();
            target.sort_by_key(|c| c.pk);
            fk.ref_columns = target.into_iter().map(|c| c.name).collect();
        }
    }
    foreign_keys.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(TableSchema {
        schema: None,
        name: name.to_owned(),
        columns,
        primary_key,
        primary_key_name: None,
        indexes,
        foreign_keys,
    })
}

/// The stored `CREATE` statement of an object (tables include their indexes and triggers).
pub fn object_ddl(conn: &mut Connection, object: &ObjectPath) -> Result<String, DbError> {
    let not_found = || DbError::new(DbErrorKind::Query, format!("{} was not found", object.name));
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = ?1 AND type IN ('table', 'view')",
            params![object.name],
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => not_found(),
            other => map_error(other),
        })?;
    let mut parts = vec![format!("{};", sql.ok_or_else(not_found)?)];
    if object.kind == ObjectKind::Table {
        let mut stmt = conn
            .prepare(
                "SELECT sql FROM sqlite_master WHERE tbl_name = ?1 \
                 AND type IN ('index', 'trigger') AND sql IS NOT NULL ORDER BY type, name",
            )
            .map_err(map_error)?;
        let extra: Vec<String> = stmt
            .query_map(params![object.name], |r| r.get(0))
            .and_then(Iterator::collect)
            .map_err(map_error)?;
        parts.extend(extra.into_iter().map(|s| format!("{s};")));
    }
    Ok(parts.join("\n\n"))
}
