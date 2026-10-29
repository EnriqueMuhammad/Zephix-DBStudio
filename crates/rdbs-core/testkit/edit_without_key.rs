//! Shared live-test scenario for editing rows of a table without a primary key, included by
//! adapter integration tests with `#[path = "../../rdbs-core/testkit/edit_without_key.rs"]
//! mod ...;`.
//!
//! The caller creates the table with columns `code`, `starts` (a date/time type of the engine)
//! and `note`, holding two identical rows `('a', <time>, NULL)` and one `('b', <time>, 'x')`.
//! The scenario edits them through `build_changes` with a target that has no primary key, so
//! rows are matched by all their column values, and checks what the server ends up with.

use rdbs_core::edit::{EditValue, PendingEdits, RowMatch, TableTarget, build_changes, row_match};
use rdbs_core::{CancelHandle, CellValue, DatabaseAdapter, DatabaseKind, ResultSet};

async fn select_all(s: &mut dyn DatabaseAdapter, sql: &str) -> ResultSet {
    let out = s
        .execute(sql, CancelHandle::new())
        .await
        .unwrap_or_else(|e| panic!("{sql}\n→ {e}"));
    out.results.into_iter().last().expect("result set")
}

pub async fn edit_without_key(
    s: &mut dyn DatabaseAdapter,
    kind: DatabaseKind,
    schema: Option<&str>,
    table: &str,
) {
    let target = TableTarget {
        kind,
        database: None,
        schema: schema.map(Into::into),
        table: table.into(),
        primary_key: Vec::new(),
        columns: vec!["code".into(), "starts".into(), "note".into()],
    };
    let select = format!("SELECT * FROM {} ORDER BY code", target.qualified());
    let before = select_all(s, &select).await;
    assert_eq!(before.rows.len(), 3, "{before:#?}");
    assert_eq!(row_match(&target, &before), Ok(RowMatch::AllColumns));

    // Rows 0 and 1 are the identical 'a' rows: delete one, fill in the note of the other.
    let mut edits = PendingEdits::default();
    edits.toggle_delete(0);
    edits.set_cell(1, 2, EditValue::Text("z".into()), &before.rows[1][2]);
    edits.set_cell(2, 2, EditValue::Text("y".into()), &before.rows[2][2]);
    let changes = build_changes(&target, &before, &edits).expect("build changes");
    s.apply_changes(&changes)
        .await
        .unwrap_or_else(|e| panic!("{changes:#?}\n→ {e}"));

    let after = select_all(s, &select).await;
    let notes: Vec<(&CellValue, &CellValue)> = after.rows.iter().map(|r| (&r[0], &r[2])).collect();
    let text = |t: &str| CellValue::Text(t.into());
    assert_eq!(
        notes,
        [(&text("a"), &text("z")), (&text("b"), &text("y"))],
        "{after:#?}"
    );
    // The date/time values are untouched.
    assert_eq!(after.rows[0][1], before.rows[0][1]);
    assert_eq!(after.rows[1][1], before.rows[2][1]);

    // Edits against the stale rows match nothing now and are rolled back as a whole.
    let mut stale = PendingEdits::default();
    stale.set_cell(2, 2, EditValue::Text("w".into()), &before.rows[2][2]);
    let changes = build_changes(&target, &before, &stale).expect("build changes");
    let err = s
        .apply_changes(&changes)
        .await
        .expect_err("stale row must not match");
    assert!(err.message.contains("affected 0 rows"), "{err}");
}
