//! T-SQL script handling: `GO` batch separators and DML detection.

use rdbs_core::sql::{self, TokenKind};

/// Splits a script into batches on lines consisting of `GO` (optionally `GO n`), as SSMS and
/// `sqlcmd` do. Blank batches are dropped. A repeat count is ignored.
pub fn split_batches(script: &str) -> Vec<&str> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    for line in script.split_inclusive('\n') {
        let trimmed = line.trim();
        let is_go = trimmed
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("go"))
            && trimmed[2..].trim().chars().all(|c| c.is_ascii_digit());
        if is_go {
            batches.push(&script[start..offset]);
            start = offset + line.len();
        }
        offset += line.len();
    }
    batches.push(&script[start..]);
    batches.retain(|b| sql::classify(b) != sql::StatementKind::Empty);
    batches
}

/// Whether a batch is a single plain DML statement, for which the affected row count is
/// reported (it runs through `sp_executesql`, which returns counts but no result sets).
pub fn is_plain_dml(batch: &str) -> bool {
    let statements = sql::split_statements(batch);
    let [range] = statements.as_slice() else {
        return false;
    };
    let stmt = &batch[range.clone()];
    let first = sql::tokenize(stmt)
        .into_iter()
        .find(|t| matches!(t.kind, TokenKind::Keyword | TokenKind::Identifier))
        .map(|t| stmt[t.range].to_ascii_uppercase());
    let has_output = sql::tokenize(stmt)
        .iter()
        .any(|t| stmt[t.range.clone()].eq_ignore_ascii_case("OUTPUT"));
    matches!(
        first.as_deref(),
        Some("INSERT" | "UPDATE" | "DELETE" | "MERGE")
    ) && !has_output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_go_lines_only() {
        let script = "select 1\nGO\nselect 'go'\n  go 2  \n\nselect 3";
        assert_eq!(
            split_batches(script),
            vec!["select 1\n", "select 'go'\n", "\nselect 3"]
        );
    }

    #[test]
    fn ignores_identifiers_starting_with_go() {
        assert_eq!(
            split_batches("goto_label:\nselect 1"),
            vec!["goto_label:\nselect 1"]
        );
        assert_eq!(split_batches("é\nselect 1"), vec!["é\nselect 1"]);
        assert!(split_batches("GO\n  \nGO").is_empty());
    }

    #[test]
    fn detects_plain_dml() {
        assert!(is_plain_dml("UPDATE t SET a = 1 WHERE id = 2;"));
        assert!(!is_plain_dml("UPDATE t SET a = 1 OUTPUT inserted.*"));
        assert!(!is_plain_dml("SELECT 1"));
        assert!(!is_plain_dml("UPDATE t SET a = 1; SELECT @@ROWCOUNT"));
        assert!(!is_plain_dml("EXEC usp_do_things"));
    }
}
