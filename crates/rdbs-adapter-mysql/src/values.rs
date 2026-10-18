//! Conversion of MySQL/MariaDB values into [`CellValue`] and native type names.

use mysql_async::Value;
use mysql_async::consts::{ColumnFlags, ColumnType};
use rdbs_core::CellValue;

/// MySQL's pseudo character set used for binary strings.
const BINARY_CHARSET: u16 = 63;

/// Logical class of a column, derived once per result set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Integer types.
    Int,
    /// Exact numerics.
    Decimal,
    /// FLOAT / DOUBLE.
    Float,
    /// Date and time types.
    Temporal,
    /// JSON documents.
    Json,
    /// Binary strings and blobs.
    Binary,
    /// Everything else, shown as text.
    Text,
}

/// Classifies a column from its protocol type and character set.
pub fn classify(ty: ColumnType, charset: u16) -> Class {
    use ColumnType::*;
    match ty {
        MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_LONG | MYSQL_TYPE_LONGLONG
        | MYSQL_TYPE_INT24 | MYSQL_TYPE_YEAR => Class::Int,
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => Class::Decimal,
        MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => Class::Float,
        MYSQL_TYPE_DATE
        | MYSQL_TYPE_NEWDATE
        | MYSQL_TYPE_TIME
        | MYSQL_TYPE_TIME2
        | MYSQL_TYPE_DATETIME
        | MYSQL_TYPE_DATETIME2
        | MYSQL_TYPE_TIMESTAMP
        | MYSQL_TYPE_TIMESTAMP2 => Class::Temporal,
        MYSQL_TYPE_JSON => Class::Json,
        MYSQL_TYPE_BIT | MYSQL_TYPE_GEOMETRY => Class::Binary,
        MYSQL_TYPE_TINY_BLOB
        | MYSQL_TYPE_MEDIUM_BLOB
        | MYSQL_TYPE_LONG_BLOB
        | MYSQL_TYPE_BLOB
        | MYSQL_TYPE_VAR_STRING
        | MYSQL_TYPE_STRING
        | MYSQL_TYPE_VARCHAR
            if charset == BINARY_CHARSET =>
        {
            Class::Binary
        }
        _ => Class::Text,
    }
}

/// Native type name shown in the grid header, e.g. `INT UNSIGNED`, `VARCHAR`, `BLOB`.
pub fn type_name(ty: ColumnType, flags: ColumnFlags, charset: u16) -> String {
    use ColumnType::*;
    let binary = charset == BINARY_CHARSET;
    let base = match ty {
        MYSQL_TYPE_TINY => "TINYINT",
        MYSQL_TYPE_SHORT => "SMALLINT",
        MYSQL_TYPE_INT24 => "MEDIUMINT",
        MYSQL_TYPE_LONG => "INT",
        MYSQL_TYPE_LONGLONG => "BIGINT",
        MYSQL_TYPE_YEAR => "YEAR",
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "DECIMAL",
        MYSQL_TYPE_FLOAT => "FLOAT",
        MYSQL_TYPE_DOUBLE => "DOUBLE",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "DATE",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "TIME",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "DATETIME",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "TIMESTAMP",
        MYSQL_TYPE_JSON => "JSON",
        MYSQL_TYPE_BIT => "BIT",
        MYSQL_TYPE_GEOMETRY => "GEOMETRY",
        MYSQL_TYPE_ENUM => "ENUM",
        MYSQL_TYPE_SET => "SET",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            if binary {
                "BLOB"
            } else {
                "TEXT"
            }
        }
        MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_VARCHAR => {
            if binary {
                "VARBINARY"
            } else {
                "VARCHAR"
            }
        }
        MYSQL_TYPE_STRING => {
            if flags.contains(ColumnFlags::ENUM_FLAG) {
                "ENUM"
            } else if flags.contains(ColumnFlags::SET_FLAG) {
                "SET"
            } else if binary {
                "BINARY"
            } else {
                "CHAR"
            }
        }
        MYSQL_TYPE_NULL => "NULL",
        _ => "UNKNOWN",
    };
    if flags.contains(ColumnFlags::UNSIGNED_FLAG)
        && matches!(
            classify(ty, charset),
            Class::Int | Class::Decimal | Class::Float
        )
    {
        format!("{base} UNSIGNED")
    } else {
        base.to_owned()
    }
}

