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
    let mut tree: Vec<DbObject> = databases
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
            let mut children = children;
            if kind == DatabaseKind::SqlServer {
                children.extend(sql_server_folders());
            }
            DbObject::new(*db, ObjectKind::Database).with_children(children)
        })
        .collect();
    if kind == DatabaseKind::SqlServer {
        tree.push(rdbs_core::agent::jobs_node(
            &sample_agent_summaries(),
            false,
        ));
    }
    tree
}

/// Demo Programmability and Security folders of a SQL Server database.
fn sql_server_folders() -> Vec<DbObject> {
    use rdbs_core::schema::{PROGRAMMABILITY_FOLDER, SECURITY_FOLDER};
    let entry = |schema: &str, name: &str, kind: ObjectKind, detail: &str| {
        let mut obj = DbObject::new(name, kind);
        if !schema.is_empty() {
            obj = obj.with_schema(schema);
        }
        if detail.is_empty() {
            obj
        } else {
            obj.with_detail(detail)
        }
    };
    let catalog = |items: &[(&str, &str, &str)]| -> Vec<DbObject> {
        items
            .iter()
            .map(|(schema, name, detail)| entry(schema, name, ObjectKind::CatalogEntry, detail))
            .collect()
    };
    let sub = |name: &str, items: Vec<DbObject>| folder(name, items);
    vec![
        folder(
            PROGRAMMABILITY_FOLDER,
            vec![
                sub(
                    "Stored Procedures",
                    vec![
                        entry("dbo", "archive_orders", ObjectKind::Procedure, ""),
                        entry("dbo", "refresh_statistics", ObjectKind::Procedure, ""),
                    ],
                ),
                sub(
                    "Functions",
                    vec![
                        entry("dbo", "calc_tax", ObjectKind::Function, "scalar"),
                        entry("dbo", "customer_tier", ObjectKind::Function, "scalar"),
                    ],
                ),
                sub("Database Triggers", catalog(&[("", "audit_ddl", "")])),
                sub("Assemblies", Vec::new()),
                sub(
                    "Types",
                    catalog(&[
                        ("dbo", "OrderLines", "table type"),
                        ("dbo", "Phone", "varchar"),
                    ]),
                ),
                sub("Rules", Vec::new()),
                sub("Defaults", Vec::new()),
                sub("Plan Guides", Vec::new()),
                sub("Sequences", catalog(&[("dbo", "invoice_no", "bigint")])),
            ],
        ),
        folder(
            SECURITY_FOLDER,
            vec![
                sub(
                    "Users",
                    catalog(&[
                        ("", "dbo", "sql user"),
                        ("", "guest", "sql user"),
                        ("", "report_reader", "sql user"),
                    ]),
                ),
                sub(
                    "Roles",
                    catalog(&[
                        ("", "db_datareader", "database role"),
                        ("", "db_owner", "database role"),
                        ("", "public", "database role"),
                    ]),
                ),
                sub(
                    "Schemas",
                    catalog(&[("", "dbo", "owner dbo"), ("", "sales", "owner dbo")]),
                ),
                sub("Asymmetric Keys", Vec::new()),
                sub("Certificates", Vec::new()),
                sub("Symmetric Keys", Vec::new()),
                sub("Always Encrypted Keys", Vec::new()),
                sub("Database Audit Specifications", Vec::new()),
                sub("Security Policies", Vec::new()),
            ],
        ),
    ]
}

/// Demo SQL Server Agent jobs, matching what a Developer edition would list.
pub(crate) fn sample_agent_summaries() -> Vec<rdbs_core::agent::AgentJobSummary> {
    use rdbs_core::agent::{AgentJobSummary, AgentStep};
    vec![
        AgentJobSummary {
            name: "Nightly index maintenance".into(),
            enabled: true,
            owner: "sa".into(),
            category: "Database Maintenance".into(),
            last_outcome: Some(1),
            last_run_date: Some(20261023),
            last_run_time: Some(20_000),
            next_run_date: Some(20261024),
            next_run_time: Some(20_000),
            running: true,
            current_step: "Rebuild indexes".into(),
            steps: vec![
                AgentStep {
                    id: 1,
                    name: "Rebuild indexes".into(),
                },
                AgentStep {
                    id: 2,
                    name: "Update statistics".into(),
                },
            ],
        },
        AgentJobSummary {
            name: "Failed backup".into(),
            enabled: false,
            owner: "sa".into(),
            category: "Database Maintenance".into(),
            last_outcome: Some(0),
            last_run_date: Some(20261022),
            last_run_time: Some(231_504),
            next_run_date: None,
            next_run_time: None,
            running: false,
            current_step: String::new(),
            steps: vec![AgentStep {
                id: 1,
                name: "Run".into(),
            }],
        },
    ]
}

