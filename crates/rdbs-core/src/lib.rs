//! Core domain model for Zephix - Database Management Studio.
//!
//! This crate is deliberately free of GUI and I/O dependencies. It defines:
//! - connection profiles and their validation ([`profile`]),
//! - database objects shown in the explorer ([`schema`]),
//! - typed result values ([`value`]) and errors ([`error`]),
//! - the capability-based adapter traits implemented by database drivers ([`adapter`]),
//! - the typed commands and events exchanged between the UI and the worker ([`message`]),
//! - a small SQL lexer used for highlighting and safety checks ([`sql`]),
//! - read-only SQL Server Agent job summaries and `CREATE` scripts ([`agent`]).

pub mod adapter;
pub mod agent;
pub mod brand;
pub mod builder;
pub mod complete;
pub mod ddlgen;
pub mod dialect;
pub mod diff;
pub mod edit;
pub mod error;
pub mod export;
pub mod job;
pub mod message;
pub mod plan;
pub mod profile;
pub mod routine;
pub mod schema;
pub mod sql;
pub mod table;
pub mod types;
pub mod value;

pub use adapter::{
    AdapterFactory, BoxFuture, CancelHandle, Capabilities, CredentialError, CredentialStore,
    DatabaseAdapter, MemoryCredentials, SecretKind, unsupported,
};
pub use error::{DbError, DbErrorKind};
pub use message::{Command, Event, QueryOutcome, RequestId, rows_affected_message};
pub use profile::{
    ConnectionProfile, DatabaseKind, Environment, ProfileId, SshAuth, SshTunnel, SslMode,
};
pub use schema::{DbObject, ObjectKind, SchemaContents, approx_rows_label};
pub use table::{ColumnDef, ForeignKeyDef, IndexDef, TableSchema};
pub use value::{CellValue, ColumnMeta, MAX_RESULT_ROWS, ResultSet};
