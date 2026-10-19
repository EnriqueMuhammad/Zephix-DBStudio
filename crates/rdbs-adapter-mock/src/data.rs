//! Deterministic sample schema and row generation.

use rdbs_core::{
    CellValue, ColumnDef, ColumnMeta, DatabaseKind, DbObject, ForeignKeyDef, IndexDef, ObjectKind,
    ResultSet, TableSchema,
};

/// Logical column types, mapped to engine-native type names.
#[derive(Clone, Copy)]
enum Ty {
    Int,
    Name,
    Email,
    Short,
    Timestamp,
    Money,
    Bool,
    Text,
}

fn native(kind: DatabaseKind, ty: Ty) -> &'static str {
    use DatabaseKind::*;
    match (ty, kind) {
        (Ty::Int, PostgreSql) => "int4",
        (Ty::Int, SqlServer) => "int",
        (Ty::Int, MySql | MariaDb) => "INT",
        (Ty::Int, Sqlite) => "INTEGER",
        (Ty::Name | Ty::Email, PostgreSql) => "varchar(100)",
        (Ty::Name | Ty::Email, SqlServer) => "nvarchar(100)",
        (Ty::Name | Ty::Email, MySql | MariaDb) => "VARCHAR(100)",
        (Ty::Short, PostgreSql) => "varchar(40)",
        (Ty::Short, SqlServer) => "nvarchar(40)",
        (Ty::Short, MySql | MariaDb) => "VARCHAR(40)",
        (Ty::Name | Ty::Email | Ty::Short | Ty::Text, Sqlite) => "TEXT",
        (Ty::Timestamp, PostgreSql) => "timestamptz",
        (Ty::Timestamp, SqlServer) => "datetime2",
        (Ty::Timestamp, MySql | MariaDb) => "DATETIME",
        (Ty::Timestamp, Sqlite) => "TEXT",
        (Ty::Money, PostgreSql) => "numeric(12,2)",
        (Ty::Money, SqlServer) => "decimal(12,2)",
        (Ty::Money, MySql | MariaDb) => "DECIMAL(12,2)",
        (Ty::Money, Sqlite) => "NUMERIC",
        (Ty::Bool, PostgreSql) => "bool",
        (Ty::Bool, SqlServer) => "bit",
        (Ty::Bool, MySql | MariaDb) => "TINYINT(1)",
        (Ty::Bool, Sqlite) => "INTEGER",
        (Ty::Text, PostgreSql) => "text",
        (Ty::Text, SqlServer) => "nvarchar(max)",
        (Ty::Text, MySql | MariaDb) => "TEXT",
        (Ty::Int, Oracle) => "NUMBER(10)",
        (Ty::Name | Ty::Email, Oracle) => "VARCHAR2(100 CHAR)",
        (Ty::Short, Oracle) => "VARCHAR2(40 CHAR)",
        (Ty::Timestamp, Oracle) => "TIMESTAMP(6)",
        (Ty::Money, Oracle) => "NUMBER(12,2)",
        (Ty::Bool, Oracle) => "NUMBER(1)",
        (Ty::Text, Oracle) => "CLOB",
        (Ty::Int, MongoDb | Redis) => "int",
        (Ty::Money, MongoDb | Redis) => "decimal",
        (Ty::Bool, MongoDb | Redis) => "bool",
        (Ty::Timestamp, MongoDb | Redis) => "date",
        (_, MongoDb | Redis) => "string",
    }
}

/// Which sample table a query targets.
#[derive(Clone, Copy)]
pub(crate) enum Dataset {
    Customers,
    Orders,
}

const CUSTOMER_COLS: &[(&str, Ty)] = &[
    ("id", Ty::Int),
    ("full_name", Ty::Name),
    ("email", Ty::Email),
    ("country", Ty::Short),
    ("city", Ty::Short),
    ("created_at", Ty::Timestamp),
    ("balance", Ty::Money),
    ("is_active", Ty::Bool),
    ("notes", Ty::Text),
];

const ORDER_COLS: &[(&str, Ty)] = &[
    ("order_id", Ty::Int),
    ("customer_id", Ty::Int),
    ("status", Ty::Short),
    ("ordered_at", Ty::Timestamp),
    ("total_amount", Ty::Money),
    ("paid", Ty::Bool),
];

const SIMPLE_COLS: &[(&str, Ty)] = &[
    ("id", Ty::Int),
    ("name", Ty::Name),
    ("updated_at", Ty::Timestamp),
];

const ORDER_ITEM_COLS: &[(&str, Ty)] = &[
    ("id", Ty::Int),
    ("order_id", Ty::Int),
    ("product_id", Ty::Int),
    ("quantity", Ty::Int),
    ("unit_price", Ty::Money),
];

