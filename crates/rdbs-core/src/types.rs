//! Engine-neutral classification of column types, cross-engine type mapping (data transfer)
//! and conversion of text values (imports) into SQL literals.

use crate::export::sql_string;
use crate::profile::DatabaseKind;

/// What kind of values a column holds, independent of the engine's spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeClass {
    /// Whole numbers.
    Integer,
    /// Exact numerics (`numeric(p,s)`, `decimal`, `money`).
    Decimal,
    /// Floating point.
    Float,
    /// Booleans / bits.
    Boolean,
    /// Character data.
    Text,
    /// Calendar date.
    Date,
    /// Time of day.
    Time,
    /// Date and time (with or without zone).
    DateTime,
    /// Binary data.
    Binary,
    /// JSON documents.
    Json,
    /// UUID / uniqueidentifier.
    Uuid,
    /// Anything else (arrays, geometry, enums, …).
    Other,
}

/// Lower-cased base name without arguments: `numeric(12,2)` → `numeric`.
fn base(data_type: &str) -> String {
    let lower = data_type.trim().to_ascii_lowercase();
    let end = lower.find('(').unwrap_or(lower.len());
    lower[..end].trim().to_owned()
}

/// `(p,s)` / `(n)` arguments of a type, if any.
fn args(data_type: &str) -> Option<&str> {
    let start = data_type.find('(')?;
    let end = data_type.rfind(')')?;
    (end > start).then(|| data_type[start + 1..end].trim())
}

/// Classifies a native column type.
pub fn classify(data_type: &str) -> TypeClass {
    let b = base(data_type);
    let lower = data_type.to_ascii_lowercase();
    if lower.ends_with("[]") {
        return TypeClass::Other;
    }
    match b.as_str() {
        "tinyint" if args(data_type) == Some("1") => TypeClass::Boolean,
        "int" | "integer" | "int2" | "int4" | "int8" | "smallint" | "bigint" | "tinyint"
        | "mediumint" | "serial" | "bigserial" | "smallserial" | "int unsigned"
        | "bigint unsigned" | "integer unsigned" | "smallint unsigned" | "tinyint unsigned"
        | "mediumint unsigned" => TypeClass::Integer,
        "number" => {
            match args(data_type).map(|a| a.split(',').map(str::trim).collect::<Vec<_>>()) {
                Some(parts) if parts.len() == 1 || parts.get(1) == Some(&"0") => {
                    if parts[0] == "1" {
                        TypeClass::Boolean
                    } else {
                        TypeClass::Integer
                    }
                }
                _ => TypeClass::Decimal,
            }
        }
        "numeric" | "decimal" | "money" | "smallmoney" => TypeClass::Decimal,
        "binary_float" | "binary_double" => TypeClass::Float,
        "varchar2" | "nvarchar2" | "nclob" | "long" => TypeClass::Text,
        "raw" | "long raw" | "bfile" => TypeClass::Binary,
        "real" | "float" | "float4" | "float8" | "double" | "double precision" => TypeClass::Float,
        "bool" | "boolean" | "bit" => TypeClass::Boolean,
        "date" => TypeClass::Date,
        "time" | "time without time zone" | "time with time zone" | "timetz" => TypeClass::Time,
        "datetime"
        | "datetime2"
        | "smalldatetime"
        | "datetimeoffset"
        | "timestamp"
        | "timestamptz"
        | "timestamp without time zone"
        | "timestamp with time zone" => TypeClass::DateTime,
        "bytea" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "binary" | "varbinary"
        | "image" => TypeClass::Binary,
        "json" | "jsonb" => TypeClass::Json,
        "uuid" | "uniqueidentifier" => TypeClass::Uuid,
        "char" | "character" | "varchar" | "character varying" | "nchar" | "nvarchar" | "text"
        | "tinytext" | "mediumtext" | "longtext" | "ntext" | "citext" | "string" | "clob"
        | "xml" | "enum" | "set" | "name" => TypeClass::Text,
        _ if b.contains("int") => TypeClass::Integer,
        _ if b.contains("char") || b.contains("text") || b.contains("clob") => TypeClass::Text,
        _ if b.contains("blob") => TypeClass::Binary,
        _ if b.contains("real") || b.contains("floa") || b.contains("doub") => TypeClass::Float,
        _ if b.contains("date") || b.contains("time") => TypeClass::DateTime,
        _ => TypeClass::Other,
    }
}

