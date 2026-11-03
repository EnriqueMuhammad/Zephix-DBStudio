//! Writing a result set as an Excel workbook (`.xlsx`).
//!
//! Numbers, booleans and dates become native Excel cells so they sort and sum; anything that
//! would lose precision or does not fit Excel's limits is written as text.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use rdbs_core::{CellValue, ResultSet};
use rust_xlsxwriter::{ExcelDateTime, Format, Workbook, Worksheet, XlsxError};

/// Excel keeps 15 significant digits; integers above 2^53 lose precision as `f64`.
const MAX_EXACT_INT: i64 = 1 << 53;
/// Characters Excel allows in one cell.
const MAX_CELL_CHARS: usize = 32_767;
/// Rows a worksheet holds, header included.
const MAX_ROWS: usize = 1_048_576;
/// Rows sampled to size the columns.
const WIDTH_SAMPLE_ROWS: usize = 500;

/// Serialises `rs` as an `.xlsx` workbook with one sheet named `sheet` (cleaned to Excel's
/// rules): a bold, frozen header row with filters, then one row per result row.
pub fn result_to_xlsx(rs: &ResultSet, sheet: &str) -> Result<Vec<u8>, String> {
    if rs.rows.len() >= MAX_ROWS {
        return Err(format!(
            "Excel sheets hold at most {} rows; this result has {}.",
            MAX_ROWS - 1,
            rs.rows.len()
        ));
    }
    build(rs, sheet).map_err(|e| format!("Could not build the Excel file: {e}"))
}

fn build(rs: &ResultSet, sheet: &str) -> Result<Vec<u8>, XlsxError> {
    let mut book = Workbook::new();
    let ws = book.add_worksheet();
    ws.set_name(sheet_name(sheet))?;
    let header = Format::new().set_bold();
    let formats = Formats {
        date: Format::new().set_num_format("yyyy-mm-dd"),
        datetime: Format::new().set_num_format("yyyy-mm-dd hh:mm:ss"),
        datetime_ms: Format::new().set_num_format("yyyy-mm-dd hh:mm:ss.000"),
        time: Format::new().set_num_format("hh:mm:ss"),
    };
    for (c, column) in rs.columns.iter().enumerate() {
        ws.write_string_with_format(0, col(c), clip(&column.name), &header)?;
    }
    for (r, row) in rs.rows.iter().enumerate() {
        let r = u32::try_from(r + 1).map_err(|_| XlsxError::RowColumnLimitError)?;
        for (c, value) in row.iter().enumerate() {
            write_cell(ws, r, col(c), value, &formats)?;
        }
    }
    if !rs.columns.is_empty() {
        let last_col = col(rs.columns.len() - 1);
        let last_row = u32::try_from(rs.rows.len()).map_err(|_| XlsxError::RowColumnLimitError)?;
        ws.set_freeze_panes(1, 0)?;
        ws.autofilter(0, 0, last_row, last_col)?;
        for (c, width) in column_widths(rs).into_iter().enumerate() {
            ws.set_column_width(col(c), width)?;
        }
    }
    book.save_to_buffer()
}

struct Formats {
    date: Format,
    datetime: Format,
    datetime_ms: Format,
    time: Format,
}

fn col(c: usize) -> u16 {
    u16::try_from(c).unwrap_or(u16::MAX)
}

