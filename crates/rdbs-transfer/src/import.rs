//! Reading CSV and spreadsheet files, and turning their rows into batched `INSERT`s.

use std::path::Path;

use calamine::{Data, Reader, open_workbook_auto};
use rdbs_core::dialect::{qualified_name, quote_identifier};
use rdbs_core::edit::Change;
use rdbs_core::types::{TypeClass, classify, infer_type, text_literal};
use rdbs_core::{ColumnDef, DatabaseKind, TableSchema};

/// Rows per `INSERT … VALUES` statement (SQLite's compound limit is 500, SQL Server's 1000).
pub const BATCH_ROWS: usize = 500;

/// A file's content as text cells.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sheet {
    /// Column names (generated as `column1…` when the file has no header row).
    pub headers: Vec<String>,
    /// Data rows; shorter rows are padded with empty strings.
    pub rows: Vec<Vec<String>>,
}

/// How to read a file.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadOptions {
    /// Whether the first row holds column names.
    pub header: bool,
    /// CSV field delimiter (`None` = detect from the first line).
    pub delimiter: Option<u8>,
    /// Worksheet to read (`None` = first one).
    pub sheet: Option<String>,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            header: true,
            delimiter: None,
            sheet: None,
        }
    }
}

/// Whether `path` is a spreadsheet (read with calamine) rather than delimited text.
pub fn is_spreadsheet(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "xlsx" | "xlsm" | "xlsb" | "xls" | "ods"
        )
    })
}

/// Worksheet names of a spreadsheet.
pub fn sheet_names(path: &Path) -> Result<Vec<String>, String> {
    let workbook =
        open_workbook_auto(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    Ok(workbook.sheet_names())
}

/// Reads a CSV/TSV or spreadsheet file.
pub fn read_file(path: &Path, options: &ReadOptions) -> Result<Sheet, String> {
    if is_spreadsheet(path) {
        read_spreadsheet(path, options)
    } else {
        let bytes =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        read_csv(&decode(&bytes), options)
    }
}

/// UTF-8 (with or without BOM), else Windows-1252 — what spreadsheet tools usually write.
pub fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

/// The delimiter among `, ; \t |` that occurs most often in the first line.
pub fn detect_delimiter(text: &str) -> u8 {
    let first = text.lines().next().unwrap_or("");
    b",;\t|"
        .iter()
        .copied()
        .max_by_key(|d| first.bytes().filter(|b| b == d).count())
        .filter(|d| first.as_bytes().contains(d))
        .unwrap_or(b',')
}

fn finish(mut rows: Vec<Vec<String>>, header: bool) -> Sheet {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    for row in &mut rows {
        row.resize(width, String::new());
    }
    let headers = if header && !rows.is_empty() {
        let mut names: Vec<String> = rows.remove(0);
        for (i, name) in names.iter_mut().enumerate() {
            if name.trim().is_empty() {
                *name = format!("column{}", i + 1);
            } else {
                *name = name.trim().to_owned();
            }
        }
        names
    } else {
        (1..=width).map(|i| format!("column{i}")).collect()
    };
    Sheet { headers, rows }
}

/// Parses delimited text (RFC 4180 quoting).
pub fn read_csv(text: &str, options: &ReadOptions) -> Result<Sheet, String> {
    let delimiter = options.delimiter.unwrap_or_else(|| detect_delimiter(text));
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut rows = Vec::new();
    for (i, record) in reader.records().enumerate() {
        let record = record.map_err(|e| format!("line {}: {e}", i + 1))?;
        // Skip completely empty lines.
        if record.iter().all(str::is_empty) && record.len() <= 1 {
            continue;
        }
        rows.push(record.iter().map(str::to_owned).collect());
    }
    Ok(finish(rows, options.header))
}

fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
        Data::Float(f) => f.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => match dt.as_datetime() {
            Some(t) if t.time() == chrono::NaiveTime::MIN => t.format("%Y-%m-%d").to_string(),
            Some(t) => t.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => dt.as_f64().to_string(),
        },
        Data::Error(e) => format!("#{e:?}"),
    }
}

