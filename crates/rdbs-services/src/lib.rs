//! Background worker that owns all database sessions.
//!
//! The UI talks to it exclusively through [`ServiceHandle`]: it sends [`Command`]s and drains
//! [`Event`]s once per frame. The worker runs a multi-threaded Tokio runtime on its own OS
//! thread, so database I/O can never block rendering.
//!
//! Each profile has one session slot guarded by an async mutex; operations on the same
//! connection are serialised, while different connections run concurrently.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc as std_mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rdbs_core::{
    AdapterFactory, BoxFuture, CancelHandle, Command, ConnectionProfile, CredentialStore,
    DatabaseAdapter, DbError, DbErrorKind, Event, ProfileId, RequestId,
};
use tokio::sync::{Mutex as AsyncMutex, mpsc};

type Notify = Arc<dyn Fn() + Send + Sync>;
type SessionSlot = Arc<AsyncMutex<Option<Box<dyn DatabaseAdapter>>>>;
/// A session is per profile and, for non-default databases, per database.
type SessionKey = (ProfileId, Option<String>);

/// Handle owned by the UI. Dropping it shuts the worker down and joins its thread.
pub struct ServiceHandle {
    commands: mpsc::UnboundedSender<Command>,
    events: std_mpsc::Receiver<Event>,
    next_request: AtomicU64,
    thread: Option<JoinHandle<()>>,
}

impl ServiceHandle {
    /// Starts the worker thread.
    ///
    /// `notify` is invoked after every emitted event; the UI passes
    /// `egui::Context::request_repaint` so new results are drawn immediately.
    pub fn spawn(
        factory: Arc<dyn AdapterFactory>,
        credentials: Arc<dyn CredentialStore>,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (evt_tx, evt_rx) = std_mpsc::channel();
        let emitter = Emitter {
            tx: evt_tx,
            notify: Arc::new(notify),
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rdbs-worker")
            .enable_all()
            .build()?;
        let thread = std::thread::Builder::new()
            .name("rdbs-services".into())
            .spawn(move || {
                let worker = Worker {
                    factory,
                    credentials,
                    emitter,
                    sessions: Arc::default(),
                    running: Arc::default(),
                };
                runtime.block_on(worker.run(cmd_rx));
                // Give in-flight tasks a moment to observe cancellation, then drop them.
                runtime.shutdown_timeout(Duration::from_millis(500));
            })?;
        Ok(Self {
            commands: cmd_tx,
            events: evt_rx,
            next_request: AtomicU64::new(1),
            thread: Some(thread),
        })
    }

    /// Allocates a fresh request id.
    pub fn next_request_id(&self) -> RequestId {
        RequestId(self.next_request.fetch_add(1, Ordering::Relaxed))
    }

    /// Sends a command. If the worker has stopped, the command is dropped and logged.
    pub fn send(&self, command: Command) {
        if let Err(err) = self.commands.send(command) {
            tracing::error!("worker is not running; dropped command {:?}", err.0);
        }
    }

    /// Returns the next pending event without blocking.
    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Waits up to `timeout` for the next event. Intended for tests and tools, not the UI.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }
}

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("worker thread panicked during shutdown");
        }
    }
}

/// Command name for logs (never the payload, which may contain secrets).
fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Connect { .. } => "Connect",
        Command::Disconnect { .. } => "Disconnect",
        Command::RefreshObjects { .. } => "RefreshObjects",
        Command::Execute { .. } => "Execute",
        Command::LoadDatabase { .. } => "LoadDatabase",
        Command::ApplyChanges { .. } => "ApplyChanges",
        Command::GetDdl { .. } => "GetDdl",
        Command::DescribeSchema { .. } => "DescribeSchema",
        Command::RunScript { .. } => "RunScript",
        Command::Explain { .. } => "Explain",
        Command::Transfer { .. } => "Transfer",
        Command::Backup { .. } => "Backup",
        Command::Cancel { .. } => "Cancel",
        Command::TestConnection { .. } => "TestConnection",
        Command::Shutdown => "Shutdown",
    }
}

#[derive(Clone)]
struct Emitter {
    tx: std_mpsc::Sender<Event>,
    notify: Notify,
}

impl Emitter {
    fn emit(&self, event: Event) {
        if self.tx.send(event).is_ok() {
            (self.notify)();
        }
    }
}

