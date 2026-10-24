//! Read SQL Server Agent jobs from `msdb`, and run the start/stop/enable statements
//! the worker sends. Creating or editing a job's steps is still a script the user runs.
//!
//! The probe uses catalog functions that return NULL instead of raising an error, so Express,
//! a stopped or missing Agent, and a login without `SQLAgentReaderRole` do not surface as
//! query failures. Listing and scripting run only after the probe says the tables are readable.

use rdbs_core::agent::{AgentJobDef, AgentJobSummary, AgentProbe};
use rdbs_core::{DbError, DbErrorKind, DbObject};

use crate::MssqlClient;
use crate::catalog::{col, text_rows};

/// Explorer node for this server: jobs, an empty Jobs folder, or a notice.
///
/// Never returns an error. A failure while reading jobs becomes a notice on the Agent folder
/// so the rest of the object tree still loads.
pub async fn load_node(client: &mut MssqlClient) -> DbObject {
    let probe = match probe(client).await {
        Ok(probe) => probe,
        Err(error) => {
            tracing::debug!("SQL Server Agent probe failed: {error}");
            return rdbs_core::agent::unavailable_node(rdbs_core::agent::unavailable_reason(
                error.code.as_deref(),
                &error.message,
            ));
        }
    };
    if let AgentProbe::Unavailable(message) = probe {
        return rdbs_core::agent::unavailable_node(message);
    }
    let stopped = service_stopped(client).await;
    match list_summaries(client).await {
        Ok(jobs) => rdbs_core::agent::jobs_node(&jobs, stopped),
        Err(error) => {
            tracing::debug!("SQL Server Agent jobs were not listed: {error}");
            rdbs_core::agent::unavailable_node(rdbs_core::agent::unavailable_reason(
                error.code.as_deref(),
                &error.message,
            ))
        }
    }
}

/// `CREATE` script for the job named `name` in `msdb`. Does not execute it.
pub async fn script_job(client: &mut MssqlClient, name: &str) -> Result<String, DbError> {
    let quoted = nlit(name);
    let header = text_rows(
        client,
        &format!(
            "SELECT j.name, CAST(j.enabled AS int), COALESCE(j.description, N''), \
                    j.start_step_id, j.notify_level_eventlog, j.notify_level_email, \
                    j.notify_level_netsend, j.notify_level_page, j.delete_level, \
                    COALESCE(c.name, N''), COALESCE(SUSER_SNAME(j.owner_sid), N'') \
             FROM msdb.dbo.sysjobs AS j \
             LEFT JOIN msdb.dbo.syscategories AS c ON c.category_id = j.category_id \
             WHERE j.name = {quoted}"
        ),
    )
    .await?;
    let Some(header) = header.first() else {
        return Err(DbError::new(
            DbErrorKind::Query,
            format!("SQL Server Agent job {name} was not found"),
        ));
    };
    let steps = text_rows(
        client,
        &format!(
            "SELECT s.step_id, s.step_name, s.subsystem, s.command, \
                    COALESCE(s.database_name, N''), COALESCE(s.database_user_name, N''), \
                    s.cmdexec_success_code, s.on_success_action, s.on_success_step_id, \
                    s.on_fail_action, s.on_fail_step_id, s.retry_attempts, s.retry_interval, \
                    s.os_run_priority, s.flags, COALESCE(s.output_file_name, N'') \
             FROM msdb.dbo.sysjobsteps AS s \
             JOIN msdb.dbo.sysjobs AS j ON j.job_id = s.job_id \
             WHERE j.name = {quoted} \
             ORDER BY s.step_id"
        ),
    )
    .await?;
    let schedules = text_rows(
        client,
        &format!(
            "SELECT sch.name, CAST(sch.enabled AS int), sch.freq_type, sch.freq_interval, \
                    sch.freq_subday_type, sch.freq_subday_interval, sch.freq_relative_interval, \
                    sch.freq_recurrence_factor, sch.active_start_date, sch.active_end_date, \
                    sch.active_start_time, sch.active_end_time \
             FROM msdb.dbo.sysjobschedules AS js \
             JOIN msdb.dbo.sysschedules AS sch ON sch.schedule_id = js.schedule_id \
             JOIN msdb.dbo.sysjobs AS j ON j.job_id = js.job_id \
             WHERE j.name = {quoted} \
             ORDER BY sch.name"
        ),
    )
    .await?;
    let def = AgentJobDef::from_catalog(header, &steps, &schedules).ok_or_else(|| {
        DbError::new(
            DbErrorKind::Query,
            format!("SQL Server Agent job {name} was not found"),
        )
    })?;
    Ok(rdbs_core::agent::create_script(&def))
}

/// Edition, msdb status, whether `sysjobs` exists, and SELECT permission.
///
/// `OBJECT_ID` is evaluated only while msdb is online, so an offline database does not raise.
async fn probe(client: &mut MssqlClient) -> Result<AgentProbe, DbError> {
    let rows = text_rows(
        client,
        "DECLARE @status nvarchar(60) = CONVERT(nvarchar(60), DATABASEPROPERTYEX(N'msdb', N'Status')); \
         DECLARE @id int = NULL; \
         DECLARE @perm int = NULL; \
         IF @status = N'ONLINE' \
         BEGIN \
             SET @id = OBJECT_ID(N'msdb.dbo.sysjobs', N'U'); \
             IF @id IS NOT NULL \
                 SET @perm = HAS_PERMS_BY_NAME(N'msdb.dbo.sysjobs', N'OBJECT', N'SELECT'); \
         END \
         SELECT CAST(SERVERPROPERTY(N'Edition') AS nvarchar(128)), @status, \
                CONVERT(nvarchar(20), @id), CONVERT(nvarchar(10), @perm)",
    )
    .await?;
    let row = rows.first().map(Vec::as_slice).unwrap_or(&[]);
    Ok(rdbs_core::agent::interpret_probe(
        &col(row, 0),
        &col(row, 1),
        &col(row, 2),
        &col(row, 3),
    ))
}