/// Canonical spelling of a type for comparisons: lower case, single spaces, PostgreSQL/SQL
/// aliases folded (`character varying(10)` = `varchar(10)`, `int4` = `integer`, …).
pub fn normalize_type(data_type: &str) -> String {
    let lower = data_type
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let (name, rest) = match lower.find('(') {
        Some(i) => (lower[..i].trim().to_owned(), lower[i..].replace(' ', "")),
        None => (lower.clone(), String::new()),
    };
    let canonical = match name.as_str() {
        "character varying" => "varchar",
        "character" => "char",
        "int" | "int4" => "integer",
        "int8" => "bigint",
        "int2" => "smallint",
        "float8" => "double precision",
        "float4" => "real",
        "bool" => "boolean",
        "decimal" => "numeric",
        "timestamp without time zone" => "timestamp",
        "timestamptz" => "timestamp with time zone",
        "time without time zone" => "time",
        "timetz" => "time with time zone",
        other => other,
    };
    // SQL Server reports its default fraction precision; `datetime2` means `datetime2(7)`.
    let rest = match (canonical, rest.as_str()) {
        ("datetime2" | "datetimeoffset" | "time", "(7)") => "",
        _ => rest.as_str(),
    };
    format!("{canonical}{rest}")
}

/// Whether two type spellings denote the same type.
pub fn equivalent_types(a: &str, b: &str) -> bool {
    normalize_type(a) == normalize_type(b)
}

/// Default expression in lower case without engine decoration (for comparisons).
pub fn normalize_default(expr: &str) -> String {
    strip_default_decoration(expr).to_ascii_lowercase()
}

/// Default expression without engine decoration: PostgreSQL casts (`'x'::text`), SQL Server
/// parentheses and `N` prefixes.
pub fn strip_default_decoration(expr: &str) -> String {
    let mut s = expr.trim().to_owned();
    loop {
        let before = s.clone();
        if s.starts_with('(') && s.ends_with(')') && balanced(&s[1..s.len() - 1]) {
            s = s[1..s.len() - 1].trim().to_owned();
        }
        if let Some(i) = s.rfind("::")
            && !s[i..].contains('\'')
        {
            s = s[..i].trim().to_owned();
        }
        if s.starts_with("N'") {
            s.remove(0);
        }
        if s == before {
            break;
        }
    }
    s
}

fn balanced(s: &str) -> bool {
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

/// Whether two optional default expressions are the same.
pub fn equivalent_defaults(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => normalize_default(a) == normalize_default(b),
        _ => false,
    }
}

/// Length argument of a character type (`varchar(100)` → 100; `max` → `None`).
fn char_length(data_type: &str) -> Option<u32> {
    args(data_type)?.parse().ok()
}

