//! Logical backups: a database (or schema) written as a SQL script that recreates the tables
//! and their rows on an empty database of the same engine. Plus schedule timing.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Timelike};
use rdbs_core::dialect::{qualified_name, quote_identifier};
use rdbs_core::export::sql_literal;
use rdbs_core::job::{BackupOptions, Frequency};
use rdbs_core::{CancelHandle, DatabaseAdapter, DatabaseKind, DbError, DbErrorKind, TableSchema};

use crate::transfer::{Progress, dependency_order, page_query};

/// Rows per page read from the server.
const PAGE_ROWS: usize = 5_000;
/// Rows per INSERT statement in the dump.
const INSERT_ROWS: usize = 200;

/// Replaces `{date}` (YYYYMMDD) and `{time}` (HHMMSS) in a path template.
pub fn expand_path(template: &Path, at: DateTime<Local>) -> PathBuf {
    let text = template.to_string_lossy();
    PathBuf::from(
        text.replace("{date}", &at.format("%Y%m%d").to_string())
            .replace("{time}", &at.format("%H%M%S").to_string()),
    )
}

/// Whether `name` could have been produced from the file-name template `pattern`.
fn matches_template(pattern: &str, name: &str) -> bool {
    // Tokens expand to digits of fixed width.
    let mut p = pattern.to_owned();
    p = p.replace("{date}", "\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}");
    p = p.replace("{time}", "\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}");
    let (p, n): (Vec<char>, Vec<char>) = (p.chars().collect(), name.chars().collect());
    p.len() == n.len()
        && p.iter().zip(&n).all(|(a, b)| match a {
            '\u{1}' => b.is_ascii_digit(),
            other => other.eq_ignore_ascii_case(b),
        })
}

/// Deletes the oldest files produced by `template` so that at most `keep` remain.
pub fn prune(template: &Path, keep: usize) -> std::io::Result<usize> {
    if keep == 0 {
        return Ok(0);
    }
    let Some(dir) = template.parent() else {
        return Ok(0);
    };
    let pattern = template
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !pattern.contains('{') {
        return Ok(0); // fixed name: the file is simply overwritten
    }
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| matches_template(&pattern, &e.file_name().to_string_lossy()))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    let mut removed = 0;
    for (_, path) in files.into_iter().skip(keep) {
        std::fs::remove_file(path)?;
        removed += 1;
    }
    Ok(removed)
}

fn io_error(e: std::io::Error) -> DbError {
    DbError::new(
        DbErrorKind::Internal,
        format!("could not write the backup: {e}"),
    )
}

