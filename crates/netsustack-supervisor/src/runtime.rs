use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use netsustack_domain::{PreferredShell, ServerConfig, ServerStatus};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, watch};

mod actor;

pub use crate::windows_backend::WindowsProcessBackend;

const COMMAND_CAPACITY: usize = 64;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime backend: {0}")]
    Backend(String),
    #[error("runtime command channel is closed")]
    Closed,
    #[error("server process is not running")]
    NotRunning,
}

#[derive(Debug, Clone)]
pub struct RuntimeSettings {
    pub health_interval: Duration,
    pub max_restart_attempts: u32,
    pub preferred_shell: PreferredShell,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            health_interval: Duration::from_secs(10),
            max_restart_attempts: 5,
            preferred_shell: PreferredShell::Auto,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeSpec {
    pub config: ServerConfig,
    pub project_id: String,
    pub project_name: String,
    pub project_root: PathBuf,
    pub settings: RuntimeSettings,
}

impl RuntimeSpec {
    pub(super) fn working_directory(&self) -> PathBuf {
        match self.config.directory.as_deref().map(str::trim) {
            None | Some("") => self.project_root.clone(),
            Some(directory) if Path::new(directory).is_absolute() => PathBuf::from(directory),
            Some(directory) => self.project_root.join(directory),
        }
    }

    pub(super) fn spawn_request(&self) -> SpawnRequest {
        self.spawn_request_with_command(self.config.command.clone())
    }

    pub(super) fn spawn_request_with_command(&self, command: String) -> SpawnRequest {
        SpawnRequest {
            command,
            cwd: self.working_directory(),
            environment: self.config.env.clone(),
            preferred_shell: self.settings.preferred_shell.clone(),
            server_name: self.config.name.clone(),
            port: self.config.port,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub command: String,
    pub cwd: PathBuf,
    pub environment: HashMap<String, String>,
    pub preferred_shell: PreferredShell,
    pub server_name: String,
    pub port: Option<u16>,
}

pub trait ManagedProcess: Send + 'static {
    fn pid(&self) -> u32;
    fn try_wait(&mut self) -> Result<Option<i32>, RuntimeError>;
    fn stop(self: Box<Self>) -> ProcessStopResult;
    fn input(&mut self, bytes: &[u8]) -> Result<(), RuntimeError>;
    fn resize(&mut self, columns: u16, rows: u16) -> Result<(), RuntimeError>;
    fn drain_output(&mut self) -> Result<Vec<u8>, RuntimeError> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Default)]
pub struct ProcessStopResult {
    pub final_output: Vec<u8>,
    pub cleanup_error: Option<RuntimeError>,
    pub output_error: Option<RuntimeError>,
}

impl ProcessStopResult {
    pub fn into_result(self) -> Result<(), RuntimeError> {
        self.cleanup_error.map_or(Ok(()), Err)
    }
}

#[async_trait]
pub trait ProcessBackend: Send + Sync + 'static {
    async fn spawn(&self, request: SpawnRequest) -> Result<Box<dyn ManagedProcess>, RuntimeError>;
    async fn check_health(&self, config: &ServerConfig) -> bool;
}

#[derive(Clone)]
pub struct ServerRuntime {
    commands: mpsc::Sender<RuntimeCommand>,
    status: watch::Receiver<ServerStatus>,
}

impl ServerRuntime {
    pub fn spawn(spec: RuntimeSpec, backend: Arc<dyn ProcessBackend>) -> Self {
        let initial_status = actor::initial_status(&spec);
        let (status_tx, status) = watch::channel(initial_status);
        let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        tokio::spawn(actor::run(spec, backend, status_tx, command_rx));
        Self { commands, status }
    }

    pub fn status(&self) -> ServerStatus {
        self.status.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<ServerStatus> {
        self.status.clone()
    }

    pub async fn start(&self) -> Result<(), RuntimeError> {
        self.request(|reply| RuntimeCommand::Start { reply }).await
    }

    pub async fn stop(&self) -> Result<(), RuntimeError> {
        self.request(|reply| RuntimeCommand::Stop { reply }).await
    }

    pub async fn restart(&self) -> Result<(), RuntimeError> {
        self.request(|reply| RuntimeCommand::Restart { reply })
            .await
    }

    pub async fn input(&self, bytes: &[u8]) -> Result<(), RuntimeError> {
        let bytes = bytes.to_vec();
        self.request(|reply| RuntimeCommand::Input { bytes, reply })
            .await
    }

    pub async fn resize(&self, columns: u16, rows: u16) -> Result<(), RuntimeError> {
        self.request(|reply| RuntimeCommand::Resize {
            columns,
            rows,
            reply,
        })
        .await
    }

    pub async fn update_config(&self, spec: RuntimeSpec) -> Result<(), RuntimeError> {
        self.request(|reply| RuntimeCommand::UpdateConfig {
            spec: Box::new(spec),
            reply,
        })
        .await
    }

    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        self.request(|reply| RuntimeCommand::Shutdown { reply })
            .await
    }

    async fn request(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<(), RuntimeError>>) -> RuntimeCommand,
    ) -> Result<(), RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(build(reply))
            .await
            .map_err(|_| RuntimeError::Closed)?;
        response.await.map_err(|_| RuntimeError::Closed)?
    }
}

pub(super) enum RuntimeCommand {
    Start {
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    Stop {
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    Restart {
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    Input {
        bytes: Vec<u8>,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    Resize {
        columns: u16,
        rows: u16,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    UpdateConfig {
        spec: Box<RuntimeSpec>,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
}