#[derive(Clone)]
struct Worker {
    factory: Arc<dyn AdapterFactory>,
    credentials: Arc<dyn CredentialStore>,
    emitter: Emitter,
    sessions: Arc<Mutex<HashMap<SessionKey, SessionSlot>>>,
    running: Arc<Mutex<HashMap<RequestId, CancelHandle>>>,
}

impl Worker {
    async fn run(self, mut commands: mpsc::UnboundedReceiver<Command>) {
        while let Some(command) = commands.recv().await {
            // Commands carry passwords: log only which command arrived.
            tracing::debug!(command = command_name(&command), "worker command");
            match command {
                Command::Shutdown => break,
                Command::Cancel { request_id } => self.cancel(request_id),
                Command::Connect { profile, password } => {
                    let w = self.clone();
                    tokio::spawn(async move { w.connect(profile, password).await });
                }
                Command::Disconnect { profile_id } => {
                    let w = self.clone();
                    tokio::spawn(async move { w.disconnect(profile_id).await });
                }
                Command::RefreshObjects { profile_id } => {
                    let w = self.clone();
                    tokio::spawn(async move { w.refresh(profile_id).await });
                }
                Command::Execute {
                    request_id,
                    profile,
                    sql,
                    password,
                    database,
                } => {
                    let w = self.clone();
                    tokio::spawn(async move {
                        w.execute(request_id, profile, sql, password, database)
                            .await;
                    });
                }
                Command::ApplyChanges {
                    request_id,
                    profile,
                    database,
                    changes,
                    password,
                } => {
                    let w = self.clone();
                    tokio::spawn(async move {
                        w.apply_changes(request_id, profile, database, changes, password)
                            .await;
                    });
                }
                Command::GetDdl {
                    request_id,
                    profile,
                    database,
                    object,
                    password,
                } => {
                    let w = self.clone();
                    tokio::spawn(async move {
                        w.get_ddl(request_id, profile, database, object, password)
                            .await;
                    });
                }
                Command::DescribeSchema {
                    request_id,
                    profile,
                    database,
                    schema,
                    password,
                } => {
                    let w = self.clone();
                    tokio::spawn(async move {
                        let result = w
                            .with_session(profile, database, password, move |a| {
                                Box::pin(async move { a.describe_schema(schema.as_deref()).await })
                            })
                            .await;
                        w.emitter
                            .emit(Event::SchemaDescribed { request_id, result });
                    });
                }
                Command::RunScript {
                    request_id,
                    profile,
                    database,
                    statements,
                    password,
                } => {
                    let w = self.clone();
                    tokio::spawn(async move {
                        let result = w
                            .with_session(profile, database, password, move |a| {
                                Box::pin(run_script(a, statements))
                            })
                            .await;
                        w.emitter.emit(Event::ScriptFinished { request_id, result });
                    });
                }
                Command::Explain {
                    request_id,
                    profile,
                    database,
                    sql,
                    analyze,
                    password,
                } => {
                    let w = self.clone();
                    tokio::spawn(async move {
                        let result = w
                            .with_session(profile, database, password, move |a| {
                                Box::pin(async move { a.explain(&sql, analyze).await })
                            })
                            .await;
                        w.emitter.emit(Event::PlanReady { request_id, result });
                    });
                }
                Command::Transfer {
                    request_id,
                    source,
                    source_database,
                    source_password,
                    target,
                    target_database,
                    target_password,
                    options,
                } => {
                    let w = self.clone();
                    tokio::spawn(async move {
                        w.transfer(
                            request_id,
                            (source, source_database, source_password),
                            (target, target_database, target_password),
                            options,
                        )
                        .await;
                    });
                }
                Command::Backup {
                    request_id,
                    profile,
                    database,
                    password,
                    options,
                } => {
                    let w = self.clone();
                    tokio::spawn(async move {
                        w.backup(request_id, profile, database, password, options)
                            .await;
                    });
                }
                Command::LoadDatabase {
                    profile,
                    database,
                    password,
                } => {
                    let w = self.clone();
                    tokio::spawn(async move { w.load_database(profile, database, password).await });
                }
                Command::TestConnection {
                    request_id,
                    profile,
                    password,
                } => {
                    let w = self.clone();
                    tokio::spawn(async move { w.test(request_id, profile, password).await });
                }
            }
        }
        self.close_all().await;
    }

