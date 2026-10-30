//! Context-aware SQL autocompletion.
//!
//! Pure function of (script, cursor, catalog): the UI only renders the popup and applies the
//! chosen item. Context comes from the lexer, so strings and comments never trigger it.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use crate::dialect::quote_identifier;
use crate::profile::DatabaseKind;
use crate::schema::{DbObject, ObjectKind};
use crate::sql::{self, KEYWORDS, Token, TokenKind};

/// A table or view known to the completer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    /// Schema (PostgreSQL/SQL Server) or database (MySQL) containing it.
    pub schema: Option<String>,
    /// Object name.
    pub name: String,
    /// Column names in ordinal order.
    pub columns: Vec<String>,
    /// Whether it is a view.
    pub is_view: bool,
}

/// Names available for completion, derived from the explorer tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    /// Schema (or MySQL database) names.
    pub schemas: Vec<String>,
    /// Tables and views.
    pub tables: Vec<TableInfo>,
}

impl Catalog {
    /// Collects schemas, tables, views and columns from an object tree. For engines without
    /// schemas (MySQL) the database plays the role of the schema.
    pub fn from_objects(objects: &[DbObject]) -> Self {
        let mut catalog = Catalog::default();
        let mut schemas = BTreeSet::new();
        fn walk(
            obj: &DbObject,
            schema: Option<&str>,
            out: &mut Catalog,
            schemas: &mut BTreeSet<String>,
        ) {
            match obj.kind {
                ObjectKind::Database | ObjectKind::Schema => {
                    let has_schema_children =
                        obj.children.iter().any(|c| c.kind == ObjectKind::Schema);
                    if !has_schema_children && !obj.children.is_empty() {
                        schemas.insert(obj.name.clone());
                    }
                    for child in &obj.children {
                        walk(child, Some(&obj.name), out, schemas);
                    }
                }
                ObjectKind::Folder => {
                    for child in &obj.children {
                        walk(child, schema, out, schemas);
                    }
                }
                ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView => {
                    out.tables.push(TableInfo {
                        schema: schema.map(str::to_owned),
                        name: obj.name.clone(),
                        columns: obj
                            .children
                            .iter()
                            .filter(|c| c.kind == ObjectKind::Column)
                            .map(|c| c.name.clone())
                            .collect(),
                        is_view: obj.kind != ObjectKind::Table,
                    });
                }
                _ => {}
            }
        }
        for obj in objects {
            walk(obj, None, &mut catalog, &mut schemas);
        }
        catalog.schemas = schemas.into_iter().collect();
        catalog
    }

    fn table(&self, name: &str) -> Option<&TableInfo> {
        self.tables.iter().find(|t| t.name == name).or_else(|| {
            self.tables
                .iter()
                .find(|t| t.name.eq_ignore_ascii_case(name))
        })
    }
}

/// Category of a suggestion (drives the icon and ranking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemKind {
    /// Multi-line template, such as a transaction block.
    Snippet,
    /// Column of a referenced table.
    Column,
    /// Table.
    Table,
    /// View.
    View,
    /// Schema or database.
    Schema,
    /// SQL keyword.
    Keyword,
}

/// One suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Text shown in the list.
    pub label: String,
    /// Category.
    pub kind: ItemKind,
    /// Secondary text (owning table or schema).
    pub detail: Option<String>,
    /// Text inserted in place of the typed prefix (quoted when necessary).
    pub insert: String,
    /// Byte offset in `insert` where the caret goes; `None` puts it after the insertion.
    pub caret: Option<usize>,
}

/// Suggestions for a cursor position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Byte range of the typed prefix that an accepted item replaces.
    pub replace: Range<usize>,
    /// Ranked suggestions (never empty).
    pub items: Vec<Item>,
}

/// Maximum number of suggestions returned.
pub const MAX_ITEMS: usize = 50;

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Unquotes `"x"`, `[x]` or `` `x` ``.
fn unquote(name: &str) -> &str {
    let n = name.trim();
    for (open, close) in [('"', '"'), ('[', ']'), ('`', '`')] {
        if n.len() >= 2 && n.starts_with(open) && n.ends_with(close) {
            return &n[1..n.len() - 1];
        }
    }
    n
}