/// Column specs of one sample table.
type Columns = &'static [(&'static str, Ty)];

/// Sample tables: name, columns, approximate rows.
const TABLES: &[(&str, Columns, &str)] = &[
    ("customers", CUSTOMER_COLS, "~1.2M rows"),
    ("orders", ORDER_COLS, "~8.4M rows"),
    ("order_items", ORDER_ITEM_COLS, "~21M rows"),
    ("products", SIMPLE_COLS, "~3.1K rows"),
    ("invoices", SIMPLE_COLS, "~640K rows"),
];

/// Foreign keys between the sample tables: (table, column, referenced table, column).
const RELATIONS: &[(&str, &str, &str, &str)] = &[
    ("orders", "customer_id", "customers", "id"),
    ("order_items", "order_id", "orders", "order_id"),
    ("order_items", "product_id", "products", "id"),
];

const FIRST: &[&str] = &[
    "Ayu", "Budi", "Citra", "Dewi", "Eko", "Fajar", "Gita", "Hana", "Indra", "Joko", "Kirana",
    "Lukas", "Maya", "Nadia", "Oscar", "Putri", "Rizky", "Sari", "Tomas", "Wulan",
];
const LAST: &[&str] = &[
    "Santoso", "Wijaya", "Pratama", "Lestari", "Nugroho", "Hidayat", "Kusuma", "Saputra", "Halim",
    "Gunawan", "Tan", "Lim",
];
const PLACES: &[(&str, &str)] = &[
    ("Indonesia", "Jakarta"),
    ("Indonesia", "Surabaya"),
    ("Indonesia", "Bandung"),
    ("Singapore", "Singapore"),
    ("Malaysia", "Kuala Lumpur"),
    ("Japan", "Osaka"),
    ("Germany", "Berlin"),
    ("Netherlands", "Rotterdam"),
];
const STATUSES: &[&str] = &["pending", "paid", "shipped", "delivered", "cancelled"];

/// SplitMix64: tiny deterministic hash so the same row always looks the same.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn pick<T: Copy>(items: &[T], seed: u64) -> T {
    // `items` is never empty; modulo keeps the index in range.
    items[(seed % items.len() as u64) as usize]
}