/// Best-effort. A missing `VIEW SERVER STATE` permission leaves the folder unmarked.
async fn service_stopped(client: &mut MssqlClient) -> bool {
    let rows = text_rows(
        client,
        "IF HAS_PERMS_BY_NAME(NULL, NULL, N'VIEW SERVER STATE') = 1 \
             EXEC(N'SELECT status_desc FROM sys.dm_server_services \
                    WHERE servicename LIKE N''SQL Server Agent%'''); \
         ELSE \
             SELECT CAST(NULL AS nvarchar(60)) AS status_desc;",
    )
    .await;
    match rows {
        Ok(rows) => rows
            .iter()
            .any(|row| rdbs_core::agent::service_is_stopped(&col(row, 0))),
        Err(error) => {
            tracing::debug!("SQL Server Agent service state was not read: {error}");
            false
        }
    }
}

async fn list_summaries(client: &mut MssqlClient) -> Result<Vec<AgentJobSummary>, DbError> {
    let rows = text_rows(
        client,
        "SELECT j.name, CAST(j.enabled AS int), \
                COALESCE(SUSER_SNAME(j.owner_sid), N''), COALESCE(c.name, N''), \
                h.run_status, h.run_date, h.run_time, n.next_run_date, n.next_run_time \
         FROM msdb.dbo.sysjobs AS j \
         LEFT JOIN msdb.dbo.syscategories AS c ON c.category_id = j.category_id \
         OUTER APPLY ( \
             SELECT TOP (1) run_status, run_date, run_time \
             FROM msdb.dbo.sysjobhistory \
             WHERE job_id = j.job_id AND step_id = 0 \
             ORDER BY instance_id DESC \
         ) AS h \
         OUTER APPLY ( \
             SELECT TOP (1) next_run_date, next_run_time \
             FROM msdb.dbo.sysjobschedules \
             WHERE job_id = j.job_id AND next_run_date > 0 \
             ORDER BY next_run_date, next_run_time \
         ) AS n \
         ORDER BY j.name",
    )
    .await?;
    let mut jobs: Vec<AgentJobSummary> = rows
        .iter()
        .filter_map(|row| AgentJobSummary::from_columns(row))
        .collect();
    match text_rows(client, ACTIVITY_SQL).await {
        Ok(activity) => {
            for row in activity {
                let name = col(&row, 0);
                if let Some(job) = jobs.iter_mut().find(|job| job.name == name) {
                    job.apply_activity(col(&row, 1) == "1", &col(&row, 2), &col(&row, 3));
                }
            }
        }
        Err(error) => {
            tracing::debug!("SQL Server Agent activity was not read: {error}");
        }
    }
    Ok(jobs)
}

/// Running state and step names. Separate from the job list so a missing permission
/// on `sysjobactivity` does not hide jobs the login can already see.
const ACTIVITY_SQL: &str = "\
SELECT j.name,
       CONVERT(nvarchar(10), CASE
           WHEN act.start_execution_date IS NOT NULL AND act.stop_execution_date IS NULL THEN 1
           ELSE 0
       END),
       COALESCE(cur.step_name, start_step.step_name, N''),
       COALESCE(steps.packed, N'')
FROM msdb.dbo.sysjobs AS j
OUTER APPLY (
    SELECT TOP (1) a.start_execution_date, a.stop_execution_date
    FROM msdb.dbo.sysjobactivity AS a
    WHERE a.job_id = j.job_id
      AND a.session_id = (SELECT MAX(session_id) FROM msdb.dbo.syssessions)
    ORDER BY a.start_execution_date DESC
) AS act
OUTER APPLY (
    SELECT TOP (1) h.step_name
    FROM msdb.dbo.sysjobhistory AS h
    WHERE h.job_id = j.job_id
      AND h.run_status = 4
      AND act.start_execution_date IS NOT NULL
      AND act.stop_execution_date IS NULL
    ORDER BY h.instance_id DESC
) AS cur
OUTER APPLY (
    SELECT TOP (1) s.step_name
    FROM msdb.dbo.sysjobsteps AS s
    WHERE s.job_id = j.job_id
      AND s.step_id = j.start_step_id
      AND act.start_execution_date IS NOT NULL
      AND act.stop_execution_date IS NULL
      AND cur.step_name IS NULL
) AS start_step
OUTER APPLY (
    SELECT STUFF((
        SELECT NCHAR(31) + CONVERT(nvarchar(10), s.step_id) + NCHAR(31)
             + REPLACE(s.step_name, NCHAR(31), N' ')
        FROM msdb.dbo.sysjobsteps AS s
        WHERE s.job_id = j.job_id
        ORDER BY s.step_id
        FOR XML PATH(''), TYPE
    ).value('.', 'nvarchar(max)'), 1, 1, N'') AS packed
) AS steps
ORDER BY j.name";

fn nlit(value: &str) -> String {
    format!("N'{}'", value.replace('\'', "''"))
}
