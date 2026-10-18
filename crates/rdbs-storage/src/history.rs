//! Query history: every executed script with its outcome, newest first.

use rdbs_core::ProfileId;
use rusqlite::params;

use crate::{ProfileStore, Result};

/// Entries kept; older ones are pruned on insert.
pub const HISTORY_LIMIT: i64 = 5_000;

/// Outcome of a recorded execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryStatus {
    /// Finished successfully.
    Ok,
    /// Failed with an error.
    Error,
    /// Cancelled by the user.
    Cancelled,
}

impl HistoryStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "ok" => Self::Ok,
            "cancelled" => Self::Cancelled,
            _ => Self::Error,
        }
    }
}

/// Data recorded for one execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewHistoryEntry {
    /// Connection the script ran on.
    pub profile_id: ProfileId,
    /// Connection name at the time (kept even if the profile is later renamed or deleted).
    pub profile_name: String,
    /// Database the script ran in, when it was not the profile's default.
    pub database: Option<String>,
    /// Executed SQL.
    pub sql: String,
    /// Wall-clock duration in milliseconds.
    pub elapsed_ms: u64,
    /// Outcome.
    pub status: HistoryStatus,
    /// Rows returned (all result sets) or affected.
    pub rows: Option<u64>,
    /// Error message for failed runs.
    pub error: Option<String>,
}

/// A stored history entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Row id (monotonic).
    pub id: i64,
    /// Local time of execution, `YYYY-MM-DD HH:MM:SS`.
    pub executed_at: String,
    /// The recorded data.
    pub entry: NewHistoryEntry,
}

pub(crate) const CREATE_HISTORY: &str = "
    CREATE TABLE IF NOT EXISTS query_history (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        profile_id   TEXT NOT NULL,
        profile_name TEXT NOT NULL,
        sql          TEXT NOT NULL,
        executed_at  TEXT NOT NULL DEFAULT (datetime('now', 'localtime')),
        elapsed_ms   INTEGER NOT NULL,
        status       TEXT NOT NULL,
        rows         INTEGER,
        error        TEXT
    );";

impl ProfileStore {
    /// Records an execution and prunes entries beyond [`HISTORY_LIMIT`].
    pub fn record_query(&self, e: &NewHistoryEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO query_history
                 (profile_id, profile_name, sql, elapsed_ms, status, rows, error, database)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                e.profile_id.to_string(),
                e.profile_name,
                e.sql,
                i64::try_from(e.elapsed_ms).unwrap_or(i64::MAX),
                e.status.as_str(),
                e.rows.and_then(|r| i64::try_from(r).ok()),
                e.error,
                e.database,
            ],
        )?;
        self.conn.execute(
            "DELETE FROM query_history WHERE id <= (SELECT id FROM query_history
                 ORDER BY id DESC LIMIT 1 OFFSET ?1)",
            params![HISTORY_LIMIT],
        )?;
        Ok(())
    }

    /// Newest entries first; `filter` matches SQL text or connection name (case-insensitive).
    pub fn recent_queries(&self, filter: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
        let pattern = format!(
            "%{}%",
            filter
                .trim()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut stmt = self.conn.prepare(
            "SELECT id, executed_at, profile_id, profile_name, sql, elapsed_ms, status, rows, error,
                    database
             FROM query_history
             WHERE sql LIKE ?1 ESCAPE '\\' OR profile_name LIKE ?1 ESCAPE '\\'
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![pattern, i64::try_from(limit).unwrap_or(i64::MAX)],
            |r| {
                let profile_id: String = r.get(2)?;
                let status: String = r.get(6)?;
                Ok(HistoryEntry {
                    id: r.get(0)?,
                    executed_at: r.get(1)?,
                    entry: NewHistoryEntry {
                        profile_id: profile_id
                            .parse()
                            .map(ProfileId)
                            .unwrap_or_else(|_| ProfileId::new()),
                        profile_name: r.get(3)?,
                        database: r.get(9)?,
                        sql: r.get(4)?,
                        elapsed_ms: r.get::<_, i64>(5)?.try_into().unwrap_or(0),
                        status: HistoryStatus::parse(&status),
                        rows: r.get::<_, Option<i64>>(7)?.and_then(|n| n.try_into().ok()),
                        error: r.get(8)?,
                    },
                })
            },
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Deletes all history.
    pub fn clear_history(&self) -> Result<()> {
        self.conn.execute("DELETE FROM query_history", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sql: &str, status: HistoryStatus) -> NewHistoryEntry {
        NewHistoryEntry {
            profile_id: ProfileId::new(),
            profile_name: "local pg".into(),
            database: Some("archive_db".into()),
            sql: sql.into(),
            elapsed_ms: 12,
            status,
            rows: Some(3),
            error: None,
        }
    }

    #[test]
    fn records_and_filters_newest_first() {
        let store = ProfileStore::open_in_memory().unwrap();
        store
            .record_query(&entry("select 1", HistoryStatus::Ok))
            .unwrap();
        store
            .record_query(&entry("select 2_000 % 7", HistoryStatus::Error))
            .unwrap();
        store
            .record_query(&entry("update t set a = 1", HistoryStatus::Cancelled))
            .unwrap();

        let all = store.recent_queries("", 10).unwrap();
        let sqls: Vec<&str> = all.iter().map(|h| h.entry.sql.as_str()).collect();
        assert_eq!(sqls, ["update t set a = 1", "select 2_000 % 7", "select 1"]);
        assert_eq!(all[0].entry.status, HistoryStatus::Cancelled);
        assert_eq!(all[0].entry.database.as_deref(), Some("archive_db"));

        assert_eq!(store.recent_queries("SELECT", 10).unwrap().len(), 2);
        // LIKE wildcards in the filter are matched literally.
        assert_eq!(store.recent_queries("%", 10).unwrap().len(), 1);
        assert_eq!(store.recent_queries("_", 10).unwrap().len(), 1);
        assert_eq!(store.recent_queries("local", 10).unwrap().len(), 3);
        assert_eq!(store.recent_queries("", 1).unwrap().len(), 1);

        store.clear_history().unwrap();
        assert!(store.recent_queries("", 10).unwrap().is_empty());
    }

    #[test]
    fn prunes_beyond_limit() {
        let store = ProfileStore::open_in_memory().unwrap();
        for i in 0..(HISTORY_LIMIT + 5) {
            store
                .record_query(&entry(&format!("select {i}"), HistoryStatus::Ok))
                .unwrap();
        }
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM query_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, HISTORY_LIMIT);
        let newest = store.recent_queries("", 1).unwrap();
        assert_eq!(newest[0].entry.sql, format!("select {}", HISTORY_LIMIT + 4));
    }
}
