//! Shared live-test scenario for `describe_schema` + `ddlgen`, included by every adapter's
//! integration tests with `#[path = "../../rdbs-core/testkit/schema_roundtrip.rs"] mod ...;`.
//!
//! 1. create two related tables from a [`TableSchema`] with [`ddlgen::create_table`];
//! 2. describe them and check keys, indexes, foreign keys, defaults and identity;
//! 3. alter one (rename, retype, drop default, add column and index) and check the result;
//! 4. recreate both from the described definitions in a second schema/database and require the
//!    copies to describe identically — the round trip data transfer relies on.

use rdbs_core::ddlgen::{alter_table, create_table};
use rdbs_core::{
    CancelHandle, ColumnDef, DatabaseAdapter, DatabaseKind, ForeignKeyDef, IndexDef, TableSchema,
};

/// Engine-specific type names used by the scenario.
pub struct Types {
    pub int: &'static str,
    pub bigint: &'static str,
    pub varchar: &'static str,
    pub text: &'static str,
}

pub async fn run_all(s: &mut dyn DatabaseAdapter, sql: &[String]) {
    for statement in sql {
        s.execute(statement, CancelHandle::new())
            .await
            .unwrap_or_else(|e| panic!("{statement}\n→ {e}"));
    }
}

async fn describe(s: &mut dyn DatabaseAdapter, schema: Option<&str>) -> Vec<TableSchema> {
    s.describe_schema(schema).await.expect("describe_schema")
}

fn find<'a>(tables: &'a [TableSchema], name: &str) -> &'a TableSchema {
    tables
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("table {name} not described: {tables:#?}"))
}

fn names(t: &TableSchema) -> Vec<&str> {
    t.columns.iter().map(|c| c.name.as_str()).collect()
}

