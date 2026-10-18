//! Serialising result sets to CSV, JSON and SQL `INSERT` statements.

use serde_json::{Map, Number, Value};

use crate::dialect::quote_identifier;
use crate::profile::DatabaseKind;
use crate::value::{CellValue, ResultSet};

/// Output format of an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExportFormat {
    /// RFC 4180 comma-separated values with a header row.
    Csv,
    /// A JSON array of objects keyed by column name.
    Json,
    /// One `INSERT` statement per row, in the connection's dialect.
    SqlInsert,
}

impl ExportFormat {
    /// All formats in menu order.
    pub const ALL: [ExportFormat; 3] = [Self::Csv, Self::Json, Self::SqlInsert];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Json => "JSON",
            Self::SqlInsert => "SQL INSERT",
        }
    }

    /// File extension without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
            Self::SqlInsert => "sql",
        }
    }
}

/// Serialises `rs`. `table` names the target table of SQL `INSERT`s.
pub fn export(rs: &ResultSet, format: ExportFormat, kind: DatabaseKind, table: &str) -> String {
    match format {
        ExportFormat::Csv => to_csv(rs),
        ExportFormat::Json => to_json(rs),
        ExportFormat::SqlInsert => to_sql_inserts(rs, kind, table),
    }
}

fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

/// Text of a cell for CSV: NULL is an empty field, binary is `0x…` hex.
fn plain(value: &CellValue) -> String {
    match value {
        CellValue::Null => String::new(),
        CellValue::Bytes(b) => hex(b, "0x"),
        other => other.to_string(),
    }
}

fn hex(bytes: &[u8], prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + bytes.len() * 2);
    out.push_str(prefix);
    for b in bytes {
        out.push_str(&format!("{b:02X}"));
    }
    out
}

/// RFC 4180 CSV with CRLF line endings and a header row.
pub fn to_csv(rs: &ResultSet) -> String {
    let mut out = String::new();
    let header: Vec<String> = rs.columns.iter().map(|c| csv_field(&c.name)).collect();
    out.push_str(&header.join(","));
    out.push_str("\r\n");
    for row in &rs.rows {
        let fields: Vec<String> = row.iter().map(|v| csv_field(&plain(v))).collect();
        out.push_str(&fields.join(","));
        out.push_str("\r\n");
    }
    out
}

fn json_value(value: &CellValue) -> Value {
    match value {
        CellValue::Null => Value::Null,
        CellValue::Bool(b) => Value::Bool(*b),
        CellValue::Int(i) => Value::Number((*i).into()),
        CellValue::Float(f) => Number::from_f64(*f).map_or(Value::Null, Value::Number),
        // Exact numerics stay strings so no precision is lost in JSON parsers.
        CellValue::Decimal(s) | CellValue::Text(s) | CellValue::Temporal(s) => {
            Value::String(s.clone())
        }
        CellValue::Json(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone())),
        CellValue::Bytes(b) => Value::String(hex(b, "0x")),
        CellValue::Other { display, .. } => Value::String(display.clone()),
    }
}

/// Pretty-printed JSON array of objects.
pub fn to_json(rs: &ResultSet) -> String {
    let rows: Vec<Value> = rs
        .rows
        .iter()
        .map(|row| {
            let mut obj = Map::new();
            for (col, value) in rs.columns.iter().zip(row) {
                obj.insert(col.name.clone(), json_value(value));
            }
            Value::Object(obj)
        })
        .collect();
    serde_json::to_string_pretty(&Value::Array(rows)).unwrap_or_else(|_| "[]".to_owned())
}

/// Quoted string literal for `kind` (N-prefixed on SQL Server).
pub fn sql_string(s: &str, kind: DatabaseKind) -> String {
    let escaped = s.replace('\'', "''");
    match kind {
        DatabaseKind::SqlServer => format!("N'{escaped}'"),
        DatabaseKind::MySql | DatabaseKind::MariaDb => {
            format!("'{}'", escaped.replace('\\', "\\\\"))
        }
        _ => format!("'{escaped}'"),
    }
}

/// SQL literal for a cell value in the dialect of `kind`.
pub fn sql_literal(value: &CellValue, kind: DatabaseKind) -> String {
    match value {
        CellValue::Null => "NULL".to_owned(),
        CellValue::Bool(b) => match kind {
            DatabaseKind::SqlServer | DatabaseKind::Sqlite | DatabaseKind::Oracle => {
                u8::from(*b).to_string()
            }
            _ => if *b { "TRUE" } else { "FALSE" }.to_owned(),
        },
        CellValue::Int(i) => i.to_string(),
        CellValue::Float(f) if f.is_finite() => f.to_string(),
        CellValue::Decimal(s) if s.parse::<f64>().is_ok() => s.clone(),
        CellValue::Bytes(b) => match kind {
            DatabaseKind::PostgreSql => format!("'{}'::bytea", hex(b, "\\x")),
            DatabaseKind::MySql | DatabaseKind::MariaDb | DatabaseKind::Sqlite => {
                format!("X'{}'", hex(b, ""))
            }
            DatabaseKind::SqlServer => hex(b, "0x"),
            DatabaseKind::Oracle => format!("HEXTORAW('{}')", hex(b, "")),
            DatabaseKind::MongoDb | DatabaseKind::Redis => format!("'{}'", hex(b, "0x")),
        },
        CellValue::Temporal(t) if kind == DatabaseKind::Oracle => oracle_temporal(t),
        other => sql_string(&other.to_string(), kind),
    }
}

