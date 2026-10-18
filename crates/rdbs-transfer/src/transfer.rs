//! Server-to-server data transfer and synchronisation.
//!
//! Tables are copied in foreign-key order (referenced tables first); tables created in the
//! target get their foreign keys at the end, so cycles are fine. Rows are read in pages and
//! written one transaction per page. Synchronisation compares rows by primary key and only
//! writes the differences.

use std::collections::HashMap;

use rdbs_core::dialect::{qualified_name, quote_identifier};
use rdbs_core::diff::convert_table;
use rdbs_core::edit::Change;
use rdbs_core::export::sql_literal;
use rdbs_core::job::{TransferMode, TransferOptions};
use rdbs_core::{
    CancelHandle, CellValue, DatabaseAdapter, DatabaseKind, DbError, DbErrorKind, TableSchema,
};

/// Progress report: a message and the overall fraction done (0–1).
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// What is happening.
    pub message: String,
    /// Fraction of tables done.
    pub fraction: f32,
}

/// Per-table outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableSummary {
    /// Table name.
    pub table: String,
    /// Rows inserted.
    pub inserted: u64,
    /// Rows updated (sync).
    pub updated: u64,
    /// Rows deleted (sync / replace).
    pub deleted: u64,
    /// Whether the table was created in the target.
    pub created: bool,
}

/// Human-readable summary of a finished transfer.
pub fn summary_text(tables: &[TableSummary]) -> String {
    let sum = |f: fn(&TableSummary) -> u64| tables.iter().map(f).sum::<u64>();
    let created = tables.iter().filter(|t| t.created).count();
    format!(
        "{} table(s): {} inserted, {} updated, {} deleted{}",
        tables.len(),
        sum(|t| t.inserted),
        sum(|t| t.updated),
        sum(|t| t.deleted),
        if created > 0 {
            format!(", {created} created")
        } else {
            String::new()
        }
    )
}

fn table_name(kind: DatabaseKind, t: &TableSchema) -> String {
    qualified_name(kind, None, t.schema.as_deref(), &t.name)
}