/// Writes the dump. Returns the file written and a summary.
pub async fn dump(
    adapter: &mut dyn DatabaseAdapter,
    kind: DatabaseKind,
    options: &BackupOptions,
    cancel: &CancelHandle,
    mut progress: impl FnMut(Progress) + Send,
) -> Result<(PathBuf, String), DbError> {
    let started = Local::now();
    let path = expand_path(&options.path, started);
    if path.as_os_str().is_empty() {
        return Err(DbError::new(DbErrorKind::Internal, "no backup file chosen"));
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(io_error)?;
    }
    progress(Progress {
        message: "Reading table definitions…".into(),
        fraction: 0.0,
    });
    let tables: Vec<TableSchema> = adapter
        .describe_schema(options.schema.as_deref())
        .await?
        .into_iter()
        .filter(|t| options.tables.is_empty() || options.tables.contains(&t.name))
        .collect();
    let tables = dependency_order(&tables);

    // Write to a temporary file first so a failed run never replaces a good backup.
    let temp = path.with_extension("partial");
    let file = std::fs::File::create(&temp).map_err(io_error)?;
    let mut out = std::io::BufWriter::new(file);
    let result: Result<(usize, u64), DbError> = async {
        writeln!(
            out,
            "-- Zephix - Database Management Studio backup\n-- Server: {}\n-- Created: {}\n-- Tables: {}\n",
            adapter.server_version(),
            started.format("%Y-%m-%d %H:%M:%S %z"),
            tables.len()
        )
        .map_err(io_error)?;
        if options.include_schema {
            for t in &tables {
                // SQLite cannot add foreign keys later, but accepts forward references.
                let without_fks = TableSchema {
                    foreign_keys: if kind == DatabaseKind::Sqlite {
                        t.foreign_keys.clone()
                    } else {
                        Vec::new()
                    },
                    ..t.clone()
                };
                for s in rdbs_core::ddlgen::create_table(kind, &without_fks) {
                    writeln!(out, "{s};\n").map_err(io_error)?;
                }
            }
        }
        let mut rows_total = 0u64;
        if options.include_data {
            let total = tables.len().max(1) as f32;
            for (i, t) in tables.iter().enumerate() {
                let columns: Vec<String> = t.columns.iter().map(|c| c.name.clone()).collect();
                if columns.is_empty() {
                    continue;
                }
                let table = qualified_name(kind, None, t.schema.as_deref(), &t.name);
                let column_list = columns
                    .iter()
                    .map(|c| quote_identifier(kind, c))
                    .collect::<Vec<_>>()
                    .join(", ");
                let identity = kind == DatabaseKind::SqlServer && t.columns.iter().any(|c| c.identity);
                writeln!(out, "-- Data of {table}").map_err(io_error)?;
                if identity {
                    writeln!(out, "SET IDENTITY_INSERT {table} ON;").map_err(io_error)?;
                }
                let mut offset = 0;
                loop {
                    if cancel.is_cancelled() {
                        return Err(DbError::cancelled());
                    }
                    let sql = page_query(kind, t, &columns, offset, PAGE_ROWS);
                    let page = adapter.execute(&sql, cancel.clone()).await?;
                    let rows = page.results.into_iter().last().map(|r| r.rows).unwrap_or_default();
                    for chunk in rows.chunks(INSERT_ROWS) {
                        let tuples = chunk
                            .iter()
                            .map(|r| {
                                format!(
                                    "({})",
                                    r.iter().map(|v| sql_literal(v, kind)).collect::<Vec<_>>().join(", ")
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(",\n  ");
                        writeln!(out, "INSERT INTO {table} ({column_list}) VALUES\n  {tuples};")
                            .map_err(io_error)?;
                    }
                    rows_total += rows.len() as u64;
                    progress(Progress {
                        message: format!("{}: {} row(s)", t.name, rows_total),
                        fraction: i as f32 / total,
                    });
                    if rows.len() < PAGE_ROWS {
                        break;
                    }
                    offset += rows.len();
                }
                if identity {
                    writeln!(out, "SET IDENTITY_INSERT {table} OFF;").map_err(io_error)?;
                }
                if kind == DatabaseKind::PostgreSql {
                    for c in t.columns.iter().filter(|c| c.identity) {
                        let col = quote_identifier(kind, &c.name);
                        writeln!(
                            out,
                            "SELECT setval(pg_get_serial_sequence('{}', '{}'), COALESCE((SELECT MAX({col}) FROM {table}), 0) + 1, false);",
                            table.replace('\'', "''"),
                            c.name.replace('\'', "''")
                        )
                        .map_err(io_error)?;
                    }
                }
                writeln!(out).map_err(io_error)?;
            }
        }
        if options.include_schema {
            for t in &tables {
                for fk in &t.foreign_keys {
                    let q = |s: &str| quote_identifier(kind, s);
                    let cols = |c: &[String]| c.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
                    if kind == DatabaseKind::Sqlite {
                        continue; // created inline above
                    }
                    writeln!(
                        out,
                        "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({});",
                        qualified_name(kind, None, t.schema.as_deref(), &t.name),
                        q(&fk.name),
                        cols(&fk.columns),
                        qualified_name(kind, None, fk.ref_schema.as_deref().or(t.schema.as_deref()), &fk.ref_table),
                        cols(&fk.ref_columns)
                    )
                    .map_err(io_error)?;
                }
            }
        }
        out.flush().map_err(io_error)?;
        Ok((tables.len(), rows_total))
    }
    .await;
    drop(out);
    match result {
        Ok((tables, rows)) => {
            std::fs::rename(&temp, &path).map_err(io_error)?;
            if let Err(e) = prune(&options.path, options.keep) {
                tracing::warn!("could not remove old backups: {e}");
            }
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            Ok((
                path.clone(),
                format!(
                    "{tables} table(s), {rows} row(s), {} KB → {}",
                    size.div_ceil(1024),
                    path.display()
                ),
            ))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

fn at(day: DateTime<Local>, hour: u32, minute: u32) -> DateTime<Local> {
    let time = NaiveTime::from_hms_opt(hour.min(23), minute.min(59), 0).unwrap_or(NaiveTime::MIN);
    let naive = day.date_naive().and_time(time);
    Local.from_local_datetime(&naive).earliest().unwrap_or(day)
}

/// The first run time strictly after `after`.
pub fn next_run(frequency: Frequency, after: DateTime<Local>) -> DateTime<Local> {
    match frequency {
        Frequency::EveryHours { hours, minute } => {
            // At least `hours` hours after the previous run, on the given minute.
            let threshold = after + Duration::hours(i64::from(hours.clamp(1, 24)) - 1);
            let mut candidate = at(threshold, threshold.hour(), minute);
            while candidate <= threshold {
                candidate += Duration::hours(1);
            }
            candidate
        }
        Frequency::Daily { hour, minute } => {
            let mut candidate = at(after, hour, minute);
            while candidate <= after {
                candidate = at(candidate + Duration::days(1), hour, minute);
            }
            candidate
        }
        Frequency::Weekly {
            weekday,
            hour,
            minute,
        } => {
            let mut candidate = at(after, hour, minute);
            while candidate <= after || candidate.weekday().num_days_from_monday() != weekday % 7 {
                candidate = at(candidate + Duration::days(1), hour, minute);
            }
            candidate
        }
    }
}

/// Whether a schedule last run at `last_run` (Unix seconds) is due at `now`.
pub fn is_due(frequency: Frequency, last_run: i64, now: DateTime<Local>) -> bool {
    let last = Local.timestamp_opt(last_run, 0).single().unwrap_or(now);
    next_run(frequency, last) <= now
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid local time")
    }

    #[test]
    fn daily_and_weekly_times() {
        let daily = Frequency::Daily {
            hour: 2,
            minute: 30,
        };
        assert_eq!(
            next_run(daily, local(2026, 10, 16, 1, 0)),
            local(2026, 10, 16, 2, 30)
        );
        assert_eq!(
            next_run(daily, local(2026, 10, 16, 2, 30)),
            local(2026, 10, 17, 2, 30)
        );
        // 2026-10-16 is a Friday; weekday 0 = Monday.
        let weekly = Frequency::Weekly {
            weekday: 0,
            hour: 9,
            minute: 0,
        };
        assert_eq!(
            next_run(weekly, local(2026, 10, 16, 12, 0)),
            local(2026, 10, 19, 9, 0)
        );
    }

    #[test]
    fn hourly_intervals() {
        let hourly = Frequency::EveryHours {
            hours: 1,
            minute: 15,
        };
        assert_eq!(
            next_run(hourly, local(2026, 10, 16, 10, 20)),
            local(2026, 10, 16, 11, 15)
        );
        assert_eq!(
            next_run(hourly, local(2026, 10, 16, 10, 10)),
            local(2026, 10, 16, 10, 15)
        );
        let every6 = Frequency::EveryHours {
            hours: 6,
            minute: 0,
        };
        let next = next_run(every6, local(2026, 10, 16, 10, 0));
        assert_eq!(next, local(2026, 10, 16, 16, 0));
    }

    #[test]
    fn due_after_the_next_run_time() {
        let daily = Frequency::Daily { hour: 2, minute: 0 };
        let last = local(2026, 10, 15, 3, 0).timestamp();
        assert!(!is_due(daily, last, local(2026, 10, 16, 1, 59)));
        assert!(is_due(daily, last, local(2026, 10, 16, 2, 0)));
    }

    #[test]
    fn paths_and_retention() {
        let t = local(2026, 10, 16, 8, 5);
        assert_eq!(
            expand_path(Path::new("C:/bk/shop_{date}_{time}.sql"), t),
            PathBuf::from("C:/bk/shop_20261016_080500.sql")
        );
        assert!(matches_template(
            "shop_{date}_{time}.sql",
            "shop_20261016_080500.sql"
        ));
        assert!(!matches_template(
            "shop_{date}_{time}.sql",
            "other_20261016_080500.sql"
        ));

        let dir = std::env::temp_dir().join(format!("rdbs-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        for i in 0..4 {
            std::fs::write(dir.join(format!("db_2026101{i}.sql")), "x").expect("file");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::fs::write(dir.join("unrelated.sql"), "x").expect("file");
        let removed = prune(&dir.join("db_{date}.sql"), 2).expect("prune");
        assert_eq!(removed, 2);
        assert!(dir.join("db_20261013.sql").exists());
        assert!(!dir.join("db_20261010.sql").exists());
        assert!(dir.join("unrelated.sql").exists());
    }
}