pub async fn schema_round_trip(
    kind: DatabaseKind,
    s: &mut dyn DatabaseAdapter,
    schema: Option<&str>,
    copy: &mut dyn DatabaseAdapter,
    copy_schema: Option<&str>,
    ty: Types,
) {
    let owned = schema.map(str::to_owned);
    let parent = TableSchema {
        schema: owned.clone(),
        name: "it_parent".into(),
        columns: vec![
            ColumnDef {
                nullable: false,
                identity: true,
                ..ColumnDef::new("id", ty.int)
            },
            ColumnDef {
                nullable: false,
                ..ColumnDef::new("code", ty.varchar)
            },
        ],
        primary_key: vec!["id".into()],
        primary_key_name: Some("it_parent_pk".into()),
        indexes: vec![IndexDef {
            constraint: true,
            ..IndexDef::new("it_parent_code_uq", vec!["code".into()], true)
        }],
        foreign_keys: Vec::new(),
    };
    let child = TableSchema {
        schema: owned.clone(),
        name: "it_child".into(),
        columns: vec![
            ColumnDef {
                nullable: false,
                ..ColumnDef::new("id", ty.int)
            },
            ColumnDef::new("parent_id", ty.int),
            ColumnDef {
                nullable: false,
                default: Some("'x'".into()),
                ..ColumnDef::new("name", ty.varchar)
            },
            ColumnDef {
                default: Some("0".into()),
                ..ColumnDef::new("score", ty.int)
            },
        ],
        primary_key: vec!["id".into()],
        primary_key_name: Some("it_child_pk".into()),
        indexes: vec![IndexDef::new(
            "it_child_name_ix",
            vec!["name".into()],
            false,
        )],
        foreign_keys: vec![ForeignKeyDef {
            name: "it_child_parent_fk".into(),
            columns: vec!["parent_id".into()],
            ref_schema: None,
            ref_table: "it_parent".into(),
            ref_columns: vec!["id".into()],
        }],
    };
    run_all(s, &create_table(kind, &parent)).await;
    run_all(s, &create_table(kind, &child)).await;

    // 2. describe
    let tables = describe(s, schema).await;
    let p = find(&tables, "it_parent").clone();
    let c = find(&tables, "it_child").clone();
    assert_eq!(names(&p), ["id", "code"]);
    assert!(p.columns[0].identity && !p.columns[0].nullable, "{p:#?}");
    assert_eq!(p.primary_key, ["id"]);
    assert_eq!(p.indexes.len(), 1, "{p:#?}");
    assert_eq!(p.indexes[0].columns, ["code"]);
    assert!(p.indexes[0].unique);
    assert_eq!(names(&c), ["id", "parent_id", "name", "score"]);
    assert!(c.columns[1].nullable && !c.columns[2].nullable, "{c:#?}");
    assert!(
        c.columns[2]
            .default
            .as_deref()
            .is_some_and(|d| d.contains('x')),
        "{c:#?}"
    );
    assert!(
        c.columns[3]
            .default
            .as_deref()
            .is_some_and(|d| d.contains('0')),
        "{c:#?}"
    );
    assert_eq!(c.foreign_keys.len(), 1, "{c:#?}");
    if kind != DatabaseKind::Sqlite {
        // SQLite does not keep foreign key names.
        assert_eq!(c.foreign_keys[0].name, "it_child_parent_fk");
    }
    assert_eq!(c.foreign_keys[0].columns, ["parent_id"]);
    assert_eq!(c.foreign_keys[0].ref_table, "it_parent");
    assert_eq!(c.foreign_keys[0].ref_columns, ["id"]);
    assert_eq!(c.foreign_keys[0].ref_schema, None);
    assert!(
        c.indexes
            .iter()
            .any(|i| i.name == "it_child_name_ix" && !i.unique),
        "{c:#?}"
    );

    // Describing and diffing against itself yields nothing to do.
    assert_eq!(
        alter_table(kind, &c, &c, &[]).expect("noop diff"),
        Vec::<String>::new()
    );

    // 3. alter: rename name → full_name, score int → bigint without default, add note + index.
    let mut wanted = c.clone();
    wanted.columns[2].name = "full_name".into();
    wanted.columns[3].data_type = ty.bigint.into();
    wanted.columns[3].default = None;
    wanted.columns.push(ColumnDef::new("note", ty.text));
    for ix in &mut wanted.indexes {
        for col in &mut ix.columns {
            if col == "name" {
                *col = "full_name".into();
            }
        }
    }
    wanted.indexes.push(IndexDef::new(
        "it_child_score_ix",
        vec!["score".into()],
        false,
    ));
    let script = alter_table(kind, &c, &wanted, &[("name".into(), "full_name".into())])
        .expect("alter script");
    run_all(s, &script).await;
    let tables = describe(s, schema).await;
    let after = find(&tables, "it_child").clone();
    assert_eq!(
        names(&after),
        ["id", "parent_id", "full_name", "score", "note"],
        "{script:#?}"
    );
    assert_eq!(after.columns[3].default, None, "{after:#?}");
    assert!(
        after.columns[3]
            .data_type
            .to_ascii_lowercase()
            .contains("big")
            || after.columns[3].data_type.eq_ignore_ascii_case("int8"),
        "{after:#?}"
    );
    assert!(
        after.columns[2].default.is_some(),
        "rename keeps the default: {after:#?}"
    );
    let ix = after
        .indexes
        .iter()
        .find(|i| i.name == "it_child_name_ix")
        .expect("renamed column stays indexed");
    assert_eq!(ix.columns, ["full_name"]);
    assert!(after.indexes.iter().any(|i| i.name == "it_child_score_ix"));
    assert_eq!(after.foreign_keys, c.foreign_keys);

    // 4. recreate both from the described definitions elsewhere; they must match exactly.
    let relocate = |t: &TableSchema| TableSchema {
        schema: copy_schema.map(str::to_owned),
        ..t.clone()
    };
    run_all(copy, &create_table(kind, &relocate(&p))).await;
    run_all(copy, &create_table(kind, &relocate(&after))).await;
    let copies = describe(copy, copy_schema).await;
    assert_eq!(find(&copies, "it_parent"), &relocate(&p));
    assert_eq!(find(&copies, "it_child"), &relocate(&after));
}
