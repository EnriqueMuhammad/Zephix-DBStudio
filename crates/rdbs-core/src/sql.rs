//! A small, dialect-tolerant SQL lexer.
//!
//! It is used for syntax highlighting, splitting scripts into statements and classifying
//! statements for production safeguards. It is not a parser: it never fails, and unknown
//! input is returned as [`TokenKind::Unknown`] tokens.

use std::ops::Range;

/// Lexical category of a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// Spaces, tabs, newlines.
    Whitespace,
    /// `-- line` or `/* block */` comment.
    Comment,
    /// Reserved or common SQL keyword.
    Keyword,
    /// Bare identifier.
    Identifier,
    /// `"quoted"`, `[bracketed]` or `` `backticked` `` identifier.
    QuotedIdentifier,
    /// String literal, including `N'…'`, `E'…'` and PostgreSQL `$$…$$`.
    String,
    /// Numeric literal.
    Number,
    /// Bind parameter or variable: `$1`, `?`, `:name`, `@var`, `@@VERSION`.
    Parameter,
    /// Operator characters.
    Operator,
    /// `(`, `)`, `,`, `;`, `.`
    Punctuation,
    /// Anything else.
    Unknown,
}

/// A token and its byte range in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Category.
    pub kind: TokenKind,
    /// Byte range into the source string (always on `char` boundaries).
    pub range: Range<usize>,
}

/// Sorted (ASCII uppercase) keywords used for highlighting and completion.
pub const KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "ANALYZE",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "AUTO_INCREMENT",
    "BEGIN",
    "BETWEEN",
    "BIGINT",
    "BOOLEAN",
    "BY",
    "CALL",
    "CASCADE",
    "CASE",
    "CAST",
    "CHAR",
    "CHECK",
    "COLUMN",
    "COMMENT",
    "COMMIT",
    "CONSTRAINT",
    "COPY",
    "CREATE",
    "CROSS",
    "CURRENT_DATE",
    "CURRENT_TIMESTAMP",
    "DATABASE",
    "DATE",
    "DECIMAL",
    "DECLARE",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DESCRIBE",
    "DISTINCT",
    "DO",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXEC",
    "EXECUTE",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FETCH",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "FUNCTION",
    "GO",
    "GRANT",
    "GROUP",
    "HAVING",
    "IDENTITY",
    "IF",
    "ILIKE",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INT",
    "INTEGER",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LEFT",
    "LIKE",
    "LIMIT",
    "MERGE",
    "NEXT",
    "NOT",
    "NULL",
    "NUMERIC",
    "NVARCHAR",
    "OFFSET",
    "ON",
    "ONLY",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PRIMARY",
    "PROCEDURE",
    "RECURSIVE",
    "REFERENCES",
    "RENAME",
    "REPLACE",
    "RETURNING",
    "RETURNS",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWS",
    "SAVEPOINT",
    "SCHEMA",
    "SELECT",
    "SET",
    "SHOW",
    "SMALLINT",
    "START",
    "TABLE",
    "TEXT",
    "THEN",
    "TIMESTAMP",
    "TOP",
    "TRANSACTION",
    "TRIGGER",
    "TRUE",
    "TRUNCATE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USE",
    "USING",
    "VALUES",
    "VARCHAR",
    "VIEW",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
];