/// Maps a column type of `from` to the closest type of `to`. Same-engine types are kept.
pub fn map_type(from: DatabaseKind, data_type: &str, to: DatabaseKind) -> String {
    let same_family = |k: DatabaseKind| match k {
        DatabaseKind::MariaDb => DatabaseKind::MySql,
        other => other,
    };
    if same_family(from) == same_family(to) || !to.is_sql() {
        return data_type.to_owned();
    }
    let b = base(data_type);
    let numeric_args = args(data_type)
        .filter(|a| a.split(',').all(|p| p.trim().parse::<u32>().is_ok()))
        .map(|a| format!("({a})"))
        .unwrap_or_default();
    let length = char_length(data_type);
    let big = matches!(
        b.as_str(),
        "bigint" | "int8" | "bigserial" | "bigint unsigned"
    );
    let small = matches!(b.as_str(), "smallint" | "int2" | "tinyint" | "smallserial");
    let fixed = matches!(b.as_str(), "char" | "character" | "nchar");
    use DatabaseKind::*;
    if to == Oracle {
        return match classify(data_type) {
            TypeClass::Integer if big => "NUMBER(19)".into(),
            TypeClass::Integer if small => "NUMBER(5)".into(),
            TypeClass::Integer => "NUMBER(10)".into(),
            TypeClass::Decimal if numeric_args.is_empty() => "NUMBER".into(),
            TypeClass::Decimal => format!("NUMBER{numeric_args}"),
            TypeClass::Float => "BINARY_DOUBLE".into(),
            TypeClass::Boolean => "NUMBER(1)".into(),
            TypeClass::Text => match length {
                Some(n) if n <= 4000 => {
                    format!("{}({n} CHAR)", if fixed { "CHAR" } else { "VARCHAR2" })
                }
                _ => "CLOB".into(),
            },
            TypeClass::Date => "DATE".into(),
            TypeClass::Time => "VARCHAR2(20)".into(),
            TypeClass::DateTime => {
                if data_type.to_ascii_lowercase().contains("zone")
                    || b == "timestamptz"
                    || b == "datetimeoffset"
                {
                    "TIMESTAMP WITH TIME ZONE".into()
                } else {
                    "TIMESTAMP".into()
                }
            }
            TypeClass::Binary => "BLOB".into(),
            TypeClass::Json | TypeClass::Other => "CLOB".into(),
            TypeClass::Uuid => "VARCHAR2(36)".into(),
        };
    }
    match (classify(data_type), to) {
        (TypeClass::Integer, Sqlite) => "INTEGER".into(),
        (TypeClass::Integer, _) if big => "bigint".into(),
        (TypeClass::Integer, _) if small => "smallint".into(),
        (TypeClass::Integer, PostgreSql) => "integer".into(),
        (TypeClass::Integer, _) => "int".into(),
        (TypeClass::Decimal, Sqlite) => "NUMERIC".into(),
        (TypeClass::Decimal, PostgreSql) => format!("numeric{numeric_args}"),
        (TypeClass::Decimal, _) => {
            if numeric_args.is_empty() {
                "decimal(38,10)".into()
            } else {
                format!("decimal{numeric_args}")
            }
        }
        (TypeClass::Float, Sqlite) => "REAL".into(),
        (TypeClass::Float, PostgreSql) => "double precision".into(),
        (TypeClass::Float, SqlServer) => "float".into(),
        (TypeClass::Float, _) => "double".into(),
        (TypeClass::Boolean, PostgreSql) => "boolean".into(),
        (TypeClass::Boolean, SqlServer) => "bit".into(),
        (TypeClass::Boolean, MySql | MariaDb) => "tinyint(1)".into(),
        (TypeClass::Boolean, Sqlite) => "BOOLEAN".into(),
        (TypeClass::Text, Sqlite) => "TEXT".into(),
        (TypeClass::Text, SqlServer) => match length {
            Some(n) if n <= 4000 => format!("{}({n})", if fixed { "nchar" } else { "nvarchar" }),
            _ => "nvarchar(max)".into(),
        },
        (TypeClass::Text, MySql | MariaDb) => match length {
            Some(n) if n <= 16383 => format!("{}({n})", if fixed { "char" } else { "varchar" }),
            _ => "longtext".into(),
        },
        (TypeClass::Text, PostgreSql) => match length {
            Some(n) => format!("{}({n})", if fixed { "char" } else { "varchar" }),
            None => "text".into(),
        },
        (TypeClass::Date, Sqlite) => "DATE".into(),
        (TypeClass::Date, _) => "date".into(),
        (TypeClass::Time, Sqlite) => "TEXT".into(),
        (TypeClass::Time, _) => "time".into(),
        (TypeClass::DateTime, PostgreSql) => {
            if data_type.to_ascii_lowercase().contains("zone")
                || b == "timestamptz"
                || b == "datetimeoffset"
            {
                "timestamp with time zone".into()
            } else {
                "timestamp".into()
            }
        }
        (TypeClass::DateTime, SqlServer) => {
            if b == "timestamptz" || data_type.to_ascii_lowercase().contains("with time zone") {
                "datetimeoffset".into()
            } else {
                "datetime2".into()
            }
        }
        (TypeClass::DateTime, MySql | MariaDb) => "datetime(6)".into(),
        (TypeClass::DateTime, Sqlite) => "DATETIME".into(),
        (TypeClass::Binary, PostgreSql) => "bytea".into(),
        (TypeClass::Binary, SqlServer) => "varbinary(max)".into(),
        (TypeClass::Binary, MySql | MariaDb) => "longblob".into(),
        (TypeClass::Binary, Sqlite) => "BLOB".into(),
        (TypeClass::Json, PostgreSql) => "jsonb".into(),
        (TypeClass::Json, SqlServer) => "nvarchar(max)".into(),
        (TypeClass::Json, MySql | MariaDb) => "json".into(),
        (TypeClass::Json, Sqlite) => "JSON".into(),
        (TypeClass::Uuid, PostgreSql) => "uuid".into(),
        (TypeClass::Uuid, SqlServer) => "uniqueidentifier".into(),
        (TypeClass::Uuid, MySql | MariaDb) => "char(36)".into(),
        (TypeClass::Uuid, Sqlite) => "TEXT".into(),
        (TypeClass::Other, Sqlite) => "TEXT".into(),
        (TypeClass::Other, SqlServer) => "nvarchar(max)".into(),
        (TypeClass::Other, MySql | MariaDb) => "longtext".into(),
        (TypeClass::Other, PostgreSql) => "text".into(),
        // Handled above / not SQL.
        (_, Oracle | MongoDb | Redis) => data_type.to_owned(),
    }
}

