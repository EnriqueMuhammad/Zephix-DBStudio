//! Local storage.
//!
//! Profiles are stored in SQLite as JSON documents next to a few indexed columns. Passwords
//! are never written there; [`KeyringCredentials`] keeps them in the OS keychain (ADR 0005).

mod credentials;
mod history;

pub use credentials::{KEYRING_SERVICE, KeyringCredentials};
pub use history::{HISTORY_LIMIT, HistoryEntry, HistoryStatus, NewHistoryEntry};

use std::path::{Path, PathBuf};

use rdbs_core::{ConnectionProfile, ProfileId};
use rusqlite::{Connection, OptionalExtension, params};

/// Storage errors.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// SQLite failure.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A stored document could not be (de)serialised.
    #[error("invalid stored profile: {0}")]
    Json(#[from] serde_json::Error),
    /// Filesystem failure (creating the data directory).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// No platform data directory could be determined.
    #[error("could not determine the application data directory")]
    NoDataDir,
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, StorageError>;

/// Current schema version, stored in `PRAGMA user_version`.
const SCHEMA_VERSION: i32 = 4;

/// Default location of the local database file (path kept from earlier builds so saved
/// connections and history stay), e.g.
/// `%APPDATA%\Zephix\RustDB Studio\data\studio.db` on Windows.
pub fn default_db_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("dev", "Zephix", "RustDB Studio")
        .ok_or(StorageError::NoDataDir)?;
    Ok(dirs.data_dir().join("studio.db"))
}

/// File where trusted SSH host keys are recorded (separate from `~/.ssh/known_hosts`).
pub fn default_known_hosts_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("dev", "Zephix", "RustDB Studio")
        .ok_or(StorageError::NoDataDir)?;
    Ok(dirs.data_dir().join("known_hosts"))
}

/// Local store: connection profiles and query history.
pub struct ProfileStore {
    conn: Connection,
}

