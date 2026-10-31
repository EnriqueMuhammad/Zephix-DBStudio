//! MongoDB adapter. Queries use the MongoDB shell syntax (see [`shell`]); documents are shown
//! as rows with one column per top-level field (nested values as JSON).
//!
//! The profile's host may also be a full connection string (`mongodb://…` or
//! `mongodb+srv://…`); the database is the one `db` refers to (default `test`).

pub mod shell;

use futures_util::TryStreamExt;
use mongodb::bson::{Bson, Document, doc};
use mongodb::options::{ClientOptions, Credential, Tls, TlsOptions};
use mongodb::{Client, IndexModel};
use rdbs_core::{
    AdapterFactory, BoxFuture, CancelHandle, Capabilities, CellValue, ColumnMeta,
    ConnectionProfile, DatabaseAdapter, DbError, DbErrorKind, DbObject, MAX_RESULT_ROWS,
    ObjectKind, QueryOutcome, ResultSet, SchemaContents, SslMode,
};

use crate::shell::{Statement, doc_arg};

/// Documents returned by `find` without an explicit `limit`.
pub const DEFAULT_LIMIT: i64 = 1_000;

/// Opens [`MongoAdapter`] sessions.
#[derive(Debug, Default, Clone, Copy)]
pub struct MongoFactory;

impl AdapterFactory for MongoFactory {
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn DatabaseAdapter>, DbError>> {
        Box::pin(async move {
            let adapter: Box<dyn DatabaseAdapter> =
                Box::new(MongoAdapter::connect(profile, password).await?);
            Ok(adapter)
        })
    }
}

/// An open MongoDB client bound to a current database.
pub struct MongoAdapter {
    client: Client,
    database: String,
    version: String,
}

fn map_error(e: mongodb::error::Error) -> DbError {
    use mongodb::error::ErrorKind;
    let (kind, code) = match e.kind.as_ref() {
        ErrorKind::Authentication { .. } => (DbErrorKind::Authentication, None),
        ErrorKind::ServerSelection { .. } | ErrorKind::Io(_) | ErrorKind::DnsResolve { .. } => {
            (DbErrorKind::Connection, None)
        }
        ErrorKind::Command(c) => {
            let kind = match c.code {
                13 | 18 => DbErrorKind::Authentication,
                // Shutdown, not primary, interrupted because the connection went away.
                91 | 10107 | 11600 | 11602 | 13435 | 13436 => DbErrorKind::Connection,
                _ => DbErrorKind::Query,
            };
            (kind, Some(c.code.to_string()))
        }
        ErrorKind::Write(_) | ErrorKind::InsertMany(_) | ErrorKind::BulkWrite(_) => {
            (DbErrorKind::Query, None)
        }
        ErrorKind::InvalidArgument { .. } => (DbErrorKind::Query, None),
        _ => (DbErrorKind::Internal, None),
    };
    let message = match e.kind.as_ref() {
        ErrorKind::Command(c) => c.message.clone(),
        _ => e.to_string(),
    };
    let err = DbError::new(kind, message);
    match code {
        Some(c) => err.with_code(c),
        None => err,
    }
}

fn query_error(message: impl Into<String>) -> DbError {
    DbError::new(DbErrorKind::Query, message)
}

/// BSON → grid cell.
pub fn cell(value: &Bson) -> CellValue {
    match value {
        Bson::Null | Bson::Undefined => CellValue::Null,
        Bson::Boolean(b) => CellValue::Bool(*b),
        Bson::Int32(i) => CellValue::Int(i64::from(*i)),
        Bson::Int64(i) => CellValue::Int(*i),
        Bson::Double(d) => CellValue::Float(*d),
        Bson::Decimal128(d) => CellValue::Decimal(d.to_string()),
        Bson::String(s) | Bson::Symbol(s) => CellValue::Text(s.clone()),
        Bson::DateTime(dt) => CellValue::Temporal(
            dt.try_to_rfc3339_string()
                .unwrap_or_else(|_| dt.timestamp_millis().to_string()),
        ),
        Bson::ObjectId(id) => CellValue::Other {
            type_name: "ObjectId".into(),
            display: id.to_hex(),
        },
        Bson::Binary(b) => CellValue::Bytes(b.bytes.clone()),
        Bson::Document(_) | Bson::Array(_) => {
            CellValue::Json(value.clone().into_relaxed_extjson().to_string())
        }
        other => CellValue::Other {
            type_name: format!("{:?}", other.element_type()),
            display: other.to_string(),
        },
    }
}

