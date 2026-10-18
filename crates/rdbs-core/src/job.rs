//! Settings of long-running background jobs (data transfer/sync, backups). The jobs
//! themselves live in `rdbs-transfer`; these types travel in [`crate::Command`]s.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What happens to the target table's rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TransferMode {
    /// Insert all source rows (target rows are kept).
    #[default]
    Append,
    /// Delete all target rows, then insert.
    Replace,
    /// Drop and recreate the target table from the source definition, then insert.
    Recreate,
    /// Make the target rows equal to the source by primary key: insert missing rows, update
    /// changed ones and (optionally) delete rows that are not in the source.
    Sync {
        /// Delete target rows missing from the source.
        delete_missing: bool,
    },
}

impl TransferMode {
    /// Label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            TransferMode::Append => "Append rows",
            TransferMode::Replace => "Replace rows (delete, then insert)",
            TransferMode::Recreate => "Drop and recreate tables",
            TransferMode::Sync { .. } => "Synchronize by primary key",
        }
    }
}

/// Settings of a data transfer between two databases.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransferOptions {
    /// Source schema (engines with schemas).
    pub source_schema: Option<String>,
    /// Target schema (engines with schemas).
    pub target_schema: Option<String>,
    /// Source tables to copy (empty = all).
    pub tables: Vec<String>,
    /// Create tables missing in the target.
    pub create_missing: bool,
    /// Row handling.
    pub mode: TransferMode,
    /// Rows read per page.
    pub page_rows: usize,
    /// Rows per INSERT statement.
    pub batch_rows: usize,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            source_schema: None,
            target_schema: None,
            tables: Vec::new(),
            create_missing: true,
            mode: TransferMode::Append,
            page_rows: 5_000,
            batch_rows: 500,
        }
    }
}

/// Settings of a logical backup (SQL dump).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupOptions {
    /// Schema to dump (engines with schemas; `None` = all user schemas).
    pub schema: Option<String>,
    /// Tables to dump (empty = all).
    pub tables: Vec<String>,
    /// Include `CREATE TABLE` statements.
    pub include_schema: bool,
    /// Include the rows as `INSERT` statements.
    pub include_data: bool,
    /// Output file. `{date}` / `{time}` are replaced by the start time.
    pub path: PathBuf,
    /// Keep only the newest N files matching the same pattern in the folder (0 = keep all).
    pub keep: usize,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            schema: None,
            tables: Vec::new(),
            include_schema: true,
            include_data: true,
            path: PathBuf::new(),
            keep: 0,
        }
    }
}

/// When a scheduled backup runs (local time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Frequency {
    /// Every `hours` hours at `minute` past the hour.
    EveryHours {
        /// Interval in hours (1–24).
        hours: u32,
        /// Minute past the hour.
        minute: u32,
    },
    /// Every day at `hour:minute`.
    Daily {
        /// Hour (0–23).
        hour: u32,
        /// Minute (0–59).
        minute: u32,
    },
    /// Every week on `weekday` (0 = Monday) at `hour:minute`.
    Weekly {
        /// Day of the week, 0 = Monday … 6 = Sunday.
        weekday: u32,
        /// Hour (0–23).
        hour: u32,
        /// Minute (0–59).
        minute: u32,
    },
}

impl Default for Frequency {
    fn default() -> Self {
        Frequency::Daily { hour: 2, minute: 0 }
    }
}

impl Frequency {
    /// Human-readable description.
    pub fn describe(self) -> String {
        const DAYS: [&str; 7] = [
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday",
        ];
        match self {
            Frequency::EveryHours { hours: 1, minute } => format!("Every hour at :{minute:02}"),
            Frequency::EveryHours { hours, minute } => {
                format!("Every {hours} hours at :{minute:02}")
            }
            Frequency::Daily { hour, minute } => format!("Daily at {hour:02}:{minute:02}"),
            Frequency::Weekly {
                weekday,
                hour,
                minute,
            } => format!(
                "Every {} at {hour:02}:{minute:02}",
                DAYS.get(weekday as usize).copied().unwrap_or("Monday")
            ),
        }
    }
}

/// A backup that runs on a schedule while the application is open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupSchedule {
    /// Stable id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Connection to back up.
    pub profile_id: crate::profile::ProfileId,
    /// Database override.
    pub database: Option<String>,
    /// What to dump and where.
    pub options: BackupOptions,
    /// When to run.
    pub frequency: Frequency,
    /// Whether the schedule is active.
    pub enabled: bool,
    /// Last run (or creation) time, Unix seconds.
    pub last_run: i64,
    /// Outcome of the last run: `Ok(summary)` or `Err(message)`.
    pub last_result: Option<Result<String, String>>,
}
