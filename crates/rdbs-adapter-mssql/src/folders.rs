//! The Programmability and Security folders of the current database.
//!
//! Every category is one catalog query returning schema (or empty), name and a short detail.
//! They run as one `UNION ALL`; when that fails (a view missing on an older server, or one the
//! login may not read), each category runs alone and a failing one shows why instead.

use rdbs_core::schema::{PROGRAMMABILITY_FOLDER, SECURITY_FOLDER};
use rdbs_core::{DbObject, ObjectKind};

use crate::MssqlClient;
use crate::catalog::{col, text_rows};

/// One sub-folder: its name, what its entries are, and the query listing them.
struct Category {
    folder: &'static str,
    kind: ObjectKind,
    /// Columns: schema name (or `''`), object name, detail (or `''`).
    sql: &'static str,
}

const PROGRAMMABILITY: &[Category] = &[
    Category {
        folder: "Stored Procedures",
        kind: ObjectKind::Procedure,
        sql: "SELECT s.name, o.name, '' FROM sys.objects o \
              JOIN sys.schemas s ON s.schema_id = o.schema_id \
              WHERE o.is_ms_shipped = 0 AND o.type IN ('P', 'PC')",
    },
    Category {
        folder: "Functions",
        kind: ObjectKind::Function,
        sql: "SELECT s.name, o.name, CASE RTRIM(o.type) WHEN 'FN' THEN 'scalar' \
                WHEN 'IF' THEN 'inline table' WHEN 'TF' THEN 'table' ELSE 'CLR' END \
              FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id \
              WHERE o.is_ms_shipped = 0 AND o.type IN ('FN', 'IF', 'TF', 'FS', 'FT')",
    },
    Category {
        folder: "Database Triggers",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT '', name, CASE WHEN is_disabled = 1 THEN 'disabled' ELSE '' END \
              FROM sys.triggers WHERE parent_class = 0",
    },
    Category {
        folder: "Assemblies",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT '', name, LOWER(REPLACE(permission_set_desc, '_', ' ')) \
              FROM sys.assemblies WHERE is_user_defined = 1",
    },
    Category {
        folder: "Types",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT s.name, t.name, CASE WHEN t.is_table_type = 1 THEN 'table type' \
                ELSE TYPE_NAME(t.system_type_id) END \
              FROM sys.types t JOIN sys.schemas s ON s.schema_id = t.schema_id \
              WHERE t.is_user_defined = 1",
    },
    Category {
        folder: "Rules",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT s.name, o.name, '' FROM sys.objects o \
              JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE o.type = 'R'",
    },
    Category {
        folder: "Defaults",
        kind: ObjectKind::CatalogEntry,
        // Stand-alone defaults; default constraints have a parent table.
        sql: "SELECT s.name, o.name, '' FROM sys.objects o \
              JOIN sys.schemas s ON s.schema_id = o.schema_id \
              WHERE o.type = 'D' AND o.parent_object_id = 0",
    },
    Category {
        folder: "Plan Guides",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT '', name, CASE WHEN is_disabled = 1 THEN 'disabled' \
                ELSE LOWER(scope_type_desc) END FROM sys.plan_guides",
    },
    Category {
        folder: "Sequences",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT s.name, q.name, TYPE_NAME(q.user_type_id) FROM sys.sequences q \
              JOIN sys.schemas s ON s.schema_id = q.schema_id",
    },
];

const SECURITY: &[Category] = &[
    Category {
        folder: "Users",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT '', name, LOWER(REPLACE(type_desc, '_', ' ')) \
              FROM sys.database_principals WHERE type IN ('S', 'U', 'G', 'E', 'X', 'C', 'K')",
    },
    Category {
        folder: "Roles",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT '', name, LOWER(REPLACE(type_desc, '_', ' ')) \
              FROM sys.database_principals WHERE type IN ('R', 'A')",
    },
    Category {
        folder: "Schemas",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT '', s.name, ISNULL('owner ' + p.name, '') FROM sys.schemas s \
              LEFT JOIN sys.database_principals p ON p.principal_id = s.principal_id",
    },
    Category {
        folder: "Asymmetric Keys",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT '', name, LOWER(algorithm_desc) FROM sys.asymmetric_keys",
    },
    Category {
        folder: "Certificates",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT '', name, ISNULL(subject, '') FROM sys.certificates",
    },
    Category {
        folder: "Symmetric Keys",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT '', name, LOWER(algorithm_desc) FROM sys.symmetric_keys",
    },
    Category {
        folder: "Always Encrypted Keys",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT '', name, 'column master key' FROM sys.column_master_keys \
              UNION ALL SELECT '', name, 'column encryption key' \
              FROM sys.column_encryption_keys",
    },
    Category {
        folder: "Database Audit Specifications",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT '', name, CASE WHEN is_state_enabled = 1 THEN 'enabled' \
                ELSE 'disabled' END FROM sys.database_audit_specifications",
    },
    Category {
        folder: "Security Policies",
        kind: ObjectKind::CatalogEntry,
        sql: "SELECT s.name, p.name, CASE WHEN p.is_enabled = 1 THEN 'enabled' \
                ELSE 'disabled' END FROM sys.security_policies p \
              JOIN sys.schemas s ON s.schema_id = p.schema_id",
    },
];