/// Converts one value. Text-protocol values arrive as `Bytes`; binary-protocol values may be
/// typed, so both shapes are handled.
pub fn cell(class: Class, value: Value) -> CellValue {
    match value {
        Value::NULL => CellValue::Null,
        Value::Int(i) => CellValue::Int(i),
        Value::UInt(u) => {
            i64::try_from(u).map_or_else(|_| CellValue::Decimal(u.to_string()), CellValue::Int)
        }
        Value::Float(f) => CellValue::Float(f64::from(f)),
        Value::Double(d) => CellValue::Float(d),
        Value::Date(y, mo, d, h, mi, s, us) => {
            CellValue::Temporal(format_datetime(y, mo, d, h, mi, s, us))
        }
        Value::Time(neg, days, h, mi, s, us) => {
            CellValue::Temporal(format_time(neg, days, h, mi, s, us))
        }
        Value::Bytes(bytes) => from_bytes(class, bytes),
    }
}

fn from_bytes(class: Class, bytes: Vec<u8>) -> CellValue {
    if class == Class::Binary {
        return CellValue::Bytes(bytes);
    }
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => return CellValue::Bytes(e.into_bytes()),
    };
    match class {
        Class::Int => text
            .parse::<i64>()
            .map_or_else(|_| CellValue::Decimal(text), CellValue::Int),
        Class::Decimal => CellValue::Decimal(text),
        Class::Float => text
            .parse()
            .map_or_else(|_| CellValue::Text(text), CellValue::Float),
        Class::Temporal => CellValue::Temporal(text),
        Class::Json => CellValue::Json(text),
        Class::Binary | Class::Text => CellValue::Text(text),
    }
}

fn format_datetime(y: u16, mo: u8, d: u8, h: u8, mi: u8, s: u8, us: u32) -> String {
    let mut out = format!("{y:04}-{mo:02}-{d:02}");
    if (h, mi, s, us) != (0, 0, 0, 0) {
        out.push_str(&format!(" {h:02}:{mi:02}:{s:02}"));
        if us != 0 {
            out.push_str(&format!(".{us:06}"));
        }
    }
    out
}

fn format_time(neg: bool, days: u32, h: u8, mi: u8, s: u8, us: u32) -> String {
    let hours = days * 24 + u32::from(h);
    let sign = if neg { "-" } else { "" };
    let mut out = format!("{sign}{hours:02}:{mi:02}:{s:02}");
    if us != 0 {
        out.push_str(&format!(".{us:06}"));
    }
    out
}

/// Renders any value as plain text (used for catalog queries).
pub fn text(value: Value) -> Option<String> {
    match cell(Class::Text, value) {
        CellValue::Null => None,
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_protocol_values_are_classified() {
        assert_eq!(
            cell(Class::Int, Value::Bytes(b"42".to_vec())),
            CellValue::Int(42)
        );
        assert_eq!(
            cell(Class::Int, Value::Bytes(b"18446744073709551615".to_vec())),
            CellValue::Decimal("18446744073709551615".into())
        );
        assert_eq!(
            cell(Class::Decimal, Value::Bytes(b"10.50".to_vec())),
            CellValue::Decimal("10.50".into())
        );
        assert_eq!(
            cell(Class::Binary, Value::Bytes(vec![0, 1])),
            CellValue::Bytes(vec![0, 1])
        );
        assert_eq!(cell(Class::Text, Value::NULL), CellValue::Null);
        assert_eq!(
            cell(Class::Text, Value::Bytes(vec![0xff])),
            CellValue::Bytes(vec![0xff])
        );
    }

    #[test]
    fn binary_protocol_values_are_formatted() {
        assert_eq!(
            cell(Class::Temporal, Value::Date(2026, 10, 16, 0, 0, 0, 0)),
            CellValue::Temporal("2026-10-16".into())
        );
        assert_eq!(
            cell(Class::Temporal, Value::Date(2026, 1, 2, 3, 4, 5, 6)),
            CellValue::Temporal("2026-01-02 03:04:05.000006".into())
        );
        assert_eq!(
            cell(Class::Temporal, Value::Time(true, 1, 2, 3, 4, 0)),
            CellValue::Temporal("-26:03:04".into())
        );
    }

    #[test]
    fn type_names_reflect_charset_and_flags() {
        assert_eq!(
            type_name(ColumnType::MYSQL_TYPE_LONG, ColumnFlags::UNSIGNED_FLAG, 33),
            "INT UNSIGNED"
        );
        assert_eq!(
            type_name(
                ColumnType::MYSQL_TYPE_BLOB,
                ColumnFlags::empty(),
                BINARY_CHARSET
            ),
            "BLOB"
        );
        assert_eq!(
            type_name(ColumnType::MYSQL_TYPE_VAR_STRING, ColumnFlags::empty(), 255),
            "VARCHAR"
        );
        assert_eq!(
            classify(ColumnType::MYSQL_TYPE_VAR_STRING, BINARY_CHARSET),
            Class::Binary
        );
    }
}
