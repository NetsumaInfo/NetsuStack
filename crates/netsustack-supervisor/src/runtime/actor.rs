use std::{sync::Arc, time::Duration};

use chrono::Utc;
use netsustack_domain::{ServerConfig, ServerState, ServerStatus};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};

use super::{ManagedProcess, ProcessBackend, RuntimeCommand, RuntimeError, RuntimeSpec};
use crate::backoff::RestartBackoff;

const INITIAL_HEALTH_INTERVAL: Duration = Duration::from_secs(1);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);
const HEALTH_EVENT_CAPACITY: usize = 8;

pub(super) async fn run(
    spec: RuntimeSpec,
    backend: Arc<dyn ProcessBackend>,
    status_tx: watch::Sender<ServerStatus>,
    commands: mpsc::Receiver<RuntimeCommand>,
) {
    let (health_tx, health_rx) = mpsc::channel(HEALTH_EVENT_CAPACITY);
    RuntimeCore::new(spec, backend, status_tx, health_tx)
        .run(commands, health_rx)
        .await;
}

struct HealthResult {
    generation: u64,
    healthy: bool,
}

struct RuntimeCore {
    spec: RuntimeSpec,
    backend: Arc<dyn ProcessBackend>,
    status: ServerStatus,
    status_tx: watch::Sender<ServerStatus>,
    process: Option<Box<dyn ManagedProcess>>,
    next_health: Option<Instant>,
    restart_at: Option<Instant>,
    consecutive_health_failures: u8,
    healthy_since: Option<Instant>,
    health_generation: u64,
    health_task: Option<JoinHandle<()>>,
    health_tx: mpsc::Sender<HealthResult>,
}

impl RuntimeCore {
    fn new(
        spec: RuntimeSpec,
        backend: Arc<dyn ProcessBackend>,
        status_tx: watch::Sender<ServerStatus>,
        health_tx: mpsc::Sender<HealthResult>,
    ) -> Self {
        Self {
            status: initial_status(&spec),
            spec,
            backend,
            status_tx,
            process: None,
            next_health: None,
            restart_at: None,
            consecutive_health_failures: 0,
            healthy_since: None,
            health_generation: 0,
            health_task: None,
            health_tx,
        }
    }