/// Converts imported text into a literal for a column of class `class`.
///
/// `empty_is_null` turns empty strings into NULL. Numbers are validated; booleans accept
/// `true/false/yes/no/1/0/t/f` (any case).
pub fn text_literal(
    kind: DatabaseKind,
    class: TypeClass,
    text: &str,
    empty_is_null: bool,
) -> Result<String, String> {
    let trimmed = text.trim();
    if (text.is_empty() && empty_is_null)
        || (trimmed.eq_ignore_ascii_case("null") && class != TypeClass::Text)
    {
        return Ok("NULL".into());
    }
    match class {
        TypeClass::Integer => trimmed
            .parse::<i128>()
            .map(|n| n.to_string())
            .map_err(|_| format!("{trimmed:?} is not a whole number")),
        TypeClass::Decimal | TypeClass::Float => {
            let normalized = trimmed.replace('_', "");
            normalized
                .parse::<f64>()
                .ok()
                .filter(|f| f.is_finite())
                .map(|_| normalized.clone())
                .ok_or_else(|| format!("{trimmed:?} is not a number"))
        }
        TypeClass::Boolean => {
            let value = match trimmed.to_ascii_lowercase().as_str() {
                "true" | "t" | "yes" | "y" | "1" => true,
                "false" | "f" | "no" | "n" | "0" => false,
                _ => return Err(format!("{trimmed:?} is not a boolean")),
            };
            Ok(match kind {
                DatabaseKind::SqlServer | DatabaseKind::Sqlite | DatabaseKind::Oracle => {
                    u8::from(value).to_string()
                }
                _ => if value { "TRUE" } else { "FALSE" }.to_owned(),
            })
        }
        TypeClass::Binary => {
            let hex = trimmed.trim_start_matches("0x").trim_start_matches("\\x");
            if !hex.len().is_multiple_of(2) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!("{trimmed:?} is not hexadecimal binary data"));
            }
            Ok(match kind {
                DatabaseKind::PostgreSql => format!("'\\x{hex}'::bytea"),
                DatabaseKind::SqlServer => format!("0x{hex}"),
                DatabaseKind::Oracle => format!("HEXTORAW('{hex}')"),
                _ => format!("X'{hex}'"),
            })
        }
        _ => Ok(sql_string(text, kind)),
    }
}