/// The Programmability and Security folders of the current database.
pub async fn load(client: &mut MssqlClient) -> Vec<DbObject> {
    let all: Vec<&Category> = PROGRAMMABILITY.iter().chain(SECURITY).collect();
    let mut entries: Vec<Result<Vec<DbObject>, String>> =
        match text_rows(client, &combined(&all)).await {
            Ok(rows) => {
                let mut lists = vec![Vec::new(); all.len()];
                for row in &rows {
                    if let Some(i) = col(row, 0).parse::<usize>().ok().filter(|i| *i < all.len()) {
                        lists[i].push(entry(all[i].kind, &row[1..]));
                    }
                }
                lists.into_iter().map(Ok).collect()
            }
            Err(_) => {
                let mut lists = Vec::new();
                for category in &all {
                    lists.push(match text_rows(client, category.sql).await {
                        Ok(rows) => Ok(rows.iter().map(|r| entry(category.kind, r)).collect()),
                        Err(e) => Err(e.message),
                    });
                }
                lists
            }
        };
    let security = entries.split_off(PROGRAMMABILITY.len());
    vec![
        folder(PROGRAMMABILITY_FOLDER, PROGRAMMABILITY, entries),
        folder(SECURITY_FOLDER, SECURITY, security),
    ]
}

/// Every category in one query, tagged with its index in `all`.
fn combined(all: &[&Category]) -> String {
    all.iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                "SELECT {i} AS k, x.s, x.n, x.d FROM ({}) AS x(s, n, d)",
                c.sql
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

/// A tree entry from (schema, name, detail).
fn entry(kind: ObjectKind, row: &[Option<String>]) -> DbObject {
    let (schema, name, detail) = (col(row, 0), col(row, 1), col(row, 2));
    let mut obj = DbObject::new(name, kind);
    if !schema.is_empty() {
        obj = obj.with_schema(schema);
    }
    if !detail.is_empty() {
        obj = obj.with_detail(detail);
    }
    obj
}

/// A top folder with one sub-folder per category, entries sorted by their shown name.
fn folder(
    name: &str,
    categories: &[Category],
    entries: Vec<Result<Vec<DbObject>, String>>,
) -> DbObject {
    let children = categories
        .iter()
        .zip(entries)
        .map(|(category, list)| {
            let items = match list {
                Ok(mut items) => {
                    items.sort_by_key(|o| o.display_name().to_lowercase());
                    items
                }
                Err(message) => vec![DbObject::new(
                    format!("Not available: {message}"),
                    ObjectKind::Notice,
                )],
            };
            DbObject::new(category.folder, ObjectKind::Folder).with_children(items)
        })
        .collect();
    DbObject::new(name, ObjectKind::Folder).with_children(children)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_query_tags_each_category() {
        let all: Vec<&Category> = PROGRAMMABILITY.iter().chain(SECURITY).collect();
        let sql = combined(&all);
        assert_eq!(sql.matches(" AS k, ").count(), all.len());
        assert!(sql.starts_with("SELECT 0 AS k, x.s, x.n, x.d FROM (SELECT s.name"));
        assert!(sql.contains(&format!("SELECT {} AS k", all.len() - 1)));
    }

    #[test]
    fn entries_carry_schema_and_detail() {
        let row = vec![
            Some("dbo".into()),
            Some("usp_load".into()),
            Some(String::new()),
        ];
        let obj = entry(ObjectKind::Procedure, &row);
        assert_eq!(obj.display_name(), "dbo.usp_load");
        assert_eq!(obj.detail, None);

        let folders = folder(
            SECURITY_FOLDER,
            &SECURITY[..2],
            vec![
                Ok(vec![
                    DbObject::new("public", ObjectKind::CatalogEntry),
                    DbObject::new("dbo", ObjectKind::CatalogEntry),
                ]),
                Err("permission denied".into()),
            ],
        );
        assert_eq!(folders.children[0].name, "Users");
        assert_eq!(folders.children[0].children[0].name, "dbo", "sorted");
        assert_eq!(folders.children[1].children[0].kind, ObjectKind::Notice);
    }
}