fn column_list(kind: DatabaseKind, cols: &[String]) -> String {
    cols.iter()
        .map(|c| quote_identifier(kind, c))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `SELECT cols FROM t ORDER BY key …` returning rows `offset..offset+limit`.
pub fn page_query(
    kind: DatabaseKind,
    t: &TableSchema,
    columns: &[String],
    offset: usize,
    limit: usize,
) -> String {
    let order = if t.primary_key.is_empty() {
        match kind {
            DatabaseKind::SqlServer => "(SELECT NULL)".to_owned(),
            _ => column_list(kind, &columns[..1.min(columns.len())]),
        }
    } else {
        column_list(kind, &t.primary_key)
    };
    let base = format!(
        "SELECT {} FROM {} ORDER BY {order}",
        column_list(kind, columns),
        table_name(kind, t)
    );
    match kind {
        DatabaseKind::SqlServer | DatabaseKind::Oracle => {
            format!("{base} OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
        }
        _ => format!("{base} LIMIT {limit} OFFSET {offset}"),
    }
}

/// Literal of a value for the target engine.
fn literal(value: &CellValue, kind: DatabaseKind) -> String {
    sql_literal(value, kind)
}

/// Whether two values are equal for synchronisation purposes (numbers compare numerically).
fn same_value(a: &CellValue, b: &CellValue, kind: DatabaseKind) -> bool {
    if a == b {
        return true;
    }
    let (la, lb) = (literal(a, kind), literal(b, kind));
    if la == lb {
        return true;
    }
    match (la.parse::<f64>(), lb.parse::<f64>()) {
        (Ok(x), Ok(y)) => (x - y).abs() <= f64::EPSILON * x.abs().max(y.abs()).max(1.0),
        _ => {
            // Booleans read back as integers (SQLite, SQL Server bit) or text.
            let norm = |v: &CellValue| match v {
                CellValue::Bool(b) => Some(i64::from(*b)),
                CellValue::Int(i @ (0 | 1)) => Some(*i),
                _ => None,
            };
            matches!((norm(a), norm(b)), (Some(x), Some(y)) if x == y)
        }
    }
}

fn insert_statement(
    kind: DatabaseKind,
    t: &TableSchema,
    columns: &[String],
    rows: &[&Vec<CellValue>],
    identity: bool,
) -> Change {
    let table = table_name(kind, t);
    let tuples = rows
        .iter()
        .map(|r| {
            format!(
                "({})",
                r.iter()
                    .map(|v| literal(v, kind))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let insert = format!(
        "INSERT INTO {table} ({}) VALUES {tuples}",
        column_list(kind, columns)
    );
    Change {
        sql: if identity {
            format!("SET IDENTITY_INSERT {table} ON; {insert}; SET IDENTITY_INSERT {table} OFF")
        } else {
            insert
        },
        expected_rows: rows.len() as u64,
    }
}

fn key_condition(kind: DatabaseKind, key_cols: &[String], values: &[&CellValue]) -> String {
    key_cols
        .iter()
        .zip(values)
        .map(|(c, v)| match v {
            CellValue::Null => format!("{} IS NULL", quote_identifier(kind, c)),
            v => format!("{} = {}", quote_identifier(kind, c), literal(v, kind)),
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Orders tables so that referenced tables come first (cycles keep their relative order).
pub fn dependency_order(tables: &[TableSchema]) -> Vec<TableSchema> {
    let mut remaining: Vec<TableSchema> = tables.to_vec();
    let mut out: Vec<TableSchema> = Vec::new();
    while !remaining.is_empty() {
        let ready = remaining.iter().position(|t| {
            t.foreign_keys.iter().all(|fk| {
                fk.ref_table == t.name
                    || out.iter().any(|o| o.name == fk.ref_table)
                    || !remaining.iter().any(|r| r.name == fk.ref_table)
            })
        });
        out.push(remaining.remove(ready.unwrap_or(0)));
    }
    out
}

async fn run_changes(target: &mut dyn DatabaseAdapter, changes: &[Change]) -> Result<u64, DbError> {
    if changes.is_empty() {
        return Ok(0);
    }
    target.apply_changes(changes).await
}

/// Reads all rows of `columns` from `t` page by page.
async fn read_pages(
    adapter: &mut dyn DatabaseAdapter,
    kind: DatabaseKind,
    t: &TableSchema,
    columns: &[String],
    page_rows: usize,
    cancel: &CancelHandle,
    mut each: impl FnMut(Vec<Vec<CellValue>>) -> Result<(), DbError>,
) -> Result<(), DbError> {
    let mut offset = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(DbError::cancelled());
        }
        let sql = page_query(kind, t, columns, offset, page_rows);
        let out = adapter.execute(&sql, cancel.clone()).await?;
        let rows = out
            .results
            .into_iter()
            .last()
            .map(|r| r.rows)
            .unwrap_or_default();
        let n = rows.len();
        each(rows)?;
        if n < page_rows {
            return Ok(());
        }
        offset += n;
    }
}

/// Copies or synchronises tables from `source` to `target`.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    source: &mut dyn DatabaseAdapter,
    source_kind: DatabaseKind,
    target: &mut dyn DatabaseAdapter,
    target_kind: DatabaseKind,
    options: &TransferOptions,
    cancel: &CancelHandle,
    mut progress: impl FnMut(Progress) + Send,
) -> Result<Vec<TableSummary>, DbError> {
    progress(Progress {
        message: "Reading table definitions…".into(),
        fraction: 0.0,
    });
    let source_tables = source
        .describe_schema(options.source_schema.as_deref())
        .await?;
    let mut target_tables = target
        .describe_schema(options.target_schema.as_deref())
        .await?;
    let selected: Vec<TableSchema> = source_tables
        .into_iter()
        .filter(|t| options.tables.is_empty() || options.tables.contains(&t.name))
        .collect();
    if selected.is_empty() {
        return Err(DbError::new(
            DbErrorKind::Query,
            "no source tables to transfer",
        ));
    }
    let ordered = dependency_order(&selected);
    let target_schema = if target_kind.has_schemas() {
        options.target_schema.clone()
    } else {
        None
    };
    let total = ordered.len() as f32;
    let mut summaries = Vec::new();
    let mut deferred_fks: Vec<String> = Vec::new();
    // Sync deletions run last, referencing tables first: (summary index, statements).
    let mut pending_deletes: Vec<(usize, Vec<Change>)> = Vec::new();

    for (index, src) in ordered.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(DbError::cancelled());
        }
        let fraction = index as f32 / total;
        progress(Progress {
            message: format!("{}: preparing", src.name),
            fraction,
        });
        let mut summary = TableSummary {
            table: src.name.clone(),
            ..TableSummary::default()
        };
        let wanted = convert_table(source_kind, src, target_kind, target_schema.as_deref());
        let existing = target_tables
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(&src.name))
            .cloned();

        // Make sure the target table exists with a usable shape.
        let recreate = options.mode == TransferMode::Recreate;
        let mut setup = Vec::new();
        if recreate && let Some(old) = &existing {
            setup.push(Change::unchecked(rdbs_core::ddlgen::drop_table(
                target_kind,
                old,
            )));
        }
        let dest = if existing.is_none() || recreate {
            if existing.is_none() && !options.create_missing {
                return Err(DbError::new(
                    DbErrorKind::Query,
                    format!("table {} does not exist in the target", src.name),
                ));
            }
            let without_fks = TableSchema {
                foreign_keys: Vec::new(),
                ..wanted.clone()
            };
            setup.extend(
                rdbs_core::ddlgen::create_table(target_kind, &without_fks)
                    .into_iter()
                    .map(Change::unchecked),
            );
            deferred_fks.extend(
                rdbs_core::diff::compare(
                    target_kind,
                    std::slice::from_ref(&wanted),
                    target_kind,
                    std::slice::from_ref(&without_fks),
                    target_schema.as_deref(),
                )
                .into_iter()
                .flat_map(|d| d.statements),
            );
            summary.created = true;
            target_tables.retain(|t| !t.name.eq_ignore_ascii_case(&src.name));
            target_tables.push(without_fks.clone());
            without_fks
        } else {
            existing.clone().unwrap_or_else(|| wanted.clone())
        };
        if matches!(options.mode, TransferMode::Replace) && existing.is_some() {
            setup.push(Change::unchecked(format!(
                "DELETE FROM {}",
                table_name(target_kind, &dest)
            )));
        }
        // MySQL commits DDL implicitly, so setup runs separately from the data.
        run_changes(target, &setup).await?;

        // Columns present on both sides, in source order.
        let columns: Vec<String> = src
            .columns
            .iter()
            .filter(|c| {
                dest.column(&c.name).is_some()
                    || dest
                        .columns
                        .iter()
                        .any(|d| d.name.eq_ignore_ascii_case(&c.name))
            })
            .map(|c| c.name.clone())
            .collect();
        let dest_columns: Vec<String> = columns
            .iter()
            .map(|c| {
                dest.columns
                    .iter()
                    .find(|d| d.name.eq_ignore_ascii_case(c))
                    .map_or_else(|| c.clone(), |d| d.name.clone())
            })
            .collect();
        if columns.is_empty() {
            summaries.push(summary);
            continue;
        }
        let identity = target_kind == DatabaseKind::SqlServer
            && dest
                .columns
                .iter()
                .any(|c| c.identity && dest_columns.contains(&c.name));

        match options.mode {
            TransferMode::Sync { delete_missing } => {
                let deletes = sync_table(
                    source,
                    source_kind,
                    src,
                    target,
                    target_kind,
                    &dest,
                    &columns,
                    &dest_columns,
                    identity,
                    delete_missing,
                    options,
                    cancel,
                    &mut summary,
                    &mut |m| {
                        progress(Progress {
                            message: m,
                            fraction,
                        })
                    },
                )
                .await?;
                pending_deletes.push((summaries.len(), deletes));
            }
            _ => {
                let mut offset = 0usize;
                loop {
                    if cancel.is_cancelled() {
                        return Err(DbError::cancelled());
                    }
                    let sql = page_query(source_kind, src, &columns, offset, options.page_rows);
                    let out = source.execute(&sql, cancel.clone()).await?;
                    let rows = out
                        .results
                        .into_iter()
                        .last()
                        .map(|r| r.rows)
                        .unwrap_or_default();
                    let n = rows.len();
                    if n > 0 {
                        let refs: Vec<&Vec<CellValue>> = rows.iter().collect();
                        let changes: Vec<Change> = refs
                            .chunks(options.batch_rows.max(1))
                            .map(|chunk| {
                                insert_statement(target_kind, &dest, &dest_columns, chunk, identity)
                            })
                            .collect();
                        summary.inserted += run_changes(target, &changes).await?;
                    }
                    progress(Progress {
                        message: format!("{}: {} row(s) copied", src.name, summary.inserted),
                        fraction,
                    });
                    if n < options.page_rows {
                        break;
                    }
                    offset += n;
                }
            }
        }

        // PostgreSQL identity sequences do not advance on explicit inserts.
        if target_kind == DatabaseKind::PostgreSql && summary.inserted > 0 {
            for c in dest
                .columns
                .iter()
                .filter(|c| c.identity && dest_columns.contains(&c.name))
            {
                let table_lit = table_name(target_kind, &dest).replace('\'', "''");
                let col = quote_identifier(target_kind, &c.name);
                let sql = format!(
                    "SELECT setval(pg_get_serial_sequence('{table_lit}', '{}'), \
                     COALESCE((SELECT MAX({col}) FROM {}), 0) + 1, false)",
                    c.name.replace('\'', "''"),
                    table_name(target_kind, &dest)
                );
                if let Err(e) = target.execute(&sql, cancel.clone()).await {
                    tracing::warn!(
                        "could not reset the sequence of {}.{}: {e}",
                        dest.name,
                        c.name
                    );
                }
            }
        }
        summaries.push(summary);
    }

    for (index, deletes) in pending_deletes.into_iter().rev() {
        if deletes.is_empty() {
            continue;
        }
        progress(Progress {
            message: format!(
                "{}: deleting {} row(s)",
                summaries[index].table,
                deletes.len()
            ),
            fraction: 0.98,
        });
        summaries[index].deleted += run_changes(target, &deletes).await?;
    }

    if !deferred_fks.is_empty() {
        progress(Progress {
            message: "Adding foreign keys…".into(),
            fraction: 0.99,
        });
        let changes: Vec<Change> = deferred_fks.into_iter().map(Change::unchecked).collect();
        run_changes(target, &changes).await?;
    }
    progress(Progress {
        message: summary_text(&summaries),
        fraction: 1.0,
    });
    Ok(summaries)
}

/// Brings the target rows in line with the source by primary key.
#[allow(clippy::too_many_arguments)]
async fn sync_table(
    source: &mut dyn DatabaseAdapter,
    source_kind: DatabaseKind,
    src: &TableSchema,
    target: &mut dyn DatabaseAdapter,
    target_kind: DatabaseKind,
    dest: &TableSchema,
    columns: &[String],
    dest_columns: &[String],
    identity: bool,
    delete_missing: bool,
    options: &TransferOptions,
    cancel: &CancelHandle,
    summary: &mut TableSummary,
    progress: &mut (dyn FnMut(String) + Send),
) -> Result<Vec<Change>, DbError> {
    if src.primary_key.is_empty() {
        return Err(DbError::new(
            DbErrorKind::Query,
            format!("{}: synchronisation needs a primary key", src.name),
        ));
    }
    let key_idx: Vec<usize> = src
        .primary_key
        .iter()
        .filter_map(|k| columns.iter().position(|c| c == k))
        .collect();
    if key_idx.len() != src.primary_key.len() {
        return Err(DbError::new(
            DbErrorKind::Query,
            format!(
                "{}: the primary key columns are missing in the target",
                src.name
            ),
        ));
    }
    let dest_key: Vec<String> = key_idx.iter().map(|i| dest_columns[*i].clone()).collect();
    let row_key = |row: &[CellValue]| -> String {
        key_idx
            .iter()
            .map(|i| literal(&row[*i], target_kind))
            .collect::<Vec<_>>()
            .join("\u{1f}")
    };

    // Current target rows by key. The target is read with the source's key order.
    progress(format!("{}: reading target rows", src.name));
    let mut existing: HashMap<String, Vec<CellValue>> = HashMap::new();
    let dest_for_read = TableSchema {
        primary_key: dest_key.clone(),
        ..dest.clone()
    };
    read_pages(
        target,
        target_kind,
        &dest_for_read,
        dest_columns,
        options.page_rows,
        cancel,
        |rows| {
            for row in rows {
                existing.insert(row_key(&row), row);
            }
            Ok(())
        },
    )
    .await?;

    progress(format!("{}: comparing", src.name));
    let mut offset = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(DbError::cancelled());
        }
        let sql = page_query(source_kind, src, columns, offset, options.page_rows);
        let out = source.execute(&sql, cancel.clone()).await?;
        let rows = out
            .results
            .into_iter()
            .last()
            .map(|r| r.rows)
            .unwrap_or_default();
        let n = rows.len();
        let mut inserts: Vec<&Vec<CellValue>> = Vec::new();
        let mut changes = Vec::new();
        for row in &rows {
            match existing.remove(&row_key(row)) {
                None => inserts.push(row),
                Some(current) => {
                    let differing: Vec<usize> = (0..columns.len())
                        .filter(|i| !key_idx.contains(i))
                        .filter(|i| !same_value(&row[*i], &current[*i], target_kind))
                        .collect();
                    if !differing.is_empty() {
                        let set = differing
                            .iter()
                            .map(|i| {
                                format!(
                                    "{} = {}",
                                    quote_identifier(target_kind, &dest_columns[*i]),
                                    literal(&row[*i], target_kind)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        let keys: Vec<&CellValue> = key_idx.iter().map(|i| &row[*i]).collect();
                        changes.push(Change {
                            sql: format!(
                                "UPDATE {} SET {set} WHERE {}",
                                table_name(target_kind, dest),
                                key_condition(target_kind, &dest_key, &keys)
                            ),
                            expected_rows: 1,
                        });
                        summary.updated += 1;
                    }
                }
            }
        }
        for chunk in inserts.chunks(options.batch_rows.max(1)) {
            changes.push(insert_statement(
                target_kind,
                dest,
                dest_columns,
                chunk,
                identity,
            ));
        }
        summary.inserted += inserts.len() as u64;
        run_changes(target, &changes).await?;
        progress(format!(
            "{}: {} inserted, {} updated",
            src.name, summary.inserted, summary.updated
        ));
        if n < options.page_rows {
            break;
        }
        offset += n;
    }

    if delete_missing && !existing.is_empty() {
        let changes: Vec<Change> = existing
            .values()
            .map(|row| {
                let keys: Vec<&CellValue> = key_idx.iter().map(|i| &row[*i]).collect();
                Change {
                    sql: format!(
                        "DELETE FROM {} WHERE {}",
                        table_name(target_kind, dest),
                        key_condition(target_kind, &dest_key, &keys)
                    ),
                    expected_rows: 1,
                }
            })
            .collect();
        return Ok(changes);
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdbs_core::{ColumnDef, ForeignKeyDef};

    fn t(name: &str, refs: &[&str]) -> TableSchema {
        TableSchema {
            name: name.into(),
            columns: vec![ColumnDef::new("id", "int")],
            primary_key: vec!["id".into()],
            foreign_keys: refs
                .iter()
                .map(|r| ForeignKeyDef {
                    name: format!("fk_{r}"),
                    columns: vec!["id".into()],
                    ref_schema: None,
                    ref_table: (*r).into(),
                    ref_columns: vec!["id".into()],
                })
                .collect(),
            ..TableSchema::default()
        }
    }

    #[test]
    fn parents_come_first() {
        let order: Vec<String> = dependency_order(&[
            t("items", &["orders"]),
            t("orders", &["customers"]),
            t("customers", &[]),
        ])
        .into_iter()
        .map(|t| t.name)
        .collect();
        assert_eq!(order, ["customers", "orders", "items"]);
        let cyclic = dependency_order(&[t("a", &["b"]), t("b", &["a"])]);
        assert_eq!(cyclic.len(), 2);
    }

    #[test]
    fn pages_per_dialect() {
        let table = t("x", &[]);
        let cols = vec!["id".to_owned()];
        assert_eq!(
            page_query(DatabaseKind::PostgreSql, &table, &cols, 10, 5),
            "SELECT \"id\" FROM \"x\" ORDER BY \"id\" LIMIT 5 OFFSET 10"
        );
        assert_eq!(
            page_query(DatabaseKind::SqlServer, &table, &cols, 0, 5),
            "SELECT [id] FROM [x] ORDER BY [id] OFFSET 0 ROWS FETCH NEXT 5 ROWS ONLY"
        );
    }

    #[test]
    fn values_compare_leniently() {
        let k = DatabaseKind::PostgreSql;
        assert!(same_value(
            &CellValue::Decimal("1.50".into()),
            &CellValue::Decimal("1.5".into()),
            k
        ));
        assert!(same_value(&CellValue::Bool(true), &CellValue::Int(1), k));
        assert!(!same_value(
            &CellValue::Text("a".into()),
            &CellValue::Text("b".into()),
            k
        ));
        assert!(same_value(&CellValue::Null, &CellValue::Null, k));
    }

    #[test]
    fn summary_reads_well() {
        let s = summary_text(&[TableSummary {
            table: "a".into(),
            inserted: 3,
            updated: 1,
            deleted: 0,
            created: true,
        }]);
        assert_eq!(s, "1 table(s): 3 inserted, 1 updated, 0 deleted, 1 created");
    }
}