fn type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Boolean(_) => "bool",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Double(_) => "double",
        Bson::Decimal128(_) => "decimal",
        Bson::String(_) => "string",
        Bson::DateTime(_) => "date",
        Bson::ObjectId(_) => "objectId",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        _ => "mixed",
    }
}

/// Documents → grid: one column per top-level field, in first-seen order.
pub fn documents_to_result(docs: &[Document]) -> ResultSet {
    let mut names: Vec<String> = Vec::new();
    for d in docs {
        for k in d.keys() {
            if !names.contains(k) {
                names.push(k.clone());
            }
        }
    }
    let columns = names
        .iter()
        .map(|n| {
            let ty = docs
                .iter()
                .find_map(|d| d.get(n).filter(|v| !matches!(v, Bson::Null)))
                .map_or("mixed", type_name);
            ColumnMeta::new(n.clone(), ty)
        })
        .collect();
    let rows = docs
        .iter()
        .map(|d| {
            names
                .iter()
                .map(|n| d.get(n).map_or(CellValue::Null, cell))
                .collect()
        })
        .collect();
    ResultSet { columns, rows }
}

fn values_to_result(name: &str, values: &[Bson]) -> ResultSet {
    ResultSet {
        columns: vec![ColumnMeta::new(
            name,
            values.first().map_or("mixed", type_name),
        )],
        rows: values.iter().map(|v| vec![cell(v)]).collect(),
    }
}

fn int_arg(args: &[(String, Vec<Bson>)], name: &str) -> Option<i64> {
    args.iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, a)| match a.first() {
            Some(Bson::Int32(i)) => Some(i64::from(*i)),
            Some(Bson::Int64(i)) => Some(*i),
            Some(Bson::Double(d)) => Some(*d as i64),
            _ => None,
        })
}

fn doc_modifier(args: &[(String, Vec<Bson>)], name: &str) -> Option<Document> {
    args.iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, a)| match a.first() {
            Some(Bson::Document(d)) => Some(d.clone()),
            _ => None,
        })
}

impl MongoAdapter {
    /// Connects and checks the server with `buildInfo`.
    pub async fn connect(
        profile: &ConnectionProfile,
        password: Option<&str>,
    ) -> Result<Self, DbError> {
        let host = profile.host.trim();
        let uri = if host.starts_with("mongodb://") || host.starts_with("mongodb+srv://") {
            host.to_owned()
        } else {
            format!("mongodb://{host}:{}", profile.port)
        };
        let mut options = ClientOptions::parse(&uri).await.map_err(|e| {
            let mut err = map_error(e);
            err.kind = DbErrorKind::Connection;
            err
        })?;
        options.app_name = Some(rdbs_core::brand::APP_NAME.into());
        options.server_selection_timeout = Some(std::time::Duration::from_secs(8));
        options.connect_timeout = Some(std::time::Duration::from_secs(8));
        if !profile.username.trim().is_empty() {
            options.credential = Some(
                Credential::builder()
                    .username(profile.username.trim().to_owned())
                    .password(password.map(str::to_owned))
                    .source(Some(
                        options
                            .credential
                            .as_ref()
                            .and_then(|c| c.source.clone())
                            .unwrap_or_else(|| "admin".into()),
                    ))
                    .build(),
            );
        }
        match profile.ssl_mode {
            SslMode::Require => {
                options.tls = Some(Tls::Enabled(
                    TlsOptions::builder()
                        .allow_invalid_certificates(true)
                        .build(),
                ));
            }
            SslMode::VerifyFull => options.tls = Some(Tls::Enabled(TlsOptions::default())),
            SslMode::Disable => options.tls = Some(Tls::Disabled),
            SslMode::Prefer => {}
        }
        let database = if profile.database.trim().is_empty() {
            options
                .default_database
                .clone()
                .unwrap_or_else(|| "test".into())
        } else {
            profile.database.trim().to_owned()
        };
        let client = Client::with_options(options).map_err(map_error)?;
        let info = client
            .database("admin")
            .run_command(doc! {"buildInfo": 1})
            .await
            .map_err(map_error)?;
        let version = format!("MongoDB {}", info.get_str("version").unwrap_or("?"));
        Ok(Self {
            client,
            database,
            version,
        })
    }

    async fn run(&mut self, script: &str) -> Result<QueryOutcome, DbError> {
        let mut outcome = QueryOutcome::default();
        for statement in shell::split_statements(script) {
            let parsed = shell::parse(&statement).map_err(query_error)?;
            self.run_one(parsed, &mut outcome).await?;
        }
        Ok(outcome)
    }

