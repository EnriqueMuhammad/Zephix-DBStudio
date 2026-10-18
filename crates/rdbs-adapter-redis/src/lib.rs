//! Redis adapter.
//!
//! The editor accepts one Redis command per line, as in `redis-cli` (`GET user:1`,
//! `HGETALL "my key"`). Lines starting with `#` or `--` are comments. The pseudo-command
//! `INSPECT key` shows a key's value whatever its type; the explorer uses it to open keys.
//!
//! The profile's database is the logical database index (`0`, or `db0`).

use rdbs_core::{
    AdapterFactory, BoxFuture, CancelHandle, Capabilities, CellValue, ColumnMeta,
    ConnectionProfile, DatabaseAdapter, DbError, DbErrorKind, DbObject, ObjectKind, QueryOutcome,
    ResultSet, SslMode,
};
use redis::Value;
use redis::aio::MultiplexedConnection;

/// Keys listed per database in the explorer.
const MAX_KEYS: usize = 500;

/// Opens [`RedisAdapter`] sessions.
#[derive(Debug, Default, Clone, Copy)]
pub struct RedisFactory;

impl AdapterFactory for RedisFactory {
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn DatabaseAdapter>, DbError>> {
        Box::pin(async move {
            let adapter: Box<dyn DatabaseAdapter> =
                Box::new(RedisAdapter::connect(profile, password).await?);
            Ok(adapter)
        })
    }
}

/// An open Redis connection.
pub struct RedisAdapter {
    conn: MultiplexedConnection,
    version: String,
    db: i64,
}

/// `db3` / `3` → 3.
pub fn parse_db(text: &str) -> i64 {
    text.trim().trim_start_matches("db").parse().unwrap_or(0)
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Connection URL for a profile (`rediss://` when TLS is required).
pub fn url(profile: &ConnectionProfile, password: Option<&str>) -> String {
    let scheme = match profile.ssl_mode {
        SslMode::Require | SslMode::VerifyFull => "rediss",
        _ => "redis",
    };
    let auth = match (profile.username.trim(), password.filter(|p| !p.is_empty())) {
        ("", None) => String::new(),
        ("", Some(p)) => format!(":{}@", percent_encode(p)),
        (u, None) => format!("{}@", percent_encode(u)),
        (u, Some(p)) => format!("{}:{}@", percent_encode(u), percent_encode(p)),
    };
    let insecure = if profile.ssl_mode == SslMode::Require {
        "#insecure"
    } else {
        ""
    };
    format!(
        "{scheme}://{auth}{}:{}/{}{insecure}",
        profile.host,
        profile.port,
        parse_db(&profile.database)
    )
}

fn ensure_crypto_provider() {
    // Feature unification can leave rustls without a process-wide default provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub(crate) fn map_error(e: redis::RedisError) -> DbError {
    let kind = if e.is_io_error() || e.is_connection_refusal() || e.is_timeout() {
        DbErrorKind::Connection
    } else if matches!(e.kind(), redis::ErrorKind::AuthenticationFailed)
        || e.code()
            .is_some_and(|c| c == "NOAUTH" || c == "WRONGPASS" || c == "NOPERM")
    {
        DbErrorKind::Authentication
    } else {
        DbErrorKind::Query
    };
    let mut err = DbError::new(
        kind,
        e.detail().map_or_else(|| e.to_string(), str::to_owned),
    );
    if let Some(code) = e.code() {
        err = err.with_code(code);
    }
    err
}

/// Splits a command line into arguments: whitespace-separated, with `"…"` (backslash
/// escapes) and `'…'` quoting.
pub fn split_args(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else { break };
        let mut arg = String::new();
        if first == '"' || first == '\'' {
            chars.next();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == first {
                    closed = true;
                    break;
                }
                if c == '\\' && first == '"' {
                    match chars.next() {
                        Some('n') => arg.push('\n'),
                        Some('t') => arg.push('\t'),
                        Some(other) => arg.push(other),
                        None => break,
                    }
                } else {
                    arg.push(c);
                }
            }
            if !closed {
                return Err("unterminated quote".into());
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                arg.push(c);
                chars.next();
            }
        }
        out.push(arg);
    }
    Ok(out)
}

fn text(bytes: &[u8]) -> CellValue {
    match std::str::from_utf8(bytes) {
        Ok(s) => CellValue::Text(s.to_owned()),
        Err(_) => CellValue::Bytes(bytes.to_vec()),
    }
}

