//! SQL Server Agent jobs.
//!
//! The explorer shows one **SQL Server Agent → Jobs** node per SQL Server connection.
//! [`history_sql`] is the statement a history tab runs; [`create_script`] is the T-SQL a
//! "Script Job as CREATE" tab shows and does not run.
//!
//! [`action_sql`] is what Start, Stop, Enable and Disable run (`sp_start_job`,
//! `sp_stop_job`, `sp_update_job`). Those change server state. [`action_requires_confirmation`]
//! is the rule the UI applies: every action on a production connection, and Stop on any
//! connection because it interrupts a running job. Editing steps and schedules is not
//! supported. Zephix has no read-only connection flag; the server still enforces
//! `SQLAgentOperatorRole` (or ownership via `SQLAgentUserRole`).

use crate::profile::Environment;
use crate::schema::{DbObject, ObjectKind};

/// Explorer folder that holds Agent jobs for one server.
pub const AGENT_FOLDER: &str = "SQL Server Agent";

/// Child folder of [`AGENT_FOLDER`].
pub const JOBS_FOLDER: &str = "Jobs";

/// Shown when the edition is SQL Server Express (Agent is not part of that edition).
pub const MSG_EXPRESS: &str = "SQL Server Express does not include SQL Server Agent.";

/// Shown when `msdb.dbo.sysjobs` is missing on an edition that could have Agent.
pub const MSG_NOT_INSTALLED: &str = "SQL Server Agent is not installed on this server.";

/// Shown when `msdb` is missing, offline, or not visible.
pub const MSG_MSDB: &str =
    "The msdb database is not online, so SQL Server Agent jobs cannot be listed.";

/// Shown when the login cannot read Agent catalog tables.
pub const MSG_PERMISSION: &str = "You do not have permission to read SQL Server Agent jobs. \
The SQLAgentReaderRole role in msdb is enough.";

/// One job as listed in the explorer (no step or schedule body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentJobSummary {
    /// `sysjobs.name`.
    pub name: String,
    /// `sysjobs.enabled`.
    pub enabled: bool,
    /// Login that owns the job, when known.
    pub owner: String,
    /// `syscategories.name`, when known.
    pub category: String,
    /// `sysjobhistory.run_status` of the latest job-outcome row (`step_id` 0).
    pub last_outcome: Option<i32>,
    /// `run_date` (`yyyymmdd`) of that row.
    pub last_run_date: Option<i32>,
    /// `run_time` (`hhmmss`) of that row.
    pub last_run_time: Option<i32>,
    /// Earliest `sysjobschedules.next_run_date` that is set.
    pub next_run_date: Option<i32>,
    /// Matching `next_run_time`.
    pub next_run_time: Option<i32>,
    /// A session in `sysjobactivity` has started and not stopped.
    pub running: bool,
    /// Step name while [`Self::running`] (from an in-progress history row, or the start step).
    pub current_step: String,
    /// Steps, in `step_id` order, for "Start at step".
    pub steps: Vec<AgentStep>,
}

/// One job step the explorer can start at. Not a full step definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStep {
    /// `sysjobsteps.step_id`, starting at 1.
    pub id: i32,
    /// `sysjobsteps.step_name`.
    pub name: String,
}

/// What the explorer can ask SQL Server Agent to do. Not step or schedule edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentJobAction {
    /// `msdb.dbo.sp_start_job`. `step_name` is `@step_name` when starting at a step.
    Start {
        /// Empty means the job's own start step.
        step_name: Option<String>,
    },
    /// `msdb.dbo.sp_stop_job`. Interrupts a running job.
    Stop,
    /// `msdb.dbo.sp_update_job @enabled`.
    SetEnabled(bool),
}

impl AgentJobSummary {
    /// Parses one row of the explorer query.
    ///
    /// Columns: name, enabled (`0`/`1` or `true`/`false`), owner, category, last outcome,
    /// last run date, last run time, next run date, next run time. Optional trailing columns
    /// are running (`0`/`1`), the current step name, and the packed step list from
    /// [`parse_step_list`]. Empty cells are missing. A date of `0` (Agent has not computed
    /// a next run) is treated as missing.
    pub fn from_columns(row: &[Option<String>]) -> Option<Self> {
        let name = cell(row, 0);
        if name.is_empty() {
            return None;
        }
        let running = cell_flag(row, 9);
        Some(Self {
            name,
            enabled: cell_flag(row, 1),
            owner: cell(row, 2),
            category: cell(row, 3),
            last_outcome: cell_i32(row, 4),
            last_run_date: nonzero(cell_i32(row, 5)),
            last_run_time: cell_i32(row, 6),
            next_run_date: nonzero(cell_i32(row, 7)),
            next_run_time: cell_i32(row, 8),
            running,
            current_step: if running {
                cell(row, 10)
            } else {
                String::new()
            },
            steps: parse_step_list(&cell(row, 11)),
        })
    }

    /// Fills running state and the step list from a follow-up query.
    ///
    /// The follow-up is separate so a login that can read `sysjobs` but not
    /// `sysjobactivity` still sees the job list.
    pub fn apply_activity(&mut self, running: bool, current_step: &str, packed_steps: &str) {
        self.running = running;
        self.current_step = if running {
            current_step.to_owned()
        } else {
            String::new()
        };
        self.steps = parse_step_list(packed_steps);
    }
}

/// One `sysjobsteps` row needed to recreate the step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentJobStep {
    /// `step_id`, starting at 1.
    pub step_id: i32,
    /// `step_name`.
    pub name: String,
    /// `subsystem` (`TSQL`, `PowerShell`, …).
    pub subsystem: String,
    /// `command`.
    pub command: String,
    /// `database_name` for T-SQL steps.
    pub database_name: String,
    /// `database_user_name`, when the step runs as a database user.
    pub database_user_name: String,
    /// `cmdexec_success_code`.
    pub success_code: i32,
    /// `on_success_action` (1 quit with success, 2 quit with failure, 3 next, 4 go to step).
    pub on_success_action: i32,
    /// `on_success_step_id`.
    pub on_success_step_id: i32,
    /// `on_fail_action`.
    pub on_fail_action: i32,
    /// `on_fail_step_id`.
    pub on_fail_step_id: i32,
    /// `retry_attempts`.
    pub retry_attempts: i32,
    /// `retry_interval` in minutes.
    pub retry_interval: i32,
    /// `os_run_priority`.
    pub os_run_priority: i32,
    /// `flags`.
    pub flags: i32,
    /// `output_file_name`.
    pub output_file_name: String,
}

