//! SQL Server XML showplans (`SET SHOWPLAN_XML` / `SET STATISTICS XML`) → [`ExplainPlan`].

use rdbs_core::plan::{ExplainPlan, PlanNode};
use rdbs_core::{DbError, DbErrorKind};
use roxmltree::{Document, Node};

fn num(node: Node<'_, '_>, attr: &str) -> Option<f64> {
    node.attribute(attr)?.parse().ok()
}

/// `RelOp` elements directly below `node` (not inside a nested `RelOp`).
fn nested_relops<'a, 'input>(node: Node<'a, 'input>) -> Vec<Node<'a, 'input>> {
    let mut out = Vec::new();
    for child in node.children().filter(Node::is_element) {
        if child.has_tag_name("RelOp") {
            out.push(child);
        } else {
            out.extend(nested_relops(child));
        }
    }
    out
}

/// First element named `tag` below `node` that is not inside a nested `RelOp`.
fn own_descendant<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Option<Node<'a, 'input>> {
    for child in node.children().filter(Node::is_element) {
        if child.has_tag_name("RelOp") {
            continue;
        }
        if child.has_tag_name(tag) {
            return Some(child);
        }
        if let Some(found) = own_descendant(child, tag) {
            return Some(found);
        }
    }
    None
}

fn strip_brackets(s: &str) -> String {
    s.trim_start_matches('[').trim_end_matches(']').to_owned()
}

fn relop(node: Node<'_, '_>) -> PlanNode {
    let physical = node.attribute("PhysicalOp").unwrap_or("?");
    let logical = node.attribute("LogicalOp").unwrap_or(physical);
    let operation = if physical == logical {
        physical.to_owned()
    } else {
        format!("{physical} ({logical})")
    };
    let object = own_descendant(node, "Object").map(|o| {
        let table = [o.attribute("Schema"), o.attribute("Table")]
            .into_iter()
            .flatten()
            .map(strip_brackets)
            .collect::<Vec<_>>()
            .join(".");
        match o.attribute("Index") {
            Some(index) => format!("{table} using {}", strip_brackets(index)),
            None => table,
        }
    });
    let mut details = Vec::new();
    for tag in ["SeekPredicates", "Predicate"] {
        if let Some(text) = own_descendant(node, tag)
            .and_then(|p| own_descendant(p, "ScalarOperator"))
            .and_then(|s| s.attribute("ScalarString"))
        {
            details.push((tag.to_owned(), text.to_owned()));
        }
    }
    if let Some(parallel) = node
        .attribute("Parallel")
        .filter(|p| *p == "true" || *p == "1")
    {
        details.push(("Parallel".to_owned(), parallel.to_owned()));
    }
    // Actual figures: sum rows over threads, take the slowest thread's time.
    let counters: Vec<Node<'_, '_>> = own_descendant(node, "RunTimeInformation")
        .map(|r| {
            r.children()
                .filter(|c| c.has_tag_name("RunTimeCountersPerThread"))
                .collect()
        })
        .unwrap_or_default();
    let actual_rows =
        (!counters.is_empty()).then(|| counters.iter().filter_map(|c| num(*c, "ActualRows")).sum());
    let actual_ms = counters
        .iter()
        .filter_map(|c| num(*c, "ActualElapsedms"))
        .reduce(f64::max);
    PlanNode {
        operation,
        object,
        cost: num(node, "EstimatedTotalSubtreeCost"),
        rows: num(node, "EstimateRows"),
        actual_rows,
        actual_ms,
        details,
        children: nested_relops(node).into_iter().map(relop).collect(),
    }
}

/// Parses one showplan document per statement batch.
pub fn parse(documents: &[String], analyzed: bool) -> Result<ExplainPlan, DbError> {
    let mut plan = ExplainPlan {
        analyzed,
        raw: documents.join("\n\n"),
        ..ExplainPlan::default()
    };
    for xml in documents {
        let doc = Document::parse(xml).map_err(|e| {
            DbError::new(
                DbErrorKind::Internal,
                format!("could not read the SQL Server plan: {e}"),
            )
        })?;
        for stmt in doc.descendants().filter(|n| n.has_tag_name("StmtSimple")) {
            let mut root = PlanNode::new(stmt.attribute("StatementType").unwrap_or("STATEMENT"));
            root.cost = num(stmt, "StatementSubTreeCost");
            root.rows = num(stmt, "StatementEstRows");
            if let Some(text) = stmt.attribute("StatementText") {
                root.details
                    .push(("Statement".to_owned(), text.trim().to_owned()));
            }
            root.children = nested_relops(stmt).into_iter().map(relop).collect();
            plan.roots.push(root);
        }
    }
    if plan.roots.is_empty() {
        return Err(DbError::new(
            DbErrorKind::Query,
            "SQL Server returned no plan for this statement",
        ));
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<ShowPlanXML xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan" Version="1.6">
<BatchSequence><Batch><Statements>
<StmtSimple StatementText="SELECT * FROM t WHERE id = 1" StatementType="SELECT" StatementSubTreeCost="0.0032" StatementEstRows="1">
<QueryPlan>
<RelOp NodeId="0" PhysicalOp="Nested Loops" LogicalOp="Inner Join" EstimateRows="1" EstimatedTotalSubtreeCost="0.0032">
  <OutputList/>
  <RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1" ActualElapsedms="2"/></RunTimeInformation>
  <NestedLoops Optimized="0">
    <RelOp NodeId="1" PhysicalOp="Clustered Index Seek" LogicalOp="Clustered Index Seek" EstimateRows="1" EstimatedTotalSubtreeCost="0.0016">
      <IndexScan>
        <Object Database="[db]" Schema="[dbo]" Table="[t]" Index="[PK_t]"/>
        <SeekPredicates><SeekPredicateNew><SeekKeys><Prefix ScanType="EQ">
          <RangeColumns/><RangeExpressions><ScalarOperator ScalarString="(1)"/></RangeExpressions>
        </Prefix></SeekKeys></SeekPredicateNew></SeekPredicates>
      </IndexScan>
    </RelOp>
    <RelOp NodeId="2" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="5" EstimatedTotalSubtreeCost="0.001">
      <TableScan><Predicate><ScalarOperator ScalarString="[u].[x]&gt;(1)"/></Predicate>
        <Object Schema="[dbo]" Table="[u]"/></TableScan>
    </RelOp>
  </NestedLoops>
</RelOp>
</QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;

    #[test]
    fn parses_operators_objects_and_actuals() {
        let plan = parse(&[XML.to_owned()], true).unwrap();
        let stmt = &plan.roots[0];
        assert_eq!(stmt.operation, "SELECT");
        assert_eq!(stmt.cost, Some(0.0032));
        let join = &stmt.children[0];
        assert_eq!(join.operation, "Nested Loops (Inner Join)");
        assert_eq!(join.actual_rows, Some(1.0));
        assert_eq!(join.actual_ms, Some(2.0));
        assert_eq!(
            join.object, None,
            "objects of children are not attributed to the join"
        );
        assert_eq!(join.children.len(), 2);
        let seek = &join.children[0];
        assert_eq!(seek.object.as_deref(), Some("dbo.t using PK_t"));
        assert_eq!(
            seek.details[0],
            ("SeekPredicates".to_owned(), "(1)".to_owned())
        );
        let scan = &join.children[1];
        assert_eq!(scan.object.as_deref(), Some("dbo.u"));
        assert_eq!(
            scan.details[0],
            ("Predicate".to_owned(), "[u].[x]>(1)".to_owned())
        );
    }

    #[test]
    fn no_statements_is_an_error() {
        assert!(parse(&[], false).is_err());
    }
}
