//! Execution plans: a common tree model plus parsers for the engine formats that are plain text
//! or JSON (PostgreSQL JSON, MySQL `FORMAT=TREE`, MariaDB JSON, SQLite `EXPLAIN QUERY PLAN`).
//! SQL Server's XML showplan is parsed in its adapter.

use serde_json::Value;

use crate::error::{DbError, DbErrorKind};

/// One operator of a plan.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanNode {
    /// Operator, e.g. `Seq Scan`, `Nested loop inner join`, `Clustered Index Seek`.
    pub operation: String,
    /// Object the operator works on (table, index), if any.
    pub object: Option<String>,
    /// Estimated total cost (engine units).
    pub cost: Option<f64>,
    /// Estimated rows.
    pub rows: Option<f64>,
    /// Actual rows (analyzed plans).
    pub actual_rows: Option<f64>,
    /// Actual time in milliseconds (analyzed plans).
    pub actual_ms: Option<f64>,
    /// Further properties (filter, join condition, …) as label/value pairs.
    pub details: Vec<(String, String)>,
    /// Inputs of this operator.
    pub children: Vec<PlanNode>,
}

impl PlanNode {
    /// Node with just an operation name.
    pub fn new(operation: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            ..Self::default()
        }
    }

    /// Largest cost in this subtree.
    pub fn max_cost(&self) -> f64 {
        self.children
            .iter()
            .map(PlanNode::max_cost)
            .fold(self.cost.unwrap_or(0.0), f64::max)
    }

    /// Number of nodes in this subtree.
    pub fn len(&self) -> usize {
        1 + self.children.iter().map(PlanNode::len).sum::<usize>()
    }

    /// Always false: a node counts itself.
    pub fn is_empty(&self) -> bool {
        false
    }
}

/// A complete plan, possibly with several roots (one per statement).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExplainPlan {
    /// Top-level operators.
    pub roots: Vec<PlanNode>,
    /// The plan as returned by the server (JSON, text or XML), for the "raw" view.
    pub raw: String,
    /// Whether the statement was executed to collect actual figures.
    pub analyzed: bool,
    /// Summary lines such as planning/execution time.
    pub notes: Vec<String>,
}

impl ExplainPlan {
    /// Largest cost of any node (for relative cost bars).
    pub fn max_cost(&self) -> f64 {
        self.roots
            .iter()
            .map(PlanNode::max_cost)
            .fold(0.0, f64::max)
    }
}

fn parse_error(format: &str, e: impl std::fmt::Display) -> DbError {
    DbError::new(
        DbErrorKind::Internal,
        format!("could not read the {format} plan: {e}"),
    )
}

fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(items) if items.iter().all(|i| !i.is_object() && !i.is_array()) => Some(
            items
                .iter()
                .filter_map(scalar_text)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        _ => None,
    }
}

/// PostgreSQL `EXPLAIN (FORMAT JSON …)` output.
pub fn parse_postgres_json(text: &str) -> Result<ExplainPlan, DbError> {
    let doc: Value = serde_json::from_str(text).map_err(|e| parse_error("PostgreSQL", e))?;
    let entries = doc.as_array().cloned().unwrap_or_else(|| vec![doc.clone()]);
    let mut plan = ExplainPlan {
        raw: serde_json::to_string_pretty(&doc).unwrap_or_else(|_| text.to_owned()),
        ..ExplainPlan::default()
    };
    for entry in &entries {
        if let Some(node) = entry.get("Plan") {
            plan.roots.push(pg_node(node));
        }
        for (key, label) in [
            ("Planning Time", "Planning time"),
            ("Execution Time", "Execution time"),
        ] {
            if let Some(ms) = entry.get(key).and_then(Value::as_f64) {
                plan.notes.push(format!("{label}: {ms:.3} ms"));
                plan.analyzed |= key == "Execution Time";
            }
        }
    }
    if plan.roots.is_empty() {
        return Err(parse_error("PostgreSQL", "no \"Plan\" entry"));
    }
    Ok(plan)
}