    fn slot(&self, key: SessionKey) -> SessionSlot {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        sessions.entry(key).or_default().clone()
    }

    fn existing_slot(&self, key: &SessionKey) -> Option<SessionSlot> {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        sessions.get(key).cloned()
    }

    /// Session key and effective profile for running in `database` (if it differs from the
    /// profile's own database).
    fn target(
        profile: &ConnectionProfile,
        database: Option<String>,
    ) -> (SessionKey, ConnectionProfile) {
        match database.filter(|db| !db.is_empty() && *db != profile.database) {
            Some(db) => {
                let mut effective = profile.clone();
                effective.database.clone_from(&db);
                ((profile.id, Some(db)), effective)
            }
            None => ((profile.id, None), profile.clone()),
        }
    }

    /// Password to use: the one entered for this session, else the saved one (if the
    /// profile saves passwords). Keychain access can block, so it runs on a blocking thread.
    async fn resolve_password(
        &self,
        profile: &ConnectionProfile,
        entered: Option<String>,
    ) -> Option<String> {
        if entered.is_some() || !profile.save_password {
            return entered;
        }
        let store = self.credentials.clone();
        let id = profile.id;
        match tokio::task::spawn_blocking(move || store.get(id, rdbs_core::SecretKind::Database))
            .await
        {
            Ok(Ok(saved)) => saved,
            Ok(Err(e)) => {
                tracing::warn!("could not read saved password for {}: {e}", profile.name);
                None
            }
            Err(e) => {
                tracing::warn!("credential lookup task failed: {e}");
                None
            }
        }
    }

    /// Copy of `profile` whose SSH tunnel carries its secret (as typed, else from the keychain).
    async fn with_ssh_secret(&self, profile: &ConnectionProfile) -> ConnectionProfile {
        let mut effective = profile.clone();
        if let Some(ssh) = effective.ssh.as_mut()
            && ssh.secret.is_none()
        {
            let store = self.credentials.clone();
            let id = profile.id;
            ssh.secret =
                tokio::task::spawn_blocking(move || store.get(id, rdbs_core::SecretKind::Ssh))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .flatten();
        }
        effective
    }

    /// Ensures `session` holds an open adapter, connecting if needed. Only the profile's main
    /// session (`announce`) reports Connecting/Connected/ConnectionFailed to the UI.
    async fn ensure_connected(
        &self,
        profile: &ConnectionProfile,
        password: Option<String>,
        session: &mut Option<Box<dyn DatabaseAdapter>>,
        announce: bool,
    ) -> Result<(), DbError> {
        if session.is_some() {
            return Ok(());
        }
        if announce {
            self.emitter.emit(Event::Connecting {
                profile_id: profile.id,
            });
        }
        let password = self.resolve_password(profile, password).await;
        let profile = &self.with_ssh_secret(profile).await;
        let address = format!("{}:{}", profile.host, profile.port);
        let attempt = self.factory.connect(profile, password.as_deref()).await;
        match attempt.map_err(|e| e.with_reachability_hint(&address)) {
            Ok(adapter) => {
                if announce {
                    self.emitter.emit(Event::Connected {
                        profile_id: profile.id,
                        server_version: adapter.server_version().to_owned(),
                        capabilities: adapter.capabilities(),
                    });
                }
                *session = Some(adapter);
                Ok(())
            }
            Err(error) => {
                if announce {
                    self.emitter.emit(Event::ConnectionFailed {
                        profile_id: profile.id,
                        error: error.clone(),
                    });
                }
                Err(error)
            }
        }
    }

    async fn connect(&self, profile: ConnectionProfile, password: Option<String>) {
        let slot = self.slot((profile.id, None));
        let mut session = slot.lock().await;
        if self
            .ensure_connected(&profile, password, &mut session, true)
            .await
            .is_err()
        {
            return;
        }
        if let Some(adapter) = session.as_mut() {
            self.emit_objects(profile.id, adapter.as_mut()).await;
        }
    }

