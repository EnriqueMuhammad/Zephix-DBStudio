//! T-SQL script handling: `GO` batch separators.

use rdbs_core::sql;

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
}