    async fn collect(
        &self,
        mut cursor: mongodb::Cursor<Document>,
        outcome: &mut QueryOutcome,
    ) -> Result<Vec<Document>, DbError> {
        let mut docs = Vec::new();
        while let Some(d) = cursor.try_next().await.map_err(map_error)? {
            if docs.len() >= MAX_RESULT_ROWS {
                outcome.truncated = true;
                break;
            }
            docs.push(d);
        }
        Ok(docs)
    }

    async fn run_one(
        &mut self,
        statement: Statement,
        outcome: &mut QueryOutcome,
    ) -> Result<(), DbError> {
        let db = self.client.database(&self.database);
        match statement {
            Statement::ShowDatabases => {
                let names = self.client.list_database_names().await.map_err(map_error)?;
                let values: Vec<Bson> = names.into_iter().map(Bson::String).collect();
                outcome.push_result(values_to_result("database", &values));
            }
            Statement::ShowCollections => {
                let mut names = db.list_collection_names().await.map_err(map_error)?;
                names.sort();
                let values: Vec<Bson> = names.into_iter().map(Bson::String).collect();
                outcome
                    .results
                    .push(values_to_result("collection", &values));
            }
            Statement::Use(name) => {
                outcome.messages.push(format!("switched to db {name}"));
                self.database = name;
            }
            Statement::Database { method, args } => {
                let command = match method.as_str() {
                    "runCommand" => doc_arg(&args, 0).map_err(query_error)?,
                    "adminCommand" => {
                        let reply = self
                            .client
                            .database("admin")
                            .run_command(doc_arg(&args, 0).map_err(query_error)?)
                            .await
                            .map_err(map_error)?;
                        outcome.push_result(documents_to_result(&[reply]));
                        return Ok(());
                    }
                    "stats" => doc! {"dbStats": 1},
                    "createCollection" => {
                        let Some(Bson::String(name)) = args.first() else {
                            return Err(query_error("createCollection needs a name"));
                        };
                        db.create_collection(name).await.map_err(map_error)?;
                        outcome.messages.push(format!("created collection {name}"));
                        return Ok(());
                    }
                    "dropDatabase" => {
                        db.drop().await.map_err(map_error)?;
                        outcome
                            .messages
                            .push(format!("dropped database {}", self.database));
                        return Ok(());
                    }
                    "getCollectionNames" => {
                        return Box::pin(self.run_one(Statement::ShowCollections, outcome)).await;
                    }
                    other => return Err(query_error(format!("unsupported db.{other}()"))),
                };
                let reply = db.run_command(command).await.map_err(map_error)?;
                outcome.push_result(documents_to_result(&[reply]));
            }
            Statement::Collection {
                collection,
                method,
                args,
                modifiers,
            } => {
                let coll = db.collection::<Document>(&collection);
                match method.as_str() {
                    "find" | "findOne" => {
                        let filter = doc_arg(&args, 0).map_err(query_error)?;
                        let mut find = coll.find(filter);
                        if let Some(p) = doc_arg(&args, 1)
                            .ok()
                            .filter(|d| !d.is_empty())
                            .or_else(|| doc_modifier(&modifiers, "projection"))
                        {
                            find = find.projection(p);
                        }
                        if let Some(s) = doc_modifier(&modifiers, "sort") {
                            find = find.sort(s);
                        }
                        if let Some(n) = int_arg(&modifiers, "skip") {
                            find = find.skip(u64::try_from(n).unwrap_or(0));
                        }
                        let explicit = int_arg(&modifiers, "limit");
                        let limit = if method == "findOne" {
                            1
                        } else {
                            explicit.unwrap_or(DEFAULT_LIMIT)
                        };
                        find = find.limit(limit);
                        let cursor = find.await.map_err(map_error)?;
                        let docs = self.collect(cursor, outcome).await?;
                        if explicit.is_none()
                            && method == "find"
                            && docs.len() as i64 == DEFAULT_LIMIT
                        {
                            outcome.messages.push(format!(
                                "showing the first {DEFAULT_LIMIT} documents; add .limit(n) for more"
                            ));
                        }
                        outcome.push_result(documents_to_result(&docs));
                    }
                    "aggregate" => {
                        let pipeline: Vec<Document> = match args.first() {
                            Some(Bson::Array(stages)) => stages
                                .iter()
                                .map(|s| match s {
                                    Bson::Document(d) => Ok(d.clone()),
                                    other => Err(query_error(format!(
                                        "pipeline stages are documents, got {other}"
                                    ))),
                                })
                                .collect::<Result<_, _>>()?,
                            _ => args
                                .iter()
                                .filter_map(|a| a.as_document().cloned())
                                .collect(),
                        };
                        let cursor = coll.aggregate(pipeline).await.map_err(map_error)?;
                        let docs = self.collect(cursor, outcome).await?;
                        outcome.push_result(documents_to_result(&docs));
                    }
                    "countDocuments" | "count" => {
                        let n = coll
                            .count_documents(doc_arg(&args, 0).map_err(query_error)?)
                            .await
                            .map_err(map_error)?;
                        outcome
                            .results
                            .push(values_to_result("count", &[Bson::Int64(n as i64)]));
                    }
                    "estimatedDocumentCount" => {
                        let n = coll.estimated_document_count().await.map_err(map_error)?;
                        outcome
                            .results
                            .push(values_to_result("count", &[Bson::Int64(n as i64)]));
                    }
                    "distinct" => {
                        let Some(Bson::String(field)) = args.first() else {
                            return Err(query_error("distinct needs a field name"));
                        };
                        let values = coll
                            .distinct(field, doc_arg(&args, 1).map_err(query_error)?)
                            .await
                            .map_err(map_error)?;
                        outcome.push_result(values_to_result(field, &values));
                    }
                    "insertOne" => {
                        let r = coll
                            .insert_one(doc_arg(&args, 0).map_err(query_error)?)
                            .await
                            .map_err(map_error)?;
                        outcome.record_rows_affected(1);
                        outcome.messages.push(format!("inserted {}", r.inserted_id));
                    }
                    "insertMany" => {
                        let Some(Bson::Array(items)) = args.first() else {
                            return Err(query_error("insertMany needs an array of documents"));
                        };
                        let docs: Vec<Document> = items
                            .iter()
                            .filter_map(|d| d.as_document().cloned())
                            .collect();
                        let r = coll.insert_many(docs).await.map_err(map_error)?;
                        outcome.record_rows_affected(r.inserted_ids.len() as u64);
                        outcome
                            .messages
                            .push(format!("inserted {} document(s)", r.inserted_ids.len()));
                    }
                    "updateOne" | "updateMany" | "replaceOne" => {
                        let filter = doc_arg(&args, 0).map_err(query_error)?;
                        let change = doc_arg(&args, 1).map_err(query_error)?;
                        let upsert = doc_arg(&args, 2)
                            .ok()
                            .and_then(|o| o.get_bool("upsert").ok())
                            .unwrap_or(false);
                        let r = match method.as_str() {
                            "updateOne" => coll.update_one(filter, change).upsert(upsert).await,
                            "updateMany" => coll.update_many(filter, change).upsert(upsert).await,
                            _ => coll.replace_one(filter, change).upsert(upsert).await,
                        }
                        .map_err(map_error)?;
                        outcome.record_rows_affected(r.modified_count);
                        outcome.messages.push(format!(
                            "matched {}, modified {}{}",
                            r.matched_count,
                            r.modified_count,
                            r.upserted_id
                                .map(|id| format!(", upserted {id}"))
                                .unwrap_or_default()
                        ));
                    }
                    "deleteOne" | "deleteMany" => {
                        let filter = doc_arg(&args, 0).map_err(query_error)?;
                        let r = if method == "deleteOne" {
                            coll.delete_one(filter).await
                        } else {
                            coll.delete_many(filter).await
                        }
                        .map_err(map_error)?;
                        outcome.record_rows_affected(r.deleted_count);
                        outcome
                            .messages
                            .push(format!("deleted {} document(s)", r.deleted_count));
                    }
                    "drop" => {
                        coll.drop().await.map_err(map_error)?;
                        outcome
                            .messages
                            .push(format!("dropped collection {collection}"));
                    }
                    "createIndex" => {
                        let keys = doc_arg(&args, 0).map_err(query_error)?;
                        let opts = doc_arg(&args, 1).map_err(query_error)?;
                        let options = mongodb::options::IndexOptions::builder()
                            .unique(opts.get_bool("unique").ok())
                            .name(opts.get_str("name").ok().map(str::to_owned))
                            .build();
                        let r = coll
                            .create_index(IndexModel::builder().keys(keys).options(options).build())
                            .await
                            .map_err(map_error)?;
                        outcome
                            .messages
                            .push(format!("created index {}", r.index_name));
                    }
                    "getIndexes" => {
                        let mut cursor = coll.list_indexes().await.map_err(map_error)?;
                        let mut docs = Vec::new();
                        while let Some(ix) = cursor.try_next().await.map_err(map_error)? {
                            let mut d = doc! {"key": ix.keys};
                            if let Some(o) = ix.options {
                                if let Some(n) = o.name {
                                    d.insert("name", n);
                                }
                                if let Some(u) = o.unique {
                                    d.insert("unique", u);
                                }
                            }
                            docs.push(d);
                        }
                        outcome.push_result(documents_to_result(&docs));
                    }
                    "dropIndex" => {
                        let Some(Bson::String(name)) = args.first() else {
                            return Err(query_error("dropIndex needs an index name"));
                        };
                        coll.drop_index(name).await.map_err(map_error)?;
                        outcome.messages.push(format!("dropped index {name}"));
                    }
                    other => return Err(query_error(format!("unsupported method {other}()"))),
                }
            }
        }
        Ok(())
    }

