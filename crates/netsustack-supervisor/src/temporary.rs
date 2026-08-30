use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use netsustack_domain::{PreferredShell, TemporaryJobState, TemporaryJobStatus, new_temporary_id};
use thiserror::Error;
use tokio::{
    sync::{Mutex as AsyncMutex, mpsc, oneshot, watch},
    time::{Instant, MissedTickBehavior},
};

use crate::{
    logs::PlainLogStore,
    runtime::{
        ManagedProcess, ProcessBackend, ProcessStopResult, RuntimeError, RuntimeSpec, SpawnRequest,
    },
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const COMPLETED_RETENTION: Duration = Duration::from_secs(60 * 60);

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);
const COMMAND_CAPACITY: usize = 4;

#[async_trait]
pub trait TemporaryClock: Send + Sync + 'static {
    fn monotonic_now(&self) -> Instant;
    fn utc_now(&self) -> DateTime<Utc>;
    async fn sleep_until(&self, deadline: Instant);
}

#[derive(Debug, Default)]
pub struct SystemTemporaryClock;

#[async_trait]
impl TemporaryClock for SystemTemporaryClock {
    fn monotonic_now(&self) -> Instant {
        Instant::now()
    }

    fn utc_now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    async fn sleep_until(&self, deadline: Instant) {
        tokio::time::sleep_until(deadline).await;
    }
}

#[derive(Debug, Clone)]
pub struct TemporaryJobSpec {
    pub name: Option<String>,
    pub command: String,
    pub directory: PathBuf,
    pub environment: HashMap<String, String>,
    pub port: Option<u16>,
    pub timeout: Duration,
    pub preferred_shell: PreferredShell,
}

impl TemporaryJobSpec {
    pub fn new(command: impl Into<String>, directory: impl Into<PathBuf>) -> Self {
        Self {
            name: None,
            command: command.into(),
            directory: directory.into(),
            environment: HashMap::new(),
            port: None,
            timeout: DEFAULT_TIMEOUT,
            preferred_shell: PreferredShell::Auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TemporaryJobResult {
    pub status: TemporaryJobStatus,
    pub logs: Vec<String>,
}

#[derive(Debug, Error)]
pub enum TemporaryError {
    #[error("temporary command cannot be empty")]
    EmptyCommand,
    #[error("temporary timeout must be between one second and {max_seconds} seconds")]
    InvalidTimeout { max_seconds: u64 },
    #[error("action {action} was not found on server {server}")]
    ActionNotFound { server: String, action: String },
    #[error("temporary job {id} was not found")]
    NotFound { id: String },
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

#[derive(Clone)]
pub struct TemporarySupervisor {
    backend: Arc<dyn ProcessBackend>,
    clock: Arc<dyn TemporaryClock>,
    jobs: Arc<AsyncMutex<HashMap<String, JobRecord>>>,
    next_sequence: Arc<AtomicU64>,
    log_buffer_lines: usize,
    lifecycle: Arc<SupervisorLifecycle>,
}

struct SupervisorLifecycle {
    shutdown: watch::Sender<bool>,
}

impl Drop for SupervisorLifecycle {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
    }
}

impl TemporarySupervisor {
    pub fn new(backend: Arc<dyn ProcessBackend>, log_buffer_lines: usize) -> Self {
        Self::with_clock(backend, Arc::new(SystemTemporaryClock), log_buffer_lines)
    }

    pub fn with_clock(
        backend: Arc<dyn ProcessBackend>,
        clock: Arc<dyn TemporaryClock>,
        log_buffer_lines: usize,
    ) -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            backend,
            clock,
            jobs: Arc::new(AsyncMutex::new(HashMap::new())),
            next_sequence: Arc::new(AtomicU64::new(0)),
            log_buffer_lines,
            lifecycle: Arc::new(SupervisorLifecycle { shutdown }),
        }
    }

    pub async fn run(&self, spec: TemporaryJobSpec) -> Result<TemporaryJobStatus, TemporaryError> {
        validate_command(&spec.command)?;
        validate_timeout(spec.timeout)?;
        let name = spec
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| derived_name(&spec.command));
        let request = SpawnRequest {
            command: spec.command,
            cwd: spec.directory,
            environment: spec.environment,
            preferred_shell: spec.preferred_shell,
            server_name: name.clone(),
            port: spec.port,
        };
        self.run_request(name, request, spec.timeout).await
    }