    /// Opens (or reuses) a session for another database and reports its objects.
    async fn load_database(
        &self,
        profile: ConnectionProfile,
        database: String,
        password: Option<String>,
    ) {
        let (key, effective) = Self::target(&profile, Some(database.clone()));
        let slot = self.slot(key);
        let mut session = slot.lock().await;
        let loaded = async {
            self.ensure_connected(&effective, password, &mut session, false)
                .await?;
            let adapter = session
                .as_mut()
                .ok_or_else(|| DbError::new(DbErrorKind::Internal, "session unavailable"))?;
            let tree = adapter.load_objects().await?;
            tree.into_iter()
                .find(|db| db.name == database)
                .map(|db| db.children)
                .ok_or_else(|| {
                    DbError::new(DbErrorKind::Query, format!("database {database} not found"))
                })
        }
        .await;
        let event = match loaded {
            Ok(objects) => Event::DatabaseObjectsLoaded {
                profile_id: profile.id,
                database,
                objects,
            },
            Err(error) => Event::DatabaseObjectsFailed {
                profile_id: profile.id,
                database,
                error,
            },
        };
        self.emitter.emit(event);
    }

    async fn refresh(&self, profile_id: ProfileId) {
        let Some(slot) = self.existing_slot(&(profile_id, None)) else {
            return;
        };
        let mut session = slot.lock().await;
        if let Some(adapter) = session.as_mut() {
            self.emit_objects(profile_id, adapter.as_mut()).await;
        }
    }

    async fn emit_objects(&self, profile_id: ProfileId, adapter: &mut dyn DatabaseAdapter) {
        let event = match adapter.load_objects().await {
            Ok(objects) => Event::ObjectsLoaded {
                profile_id,
                objects,
            },
            Err(error) => Event::ObjectsFailed { profile_id, error },
        };
        self.emitter.emit(event);
    }

    async fn disconnect(&self, profile_id: ProfileId) {
        let slots: Vec<SessionSlot> = {
            let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            let keys: Vec<SessionKey> = sessions
                .keys()
                .filter(|(id, _)| *id == profile_id)
                .cloned()
                .collect();
            keys.iter().filter_map(|k| sessions.remove(k)).collect()
        };
        for slot in slots {
            if let Some(mut adapter) = slot.lock().await.take() {
                adapter.close().await;
            }
        }
        self.emitter.emit(Event::Disconnected { profile_id });
    }