fn pg_node(v: &Value) -> PlanNode {
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let num = |k: &str| v.get(k).and_then(Value::as_f64);
    let mut operation = text("Node Type").unwrap_or_else(|| "?".into());
    if let Some(join) = text("Join Type")
        && (operation.contains("Join") || operation == "Nested Loop")
    {
        operation = format!("{operation} ({join})");
    }
    let relation = text("Relation Name").map(|r| match (text("Schema"), text("Alias")) {
        (Some(s), Some(a)) if a != r => format!("{s}.{r} {a}"),
        (Some(s), _) => format!("{s}.{r}"),
        (None, Some(a)) if a != r => format!("{r} {a}"),
        _ => r,
    });
    let object = match (relation, text("Index Name")) {
        (Some(r), Some(i)) => Some(format!("{r} using {i}")),
        (r, i) => r
            .or(i)
            .or_else(|| text("CTE Name"))
            .or_else(|| text("Function Name")),
    };
    let loops = num("Actual Loops").unwrap_or(1.0);
    const SHOWN: &[&str] = &[
        "Node Type",
        "Join Type",
        "Relation Name",
        "Schema",
        "Alias",
        "Index Name",
        "Total Cost",
        "Plan Rows",
        "Actual Rows",
        "Actual Total Time",
        "Actual Loops",
        "Plans",
        "Startup Cost",
        "Actual Startup Time",
        "Plan Width",
        "Parallel Aware",
        "Async Capable",
        "Parent Relationship",
    ];
    let details = v
        .as_object()
        .map(|o| {
            o.iter()
                .filter(|(k, _)| !SHOWN.contains(&k.as_str()))
                .filter_map(|(k, val)| scalar_text(val).map(|s| (k.clone(), s)))
                .collect()
        })
        .unwrap_or_default();
    PlanNode {
        operation,
        object,
        cost: num("Total Cost"),
        rows: num("Plan Rows"),
        actual_rows: num("Actual Rows").map(|r| r * loops),
        actual_ms: num("Actual Total Time").map(|t| t * loops),
        details,
        children: v
            .get("Plans")
            .and_then(Value::as_array)
            .map(|plans| plans.iter().map(pg_node).collect())
            .unwrap_or_default(),
    }
}

/// Value of `key=` inside a parenthesised group such as `(cost=1.25 rows=3)`.
fn field(group: &str, key: &str) -> Option<f64> {
    let start = group.find(&format!("{key}="))? + key.len() + 1;
    let rest = &group[start..];
    let end = rest.find([' ', ')']).unwrap_or(rest.len());
    let value = &rest[..end];
    // `actual time=0.05..0.07` → the second number is the total.
    value.rsplit("..").next()?.parse().ok()
}

/// MySQL `EXPLAIN FORMAT=TREE` / `EXPLAIN ANALYZE` output.
pub fn parse_mysql_tree(text: &str) -> Result<ExplainPlan, DbError> {
    // (indent, node) stack; children are attached when a shallower line appears.
    let mut stack: Vec<(usize, PlanNode)> = Vec::new();
    let mut roots = Vec::new();
    let mut analyzed = false;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let Some(body) = line.trim_start().strip_prefix("-> ") else {
            continue;
        };
        let (operation, groups) = match body.find("  (") {
            Some(i) => (body[..i].trim(), &body[i..]),
            None => (body.trim(), ""),
        };
        let actual = groups.find("(actual").map(|i| &groups[i..]);
        let estimate = groups.find("(cost=").map(|i| &groups[i..]);
        let mut node = PlanNode::new(operation);
        node.cost = estimate.and_then(|g| field(g, "cost"));
        node.rows = estimate.and_then(|g| field(g, "rows"));
        if let Some(a) = actual {
            analyzed = true;
            let loops = field(a, "loops").unwrap_or(1.0);
            node.actual_rows = field(a, "rows").map(|r| r * loops);
            node.actual_ms = field(a, "time").map(|t| t * loops);
        }
        node.object = mysql_object(operation);
        while stack.last().is_some_and(|(top, _)| *top >= indent) {
            if let Some((_, done)) = stack.pop() {
                attach(&mut stack, &mut roots, done);
            }
        }
        stack.push((indent, node));
    }
    while let Some((_, done)) = stack.pop() {
        attach(&mut stack, &mut roots, done);
    }
    if roots.is_empty() {
        return Err(parse_error("MySQL", "no plan lines"));
    }
    Ok(ExplainPlan {
        roots,
        raw: text.to_owned(),
        analyzed,
        notes: Vec::new(),
    })
}

