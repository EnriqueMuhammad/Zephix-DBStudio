//! Model of the visual query builder and its SQL generation.

use serde::{Deserialize, Serialize};

use crate::dialect::{qualified_name, quote_identifier};
use crate::export::sql_string;
use crate::profile::DatabaseKind;
use crate::table::TableSchema;

/// A table in the query, with its alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableRef {
    /// Schema.
    pub schema: Option<String>,
    /// Table name.
    pub name: String,
    /// Alias used in the SQL.
    pub alias: String,
}

/// A column of one of the query's tables.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColRef {
    /// Alias of the table.
    pub table: String,
    /// Column name.
    pub column: String,
}

/// Join type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum JoinKind {
    /// `INNER JOIN`.
    #[default]
    Inner,
    /// `LEFT JOIN`.
    Left,
    /// `RIGHT JOIN`.
    Right,
    /// `FULL JOIN`.
    Full,
}

impl JoinKind {
    /// All join kinds.
    pub const ALL: [JoinKind; 4] = [
        JoinKind::Inner,
        JoinKind::Left,
        JoinKind::Right,
        JoinKind::Full,
    ];

    /// SQL keyword.
    pub fn sql(self) -> &'static str {
        match self {
            JoinKind::Inner => "INNER JOIN",
            JoinKind::Left => "LEFT JOIN",
            JoinKind::Right => "RIGHT JOIN",
            JoinKind::Full => "FULL JOIN",
        }
    }
}

/// `left = right` join between a table already in the query (`left`) and another (`right`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Join {
    /// Join type.
    pub kind: JoinKind,
    /// Column of a table that precedes the joined one.
    pub left: ColRef,
    /// Column of the joined table.
    pub right: ColRef,
}

/// Aggregate function applied to a selected column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Aggregate {
    /// Plain column.
    #[default]
    None,
    /// `COUNT(col)`.
    Count,
    /// `SUM(col)`.
    Sum,
    /// `AVG(col)`.
    Avg,
    /// `MIN(col)`.
    Min,
    /// `MAX(col)`.
    Max,
}

impl Aggregate {
    /// All aggregates.
    pub const ALL: [Aggregate; 6] = [
        Aggregate::None,
        Aggregate::Count,
        Aggregate::Sum,
        Aggregate::Avg,
        Aggregate::Min,
        Aggregate::Max,
    ];

    /// Label / function name.
    pub fn label(self) -> &'static str {
        match self {
            Aggregate::None => "—",
            Aggregate::Count => "COUNT",
            Aggregate::Sum => "SUM",
            Aggregate::Avg => "AVG",
            Aggregate::Min => "MIN",
            Aggregate::Max => "MAX",
        }
    }
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Sort {
    /// Not sorted.
    #[default]
    None,
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
}

/// A selected output column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectColumn {
    /// Column.
    pub col: ColRef,
    /// Output alias.
    pub alias: String,
    /// Aggregate.
    pub aggregate: Aggregate,
    /// Sort.
    pub sort: Sort,
}

/// Comparison operator of a filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Operator {
    /// `=`
    #[default]
    Eq,
    /// `<>`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `LIKE`
    Like,
    /// `NOT LIKE`
    NotLike,
    /// `IN (…)`, comma-separated values.
    In,
    /// `IS NULL`
    IsNull,
    /// `IS NOT NULL`
    IsNotNull,
}

impl Operator {
    /// All operators.
    pub const ALL: [Operator; 11] = [
        Operator::Eq,
        Operator::Ne,
        Operator::Lt,
        Operator::Le,
        Operator::Gt,
        Operator::Ge,
        Operator::Like,
        Operator::NotLike,
        Operator::In,
        Operator::IsNull,
        Operator::IsNotNull,
    ];

    /// SQL text.
    pub fn sql(self) -> &'static str {
        match self {
            Operator::Eq => "=",
            Operator::Ne => "<>",
            Operator::Lt => "<",
            Operator::Le => "<=",
            Operator::Gt => ">",
            Operator::Ge => ">=",
            Operator::Like => "LIKE",
            Operator::NotLike => "NOT LIKE",
            Operator::In => "IN",
            Operator::IsNull => "IS NULL",
            Operator::IsNotNull => "IS NOT NULL",
        }
    }

    /// Whether the operator takes a value.
    pub fn has_value(self) -> bool {
        !matches!(self, Operator::IsNull | Operator::IsNotNull)
    }
}