/// One schedule attached to a job (`sysschedules` via `sysjobschedules`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentJobSchedule {
    /// Schedule name.
    pub name: String,
    /// `enabled`.
    pub enabled: bool,
    /// `freq_type`.
    pub freq_type: i32,
    /// `freq_interval`.
    pub freq_interval: i32,
    /// `freq_subday_type`.
    pub freq_subday_type: i32,
    /// `freq_subday_interval`.
    pub freq_subday_interval: i32,
    /// `freq_relative_interval`.
    pub freq_relative_interval: i32,
    /// `freq_recurrence_factor`.
    pub freq_recurrence_factor: i32,
    /// `active_start_date` (`yyyymmdd`).
    pub active_start_date: i32,
    /// `active_end_date`.
    pub active_end_date: i32,
    /// `active_start_time` (`hhmmss`).
    pub active_start_time: i32,
    /// `active_end_time`.
    pub active_end_time: i32,
}

/// Everything [`create_script`] needs to emit a `CREATE` script. Populated from `msdb`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentJobDef {
    /// Job name.
    pub name: String,
    /// `enabled`.
    pub enabled: bool,
    /// `description`.
    pub description: String,
    /// Category name. Empty skips `sp_add_category`.
    pub category: String,
    /// Owner login. Empty omits `@owner_login_name`.
    pub owner: String,
    /// `start_step_id`.
    pub start_step_id: i32,
    /// `notify_level_eventlog`.
    pub notify_level_eventlog: i32,
    /// `notify_level_email`.
    pub notify_level_email: i32,
    /// `notify_level_netsend`.
    pub notify_level_netsend: i32,
    /// `notify_level_page`.
    pub notify_level_page: i32,
    /// `delete_level`.
    pub delete_level: i32,
    /// Steps in `step_id` order.
    pub steps: Vec<AgentJobStep>,
    /// Schedules.
    pub schedules: Vec<AgentJobSchedule>,
}

impl AgentJobDef {
    /// Builds a definition from the three catalog result sets (header, steps, schedules).
    ///
    /// Header columns: name, enabled, description, start_step_id, the four notify levels,
    /// delete_level, category, owner. Returns `None` when the header has no name.
    pub fn from_catalog(
        header: &[Option<String>],
        steps: &[Vec<Option<String>>],
        schedules: &[Vec<Option<String>>],
    ) -> Option<Self> {
        let name = cell(header, 0);
        if name.is_empty() {
            return None;
        }
        Some(Self {
            name,
            enabled: cell_flag(header, 1),
            description: cell(header, 2),
            start_step_id: cell_i32(header, 3).unwrap_or(1),
            notify_level_eventlog: cell_i32(header, 4).unwrap_or(0),
            notify_level_email: cell_i32(header, 5).unwrap_or(0),
            notify_level_netsend: cell_i32(header, 6).unwrap_or(0),
            notify_level_page: cell_i32(header, 7).unwrap_or(0),
            delete_level: cell_i32(header, 8).unwrap_or(0),
            category: cell(header, 9),
            owner: cell(header, 10),
            steps: steps.iter().filter_map(|row| step_from(row)).collect(),
            schedules: schedules
                .iter()
                .filter_map(|row| schedule_from(row))
                .collect(),
        })
    }
}

/// What the availability probe decided, before any job query is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentProbe {
    /// `msdb.dbo.sysjobs` exists and the login may select from it.
    Ready,
    /// A fixed explanation safe to show in the tree. No error was raised.
    Unavailable(&'static str),
}

/// Interprets the probe row: edition, `DATABASEPROPERTYEX(msdb, 'Status')`,
/// `OBJECT_ID(msdb.dbo.sysjobs)`, `HAS_PERMS_BY_NAME(..., 'SELECT')`.
///
/// A permission value other than `0` (including unknown) is [`AgentProbe::Ready`] so a login
/// that can read the table is not hidden. The list query still fails closed if the read is
/// rejected.
pub fn interpret_probe(edition: &str, status: &str, object_id: &str, perm: &str) -> AgentProbe {
    if !status.eq_ignore_ascii_case("ONLINE") {
        return AgentProbe::Unavailable(MSG_MSDB);
    }
    if object_id.is_empty() {
        return if edition.to_ascii_lowercase().contains("express") {
            AgentProbe::Unavailable(MSG_EXPRESS)
        } else {
            AgentProbe::Unavailable(MSG_NOT_INSTALLED)
        };
    }
    if perm == "0" {
        return AgentProbe::Unavailable(MSG_PERMISSION);
    }
    AgentProbe::Ready
}

/// Message for an unexpected failure while reading Agent catalog tables.
///
/// Known SQL Server error numbers become the same fixed sentences as [`interpret_probe`].
/// Anything else keeps the server text so the tree explains the failure without a dialog.
pub fn unavailable_reason(code: Option<&str>, message: &str) -> String {
    match code {
        Some("208") => MSG_NOT_INSTALLED.to_owned(),
        Some("229" | "230" | "300") => MSG_PERMISSION.to_owned(),
        Some("916" | "942" | "945" | "4060") => MSG_MSDB.to_owned(),
        _ => {
            let lower = message.to_ascii_lowercase();
            if lower.contains("permission") && lower.contains("denied") {
                MSG_PERMISSION.to_owned()
            } else if lower.contains("invalid object name") {
                MSG_NOT_INSTALLED.to_owned()
            } else if lower.contains("msdb")
                && (lower.contains("cannot be opened") || lower.contains("not able to access"))
            {
                MSG_MSDB.to_owned()
            } else {
                format!("SQL Server Agent jobs could not be read: {message}")
            }
        }
    }
}

/// `sys.dm_server_services.status_desc` of `Stopped`.
pub fn service_is_stopped(status: &str) -> bool {
    status.trim().eq_ignore_ascii_case("stopped")
}

/// `run_status`: 0 failed, 1 succeeded, 2 retry, 3 canceled, 4 in progress.
pub fn run_outcome(status: i32) -> &'static str {
    match status {
        0 => "Failed",
        1 => "Succeeded",
        2 => "Retry",
        3 => "Canceled",
        4 => "In progress",
        _ => "Unknown",
    }
}

/// `yyyymmdd` + `hhmmss` (hours may be a single digit) → `YYYY-MM-DD HH:MM:SS`.
///
/// `None` when the numbers cannot be a calendar date or a time of day. A date of `0` (the
/// Agent sentinel for "no next run") is `None`.
pub fn format_agent_datetime(date: i32, time: i32) -> Option<String> {
    if date <= 0 || time < 0 {
        return None;
    }
    let year = date / 10_000;
    let month = (date / 100) % 100;
    let day = date % 100;
    let hour = time / 10_000;
    let minute = (time / 100) % 100;
    let second = time % 100;
    if !(1900..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
    ))
}