    async fn run(
        mut self,
        mut commands: mpsc::Receiver<RuntimeCommand>,
        mut health_rx: mpsc::Receiver<HealthResult>,
    ) {
        let mut poll = tokio::time::interval(PROCESS_POLL_INTERVAL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else {
                        let _ = self.stop_process().await;
                        break;
                    };
                    if self.handle_command(command).await {
                        break;
                    }
                }
                Some(result) = health_rx.recv() => self.handle_health_result(result).await,
                _ = poll.tick(), if self.needs_tick() => self.tick().await,
            }
        }
        self.cancel_health_probe();
    }

    fn needs_tick(&self) -> bool {
        self.process.is_some() || self.restart_at.is_some() || self.next_health.is_some()
    }

    async fn handle_command(&mut self, command: RuntimeCommand) -> bool {
        match command {
            RuntimeCommand::Start { reply } => {
                let result = self.manual_start().await;
                let _ = reply.send(result);
            }
            RuntimeCommand::Stop { reply } => {
                let result = self.stop_process().await;
                let _ = reply.send(result);
            }
            RuntimeCommand::Restart { reply } => {
                self.reset_restart_budget();
                let result = match self.stop_process().await {
                    Ok(()) => self.spawn_process().await,
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
            RuntimeCommand::Input { bytes, reply } => {
                let result = self
                    .process
                    .as_mut()
                    .ok_or(RuntimeError::NotRunning)
                    .and_then(|process| process.input(&bytes));
                let _ = reply.send(result);
            }
            RuntimeCommand::Resize {
                columns,
                rows,
                reply,
            } => {
                let result = self
                    .process
                    .as_mut()
                    .ok_or(RuntimeError::NotRunning)
                    .and_then(|process| process.resize(columns, rows));
                let _ = reply.send(result);
            }
            RuntimeCommand::UpdateConfig { spec, reply } => {
                self.update_config(*spec);
                let _ = reply.send(Ok(()));
            }
            RuntimeCommand::Shutdown { reply } => {
                let result = self.stop_process().await;
                let _ = reply.send(result);
                return true;
            }
        }
        false
    }

    async fn manual_start(&mut self) -> Result<(), RuntimeError> {
        if self.process.is_some() {
            return Ok(());
        }
        self.reset_restart_budget();
        self.spawn_process().await
    }

    async fn spawn_process(&mut self) -> Result<(), RuntimeError> {
        if self.process.is_some() {
            return Ok(());
        }
        self.cancel_health_probe();
        self.restart_at = None;
        self.status.state = ServerState::Starting;
        self.status.healthy = false;
        self.status.last_error = None;
        self.status.last_exit_code = None;
        self.consecutive_health_failures = 0;
        self.healthy_since = None;
        self.publish();
        match self.backend.spawn(self.spec.spawn_request()).await {
            Ok(process) => {
                self.status.pid = Some(process.pid());
                self.status.started_at = Some(Utc::now());
                self.process = Some(process);
                let now = Instant::now();
                if has_health_probe(&self.spec.config) {
                    self.next_health = Some(now + INITIAL_HEALTH_INTERVAL);
                } else {
                    self.status.state = ServerState::Running;
                    self.status.healthy = true;
                    self.healthy_since = Some(now);
                }
                self.publish();
                Ok(())
            }
            Err(error) => {
                self.status.last_error = Some(error.to_string());
                self.status.state = ServerState::Failed;
                self.status.pid = None;
                self.next_health = None;
                self.publish();
                Err(error)
            }
        }
    }

    async fn stop_process(&mut self) -> Result<(), RuntimeError> {
        self.cancel_health_probe();
        self.restart_at = None;
        self.healthy_since = None;
        self.consecutive_health_failures = 0;
        let result = match self.process.take() {
            Some(process) => stop_managed_process(process).await,
            None => Ok(()),
        };
        self.status.pid = None;
        self.status.healthy = false;
        self.status.started_at = None;
        self.status.state = ServerState::Stopped;
        if let Err(error) = &result {
            self.status.last_error = Some(error.to_string());
        }
        self.publish();
        result
    }

    async fn tick(&mut self) {
        let now = Instant::now();
        if self.healthy_since.is_some_and(|healthy_since| {
            now.duration_since(healthy_since) > RestartBackoff::HEALTHY_RESET_AFTER
        }) && self.status.restart_count != 0
        {
            self.status.restart_count = 0;
            self.publish();
        }

        let wait_result = self.process.as_mut().map(|process| process.try_wait());
        match wait_result {
            Some(Ok(Some(code))) => self.process_exited(code).await,
            Some(Err(error)) => self.process_wait_failed(error).await,
            _ => {}
        }

        if self.restart_at.is_some_and(|deadline| now >= deadline) {
            let _ = self.spawn_process().await;
        }
        if self.process.is_some() && self.next_health.is_some_and(|deadline| now >= deadline) {
            self.launch_health_probe();
        }
    }

    async fn process_exited(&mut self, code: i32) {
        let previous_state = self.status.state;
        self.cancel_health_probe();
        if previous_state == ServerState::Unhealthy && self.spec.config.auto_restart {
            self.status.state = ServerState::Restarting;
            self.publish();
        }
        if let Some(process) = self.process.take() {
            drop_managed_process(process).await;
        }
        self.status.pid = None;
        self.status.healthy = false;
        self.status.last_exit_code = Some(code);
        self.status.started_at = None;
        self.healthy_since = None;
        if previous_state == ServerState::Starting {
            self.status.state = ServerState::Failed;
            self.status.last_error =
                Some(format!("process exited during startup with code {code}"));
            self.publish();
        } else if self.spec.config.auto_restart {
            self.schedule_restart(format!("process exited with code {code}"));
        } else {
            self.status.state = ServerState::Stopped;
            self.publish();
        }
    }

    fn launch_health_probe(&mut self) {
        self.cancel_health_probe();
        self.health_generation = self.health_generation.wrapping_add(1);
        let generation = self.health_generation;
        let backend = self.backend.clone();
        let config = self.spec.config.clone();
        let results = self.health_tx.clone();
        self.health_task = Some(tokio::spawn(async move {
            let healthy = backend.check_health(&config).await;
            let _ = results
                .send(HealthResult {
                    generation,
                    healthy,
                })
                .await;
        }));
    }

    async fn handle_health_result(&mut self, result: HealthResult) {
        if result.generation != self.health_generation || self.process.is_none() {
            return;
        }
        self.health_task = None;
        let now = Instant::now();
        if result.healthy {
            self.status.healthy = true;
            self.status.state = ServerState::Running;
            self.consecutive_health_failures = 0;
            self.healthy_since.get_or_insert(now);
            self.next_health = Some(now + self.health_interval());
            self.publish();
            return;
        }

        self.status.healthy = false;
        if self.status.state == ServerState::Starting {
            self.next_health = Some(now + INITIAL_HEALTH_INTERVAL);
            self.publish();
            return;
        }
        self.healthy_since = None;
        self.consecutive_health_failures = self.consecutive_health_failures.saturating_add(1);
        self.status.state = ServerState::Unhealthy;
        self.next_health = Some(now + self.health_interval());
        self.publish();
        if self.consecutive_health_failures >= 3 && self.spec.config.auto_restart {
            self.consecutive_health_failures = 0;
            self.cancel_health_probe();
            self.status.state = ServerState::Restarting;
            self.publish();
            let stop_error = match self.process.take() {
                Some(process) => stop_managed_process(process).await.err(),
                None => None,
            };
            self.status.pid = None;
            self.status.started_at = None;
            let reason = stop_error.map_or_else(
                || "three consecutive health failures".into(),
                |error| format!("health restart cleanup failed: {error}"),
            );
            self.schedule_restart(reason);
        }
    }

    fn health_interval(&self) -> Duration {
        self.spec
            .settings
            .health_interval
            .max(Duration::from_secs(2))
    }

    fn cancel_health_probe(&mut self) {
        self.health_generation = self.health_generation.wrapping_add(1);
        if let Some(task) = self.health_task.take() {
            task.abort();
        }
        self.next_health = None;
    }

    fn schedule_restart(&mut self, reason: String) {
        let budget = restart_budget(&self.spec);
        if self.status.restart_count >= budget {
            self.status.state = ServerState::Failed;
            self.status.last_error = Some(format!(
                "restart budget exhausted after {} attempts ({reason})",
                self.status.restart_count
            ));
            self.restart_at = None;
            self.publish();
            return;
        }
        let attempt = self.status.restart_count;
        self.status.restart_count += 1;
        self.status.state = ServerState::Restarting;
        self.status.last_error = Some(reason);
        self.restart_at = Some(Instant::now() + RestartBackoff::delay(attempt));
        self.publish();
    }

    async fn process_wait_failed(&mut self, error: RuntimeError) {
        self.cancel_health_probe();
        let restart_unhealthy =
            self.status.state == ServerState::Unhealthy && self.spec.config.auto_restart;
        if restart_unhealthy {
            self.status.state = ServerState::Restarting;
            self.publish();
        }
        if let Some(process) = self.process.take() {
            drop_managed_process(process).await;
        }
        self.status.pid = None;
        self.status.healthy = false;
        self.status.started_at = None;
        self.healthy_since = None;
        let reason = error.to_string();
        if restart_unhealthy {
            self.schedule_restart(reason);
        } else if self.status.state == ServerState::Unhealthy {
            self.status.state = ServerState::Stopped;
            self.status.last_error = Some(reason);
            self.publish();
        } else {
            self.status.state = ServerState::Failed;
            self.status.last_error = Some(reason);
            self.restart_at = None;
            self.publish();
        }
    }

    fn reset_restart_budget(&mut self) {
        self.status.restart_count = 0;
        self.restart_at = None;
        self.healthy_since = None;
        self.consecutive_health_failures = 0;
    }

    fn update_config(&mut self, spec: RuntimeSpec) {
        let previous_health = health_probe_signature(&self.spec.config);
        self.cancel_health_probe();
        self.spec = spec;
        self.apply_updated_restart_policy();
        self.reconfigure_health(previous_health);
        self.refresh_status_metadata();
        self.publish();
    }

    fn apply_updated_restart_policy(&mut self) {
        if self.status.state != ServerState::Restarting || self.process.is_some() {
            return;
        }
        if !self.spec.config.auto_restart {
            self.restart_at = None;
            self.status.state = ServerState::Stopped;
            self.status.healthy = false;
        } else if self.status.restart_count > restart_budget(&self.spec) {
            self.restart_at = None;
            self.status.state = ServerState::Failed;
            self.status.last_error = Some(format!(
                "restart budget lowered to {} after {} attempts",
                restart_budget(&self.spec),
                self.status.restart_count
            ));
        }
    }

    fn reconfigure_health(&mut self, previous: Option<HealthProbeSignature>) {
        if self.process.is_none() {
            return;
        }
        let current = health_probe_signature(&self.spec.config);
        let now = Instant::now();
        self.consecutive_health_failures = 0;
        if current.is_some() {
            if current != previous {
                if self.status.state == ServerState::Running {
                    self.status.state = ServerState::Unhealthy;
                }
                self.status.healthy = false;
                self.healthy_since = None;
            }
            self.next_health = Some(now + INITIAL_HEALTH_INTERVAL);
        } else {
            self.status.state = ServerState::Running;
            self.status.healthy = true;
            self.healthy_since = Some(now);
        }
    }

    fn refresh_status_metadata(&mut self) {
        self.status.id.clone_from(&self.spec.config.id);
        self.status.name.clone_from(&self.spec.config.name);
        self.status.project_id.clone_from(&self.spec.project_id);
        self.status.project_name.clone_from(&self.spec.project_name);
        self.status.command.clone_from(&self.spec.config.command);
        self.status.port = self.spec.config.port;
        self.status.directory = self.spec.working_directory().to_string_lossy().into_owned();
        self.status.url = self
            .spec
            .config
            .port
            .map(|port| format!("http://localhost:{port}"));
    }

    fn publish(&self) {
        self.status_tx.send_replace(self.status.clone());
    }
}

async fn stop_managed_process(process: Box<dyn ManagedProcess>) -> Result<(), RuntimeError> {
    tokio::task::spawn_blocking(move || process.stop())
        .await
        .map_err(|error| RuntimeError::Backend(format!("stop task failed: {error}")))?
}

async fn drop_managed_process(process: Box<dyn ManagedProcess>) {
    let _ = tokio::task::spawn_blocking(move || drop(process)).await;
}

fn restart_budget(spec: &RuntimeSpec) -> u32 {
    spec.settings.max_restart_attempts.clamp(1, 20)
}

pub(super) fn initial_status(spec: &RuntimeSpec) -> ServerStatus {
    ServerStatus {
        id: spec.config.id.clone(),
        name: spec.config.name.clone(),
        project_id: spec.project_id.clone(),
        project_name: spec.project_name.clone(),
        command: spec.config.command.clone(),
        port: spec.config.port,
        directory: spec.working_directory().to_string_lossy().into_owned(),
        state: ServerState::Stopped,
        pid: None,
        started_at: None,
        restart_count: 0,
        last_exit_code: None,
        last_error: None,
        healthy: false,
        url: spec
            .config
            .port
            .map(|port| format!("http://localhost:{port}")),
        cpu_percent: None,
        memory_bytes: None,
        resident_memory_bytes: None,
        process_count: None,
        temporary: None,
        timeout_seconds: None,
        deadline: None,
        finished_at: None,
        timed_out: None,
    }
}

fn has_health_probe(config: &ServerConfig) -> bool {
    config.port.is_some()
        || config
            .health_url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
}

type HealthProbeSignature = (Option<u16>, Option<String>, Option<u16>);

fn health_probe_signature(config: &ServerConfig) -> Option<HealthProbeSignature> {
    let health_url = config
        .health_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned);
    if config.port.is_none() && health_url.is_none() {
        return None;
    }
    let health_status = health_url.as_ref().and(config.health_status);
    Some((config.port, health_url, health_status))
}