impl ProfileStore {
    /// Opens (creating if needed) the store at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    /// Opens a private in-memory store (tests, or fallback when the disk is unavailable).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS connection_profiles (
                     id          TEXT PRIMARY KEY,
                     name        TEXT NOT NULL,
                     kind        TEXT NOT NULL,
                     sort_order  INTEGER NOT NULL,
                     data        TEXT NOT NULL,
                     updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
                 );",
            )?;
        }
        if version < 2 {
            conn.execute_batch(history::CREATE_HISTORY)?;
        }
        if version < 3 {
            conn.execute_batch("ALTER TABLE query_history ADD COLUMN database TEXT;")?;
        }
        if version < 4 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS backup_schedules (
                     id    TEXT PRIMARY KEY,
                     data  TEXT NOT NULL
                 );",
            )?;
        }
        if version < SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn })
    }

    /// All backup schedules, by name.
    pub fn schedules(&self) -> Result<Vec<rdbs_core::job::BackupSchedule>> {
        let mut stmt = self.conn.prepare("SELECT data FROM backup_schedules")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out: Vec<rdbs_core::job::BackupSchedule> = Vec::new();
        for json in rows {
            out.push(serde_json::from_str(&json?)?);
        }
        out.sort_by_key(|s| s.name.to_lowercase());
        Ok(out)
    }

    /// Inserts or replaces a backup schedule.
    pub fn save_schedule(&self, schedule: &rdbs_core::job::BackupSchedule) -> Result<()> {
        self.conn.execute(
            "INSERT INTO backup_schedules (id, data) VALUES (?1, ?2) \
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![schedule.id, serde_json::to_string(schedule)?],
        )?;
        Ok(())
    }

    /// Deletes a backup schedule.
    pub fn delete_schedule(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM backup_schedules WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// All profiles in display order.
    pub fn list(&self) -> Result<Vec<ConnectionProfile>> {
        let mut stmt = self
            .conn
            .prepare("SELECT data FROM connection_profiles ORDER BY sort_order, name")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            out.push(serde_json::from_str(&json?)?);
        }
        Ok(out)
    }

    /// Looks up one profile.
    pub fn get(&self, id: ProfileId) -> Result<Option<ConnectionProfile>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT data FROM connection_profiles WHERE id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
    }

    /// Inserts or updates a profile. New profiles are appended at the end.
    pub fn upsert(&self, profile: &ConnectionProfile) -> Result<()> {
        let data = serde_json::to_string(profile)?;
        self.conn.execute(
            "INSERT INTO connection_profiles (id, name, kind, sort_order, data)
             VALUES (?1, ?2, ?3,
                     (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM connection_profiles), ?4)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 kind = excluded.kind,
                 data = excluded.data,
                 updated_at = datetime('now')",
            params![
                profile.id.to_string(),
                profile.name,
                format!("{:?}", profile.kind),
                data
            ],
        )?;
        Ok(())
    }

    /// Deletes a profile; returns whether it existed.
    pub fn delete(&self, id: ProfileId) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM connection_profiles WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(n > 0)
    }

    /// Whether the store has no profiles yet (first start).
    pub fn is_empty(&self) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM connection_profiles", [], |r| r.get(0))?;
        Ok(n == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdbs_core::{DatabaseKind, Environment};

    fn profile(name: &str) -> ConnectionProfile {
        let mut p = ConnectionProfile::new(DatabaseKind::PostgreSql);
        p.name = name.into();
        p
    }

    #[test]
    fn schedules_round_trip() {
        use rdbs_core::job::{BackupOptions, BackupSchedule, Frequency};
        let store = ProfileStore::open_in_memory().unwrap();
        let mut s = BackupSchedule {
            id: "s1".into(),
            name: "Nightly".into(),
            profile_id: ProfileId::new(),
            database: Some("shop".into()),
            options: BackupOptions::default(),
            frequency: Frequency::Daily { hour: 1, minute: 0 },
            enabled: true,
            last_run: 1,
            last_result: None,
        };
        store.save_schedule(&s).unwrap();
        s.last_result = Some(Ok("done".into()));
        store.save_schedule(&s).unwrap();
        assert_eq!(store.schedules().unwrap(), vec![s.clone()]);
        store.delete_schedule("s1").unwrap();
        assert!(store.schedules().unwrap().is_empty());
    }

    #[test]
    fn crud_round_trip() {
        let store = ProfileStore::open_in_memory().unwrap();
        assert!(store.is_empty().unwrap());

        let mut a = profile("a");
        store.upsert(&a).unwrap();
        store.upsert(&profile("b")).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);

        a.environment = Environment::Production;
        a.name = "a2".into();
        store.upsert(&a).unwrap();
        assert_eq!(store.get(a.id).unwrap(), Some(a.clone()));

        assert!(store.delete(a.id).unwrap());
        assert!(!store.delete(a.id).unwrap());
        assert_eq!(store.get(a.id).unwrap(), None);
    }

    #[test]
    fn list_preserves_insertion_order_across_updates() {
        let store = ProfileStore::open_in_memory().unwrap();
        let mut z = profile("zeta");
        store.upsert(&z).unwrap();
        store.upsert(&profile("alpha")).unwrap();
        z.port = 6543;
        store.upsert(&z).unwrap();
        let names: Vec<String> = store.list().unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["zeta", "alpha"]);
    }

    #[test]
    fn upgrades_version_1_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE connection_profiles (id TEXT PRIMARY KEY, name TEXT NOT NULL,
                 kind TEXT NOT NULL, sort_order INTEGER NOT NULL, data TEXT NOT NULL,
                 updated_at TEXT NOT NULL DEFAULT (datetime('now')));
             PRAGMA user_version = 1;",
        )
        .unwrap();
        let store = ProfileStore::init(conn).unwrap();
        assert!(store.recent_queries("", 5).unwrap().is_empty());
        let version: i32 = store
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn reopening_file_keeps_data_and_schema() {
        let dir = std::env::temp_dir().join(format!("rdbs-test-{}", ProfileId::new()));
        let path = dir.join("studio.db");
        let p = profile("persisted");
        {
            let store = ProfileStore::open(&path).unwrap();
            store.upsert(&p).unwrap();
        }
        let store = ProfileStore::open(&path).unwrap();
        assert_eq!(store.list().unwrap(), vec![p]);
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }
}