/// Dimmed text next to a job name: running step, enabled state, owner, category, last outcome, next run.
pub fn job_detail(job: &AgentJobSummary) -> String {
    let mut parts = Vec::new();
    if job.running {
        parts.push("running".to_owned());
        if !job.current_step.is_empty() {
            parts.push(format!("step {}", job.current_step));
        }
    }
    parts.push(if job.enabled { "enabled" } else { "disabled" }.to_owned());
    if !job.owner.is_empty() {
        parts.push(job.owner.clone());
    }
    if !job.category.is_empty() {
        parts.push(job.category.clone());
    }
    match (job.last_outcome, job.last_run_date, job.last_run_time) {
        (Some(status), Some(date), Some(time)) => match format_agent_datetime(date, time) {
            Some(when) => parts.push(format!("{} {when}", run_outcome(status))),
            None => parts.push(run_outcome(status).to_owned()),
        },
        (Some(status), _, _) => parts.push(run_outcome(status).to_owned()),
        _ => parts.push("never run".to_owned()),
    }
    if let (Some(date), Some(time)) = (job.next_run_date, job.next_run_time)
        && let Some(when) = format_agent_datetime(date, time)
    {
        parts.push(format!("next {when}"));
    }
    parts.join(" · ")
}

/// `SQL Server Agent / Jobs / <job>` for a successful read.
///
/// `service_stopped` marks the Agent folder when the service is known to be stopped. Jobs
/// stay listed: they live in `msdb` and can still be scripted and their history read.
pub fn jobs_node(jobs: &[AgentJobSummary], service_stopped: bool) -> DbObject {
    let children = jobs
        .iter()
        .filter(|job| !job.name.trim().is_empty())
        .map(|job| {
            let mut node =
                DbObject::new(job.name.clone(), ObjectKind::AgentJob).with_detail(job_detail(job));
            node.notes = job_notes(job, service_stopped);
            node
        })
        .collect::<Vec<_>>();
    let empty = children.is_empty();
    let mut jobs_folder = DbObject::new(JOBS_FOLDER, ObjectKind::Folder).with_children(children);
    if empty {
        jobs_folder = jobs_folder.with_detail("none");
    }
    let mut root = DbObject::new(AGENT_FOLDER, ObjectKind::Folder).with_children(vec![jobs_folder]);
    if service_stopped {
        root = root.with_detail("service stopped");
    }
    root
}

/// Agent folder whose only child explains why jobs were not listed. Not an error dialog.
pub fn unavailable_node(message: impl Into<String>) -> DbObject {
    DbObject::new(AGENT_FOLDER, ObjectKind::Folder)
        .with_detail("not available")
        .with_children(vec![DbObject::new(message.into(), ObjectKind::Notice)])
}

/// `SELECT` of `msdb.dbo.sysjobhistory` for one job, newest run first, including step 0
/// (the job outcome) and each step's message.
pub fn history_sql(job_name: &str) -> String {
    let name = nvarchar_expr(job_name);
    format!(
        "\
SELECT
    h.instance_id,
    h.step_id,
    h.step_name,
    CASE h.run_status
        WHEN 0 THEN N'Failed'
        WHEN 1 THEN N'Succeeded'
        WHEN 2 THEN N'Retry'
        WHEN 3 THEN N'Canceled'
        WHEN 4 THEN N'In progress'
        ELSE N'Unknown'
    END AS outcome,
    CASE
        WHEN h.run_date IS NULL OR h.run_date = 0 THEN NULL
        ELSE CONVERT(varchar(10), CONVERT(date, CONVERT(varchar(8), h.run_date)), 23)
            + N' ' + STUFF(STUFF(RIGHT(N'000000' + CONVERT(varchar(6), h.run_time), 6), 3, 0, N':'), 6, 0, N':')
    END AS run_time,
    STUFF(STUFF(RIGHT(N'000000' + CONVERT(varchar(6), h.run_duration), 6), 3, 0, N':'), 6, 0, N':') AS duration,
    h.retries_attempted AS retries,
    h.sql_severity,
    h.sql_message_id,
    h.message
FROM msdb.dbo.sysjobhistory AS h
INNER JOIN msdb.dbo.sysjobs AS j ON j.job_id = h.job_id
WHERE j.name = {name}
ORDER BY h.instance_id DESC;"
    )
}

/// Decodes the step list column: `id` + unit separator + `name`, repeated.
///
/// A unit separator inside a step name is not representable and is dropped by the query.
pub fn parse_step_list(packed: &str) -> Vec<AgentStep> {
    let mut parts = packed.split('\u{1f}').filter(|part| !part.is_empty());
    let mut steps = Vec::new();
    while let Some(id) = parts.next() {
        let Some(name) = parts.next() else { break };
        let Ok(id) = id.parse::<i32>() else { continue };
        if name.is_empty() {
            continue;
        }
        steps.push(AgentStep {
            id,
            name: name.to_owned(),
        });
    }
    steps
}

/// Notes stored on an [`ObjectKind::AgentJob`] node. See [`job_controls`].
pub fn job_notes(job: &AgentJobSummary, agent_stopped: bool) -> Vec<String> {
    let mut notes = vec![if job.enabled {
        "enabled".to_owned()
    } else {
        "disabled".to_owned()
    }];
    if job.running {
        notes.push("running".to_owned());
    }
    if agent_stopped {
        notes.push("agent-stopped".to_owned());
    }
    if !job.current_step.is_empty() {
        notes.push(format!("step\t{}", job.current_step));
    }
    for step in &job.steps {
        notes.push(format!("item\t{}\t{}", step.id, step.name));
    }
    notes
}

/// Which job actions the explorer offers, from [`job_notes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobControls {
    /// Start is clickable.
    pub can_start: bool,
    /// Shown when Start is disabled.
    pub start_reason: &'static str,
    /// Stop is in the menu (the job is running, or was when the tree was loaded).
    pub show_stop: bool,
    /// Stop is clickable. False when Agent itself is stopped.
    pub can_stop: bool,
    /// Shown when Stop is visible but disabled.
    pub stop_reason: &'static str,
    /// `Some(true)` offers Enable, `Some(false)` offers Disable.
    pub set_enabled: Option<bool>,
    /// Steps for "Start at step", in order.
    pub steps: Vec<AgentStep>,
}

/// Menu state for one job node. Unknown notes hide the write actions.
pub fn job_controls(notes: &[String]) -> JobControls {
    let enabled = notes.iter().any(|n| n == "enabled");
    let disabled = notes.iter().any(|n| n == "disabled");
    let running = notes.iter().any(|n| n == "running");
    let agent_stopped = notes.iter().any(|n| n == "agent-stopped");
    let steps = notes
        .iter()
        .filter_map(|note| {
            let mut parts = note.splitn(3, '\t');
            if parts.next()? != "item" {
                return None;
            }
            let id = parts.next()?.parse().ok()?;
            let name = parts.next()?.to_owned();
            if name.is_empty() {
                None
            } else {
                Some(AgentStep { id, name })
            }
        })
        .collect();
    let known = enabled || disabled;
    let can_start = known && enabled && !running && !agent_stopped;
    let start_reason = if !known {
        "This job cannot be changed from here."
    } else if agent_stopped {
        "SQL Server Agent is stopped."
    } else if running {
        "The job is already running."
    } else if !enabled {
        "Enable the job before starting it."
    } else {
        ""
    };
    JobControls {
        can_start,
        start_reason,
        show_stop: running,
        can_stop: running && !agent_stopped,
        stop_reason: if agent_stopped {
            "SQL Server Agent is stopped."
        } else {
            ""
        },
        set_enabled: if !known {
            None
        } else if enabled {
            Some(false)
        } else {
            Some(true)
        },
        steps,
    }
}