fn write_cell(
    ws: &mut Worksheet,
    row: u32,
    col: u16,
    value: &CellValue,
    formats: &Formats,
) -> Result<(), XlsxError> {
    match value {
        CellValue::Null => {}
        CellValue::Bool(b) => {
            ws.write_boolean(row, col, *b)?;
        }
        CellValue::Int(i) if (-MAX_EXACT_INT..=MAX_EXACT_INT).contains(i) => {
            ws.write_number(row, col, *i as f64)?;
        }
        CellValue::Float(f) if f.is_finite() => {
            ws.write_number(row, col, *f)?;
        }
        CellValue::Decimal(s) => match exact_number(s) {
            Some(n) => {
                ws.write_number(row, col, n)?;
            }
            None => {
                ws.write_string(row, col, clip(s))?;
            }
        },
        CellValue::Temporal(s) => match temporal(s) {
            Some((dt, Kind::Date)) => {
                ws.write_datetime_with_format(row, col, &dt, &formats.date)?;
            }
            Some((dt, Kind::DateTime { millis })) => {
                let format = if millis {
                    &formats.datetime_ms
                } else {
                    &formats.datetime
                };
                ws.write_datetime_with_format(row, col, &dt, format)?;
            }
            Some((dt, Kind::Time)) => {
                ws.write_datetime_with_format(row, col, &dt, &formats.time)?;
            }
            None => {
                ws.write_string(row, col, clip(s))?;
            }
        },
        other => {
            ws.write_string(row, col, clip(&rdbs_core::export::plain_text(other)))?;
        }
    }
    Ok(())
}

/// A decimal that `f64` (and Excel) represent without losing digits.
fn exact_number(s: &str) -> Option<f64> {
    let t = s.trim();
    let digits = t.chars().filter(char::is_ascii_digit).count();
    let plain = !t.is_empty()
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.'));
    if !plain || digits > 15 {
        return None;
    }
    t.parse::<f64>().ok().filter(|n| n.is_finite())
}

enum Kind {
    Date,
    DateTime { millis: bool },
    Time,
}

/// Parses the ISO-8601 renderings adapters produce. Values with a time zone, or outside
/// Excel's 1900–9999 range, stay text.
fn temporal(s: &str) -> Option<(ExcelDateTime, Kind)> {
    let t = s.trim();
    let millis = |time: &NaiveTime| (time.nanosecond() / 1_000_000).min(999) as u16;
    let date_of = |d: NaiveDate| {
        use chrono::Datelike;
        ExcelDateTime::from_ymd(
            u16::try_from(d.year()).ok()?,
            d.month() as u8,
            d.day() as u8,
        )
        .ok()
    };
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(t, fmt) {
            let time = dt.time();
            let excel = date_of(dt.date())?
                .and_hms_milli(
                    time.hour() as u16,
                    time.minute() as u8,
                    time.second().min(59) as u8,
                    millis(&time),
                )
                .ok()?;
            return Some((
                excel,
                Kind::DateTime {
                    millis: time.nanosecond() >= 1_000_000,
                },
            ));
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        return Some((date_of(d)?, Kind::Date));
    }
    if let Ok(time) = NaiveTime::parse_from_str(t, "%H:%M:%S%.f") {
        let excel = ExcelDateTime::from_hms_milli(
            time.hour() as u16,
            time.minute() as u8,
            time.second().min(59) as u8,
            millis(&time),
        )
        .ok()?;
        return Some((excel, Kind::Time));
    }
    None
}

/// Text cut to what one Excel cell holds.
fn clip(s: &str) -> &str {
    match s.char_indices().nth(MAX_CELL_CHARS) {
        Some((at, _)) => &s[..at],
        None => s,
    }
}

/// A valid sheet name: at most 31 characters, none of `[]:*?/\`, not empty.
fn sheet_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\'))
        .take(31)
        .collect();
    let cleaned = cleaned.trim_matches('\'').trim().to_owned();
    if cleaned.is_empty() {
        "Result".to_owned()
    } else {
        cleaned
    }
}

