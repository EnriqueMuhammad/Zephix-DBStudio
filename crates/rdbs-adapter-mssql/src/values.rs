//! Conversion of TDS values into [`CellValue`] and SQL Server type names.

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use rdbs_core::CellValue;
use tiberius::{ColumnData, ColumnType, FromSql};

/// SQL Server type name for a result column. Nullable variants (`Intn`, `Floatn`, …) do not
/// carry their exact width in the metadata, so the common name is used.
pub fn type_name(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Null => "null",
        ColumnType::Bit | ColumnType::Bitn => "bit",
        ColumnType::Int1 => "tinyint",
        ColumnType::Int2 => "smallint",
        ColumnType::Int4 | ColumnType::Intn => "int",
        ColumnType::Int8 => "bigint",
        ColumnType::Float4 => "real",
        ColumnType::Float8 | ColumnType::Floatn => "float",
        ColumnType::Money => "money",
        ColumnType::Money4 => "smallmoney",
        ColumnType::Decimaln => "decimal",
        ColumnType::Numericn => "numeric",
        ColumnType::Guid => "uniqueidentifier",
        ColumnType::Datetime4 => "smalldatetime",
        ColumnType::Datetime | ColumnType::Datetimen => "datetime",
        ColumnType::Daten => "date",
        ColumnType::Timen => "time",
        ColumnType::Datetime2 => "datetime2",
        ColumnType::DatetimeOffsetn => "datetimeoffset",
        ColumnType::BigVarBin => "varbinary",
        ColumnType::BigBinary => "binary",
        ColumnType::BigVarChar => "varchar",
        ColumnType::BigChar => "char",
        ColumnType::NVarchar => "nvarchar",
        ColumnType::NChar => "nchar",
        ColumnType::Xml => "xml",
        ColumnType::Udt => "udt",
        ColumnType::Text => "text",
        ColumnType::NText => "ntext",
        ColumnType::Image => "image",
        ColumnType::SSVariant => "sql_variant",
    }
}

fn temporal<'a, T: FromSql<'a>>(
    data: &'a ColumnData<'static>,
    fmt: impl FnOnce(T) -> String,
) -> CellValue {
    match T::from_sql(data) {
        Ok(Some(v)) => CellValue::Temporal(fmt(v)),
        Ok(None) => CellValue::Null,
        Err(e) => CellValue::Other {
            type_name: "temporal".to_owned(),
            display: format!("<{e}>"),
        },
    }
}

/// Converts one cell.
pub fn cell(data: &ColumnData<'static>) -> CellValue {
    match data {
        ColumnData::U8(v) => v.map_or(CellValue::Null, |v| CellValue::Int(i64::from(v))),
        ColumnData::I16(v) => v.map_or(CellValue::Null, |v| CellValue::Int(i64::from(v))),
        ColumnData::I32(v) => v.map_or(CellValue::Null, |v| CellValue::Int(i64::from(v))),
        ColumnData::I64(v) => v.map_or(CellValue::Null, CellValue::Int),
        ColumnData::F32(v) => v.map_or(CellValue::Null, |v| CellValue::Float(f64::from(v))),
        ColumnData::F64(v) => v.map_or(CellValue::Null, CellValue::Float),
        ColumnData::Bit(v) => v.map_or(CellValue::Null, CellValue::Bool),
        ColumnData::String(v) => v
            .as_ref()
            .map_or(CellValue::Null, |s| CellValue::Text(s.to_string())),
        ColumnData::Guid(v) => v.map_or(CellValue::Null, |g| CellValue::Text(g.to_string())),
        ColumnData::Binary(v) => v
            .as_ref()
            .map_or(CellValue::Null, |b| CellValue::Bytes(b.to_vec())),
        ColumnData::Numeric(v) => v.map_or(CellValue::Null, |n| CellValue::Decimal(n.to_string())),
        ColumnData::Xml(v) => v
            .as_ref()
            .map_or(CellValue::Null, |x| CellValue::Text(x.to_string())),
        ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => {
            temporal(data, |v: NaiveDateTime| {
                v.format("%Y-%m-%d %H:%M:%S%.f").to_string()
            })
        }
        ColumnData::Date(_) => temporal(data, |v: NaiveDate| v.format("%Y-%m-%d").to_string()),
        ColumnData::Time(_) => temporal(data, |v: NaiveTime| v.format("%H:%M:%S%.f").to_string()),
        ColumnData::DateTimeOffset(_) => temporal(data, |v: DateTime<FixedOffset>| {
            v.format("%Y-%m-%d %H:%M:%S%.f %:z").to_string()
        }),
    }
}

/// Renders a cell as plain text (used for catalog queries).
pub fn text(data: &ColumnData<'static>) -> Option<String> {
    match cell(data) {
        CellValue::Null => None,
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    #[test]
    fn converts_scalars() {
        assert_eq!(cell(&ColumnData::I32(Some(7))), CellValue::Int(7));
        assert_eq!(cell(&ColumnData::I32(None)), CellValue::Null);
        assert_eq!(cell(&ColumnData::Bit(Some(true))), CellValue::Bool(true));
        assert_eq!(
            cell(&ColumnData::String(Some(Cow::Borrowed("héllo")))),
            CellValue::Text("héllo".into())
        );
        assert_eq!(
            cell(&ColumnData::Binary(Some(Cow::Owned(vec![1, 2])))),
            CellValue::Bytes(vec![1, 2])
        );
        assert_eq!(text(&ColumnData::U8(Some(3))).as_deref(), Some("3"));
    }

    #[test]
    fn names_common_types() {
        assert_eq!(type_name(ColumnType::NVarchar), "nvarchar");
        assert_eq!(type_name(ColumnType::Intn), "int");
        assert_eq!(type_name(ColumnType::DatetimeOffsetn), "datetimeoffset");
    }
}
