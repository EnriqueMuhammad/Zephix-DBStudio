//! SQLite values → [`CellValue`].
//!
//! SQLite stores values by storage class (NULL, INTEGER, REAL, TEXT, BLOB) regardless of the
//! declared column type, so the declared type only refines the mapping (booleans, dates, JSON).

use rdbs_core::CellValue;
use rusqlite::types::ValueRef;

/// Converts one value; `decl` is the declared column type (may be empty).
pub fn cell(value: ValueRef<'_>, decl: &str) -> CellValue {
    let decl = decl.to_ascii_lowercase();
    match value {
        ValueRef::Null => CellValue::Null,
        ValueRef::Integer(i) if decl.contains("bool") => CellValue::Bool(i != 0),
        ValueRef::Integer(i) => CellValue::Int(i),
        ValueRef::Real(f) => CellValue::Float(f),
        ValueRef::Text(bytes) => {
            let text = String::from_utf8_lossy(bytes).into_owned();
            if decl.contains("json") {
                CellValue::Json(text)
            } else if decl.contains("date") || decl.contains("time") {
                CellValue::Temporal(text)
            } else {
                CellValue::Text(text)
            }
        }
        ValueRef::Blob(bytes) => CellValue::Bytes(bytes.to_vec()),
    }
}

/// Storage class name of a converted value (`None` for NULL).
pub fn storage_class(value: &CellValue) -> Option<&'static str> {
    Some(match value {
        CellValue::Null => return None,
        CellValue::Bool(_) | CellValue::Int(_) => "INTEGER",
        CellValue::Float(_) => "REAL",
        CellValue::Bytes(_) => "BLOB",
        _ => "TEXT",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_type_refines_storage_class() {
        assert_eq!(cell(ValueRef::Integer(1), "BOOLEAN"), CellValue::Bool(true));
        assert_eq!(cell(ValueRef::Integer(7), "INTEGER"), CellValue::Int(7));
        assert_eq!(
            cell(ValueRef::Text(b"2026-01-01"), "DATE"),
            CellValue::Temporal("2026-01-01".into())
        );
        assert_eq!(
            cell(ValueRef::Text(b"{}"), "JSON"),
            CellValue::Json("{}".into())
        );
        assert_eq!(cell(ValueRef::Text(b"x"), ""), CellValue::Text("x".into()));
        assert_eq!(
            cell(ValueRef::Blob(&[1, 2]), "BLOB"),
            CellValue::Bytes(vec![1, 2])
        );
        assert_eq!(storage_class(&CellValue::Float(1.0)), Some("REAL"));
        assert_eq!(storage_class(&CellValue::Null), None);
    }
}