/// A `WHERE` condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    /// Column.
    pub col: ColRef,
    /// Operator.
    pub op: Operator,
    /// Value as typed (numbers stay numbers, anything else becomes a string literal).
    pub value: String,
    /// Combine with the previous condition using OR instead of AND.
    pub or: bool,
}

/// Everything the builder knows about a query.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct QueryModel {
    /// Tables in FROM order.
    pub tables: Vec<TableRef>,
    /// Joins.
    pub joins: Vec<Join>,
    /// Output columns (empty = `*`).
    pub columns: Vec<SelectColumn>,
    /// Conditions.
    pub filters: Vec<Filter>,
    /// `SELECT DISTINCT`.
    pub distinct: bool,
    /// Row limit.
    pub limit: Option<u32>,
}

fn value_literal(kind: DatabaseKind, value: &str) -> String {
    let v = value.trim();
    if v.parse::<f64>().is_ok() {
        v.to_owned()
    } else {
        sql_string(v, kind)
    }
}

impl QueryModel {
    /// Adds a table with a unique alias and returns the alias. Foreign keys between the new
    /// table and tables already in the query become joins.
    pub fn add_table(&mut self, table: &TableSchema, all: &[TableSchema]) -> String {
        let base = table.name.clone();
        let mut alias = base.clone();
        let mut n = 2;
        while self.tables.iter().any(|t| t.alias == alias) {
            alias = format!("{base}{n}");
            n += 1;
        }
        let find = |r: &TableRef| {
            all.iter()
                .find(|t| t.name == r.name && t.schema == r.schema)
        };
        // new → existing (new table has the FK)
        let mut join = None;
        for existing in &self.tables {
            if let Some(fk) = table
                .foreign_keys
                .iter()
                .find(|fk| fk.ref_table == existing.name)
            {
                join = Some(Join {
                    kind: JoinKind::Inner,
                    left: ColRef {
                        table: existing.alias.clone(),
                        column: fk.ref_columns.first().cloned().unwrap_or_default(),
                    },
                    right: ColRef {
                        table: alias.clone(),
                        column: fk.columns.first().cloned().unwrap_or_default(),
                    },
                });
                break;
            }
            // existing → new (existing table has the FK)
            if let Some(def) = find(existing)
                && let Some(fk) = def
                    .foreign_keys
                    .iter()
                    .find(|fk| fk.ref_table == table.name)
            {
                join = Some(Join {
                    kind: JoinKind::Inner,
                    left: ColRef {
                        table: existing.alias.clone(),
                        column: fk.columns.first().cloned().unwrap_or_default(),
                    },
                    right: ColRef {
                        table: alias.clone(),
                        column: fk.ref_columns.first().cloned().unwrap_or_default(),
                    },
                });
                break;
            }
        }
        self.tables.push(TableRef {
            schema: table.schema.clone(),
            name: table.name.clone(),
            alias: alias.clone(),
        });
        if let Some(j) = join {
            self.joins.push(j);
        }
        alias
    }

    /// Removes a table and everything that refers to it.
    pub fn remove_table(&mut self, alias: &str) {
        self.tables.retain(|t| t.alias != alias);
        self.joins
            .retain(|j| j.left.table != alias && j.right.table != alias);
        self.columns.retain(|c| c.col.table != alias);
        self.filters.retain(|f| f.col.table != alias);
    }

    /// Toggles a column in the output list.
    pub fn toggle_column(&mut self, col: ColRef) {
        if let Some(i) = self.columns.iter().position(|c| c.col == col) {
            self.columns.remove(i);
        } else {
            self.columns.push(SelectColumn {
                col,
                alias: String::new(),
                aggregate: Aggregate::None,
                sort: Sort::None,
            });
        }
    }