/// Production connections confirm every action. Stop also confirms on development and
/// staging, because it interrupts a running job.
pub fn action_requires_confirmation(action: &AgentJobAction, environment: Environment) -> bool {
    matches!(action, AgentJobAction::Stop) || environment.requires_write_confirmation()
}

/// Copy for the confirmation dialog.
pub struct ActionPrompt {
    /// Dialog title, without an icon.
    pub title: String,
    /// What will happen.
    pub body: String,
    /// Accept button.
    pub confirm_label: String,
}

/// Dialog text. Call this only when [`action_requires_confirmation`] is true.
pub fn action_prompt(
    job_name: &str,
    action: &AgentJobAction,
    connection: &str,
    production: bool,
) -> ActionPrompt {
    match action {
        AgentJobAction::Stop => ActionPrompt {
            title: "Stop job?".to_owned(),
            body: if production {
                format!(
                    "Stopping “{job_name}” interrupts the running job on “{connection}”, a production connection."
                )
            } else {
                format!("Stopping “{job_name}” interrupts the running job on “{connection}”.")
            },
            confirm_label: "Stop job".to_owned(),
        },
        AgentJobAction::Start { step_name } => {
            let what = match step_name.as_deref().filter(|step| !step.is_empty()) {
                Some(step) => format!("start “{job_name}” at step “{step}”"),
                None => format!("start “{job_name}”"),
            };
            production_prompt("Start job on production?", &what, connection, "Start job")
        }
        AgentJobAction::SetEnabled(true) => production_prompt(
            "Enable job on production?",
            &format!("enable “{job_name}”"),
            connection,
            "Enable job",
        ),
        AgentJobAction::SetEnabled(false) => production_prompt(
            "Disable job on production?",
            &format!("disable “{job_name}”"),
            connection,
            "Disable job",
        ),
    }
}

fn production_prompt(
    title: &str,
    verb: &str,
    connection: &str,
    confirm_label: &str,
) -> ActionPrompt {
    ActionPrompt {
        title: title.to_owned(),
        body: format!("This will {verb} on “{connection}”, a production connection."),
        confirm_label: confirm_label.to_owned(),
    }
}

/// Status-bar text after the procedure accepts the request.
pub fn action_success(job_name: &str, action: &AgentJobAction) -> String {
    match action {
        AgentJobAction::Start {
            step_name: Some(step),
        } if !step.is_empty() => {
            format!("Started job “{job_name}” at step “{step}”.")
        }
        AgentJobAction::Start { .. } => format!("Started job “{job_name}”."),
        AgentJobAction::Stop => format!("Stop requested for job “{job_name}”."),
        AgentJobAction::SetEnabled(true) => format!("Enabled job “{job_name}”."),
        AgentJobAction::SetEnabled(false) => format!("Disabled job “{job_name}”."),
    }
}

/// One sentence for the status bar when the procedure fails.
///
/// `code` is the SQL Server error number when the adapter has one. The message is the
/// server text. Known cases (Agent stopped, already running, disabled, permission,
/// missing job) replace that text; anything else is kept so the server's wording shows.
pub fn action_failure(
    job_name: &str,
    action: &AgentJobAction,
    code: Option<&str>,
    message: &str,
) -> String {
    let verb = match action {
        AgentJobAction::Start { .. } => "start",
        AgentJobAction::Stop => "stop",
        AgentJobAction::SetEnabled(true) => "enable",
        AgentJobAction::SetEnabled(false) => "disable",
    };
    format!(
        "Could not {verb} job “{job_name}”: {}",
        friendly_reason(code, message)
    )
}

fn friendly_reason(code: Option<&str>, message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("sqlserveragent is not currently running")
        || lower.contains("sql server agent is not currently running")
    {
        return "SQL Server Agent is stopped.".to_owned();
    }
    if lower.contains("already running") {
        return "the job is already running.".to_owned();
    }
    if lower.contains("not currently running") || lower.contains("not currently executing") {
        return "the job is not running.".to_owned();
    }
    if code == Some("14222") || lower.contains("disabled") {
        return "the job is disabled. Enable it before starting it.".to_owned();
    }
    if matches!(code, Some("229" | "230" | "300"))
        || lower.contains("permission")
        || lower.contains("sqlagentoperatorrole")
    {
        return "you do not have permission. SQLAgentOperatorRole in msdb can start, stop and \
                enable every job; SQLAgentUserRole can change jobs it owns."
            .to_owned();
    }
    if code == Some("14262") || lower.contains("does not exist") {
        return "the job was not found.".to_owned();
    }
    let trimmed = message.trim();
    if trimmed.is_empty() {
        "the server rejected the request.".to_owned()
    } else if let Some(code) = code.filter(|c| !c.is_empty()) {
        format!("[{code}] {trimmed}")
    } else {
        trimmed.to_owned()
    }
}

/// The statement Start, Stop, Enable or Disable executes. It does not edit steps or schedules.
pub fn action_sql(job_name: &str, action: &AgentJobAction) -> String {
    let name = nvarchar_expr(job_name);
    match action {
        AgentJobAction::Start { step_name } => {
            let mut sql = format!("EXEC msdb.dbo.sp_start_job @job_name = {name}");
            if let Some(step) = step_name.as_deref().filter(|step| !step.is_empty()) {
                sql.push_str(&format!(", @step_name = {}", nvarchar_expr(step)));
            }
            sql.push(';');
            sql
        }
        AgentJobAction::Stop => format!("EXEC msdb.dbo.sp_stop_job @job_name = {name};"),
        AgentJobAction::SetEnabled(enabled) => format!(
            "EXEC msdb.dbo.sp_update_job @job_name = {name}, @enabled = {};",
            i32::from(*enabled)
        ),
    }
}

/// Inverse of [`action_sql`] for the statements this module generates.
///
/// Returns `None` for any other batch, including a CREATE script (that calls
/// `sp_update_job` with `@job_id`, not `@job_name`).
pub fn parse_action_sql(sql: &str) -> Option<(String, AgentJobAction)> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let lower = trimmed.to_ascii_lowercase();
    let action = if lower.contains("sp_start_job") {
        AgentJobAction::Start {
            step_name: nvarchar_param(trimmed, "@step_name"),
        }
    } else if lower.contains("sp_stop_job") {
        AgentJobAction::Stop
    } else if lower.contains("sp_update_job") && int_param(trimmed, "@enabled").is_some() {
        AgentJobAction::SetEnabled(int_param(trimmed, "@enabled")? == 1)
    } else {
        return None;
    };
    let job_name = nvarchar_param(trimmed, "@job_name")?;
    if job_name.is_empty() || !lower.starts_with("exec ") {
        return None;
    }
    Some((job_name, action))
}

