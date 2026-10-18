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

#[cfg(test)]
mod tests {
    use super::*;

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
