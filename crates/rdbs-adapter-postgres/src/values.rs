//! Conversion of PostgreSQL text-format values into [`CellValue`].
//!
//! Queries run over the simple-query protocol, so every value arrives already formatted by the
//! server exactly as `psql` would show it. The column type (from `prepare`) only decides how a
//! value is classified for display: numbers right-aligned, NULL styling, binary as bytes, etc.

use rdbs_core::CellValue;
use tokio_postgres::types::Type;

/// Converts one text value of a column of type `ty` (unknown types are treated as text).
pub fn cell(ty: Option<&Type>, text: Option<String>) -> CellValue {
    let Some(text) = text else {
        return CellValue::Null;
    };
    let Some(ty) = ty else {
        return CellValue::Text(text);
    };
    match *ty {
        Type::BOOL => match text.as_str() {
            "t" => CellValue::Bool(true),
            "f" => CellValue::Bool(false),
            _ => CellValue::Text(text),
        },
        Type::INT2 | Type::INT4 | Type::INT8 | Type::OID => text
            .parse()
            .map_or_else(|_| CellValue::Text(text), CellValue::Int),
        Type::FLOAT4 | Type::FLOAT8 => text
            .parse()
            .map_or_else(|_| CellValue::Text(text), CellValue::Float),
        Type::NUMERIC | Type::MONEY => CellValue::Decimal(text),
        Type::BYTEA => decode_bytea_hex(&text).map_or(CellValue::Text(text), CellValue::Bytes),
        Type::JSON | Type::JSONB => CellValue::Json(text),
        Type::DATE
        | Type::TIME
        | Type::TIMETZ
        | Type::TIMESTAMP
        | Type::TIMESTAMPTZ
        | Type::INTERVAL => CellValue::Temporal(text),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::CHAR | Type::UUID => {
            CellValue::Text(text)
        }
        _ => CellValue::Other {
            type_name: ty.name().to_owned(),
            display: text,
        },
    }
}

/// Decodes PostgreSQL's `bytea_output = hex` format (`\x0a1b…`).
fn decode_bytea_hex(text: &str) -> Option<Vec<u8>> {
    let hex = text.strip_prefix("\\x")?;
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_type() {
        assert_eq!(
            cell(Some(&Type::INT4), Some("42".into())),
            CellValue::Int(42)
        );
        assert_eq!(
            cell(Some(&Type::BOOL), Some("t".into())),
            CellValue::Bool(true)
        );
        assert_eq!(
            cell(Some(&Type::FLOAT8), Some("1.5".into())),
            CellValue::Float(1.5)
        );
        assert_eq!(
            cell(Some(&Type::NUMERIC), Some("12345678901234567890.01".into())),
            CellValue::Decimal("12345678901234567890.01".into())
        );
        assert_eq!(
            cell(Some(&Type::BYTEA), Some("\\xdead".into())),
            CellValue::Bytes(vec![0xde, 0xad])
        );
        assert_eq!(cell(Some(&Type::TEXT), None), CellValue::Null);
        assert_eq!(cell(None, Some("x".into())), CellValue::Text("x".into()));
        assert_eq!(
            cell(Some(&Type::INT4_ARRAY), Some("{1,2}".into())),
            CellValue::Other {
                type_name: "_int4".into(),
                display: "{1,2}".into()
            }
        );
    }

    #[test]
    fn keeps_unparseable_values_as_text() {
        assert_eq!(
            cell(Some(&Type::INT8), Some("NaN?".into())),
            CellValue::Text("NaN?".into())
        );
        assert_eq!(
            cell(Some(&Type::BYTEA), Some("\\xabc".into())),
            CellValue::Text("\\xabc".into())
        );
    }
}