fn nvarchar_param(sql: &str, param: &str) -> Option<String> {
    let lower = sql.to_ascii_lowercase();
    let key = format!("{} = n'", param.to_ascii_lowercase());
    let start = lower.find(&key)? + key.len();
    let rest = sql.get(start..)?;
    let mut out = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' {
            if chars.peek() == Some(&'\'') {
                chars.next();
                out.push('\'');
            } else {
                return Some(out);
            }
        } else {
            out.push(c);
        }
    }
    None
}

fn int_param(sql: &str, param: &str) -> Option<i32> {
    let lower = sql.to_ascii_lowercase();
    let key = format!("{} = ", param.to_ascii_lowercase());
    let start = lower.find(&key)? + key.len();
    let rest = sql.get(start..)?;
    let digits: String = rest
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// T-SQL that recreates the job (category, job, steps, schedules, local target).
///
/// The script is for an editor tab. Zephix does not run it. It uses three-part `msdb`
/// names after `USE [msdb]`, targets `(LOCAL)`, and does not call `sp_start_job`.
/// A step command that contains its own `GO` line is emitted as `NCHAR(10)` concatenation
/// so a batch splitter does not cut the script in half.
pub fn create_script(job: &AgentJobDef) -> String {
    let mut out = String::from("USE [msdb];\nGO\n\n");
    let title = job.name.replace(['\r', '\n'], " ");
    out.push_str(&format!(
        "-- SQL Server Agent job: {title}\n\
         -- Created by Zephix as a script only. Running it creates the job; it does not start it.\n\n"
    ));
    out.push_str("BEGIN TRANSACTION;\nDECLARE @ReturnCode int;\nDECLARE @jobId binary(16);\nSET @ReturnCode = 0;\n\n");

    if !job.category.is_empty() {
        let name = nvarchar_expr(&job.category);
        let ty = category_type(&job.category);
        out.push_str(&format!(
            "IF NOT EXISTS (\n    \
             SELECT 1 FROM msdb.dbo.syscategories\n    \
             WHERE name = {name} AND category_class = 1\n)\n\
             BEGIN\n    \
             EXEC @ReturnCode = msdb.dbo.sp_add_category\n        \
             @class = N'JOB', @type = N'{ty}', @name = {name};\n    \
             {CHECK}\n\
             END;\n\n",
            CHECK = check(),
        ));
    }

    let mut params = vec![
        format!("@job_name = {}", nvarchar_expr(&job.name)),
        format!("@enabled = {}", i32::from(job.enabled)),
        format!("@description = {}", nvarchar_expr(&job.description)),
        format!("@start_step_id = {}", job.start_step_id.max(1)),
        format!("@notify_level_eventlog = {}", job.notify_level_eventlog),
        format!("@notify_level_email = {}", job.notify_level_email),
        format!("@notify_level_netsend = {}", job.notify_level_netsend),
        format!("@notify_level_page = {}", job.notify_level_page),
        format!("@delete_level = {}", job.delete_level),
    ];
    if !job.category.is_empty() {
        params.push(format!("@category_name = {}", nvarchar_expr(&job.category)));
    }
    if !job.owner.is_empty() {
        params.push(format!("@owner_login_name = {}", nvarchar_expr(&job.owner)));
    }
    params.push("@job_id = @jobId OUTPUT".to_owned());
    out.push_str(&exec_proc("msdb.dbo.sp_add_job", &params));

    for step in &job.steps {
        out.push_str(&exec_proc("msdb.dbo.sp_add_jobstep", &step_params(step)));
    }
    if !job.steps.is_empty() {
        out.push_str(&exec_proc(
            "msdb.dbo.sp_update_job",
            &[
                "@job_id = @jobId".to_owned(),
                format!("@start_step_id = {}", job.start_step_id.max(1)),
            ],
        ));
    }
    for schedule in &job.schedules {
        out.push_str(&exec_proc(
            "msdb.dbo.sp_add_jobschedule",
            &schedule_params(schedule),
        ));
    }
    out.push_str(&exec_proc(
        "msdb.dbo.sp_add_jobserver",
        &[
            "@job_id = @jobId".to_owned(),
            "@server_name = N'(LOCAL)'".to_owned(),
        ],
    ));
    out.push_str(
        "\
COMMIT TRANSACTION;
GOTO EndSave;

QuitWithRollback:
    IF (@@TRANCOUNT > 0) ROLLBACK TRANSACTION;

EndSave:
",
    );
    out
}

fn step_from(row: &[Option<String>]) -> Option<AgentJobStep> {
    if cell(row, 0).is_empty() && cell(row, 1).is_empty() {
        return None;
    }
    Some(AgentJobStep {
        step_id: cell_i32(row, 0).unwrap_or(1),
        name: cell(row, 1),
        subsystem: {
            let subsystem = cell(row, 2);
            if subsystem.is_empty() {
                "TSQL".to_owned()
            } else {
                subsystem
            }
        },
        command: cell(row, 3),
        database_name: cell(row, 4),
        database_user_name: cell(row, 5),
        success_code: cell_i32(row, 6).unwrap_or(0),
        on_success_action: cell_i32(row, 7).unwrap_or(1),
        on_success_step_id: cell_i32(row, 8).unwrap_or(0),
        on_fail_action: cell_i32(row, 9).unwrap_or(2),
        on_fail_step_id: cell_i32(row, 10).unwrap_or(0),
        retry_attempts: cell_i32(row, 11).unwrap_or(0),
        retry_interval: cell_i32(row, 12).unwrap_or(0),
        os_run_priority: cell_i32(row, 13).unwrap_or(0),
        flags: cell_i32(row, 14).unwrap_or(0),
        output_file_name: cell(row, 15),
    })
}

fn schedule_from(row: &[Option<String>]) -> Option<AgentJobSchedule> {
    let name = cell(row, 0);
    if name.is_empty() {
        return None;
    }
    Some(AgentJobSchedule {
        name,
        enabled: cell_flag(row, 1),
        freq_type: cell_i32(row, 2).unwrap_or(0),
        freq_interval: cell_i32(row, 3).unwrap_or(0),
        freq_subday_type: cell_i32(row, 4).unwrap_or(0),
        freq_subday_interval: cell_i32(row, 5).unwrap_or(0),
        freq_relative_interval: cell_i32(row, 6).unwrap_or(0),
        freq_recurrence_factor: cell_i32(row, 7).unwrap_or(0),
        active_start_date: cell_i32(row, 8).unwrap_or(0),
        active_end_date: cell_i32(row, 9).unwrap_or(0),
        active_start_time: cell_i32(row, 10).unwrap_or(0),
        active_end_time: cell_i32(row, 11).unwrap_or(0),
    })
}

fn step_params(step: &AgentJobStep) -> Vec<String> {
    let mut params = vec![
        "@job_id = @jobId".to_owned(),
        format!("@step_id = {}", step.step_id),
        format!("@step_name = {}", nvarchar_expr(&step.name)),
        format!("@subsystem = {}", nvarchar_expr(&step.subsystem)),
        format!("@command = {}", nvarchar_expr(&step.command)),
        format!("@cmdexec_success_code = {}", step.success_code),
        format!("@on_success_action = {}", step.on_success_action),
        format!("@on_success_step_id = {}", step.on_success_step_id),
        format!("@on_fail_action = {}", step.on_fail_action),
        format!("@on_fail_step_id = {}", step.on_fail_step_id),
        format!("@retry_attempts = {}", step.retry_attempts),
        format!("@retry_interval = {}", step.retry_interval),
        format!("@os_run_priority = {}", step.os_run_priority),
        format!("@flags = {}", step.flags),
    ];
    if !step.database_name.is_empty() {
        params.push(format!(
            "@database_name = {}",
            nvarchar_expr(&step.database_name)
        ));
    }
    if !step.database_user_name.is_empty() {
        params.push(format!(
            "@database_user_name = {}",
            nvarchar_expr(&step.database_user_name)
        ));
    }
    if !step.output_file_name.is_empty() {
        params.push(format!(
            "@output_file_name = {}",
            nvarchar_expr(&step.output_file_name)
        ));
    }
    params
}

fn schedule_params(schedule: &AgentJobSchedule) -> Vec<String> {
    vec![
        "@job_id = @jobId".to_owned(),
        format!("@name = {}", nvarchar_expr(&schedule.name)),
        format!("@enabled = {}", i32::from(schedule.enabled)),
        format!("@freq_type = {}", schedule.freq_type),
        format!("@freq_interval = {}", schedule.freq_interval),
        format!("@freq_subday_type = {}", schedule.freq_subday_type),
        format!("@freq_subday_interval = {}", schedule.freq_subday_interval),
        format!(
            "@freq_relative_interval = {}",
            schedule.freq_relative_interval
        ),
        format!(
            "@freq_recurrence_factor = {}",
            schedule.freq_recurrence_factor
        ),
        format!("@active_start_date = {}", schedule.active_start_date),
        format!("@active_end_date = {}", schedule.active_end_date),
        format!("@active_start_time = {}", schedule.active_start_time),
        format!("@active_end_time = {}", schedule.active_end_time),
    ]
}

fn exec_proc(proc: &str, params: &[String]) -> String {
    format!(
        "EXEC @ReturnCode = {proc}\n    {};\n{}\n\n",
        params.join(",\n    "),
        check()
    )
}

fn check() -> &'static str {
    "IF (@@ERROR <> 0 OR @ReturnCode <> 0) GOTO QuitWithRollback;"
}