/// Infers a column type from sample values (imports into a new table).
pub fn infer_type<'a>(kind: DatabaseKind, values: impl IntoIterator<Item = &'a str>) -> String {
    let mut all_int = true;
    let mut all_num = true;
    let mut all_bool = true;
    let mut all_date = true;
    let mut all_datetime = true;
    let mut max_len = 0usize;
    let mut any = false;
    for v in values {
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        any = true;
        max_len = max_len.max(v.chars().count());
        all_int &= v.parse::<i64>().is_ok();
        all_num &= v.parse::<f64>().is_ok_and(f64::is_finite);
        all_bool &= matches!(
            v.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no"
        );
        let is_date = v.len() == 10
            && v.as_bytes()[4] == b'-'
            && v.as_bytes()[7] == b'-'
            && v[..4].parse::<u16>().is_ok();
        all_date &= is_date;
        all_datetime &= v.len() >= 16
            && v.as_bytes().get(4) == Some(&b'-')
            && matches!(v.as_bytes().get(10), Some(b'T' | b' '))
            && v[..4].parse::<u16>().is_ok();
    }
    let class_type = |class: &str| -> String {
        let generic = match class {
            "int" => "bigint",
            "num" => "double precision",
            "bool" => "boolean",
            "date" => "date",
            "datetime" => "timestamp",
            _ => "text",
        };
        map_type(DatabaseKind::PostgreSql, generic, kind)
    };
    if !any {
        return class_type("text");
    }
    if all_int {
        class_type("int")
    } else if all_num {
        class_type("num")
    } else if all_bool {
        class_type("bool")
    } else if all_date {
        class_type("date")
    } else if all_datetime {
        class_type("datetime")
    } else {
        let len = (max_len.max(1) * 2).next_power_of_two().max(32);
        if len <= 4000 {
            map_type(DatabaseKind::PostgreSql, &format!("varchar({len})"), kind)
        } else {
            class_type("text")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DatabaseKind::*;

    #[test]
    fn classifies_common_types() {
        assert_eq!(classify("integer"), TypeClass::Integer);
        assert_eq!(classify("INT UNSIGNED"), TypeClass::Integer);
        assert_eq!(classify("tinyint(1)"), TypeClass::Boolean);
        assert_eq!(classify("numeric(12,2)"), TypeClass::Decimal);
        assert_eq!(classify("character varying(50)"), TypeClass::Text);
        assert_eq!(classify("nvarchar(max)"), TypeClass::Text);
        assert_eq!(classify("timestamp with time zone"), TypeClass::DateTime);
        assert_eq!(classify("datetime2(7)"), TypeClass::DateTime);
        assert_eq!(classify("bytea"), TypeClass::Binary);
        assert_eq!(classify("jsonb"), TypeClass::Json);
        assert_eq!(classify("integer[]"), TypeClass::Other);
        assert_eq!(classify("VARCHAR(100)"), TypeClass::Text);
    }

    #[test]
    fn maps_between_engines() {
        assert_eq!(
            map_type(PostgreSql, "character varying(50)", SqlServer),
            "nvarchar(50)"
        );
        assert_eq!(map_type(PostgreSql, "text", SqlServer), "nvarchar(max)");
        assert_eq!(map_type(SqlServer, "nvarchar(max)", PostgreSql), "text");
        assert_eq!(map_type(SqlServer, "bit", PostgreSql), "boolean");
        assert_eq!(map_type(PostgreSql, "boolean", MySql), "tinyint(1)");
        assert_eq!(
            map_type(MySql, "decimal(12,2)", PostgreSql),
            "numeric(12,2)"
        );
        assert_eq!(map_type(PostgreSql, "numeric", SqlServer), "decimal(38,10)");
        assert_eq!(map_type(PostgreSql, "bigint", Sqlite), "INTEGER");
        assert_eq!(map_type(MySql, "datetime", PostgreSql), "timestamp");
        assert_eq!(
            map_type(PostgreSql, "timestamp with time zone", SqlServer),
            "datetimeoffset"
        );
        assert_eq!(map_type(PostgreSql, "uuid", MySql), "char(36)");
        assert_eq!(
            map_type(MySql, "int", MariaDb),
            "int",
            "same family keeps types"
        );
    }

    #[test]
    fn normalizes_spellings() {
        assert!(equivalent_types("character varying(50)", "VARCHAR(50)"));
        assert!(equivalent_types("int4", "integer"));
        assert!(equivalent_types("numeric(12, 2)", "decimal(12,2)"));
        assert!(equivalent_types("timestamp without time zone", "timestamp"));
        assert!(!equivalent_types("varchar(50)", "varchar(51)"));
        assert!(equivalent_types("datetime2", "datetime2(7)"));
        assert!(!equivalent_types("datetime2", "datetime2(3)"));
        assert!(equivalent_defaults(
            Some("'x'::character varying"),
            Some("'x'")
        ));
        assert!(equivalent_defaults(Some("((0))"), Some("0")));
        assert!(equivalent_defaults(Some("(N'a')"), Some("'a'")));
        assert!(!equivalent_defaults(Some("0"), None));
        assert!(!equivalent_defaults(Some("1"), Some("0")));
    }

    #[test]
    fn converts_text_values() {
        let lit = |class, text| text_literal(PostgreSql, class, text, true);
        assert_eq!(lit(TypeClass::Integer, " 42 ").unwrap(), "42");
        assert!(lit(TypeClass::Integer, "4.2").is_err());
        assert_eq!(lit(TypeClass::Decimal, "1.50").unwrap(), "1.50");
        assert_eq!(lit(TypeClass::Boolean, "Yes").unwrap(), "TRUE");
        assert_eq!(
            text_literal(SqlServer, TypeClass::Boolean, "false", true).unwrap(),
            "0"
        );
        assert_eq!(lit(TypeClass::Text, "it's").unwrap(), "'it''s'");
        assert_eq!(lit(TypeClass::Text, "").unwrap(), "NULL");
        assert_eq!(
            text_literal(PostgreSql, TypeClass::Text, "", false).unwrap(),
            "''"
        );
        assert_eq!(lit(TypeClass::Integer, "NULL").unwrap(), "NULL");
        assert_eq!(lit(TypeClass::Text, "NULL").unwrap(), "'NULL'");
        assert_eq!(
            lit(TypeClass::Binary, "0xDEAD").unwrap(),
            "'\\xDEAD'::bytea"
        );
    }

    #[test]
    fn infers_types_from_samples() {
        assert_eq!(infer_type(PostgreSql, ["1", "2", ""]), "bigint");
        assert_eq!(infer_type(PostgreSql, ["1.5", "2"]), "double precision");
        assert_eq!(infer_type(SqlServer, ["true", "no"]), "bit");
        assert_eq!(infer_type(MySql, ["2026-01-02", "2025-12-31"]), "date");
        assert_eq!(infer_type(PostgreSql, ["2026-01-02 10:00:00"]), "timestamp");
        assert_eq!(infer_type(PostgreSql, ["abc", "de"]), "varchar(32)");
        assert_eq!(infer_type(Sqlite, ["abc"]), "TEXT");
        assert_eq!(infer_type(PostgreSql, Vec::<&str>::new()), "text");
    }
}