fn attach(stack: &mut [(usize, PlanNode)], roots: &mut Vec<PlanNode>, node: PlanNode) {
    match stack.last_mut() {
        Some((_, parent)) => parent.children.push(node),
        None => roots.push(node),
    }
}

/// `Table scan on c` / `Index lookup on o using idx (…)` → the table (and index).
fn mysql_object(operation: &str) -> Option<String> {
    let rest = operation.split(" on ").nth(1)?;
    let mut words = rest.split_whitespace();
    let table = words.next()?.to_owned();
    let index = rest
        .split(" using ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next());
    Some(match index {
        Some(i) => format!("{table} using {i}"),
        None => table,
    })
}

/// Any JSON plan (MariaDB `EXPLAIN FORMAT=JSON`): objects become nodes, scalars details.
pub fn parse_generic_json(text: &str) -> Result<ExplainPlan, DbError> {
    let doc: Value = serde_json::from_str(text).map_err(|e| parse_error("JSON", e))?;
    let root = json_node("query", &doc);
    Ok(ExplainPlan {
        roots: vec![root],
        raw: serde_json::to_string_pretty(&doc).unwrap_or_else(|_| text.to_owned()),
        analyzed: text.contains("\"r_rows\"") || text.contains("\"r_total_time_ms\""),
        notes: Vec::new(),
    })
}

fn json_node(name: &str, v: &Value) -> PlanNode {
    let mut node = PlanNode::new(name);
    let Some(obj) = v.as_object() else {
        if let Some(items) = v.as_array() {
            node.children = items.iter().map(|i| json_node(name, i)).collect();
        }
        return node;
    };
    let num = |k: &str| obj.get(k).and_then(Value::as_f64);
    if let Some(table) = obj.get("table_name").and_then(Value::as_str) {
        let access = obj.get("access_type").and_then(Value::as_str).unwrap_or("");
        node.operation = format!("{name} {access}").trim().to_owned();
        node.object = Some(match obj.get("key").and_then(Value::as_str) {
            Some(key) => format!("{table} using {key}"),
            None => table.to_owned(),
        });
    }
    node.rows = num("rows");
    node.cost = num("cost").or_else(|| num("query_cost"));
    node.actual_rows = num("r_rows");
    node.actual_ms = num("r_total_time_ms");
    for (key, val) in obj {
        match val {
            Value::Object(_) => node.children.push(json_node(key, val)),
            Value::Array(items) if items.iter().any(Value::is_object) => {
                // `[{"table": {...}}, …]`: a single-key wrapper names its content.
                node.children.extend(items.iter().map(|item| {
                    match item
                        .as_object()
                        .filter(|o| o.len() == 1)
                        .and_then(|o| o.iter().next())
                    {
                        Some((inner_key, inner)) => json_node(inner_key, inner),
                        None => json_node(key, item),
                    }
                }));
            }
            other => {
                if let Some(text) = scalar_text(other)
                    && !matches!(key.as_str(), "table_name" | "access_type" | "key")
                {
                    node.details.push((key.clone(), text));
                }
            }
        }
    }
    node
}

/// SQLite `EXPLAIN QUERY PLAN` rows: `(id, parent, detail)`.
pub fn from_parent_rows(rows: &[(i64, i64, String)], raw: String) -> ExplainPlan {
    fn build(rows: &[(i64, i64, String)], parent: i64) -> Vec<PlanNode> {
        rows.iter()
            .filter(|(id, p, _)| *p == parent && *id != parent)
            .map(|(id, _, detail)| {
                let mut node = PlanNode::new(detail.clone());
                node.object = sqlite_object(detail);
                node.children = build(rows, *id);
                node
            })
            .collect()
    }
    ExplainPlan {
        roots: build(rows, 0),
        raw,
        analyzed: false,
        notes: Vec::new(),
    }
}

/// `SEARCH t USING INDEX ix (a=?)` → `t using ix`.
fn sqlite_object(detail: &str) -> Option<String> {
    let mut words = detail.split_whitespace();
    let verb = words.next()?;
    if !matches!(verb, "SCAN" | "SEARCH") {
        return None;
    }
    let table = words.next()?;
    let index = detail
        .split(" INDEX ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next());
    Some(match index {
        Some(i) => format!("{table} using {i}"),
        None => table.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_json_tree() {
        let json = r#"[{"Plan": {"Node Type": "Hash Join", "Join Type": "Inner", "Total Cost": 42.5,
            "Plan Rows": 10, "Actual Rows": 3, "Actual Loops": 2, "Actual Total Time": 1.5,
            "Hash Cond": "(o.customer_id = c.id)",
            "Plans": [
              {"Node Type": "Seq Scan", "Relation Name": "orders", "Schema": "public", "Alias": "o",
               "Total Cost": 20.0, "Plan Rows": 100},
              {"Node Type": "Index Scan", "Relation Name": "customers", "Alias": "customers",
               "Index Name": "customers_pkey", "Total Cost": 8.1, "Plan Rows": 1}
            ]}, "Planning Time": 0.1, "Execution Time": 2.25}]"#;
        let plan = parse_postgres_json(json).unwrap();
        assert!(plan.analyzed);
        assert_eq!(
            plan.notes,
            ["Planning time: 0.100 ms", "Execution time: 2.250 ms"]
        );
        let root = &plan.roots[0];
        assert_eq!(root.operation, "Hash Join (Inner)");
        assert_eq!(root.actual_rows, Some(6.0));
        assert_eq!(root.actual_ms, Some(3.0));
        assert_eq!(
            root.details,
            [("Hash Cond".to_owned(), "(o.customer_id = c.id)".to_owned())]
        );
        assert_eq!(root.children[0].object.as_deref(), Some("public.orders o"));
        assert_eq!(
            root.children[1].object.as_deref(),
            Some("customers using customers_pkey")
        );
        assert_eq!(plan.max_cost(), 42.5);
        assert_eq!(root.len(), 3);
    }

    #[test]
    fn mysql_tree_with_actuals() {
        let text = "-> Nested loop inner join  (cost=1.10 rows=2) (actual time=0.05..0.07 rows=2 loops=1)\n    -> Table scan on c  (cost=0.45 rows=2) (actual time=0.02..0.03 rows=2 loops=1)\n    -> Index lookup on o using fk_o (customer_id=c.id)  (cost=0.30 rows=1) (actual time=0.01..0.01 rows=1 loops=2)\n        -> Filter: (o.x > 1)  (cost=0.1 rows=1)\n";
        let plan = parse_mysql_tree(text).unwrap();
        assert!(plan.analyzed);
        assert_eq!(plan.roots.len(), 1);
        let root = &plan.roots[0];
        assert_eq!(root.operation, "Nested loop inner join");
        assert_eq!(root.cost, Some(1.10));
        assert_eq!(root.actual_ms, Some(0.07));
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].object.as_deref(), Some("c"));
        let lookup = &root.children[1];
        assert_eq!(lookup.object.as_deref(), Some("o using fk_o"));
        assert_eq!(lookup.actual_rows, Some(2.0));
        assert_eq!(lookup.children[0].operation, "Filter: (o.x > 1)");
    }

    #[test]
    fn generic_json_builds_nodes() {
        let json = r#"{"query_block": {"select_id": 1, "cost": 4.5,
            "nested_loop": [{"table": {"table_name": "t", "access_type": "ALL", "rows": 10}},
                            {"table": {"table_name": "u", "access_type": "ref", "key": "ix", "rows": 1}}]}}"#;
        let plan = parse_generic_json(json).unwrap();
        let block = &plan.roots[0].children[0];
        assert_eq!(block.operation, "query_block");
        assert_eq!(block.cost, Some(4.5));
        assert_eq!(block.children.len(), 2);
        assert_eq!(block.children[1].object.as_deref(), Some("u using ix"));
        assert_eq!(block.children[1].operation, "table ref");
    }

    #[test]
    fn sqlite_rows_become_a_tree() {
        let rows = vec![
            (2, 0, "SCAN o".to_owned()),
            (
                5,
                0,
                "SEARCH c USING INTEGER PRIMARY KEY (rowid=?)".to_owned(),
            ),
            (9, 5, "USE TEMP B-TREE FOR ORDER BY".to_owned()),
        ];
        let plan = from_parent_rows(&rows, String::new());
        assert_eq!(plan.roots.len(), 2);
        assert_eq!(plan.roots[0].object.as_deref(), Some("o"));
        assert_eq!(plan.roots[1].object.as_deref(), Some("c"));
        assert_eq!(
            plan.roots[1].children[0].operation,
            "USE TEMP B-TREE FOR ORDER BY"
        );
    }
}