/// Whether `name` can be written without quotes in `kind`.
fn needs_quotes(name: &str, kind: DatabaseKind) -> bool {
    let mut chars = name.chars();
    let starts_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let rest_ok = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    // PostgreSQL folds unquoted identifiers to lower case.
    let case_ok = kind != DatabaseKind::PostgreSql || !name.chars().any(|c| c.is_ascii_uppercase());
    !(starts_ok && rest_ok && case_ok) || sql::is_keyword(name)
}

fn ident(name: &str, kind: DatabaseKind) -> String {
    if needs_quotes(name, kind) {
        quote_identifier(kind, name)
    } else {
        name.to_owned()
    }
}

/// Significant tokens (no whitespace/comments) of the statement containing `cursor`.
fn statement_tokens(sql_text: &str, cursor: usize) -> (Range<usize>, Vec<Token>) {
    let range = sql::split_statements(sql_text)
        .into_iter()
        .rfind(|r| r.start <= cursor)
        .map_or(0..sql_text.len(), |r| {
            r.start..sql_text.len().min(r.end.max(cursor))
        });
    let stmt = &sql_text[range.clone()];
    let tokens = sql::tokenize(stmt)
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment))
        .map(|t| Token {
            kind: t.kind,
            range: t.range.start + range.start..t.range.end + range.start,
        })
        .collect();
    (range, tokens)
}

/// Tables referenced after FROM/JOIN/UPDATE/INTO, with their aliases (alias → table name).
fn referenced_tables(sql_text: &str, tokens: &[Token]) -> (Vec<String>, HashMap<String, String>) {
    let text = |t: &Token| unquote(&sql_text[t.range.clone()]).to_owned();
    let upper = |t: &Token| sql_text[t.range.clone()].to_ascii_uppercase();
    let is_name = |t: &Token| matches!(t.kind, TokenKind::Identifier | TokenKind::QuotedIdentifier);
    let mut tables = Vec::new();
    let mut aliases = HashMap::new();
    let mut i = 0;
    while i < tokens.len() {
        if matches!(
            upper(&tokens[i]).as_str(),
            "FROM" | "JOIN" | "UPDATE" | "INTO"
        ) {
            i += 1;
            // qualified name: a.b.c → last part is the table
            while let Some(first) = tokens.get(i).filter(|t| is_name(t)) {
                let mut table = text(first);
                i += 1;
                while tokens
                    .get(i)
                    .is_some_and(|t| &sql_text[t.range.clone()] == ".")
                    && tokens.get(i + 1).is_some_and(is_name)
                {
                    table = text(&tokens[i + 1]);
                    i += 2;
                }
                if tokens.get(i).is_some_and(|t| upper(t) == "AS") {
                    i += 1;
                }
                if let Some(alias) = tokens.get(i).filter(|t| is_name(t)) {
                    aliases.insert(text(alias).to_lowercase(), table.clone());
                    i += 1;
                }
                tables.push(table);
                if tokens
                    .get(i)
                    .is_some_and(|t| &sql_text[t.range.clone()] == ",")
                {
                    i += 1;
                    continue;
                }
                break;
            }
        } else {
            i += 1;
        }
    }
    (tables, aliases)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    Tables,
    Columns,
    General,
}