    pub async fn run_action(
        &self,
        server: &RuntimeSpec,
        action_name: &str,
        timeout: Duration,
    ) -> Result<TemporaryJobStatus, TemporaryError> {
        validate_timeout(timeout)?;
        let action = server
            .config
            .actions
            .iter()
            .find(|action| action.name.eq_ignore_ascii_case(action_name))
            .ok_or_else(|| TemporaryError::ActionNotFound {
                server: server.config.name.clone(),
                action: action_name.to_owned(),
            })?;
        validate_command(&action.command)?;
        let request = server.spawn_request_with_command(action.command.clone());
        self.run_request(action.name.clone(), request, timeout)
            .await
    }

    pub async fn status(&self, id: &str) -> Option<TemporaryJobStatus> {
        let jobs = self.jobs.lock().await;
        let status = jobs.get(id)?.status.borrow().clone();
        Some(status)
    }

    pub async fn list(&self) -> Vec<TemporaryJobStatus> {
        let jobs = self.jobs.lock().await;
        let mut ordered = jobs.values().collect::<Vec<_>>();
        ordered.sort_unstable_by_key(|job| job.sequence);
        ordered
            .into_iter()
            .map(|job| job.status.borrow().clone())
            .collect()
    }

    pub async fn wait(&self, id: &str) -> Result<TemporaryJobResult, TemporaryError> {
        let job = self.job(id).await?;
        wait_for_job(&job, id).await
    }

    pub async fn logs(&self, id: &str, tail: usize) -> Result<Vec<String>, TemporaryError> {
        let job = self.job(id).await?;
        Ok(job
            .logs
            .lock()
            .expect("temporary log store poisoned")
            .tail(tail))
    }

    pub async fn stop(&self, id: &str) -> Result<TemporaryJobStatus, TemporaryError> {
        let job = self.job(id).await?;
        if *job.completed.borrow() {
            return Ok(job.status.borrow().clone());
        }
        let (reply, response) = oneshot::channel();
        if job.commands.send(JobCommand::Stop { reply }).await.is_err() {
            return Ok(wait_for_job(&job, id).await?.status);
        }
        match response.await {
            Ok(status) => Ok(status),
            Err(_) => Ok(wait_for_job(&job, id).await?.status),
        }
    }

    #[doc(hidden)]
    /// Diagnostic seam that only reads the registry; it never triggers eviction.
    pub async fn retained_job_count(&self) -> usize {
        self.jobs.lock().await.len()
    }

    async fn run_request(
        &self,
        name: String,
        request: SpawnRequest,
        timeout: Duration,
    ) -> Result<TemporaryJobStatus, TemporaryError> {
        let id = new_temporary_id();
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        let started_at = self.clock.utc_now();
        let deadline = started_at
            + chrono::Duration::from_std(timeout)
                .expect("the validated seven-day timeout always fits chrono");
        let deadline_instant = self.clock.monotonic_now() + timeout;
        let process = self.backend.spawn(request.clone()).await?;
        let status = TemporaryJobStatus {
            id: id.clone(),
            name,
            command: request.command,
            directory: request.cwd.to_string_lossy().into_owned(),
            state: TemporaryJobState::Running,
            pid: Some(process.pid()),
            started_at: Some(started_at),
            finished_at: None,
            timeout_seconds: timeout.as_secs(),
            deadline: Some(deadline),
            exit_code: None,
            error: None,
        };
        let (status_tx, status_rx) = watch::channel(status.clone());
        let (completed_tx, completed_rx) = watch::channel(false);
        let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let completed_at = Arc::new(Mutex::new(None));
        let logs = Arc::new(Mutex::new(PlainLogStore::memory(self.log_buffer_lines)));
        self.jobs.lock().await.insert(
            id.clone(),
            JobRecord {
                sequence,
                status: status_rx,
                completed: completed_rx,
                commands,
                completed_at: completed_at.clone(),
                logs: logs.clone(),
            },
        );
        tokio::spawn(run_job(
            process,
            JobActor {
                status: status.clone(),
                deadline: deadline_instant,
                clock: self.clock.clone(),
                status_tx,
                completed_tx,
                completed_at,
                logs,
                job_id: id,
                jobs: Arc::downgrade(&self.jobs),
                shutdown: self.lifecycle.shutdown.subscribe(),
                log_error: None,
            },
            command_rx,
        ));
        Ok(status)
    }

    async fn job(&self, id: &str) -> Result<JobRecord, TemporaryError> {
        self.jobs
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| TemporaryError::NotFound { id: id.to_owned() })
    }
}

