//! A small parser for the MongoDB shell syntax used in the editor:
//! `db.orders.find({status: 'paid'}).sort({total: -1}).limit(10)`.
//!
//! Arguments are "relaxed JSON" (unquoted keys, single quotes, trailing commas,
//! `ObjectId(…)`, `ISODate(…)`, `NumberLong(…)`), converted to MongoDB Extended JSON.

use mongodb::bson::{Bson, Document};

/// A parsed shell statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    /// `show dbs` / `show databases`.
    ShowDatabases,
    /// `show collections`.
    ShowCollections,
    /// `use name`.
    Use(String),
    /// `db.method(args)` (runCommand, adminCommand, createCollection, stats, …).
    Database {
        /// Method name.
        method: String,
        /// Arguments.
        args: Vec<Bson>,
    },
    /// `db.collection.method(args).modifier(args)…`.
    Collection {
        /// Collection name.
        collection: String,
        /// Method name.
        method: String,
        /// Arguments.
        args: Vec<Bson>,
        /// Chained calls such as `sort`, `limit`, `skip`, `projection`.
        modifiers: Vec<(String, Vec<Bson>)>,
    },
}

/// Splits a script into statements: `;` or a newline ends a statement unless brackets are
/// open or the next line continues with `.method(`.
pub fn split_statements(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let chars: Vec<char> = script.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            current.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                current.push(c);
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                // Line comment.
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '(' | '[' | '{' => {
                depth += 1;
                current.push(c);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                current.push(c);
            }
            ';' if depth <= 0 => {
                out.push(std::mem::take(&mut current));
            }
            '\n' if depth <= 0 => {
                // A following line starting with '.' continues the chain.
                let rest: String = chars[i + 1..].iter().collect();
                if !rest.trim_start().starts_with('.') {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
        i += 1;
    }
    out.push(current);
    out.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .collect()
}

/// Converts relaxed JSON to strict (extended) JSON text.
pub fn relaxed_to_json(input: &str) -> Result<String, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len() + 16);
    let mut i = 0;
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$' || c == '.';
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' | '\'' => {
                let q = c;
                let mut s = String::new();
                i += 1;
                let mut closed = false;
                while i < chars.len() {
                    let d = chars[i];
                    if d == '\\' && i + 1 < chars.len() {
                        let n = chars[i + 1];
                        match n {
                            '\'' => s.push('\''),
                            '"' => s.push('"'),
                            other => {
                                s.push('\\');
                                s.push(other);
                            }
                        }
                        i += 2;
                        continue;
                    }
                    if d == q {
                        closed = true;
                        i += 1;
                        break;
                    }
                    if d == '"' {
                        s.push('\\');
                    }
                    s.push(d);
                    i += 1;
                }
                if !closed {
                    return Err("unterminated string".into());
                }
                out.push('"');
                out.push_str(&s);
                out.push('"');
                continue;
            }
            ',' => {
                // Drop trailing commas before a closing bracket.
                let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
                if !matches!(next, Some('}' | ']')) {
                    out.push(',');
                }
            }
            // Exponents and suffixes of numbers (`1e5`) stay as they are.
            c if c.is_alphabetic() && out.ends_with(|p: char| p.is_ascii_digit()) => out.push(c),
            c if c.is_alphabetic() || c == '_' || c == '$' => {
                let start = i;
                while i < chars.len() && is_ident(chars[i]) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let next = chars[i..].iter().find(|c| !c.is_whitespace()).copied();
                if next == Some(':') {
                    out.push('"');
                    out.push_str(&word);
                    out.push('"');
                    continue;
                }
                if next == Some('(') {
                    // Helper call: take the argument text up to the matching ')'.
                    let open = i + chars[i..].iter().position(|c| *c == '(').unwrap_or(0);
                    let mut depth = 0;
                    let mut end = open;
                    for (j, ch) in chars.iter().enumerate().skip(open) {
                        match ch {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    end = j;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    let arg: String = chars[open + 1..end].iter().collect();
                    let arg = arg.trim();
                    let unquoted = arg.trim_matches(|c| c == '"' || c == '\'');
                    let replacement = match word.as_str() {
                        "ObjectId" => format!("{{\"$oid\":\"{unquoted}\"}}"),
                        "ISODate" | "Date" => {
                            if arg.is_empty() {
                                format!("{{\"$date\":\"{}\"}}", now_iso())
                            } else {
                                format!("{{\"$date\":\"{unquoted}\"}}")
                            }
                        }
                        "NumberLong" => format!("{{\"$numberLong\":\"{unquoted}\"}}"),
                        "NumberInt" => unquoted.to_owned(),
                        "NumberDecimal" => format!("{{\"$numberDecimal\":\"{unquoted}\"}}"),
                        "UUID" => format!("{{\"$uuid\":\"{unquoted}\"}}"),
                        other => return Err(format!("unsupported helper {other}()")),
                    };
                    out.push_str(&replacement);
                    i = end + 1;
                    continue;
                }
                match word.as_str() {
                    "true" | "false" | "null" => out.push_str(&word),
                    "new" => {} // `new Date(…)`
                    "undefined" => out.push_str("null"),
                    other => return Err(format!("unexpected word {other}")),
                }
                continue;
            }
            other => out.push(other),
        }
        i += 1;
    }
    Ok(out)
}

fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    mongodb::bson::DateTime::from_millis(secs)
        .try_to_rfc3339_string()
        .unwrap_or_default()
}

/// Parses one relaxed-JSON value into BSON.
pub fn parse_value(text: &str) -> Result<Bson, String> {
    let json = relaxed_to_json(text)?;
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("{e} in {text}"))?;
    Bson::try_from(value).map_err(|e| e.to_string())
}

/// Splits `a, {b: 1}, [c]` at top-level commas.
fn split_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut current = String::new();
    let mut escaped = false;
    for c in text.chars() {
        if let Some(q) = quote {
            current.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                current.push(c);
            }
            '(' | '[' | '{' => {
                depth += 1;
                current.push(c);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                current.push(c);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    out.push(current);
    out.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `name(args)` calls of a chain such as `find({}).sort({a: 1}).limit(5)`.
fn parse_calls(text: &str) -> Result<Vec<(String, Vec<Bson>)>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut calls = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && (chars[i] == '.' || chars[i].is_whitespace()) {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        let name: String = chars[start..i].iter().collect();
        if name.is_empty() || chars.get(i) != Some(&'(') {
            return Err(format!(
                "expected a method call near {:?}",
                chars[start..].iter().collect::<String>()
            ));
        }
        let mut depth = 0;
        let mut quote: Option<char> = None;
        let mut end = None;
        for (j, &c) in chars.iter().enumerate().skip(i) {
            if let Some(q) = quote {
                if c == q && chars[j - 1] != '\\' {
                    quote = None;
                }
                continue;
            }
            match c {
                '"' | '\'' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(j);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.ok_or("missing ')'")?;
        let inner: String = chars[i + 1..end].iter().collect();
        let args = split_args(&inner)
            .iter()
            .map(|a| parse_value(a))
            .collect::<Result<Vec<_>, _>>()?;
        calls.push((name, args));
        i = end + 1;
    }
    Ok(calls)
}

/// Parses one statement.
pub fn parse(statement: &str) -> Result<Statement, String> {
    let s = statement.trim().trim_end_matches(';').trim();
    let lower = s.to_ascii_lowercase();
    if lower == "show dbs" || lower == "show databases" {
        return Ok(Statement::ShowDatabases);
    }
    if lower == "show collections" || lower == "show tables" {
        return Ok(Statement::ShowCollections);
    }
    if let Some(name) = s.strip_prefix("use ") {
        return Ok(Statement::Use(name.trim().to_owned()));
    }
    let Some(rest) = s.strip_prefix("db.") else {
        return Err("statements start with db., show or use".into());
    };
    // db.getCollection("name").method(...)
    let (collection, chain) = if let Some(after) = rest.strip_prefix("getCollection(") {
        let close = after.find(')').ok_or("missing ')' after getCollection(")?;
        let name = after[..close]
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_owned();
        (
            Some(name),
            after[close + 1..].trim_start_matches('.').to_owned(),
        )
    } else {
        let name_end = rest.find(['.', '(']).ok_or("incomplete statement")?;
        if rest[name_end..].starts_with('(') {
            (None, rest.to_owned())
        } else {
            (
                Some(rest[..name_end].to_owned()),
                rest[name_end + 1..].to_owned(),
            )
        }
    };
    let mut calls = parse_calls(&chain)?;
    if calls.is_empty() {
        return Err("missing method call".into());
    }
    let (method, args) = calls.remove(0);
    Ok(match collection {
        None => Statement::Database { method, args },
        Some(collection) => Statement::Collection {
            collection,
            method,
            args,
            modifiers: calls,
        },
    })
}

/// The argument at `i` as a document (`{}` when absent).
pub fn doc_arg(args: &[Bson], i: usize) -> Result<Document, String> {
    match args.get(i) {
        None | Some(Bson::Null) => Ok(Document::new()),
        Some(Bson::Document(d)) => Ok(d.clone()),
        Some(other) => Err(format!("expected a document, got {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    #[test]
    fn relaxed_json_becomes_extended_json() {
        assert_eq!(
            parse_value("{name: 'Ayu', age: {$gt: 30}, tags: ['a', 'b',], }").unwrap(),
            Bson::Document(doc! {"name": "Ayu", "age": {"$gt": 30}, "tags": ["a", "b"]})
        );
        let Bson::Document(d) = parse_value(r#"{_id: ObjectId("65f0c0ffee0000000000abcd"), at: ISODate("2026-01-02T03:04:05Z"), n: NumberLong(5)}"#).unwrap() else {
            panic!("document");
        };
        assert!(matches!(d.get("_id"), Some(Bson::ObjectId(_))));
        assert!(matches!(d.get("at"), Some(Bson::DateTime(_))));
        assert_eq!(d.get("n"), Some(&Bson::Int64(5)));
        assert_eq!(
            parse_value("'it\\'s'").unwrap(),
            Bson::String("it's".into())
        );
    }

    #[test]
    fn parses_shell_statements() {
        assert_eq!(parse("show dbs").unwrap(), Statement::ShowDatabases);
        assert_eq!(parse("use shop").unwrap(), Statement::Use("shop".into()));
        let s = parse("db.orders.find({status: 'paid'}, {total: 1}).sort({total: -1}).limit(10)")
            .unwrap();
        let Statement::Collection {
            collection,
            method,
            args,
            modifiers,
        } = s
        else {
            panic!("collection statement");
        };
        assert_eq!(collection, "orders");
        assert_eq!(method, "find");
        assert_eq!(args.len(), 2);
        assert_eq!(modifiers[0].0, "sort");
        assert_eq!(modifiers[1], ("limit".to_owned(), vec![Bson::Int32(10)]));
        let s = parse(r#"db.getCollection("my coll").countDocuments({})"#).unwrap();
        assert!(
            matches!(s, Statement::Collection { ref collection, .. } if collection == "my coll")
        );
        let s = parse("db.runCommand({ping: 1})").unwrap();
        assert!(matches!(s, Statement::Database { ref method, .. } if method == "runCommand"));
        assert!(parse("select 1").is_err());
    }

    #[test]
    fn splits_scripts_with_chains_on_new_lines() {
        let parts = split_statements(
            "use shop\ndb.a.find({x: 1,\n  y: 2})\n  .limit(2);\n// comment\ndb.b.drop()",
        );
        assert_eq!(parts.len(), 3);
        assert!(parts[1].contains(".limit(2)"));
        assert_eq!(parts[2], "db.b.drop()");
    }
}
