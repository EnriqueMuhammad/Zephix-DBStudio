//! Result values. Adapters convert driver values into [`CellValue`] while keeping the
//! native column type name in [`ColumnMeta`], so no type information is lost.

use std::fmt;

/// Maximum number of rows an adapter keeps in memory for one result set. Further rows are
/// drained from the server and dropped, and [`crate::QueryOutcome::truncated`] is set.
pub const MAX_RESULT_ROWS: usize = 200_000;

/// Metadata of a result column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMeta {
    /// Column name or alias.
    pub name: String,
    /// Native type name as reported by the server, e.g. `int4`, `nvarchar(50)`, `DATETIME`.
    pub type_name: String,
    /// Whether the column may contain NULL (`None` when the driver does not say).
    pub nullable: Option<bool>,
}

impl ColumnMeta {
    /// Convenience constructor.
    pub fn new(name: impl Into<String>, type_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
            nullable: None,
        }
    }
}

/// A single cell value.
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    /// SQL NULL.
    Null,
    /// Boolean / bit.
    Bool(bool),
    /// Any integer type that fits into `i64`.
    Int(i64),
    /// Floating point.
    Float(f64),
    /// Exact numeric kept as text to avoid precision loss (DECIMAL, NUMERIC, MONEY).
    Decimal(String),
    /// Character data.
    Text(String),
    /// Binary data.
    Bytes(Vec<u8>),
    /// Date/time values rendered by the adapter in ISO-8601.
    Temporal(String),
    /// JSON document text.
    Json(String),
    /// Anything else, with the adapter's textual rendering.
    Other {
        /// Native type name.
        type_name: String,
        /// Display text.
        display: String,
    },
}

impl CellValue {
    /// Whether this is SQL NULL.
    pub fn is_null(&self) -> bool {
        matches!(self, CellValue::Null)
    }

    /// Whether the value is numeric (right-aligned in the grid).
    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            CellValue::Int(_) | CellValue::Float(_) | CellValue::Decimal(_)
        )
    }
}

impl fmt::Display for CellValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CellValue::Null => f.write_str("NULL"),
            CellValue::Bool(b) => write!(f, "{b}"),
            CellValue::Int(i) => write!(f, "{i}"),
            CellValue::Float(x) => write!(f, "{x}"),
            CellValue::Decimal(s)
            | CellValue::Text(s)
            | CellValue::Temporal(s)
            | CellValue::Json(s) => f.write_str(s),
            CellValue::Bytes(bytes) => {
                const MAX: usize = 32;
                f.write_str("0x")?;
                for b in bytes.iter().take(MAX) {
                    write!(f, "{b:02X}")?;
                }
                if bytes.len() > MAX {
                    write!(f, "… ({} bytes)", bytes.len())?;
                }
                Ok(())
            }
            CellValue::Other { display, .. } => f.write_str(display),
        }
    }
}

/// A fully materialised tabular result.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResultSet {
    /// Column metadata.
    pub columns: Vec<ColumnMeta>,
    /// Row-major cell values; every row has `columns.len()` cells.
    pub rows: Vec<Vec<CellValue>>,
}

impl ResultSet {
    /// Returns the cell at (`row`, `col`) if it exists.
    pub fn cell(&self, row: usize, col: usize) -> Option<&CellValue> {
        self.rows.get(row).and_then(|r| r.get(col))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_formats() {
        assert_eq!(CellValue::Null.to_string(), "NULL");
        assert_eq!(CellValue::Int(-5).to_string(), "-5");
        assert_eq!(CellValue::Bytes(vec![0xde, 0xad]).to_string(), "0xDEAD");
        let long = CellValue::Bytes(vec![0; 40]).to_string();
        assert!(long.ends_with("(40 bytes)"), "{long}");
    }

    #[test]
    fn cell_lookup_is_bounds_checked() {
        let rs = ResultSet {
            columns: vec![ColumnMeta::new("id", "int4")],
            rows: vec![vec![CellValue::Int(1)]],
        };
        assert_eq!(rs.cell(0, 0), Some(&CellValue::Int(1)));
        assert_eq!(rs.cell(1, 0), None);
        assert_eq!(rs.cell(0, 1), None);
    }
}