    fn cancel(&self, request_id: RequestId) {
        let running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handle) = running.get(&request_id) {
            handle.cancel();
        }
    }

    async fn execute(
        &self,
        request_id: RequestId,
        profile: ConnectionProfile,
        sql: String,
        password: Option<String>,
        database: Option<String>,
    ) {
        let (key, effective) = Self::target(&profile, database);
        let main_session = key.1.is_none();
        let cancel = CancelHandle::new();
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(request_id, cancel.clone());
        self.emitter.emit(Event::QueryStarted { request_id });
        let started = Instant::now();

        let result = async {
            let slot = self.slot(key);
            let mut session = slot.lock().await;
            if cancel.is_cancelled() {
                return Err(DbError::cancelled());
            }
            self.ensure_connected(&effective, password, &mut session, main_session)
                .await?;
            let result = match session.as_mut() {
                Some(adapter) => adapter.execute(&sql, cancel.clone()).await,
                None => Err(DbError::new(DbErrorKind::Internal, "session unavailable")),
            };
            // A broken connection is discarded so the next execution reconnects.
            if result
                .as_ref()
                .is_err_and(|e| e.kind == DbErrorKind::Connection)
            {
                *session = None;
                if main_session {
                    self.emitter.emit(Event::Disconnected {
                        profile_id: profile.id,
                    });
                }
            }
            result
        }
        .await;

        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&request_id);
        self.emitter.emit(Event::QueryFinished {
            request_id,
            elapsed: started.elapsed(),
            result,
        });
    }

    /// Runs `f` on the session for (`profile`, `database`), connecting first if needed. A broken
    /// connection is discarded so the next request reconnects.
    async fn with_session<T>(
        &self,
        profile: ConnectionProfile,
        database: Option<String>,
        password: Option<String>,
        f: impl for<'a> FnOnce(&'a mut dyn DatabaseAdapter) -> BoxFuture<'a, Result<T, DbError>>,
    ) -> Result<T, DbError> {
        let (key, effective) = Self::target(&profile, database);
        let main_session = key.1.is_none();
        let slot = self.slot(key);
        let mut session = slot.lock().await;
        self.ensure_connected(&effective, password, &mut session, main_session)
            .await?;
        let adapter = session
            .as_mut()
            .ok_or_else(|| DbError::new(DbErrorKind::Internal, "session unavailable"))?;
        let result = f(adapter.as_mut()).await;
        if result
            .as_ref()
            .is_err_and(|e| e.kind == DbErrorKind::Connection)
        {
            *session = None;
            if main_session {
                self.emitter.emit(Event::Disconnected {
                    profile_id: profile.id,
                });
            }
        }
        result
    }

    /// Opens a connection that belongs to one job (not shared with interactive sessions).
    async fn open_dedicated(
        &self,
        profile: &ConnectionProfile,
        database: Option<String>,
        password: Option<String>,
    ) -> Result<Box<dyn DatabaseAdapter>, DbError> {
        let (_, effective) = Self::target(profile, database);
        let password = self.resolve_password(&effective, password).await;
        let effective = self.with_ssh_secret(&effective).await;
        let address = format!("{}:{}", effective.host, effective.port);
        self.factory
            .connect(&effective, password.as_deref())
            .await
            .map_err(|e| e.with_reachability_hint(&address))
    }

    /// Registers a cancellable job and reports its start.
    fn start_job(&self, request_id: RequestId) -> CancelHandle {
        let cancel = CancelHandle::new();
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(request_id, cancel.clone());
        self.emitter.emit(Event::JobProgress {
            request_id,
            message: "Connecting…".into(),
            fraction: Some(0.0),
        });
        cancel
    }

    fn finish_job(&self, request_id: RequestId, result: Result<String, DbError>) {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&request_id);
        self.emitter.emit(Event::JobFinished { request_id, result });
    }

    async fn transfer(
        &self,
        request_id: RequestId,
        source: (ConnectionProfile, Option<String>, Option<String>),
        target: (ConnectionProfile, Option<String>, Option<String>),
        options: rdbs_core::job::TransferOptions,
    ) {
        let cancel = self.start_job(request_id);
        let result = async {
            let mut src = self.open_dedicated(&source.0, source.1, source.2).await?;
            let mut dst = self.open_dedicated(&target.0, target.1, target.2).await?;
            let emitter = self.emitter.clone();
            let outcome = rdbs_transfer::transfer::run(
                src.as_mut(),
                source.0.kind,
                dst.as_mut(),
                target.0.kind,
                &options,
                &cancel,
                |p| {
                    emitter.emit(Event::JobProgress {
                        request_id,
                        message: p.message,
                        fraction: Some(p.fraction),
                    });
                },
            )
            .await;
            src.close().await;
            dst.close().await;
            outcome.map(|tables| rdbs_transfer::transfer::summary_text(&tables))
        }
        .await;
        self.finish_job(request_id, result);
    }

    async fn backup(
        &self,
        request_id: RequestId,
        profile: ConnectionProfile,
        database: Option<String>,
        password: Option<String>,
        options: rdbs_core::job::BackupOptions,
    ) {
        let cancel = self.start_job(request_id);
        let result = async {
            let mut adapter = self.open_dedicated(&profile, database, password).await?;
            let emitter = self.emitter.clone();
            let outcome = rdbs_transfer::backup::dump(
                adapter.as_mut(),
                profile.kind,
                &options,
                &cancel,
                |p| {
                    emitter.emit(Event::JobProgress {
                        request_id,
                        message: p.message,
                        fraction: Some(p.fraction),
                    });
                },
            )
            .await;
            adapter.close().await;
            outcome.map(|(_, summary)| summary)
        }
        .await;
        self.finish_job(request_id, result);
    }

    async fn apply_changes(
        &self,
        request_id: RequestId,
        profile: ConnectionProfile,
        database: Option<String>,
        changes: Vec<rdbs_core::edit::Change>,
        password: Option<String>,
    ) {
        let (key, effective) = Self::target(&profile, database);
        let main_session = key.1.is_none();
        let slot = self.slot(key);
        let mut session = slot.lock().await;
        let result = async {
            self.ensure_connected(&effective, password, &mut session, main_session)
                .await?;
            match session.as_mut() {
                Some(adapter) => adapter.apply_changes(&changes).await,
                None => Err(DbError::new(DbErrorKind::Internal, "session unavailable")),
            }
        }
        .await;
        self.emitter
            .emit(Event::ChangesApplied { request_id, result });
    }

    async fn get_ddl(
        &self,
        request_id: RequestId,
        profile: ConnectionProfile,
        database: Option<String>,
        object: rdbs_core::schema::ObjectPath,
        password: Option<String>,
    ) {
        let (key, effective) = Self::target(&profile, database);
        let main_session = key.1.is_none();
        let slot = self.slot(key);
        let mut session = slot.lock().await;
        let result = async {
            self.ensure_connected(&effective, password, &mut session, main_session)
                .await?;
            match session.as_mut() {
                Some(adapter) => adapter.object_ddl(&object).await,
                None => Err(DbError::new(DbErrorKind::Internal, "session unavailable")),
            }
        }
        .await;
        self.emitter.emit(Event::DdlLoaded { request_id, result });
    }

    async fn test(
        &self,
        request_id: RequestId,
        profile: ConnectionProfile,
        password: Option<String>,
    ) {
        let password = self.resolve_password(&profile, password).await;
        let profile = self.with_ssh_secret(&profile).await;
        let address = format!("{}:{}", profile.host, profile.port);
        let attempt = self.factory.connect(&profile, password.as_deref()).await;
        let result = match attempt.map_err(|e| e.with_reachability_hint(&address)) {
            Ok(mut adapter) => {
                let version = adapter.server_version().to_owned();
                adapter.close().await;
                Ok(version)
            }
            Err(e) => Err(e),
        };
        self.emitter
            .emit(Event::TestFinished { request_id, result });
    }

    async fn close_all(&self) {
        let slots: Vec<SessionSlot> = {
            let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            sessions.drain().map(|(_, slot)| slot).collect()
        };
        for slot in slots {
            // Don't wait forever behind a long-running query during shutdown.
            if let Ok(mut session) = slot.try_lock()
                && let Some(mut adapter) = session.take()
            {
                adapter.close().await;
            }
        }
    }
}

