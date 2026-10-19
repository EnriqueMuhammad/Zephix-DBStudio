//! Stored functions and procedures: parameters, the statement that executes one, and the
//! script that edits one.

use serde::{Deserialize, Serialize};

use crate::dialect::quote_identifier;
use crate::profile::DatabaseKind;
use crate::schema::ObjectKind;

/// Direction of a routine parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamMode {
    /// Input only.
    In,
    /// Output only.
    Out,
    /// Input and output.
    InOut,
}

impl ParamMode {
    /// Short label (`IN`, `OUT`, `IN OUT`).
    pub fn label(self) -> &'static str {
        match self {
            ParamMode::In => "IN",
            ParamMode::Out => "OUT",
            ParamMode::InOut => "IN OUT",
        }
    }

    /// Whether the caller supplies a value.
    pub fn takes_value(self) -> bool {
        matches!(self, ParamMode::In | ParamMode::InOut)
    }

    /// Whether the routine hands a value back through it.
    pub fn returns_value(self) -> bool {
        matches!(self, ParamMode::Out | ParamMode::InOut)
    }
}

/// One parameter, in declaration order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineParam {
    /// Name as declared (SQL Server names keep their `@`). May be empty for positional
    /// PostgreSQL parameters.
    pub name: String,
    /// Declared type, e.g. `integer` or `nvarchar(50)`.
    pub data_type: String,
    /// Direction.
    pub mode: ParamMode,
}

/// What is needed to call a routine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineSignature {
    /// Parameters in declaration order.
    pub params: Vec<RoutineParam>,
    /// The function returns rows (table-valued / set-returning) rather than one value.
    pub returns_table: bool,
}

/// A value entered for an input parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgValue {
    /// SQL `NULL`.
    Null,
    /// Text as typed; numbers and booleans of numeric/boolean parameters are passed as is,
    /// everything else becomes a string literal.
    Text(String),
}