#[derive(Clone)]
struct JobRecord {
    sequence: u64,
    status: watch::Receiver<TemporaryJobStatus>,
    completed: watch::Receiver<bool>,
    commands: mpsc::Sender<JobCommand>,
    completed_at: Arc<Mutex<Option<Instant>>>,
    logs: Arc<Mutex<PlainLogStore>>,
}

impl JobRecord {
    fn result(&self) -> TemporaryJobResult {
        TemporaryJobResult {
            status: self.status.borrow().clone(),
            logs: self
                .logs
                .lock()
                .expect("temporary log store poisoned")
                .tail(usize::MAX),
        }
    }
}

enum JobCommand {
    Stop {
        reply: oneshot::Sender<TemporaryJobStatus>,
    },
}

struct JobActor {
    status: TemporaryJobStatus,
    deadline: Instant,
    clock: Arc<dyn TemporaryClock>,
    status_tx: watch::Sender<TemporaryJobStatus>,
    completed_tx: watch::Sender<bool>,
    completed_at: Arc<Mutex<Option<Instant>>>,
    logs: Arc<Mutex<PlainLogStore>>,
    job_id: String,
    jobs: Weak<AsyncMutex<HashMap<String, JobRecord>>>,
    shutdown: watch::Receiver<bool>,
    log_error: Option<String>,
}

impl JobActor {
    fn capture_output(&mut self, process: &mut dyn ManagedProcess) {
        record_first_error(
            &mut self.log_error,
            capture_output(process, self.logs.as_ref()),
        );
    }

    fn capture_bytes(&mut self, output: &[u8]) {
        record_first_error(
            &mut self.log_error,
            ingest_output(output, self.logs.as_ref()),
        );
    }

    fn finish_logs(&mut self) {
        record_first_error(&mut self.log_error, finish_logs(self.logs.as_ref()));
    }

    fn publish_timeout(&mut self) {
        self.status.state = TemporaryJobState::TimedOut;
        self.status.exit_code = Some(124);
        self.status.error = Some("deadline reached".to_owned());
        self.status_tx.send_replace(self.status.clone());
    }

    fn complete(
        &mut self,
        state: TemporaryJobState,
        exit_code: Option<i32>,
        error: Option<String>,
    ) {
        self.status.state = state;
        self.status.pid = None;
        self.status.finished_at = Some(self.clock.utc_now());
        self.status.exit_code = exit_code;
        self.status.error = error;
        let completed = self.clock.monotonic_now();
        *self
            .completed_at
            .lock()
            .expect("temporary completion clock poisoned") = Some(completed);
        self.status_tx.send_replace(self.status.clone());
        schedule_eviction(
            self.clock.clone(),
            self.jobs.clone(),
            self.job_id.clone(),
            completed,
            self.shutdown.clone(),
        );
        self.completed_tx.send_replace(true);
    }
}

async fn run_job(
    mut process: Box<dyn ManagedProcess>,
    mut actor: JobActor,
    mut commands: mpsc::Receiver<JobCommand>,
) {
    let mut poll = tokio::time::interval(PROCESS_POLL_INTERVAL);
    poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            command = commands.recv() => {
                actor.capture_output(process.as_mut());
                let Some(JobCommand::Stop { reply }) = command else {
                    let _ = stop_managed_process(process).await;
                    return;
                };
                let stop_error = finish_stopped_process(&mut actor, stop_managed_process(process).await);
                actor.finish_logs();
                let error = combine_errors(
                    stop_error,
                    actor.log_error.take(),
                );
                actor.complete(
                    TemporaryJobState::Stopped,
                    Some(130),
                    error,
                );
                let _ = reply.send(actor.status);
                return;
            }
            _ = poll.tick() => {
                actor.capture_output(process.as_mut());
                match process.try_wait() {
                    Ok(Some(code)) => {
                        actor.capture_output(process.as_mut());
                        drop_managed_process(process).await;
                        actor.finish_logs();
                        let state = if code == 0 {
                            TemporaryJobState::Succeeded
                        } else {
                            TemporaryJobState::Failed
                        };
                        let error = actor.log_error.take();
                        actor.complete(state, Some(code), error);
                        return;
                    }
                    Ok(None) if actor.clock.monotonic_now() >= actor.deadline => {
                        actor.publish_timeout();
                        let stop_error = finish_stopped_process(
                            &mut actor,
                            stop_managed_process(process).await,
                        );
                        actor.finish_logs();
                        let error = combine_errors(
                            Some("deadline reached".to_owned()),
                            combine_errors(
                                stop_error.map(|error| format!("process cleanup failed: {error}")),
                                actor.log_error.take(),
                            ),
                        );
                        actor.complete(TemporaryJobState::TimedOut, Some(124), error);
                        return;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        actor.capture_output(process.as_mut());
                        let cleanup_error = finish_stopped_process(
                            &mut actor,
                            stop_managed_process(process).await,
                        );
                        actor.finish_logs();
                        let log_error = actor.log_error.take();
                        actor.complete(
                            TemporaryJobState::Failed,
                            Some(1),
                            combine_errors(
                                Some(error.to_string()),
                                combine_errors(
                                    cleanup_error.map(|error| {
                                        format!("process cleanup failed: {error}")
                                    }),
                                    log_error,
                                ),
                            ),
                        );
                        return;
                    }
                }
            }
        }
    }
}