/// Executes statements in order; the error of a failing statement names it.
async fn run_script(
    adapter: &mut dyn DatabaseAdapter,
    statements: Vec<String>,
) -> Result<usize, DbError> {
    for (i, statement) in statements.iter().enumerate() {
        if let Err(mut e) = adapter.execute(statement, CancelHandle::new()).await {
            let position = format!(
                "Statement {} of {} failed: {statement}",
                i + 1,
                statements.len()
            );
            e.detail = Some(match e.detail.take() {
                Some(detail) => format!("{position}\n{detail}"),
                None => position,
            });
            return Err(e);
        }
    }
    Ok(statements.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdbs_adapter_mock::MockAdapterFactory;
    use rdbs_core::{DatabaseKind, MemoryCredentials};

    const WAIT: Duration = Duration::from_secs(5);

    fn handle() -> ServiceHandle {
        ServiceHandle::spawn(
            Arc::new(MockAdapterFactory::instant()),
            Arc::new(MemoryCredentials::default()),
            || {},
        )
        .unwrap()
    }

    fn profile() -> ConnectionProfile {
        let mut p = ConnectionProfile::new(DatabaseKind::PostgreSql);
        p.name = "test".into();
        p
    }

    /// Waits for the first event matching `pred`, skipping others.
    fn wait_for(h: &ServiceHandle, mut pred: impl FnMut(&Event) -> bool) -> Event {
        let deadline = Instant::now() + WAIT;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match h.recv_timeout(remaining) {
                Some(e) if pred(&e) => return e,
                Some(_) => continue,
                None => break,
            }
        }
        panic!("timed out waiting for event");
    }

    #[test]
    fn connect_emits_connected_then_objects() {
        let h = handle();
        let p = profile();
        h.send(Command::Connect {
            profile: p.clone(),
            password: None,
        });
        let e = wait_for(&h, |e| matches!(e, Event::Connected { .. }));
        assert!(matches!(e, Event::Connected { profile_id, .. } if profile_id == p.id));
        let e = wait_for(&h, |e| matches!(e, Event::ObjectsLoaded { .. }));
        let Event::ObjectsLoaded { objects, .. } = e else {
            unreachable!()
        };
        assert!(!objects.is_empty());
    }

    #[test]
    fn execute_connects_lazily_and_returns_rows() {
        let h = handle();
        let id = h.next_request_id();
        h.send(Command::Execute {
            request_id: id,
            profile: profile(),
            sql: "select * from customers limit 5".into(),
            password: None,
            database: None,
        });
        let e = wait_for(&h, |e| matches!(e, Event::QueryFinished { .. }));
        let Event::QueryFinished {
            request_id, result, ..
        } = e
        else {
            unreachable!()
        };
        assert_eq!(request_id, id);
        assert_eq!(result.unwrap().last_result().unwrap().rows.len(), 5);
    }

    #[test]
    fn cancel_aborts_running_query() {
        let h = handle();
        let id = h.next_request_id();
        h.send(Command::Execute {
            request_id: id,
            profile: profile(),
            sql: "select pg_sleep(10)".into(),
            password: None,
            database: None,
        });
        wait_for(&h, |e| matches!(e, Event::QueryStarted { .. }));
        h.send(Command::Cancel { request_id: id });
        let e = wait_for(&h, |e| matches!(e, Event::QueryFinished { .. }));
        let Event::QueryFinished { result, .. } = e else {
            unreachable!()
        };
        assert_eq!(result.unwrap_err().kind, DbErrorKind::Cancelled);
    }

    #[test]
    fn test_connection_reports_failure() {
        let h = handle();
        let mut p = profile();
        p.host = "fail.invalid".into();
        let id = h.next_request_id();
        h.send(Command::TestConnection {
            request_id: id,
            profile: p,
            password: None,
        });
        let e = wait_for(&h, |e| matches!(e, Event::TestFinished { .. }));
        let Event::TestFinished { result, .. } = e else {
            unreachable!()
        };
        assert_eq!(result.unwrap_err().kind, DbErrorKind::Connection);
    }

    #[test]
    fn disconnect_emits_event() {
        let h = handle();
        let p = profile();
        h.send(Command::Connect {
            profile: p.clone(),
            password: None,
        });
        wait_for(&h, |e| matches!(e, Event::ObjectsLoaded { .. }));
        h.send(Command::Disconnect { profile_id: p.id });
        wait_for(&h, |e| matches!(e, Event::Disconnected { .. }));
    }

    /// Records the password each connection attempt receives, then defers to the mock.
    /// Records (database password, SSH secret) of each connection attempt.
    #[allow(clippy::type_complexity)]
    struct RecordingFactory(
        Mutex<Vec<(Option<String>, Option<String>)>>,
        MockAdapterFactory,
    );

    impl Default for RecordingFactory {
        fn default() -> Self {
            Self(Mutex::default(), MockAdapterFactory::instant())
        }
    }

    impl AdapterFactory for RecordingFactory {
        fn connect<'a>(
            &'a self,
            profile: &'a ConnectionProfile,
            password: Option<&'a str>,
        ) -> rdbs_core::BoxFuture<'a, Result<Box<dyn DatabaseAdapter>, DbError>> {
            self.0.lock().unwrap().push((
                password.map(str::to_owned),
                profile.ssh.as_ref().and_then(|s| s.secret.clone()),
            ));
            self.1.connect(profile, password)
        }
    }

    #[test]
    fn passwords_come_from_session_or_keychain() {
        let factory = Arc::new(RecordingFactory::default());
        let credentials = Arc::new(MemoryCredentials::default());
        let h = ServiceHandle::spawn(factory.clone(), credentials.clone(), || {}).unwrap();

        let saved = profile();
        credentials
            .set(saved.id, rdbs_core::SecretKind::Database, "from-keychain")
            .unwrap();
        h.send(Command::Connect {
            profile: saved.clone(),
            password: None,
        });
        wait_for(&h, |e| matches!(e, Event::Connected { .. }));

        let mut typed = profile();
        typed.id = rdbs_core::ProfileId::new();
        typed.save_password = false;
        credentials
            .set(
                typed.id,
                rdbs_core::SecretKind::Database,
                "must-not-be-used",
            )
            .unwrap();
        h.send(Command::Connect {
            profile: typed,
            password: Some("typed".into()),
        });
        wait_for(&h, |e| matches!(e, Event::Connected { .. }));

        let seen: Vec<Option<String>> = factory
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        assert_eq!(
            seen,
            vec![Some("from-keychain".to_owned()), Some("typed".to_owned())]
        );
    }

    #[test]
    fn ssh_secret_is_filled_from_the_keychain() {
        let factory = Arc::new(RecordingFactory::default());
        let credentials = Arc::new(MemoryCredentials::default());
        let h = ServiceHandle::spawn(factory.clone(), credentials.clone(), || {}).unwrap();
        let mut p = profile();
        p.ssh = Some(rdbs_core::SshTunnel {
            host: "bastion".into(),
            username: "me".into(),
            ..rdbs_core::SshTunnel::default()
        });
        credentials
            .set(p.id, rdbs_core::SecretKind::Ssh, "tunnel-pw")
            .unwrap();
        h.send(Command::Connect {
            profile: p,
            password: None,
        });
        wait_for(&h, |e| matches!(e, Event::Connected { .. }));
        let seen = factory.0.lock().unwrap().clone();
        assert_eq!(seen[0].1.as_deref(), Some("tunnel-pw"));
    }

    #[test]
    fn describes_schema_and_runs_scripts() {
        let h = handle();
        let id = h.next_request_id();
        h.send(Command::DescribeSchema {
            request_id: id,
            profile: profile(),
            database: None,
            schema: Some("audit".into()),
            password: None,
        });
        let e = wait_for(&h, |e| matches!(e, Event::SchemaDescribed { .. }));
        let Event::SchemaDescribed { request_id, result } = e else {
            unreachable!()
        };
        assert_eq!(request_id, id);
        let tables = result.unwrap();
        assert!(tables.iter().all(|t| t.schema.as_deref() == Some("audit")));
        assert!(tables.iter().any(|t| !t.foreign_keys.is_empty()));

        let id = h.next_request_id();
        h.send(Command::RunScript {
            request_id: id,
            profile: profile(),
            database: None,
            statements: vec!["CREATE TABLE a (x int)".into(), "this is an error".into()],
            password: None,
        });
        let e = wait_for(&h, |e| matches!(e, Event::ScriptFinished { .. }));
        let Event::ScriptFinished { result, .. } = e else {
            unreachable!()
        };
        let detail = result.unwrap_err().detail.unwrap_or_default();
        assert!(detail.starts_with("Statement 2 of 2 failed"), "{detail}");
    }

    #[test]
    fn transfer_job_reports_progress_and_finishes() {
        let h = handle();
        let id = h.next_request_id();
        let mut target = profile();
        target.id = rdbs_core::ProfileId::new();
        h.send(Command::Transfer {
            request_id: id,
            source: profile(),
            source_database: None,
            source_password: None,
            target,
            target_database: None,
            target_password: None,
            options: rdbs_core::job::TransferOptions {
                tables: vec!["customers".into()],
                ..rdbs_core::job::TransferOptions::default()
            },
        });
        wait_for(
            &h,
            |e| matches!(e, Event::JobProgress { request_id, .. } if *request_id == id),
        );
        let e = wait_for(&h, |e| matches!(e, Event::JobFinished { .. }));
        let Event::JobFinished { result, .. } = e else {
            unreachable!()
        };
        let summary = result.unwrap();
        assert!(
            summary.starts_with("1 table(s): 1000 inserted"),
            "{summary}"
        );
    }

    #[test]
    fn other_databases_load_in_their_own_session() {
        let h = handle();
        let p = profile();
        h.send(Command::LoadDatabase {
            profile: p.clone(),
            database: "analytics".into(),
            password: None,
        });
        let e = wait_for(&h, |e| {
            matches!(
                e,
                Event::DatabaseObjectsLoaded { .. }
                    | Event::DatabaseObjectsFailed { .. }
                    | Event::Connected { .. }
            )
        });
        let Event::DatabaseObjectsLoaded {
            database, objects, ..
        } = e
        else {
            panic!("expected objects of the other database, got {e:?}");
        };
        assert_eq!(database, "analytics");
        assert!(!objects.is_empty());

        h.send(Command::LoadDatabase {
            profile: p,
            database: "missing_db".into(),
            password: None,
        });
        let e = wait_for(&h, |e| matches!(e, Event::DatabaseObjectsFailed { .. }));
        assert!(
            matches!(e, Event::DatabaseObjectsFailed { database, .. } if database == "missing_db")
        );
    }
}