/// Statement(s) that execute `routine` (a function or a procedure) with `args`, one per
/// parameter in declaration order (ignored for output-only parameters).
pub fn call_statement(
    kind: DatabaseKind,
    qualified: &str,
    routine: ObjectKind,
    signature: &RoutineSignature,
    args: &[ArgValue],
) -> String {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(ArgValue::Null);
    let params: Vec<(usize, &RoutineParam)> = signature.params.iter().enumerate().collect();
    let function = routine == ObjectKind::Function;
    match kind {
        DatabaseKind::SqlServer => {
            let mut out = String::new();
            let outputs: Vec<&RoutineParam> = signature
                .params
                .iter()
                .filter(|p| !function && p.mode.returns_value())
                .collect();
            for p in &outputs {
                out.push_str(&format!("DECLARE {} {};\n", var_name(p, "@"), p.data_type));
            }
            for (i, p) in &params {
                if !function && p.mode == ParamMode::InOut {
                    out.push_str(&format!(
                        "SET {} = {};\n",
                        var_name(p, "@"),
                        literal(kind, p, &arg(*i))
                    ));
                }
            }
            if function {
                let list = params
                    .iter()
                    .filter(|(_, p)| p.mode.takes_value())
                    .map(|(i, p)| literal(kind, p, &arg(*i)))
                    .collect::<Vec<_>>()
                    .join(", ");
                if signature.returns_table {
                    out.push_str(&format!("SELECT *\nFROM {qualified}({list});"));
                } else {
                    out.push_str(&format!("SELECT {qualified}({list}) AS result;"));
                }
            } else {
                let list = params
                    .iter()
                    .map(|(i, p)| {
                        let name = if p.name.starts_with('@') {
                            p.name.clone()
                        } else {
                            format!("@{}", p.name)
                        };
                        match p.mode {
                            ParamMode::In => format!("{name} = {}", literal(kind, p, &arg(*i))),
                            _ => format!("{name} = {} OUTPUT", var_name(p, "@")),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                if list.is_empty() {
                    out.push_str(&format!("EXEC {qualified};"));
                } else {
                    out.push_str(&format!("EXEC {qualified} {list};"));
                }
                if !outputs.is_empty() {
                    let cols = outputs
                        .iter()
                        .map(|p| {
                            format!(
                                "{} AS {}",
                                var_name(p, "@"),
                                quote_identifier(kind, p.name.trim_start_matches('@'))
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    out.push_str(&format!("\nSELECT {cols};"));
                }
            }
            out
        }
        DatabaseKind::MySql | DatabaseKind::MariaDb => {
            if function {
                let list = params
                    .iter()
                    .map(|(i, p)| literal(kind, p, &arg(*i)))
                    .collect::<Vec<_>>()
                    .join(", ");
                return format!("SELECT {qualified}({list}) AS result;");
            }
            let mut out = String::new();
            for (i, p) in &params {
                if p.mode == ParamMode::InOut {
                    out.push_str(&format!(
                        "SET {} = {};\n",
                        var_name(p, "@"),
                        literal(kind, p, &arg(*i))
                    ));
                }
            }
            let list = params
                .iter()
                .map(|(i, p)| match p.mode {
                    ParamMode::In => literal(kind, p, &arg(*i)),
                    _ => var_name(p, "@"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("CALL {qualified}({list});"));
            let outputs: Vec<String> = signature
                .params
                .iter()
                .filter(|p| p.mode.returns_value())
                .map(|p| {
                    format!(
                        "{} AS {}",
                        var_name(p, "@"),
                        quote_identifier(kind, &p.name)
                    )
                })
                .collect();
            if !outputs.is_empty() {
                out.push_str(&format!("\nSELECT {};", outputs.join(", ")));
            }
            out
        }
        DatabaseKind::Oracle => {
            let list = params
                .iter()
                .map(|(i, p)| match p.mode {
                    ParamMode::In => literal(kind, p, &arg(*i)),
                    _ => var_name(p, "v_"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            let call = if list.is_empty() {
                qualified.to_owned()
            } else {
                format!("{qualified}({list})")
            };
            if function {
                return format!("SELECT {call} AS result\nFROM dual");
            }
            let outputs: Vec<&RoutineParam> = signature
                .params
                .iter()
                .filter(|p| p.mode.returns_value())
                .collect();
            if outputs.is_empty() {
                return format!("BEGIN\n  {call};\nEND;");
            }
            let mut out = String::from("DECLARE\n");
            for (i, p) in &params {
                if p.mode.returns_value() {
                    let init = if p.mode == ParamMode::InOut {
                        format!(" := {}", literal(kind, p, &arg(*i)))
                    } else {
                        String::new()
                    };
                    out.push_str(&format!(
                        "  {} {}{init};\n",
                        var_name(p, "v_"),
                        oracle_variable_type(&p.data_type)
                    ));
                }
            }
            out.push_str(&format!("BEGIN\n  {call};\n"));
            for p in outputs {
                out.push_str(&format!(
                    "  DBMS_OUTPUT.PUT_LINE('{}' || ' = ' || {});\n",
                    p.name.replace('\'', "''"),
                    var_name(p, "v_")
                ));
            }
            out.push_str("END;");
            out
        }
        // PostgreSQL (and anything else with SQL routines).
        _ => {
            // Functions take only input arguments; procedures take every argument, with NULL
            // standing in for output-only ones.
            let list = params
                .iter()
                .filter(|(_, p)| !function || p.mode.takes_value())
                .map(|(i, p)| {
                    if p.mode.takes_value() {
                        literal(kind, p, &arg(*i))
                    } else {
                        "NULL".to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            if function {
                format!("SELECT *\nFROM {qualified}({list});")
            } else {
                format!("CALL {qualified}({list});")
            }
        }
    }
}

/// Name of the variable holding an output parameter.
fn var_name(p: &RoutineParam, prefix: &str) -> String {
    let base: String = p
        .name
        .trim_start_matches('@')
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let base = if base.is_empty() {
        "out".to_owned()
    } else {
        base
    };
    format!("{prefix}{base}")
}

/// Oracle variables need a length for character types.
fn oracle_variable_type(declared: &str) -> String {
    let t = declared.trim().to_ascii_uppercase();
    match t.as_str() {
        "VARCHAR2" | "NVARCHAR2" | "VARCHAR" | "CHAR" | "NCHAR" => format!("{t}(4000)"),
        "RAW" => "RAW(2000)".to_owned(),
        _ => t,
    }
}

/// SQL literal for an argument of parameter `p`.
pub fn literal(kind: DatabaseKind, p: &RoutineParam, value: &ArgValue) -> String {
    let ArgValue::Text(text) = value else {
        return "NULL".to_owned();
    };
    let t = p.data_type.to_ascii_lowercase();
    let trimmed = text.trim();
    let numeric = [
        "int", "dec", "numeric", "number", "real", "float", "double", "money", "serial", "bit",
    ]
    .iter()
    .any(|n| t.contains(n))
        && !t.contains("interval");
    if numeric && !trimmed.is_empty() && trimmed.parse::<f64>().is_ok() {
        return trimmed.to_owned();
    }
    let boolean = t.starts_with("bool");
    if boolean && matches!(trimmed.to_ascii_lowercase().as_str(), "true" | "false") {
        return trimmed.to_ascii_uppercase();
    }
    let escaped = text.replace('\'', "''");
    let escaped = if matches!(kind, DatabaseKind::MySql | DatabaseKind::MariaDb) {
        escaped.replace('\\', "\\\\")
    } else {
        escaped
    };
    if kind == DatabaseKind::SqlServer {
        format!("N'{escaped}'")
    } else {
        format!("'{escaped}'")
    }
}

/// Turns the definition returned by the server into a script that saves changes when run.
pub fn edit_script(kind: DatabaseKind, qualified: &str, routine: ObjectKind, ddl: &str) -> String {
    let what = if routine == ObjectKind::Function {
        "FUNCTION"
    } else {
        "PROCEDURE"
    };
    let body = ddl.trim();
    match kind {
        DatabaseKind::SqlServer => {
            let replaced = replace_leading_create(body, "CREATE OR ALTER");
            format!(
                "-- Edit the {} below, then run the script (Ctrl+Enter) to save it.\n{replaced}",
                what.to_ascii_lowercase()
            )
        }
        DatabaseKind::MariaDb => {
            let replaced = if starts_with_create_or_replace(body) {
                body.to_owned()
            } else {
                replace_leading_create(body, "CREATE OR REPLACE")
            };
            format!(
                "-- Edit the {} below, then run the script (Ctrl+Enter) to save it.\n{replaced}",
                what.to_ascii_lowercase()
            )
        }
        DatabaseKind::MySql => format!(
            "-- Edit the {lower} below, then run the script (Ctrl+Enter) to save it.\n\
             -- MySQL has no CREATE OR REPLACE for routines: the old {lower} is dropped first.\n\
             DROP {what} IF EXISTS {qualified};\n{body}",
            lower = what.to_ascii_lowercase()
        ),
        _ => {
            let replaced = if starts_with_create_or_replace(body) {
                body.to_owned()
            } else {
                replace_leading_create(body, "CREATE OR REPLACE")
            };
            format!(
                "-- Edit the {} below, then run the script (Ctrl+Enter) to save it.\n{replaced}",
                what.to_ascii_lowercase()
            )
        }
    }
}

fn starts_with_create_or_replace(sql: &str) -> bool {
    let words: Vec<String> = code_start(sql)
        .split_whitespace()
        .take(3)
        .map(str::to_ascii_uppercase)
        .collect();
    words == ["CREATE", "OR", "REPLACE"]
}

/// The text after leading comments and whitespace.
fn code_start(sql: &str) -> &str {
    let mut rest = sql.trim_start();
    loop {
        if let Some(r) = rest.strip_prefix("--") {
            rest = r
                .split_once('\n')
                .map_or("", |(_, after)| after)
                .trim_start();
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r
                .split_once("*/")
                .map_or("", |(_, after)| after)
                .trim_start();
        } else {
            return rest;
        }
    }
}

/// Replaces the first `CREATE` keyword (after leading comments) with `replacement`.
fn replace_leading_create(sql: &str, replacement: &str) -> String {
    let start = code_start(sql);
    let offset = sql.len() - start.len();
    if start.len() >= 6 && start[..6].eq_ignore_ascii_case("CREATE") {
        format!("{}{replacement}{}", &sql[..offset], &start[6..])
    } else {
        sql.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, data_type: &str, mode: ParamMode) -> RoutineParam {
        RoutineParam {
            name: name.into(),
            data_type: data_type.into(),
            mode,
        }
    }

    fn text(s: &str) -> ArgValue {
        ArgValue::Text(s.into())
    }

    #[test]
    fn sql_server_procedure_with_output() {
        let sig = RoutineSignature {
            params: vec![
                p("@id", "int", ParamMode::In),
                p("@name", "nvarchar(50)", ParamMode::In),
                p("@total", "decimal(12,2)", ParamMode::Out),
            ],
            returns_table: false,
        };
        let sql = call_statement(
            DatabaseKind::SqlServer,
            "[dbo].[get_total]",
            ObjectKind::Procedure,
            &sig,
            &[text("7"), text("O'Neil"), ArgValue::Null],
        );
        assert_eq!(
            sql,
            "DECLARE @total decimal(12,2);\n\
             EXEC [dbo].[get_total] @id = 7, @name = N'O''Neil', @total = @total OUTPUT;\n\
             SELECT @total AS [total];"
        );
    }

    #[test]
    fn sql_server_functions() {
        let scalar = RoutineSignature {
            params: vec![p("@x", "int", ParamMode::In)],
            returns_table: false,
        };
        assert_eq!(
            call_statement(
                DatabaseKind::SqlServer,
                "[dbo].[f]",
                ObjectKind::Function,
                &scalar,
                &[text("3")]
            ),
            "SELECT [dbo].[f](3) AS result;"
        );
        let table = RoutineSignature {
            returns_table: true,
            ..scalar
        };
        assert_eq!(
            call_statement(
                DatabaseKind::SqlServer,
                "[dbo].[f]",
                ObjectKind::Function,
                &table,
                &[ArgValue::Null]
            ),
            "SELECT *\nFROM [dbo].[f](NULL);"
        );
        let none = RoutineSignature::default();
        assert_eq!(
            call_statement(
                DatabaseKind::SqlServer,
                "[dbo].[p]",
                ObjectKind::Procedure,
                &none,
                &[]
            ),
            "EXEC [dbo].[p];"
        );
    }

    #[test]
    fn mysql_procedure_with_out_and_inout() {
        let sig = RoutineSignature {
            params: vec![
                p("since", "date", ParamMode::In),
                p("counter", "int", ParamMode::InOut),
                p("total", "decimal", ParamMode::Out),
            ],
            returns_table: false,
        };
        let sql = call_statement(
            DatabaseKind::MySql,
            "`shop`.`stats`",
            ObjectKind::Procedure,
            &sig,
            &[text("2026-01-01"), text("5"), ArgValue::Null],
        );
        assert_eq!(
            sql,
            "SET @counter = 5;\n\
             CALL `shop`.`stats`('2026-01-01', @counter, @total);\n\
             SELECT @counter AS `counter`, @total AS `total`;"
        );
        let f = call_statement(
            DatabaseKind::MySql,
            "`shop`.`tax`",
            ObjectKind::Function,
            &RoutineSignature {
                params: vec![p("amount", "decimal(10,2)", ParamMode::In)],
                returns_table: false,
            },
            &[text("10.5")],
        );
        assert_eq!(f, "SELECT `shop`.`tax`(10.5) AS result;");
    }

    #[test]
    fn postgres_calls() {
        let sig = RoutineSignature {
            params: vec![
                p("a", "integer", ParamMode::In),
                p("flag", "boolean", ParamMode::In),
                p("result", "text", ParamMode::Out),
            ],
            returns_table: false,
        };
        let args = [text("1"), text("true"), ArgValue::Null];
        assert_eq!(
            call_statement(
                DatabaseKind::PostgreSql,
                "\"public\".\"f\"",
                ObjectKind::Function,
                &sig,
                &args
            ),
            "SELECT *\nFROM \"public\".\"f\"(1, TRUE);"
        );
        assert_eq!(
            call_statement(
                DatabaseKind::PostgreSql,
                "\"public\".\"p\"",
                ObjectKind::Procedure,
                &sig,
                &args
            ),
            "CALL \"public\".\"p\"(1, TRUE, NULL);"
        );
    }

    #[test]
    fn oracle_calls() {
        let sig = RoutineSignature {
            params: vec![
                p("P_ID", "NUMBER", ParamMode::In),
                p("P_NAME", "VARCHAR2", ParamMode::Out),
            ],
            returns_table: false,
        };
        let sql = call_statement(
            DatabaseKind::Oracle,
            "\"HR\".\"GET_NAME\"",
            ObjectKind::Procedure,
            &sig,
            &[text("100"), ArgValue::Null],
        );
        assert_eq!(
            sql,
            "DECLARE\n  v_P_NAME VARCHAR2(4000);\nBEGIN\n  \"HR\".\"GET_NAME\"(100, v_P_NAME);\n  \
             DBMS_OUTPUT.PUT_LINE('P_NAME' || ' = ' || v_P_NAME);\nEND;"
        );
        let f = call_statement(
            DatabaseKind::Oracle,
            "\"HR\".\"F\"",
            ObjectKind::Function,
            &RoutineSignature::default(),
            &[],
        );
        assert_eq!(f, "SELECT \"HR\".\"F\" AS result\nFROM dual");
    }

    #[test]
    fn literals_quote_text_but_not_numbers() {
        let n = p("x", "numeric(10,2)", ParamMode::In);
        assert_eq!(
            literal(DatabaseKind::PostgreSql, &n, &text(" 12.5 ")),
            "12.5"
        );
        assert_eq!(literal(DatabaseKind::PostgreSql, &n, &text("abc")), "'abc'");
        let s = p("s", "varchar(10)", ParamMode::In);
        assert_eq!(
            literal(DatabaseKind::MySql, &s, &text("a\\b'c")),
            "'a\\\\b''c'"
        );
        assert_eq!(literal(DatabaseKind::SqlServer, &s, &text("é")), "N'é'");
        assert_eq!(literal(DatabaseKind::Oracle, &s, &ArgValue::Null), "NULL");
        let i = p("i", "interval", ParamMode::In);
        assert_eq!(literal(DatabaseKind::PostgreSql, &i, &text("1")), "'1'");
    }

    #[test]
    fn edit_scripts_save_when_run() {
        let ms = edit_script(
            DatabaseKind::SqlServer,
            "[dbo].[p]",
            ObjectKind::Procedure,
            "-- note\n/* header */\ncreate procedure dbo.p AS SELECT 1",
        );
        assert!(
            ms.ends_with("-- note\n/* header */\nCREATE OR ALTER procedure dbo.p AS SELECT 1"),
            "{ms}"
        );
        let my = edit_script(
            DatabaseKind::MySql,
            "`shop`.`p`",
            ObjectKind::Procedure,
            "CREATE DEFINER=`root`@`%` PROCEDURE `p`() BEGIN SELECT 1; END;",
        );
        assert!(
            my.contains("DROP PROCEDURE IF EXISTS `shop`.`p`;\nCREATE DEFINER"),
            "{my}"
        );
        let maria = edit_script(
            DatabaseKind::MariaDb,
            "`shop`.`f`",
            ObjectKind::Function,
            "CREATE DEFINER=`root`@`%` FUNCTION `f`() RETURNS int RETURN 1;",
        );
        assert!(maria.contains("\nCREATE OR REPLACE DEFINER"), "{maria}");
        let pg_ddl = "CREATE OR REPLACE FUNCTION public.f()\n RETURNS integer AS $$ SELECT 1 $$;";
        let pg = edit_script(
            DatabaseKind::PostgreSql,
            "\"public\".\"f\"",
            ObjectKind::Function,
            pg_ddl,
        );
        assert!(pg.ends_with(pg_ddl), "{pg}");
        let ora = edit_script(
            DatabaseKind::Oracle,
            "\"HR\".\"P\"",
            ObjectKind::Procedure,
            "\n  CREATE OR REPLACE EDITIONABLE PROCEDURE \"HR\".\"P\" AS BEGIN NULL; END;",
        );
        assert!(ora.contains("CREATE OR REPLACE EDITIONABLE"), "{ora}");
    }
}
