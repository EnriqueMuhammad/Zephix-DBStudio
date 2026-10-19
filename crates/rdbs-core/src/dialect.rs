//! Engine-specific SQL text generation (identifier quoting, preview queries).

use crate::profile::DatabaseKind;

/// Quotes an identifier for `kind`, escaping embedded quote characters.
pub fn quote_identifier(kind: DatabaseKind, name: &str) -> String {
    match kind {
        DatabaseKind::SqlServer => format!("[{}]", name.replace(']', "]]")),
        DatabaseKind::MySql | DatabaseKind::MariaDb => format!("`{}`", name.replace('`', "``")),
        DatabaseKind::PostgreSql | DatabaseKind::Sqlite | DatabaseKind::Oracle => {
            format!("\"{}\"", name.replace('"', "\"\""))
        }
        // Document and key-value stores use names as they are.
        DatabaseKind::MongoDb | DatabaseKind::Redis => name.to_owned(),
    }
}

/// Fully qualified, quoted object name. `database` is only used where cross-database
/// references are idiomatic (SQL Server, MySQL/MariaDB).
pub fn qualified_name(
    kind: DatabaseKind,
    database: Option<&str>,
    schema: Option<&str>,
    object: &str,
) -> String {
    let parts: Vec<&str> = match kind {
        DatabaseKind::SqlServer => [database, schema].into_iter().flatten().collect(),
        DatabaseKind::MySql | DatabaseKind::MariaDb => database.into_iter().collect(),
        DatabaseKind::PostgreSql | DatabaseKind::Sqlite | DatabaseKind::Oracle => {
            schema.into_iter().collect()
        }
        DatabaseKind::MongoDb | DatabaseKind::Redis => Vec::new(),
    };
    parts
        .into_iter()
        .chain(std::iter::once(object))
        .map(|p| quote_identifier(kind, p))
        .collect::<Vec<_>>()
        .join(".")
}

/// `SELECT` returning the first `limit` rows of an object, in the engine's dialect.
pub fn select_preview(kind: DatabaseKind, qualified: &str, limit: u32) -> String {
    match kind {
        DatabaseKind::SqlServer => format!("SELECT TOP {limit} *\nFROM {qualified};"),
        DatabaseKind::Oracle => {
            format!("SELECT *\nFROM {qualified}\nFETCH FIRST {limit} ROWS ONLY")
        }
        DatabaseKind::MongoDb => {
            format!("db.getCollection(\"{qualified}\").find({{}}).limit({limit})")
        }
        DatabaseKind::Redis => format!(
            "INSPECT \"{}\"",
            qualified.replace('\\', "\\\\").replace('"', "\\\"")
        ),
        _ => format!("SELECT *\nFROM {qualified}\nLIMIT {limit};"),
    }
}

/// `SELECT` for a table data view: optional `WHERE` condition and `ORDER BY` list (both as
/// typed, without the keywords) and a row limit (`None` = all rows). SQL engines only.
pub fn select_rows(
    kind: DatabaseKind,
    qualified: &str,
    filter: &str,
    order: &str,
    limit: Option<u32>,
) -> String {
    let filter = strip_keyword(filter.trim(), "WHERE");
    let order = strip_keyword(order.trim(), "ORDER BY");
    let top = match (kind, limit) {
        (DatabaseKind::SqlServer, Some(n)) => format!("TOP {n} "),
        _ => String::new(),
    };
    let mut sql = format!(
        "SELECT {top}*
FROM {qualified}"
    );
    if !filter.is_empty() {
        sql.push_str(&format!(
            "
WHERE {filter}"
        ));
    }
    if !order.is_empty() {
        sql.push_str(&format!(
            "
ORDER BY {order}"
        ));
    }
    match (kind, limit) {
        (DatabaseKind::SqlServer, _) | (_, None) => {}
        (DatabaseKind::Oracle, Some(n)) => sql.push_str(&format!(
            "
FETCH FIRST {n} ROWS ONLY"
        )),
        (_, Some(n)) => sql.push_str(&format!(
            "
LIMIT {n}"
        )),
    }
    if kind != DatabaseKind::Oracle {
        sql.push(';');
    }
    sql
}

/// `text` without a leading `keyword` (case-insensitive), so "WHERE a = 1" and "a = 1" mean
/// the same in the filter box.
fn strip_keyword<'a>(text: &'a str, keyword: &str) -> &'a str {
    let words: Vec<&str> = keyword.split(' ').collect();
    let mut rest = text;
    for w in words {
        let candidate = rest.trim_start();
        if candidate.len() >= w.len()
            && candidate[..w.len()].eq_ignore_ascii_case(w)
            && candidate[w.len()..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace)
        {
            rest = &candidate[w.len()..];
        } else {
            return text;
        }
    }
    rest.trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_view_selects() {
        assert_eq!(
            select_rows(DatabaseKind::SqlServer, "[dbo].[t]", "", "", Some(100)),
            "SELECT TOP 100 *
FROM [dbo].[t];"
        );
        assert_eq!(
            select_rows(
                DatabaseKind::PostgreSql,
                "\"t\"",
                "WHERE id > 5",
                "order by name desc",
                Some(1000)
            ),
            "SELECT *
FROM \"t\"
WHERE id > 5
ORDER BY name desc
LIMIT 1000;"
        );
        assert_eq!(
            select_rows(DatabaseKind::Oracle, "\"T\"", "x = 1", "", Some(10)),
            "SELECT *
FROM \"T\"
WHERE x = 1
FETCH FIRST 10 ROWS ONLY"
        );
        assert_eq!(
            select_rows(DatabaseKind::MySql, "`t`", "whereabouts = 1", "", None),
            "SELECT *
FROM `t`
WHERE whereabouts = 1;"
        );
    }

    #[test]
    fn quoting_escapes_delimiters() {
        assert_eq!(quote_identifier(DatabaseKind::SqlServer, "a]b"), "[a]]b]");
        assert_eq!(quote_identifier(DatabaseKind::MySql, "a`b"), "`a``b`");
        assert_eq!(
            quote_identifier(DatabaseKind::PostgreSql, "a\"b"),
            "\"a\"\"b\""
        );
    }

    #[test]
    fn qualified_names_follow_engine_conventions() {
        let q = |k| qualified_name(k, Some("db"), Some("s"), "t");
        assert_eq!(q(DatabaseKind::SqlServer), "[db].[s].[t]");
        assert_eq!(q(DatabaseKind::PostgreSql), "\"s\".\"t\"");
        assert_eq!(q(DatabaseKind::MySql), "`db`.`t`");
    }

    #[test]
    fn preview_uses_top_or_limit() {
        assert!(select_preview(DatabaseKind::SqlServer, "[t]", 5).starts_with("SELECT TOP 5"));
        assert!(select_preview(DatabaseKind::PostgreSql, "\"t\"", 5).ends_with("LIMIT 5;"));
    }
}