fn schedule_eviction(
    clock: Arc<dyn TemporaryClock>,
    jobs: Weak<AsyncMutex<HashMap<String, JobRecord>>>,
    job_id: String,
    completed_at: Instant,
    mut shutdown: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        tokio::select! {
            _ = clock.sleep_until(completed_at + COMPLETED_RETENTION) => {}
            _ = shutdown.changed() => return,
        }
        let Some(jobs) = jobs.upgrade() else {
            return;
        };
        let mut jobs = jobs.lock().await;
        let is_same_completion = jobs.get(&job_id).is_some_and(|job| {
            *job.completed_at
                .lock()
                .expect("temporary completion clock poisoned")
                == Some(completed_at)
        });
        if is_same_completion {
            jobs.remove(&job_id);
        }
    });
}

fn capture_output(
    process: &mut dyn ManagedProcess,
    logs: &Mutex<PlainLogStore>,
) -> Result<(), String> {
    let output = process.drain_output().map_err(|error| error.to_string())?;
    ingest_output(&output, logs)
}

fn ingest_output(output: &[u8], logs: &Mutex<PlainLogStore>) -> Result<(), String> {
    if output.is_empty() {
        return Ok(());
    }
    logs.lock()
        .expect("temporary log store poisoned")
        .ingest(output)
        .map_err(|error| error.to_string())
}

fn finish_logs(logs: &Mutex<PlainLogStore>) -> Result<(), String> {
    logs.lock()
        .expect("temporary log store poisoned")
        .finish()
        .map_err(|error| error.to_string())
}

fn record_first_error(first: &mut Option<String>, result: Result<(), String>) {
    if first.is_none() {
        *first = result.err();
    }
}

fn combine_errors(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(first), Some(second)) => Some(format!("{first}; {second}")),
        (Some(error), None) | (None, Some(error)) => Some(error),
        (None, None) => None,
    }
}

async fn stop_managed_process(
    process: Box<dyn ManagedProcess>,
) -> Result<ProcessStopResult, RuntimeError> {
    tokio::task::spawn_blocking(move || process.stop())
        .await
        .map_err(|error| RuntimeError::Backend(format!("stop task failed: {error}")))
}

fn finish_stopped_process(
    actor: &mut JobActor,
    stopped: Result<ProcessStopResult, RuntimeError>,
) -> Option<String> {
    match stopped {
        Ok(stopped) => {
            actor.capture_bytes(&stopped.final_output);
            actor.log_error = combine_errors(
                actor.log_error.take(),
                stopped.output_error.map(|error| error.to_string()),
            );
            stopped.cleanup_error.map(|error| error.to_string())
        }
        Err(error) => Some(error.to_string()),
    }
}

async fn wait_for_job(job: &JobRecord, id: &str) -> Result<TemporaryJobResult, TemporaryError> {
    let mut completed = job.completed.clone();
    while !*completed.borrow_and_update() {
        completed
            .changed()
            .await
            .map_err(|_| TemporaryError::NotFound { id: id.to_owned() })?;
    }
    Ok(job.result())
}

async fn drop_managed_process(process: Box<dyn ManagedProcess>) {
    let _ = tokio::task::spawn_blocking(move || drop(process)).await;
}

fn validate_command(command: &str) -> Result<(), TemporaryError> {
    if command.trim().is_empty() {
        Err(TemporaryError::EmptyCommand)
    } else {
        Ok(())
    }
}

fn validate_timeout(timeout: Duration) -> Result<(), TemporaryError> {
    if timeout < Duration::from_secs(1) || timeout > MAX_TIMEOUT {
        Err(TemporaryError::InvalidTimeout {
            max_seconds: MAX_TIMEOUT.as_secs(),
        })
    } else {
        Ok(())
    }
}

fn derived_name(command: &str) -> String {
    command
        .split_whitespace()
        .take(4)
        .collect::<Vec<_>>()
        .join(" ")
}