/// Definition used when demo mode scripts an Agent job.
pub fn sample_agent_job(name: &str) -> rdbs_core::agent::AgentJobDef {
    use rdbs_core::agent::{AgentJobDef, AgentJobSchedule, AgentJobStep};
    AgentJobDef {
        name: name.to_owned(),
        enabled: name != "Failed backup",
        description: "Sample SQL Server Agent job (demo mode).".into(),
        category: "Database Maintenance".into(),
        owner: "sa".into(),
        start_step_id: 1,
        notify_level_eventlog: 0,
        notify_level_email: 0,
        notify_level_netsend: 0,
        notify_level_page: 0,
        delete_level: 0,
        steps: vec![AgentJobStep {
            step_id: 1,
            name: "Run".into(),
            subsystem: "TSQL".into(),
            command: "SELECT 1 AS zephix_agent;".into(),
            database_name: "master".into(),
            database_user_name: String::new(),
            success_code: 0,
            on_success_action: 1,
            on_success_step_id: 0,
            on_fail_action: 2,
            on_fail_step_id: 0,
            retry_attempts: 0,
            retry_interval: 0,
            os_run_priority: 0,
            flags: 0,
            output_file_name: String::new(),
        }],
        schedules: vec![AgentJobSchedule {
            name: "nightly".into(),
            enabled: true,
            freq_type: 4,
            freq_interval: 1,
            freq_subday_type: 1,
            freq_subday_interval: 0,
            freq_relative_interval: 0,
            freq_recurrence_factor: 0,
            active_start_date: 20261024,
            active_end_date: 99991231,
            active_start_time: 20000,
            active_end_time: 235959,
        }],
    }
}

/// A few `sysjobhistory` rows, so demo mode's View History opens a real grid.
pub fn sample_job_history() -> ResultSet {
    let col = |name, ty| ColumnMeta::new(name, ty);
    ResultSet {
        columns: vec![
            col("instance_id", "int"),
            col("step_id", "int"),
            col("step_name", "nvarchar"),
            col("outcome", "nvarchar"),
            col("run_time", "varchar"),
            col("duration", "varchar"),
            col("retries", "int"),
            col("sql_severity", "int"),
            col("sql_message_id", "int"),
            col("message", "nvarchar"),
        ],
        rows: vec![
            vec![
                CellValue::Int(2),
                CellValue::Int(0),
                CellValue::Text("(Job outcome)".into()),
                CellValue::Text("Failed".into()),
                CellValue::Text("2026-10-22 23:15:04".into()),
                CellValue::Text("00:00:12".into()),
                CellValue::Int(0),
                CellValue::Int(16),
                CellValue::Int(50000),
                CellValue::Text("The job failed. The step 1 message has the details.".into()),
            ],
            vec![
                CellValue::Int(1),
                CellValue::Int(1),
                CellValue::Text("Run".into()),
                CellValue::Text("Failed".into()),
                CellValue::Text("2026-10-22 23:15:04".into()),
                CellValue::Text("00:00:12".into()),
                CellValue::Int(0),
                CellValue::Int(16),
                CellValue::Int(50000),
                CellValue::Text("Backup device is offline. The statement was aborted.".into()),
            ],
        ],
    }
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

    #[test]
    fn sql_server_lists_agent_jobs() {
        let tree = sample_objects(DatabaseKind::SqlServer);
        let agent = tree
            .iter()
            .find(|o| o.name == rdbs_core::agent::AGENT_FOLDER)
            .expect("agent folder");
        let jobs = &agent.children[0];
        assert_eq!(jobs.name, rdbs_core::agent::JOBS_FOLDER);
        assert!(
            jobs.children
                .iter()
                .any(|j| j.kind == ObjectKind::AgentJob && j.name == "Failed backup")
        );
        let nightly = jobs
            .children
            .iter()
            .find(|j| j.name == "Nightly index maintenance")
            .expect("nightly");
        assert!(
            nightly
                .detail
                .as_deref()
                .unwrap_or("")
                .starts_with("running")
        );
        assert!(nightly.notes.iter().any(|n| n == "running"));
        assert!(
            tree.iter()
                .filter(|o| o.kind == ObjectKind::Database)
                .count()
                >= 1
        );
    }
}