fn read_spreadsheet(path: &Path, options: &ReadOptions) -> Result<Sheet, String> {
    let mut workbook =
        open_workbook_auto(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let name = match &options.sheet {
        Some(name) => name.clone(),
        None => workbook
            .sheet_names()
            .first()
            .cloned()
            .ok_or_else(|| "the workbook has no sheets".to_owned())?,
    };
    let range = workbook
        .worksheet_range(&name)
        .map_err(|e| format!("cannot read sheet {name}: {e}"))?;
    let rows = range
        .rows()
        .filter(|r| r.iter().any(|c| *c != Data::Empty))
        .map(|r| r.iter().map(cell_text).collect())
        .collect();
    Ok(finish(rows, options.header))
}

/// A table definition for importing `sheet` into a new table, with types inferred from the
/// first 1 000 rows.
pub fn table_for_sheet(
    kind: DatabaseKind,
    schema: Option<String>,
    name: &str,
    sheet: &Sheet,
) -> TableSchema {
    let sample = &sheet.rows[..sheet.rows.len().min(1_000)];
    TableSchema {
        schema,
        name: name.to_owned(),
        columns: sheet
            .headers
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let values = sample.iter().map(|r| r[i].as_str());
                ColumnDef::new(h.clone(), infer_type(kind, values))
            })
            .collect(),
        ..TableSchema::default()
    }
}

/// Default mapping: target column ← source column with the same name (case-insensitive).
pub fn auto_map(target: &TableSchema, sheet: &Sheet) -> Vec<Option<usize>> {
    target
        .columns
        .iter()
        .map(|c| {
            sheet
                .headers
                .iter()
                .position(|h| h.eq_ignore_ascii_case(&c.name))
        })
        .collect()
}

/// A value that cannot be converted for its column.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("row {row}, column {column}: {message}")]
pub struct ImportError {
    /// 1-based data row.
    pub row: usize,
    /// Target column.
    pub column: String,
    /// What is wrong.
    pub message: String,
}

/// Import settings.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportOptions {
    /// Empty cells become NULL (otherwise empty strings).
    pub empty_is_null: bool,
    /// Rows per INSERT.
    pub batch_rows: usize,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            empty_is_null: true,
            batch_rows: BATCH_ROWS,
        }
    }
}