/// Whether `word` is a known SQL keyword (case-insensitive).
pub fn is_keyword(word: &str) -> bool {
    if word.len() > 24 || !word.is_ascii() {
        return false;
    }
    let upper = word.to_ascii_uppercase();
    KEYWORDS.binary_search(&upper.as_str()).is_ok()
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_nth(&self, n: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn bump_while(&mut self, mut pred: impl FnMut(char) -> bool) {
        while let Some(c) = self.peek() {
            if !pred(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
    }

    /// Consumes until (and including) `close`; a doubled `close` is an escape.
    fn quoted(&mut self, close: char) {
        while let Some(c) = self.bump() {
            if c == close {
                if self.peek() == Some(close) {
                    self.bump();
                } else {
                    return;
                }
            }
        }
    }

    /// PostgreSQL dollar-quoted string. `self.pos` is at the opening `$`.
    /// Returns `false` (without consuming) if this is not a dollar quote.
    fn dollar_quoted(&mut self) -> bool {
        let rest = &self.src[self.pos + 1..];
        let tag_len = rest
            .char_indices()
            .find(|&(_, c)| !(c.is_alphanumeric() || c == '_'))
            .map_or(rest.len(), |(i, _)| i);
        if !rest[tag_len..].starts_with('$')
            || rest[..tag_len].starts_with(|c: char| c.is_ascii_digit())
        {
            return false;
        }
        let delimiter = &self.src[self.pos..self.pos + tag_len + 2];
        let body_start = self.pos + delimiter.len();
        self.pos = match self.src[body_start..].find(delimiter) {
            Some(i) => body_start + i + delimiter.len(),
            None => self.src.len(),
        };
        true
    }

    fn next_kind(&mut self) -> Option<TokenKind> {
        let c = self.bump()?;
        let kind = match c {
            c if c.is_whitespace() => {
                self.bump_while(char::is_whitespace);
                TokenKind::Whitespace
            }
            '-' if self.peek() == Some('-') => {
                self.bump_while(|c| c != '\n');
                TokenKind::Comment
            }
            '/' if self.peek() == Some('*') => {
                self.bump();
                match self.src[self.pos..].find("*/") {
                    Some(i) => self.pos += i + 2,
                    None => self.pos = self.src.len(),
                }
                TokenKind::Comment
            }
            '\'' => {
                self.quoted('\'');
                TokenKind::String
            }
            '"' => {
                self.quoted('"');
                TokenKind::QuotedIdentifier
            }
            '`' => {
                self.quoted('`');
                TokenKind::QuotedIdentifier
            }
            '[' => {
                self.quoted(']');
                TokenKind::QuotedIdentifier
            }
            '$' => {
                self.pos -= 1;
                if self.dollar_quoted() {
                    TokenKind::String
                } else {
                    self.pos += 1;
                    if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.bump_while(|c| c.is_ascii_digit());
                        TokenKind::Parameter
                    } else {
                        TokenKind::Operator
                    }
                }
            }
            '?' => TokenKind::Parameter,
            '@' => {
                self.bump_while(|c| c == '@');
                self.bump_while(is_ident_continue);
                TokenKind::Parameter
            }
            ':' if self.peek() == Some(':') => {
                self.bump();
                TokenKind::Operator
            }
            ':' if self.peek().is_some_and(is_ident_start) => {
                self.bump_while(is_ident_continue);
                TokenKind::Parameter
            }
            '#' if self.peek().is_some_and(|c| is_ident_start(c) || c == '#') => {
                self.bump_while(|c| c == '#' || is_ident_continue(c));
                TokenKind::Identifier
            }
            c if c.is_ascii_digit() => {
                self.number();
                TokenKind::Number
            }
            '.' if self.peek().is_some_and(|c| c.is_ascii_digit()) => {
                self.number();
                TokenKind::Number
            }
            c if is_ident_start(c) => {
                // String prefixes: N'…' (national), E'…' (escape), X'…'/B'…' (binary/bit).
                if matches!(c, 'N' | 'n' | 'E' | 'e' | 'X' | 'x' | 'B' | 'b')
                    && self.peek() == Some('\'')
                {
                    self.bump();
                    self.quoted('\'');
                    TokenKind::String
                } else {
                    let start = self.pos - c.len_utf8();
                    self.bump_while(is_ident_continue);
                    if is_keyword(&self.src[start..self.pos]) {
                        TokenKind::Keyword
                    } else {
                        TokenKind::Identifier
                    }
                }
            }
            '(' | ')' | ',' | ';' | '.' => TokenKind::Punctuation,
            '+' | '-' | '*' | '/' | '%' | '=' | '<' | '>' | '!' | '|' | '&' | '^' | '~' | ':' => {
                TokenKind::Operator
            }
            _ => TokenKind::Unknown,
        };
        Some(kind)
    }

    fn number(&mut self) {
        self.bump_while(|c| c.is_ascii_digit() || c == '.');
        if matches!(self.peek(), Some('e' | 'E'))
            && (self.peek_nth(1).is_some_and(|c| c.is_ascii_digit())
                || (matches!(self.peek_nth(1), Some('+' | '-'))
                    && self.peek_nth(2).is_some_and(|c| c.is_ascii_digit())))
        {
            self.bump();
            self.bump();
            self.bump_while(|c| c.is_ascii_digit());
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Splits `sql` into tokens. Concatenating all token ranges reproduces the input exactly.
pub fn tokenize(sql: &str) -> Vec<Token> {
    let mut lexer = Lexer { src: sql, pos: 0 };
    let mut tokens = Vec::new();
    loop {
        let start = lexer.pos;
        let Some(kind) = lexer.next_kind() else { break };
        tokens.push(Token {
            kind,
            range: start..lexer.pos,
        });
    }
    tokens
}

/// Byte ranges of the individual statements in a script, split on top-level `;`.
/// Ranges are trimmed of surrounding whitespace/comments; empty statements are skipped.
pub fn split_statements(sql: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut current: Option<Range<usize>> = None;
    for token in tokenize(sql) {
        match token.kind {
            TokenKind::Punctuation if &sql[token.range.clone()] == ";" => {
                out.extend(current.take());
            }
            TokenKind::Whitespace | TokenKind::Comment => {}
            _ => {
                current = Some(match current {
                    Some(r) => r.start..token.range.end,
                    None => token.range.clone(),
                });
            }
        }
    }
    out.extend(current);
    out
}

/// Coarse classification of what a statement does.
///
/// Variants are ordered by risk, so `max()` over several statements yields the riskiest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StatementKind {
    /// Nothing to execute (blank or comments only).
    Empty,
    /// Reads data only.
    Read,
    /// Session-level or unrecognised statement (`SET`, `USE`, …).
    Other,
    /// Transaction control.
    Transaction,
    /// Modifies data (`INSERT`, `UPDATE`, `DELETE`, `MERGE`, `SELECT … INTO`, …).
    Write,
    /// Modifies schema or permissions (`CREATE`, `ALTER`, `DROP`, `TRUNCATE`, `GRANT`, …).
    Ddl,
}

impl StatementKind {
    /// Whether executing it cannot change data, schema or session state.
    pub fn is_read_only(self) -> bool {
        matches!(self, StatementKind::Empty | StatementKind::Read)
    }

    /// Short label for confirmation dialogs.
    pub fn label(self) -> &'static str {
        match self {
            StatementKind::Empty => "empty",
            StatementKind::Read => "read",
            StatementKind::Other => "session/other",
            StatementKind::Transaction => "transaction control",
            StatementKind::Write => "data modification",
            StatementKind::Ddl => "schema change",
        }
    }
}

/// Classifies a whole script; returns the riskiest statement kind found.
pub fn classify(sql: &str) -> StatementKind {
    split_statements(sql)
        .into_iter()
        .map(|r| classify_statement(&sql[r]))
        .max()
        .unwrap_or(StatementKind::Empty)
}

/// Whether the server reports a meaningful row count for this single statement (data
/// modification such as `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `SELECT … INTO`, `COPY`).
/// Transaction control, DDL and procedure calls report none, or a zero that means nothing.
pub fn reports_row_count(stmt: &str) -> bool {
    if classify_statement(stmt) != StatementKind::Write {
        return false;
    }
    let first = tokenize(stmt)
        .into_iter()
        .find(|t| matches!(t.kind, TokenKind::Keyword | TokenKind::Identifier))
        .map(|t| stmt[t.range].to_ascii_uppercase());
    !matches!(first.as_deref(), Some("CALL" | "EXEC" | "EXECUTE" | "DO"))
}

fn classify_statement(stmt: &str) -> StatementKind {
    let words: Vec<String> = tokenize(stmt)
        .into_iter()
        .filter(|t| matches!(t.kind, TokenKind::Keyword | TokenKind::Identifier))
        .map(|t| stmt[t.range].to_ascii_uppercase())
        .collect();
    let Some(first) = words.first() else {
        return StatementKind::Other;
    };
    let has = |kw: &str| words.iter().any(|w| w == kw);
    let writes = ["INSERT", "UPDATE", "DELETE", "MERGE"];

    match first.as_str() {
        "SELECT" if has("INTO") => StatementKind::Write,
        "WITH" if writes.iter().any(|w| has(w)) => StatementKind::Write,
        "EXPLAIN" if words.get(1).is_some_and(|w| w == "ANALYZE") => {
            // EXPLAIN ANALYZE executes the statement.
            classify_statement(&words[2..].join(" "))
        }
        "SELECT" | "WITH" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "VALUES" | "TABLE" => {
            StatementKind::Read
        }
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "REPLACE" | "UPSERT" | "COPY" | "CALL"
        | "EXEC" | "EXECUTE" | "DO" => StatementKind::Write,
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME" | "GRANT" | "REVOKE" | "COMMENT" => {
            StatementKind::Ddl
        }
        "BEGIN" | "START" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" | "END" => {
            StatementKind::Transaction
        }
        _ => StatementKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(sql: &str) -> Vec<(TokenKind, &str)> {
        tokenize(sql)
            .into_iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| (t.kind, &sql[t.range]))
            .collect()
    }

    #[test]
    fn keyword_table_is_sorted_and_unique() {
        assert!(KEYWORDS.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn tokens_cover_input_exactly() {
        let sql = "SELECT N'héllo', [x y], `z`, \"q\"\"\" -- c\n/* b */ 1.5e-3 FROM t;";
        let rebuilt: String = tokenize(sql)
            .iter()
            .map(|t| &sql[t.range.clone()])
            .collect();
        assert_eq!(rebuilt, sql);
    }

    #[test]
    fn lexes_common_constructs() {
        use TokenKind::*;
        assert_eq!(
            kinds("select id, 'it''s' from \"T\" where x >= 10 -- note"),
            vec![
                (Keyword, "select"),
                (Identifier, "id"),
                (Punctuation, ","),
                (String, "'it''s'"),
                (Keyword, "from"),
                (QuotedIdentifier, "\"T\""),
                (Keyword, "where"),
                (Identifier, "x"),
                (Operator, ">"),
                (Operator, "="),
                (Number, "10"),
                (Comment, "-- note"),
            ]
        );
    }

    #[test]
    fn lexes_dialect_specifics() {
        use TokenKind::*;
        assert_eq!(
            kinds("$$ body; $$ $fn$ x $fn$ $1 @id @@VERSION :name ?::int #tmp"),
            vec![
                (String, "$$ body; $$"),
                (String, "$fn$ x $fn$"),
                (Parameter, "$1"),
                (Parameter, "@id"),
                (Parameter, "@@VERSION"),
                (Parameter, ":name"),
                (Parameter, "?"),
                (Operator, "::"),
                (Keyword, "int"),
                (Identifier, "#tmp"),
            ]
        );
    }

    #[test]
    fn unterminated_constructs_do_not_panic() {
        for sql in [
            "'abc",
            "/* open",
            "[x",
            "$tag$ never closed",
            "\"q",
            "1e",
            "é",
        ] {
            let rebuilt: String = tokenize(sql)
                .iter()
                .map(|t| &sql[t.range.clone()])
                .collect();
            assert_eq!(rebuilt, sql);
        }
    }

    #[test]
    fn splits_statements_ignoring_semicolons_in_strings() {
        let sql = "select ';'; \n -- c;\n update t set a=1 ;; ";
        let parts: Vec<&str> = split_statements(sql).into_iter().map(|r| &sql[r]).collect();
        assert_eq!(parts, vec!["select ';'", "update t set a=1"]);
    }

    #[test]
    fn classifies_statements() {
        use StatementKind::*;
        let cases = [
            ("SELECT * FROM t", Read),
            ("  -- only a comment", Empty),
            ("with x as (select 1) select * from x", Read),
            (
                "with d as (delete from t returning *) select * from d",
                Write,
            ),
            ("select * into backup from t", Write),
            ("explain select 1", Read),
            ("explain analyze delete from t", Write),
            ("update t set a = 1", Write),
            ("drop table t", Ddl),
            ("truncate table t", Ddl),
            ("begin", Transaction),
            ("set search_path to public", Other),
            ("select 1; drop table t", Ddl),
        ];
        for (sql, expected) in cases {
            assert_eq!(classify(sql), expected, "{sql}");
        }
    }

    #[test]
    fn row_counts_only_for_data_modification() {
        assert!(reports_row_count("UPDATE t SET a = 1"));
        assert!(reports_row_count("insert into t values (1)"));
        assert!(reports_row_count("SELECT * INTO t2 FROM t"));
        assert!(!reports_row_count("SELECT 1"));
        assert!(!reports_row_count("BEGIN TRANSACTION"));
        assert!(!reports_row_count("CREATE TABLE t (a int)"));
        assert!(!reports_row_count("CALL p()"));
    }
}