    /// The SQL text.
    pub fn to_sql(&self, kind: DatabaseKind) -> String {
        if self.tables.is_empty() {
            return String::new();
        }
        let q = |s: &str| quote_identifier(kind, s);
        let col = |c: &ColRef| format!("{}.{}", q(&c.table), q(&c.column));
        let table_sql = |t: &TableRef| {
            let name = qualified_name(kind, None, t.schema.as_deref(), &t.name);
            if t.alias == t.name {
                name
            } else {
                format!("{name} {}", q(&t.alias))
            }
        };
        let select_list = if self.columns.is_empty() {
            "*".to_owned()
        } else {
            self.columns
                .iter()
                .map(|c| {
                    let expr = match c.aggregate {
                        Aggregate::None => col(&c.col),
                        a => format!("{}({})", a.label(), col(&c.col)),
                    };
                    if c.alias.trim().is_empty() {
                        expr
                    } else {
                        format!("{expr} AS {}", q(c.alias.trim()))
                    }
                })
                .collect::<Vec<_>>()
                .join(",\n       ")
        };
        let top = match (kind, self.limit) {
            (DatabaseKind::SqlServer, Some(n)) => format!("TOP {n} "),
            _ => String::new(),
        };
        let mut sql = format!(
            "SELECT {}{top}{select_list}\nFROM {}",
            if self.distinct { "DISTINCT " } else { "" },
            table_sql(&self.tables[0])
        );
        let mut placed = vec![self.tables[0].alias.clone()];
        for t in &self.tables[1..] {
            let joins: Vec<&Join> = self
                .joins
                .iter()
                .filter(|j| j.right.table == t.alias && placed.contains(&j.left.table))
                .collect();
            if joins.is_empty() {
                // Joined the other way round (left side is the new table)?
                let reverse: Vec<&Join> = self
                    .joins
                    .iter()
                    .filter(|j| j.left.table == t.alias && placed.contains(&j.right.table))
                    .collect();
                if reverse.is_empty() {
                    sql.push_str(&format!("\nCROSS JOIN {}", table_sql(t)));
                } else {
                    let on = reverse
                        .iter()
                        .map(|j| format!("{} = {}", col(&j.right), col(&j.left)))
                        .collect::<Vec<_>>()
                        .join(" AND ");
                    sql.push_str(&format!(
                        "\n{} {} ON {on}",
                        reverse[0].kind.sql(),
                        table_sql(t)
                    ));
                }
            } else {
                let on = joins
                    .iter()
                    .map(|j| format!("{} = {}", col(&j.left), col(&j.right)))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                sql.push_str(&format!(
                    "\n{} {} ON {on}",
                    joins[0].kind.sql(),
                    table_sql(t)
                ));
            }
            placed.push(t.alias.clone());
        }
        if !self.filters.is_empty() {
            let conditions = self
                .filters
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let cond = match f.op {
                        Operator::IsNull | Operator::IsNotNull => {
                            format!("{} {}", col(&f.col), f.op.sql())
                        }
                        Operator::In => format!(
                            "{} IN ({})",
                            col(&f.col),
                            f.value
                                .split(',')
                                .filter(|v| !v.trim().is_empty())
                                .map(|v| value_literal(kind, v))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        op => format!(
                            "{} {} {}",
                            col(&f.col),
                            op.sql(),
                            value_literal(kind, &f.value)
                        ),
                    };
                    match i {
                        0 => cond,
                        _ if f.or => format!("\n   OR {cond}"),
                        _ => format!("\n  AND {cond}"),
                    }
                })
                .collect::<String>();
            sql.push_str(&format!("\nWHERE {conditions}"));
        }
        if self.columns.iter().any(|c| c.aggregate != Aggregate::None) {
            let groups: Vec<String> = self
                .columns
                .iter()
                .filter(|c| c.aggregate == Aggregate::None)
                .map(|c| col(&c.col))
                .collect();
            if !groups.is_empty() {
                sql.push_str(&format!("\nGROUP BY {}", groups.join(", ")));
            }
        }
        let order: Vec<String> = self
            .columns
            .iter()
            .filter(|c| c.sort != Sort::None)
            .map(|c| {
                let expr = match c.aggregate {
                    Aggregate::None => col(&c.col),
                    a => format!("{}({})", a.label(), col(&c.col)),
                };
                format!(
                    "{expr} {}",
                    if c.sort == Sort::Asc { "ASC" } else { "DESC" }
                )
            })
            .collect();
        if !order.is_empty() {
            sql.push_str(&format!("\nORDER BY {}", order.join(", ")));
        }
        if kind != DatabaseKind::SqlServer
            && let Some(n) = self.limit
        {
            sql.push_str(&format!("\nLIMIT {n}"));
        }
        sql
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{ColumnDef, ForeignKeyDef};

