//! Moving data in and out of databases: file imports, server-to-server transfer and
//! synchronisation, logical backups, and Excel exports of results. Everything here works on
//! [`rdbs_core`] types and [`rdbs_core::DatabaseAdapter`] sessions, so it runs in the
//! background worker.

pub mod backup;
pub mod import;
pub mod transfer;
pub mod xlsx;