/// Transaction block template offered for `begin`: the work goes on the empty line, and the
/// script ends in a rollback until the commit is uncommented. Statements are separated by `;`
/// except on SQL Server, where a batch needs none.
fn transaction_template(kind: DatabaseKind, lower: bool) -> Option<Item> {
    let (begin, end) = match kind {
        DatabaseKind::SqlServer => ("BEGIN TRANSACTION", ""),
        DatabaseKind::PostgreSql | DatabaseKind::Sqlite => ("BEGIN TRANSACTION", ";"),
        DatabaseKind::MySql | DatabaseKind::MariaDb => ("START TRANSACTION", ";"),
        DatabaseKind::Oracle | DatabaseKind::MongoDb | DatabaseKind::Redis => return None,
    };
    let case = |s: &str| {
        if lower {
            s.to_lowercase()
        } else {
            s.to_owned()
        }
    };
    let head = format!(
        "{}{end}
--Query
",
        case(begin)
    );
    let insert = format!(
        "{head}
--{}{end}
{}{end}",
        case("COMMIT"),
        case("ROLLBACK")
    );
    Some(Item {
        label: case("BEGIN TRANSACTION"),
        kind: ItemKind::Snippet,
        detail: Some("template".to_owned()),
        caret: Some(head.len()),
        insert,
    })
}