    async fn tree(&mut self) -> Result<Vec<DbObject>, DbError> {
        let mut names = self.client.list_database_names().await.map_err(map_error)?;
        if !names.contains(&self.database) {
            names.push(self.database.clone());
        }
        names.sort();
        let db = self.client.database(&self.database);
        let mut specs = db.list_collections().await.map_err(map_error)?;
        let mut contents = SchemaContents::default();
        let mut count = 0;
        while let Some(spec) = specs.try_next().await.map_err(map_error)? {
            let is_view = matches!(spec.collection_type, mongodb::results::CollectionType::View);
            let coll = db.collection::<Document>(&spec.name);
            // Field names of one sample document help autocomplete.
            let mut obj = DbObject::new(
                spec.name.clone(),
                if is_view {
                    ObjectKind::View
                } else {
                    ObjectKind::Table
                },
            );
            if count < 100 {
                if let Ok(Some(sample)) = coll.find_one(doc! {}).await {
                    obj = obj.with_children(
                        sample
                            .iter()
                            .map(|(k, v)| {
                                DbObject::new(k.clone(), ObjectKind::Column)
                                    .with_detail(type_name(v))
                            })
                            .collect(),
                    );
                }
                if !is_view
                    && let Ok(n) = coll.estimated_document_count().await
                    && let Some(label) = rdbs_core::approx_rows_label(n as i64)
                {
                    obj = obj.with_detail(label.replace("rows", "docs"));
                }
            }
            count += 1;
            if is_view {
                contents.views.push(obj);
            } else {
                contents.tables.push(obj);
            }
        }
        contents.tables.sort_by(|a, b| a.name.cmp(&b.name));
        contents.views.sort_by(|a, b| a.name.cmp(&b.name));
        let mut folders = contents.into_folders();
        if let Some(f) = folders.first_mut() {
            f.name = "Collections".into();
        }
        let mut folders = Some(folders);
        Ok(names
            .into_iter()
            .map(|name| {
                if name == self.database {
                    DbObject::new(name, ObjectKind::Database)
                        .with_children(folders.take().unwrap_or_default())
                } else {
                    DbObject::new(name, ObjectKind::Database)
                        .with_detail(rdbs_core::schema::NOT_LOADED)
                }
            })
            .collect())
    }
}

impl DatabaseAdapter for MongoAdapter {
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

    fn ping(&mut self) -> BoxFuture<'_, Result<(), DbError>> {
        let database = self.database.clone();
        Box::pin(async move {
            self.client
                .database(&database)
                .run_command(doc! { "ping": 1 })
                .await
                .map_err(map_error)?;
            Ok(())
        })
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

    fn close(&mut self) -> BoxFuture<'_, ()> {
        let client = self.client.clone();
        Box::pin(async move { client.shutdown().await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_flatten_to_columns() {
        let docs = vec![
            doc! {"_id": 1, "name": "Ayu", "tags": ["a"]},
            doc! {"_id": 2, "city": "Jakarta"},
        ];
        let rs = documents_to_result(&docs);
        let names: Vec<&str> = rs.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["_id", "name", "tags", "city"]);
        assert_eq!(rs.columns[2].type_name, "array");
        assert_eq!(rs.rows[0][2], CellValue::Json("[\"a\"]".into()));
        assert_eq!(rs.rows[1][1], CellValue::Null);
        assert_eq!(rs.rows[1][3], CellValue::Text("Jakarta".into()));
    }
}
