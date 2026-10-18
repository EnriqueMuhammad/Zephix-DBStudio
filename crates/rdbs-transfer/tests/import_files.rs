//! Imports from real files into a real (SQLite) database.

#![allow(clippy::expect_used)]

use std::path::PathBuf;

use rdbs_adapter_sqlite::SqliteAdapter;
use rdbs_core::{CancelHandle, CellValue, DatabaseAdapter, DatabaseKind};
use rdbs_transfer::import::{
    ImportOptions, ReadOptions, auto_map, build_inserts, read_file, sheet_names, table_for_sheet,
};

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rdbs-import-{}-{name}", std::process::id()))
}

#[test]
fn reads_xlsx_with_types_and_dates() {
    let path = temp("book.xlsx");
    let mut book = rust_xlsxwriter::Workbook::new();
    let sheet = book.add_worksheet().set_name("People").expect("name");
    sheet.write_string(0, 0, "id").expect("cell");
    sheet.write_string(0, 1, "name").expect("cell");
    sheet.write_string(0, 2, "joined").expect("cell");
    sheet.write_number(1, 0, 1.0).expect("cell");
    sheet.write_string(1, 1, "Ayu").expect("cell");
    let date = rust_xlsxwriter::ExcelDateTime::from_ymd(2026, 3, 4).expect("date");
    let format = rust_xlsxwriter::Format::new().set_num_format("yyyy-mm-dd");
    sheet
        .write_datetime_with_format(1, 2, &date, &format)
        .expect("cell");
    sheet.write_number(2, 0, 2.5).expect("cell");
    sheet.write_boolean(2, 1, true).expect("cell");
    book.add_worksheet().set_name("Other").expect("name");
    book.save(&path).expect("save");

    assert_eq!(sheet_names(&path).expect("names"), ["People", "Other"]);
    let data = read_file(&path, &ReadOptions::default()).expect("read");
    assert_eq!(data.headers, ["id", "name", "joined"]);
    assert_eq!(data.rows[0], ["1", "Ayu", "2026-03-04"]);
    assert_eq!(data.rows[1], ["2.5", "true", ""]);
}

#[tokio::test]
async fn csv_into_a_new_sqlite_table() {
    let csv = temp("people.csv");
    std::fs::write(
        &csv,
        "id;name;score\n1;Ayu;9.5\n2;Budi;\n3;\"Citra; Dewi\";7\n",
    )
    .expect("csv");
    let db = temp("import.sqlite");
    std::fs::write(&db, b"").expect("db");
    let mut s = SqliteAdapter::open(&db.display().to_string())
        .await
        .expect("open");

    let sheet = read_file(&csv, &ReadOptions::default()).expect("read");
    let table = table_for_sheet(DatabaseKind::Sqlite, None, "people", &sheet);
    for statement in rdbs_core::ddlgen::create_table(DatabaseKind::Sqlite, &table) {
        s.execute(&statement, CancelHandle::new())
            .await
            .expect("create");
    }
    let mapping = auto_map(&table, &sheet);
    let changes = build_inserts(
        DatabaseKind::Sqlite,
        &table,
        &sheet,
        &mapping,
        &ImportOptions::default(),
    )
    .expect("inserts");
    assert_eq!(s.apply_changes(&changes).await.expect("apply"), 3);

    let out = s
        .execute(
            "SELECT id, name, score FROM people ORDER BY id",
            CancelHandle::new(),
        )
        .await
        .expect("select");
    let rows = &out.results[0].rows;
    assert_eq!(rows[2][1], CellValue::Text("Citra; Dewi".into()));
    assert_eq!(rows[1][2], CellValue::Null, "empty cell became NULL");
    assert_eq!(rows[0][2], CellValue::Float(9.5));
}