/// Column widths in characters from the header and the first rows, between 8 and 60.
fn column_widths(rs: &ResultSet) -> Vec<f64> {
    rs.columns
        .iter()
        .enumerate()
        .map(|(c, column)| {
            let longest = rs
                .rows
                .iter()
                .take(WIDTH_SAMPLE_ROWS)
                .filter_map(|row| row.get(c))
                .map(|v| rdbs_core::export::plain_text(v).chars().count())
                .chain(std::iter::once(column.name.chars().count() + 2))
                .max()
                .unwrap_or(8);
            (longest as f64 + 1.0).clamp(8.0, 60.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdbs_core::ColumnMeta;

    fn rs() -> ResultSet {
        ResultSet {
            columns: vec![
                ColumnMeta::new("id", "int"),
                ColumnMeta::new("name", "text"),
                ColumnMeta::new("price", "numeric"),
                ColumnMeta::new("at", "timestamp"),
            ],
            rows: vec![
                vec![
                    CellValue::Int(1),
                    CellValue::Text("Ann".into()),
                    CellValue::Decimal("12.50".into()),
                    CellValue::Temporal("2026-11-02 23:00:48".into()),
                ],
                vec![
                    CellValue::Int(i64::MAX),
                    CellValue::Null,
                    CellValue::Decimal("12345678901234567890.1".into()),
                    CellValue::Temporal("2026-11-02 23:00:48+07:00".into()),
                ],
            ],
        }
    }

    #[test]
    fn writes_a_zip_workbook() {
        let bytes = result_to_xlsx(&rs(), "orders").unwrap();
        assert!(bytes.starts_with(b"PK"), "xlsx is a zip file");
        assert!(bytes.len() > 1000);
        // Empty results still produce a valid file.
        assert!(result_to_xlsx(&ResultSet::default(), "").is_ok());
    }

    #[test]
    fn reads_back_with_native_cells() {
        use calamine::{Data, Reader, Xlsx};
        let bytes = result_to_xlsx(&rs(), "orders").unwrap();
        let mut book: Xlsx<_> =
            calamine::open_workbook_from_rs(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(book.sheet_names(), vec!["orders".to_owned()]);
        let range = book.worksheet_range("orders").unwrap();
        assert_eq!(range.get((0, 0)), Some(&Data::String("id".into())));
        assert_eq!(range.get((1, 0)), Some(&Data::Float(1.0)));
        assert_eq!(range.get((1, 1)), Some(&Data::String("Ann".into())));
        assert_eq!(range.get((1, 2)), Some(&Data::Float(12.5)));
        assert!(
            matches!(range.get((1, 3)), Some(Data::DateTime(_))),
            "{:?}",
            range.get((1, 3))
        );
        // Too large for f64, kept exact as text; NULL is an empty cell.
        assert_eq!(range.get((2, 0)), Some(&Data::String(i64::MAX.to_string())));
        assert!(matches!(range.get((2, 1)), None | Some(Data::Empty)));
        assert_eq!(
            range.get((2, 2)),
            Some(&Data::String("12345678901234567890.1".into()))
        );
        assert_eq!(
            range.get((2, 3)),
            Some(&Data::String("2026-11-02 23:00:48+07:00".into()))
        );
    }

    #[test]
    fn keeps_precision_or_falls_back_to_text() {
        assert_eq!(exact_number("12.50"), Some(12.5));
        assert_eq!(exact_number("-3"), Some(-3.0));
        assert_eq!(exact_number("12345678901234567890.1"), None);
        assert_eq!(exact_number("1e5"), None);
        assert_eq!(exact_number(""), None);
    }

    #[test]
    fn recognises_iso_dates_and_times() {
        assert!(matches!(temporal("2026-11-02"), Some((_, Kind::Date))));
        assert!(matches!(
            temporal("2026-11-02 23:00:48"),
            Some((_, Kind::DateTime { millis: false }))
        ));
        assert!(matches!(
            temporal("2026-11-02T23:00:48.250"),
            Some((_, Kind::DateTime { millis: true }))
        ));
        assert!(matches!(temporal("08:30:00"), Some((_, Kind::Time))));
        // Time zones and dates Excel cannot show stay text.
        assert!(temporal("2026-11-02 23:00:48+07:00").is_none());
        assert!(temporal("1850-01-01").is_none());
        assert!(temporal("yesterday").is_none());
    }

    #[test]
    fn cleans_sheet_names_and_long_text() {
        assert_eq!(sheet_name("dbo.[orders]"), "dbo.orders");
        assert_eq!(sheet_name("a/b:c*d?e"), "abcde");
        assert_eq!(sheet_name(""), "Result");
        assert_eq!(sheet_name(&"x".repeat(40)).len(), 31);
        let long = "é".repeat(MAX_CELL_CHARS + 10);
        assert_eq!(clip(&long).chars().count(), MAX_CELL_CHARS);
    }
}