    fn tables() -> Vec<TableSchema> {
        let customers = TableSchema {
            schema: Some("public".into()),
            name: "customers".into(),
            columns: vec![ColumnDef::new("id", "int"), ColumnDef::new("name", "text")],
            primary_key: vec!["id".into()],
            ..TableSchema::default()
        };
        let orders = TableSchema {
            schema: Some("public".into()),
            name: "orders".into(),
            columns: vec![
                ColumnDef::new("id", "int"),
                ColumnDef::new("customer_id", "int"),
                ColumnDef::new("total", "numeric"),
            ],
            primary_key: vec!["id".into()],
            foreign_keys: vec![ForeignKeyDef {
                name: "fk".into(),
                columns: vec!["customer_id".into()],
                ref_schema: None,
                ref_table: "customers".into(),
                ref_columns: vec!["id".into()],
            }],
            ..TableSchema::default()
        };
        vec![customers, orders]
    }

    #[test]
    fn joins_from_foreign_keys_and_aggregates() {
        let all = tables();
        let mut m = QueryModel::default();
        let c = m.add_table(&all[0], &all);
        let o = m.add_table(&all[1], &all);
        assert_eq!(m.joins.len(), 1, "foreign key became a join");
        m.toggle_column(ColRef {
            table: c.clone(),
            column: "name".into(),
        });
        m.toggle_column(ColRef {
            table: o.clone(),
            column: "total".into(),
        });
        m.columns[1].aggregate = Aggregate::Sum;
        m.columns[1].alias = "revenue".into();
        m.columns[1].sort = Sort::Desc;
        m.filters.push(Filter {
            col: ColRef {
                table: c.clone(),
                column: "name".into(),
            },
            op: Operator::Like,
            value: "A%".into(),
            or: false,
        });
        m.filters.push(Filter {
            col: ColRef {
                table: o.clone(),
                column: "total".into(),
            },
            op: Operator::Gt,
            value: "10".into(),
            or: true,
        });
        m.limit = Some(5);
        assert_eq!(
            m.to_sql(DatabaseKind::PostgreSql),
            "SELECT \"customers\".\"name\",\n       SUM(\"orders\".\"total\") AS \"revenue\"\n\
             FROM \"public\".\"customers\"\n\
             INNER JOIN \"public\".\"orders\" ON \"customers\".\"id\" = \"orders\".\"customer_id\"\n\
             WHERE \"customers\".\"name\" LIKE 'A%'\n   OR \"orders\".\"total\" > 10\n\
             GROUP BY \"customers\".\"name\"\n\
             ORDER BY SUM(\"orders\".\"total\") DESC\n\
             LIMIT 5"
        );
        let ms = m.to_sql(DatabaseKind::SqlServer);
        assert!(ms.starts_with("SELECT TOP 5 [customers].[name]"), "{ms}");
        assert!(!ms.contains("LIMIT"));
    }

    #[test]
    fn self_join_gets_unique_alias_and_removal_cleans_up() {
        let all = tables();
        let mut m = QueryModel::default();
        m.add_table(&all[0], &all);
        let second = m.add_table(&all[0], &all);
        assert_eq!(second, "customers2");
        assert!(
            m.to_sql(DatabaseKind::PostgreSql)
                .contains("CROSS JOIN \"public\".\"customers\" \"customers2\"")
        );
        m.toggle_column(ColRef {
            table: second.clone(),
            column: "id".into(),
        });
        m.remove_table(&second);
        assert!(m.columns.is_empty());
        assert_eq!(m.to_sql(DatabaseKind::MySql), "SELECT *\nFROM `customers`");
    }

    #[test]
    fn in_and_null_filters() {
        let all = tables();
        let mut m = QueryModel::default();
        let c = m.add_table(&all[0], &all);
        m.filters.push(Filter {
            col: ColRef {
                table: c.clone(),
                column: "id".into(),
            },
            op: Operator::In,
            value: "1, 2,x".into(),
            or: false,
        });
        m.filters.push(Filter {
            col: ColRef {
                table: c,
                column: "name".into(),
            },
            op: Operator::IsNotNull,
            value: String::new(),
            or: false,
        });
        let sql = m.to_sql(DatabaseKind::PostgreSql);
        assert!(sql.contains("\"id\" IN (1, 2, 'x')"), "{sql}");
        assert!(
            sql.contains("AND \"customers\".\"name\" IS NOT NULL"),
            "{sql}"
        );
    }
}