fn timestamp(seed: u64) -> String {
    let month = seed % 12 + 1;
    let day = (seed >> 8) % 28 + 1;
    let hour = (seed >> 16) % 24;
    let minute = (seed >> 24) % 60;
    let second = (seed >> 32) % 60;
    format!("2026-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

fn money(seed: u64) -> String {
    let cents = seed % 2_500_000;
    format!("{}.{:02}", cents / 100, cents % 100)
}

pub(crate) fn generate(kind: DatabaseKind, dataset: Dataset, rows: usize) -> ResultSet {
    let cols = match dataset {
        Dataset::Customers => CUSTOMER_COLS,
        Dataset::Orders => ORDER_COLS,
    };
    let columns = cols
        .iter()
        .map(|&(name, ty)| ColumnMeta {
            name: name.to_owned(),
            type_name: native(kind, ty).to_owned(),
            nullable: Some(!matches!(ty, Ty::Int)),
        })
        .collect();

    let rows = (0..rows as u64)
        .map(|i| {
            let s = mix(i);
            let id = i as i64 + 1;
            match dataset {
                Dataset::Customers => {
                    let first = pick(FIRST, s);
                    let last = pick(LAST, s >> 7);
                    let (country, city) = pick(PLACES, s >> 13);
                    vec![
                        CellValue::Int(id),
                        CellValue::Text(format!("{first} {last}")),
                        CellValue::Text(format!(
                            "{}.{}{}@example.com",
                            first.to_lowercase(),
                            last.to_lowercase(),
                            id
                        )),
                        CellValue::Text(country.to_owned()),
                        if s.is_multiple_of(17) {
                            CellValue::Null
                        } else {
                            CellValue::Text(city.to_owned())
                        },
                        CellValue::Temporal(timestamp(s >> 3)),
                        CellValue::Decimal(money(s >> 5)),
                        CellValue::Bool(!s.is_multiple_of(5)),
                        if s.is_multiple_of(3) {
                            CellValue::Text("VIP customer, prefers email contact".to_owned())
                        } else {
                            CellValue::Null
                        },
                    ]
                }
                Dataset::Orders => vec![
                    CellValue::Int(10_000 + id),
                    CellValue::Int((s % 5_000) as i64 + 1),
                    CellValue::Text(pick(STATUSES, s >> 11).to_owned()),
                    CellValue::Temporal(timestamp(s >> 2)),
                    CellValue::Decimal(money(s >> 9)),
                    CellValue::Bool(!s.is_multiple_of(4)),
                ],
            }
        })
        .collect();

    ResultSet { columns, rows }
}

fn table(kind: DatabaseKind, name: &str, cols: &[(&str, Ty)], rows: &str) -> DbObject {
    let columns = cols
        .iter()
        .enumerate()
        .map(|(i, &(col, ty))| {
            let mut detail = native(kind, ty).to_owned();
            if i == 0 {
                detail.push_str(" · PK");
            }
            DbObject::new(col, ObjectKind::Column).with_detail(detail)
        })
        .collect();
    DbObject::new(name, ObjectKind::Table)
        .with_detail(rows)
        .with_children(columns)
}

fn folder(name: &str, children: Vec<DbObject>) -> DbObject {
    DbObject::new(name, ObjectKind::Folder).with_children(children)
}

fn schema_contents(kind: DatabaseKind) -> Vec<DbObject> {
    let mut folders = vec![
        folder(
            "Tables",
            TABLES
                .iter()
                .map(|&(name, cols, rows)| table(kind, name, cols, rows))
                .collect(),
        ),
        folder(
            "Views",
            vec![
                DbObject::new("active_customers", ObjectKind::View),
                DbObject::new("monthly_revenue", ObjectKind::View),
            ],
        ),
    ];
    if kind == DatabaseKind::PostgreSql {
        folders.push(folder(
            "Materialized Views",
            vec![DbObject::new("sales_summary", ObjectKind::MaterializedView)],
        ));
    }
    if kind != DatabaseKind::Sqlite {
        folders.push(folder(
            "Functions",
            vec![
                DbObject::new("calc_tax", ObjectKind::Function).with_detail("numeric"),
                DbObject::new("customer_tier", ObjectKind::Function).with_detail("text"),
            ],
        ));
        folders.push(folder(
            "Procedures",
            vec![
                DbObject::new("refresh_statistics", ObjectKind::Procedure),
                DbObject::new("archive_orders", ObjectKind::Procedure),
            ],
        ));
    }
    folders
}

/// Parameters of the sample routines.
pub fn sample_routine(name: &str) -> Option<rdbs_core::routine::RoutineSignature> {
    use rdbs_core::routine::{ParamMode, RoutineParam, RoutineSignature};
    let param = |name: &str, data_type: &str, mode: ParamMode| RoutineParam {
        name: name.to_owned(),
        data_type: data_type.to_owned(),
        mode,
    };
    let params = match name {
        "calc_tax" => vec![
            param("amount", "numeric(12,2)", ParamMode::In),
            param("rate", "numeric(5,2)", ParamMode::In),
        ],
        "customer_tier" => vec![param("customer_id", "integer", ParamMode::In)],
        "refresh_statistics" => Vec::new(),
        "archive_orders" => vec![
            param("before_date", "date", ParamMode::In),
            param("moved", "integer", ParamMode::Out),
        ],
        _ => return None,
    };
    Some(RoutineSignature {
        params,
        returns_table: false,
    })
}

/// Definition of a sample view or routine, roughly as `kind` would report it.
pub fn sample_routine_ddl(
    kind: DatabaseKind,
    object: &rdbs_core::schema::ObjectPath,
) -> Option<String> {
    let name = rdbs_core::dialect::qualified_name(
        kind,
        object.database.as_deref(),
        object.schema.as_deref(),
        &object.name,
    );
    let signature = sample_routine(&object.name);
    let params = signature
        .as_ref()
        .map(|s| {
            s.params
                .iter()
                .map(|p| match kind {
                    DatabaseKind::SqlServer => format!(
                        "@{} {}{}",
                        p.name,
                        p.data_type,
                        if p.mode.returns_value() {
                            " OUTPUT"
                        } else {
                            ""
                        }
                    ),
                    _ => format!(
                        "{}{} {}",
                        if p.mode.returns_value() { "OUT " } else { "" },
                        p.name,
                        p.data_type
                    ),
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let body = match kind {
        DatabaseKind::SqlServer => "AS\nBEGIN\n    SET NOCOUNT ON;\n    -- sample body\nEND",
        DatabaseKind::MySql | DatabaseKind::MariaDb => "BEGIN\n    -- sample body\nEND",
        _ => "AS $$\nBEGIN\n    -- sample body\nEND;\n$$ LANGUAGE plpgsql",
    };
    match object.kind {
        ObjectKind::View | ObjectKind::MaterializedView => Some(format!(
            "CREATE VIEW {name} AS\nSELECT id, full_name, country\nFROM customers\nWHERE is_active;"
        )),
        ObjectKind::Function => {
            signature.as_ref()?;
            let returns = if object.name == "customer_tier" {
                "text"
            } else {
                "numeric"
            };
            Some(match kind {
                DatabaseKind::SqlServer => {
                    format!("CREATE FUNCTION {name}({params})\nRETURNS {returns}\n{body}")
                }
                DatabaseKind::MySql | DatabaseKind::MariaDb => format!(
                    "CREATE DEFINER=`app`@`%` FUNCTION {name}({params}) RETURNS {returns}\n{body};"
                ),
                _ => format!(
                    "CREATE OR REPLACE FUNCTION {name}({params})\n RETURNS {returns}\n {body};"
                ),
            })
        }
        ObjectKind::Procedure => {
            signature.as_ref()?;
            Some(match kind {
                DatabaseKind::SqlServer => format!("CREATE PROCEDURE {name} {params}\n{body}"),
                DatabaseKind::MySql | DatabaseKind::MariaDb => {
                    format!("CREATE DEFINER=`app`@`%` PROCEDURE {name}({params})\n{body};")
                }
                _ => format!("CREATE OR REPLACE PROCEDURE {name}({params})\n {body};"),
            })
        }
        _ => None,
    }
}

/// Structured definitions of the sample tables (in `schema`, for engines with schemas).
pub fn sample_tables(kind: DatabaseKind, schema: Option<&str>) -> Vec<TableSchema> {
    let schema = schema.map(str::to_owned).or_else(|| match kind {
        DatabaseKind::PostgreSql => Some("public".into()),
        DatabaseKind::SqlServer => Some("dbo".into()),
        _ => None,
    });
    TABLES
        .iter()
        .map(|&(name, cols, _)| {
            let columns = cols
                .iter()
                .enumerate()
                .map(|(i, &(col, ty))| ColumnDef {
                    nullable: i != 0,
                    identity: i == 0,
                    ..ColumnDef::new(col, native(kind, ty))
                })
                .collect();
            let foreign_keys = RELATIONS
                .iter()
                .filter(|r| r.0 == name)
                .map(|&(_, col, ref_table, ref_col)| ForeignKeyDef {
                    name: format!("fk_{name}_{col}"),
                    columns: vec![col.into()],
                    ref_schema: None,
                    ref_table: ref_table.into(),
                    ref_columns: vec![ref_col.into()],
                })
                .collect();
            let indexes = if name == "customers" {
                vec![IndexDef::new(
                    "ix_customers_email",
                    vec!["email".into()],
                    true,
                )]
            } else {
                Vec::new()
            };
            TableSchema {
                schema: schema.clone(),
                name: name.into(),
                columns,
                primary_key: vec![cols[0].0.into()],
                primary_key_name: Some(format!("pk_{name}")),
                indexes,
                foreign_keys,
            }
        })
        .collect()
}

/// Object tree for a mock server of the given engine.
pub fn sample_objects(kind: DatabaseKind) -> Vec<DbObject> {
    let databases: &[&str] = match kind {
        DatabaseKind::PostgreSql => &["app_dev", "analytics"],
        DatabaseKind::SqlServer => &["SalesDB", "HR", "ReportServer"],
        DatabaseKind::MySql => &["shop", "inventory", "sys"],
        DatabaseKind::MariaDb => &["inventory", "wordpress"],
        DatabaseKind::Sqlite => &["main"],
        DatabaseKind::Oracle => &["FREEPDB1"],
        DatabaseKind::MongoDb => &["shop", "logs"],
        DatabaseKind::Redis => &["db0", "db1"],
    };
    let schemas: &[&str] = match kind {
        DatabaseKind::PostgreSql => &["public", "audit"],
        DatabaseKind::SqlServer => &["dbo", "sales"],
        DatabaseKind::Oracle => &["APP", "HR"],
        _ => &[],
    };
    databases
        .iter()
        .map(|db| {
            let children = if schemas.is_empty() {
                schema_contents(kind)
            } else {
                schemas
                    .iter()
                    .map(|s| {
                        DbObject::new(*s, ObjectKind::Schema).with_children(schema_contents(kind))
                    })
                    .collect()
            };
            DbObject::new(*db, ObjectKind::Database).with_children(children)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic() {
        let a = generate(DatabaseKind::PostgreSql, Dataset::Orders, 50);
        let b = generate(DatabaseKind::PostgreSql, Dataset::Orders, 50);
        assert_eq!(a, b);
    }

    #[test]
    fn schemas_only_for_engines_that_have_them() {
        let pg = sample_objects(DatabaseKind::PostgreSql);
        assert_eq!(pg[0].children[0].kind, ObjectKind::Schema);
        let my = sample_objects(DatabaseKind::MySql);
        assert_eq!(my[0].children[0].kind, ObjectKind::Folder);
    }
}