/// A scalar rendering of any value.
fn cell(value: &Value) -> CellValue {
    match value {
        Value::Nil => CellValue::Null,
        Value::Int(i) => CellValue::Int(*i),
        Value::BulkString(b) => text(b),
        Value::SimpleString(s) => CellValue::Text(s.clone()),
        Value::Okay => CellValue::Text("OK".into()),
        Value::Double(d) => CellValue::Float(*d),
        Value::Boolean(b) => CellValue::Bool(*b),
        Value::VerbatimString { text, .. } => CellValue::Text(text.clone()),
        other => CellValue::Text(display(other)),
    }
}

fn display(value: &Value) -> String {
    match value {
        Value::Array(items) | Value::Set(items) => format!(
            "[{}]",
            items.iter().map(display).collect::<Vec<_>>().join(", ")
        ),
        Value::Map(pairs) => format!(
            "{{{}}}",
            pairs
                .iter()
                .map(|(k, v)| format!("{}: {}", display(k), display(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => match cell(other) {
            CellValue::Null => "(nil)".into(),
            c => c.to_string(),
        },
    }
}

/// Commands whose flat array reply is a list of field/value pairs.
fn pairs_reply(command: &str) -> bool {
    matches!(
        command,
        "HGETALL" | "CONFIG" | "XINFO" | "HRANDFIELD" | "INSPECT_HASH"
    )
}

/// Turns a reply into a result grid.
pub fn to_result(command: &str, value: &Value) -> ResultSet {
    let rows_of = |pairs: Vec<(CellValue, CellValue)>, k: &str, v: &str| ResultSet {
        columns: vec![ColumnMeta::new(k, "string"), ColumnMeta::new(v, "value")],
        rows: pairs.into_iter().map(|(a, b)| vec![a, b]).collect(),
    };
    match value {
        Value::Map(pairs) => rows_of(
            pairs.iter().map(|(k, v)| (cell(k), cell(v))).collect(),
            "field",
            "value",
        ),
        Value::Array(items) | Value::Set(items) if pairs_reply(command) && items.len() % 2 == 0 => {
            rows_of(
                items
                    .chunks(2)
                    .map(|p| (cell(&p[0]), cell(&p[1])))
                    .collect(),
                "field",
                "value",
            )
        }
        Value::Array(items) | Value::Set(items) => ResultSet {
            columns: vec![
                ColumnMeta::new("#", "int"),
                ColumnMeta::new("value", "value"),
            ],
            rows: items
                .iter()
                .enumerate()
                .map(|(i, v)| vec![CellValue::Int(i as i64 + 1), cell(v)])
                .collect(),
        },
        // INFO replies are "key:value" lines.
        Value::BulkString(b) if command == "INFO" => {
            let text = String::from_utf8_lossy(b);
            rows_of(
                text.lines()
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (CellValue::Text(k.into()), CellValue::Text(v.trim().into())))
                    .collect(),
                "key",
                "value",
            )
        }
        scalar => ResultSet {
            columns: vec![ColumnMeta::new("value", "value")],
            rows: vec![vec![cell(scalar)]],
        },
    }
}

impl RedisAdapter {
    /// Connects and reads the server version.
    pub async fn connect(
        profile: &ConnectionProfile,
        password: Option<&str>,
    ) -> Result<Self, DbError> {
        ensure_crypto_provider();
        let client = redis::Client::open(url(profile, password)).map_err(|e| {
            let mut err = map_error(e);
            err.kind = DbErrorKind::Connection;
            err
        })?;
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(map_error)?;
        let info: Value = redis::cmd("INFO")
            .arg("server")
            .query_async(&mut conn)
            .await
            .map_err(map_error)?;
        let info = match &info {
            Value::BulkString(b) => String::from_utf8_lossy(b).into_owned(),
            Value::VerbatimString { text, .. } => text.clone(),
            other => display(other),
        };
        let version = info
            .lines()
            .find_map(|l| l.strip_prefix("redis_version:"))
            .map_or_else(|| "Redis".to_owned(), |v| format!("Redis {}", v.trim()));
        Ok(Self {
            conn,
            version,
            db: parse_db(&profile.database),
        })
    }

    async fn query(&mut self, args: &[String]) -> Result<Value, DbError> {
        let mut cmd = redis::cmd(&args[0]);
        for a in &args[1..] {
            cmd.arg(a);
        }
        cmd.query_async(&mut self.conn).await.map_err(map_error)
    }

    /// `INSPECT key`: the value of a key whatever its type.
    async fn inspect(&mut self, key: &str) -> Result<(ResultSet, String), DbError> {
        let k = key.to_owned();
        let kind = match self.query(&["TYPE".into(), k.clone()]).await? {
            Value::SimpleString(s) => s,
            Value::Okay => "string".into(),
            other => display(&other),
        };
        let ttl = match self.query(&["TTL".into(), k.clone()]).await? {
            Value::Int(-1) => "no expiry".to_owned(),
            Value::Int(-2) => "missing".to_owned(),
            Value::Int(n) => format!("expires in {n} s"),
            _ => String::new(),
        };
        let (command, args): (&str, Vec<String>) = match kind.as_str() {
            "string" => ("GET", vec!["GET".into(), k]),
            "hash" => ("INSPECT_HASH", vec!["HGETALL".into(), k]),
            "list" => ("LRANGE", vec!["LRANGE".into(), k, "0".into(), "999".into()]),
            "set" => ("SMEMBERS", vec!["SMEMBERS".into(), k]),
            "zset" => (
                "ZRANGE",
                vec![
                    "ZRANGE".into(),
                    k,
                    "0".into(),
                    "999".into(),
                    "WITHSCORES".into(),
                ],
            ),
            "stream" => (
                "XRANGE",
                vec![
                    "XRANGE".into(),
                    k,
                    "-".into(),
                    "+".into(),
                    "COUNT".into(),
                    "1000".into(),
                ],
            ),
            "none" => {
                return Ok((
                    to_result("GET", &Value::Nil),
                    format!("{key}: key does not exist"),
                ));
            }
            other => {
                return Err(DbError::new(
                    DbErrorKind::Unsupported,
                    format!("keys of type {other} cannot be shown"),
                ));
            }
        };
        let value = self.query(&args).await?;
        let mut rs = to_result(command, &value);
        if kind == "zset"
            && let Value::Array(items) = &value
        {
            rs = ResultSet {
                columns: vec![
                    ColumnMeta::new("member", "string"),
                    ColumnMeta::new("score", "double"),
                ],
                rows: items
                    .chunks(2)
                    .map(|p| vec![cell(&p[0]), cell(&p[1])])
                    .collect(),
            };
        }
        Ok((rs, format!("{key}: {kind}, {ttl}")))
    }

    async fn run(&mut self, script: &str) -> Result<QueryOutcome, DbError> {
        let mut outcome = QueryOutcome::default();
        for (n, line) in script.lines().enumerate() {
            let line = line.trim().trim_end_matches(';');
            if line.is_empty() || line.starts_with('#') || line.starts_with("--") {
                continue;
            }
            let args = split_args(line)
                .map_err(|e| DbError::new(DbErrorKind::Query, format!("line {}: {e}", n + 1)))?;
            if args.is_empty() {
                continue;
            }
            let command = args[0].to_ascii_uppercase();
            if command == "INSPECT" {
                let key = args
                    .get(1)
                    .ok_or_else(|| DbError::new(DbErrorKind::Query, "INSPECT needs a key name"))?;
                let (rs, message) = self.inspect(key).await?;
                outcome.messages.push(message);
                outcome.results.push(rs);
                continue;
            }
            let value = self.query(&args).await?;
            match &value {
                Value::Okay => outcome.messages.push(format!("{command}: OK")),
                Value::Int(i) if command.starts_with("DEL") || command.ends_with("SET") => {
                    outcome.rows_affected = Some(u64::try_from(*i).unwrap_or(0));
                    outcome.results.push(to_result(&command, &value));
                }
                _ => outcome.results.push(to_result(&command, &value)),
            }
        }
        Ok(outcome)
    }

    async fn tree(&mut self) -> Result<Vec<DbObject>, DbError> {
        let databases = match self
            .query(&["CONFIG".into(), "GET".into(), "databases".into()])
            .await
        {
            Ok(Value::Array(items)) if items.len() == 2 => display(&items[1]).parse().unwrap_or(16),
            Ok(Value::Map(pairs)) => pairs
                .first()
                .map(|(_, v)| display(v).parse().unwrap_or(16))
                .unwrap_or(16),
            _ => 16, // CONFIG may be disabled (managed services)
        };
        let keyspace = match self.query(&["INFO".into(), "keyspace".into()]).await? {
            Value::BulkString(b) => String::from_utf8_lossy(&b).into_owned(),
            Value::VerbatimString { text, .. } => text,
            other => display(&other),
        };
        let key_count = |db: i64| -> i64 {
            keyspace
                .lines()
                .find_map(|l| l.strip_prefix(&format!("db{db}:keys=")))
                .and_then(|rest| rest.split(',').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0)
        };
        let mut keys: Vec<String> = Vec::new();
        let mut cursor = "0".to_owned();
        loop {
            let reply = self
                .query(&["SCAN".into(), cursor.clone(), "COUNT".into(), "1000".into()])
                .await?;
            let Value::Array(parts) = reply else { break };
            let next = parts.first().map(display).unwrap_or_else(|| "0".into());
            if let Some(Value::Array(found)) = parts.get(1) {
                keys.extend(found.iter().map(display));
            }
            if next == "0" || keys.len() >= MAX_KEYS {
                break;
            }
            cursor = next;
        }
        keys.sort();
        let truncated = keys.len() > MAX_KEYS;
        keys.truncate(MAX_KEYS);
        Ok((0..databases.max(self.db + 1))
            .map(|db| {
                let name = format!("db{db}");
                let count = key_count(db);
                let label = rdbs_core::approx_rows_label(count)
                    .map(|l| l.replace("rows", "keys"))
                    .unwrap_or_default();
                if db == self.db {
                    let mut folder = DbObject::new("Keys", ObjectKind::Folder).with_children(
                        keys.iter()
                            .map(|k| DbObject::new(k.clone(), ObjectKind::Table))
                            .collect(),
                    );
                    if truncated {
                        folder = folder.with_detail(format!("first {MAX_KEYS}"));
                    }
                    DbObject::new(name, ObjectKind::Database)
                        .with_detail(label)
                        .with_children(vec![folder])
                } else if count == 0 {
                    DbObject::new(name, ObjectKind::Database).with_detail("empty")
                } else {
                    DbObject::new(name, ObjectKind::Database)
                        .with_detail(rdbs_core::schema::NOT_LOADED)
                }
            })
            .filter(|d| d.name == format!("db{}", self.db) || d.detail.as_deref() != Some("empty"))
            .collect())
    }
}

impl DatabaseAdapter for RedisAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            query_cancellation: true,
            streaming_results: false,
            transactions: false,
            schemas: false,
            multiple_databases: true,
            explain_plan: false,
        }
    }

    fn server_version(&self) -> &str {
        &self.version
    }

    fn load_objects(&mut self) -> BoxFuture<'_, Result<Vec<DbObject>, DbError>> {
        Box::pin(self.tree())
    }

    fn execute<'a>(
        &'a mut self,
        sql: &'a str,
        cancel: CancelHandle,
    ) -> BoxFuture<'a, Result<QueryOutcome, DbError>> {
        Box::pin(async move {
            tokio::select! {
                result = self.run(sql) => result,
                () = cancel.cancelled() => Err(DbError::cancelled()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_redis_cli() {
        assert_eq!(split_args("SET a 1").unwrap(), ["SET", "a", "1"]);
        assert_eq!(
            split_args(r#"SET "my key" "line\nbreak" 'it''s'"#).unwrap(),
            ["SET", "my key", "line\nbreak", "it", "s"]
        );
        assert!(split_args("GET \"open").is_err());
        assert!(split_args("   ").unwrap().is_empty());
    }

    #[test]
    fn urls_encode_credentials_and_db() {
        let mut p = ConnectionProfile::new(rdbs_core::DatabaseKind::Redis);
        p.host = "cache".into();
        p.port = 6379;
        p.database = "db2".into();
        p.ssl_mode = SslMode::Disable;
        assert_eq!(url(&p, Some("p@ss")), "redis://:p%40ss@cache:6379/2");
        p.username = "app".into();
        p.ssl_mode = SslMode::Require;
        assert_eq!(url(&p, None), "rediss://app@cache:6379/2#insecure");
    }

    #[test]
    fn replies_become_grids() {
        let hash = Value::Array(vec![
            Value::BulkString(b"name".to_vec()),
            Value::BulkString(b"Ayu".to_vec()),
        ]);
        let rs = to_result("HGETALL", &hash);
        assert_eq!(rs.columns[0].name, "field");
        assert_eq!(
            rs.rows[0],
            vec![
                CellValue::Text("name".into()),
                CellValue::Text("Ayu".into())
            ]
        );
        let list = to_result("LRANGE", &Value::Array(vec![Value::Int(1), Value::Nil]));
        assert_eq!(list.rows[1], vec![CellValue::Int(2), CellValue::Null]);
        let info = to_result(
            "INFO",
            &Value::BulkString(b"# Server\r\nredis_version:7.4.0\r\n".to_vec()),
        );
        assert_eq!(info.rows[0][1], CellValue::Text("7.4.0".into()));
        assert_eq!(to_result("GET", &Value::Nil).rows[0][0], CellValue::Null);
    }
}