/// Oracle date/time literal: `DATE '…'` / `TIMESTAMP '…'` (independent of NLS settings).
fn oracle_temporal(text: &str) -> String {
    let t = text.trim();
    let b = t.as_bytes();
    let date_prefix =
        b.len() >= 10 && b[4] == b'-' && b[7] == b'-' && t[..4].parse::<u16>().is_ok();
    if date_prefix && b.len() == 10 {
        format!("DATE '{t}'")
    } else if date_prefix && matches!(b.get(10), Some(b' ' | b'T')) {
        format!(
            "TIMESTAMP '{}'",
            t.replacen('T', " ", 1).replace('\'', "''")
        )
    } else {
        sql_string(t, DatabaseKind::Oracle)
    }
}

/// One `INSERT INTO table (cols) VALUES (…);` per row.
pub fn to_sql_inserts(rs: &ResultSet, kind: DatabaseKind, table: &str) -> String {
    let columns: Vec<String> = rs
        .columns
        .iter()
        .map(|c| quote_identifier(kind, &c.name))
        .collect();
    let prefix = format!(
        "INSERT INTO {} ({}) VALUES (",
        quote_identifier(kind, table),
        columns.join(", ")
    );
    let mut out = String::new();
    for row in &rs.rows {
        let values: Vec<String> = row.iter().map(|v| sql_literal(v, kind)).collect();
        out.push_str(&prefix);
        out.push_str(&values.join(", "));
        out.push_str(");\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::ColumnMeta;

    fn sample() -> ResultSet {
        ResultSet {
            columns: vec![
                ColumnMeta::new("id", "int4"),
                ColumnMeta::new("name", "text"),
                ColumnMeta::new("ok", "bool"),
                ColumnMeta::new("raw", "bytea"),
                ColumnMeta::new("doc", "jsonb"),
                ColumnMeta::new("amount", "numeric"),
            ],
            rows: vec![
                vec![
                    CellValue::Int(1),
                    CellValue::Text("O'Neil, \"Bob\"".into()),
                    CellValue::Bool(true),
                    CellValue::Bytes(vec![0xde, 0xad]),
                    CellValue::Json("{\"a\":1}".into()),
                    CellValue::Decimal("12345678901234567890.10".into()),
                ],
                vec![
                    CellValue::Int(2),
                    CellValue::Null,
                    CellValue::Bool(false),
                    CellValue::Null,
                    CellValue::Null,
                    CellValue::Null,
                ],
            ],
        }
    }

    #[test]
    fn csv_quotes_and_escapes() {
        let csv = to_csv(&sample());
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(lines[0], "id,name,ok,raw,doc,amount");
        assert_eq!(
            lines[1],
            "1,\"O'Neil, \"\"Bob\"\"\",true,0xDEAD,\"{\"\"a\"\":1}\",12345678901234567890.10"
        );
        assert_eq!(lines[2], "2,,false,,,");
    }

    #[test]
    fn json_keeps_types_and_precision() {
        let parsed: Value = serde_json::from_str(&to_json(&sample())).unwrap();
        assert_eq!(parsed[0]["id"], 1);
        assert_eq!(parsed[0]["ok"], true);
        assert_eq!(parsed[0]["doc"]["a"], 1);
        assert_eq!(parsed[0]["amount"], "12345678901234567890.10");
        assert_eq!(parsed[1]["name"], Value::Null);
    }

    #[test]
    fn sql_inserts_follow_dialect() {
        let pg = to_sql_inserts(&sample(), DatabaseKind::PostgreSql, "people");
        assert!(pg.starts_with(
            "INSERT INTO \"people\" (\"id\", \"name\", \"ok\", \"raw\", \"doc\", \"amount\") VALUES (1, 'O''Neil, \"Bob\"', TRUE, '\\xDEAD'::bytea, '{\"a\":1}', 12345678901234567890.10);"
        ), "{pg}");
        let ms = to_sql_inserts(&sample(), DatabaseKind::SqlServer, "people");
        assert!(ms.contains("[people]") && ms.contains("N'O''Neil") && ms.contains(", 1, 0xDEAD"));
        let my = to_sql_inserts(&sample(), DatabaseKind::MySql, "people");
        assert!(my.contains("`people`") && my.contains("X'DEAD'"));
        assert_eq!(pg.lines().count(), 2);
    }
}