/// Suggestions at byte offset `cursor`. `forced` (Ctrl+Space) also completes an empty prefix.
pub fn complete(
    sql_text: &str,
    cursor: usize,
    catalog: &Catalog,
    kind: DatabaseKind,
    forced: bool,
) -> Option<Completion> {
    if cursor > sql_text.len() || !sql_text.is_char_boundary(cursor) {
        return None;
    }
    // Never complete inside strings, comments or quoted identifiers.
    let inside_literal = sql::tokenize(sql_text).into_iter().any(|t| {
        matches!(
            t.kind,
            TokenKind::String | TokenKind::Comment | TokenKind::QuotedIdentifier
        ) && t.range.start < cursor
            && (cursor < t.range.end
                || (t.kind == TokenKind::Comment && sql_text[t.range.clone()].starts_with("--")))
            && !(cursor == t.range.end && t.kind != TokenKind::Comment)
    });
    if inside_literal {
        return None;
    }

    let start = sql_text[..cursor]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_ident_char(c))
        .last()
        .map_or(cursor, |(i, _)| i);
    let prefix = &sql_text[start..cursor];
    let qualifier = sql_text[..start].strip_suffix('.').map(|before| {
        let q_start = before
            .char_indices()
            .rev()
            .take_while(|&(_, c)| is_ident_char(c) || matches!(c, '"' | '[' | ']' | '`'))
            .last()
            .map_or(before.len(), |(i, _)| i);
        unquote(&before[q_start..]).to_owned()
    });
    if prefix.is_empty() && qualifier.is_none() && !forced {
        return None;
    }
    if prefix.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }

    let (_, tokens) = statement_tokens(sql_text, start);
    let before: Vec<&Token> = tokens.iter().filter(|t| t.range.end <= start).collect();
    let (referenced, aliases) = referenced_tables(sql_text, &tokens);

    let mut items: Vec<Item> = Vec::new();
    let column_items = |table: &TableInfo| -> Vec<Item> {
        table
            .columns
            .iter()
            .map(|c| Item {
                label: c.clone(),
                kind: ItemKind::Column,
                detail: Some(table.name.clone()),
                insert: ident(c, kind),
                caret: None,
            })
            .collect()
    };
    let table_item = |t: &TableInfo| Item {
        label: t.name.clone(),
        kind: if t.is_view {
            ItemKind::View
        } else {
            ItemKind::Table
        },
        detail: t.schema.clone(),
        insert: ident(&t.name, kind),
        caret: None,
    };

    if let Some(q) = &qualifier {
        let target = aliases
            .get(&q.to_lowercase())
            .cloned()
            .unwrap_or_else(|| q.clone());
        if let Some(table) = catalog.table(&target) {
            items.extend(column_items(table));
        }
        items.extend(
            catalog
                .tables
                .iter()
                .filter(|t| {
                    t.schema
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case(q))
                })
                .map(table_item),
        );
    } else {
        let last_keyword = before
            .iter()
            .rev()
            .find(|t| t.kind == TokenKind::Keyword)
            .map(|t| sql_text[t.range.clone()].to_ascii_uppercase());
        let context = match last_keyword.as_deref() {
            Some("FROM" | "JOIN" | "UPDATE" | "INTO" | "TABLE") => Context::Tables,
            Some(
                "SELECT" | "WHERE" | "ON" | "AND" | "OR" | "BY" | "SET" | "HAVING" | "DISTINCT"
                | "WHEN" | "THEN" | "ELSE" | "RETURNING",
            ) => Context::Columns,
            _ => Context::General,
        };
        if context != Context::Tables {
            let mut seen = BTreeSet::new();
            for name in &referenced {
                if let Some(table) = catalog.table(name)
                    && seen.insert(table.name.clone())
                {
                    items.extend(column_items(table));
                }
            }
        }
        if context != Context::Columns || referenced.is_empty() {
            items.extend(catalog.tables.iter().map(table_item));
            items.extend(catalog.schemas.iter().map(|s| Item {
                label: s.clone(),
                kind: ItemKind::Schema,
                detail: None,
                insert: ident(s, kind),
                caret: None,
            }));
        }
        let lower = prefix.chars().next().is_some_and(char::is_lowercase);
        items.extend(KEYWORDS.iter().map(|k| Item {
            label: (*k).to_owned(),
            kind: ItemKind::Keyword,
            detail: None,
            insert: if lower {
                k.to_lowercase()
            } else {
                (*k).to_owned()
            },
            caret: None,
        }));
        items.extend(transaction_template(kind, lower));
    }

    let needle = prefix.to_lowercase();
    let mut ranked: Vec<(u8, Item)> = items
        .into_iter()
        .filter_map(|item| {
            let label = item.label.to_lowercase();
            let rank = if needle.is_empty() || label.starts_with(&needle) {
                0
            } else if needle.len() >= 3 && label.contains(&needle) {
                1
            } else {
                return None;
            };
            // Templates need a few letters, so short prefixes still rank keywords first.
            if item.kind == ItemKind::Snippet && needle.chars().count() < 3 {
                return None;
            }
            // Hide a keyword that is already fully typed.
            if item.kind == ItemKind::Keyword && label == needle {
                return None;
            }
            Some((rank, item))
        })
        .collect();
    ranked.sort_by(|(ra, a), (rb, b)| {
        ra.cmp(rb)
            .then(a.kind.cmp(&b.kind))
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    let mut seen = BTreeSet::new();
    let items: Vec<Item> = ranked
        .into_iter()
        .map(|(_, item)| item)
        .filter(|item| seen.insert((item.kind, item.label.clone())))
        .take(MAX_ITEMS)
        .collect();
    (!items.is_empty()).then_some(Completion {
        replace: start..cursor,
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        let table = |name: &str, cols: &[&str]| {
            DbObject::new(name, ObjectKind::Table).with_children(
                cols.iter()
                    .map(|c| DbObject::new(*c, ObjectKind::Column))
                    .collect(),
            )
        };
        let tree = vec![
            DbObject::new("app", ObjectKind::Database).with_children(vec![
                DbObject::new("public", ObjectKind::Schema).with_children(vec![
                    DbObject::new("Tables", ObjectKind::Folder).with_children(vec![
                        table("customers", &["id", "full_name", "email"]),
                        table("orders", &["order_id", "customer_id", "total"]),
                        table("OrderItems", &["id"]),
                    ]),
                    DbObject::new("Views", ObjectKind::Folder)
                        .with_children(vec![DbObject::new("active_customers", ObjectKind::View)]),
                ]),
            ]),
        ];
        Catalog::from_objects(&tree)
    }

    fn labels(sql_text: &str) -> Vec<String> {
        let cursor = sql_text.find('|').expect("cursor marker");
        let text = sql_text.replace('|', "");
        complete(&text, cursor, &catalog(), DatabaseKind::PostgreSql, false)
            .map(|c| c.items.into_iter().map(|i| i.label).collect())
            .unwrap_or_default()
    }

    #[test]
    fn builds_catalog_from_tree() {
        let c = catalog();
        assert_eq!(c.schemas, vec!["public"]);
        assert_eq!(c.tables.len(), 4);
        assert_eq!(c.tables[0].schema.as_deref(), Some("public"));
        assert!(c.tables[3].is_view);
    }

    #[test]
    fn tables_after_from() {
        let l = labels("SELECT * FROM cu|");
        assert_eq!(l[0], "customers");
        assert!(!l.contains(&"full_name".to_owned()));
    }

    #[test]
    fn columns_of_referenced_tables_first() {
        let l = labels("SELECT fu| FROM customers");
        assert_eq!(l[0], "full_name");
        assert!(
            !l.contains(&"customers".to_owned()),
            "tables are not offered in SELECT list"
        );
        let l = labels("SELECT * FROM customers WHERE e|");
        assert_eq!(l[0], "email");
    }

    #[test]
    fn alias_and_table_qualifiers() {
        assert_eq!(
            labels("SELECT c.| FROM customers c"),
            vec!["email", "full_name", "id"]
        );
        assert_eq!(
            labels("SELECT o.cu| FROM orders AS o JOIN customers c ON c.id = o.customer_id"),
            vec!["customer_id"]
        );
        assert_eq!(labels("SELECT customers.e| FROM customers"), vec!["email"]);
        let l = labels("SELECT * FROM public.|");
        assert!(l.contains(&"orders".to_owned()) && l.contains(&"active_customers".to_owned()));
    }

    #[test]
    fn keywords_follow_typed_case() {
        let cursor = "sel".len();
        let c = complete("sel", cursor, &catalog(), DatabaseKind::PostgreSql, false).unwrap();
        assert_eq!(c.items[0].insert, "select");
        assert_eq!(c.replace, 0..3);
        let c = complete("SEL", cursor, &catalog(), DatabaseKind::PostgreSql, false).unwrap();
        assert_eq!(c.items[0].insert, "SELECT");
    }

    #[test]
    fn quotes_names_when_needed() {
        let cursor = "SELECT * FROM Ord".len();
        let c = complete(
            "SELECT * FROM Ord",
            cursor,
            &catalog(),
            DatabaseKind::PostgreSql,
            false,
        )
        .unwrap();
        let item = c.items.iter().find(|i| i.label == "OrderItems").unwrap();
        assert_eq!(item.insert, "\"OrderItems\"");
        let c = complete(
            "SELECT * FROM Ord",
            cursor,
            &catalog(),
            DatabaseKind::MySql,
            false,
        )
        .unwrap();
        let item = c.items.iter().find(|i| i.label == "OrderItems").unwrap();
        assert_eq!(item.insert, "OrderItems");
    }

    #[test]
    fn silent_in_strings_comments_and_without_prefix() {
        assert!(labels("SELECT 'cu|'").is_empty());
        assert!(labels("SELECT 1 -- cu|").is_empty());
        assert!(labels("SELECT |").is_empty());
        assert!(labels("SELECT 12|").is_empty());
        let forced = complete("SELECT ", 7, &catalog(), DatabaseKind::PostgreSql, true);
        assert!(forced.is_some());
    }

    #[test]
    fn offers_transaction_template_for_begin() {
        let c = complete("beg", 3, &catalog(), DatabaseKind::SqlServer, false).unwrap();
        let item = &c.items[0];
        assert_eq!(item.kind, ItemKind::Snippet);
        assert_eq!(
            item.insert,
            "begin transaction
--Query

--commit
rollback"
        );
        assert_eq!(
            &item.insert[item.caret.unwrap()..],
            "
--commit
rollback"
        );
        let c = complete("BEGIN", 5, &catalog(), DatabaseKind::PostgreSql, false).unwrap();
        assert_eq!(
            c.items[0].insert,
            "BEGIN TRANSACTION;
--Query

--COMMIT;
ROLLBACK;"
        );
        // Not for a short prefix, nor where the engine has no such statement.
        let c = complete("b", 1, &catalog(), DatabaseKind::SqlServer, false).unwrap();
        assert!(c.items.iter().all(|i| i.kind != ItemKind::Snippet));
        let c = complete("begin", 5, &catalog(), DatabaseKind::Oracle, false);
        assert!(c.is_none_or(|c| c.items.iter().all(|i| i.kind != ItemKind::Snippet)));
    }
}
