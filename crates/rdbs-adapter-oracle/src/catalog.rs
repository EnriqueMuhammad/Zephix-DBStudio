//! Object tree, table definitions, DDL and plans from the Oracle data dictionary.

use std::collections::{BTreeMap, HashMap};

use rdbs_core::plan::{ExplainPlan, PlanNode};
use rdbs_core::schema::ObjectPath;
use rdbs_core::{
    ColumnDef, DbError, DbErrorKind, DbObject, ForeignKeyDef, IndexDef, ObjectKind, SchemaContents,
    TableSchema,
};

use crate::map_error;

type Rows = Vec<Vec<Option<String>>>;

/// Runs a query and returns all values as text.
fn text_rows(
    conn: &oracle::Connection,
    sql: &str,
    params: &[&dyn oracle::sql_type::ToSql],
) -> Result<Rows, DbError> {
    let rows = conn.query(sql, params).map_err(map_error)?;
    let width = rows.column_info().len();
    let mut out = Vec::new();
    for row in rows {
        let row = row.map_err(map_error)?;
        out.push(
            (0..width)
                .map(|i| row.get::<usize, Option<String>>(i))
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_error)?,
        );
    }
    Ok(out)
}

fn col(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

fn current_schema(conn: &oracle::Connection) -> Result<String, DbError> {
    Ok(text_rows(
        conn,
        "SELECT SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA') FROM dual",
        &[],
    )?
    .first()
    .map(|r| col(r, 0))
    .unwrap_or_default())
}

/// User schemas (not Oracle-maintained) plus the current one.
const USER_OWNERS: &str = "(SELECT username FROM all_users WHERE oracle_maintained = 'N' \
     UNION SELECT SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA') FROM dual)";

/// The explorer tree: the service as database, schemas inside.
pub fn load(conn: &oracle::Connection) -> Result<Vec<DbObject>, DbError> {
    let service = text_rows(
        conn,
        "SELECT SYS_CONTEXT('USERENV', 'SERVICE_NAME') FROM dual",
        &[],
    )?
    .first()
    .map(|r| col(r, 0))
    .unwrap_or_else(|| "ORACLE".into());
    let objects = text_rows(
        conn,
        &format!(
            "SELECT owner, object_name, object_type FROM all_objects \
             WHERE owner IN {USER_OWNERS} \
               AND object_type IN ('TABLE', 'VIEW', 'MATERIALIZED VIEW', 'FUNCTION', 'PROCEDURE') \
               AND object_name NOT LIKE 'BIN$%' AND object_name NOT LIKE 'DR$%' \
             ORDER BY owner, object_name"
        ),
        &[],
    )?;
    let columns = text_rows(
        conn,
        &format!(
            "SELECT c.owner, c.table_name, c.column_name, c.data_type, c.nullable, \
                    CASE WHEN EXISTS (SELECT 1 FROM all_cons_columns cc \
                         JOIN all_constraints k ON k.owner = cc.owner AND k.constraint_name = cc.constraint_name \
                         WHERE k.constraint_type = 'P' AND cc.owner = c.owner \
                           AND cc.table_name = c.table_name AND cc.column_name = c.column_name) \
                    THEN 'Y' ELSE 'N' END \
             FROM all_tab_columns c WHERE c.owner IN {USER_OWNERS} \
             ORDER BY c.owner, c.table_name, c.column_id"
        ),
        &[],
    )?;
    let rows_estimate = text_rows(
        conn,
        &format!("SELECT owner, table_name, num_rows FROM all_tables WHERE owner IN {USER_OWNERS}"),
        &[],
    )?;
    let estimate: HashMap<(String, String), i64> = rows_estimate
        .iter()
        .filter_map(|r| Some(((col(r, 0), col(r, 1)), col(r, 2).parse().ok()?)))
        .collect();
    let mut by_table: HashMap<(String, String), Vec<DbObject>> = HashMap::new();
    for r in &columns {
        let mut detail = col(r, 3);
        if col(r, 5) == "Y" {
            detail.push_str(" · PK");
        } else if col(r, 4) == "N" {
            detail.push_str(" · NOT NULL");
        }
        by_table
            .entry((col(r, 0), col(r, 1)))
            .or_default()
            .push(DbObject::new(col(r, 2), ObjectKind::Column).with_detail(detail));
    }
    let mut schemas: BTreeMap<String, SchemaContents> = BTreeMap::new();
    for r in &objects {
        let (owner, name, kind) = (col(r, 0), col(r, 1), col(r, 2));
        let group = schemas.entry(owner.clone()).or_default();
        let children = by_table
            .remove(&(owner.clone(), name.clone()))
            .unwrap_or_default();
        match kind.as_str() {
            "TABLE" => {
                let mut obj =
                    DbObject::new(name.clone(), ObjectKind::Table).with_children(children);
                if let Some(label) = estimate
                    .get(&(owner, name))
                    .and_then(|n| rdbs_core::approx_rows_label(*n))
                {
                    obj = obj.with_detail(label);
                }
                group.tables.push(obj);
            }
            "VIEW" => group
                .views
                .push(DbObject::new(name, ObjectKind::View).with_children(children)),
            "MATERIALIZED VIEW" => group
                .materialized_views
                .push(DbObject::new(name, ObjectKind::MaterializedView).with_children(children)),
            "FUNCTION" => group
                .functions
                .push(DbObject::new(name, ObjectKind::Function)),
            _ => group
                .procedures
                .push(DbObject::new(name, ObjectKind::Procedure)),
        }
    }
    let current = current_schema(conn)?;
    schemas.entry(current).or_default();
    Ok(vec![
        DbObject::new(service, ObjectKind::Database).with_children(
            schemas
                .into_iter()
                .map(|(name, c)| {
                    DbObject::new(name, ObjectKind::Schema).with_children(c.into_folders())
                })
                .collect(),
        ),
    ])
}

/// `VARCHAR2` + lengths → `VARCHAR2(50 CHAR)`, `NUMBER` + precision/scale → `NUMBER(12,2)`.
pub(crate) fn format_type(
    data_type: &str,
    data_length: &str,
    char_length: &str,
    char_used: &str,
    precision: &str,
    scale: &str,
) -> String {
    match data_type {
        "VARCHAR2" | "CHAR" => {
            let unit = if char_used == "C" { " CHAR" } else { " BYTE" };
            let n = if char_used == "C" {
                char_length
            } else {
                data_length
            };
            format!("{data_type}({n}{unit})")
        }
        "NVARCHAR2" | "NCHAR" => format!("{data_type}({char_length})"),
        "RAW" => format!("RAW({data_length})"),
        "NUMBER" => match (precision, scale) {
            ("", "") => "NUMBER".into(),
            ("", "0") => "INTEGER".into(),
            (p, "0") | (p, "") => format!("NUMBER({p})"),
            (p, s) => format!("NUMBER({p},{s})"),
        },
        "FLOAT" if !precision.is_empty() => format!("FLOAT({precision})"),
        other => other.to_owned(),
    }
}

/// Structured definitions of the tables of `schema` (default: the current schema).
pub fn describe(
    conn: &oracle::Connection,
    schema: Option<&str>,
) -> Result<Vec<TableSchema>, DbError> {
    let owner = match schema {
        Some(s) if !s.is_empty() => s.to_owned(),
        _ => current_schema(conn)?,
    };
    let tables = text_rows(
        conn,
        "SELECT table_name FROM all_tables WHERE owner = :1 AND nested = 'NO' \
           AND table_name NOT LIKE 'BIN$%' AND (iot_type IS NULL OR iot_type = 'IOT') \
         ORDER BY table_name",
        &[&owner],
    )?;
    let mut out: Vec<TableSchema> = tables
        .iter()
        .map(|r| TableSchema {
            schema: Some(owner.clone()),
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
        "SELECT table_name, column_name, data_type, data_length, char_length, char_used, \
                data_precision, data_scale, nullable, data_default, identity_column \
         FROM all_tab_columns WHERE owner = :1 ORDER BY table_name, column_id",
        &[&owner],
    )?;
    for r in &columns {
        let Some(&t) = index.get(&col(r, 0)) else {
            continue;
        };
        let identity = col(r, 10) == "YES";
        let default = r
            .get(9)
            .cloned()
            .flatten()
            .map(|d| d.trim().to_owned())
            .filter(|d| !d.is_empty() && !d.eq_ignore_ascii_case("NULL") && !identity);
        out[t].columns.push(ColumnDef {
            name: col(r, 1),
            data_type: format_type(
                &col(r, 2),
                &col(r, 3),
                &col(r, 4),
                &col(r, 5),
                &col(r, 6),
                &col(r, 7),
            ),
            nullable: col(r, 8) != "N",
            default,
            identity,
        });
    }

    let constraints = text_rows(
        conn,
        "SELECT c.table_name, c.constraint_name, c.constraint_type, cc.column_name, \
                r.owner, r.table_name, rc.column_name \
         FROM all_constraints c \
         JOIN all_cons_columns cc ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name \
         LEFT JOIN all_constraints r ON r.owner = c.r_owner AND r.constraint_name = c.r_constraint_name \
         LEFT JOIN all_cons_columns rc ON rc.owner = r.owner AND rc.constraint_name = r.constraint_name \
                                      AND rc.position = cc.position \
         WHERE c.owner = :1 AND c.constraint_type IN ('P', 'U', 'R') \
         ORDER BY c.table_name, c.constraint_name, cc.position",
        &[&owner],
    )?;
    for r in &constraints {
        let Some(&t) = index.get(&col(r, 0)) else {
            continue;
        };
        let name = col(r, 1);
        let column = col(r, 3);
        match col(r, 2).as_str() {
            "P" => {
                out[t].primary_key_name = Some(name);
                out[t].primary_key.push(column);
            }
            "U" => match out[t].indexes.iter_mut().find(|i| i.name == name) {
                Some(ix) => ix.columns.push(column),
                None => out[t].indexes.push(IndexDef {
                    constraint: true,
                    ..IndexDef::new(name, vec![column], true)
                }),
            },
            _ => {
                let position = match out[t].foreign_keys.iter().position(|f| f.name == name) {
                    Some(i) => i,
                    None => {
                        let ref_owner = col(r, 4);
                        out[t].foreign_keys.push(ForeignKeyDef {
                            name,
                            columns: Vec::new(),
                            ref_schema: (ref_owner != owner).then_some(ref_owner),
                            ref_table: col(r, 5),
                            ref_columns: Vec::new(),
                        });
                        out[t].foreign_keys.len() - 1
                    }
                };
                let fk = &mut out[t].foreign_keys[position];
                fk.columns.push(column);
                fk.ref_columns.push(col(r, 6));
            }
        }
    }

    let indexes = text_rows(
        conn,
        "SELECT i.table_name, i.index_name, i.uniqueness, ic.column_name \
         FROM all_indexes i \
         JOIN all_ind_columns ic ON ic.index_owner = i.owner AND ic.index_name = i.index_name \
         WHERE i.table_owner = :1 AND i.index_type = 'NORMAL' \
           AND NOT EXISTS (SELECT 1 FROM all_constraints c WHERE c.owner = i.table_owner \
                           AND c.index_name = i.index_name AND c.constraint_type IN ('P', 'U')) \
         ORDER BY i.table_name, i.index_name, ic.column_position",
        &[&owner],
    )?;
    for r in &indexes {
        let Some(&t) = index.get(&col(r, 0)) else {
            continue;
        };
        let name = col(r, 1);
        match out[t].indexes.iter_mut().find(|i| i.name == name) {
            Some(ix) => ix.columns.push(col(r, 3)),
            None => {
                out[t]
                    .indexes
                    .push(IndexDef::new(name, vec![col(r, 3)], col(r, 2) == "UNIQUE"))
            }
        }
    }
    Ok(out)
}

/// `DBMS_METADATA.GET_DDL` for tables, views and routines.
pub fn object_ddl(conn: &oracle::Connection, object: &ObjectPath) -> Result<String, DbError> {
    let kind = match object.kind {
        ObjectKind::Table => "TABLE",
        ObjectKind::View => "VIEW",
        ObjectKind::MaterializedView => "MATERIALIZED_VIEW",
        ObjectKind::Function => "FUNCTION",
        ObjectKind::Procedure => "PROCEDURE",
        _ => {
            return Err(DbError::new(
                DbErrorKind::Unsupported,
                "DDL is available for tables, views and routines",
            ));
        }
    };
    let owner = match &object.schema {
        Some(s) => s.clone(),
        None => current_schema(conn)?,
    };
    let rows = text_rows(
        conn,
        "SELECT DBMS_METADATA.GET_DDL(:1, :2, :3) FROM dual",
        &[&kind, &object.name, &owner],
    )?;
    let ddl = rows.first().map(|r| col(r, 0)).unwrap_or_default();
    Ok(format!("{};", ddl.trim()))
}

/// `EXPLAIN PLAN` + `PLAN_TABLE` as a tree.
pub fn explain(conn: &oracle::Connection, statement: &str) -> Result<ExplainPlan, DbError> {
    let id = format!("RDBS{}", std::process::id());
    conn.execute(
        &format!("DELETE FROM plan_table WHERE statement_id = '{id}'"),
        &[],
    )
    .map_err(map_error)?;
    conn.execute(
        &format!("EXPLAIN PLAN SET STATEMENT_ID = '{id}' FOR {statement}"),
        &[],
    )
    .map_err(map_error)?;
    let rows = text_rows(
        conn,
        "SELECT id, parent_id, operation, options, object_owner, object_name, cost, cardinality, \
                access_predicates, filter_predicates \
         FROM plan_table WHERE statement_id = :1 ORDER BY id",
        &[&id],
    )?;
    let _ = conn.execute(
        &format!("DELETE FROM plan_table WHERE statement_id = '{id}'"),
        &[],
    );
    let raw = rows
        .iter()
        .map(|r| {
            format!(
                "{:>3} {:>3}  {} {}  {}.{}  cost={} rows={}",
                col(r, 0),
                col(r, 1),
                col(r, 2),
                col(r, 3),
                col(r, 4),
                col(r, 5),
                col(r, 6),
                col(r, 7)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    fn build(rows: &Rows, parent: &str) -> Vec<PlanNode> {
        rows.iter()
            .filter(|r| col(r, 1) == parent)
            .map(|r| {
                let mut node =
                    PlanNode::new(format!("{} {}", col(r, 2), col(r, 3)).trim().to_owned());
                let name = col(r, 5);
                if !name.is_empty() {
                    node.object = Some(format!("{}.{}", col(r, 4), name));
                }
                node.cost = col(r, 6).parse().ok();
                node.rows = col(r, 7).parse().ok();
                for (label, i) in [("Access", 8), ("Filter", 9)] {
                    let text = col(r, i);
                    if !text.is_empty() {
                        node.details.push((label.to_owned(), text));
                    }
                }
                node.children = build(rows, &col(r, 0));
                node
            })
            .collect()
    }
    Ok(ExplainPlan {
        roots: build(&rows, ""),
        raw,
        analyzed: false,
        notes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_dictionary_types() {
        assert_eq!(
            format_type("VARCHAR2", "200", "50", "C", "", ""),
            "VARCHAR2(50 CHAR)"
        );
        assert_eq!(
            format_type("VARCHAR2", "50", "50", "B", "", ""),
            "VARCHAR2(50 BYTE)"
        );
        assert_eq!(
            format_type("NUMBER", "22", "0", "", "12", "2"),
            "NUMBER(12,2)"
        );
        assert_eq!(
            format_type("NUMBER", "22", "0", "", "10", "0"),
            "NUMBER(10)"
        );
        assert_eq!(format_type("NUMBER", "22", "0", "", "", ""), "NUMBER");
        assert_eq!(
            format_type("TIMESTAMP(6)", "11", "0", "", "", "6"),
            "TIMESTAMP(6)"
        );
    }
}