fn category_type(name: &str) -> &'static str {
    if name.to_ascii_lowercase().contains("multi-server") {
        "MULTI-SERVER"
    } else {
        "LOCAL"
    }
}

/// `N'…'` literal, or `NCHAR(10)` concatenation when a line is only `GO`.
fn nvarchar_expr(value: &str) -> String {
    if !value.split('\n').any(is_go_line) {
        return format!("N'{}'", value.replace('\'', "''"));
    }
    value
        .split('\n')
        .enumerate()
        .map(|(i, line)| {
            let line = line.trim_end_matches('\r');
            let lit = format!("N'{}'", line.replace('\'', "''"));
            if i == 0 {
                lit
            } else {
                format!("NCHAR(10) + {lit}")
            }
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

fn is_go_line(line: &str) -> bool {
    let trimmed = line.trim().trim_end_matches('\r');
    let Some(rest) = trimmed
        .get(..2)
        .filter(|prefix| prefix.eq_ignore_ascii_case("go"))
        .map(|_| &trimmed[2..])
    else {
        return false;
    };
    rest.trim().chars().all(|c| c.is_ascii_digit())
}

fn cell(row: &[Option<String>], index: usize) -> String {
    row.get(index).cloned().flatten().unwrap_or_default()
}

fn cell_i32(row: &[Option<String>], index: usize) -> Option<i32> {
    let text = cell(row, index);
    if text.is_empty() {
        None
    } else {
        text.parse().ok()
    }
}

fn cell_flag(row: &[Option<String>], index: usize) -> bool {
    matches!(cell(row, index).as_str(), "1" | "true" | "TRUE" | "True")
}

fn nonzero(value: Option<i32>) -> Option<i32> {
    value.filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::{self, StatementKind};

    fn opt(values: &[&str]) -> Vec<Option<String>> {
        values
            .iter()
            .map(|v| {
                if v.is_empty() {
                    None
                } else {
                    Some((*v).to_owned())
                }
            })
            .collect()
    }

    fn summary() -> AgentJobSummary {
        AgentJobSummary::from_columns(&opt(&[
            "Nightly",
            "1",
            "sa",
            "Database Maintenance",
            "1",
            "20261024",
            "20130",
            "20261025",
            "20000",
        ]))
        .expect("summary")
    }

    #[test]
    fn formats_agent_datetimes() {
        assert_eq!(
            format_agent_datetime(20261024, 20130).as_deref(),
            Some("2026-10-24 02:01:30")
        );
        assert_eq!(
            format_agent_datetime(20261024, 0).as_deref(),
            Some("2026-10-24 00:00:00")
        );
        assert_eq!(format_agent_datetime(0, 0), None);
        assert_eq!(format_agent_datetime(20261340, 0), None);
        assert_eq!(format_agent_datetime(20261024, 240000), None);
        assert_eq!(format_agent_datetime(20261024, 160), None);
    }

    #[test]
    fn names_run_outcomes() {
        assert_eq!(run_outcome(0), "Failed");
        assert_eq!(run_outcome(1), "Succeeded");
        assert_eq!(run_outcome(2), "Retry");
        assert_eq!(run_outcome(3), "Canceled");
        assert_eq!(run_outcome(4), "In progress");
        assert_eq!(run_outcome(9), "Unknown");
    }

    #[test]
    fn parses_summary_and_builds_detail() {
        let job = summary();
        assert!(job.enabled);
        assert_eq!(job.last_outcome, Some(1));
        assert_eq!(
            job_detail(&job),
            "enabled · sa · Database Maintenance · Succeeded 2026-10-24 02:01:30 · next 2026-10-25 02:00:00"
        );

        let disabled =
            AgentJobSummary::from_columns(&opt(&["old", "0", "", "", "", "", "", "0", "0"]))
                .expect("disabled");
        assert!(!disabled.enabled);
        assert_eq!(disabled.next_run_date, None);
        assert_eq!(job_detail(&disabled), "disabled · never run");

        let failed = AgentJobSummary::from_columns(&opt(&[
            "bad", "true", "dbo", "", "0", "20260102", "", "", "",
        ]))
        .expect("failed");
        assert!(failed.enabled);
        assert_eq!(job_detail(&failed), "enabled · dbo · Failed");
        assert!(AgentJobSummary::from_columns(&opt(&[""])).is_none());
    }

    #[test]
    fn builds_job_tree_and_unavailable_node() {
        let node = jobs_node(&[summary()], true);
        assert_eq!(node.name, AGENT_FOLDER);
        assert_eq!(node.kind, ObjectKind::Folder);
        assert_eq!(node.detail.as_deref(), Some("service stopped"));
        assert_eq!(node.children[0].name, JOBS_FOLDER);
        assert_eq!(node.children[0].children[0].kind, ObjectKind::AgentJob);
        assert_eq!(node.children[0].children[0].name, "Nightly");

        let empty = jobs_node(&[], false);
        assert_eq!(empty.detail, None);
        assert_eq!(empty.children[0].detail.as_deref(), Some("none"));
        assert!(empty.children[0].children.is_empty());

        let missing = unavailable_node(MSG_PERMISSION);
        assert_eq!(missing.detail.as_deref(), Some("not available"));
        assert_eq!(missing.children[0].kind, ObjectKind::Notice);
        assert_eq!(missing.children[0].name, MSG_PERMISSION);
    }

    #[test]
    fn probe_hides_express_offline_and_denied_reads() {
        assert_eq!(
            interpret_probe("Express Edition (64-bit)", "ONLINE", "", ""),
            AgentProbe::Unavailable(MSG_EXPRESS)
        );
        assert_eq!(
            interpret_probe("Developer Edition", "ONLINE", "", "1"),
            AgentProbe::Unavailable(MSG_NOT_INSTALLED)
        );
        assert_eq!(
            interpret_probe("Developer Edition", "OFFLINE", "1", "1"),
            AgentProbe::Unavailable(MSG_MSDB)
        );
        assert_eq!(
            interpret_probe("Developer Edition", "", "1", "1"),
            AgentProbe::Unavailable(MSG_MSDB)
        );
        assert_eq!(
            interpret_probe("Standard Edition", "ONLINE", "42", "0"),
            AgentProbe::Unavailable(MSG_PERMISSION)
        );
        assert_eq!(
            interpret_probe("Developer Edition (64-bit)", "online", "42", "1"),
            AgentProbe::Ready
        );
        assert_eq!(
            interpret_probe("Developer Edition", "ONLINE", "42", ""),
            AgentProbe::Ready
        );
    }

    #[test]
    fn unexpected_errors_stay_a_single_sentence() {
        assert_eq!(
            unavailable_reason(Some("208"), "Invalid object name 'msdb.dbo.sysjobs'."),
            MSG_NOT_INSTALLED
        );
        assert_eq!(unavailable_reason(Some("229"), "denied"), MSG_PERMISSION);
        assert_eq!(unavailable_reason(Some("916"), "x"), MSG_MSDB);
        assert_eq!(
            unavailable_reason(
                None,
                "The SELECT permission was denied on the object 'sysjobs'."
            ),
            MSG_PERMISSION
        );
        let other = unavailable_reason(None, "connection reset");
        assert!(other.contains("connection reset"));
        assert!(!other.to_ascii_lowercase().contains("panic"));
        assert!(service_is_stopped("Stopped"));
        assert!(service_is_stopped(" stopped "));
        assert!(!service_is_stopped("Running"));
        assert!(!service_is_stopped(""));
    }

    #[test]
    fn history_sql_is_a_read_and_escapes_the_name() {
        let sql = history_sql("O'Brien");
        assert!(sql.contains("N'O''Brien'"));
        assert!(sql.contains("msdb.dbo.sysjobhistory"));
        assert!(sql.contains("h.message"));
        assert!(sql.contains("h.step_id"));
        assert!(sql.contains("WHEN 0 THEN N'Failed'"));
        assert_eq!(sql::classify(&sql), StatementKind::Read);
        assert!(!sql.to_ascii_lowercase().contains("sp_start_job"));
    }

    #[test]
    fn create_script_includes_steps_and_schedules_without_starting() {
        let header = opt(&[
            "Nightly's backup",
            "1",
            "line\nwith 'quotes'",
            "2",
            "0",
            "0",
            "0",
            "0",
            "0",
            "[Uncategorized (Local)]",
            "sa",
        ]);
        let steps = vec![opt(&[
            "1",
            "hello",
            "TSQL",
            "SELECT 1 AS n\nGO\nSELECT 2",
            "master",
            "",
            "0",
            "3",
            "0",
            "2",
            "0",
            "1",
            "5",
            "0",
            "0",
            "C:\\out.log",
        ])];
        let schedules = vec![opt(&[
            "daily", "1", "4", "1", "1", "0", "0", "0", "20261024", "99991231", "20000", "235959",
        ])];
        let job = AgentJobDef::from_catalog(&header, &steps, &schedules).expect("def");
        assert_eq!(job.start_step_id, 2);
        assert_eq!(job.steps[0].retry_attempts, 1);
        assert_eq!(job.schedules[0].freq_type, 4);

        let script = create_script(&job);
        assert!(script.starts_with("USE [msdb];\nGO\n"));
        assert!(script.contains("@job_name = N'Nightly''s backup'"));
        assert!(script.contains("@owner_login_name = N'sa'"));
        assert!(script.contains("@category_name = N'[Uncategorized (Local)]'"));
        assert!(script.contains("@type = N'LOCAL'"));
        assert!(script.contains("NCHAR(10) + N'GO'"));
        assert!(script.contains("@database_name = N'master'"));
        assert!(script.contains("@output_file_name = N'C:\\out.log'"));
        assert!(script.contains("@retry_attempts = 1"));
        assert!(script.contains("@start_step_id = 2"));
        assert!(script.contains("@name = N'daily'"));
        assert!(script.contains("@freq_type = 4"));
        assert!(script.contains("@active_start_time = 20000"));
        assert!(script.contains("@server_name = N'(LOCAL)'"));
        assert!(script.contains("sp_add_jobstep"));
        assert!(script.contains("sp_add_jobschedule"));
        assert!(!script.contains("sp_start_job"));
        assert!(!script.contains("sp_update_job @enabled"));
        let go_lines: Vec<_> = script.lines().filter(|line| is_go_line(line)).collect();
        assert_eq!(go_lines, ["GO"]);
        assert_ne!(sql::classify(&script), StatementKind::Read);

        let multi = AgentJobDef {
            category: "Multi-Server".into(),
            owner: String::new(),
            steps: Vec::new(),
            schedules: Vec::new(),
            ..job
        };
        let multi_script = create_script(&multi);
        assert!(multi_script.contains("@type = N'MULTI-SERVER'"));
        assert!(!multi_script.contains("@owner_login_name"));
        assert!(!multi_script.contains("sp_add_jobstep"));
    }

    #[test]
    fn running_jobs_lead_the_detail_and_keep_their_steps() {
        let mut row = opt(&[
            "Nightly",
            "1",
            "sa",
            "Database Maintenance",
            "4",
            "20261024",
            "20130",
            "0",
            "0",
            "1",
            "Rebuild",
            "1\u{1f}Rebuild\u{1f}2\u{1f}Stats",
        ]);
        let job = AgentJobSummary::from_columns(&row).expect("running");
        assert!(job.running);
        assert_eq!(job.current_step, "Rebuild");
        assert_eq!(job.steps.len(), 2);
        assert_eq!(job.steps[1].name, "Stats");
        assert!(job_detail(&job).starts_with("running · step Rebuild · enabled"));

        row[9] = Some("0".into());
        let idle = AgentJobSummary::from_columns(&row).expect("idle");
        assert!(!idle.running);
        assert!(idle.current_step.is_empty());
        assert_eq!(idle.steps.len(), 2);

        let mut parsed = summary();
        parsed.apply_activity(true, "Wait", "1\u{1f}Wait");
        assert!(parsed.running);
        assert_eq!(parsed.current_step, "Wait");
        parsed.apply_activity(false, "Wait", "");
        assert!(!parsed.running);
        assert!(parsed.current_step.is_empty());
        assert!(parsed.steps.is_empty());
    }

    #[test]
    fn menu_offers_only_actions_that_apply() {
        let enabled = summary();
        let notes = job_notes(&enabled, false);
        let menu = job_controls(&notes);
        assert!(menu.can_start);
        assert!(!menu.show_stop);
        assert_eq!(menu.set_enabled, Some(false));

        let stopped_agent = job_controls(&job_notes(&enabled, true));
        assert!(!stopped_agent.can_start);
        assert_eq!(stopped_agent.start_reason, "SQL Server Agent is stopped.");
        assert_eq!(stopped_agent.set_enabled, Some(false));

        let mut running = enabled.clone();
        running.running = true;
        running.current_step = "Rebuild".into();
        running.steps = vec![
            AgentStep {
                id: 1,
                name: "Rebuild".into(),
            },
            AgentStep {
                id: 2,
                name: "Stats".into(),
            },
        ];
        let menu = job_controls(&job_notes(&running, false));
        assert!(!menu.can_start);
        assert_eq!(menu.start_reason, "The job is already running.");
        assert!(menu.show_stop && menu.can_stop);
        assert_eq!(menu.steps.len(), 2);

        let mut disabled = enabled;
        disabled.enabled = false;
        let menu = job_controls(&job_notes(&disabled, false));
        assert!(!menu.can_start);
        assert!(menu.start_reason.contains("Enable"));
        assert_eq!(menu.set_enabled, Some(true));
        assert!(job_controls(&[]).set_enabled.is_none());
    }

    #[test]
    fn action_sql_round_trips_and_confirms_stop_everywhere() {
        let start = AgentJobAction::Start { step_name: None };
        let at_step = AgentJobAction::Start {
            step_name: Some("O'Hare".into()),
        };
        let stop = AgentJobAction::Stop;
        let enable = AgentJobAction::SetEnabled(true);
        let disable = AgentJobAction::SetEnabled(false);

        for action in [&start, &at_step, &stop, &enable, &disable] {
            let sql = action_sql("O'Brien", action);
            assert_eq!(
                parse_action_sql(&sql),
                Some(("O'Brien".into(), action.clone()))
            );
            assert_ne!(sql::classify(&sql), StatementKind::Read);
            assert!(!sql.contains("sp_add_jobstep"));
            assert!(!sql.contains("sp_delete_job"));
        }
        assert!(action_sql("Nightly", &at_step).contains("@step_name = N'O''Hare'"));
        assert!(action_sql("Nightly", &disable).contains("@enabled = 0"));
        assert!(parse_action_sql("SELECT 1").is_none());
        assert!(
            parse_action_sql("EXEC msdb.dbo.sp_update_job @job_id = @jobId, @start_step_id = 1;")
                .is_none()
        );

        assert!(action_requires_confirmation(
            &stop,
            Environment::Development
        ));
        assert!(action_requires_confirmation(&stop, Environment::Production));
        assert!(!action_requires_confirmation(
            &start,
            Environment::Development
        ));
        assert!(!action_requires_confirmation(&enable, Environment::Staging));
        assert!(action_requires_confirmation(
            &start,
            Environment::Production
        ));
        assert!(action_requires_confirmation(
            &disable,
            Environment::Production
        ));

        let prompt = action_prompt("Nightly", &stop, "Sales", false);
        assert!(prompt.body.contains("interrupts"));
        assert!(!prompt.body.contains("production"));
        let prompt = action_prompt("Nightly", &start, "Sales", true);
        assert!(prompt.body.contains("production"));
        assert!(prompt.body.contains("start “Nightly”"));

        assert_eq!(
            action_success("Nightly", &at_step),
            "Started job “Nightly” at step “O'Hare”."
        );
        assert!(
            action_failure(
                "Nightly",
                &start,
                Some("22022"),
                "SQLServerAgent is not currently running so it cannot be notified of this action."
            )
            .contains("SQL Server Agent is stopped")
        );
        assert!(
            action_failure(
                "Nightly",
                &start,
                None,
                "Request to run job Nightly refused because the job is already running."
            )
            .contains("already running")
        );
        assert!(
            action_failure(
                "Nightly",
                &stop,
                None,
                "Request to stop job Nightly refused because the job is not currently running."
            )
            .contains("not running")
        );
        assert!(
            !action_failure(
                "Nightly",
                &stop,
                None,
                "Request to stop job Nightly refused because the job is not currently running."
            )
            .contains("Agent is stopped")
        );
        assert!(
            action_failure("Nightly", &start, Some("14222"), "The job is disabled.")
                .contains("disabled")
        );
        assert!(
            action_failure(
                "Nightly",
                &start,
                Some("229"),
                "The EXECUTE permission was denied on the object 'sp_start_job'."
            )
            .contains("SQLAgentOperatorRole")
        );
        assert!(
            action_failure("Nightly", &enable, Some("14262"), "does not exist")
                .contains("not found")
        );
        let other = action_failure(
            "Nightly",
            &start,
            Some("50000"),
            "Backup device is offline.",
        );
        assert!(other.contains("Backup device is offline"));
        assert!(other.contains("50000"));
    }
}