/// Batched INSERTs for the mapped columns. `mapping[i]` is the source column for target
/// column `i` (`None` = not imported). Each change expects exactly its row count.
pub fn build_inserts(
    kind: DatabaseKind,
    target: &TableSchema,
    sheet: &Sheet,
    mapping: &[Option<usize>],
    options: &ImportOptions,
) -> Result<Vec<Change>, ImportError> {
    let pairs: Vec<(usize, usize)> = mapping
        .iter()
        .enumerate()
        .filter_map(|(t, s)| s.map(|s| (t, s)))
        .filter(|(t, _)| *t < target.columns.len())
        .collect();
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let classes: Vec<TypeClass> = pairs
        .iter()
        .map(|(t, _)| classify(&target.columns[*t].data_type))
        .collect();
    let table = qualified_name(kind, None, target.schema.as_deref(), &target.name);
    let columns = pairs
        .iter()
        .map(|(t, _)| quote_identifier(kind, &target.columns[*t].name))
        .collect::<Vec<_>>()
        .join(", ");
    // SQL Server refuses explicit identity values unless IDENTITY_INSERT is on. The setting
    // must be in the same batch: statements run through sp_executesql, which resets it.
    let identity =
        kind == DatabaseKind::SqlServer && pairs.iter().any(|(t, _)| target.columns[*t].identity);
    let mut out = Vec::new();
    let batch = options.batch_rows.max(1);
    for (chunk_index, chunk) in sheet.rows.chunks(batch).enumerate() {
        let mut tuples = Vec::with_capacity(chunk.len());
        for (offset, row) in chunk.iter().enumerate() {
            let row_number = chunk_index * batch + offset + 1;
            let mut values = Vec::with_capacity(pairs.len());
            for ((t, s), class) in pairs.iter().zip(&classes) {
                let column = &target.columns[*t];
                let text = row.get(*s).map_or("", String::as_str);
                let literal =
                    text_literal(kind, *class, text, options.empty_is_null).map_err(|message| {
                        ImportError {
                            row: row_number,
                            column: column.name.clone(),
                            message,
                        }
                    })?;
                if literal == "NULL"
                    && !column.nullable
                    && column.default.is_none()
                    && !column.identity
                {
                    return Err(ImportError {
                        row: row_number,
                        column: column.name.clone(),
                        message: "the column does not allow NULL".into(),
                    });
                }
                values.push(literal);
            }
            tuples.push(format!("({})", values.join(", ")));
        }
        let insert = format!(
            "INSERT INTO {table} ({columns}) VALUES {}",
            tuples.join(", ")
        );
        out.push(Change {
            sql: if identity {
                format!("SET IDENTITY_INSERT {table} ON; {insert}; SET IDENTITY_INSERT {table} OFF")
            } else {
                insert
            },
            expected_rows: chunk.len() as u64,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_delimiters_and_quotes() {
        assert_eq!(detect_delimiter("a;b;c\n1;2;3"), b';');
        assert_eq!(detect_delimiter("a\tb\n"), b'\t');
        assert_eq!(detect_delimiter("single"), b',');
        let sheet = read_csv(
            "id,name,note\n1,\"Doe, Jane\",\"said \"\"hi\"\"\"\n2,Bob\n\n",
            &ReadOptions::default(),
        )
        .unwrap();
        assert_eq!(sheet.headers, ["id", "name", "note"]);
        assert_eq!(sheet.rows.len(), 2);
        assert_eq!(sheet.rows[0][1], "Doe, Jane");
        assert_eq!(sheet.rows[0][2], "said \"hi\"");
        assert_eq!(sheet.rows[1], ["2", "Bob", ""], "short rows are padded");
    }

    #[test]
    fn headerless_files_get_generated_names() {
        let options = ReadOptions {
            header: false,
            ..ReadOptions::default()
        };
        let sheet = read_csv("1;x\n2;y", &options).unwrap();
        assert_eq!(sheet.headers, ["column1", "column2"]);
        assert_eq!(sheet.rows.len(), 2);
    }

    #[test]
    fn decodes_bom_and_windows_1252() {
        assert_eq!(decode(b"\xEF\xBB\xBFa,b"), "a,b");
        assert_eq!(decode(b"caf\xE9"), "café");
    }

    fn target() -> TableSchema {
        TableSchema {
            schema: Some("dbo".into()),
            name: "people".into(),
            columns: vec![
                ColumnDef {
                    identity: true,
                    nullable: false,
                    ..ColumnDef::new("id", "int")
                },
                ColumnDef {
                    nullable: false,
                    ..ColumnDef::new("name", "nvarchar(50)")
                },
                ColumnDef::new("active", "bit"),
            ],
            ..TableSchema::default()
        }
    }

    #[test]
    fn builds_batches_with_identity_insert() {
        let sheet = Sheet {
            headers: vec!["ID".into(), "Name".into(), "Active".into()],
            rows: (1..=3)
                .map(|i| vec![i.to_string(), format!("p{i}"), "yes".into()])
                .collect(),
        };
        let mapping = auto_map(&target(), &sheet);
        assert_eq!(mapping, [Some(0), Some(1), Some(2)]);
        let options = ImportOptions {
            batch_rows: 2,
            ..ImportOptions::default()
        };
        let changes = build_inserts(
            DatabaseKind::SqlServer,
            &target(),
            &sheet,
            &mapping,
            &options,
        )
        .unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(
            changes[0].sql,
            "SET IDENTITY_INSERT [dbo].[people] ON; INSERT INTO [dbo].[people] ([id], [name], [active]) VALUES (1, N'p1', 1), (2, N'p2', 1); SET IDENTITY_INSERT [dbo].[people] OFF"
        );
        assert_eq!(changes[0].expected_rows, 2);
        assert_eq!(changes[1].expected_rows, 1);
    }

    #[test]
    fn reports_the_offending_cell() {
        let sheet = Sheet {
            headers: vec!["name".into(), "active".into()],
            rows: vec![
                vec!["a".into(), "yes".into()],
                vec!["".into(), "maybe".into()],
            ],
        };
        let mapping = vec![None, Some(0), Some(1)];
        let err = build_inserts(
            DatabaseKind::PostgreSql,
            &target(),
            &sheet,
            &mapping,
            &ImportOptions::default(),
        )
        .unwrap_err();
        assert_eq!(err.row, 2);
        assert_eq!(err.column, "name");
        assert!(err.message.contains("NULL"));
    }

    #[test]
    fn new_tables_get_inferred_types() {
        let sheet = Sheet {
            headers: vec!["id".into(), "price".into(), "label".into()],
            rows: vec![vec!["1".into(), "9.5".into(), "x".into()]],
        };
        let t = table_for_sheet(
            DatabaseKind::PostgreSql,
            Some("public".into()),
            "imp",
            &sheet,
        );
        let types: Vec<&str> = t.columns.iter().map(|c| c.data_type.as_str()).collect();
        assert_eq!(types, ["bigint", "double precision", "varchar(32)"]);
    }
}
