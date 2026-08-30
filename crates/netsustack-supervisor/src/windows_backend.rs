use std::{path::PathBuf, time::Duration};

use async_trait::async_trait;
use netsustack_domain::{PreferredShell, ServerConfig};
use netsustack_windows::{
    ConPtyProcess, ShellKind, ShellPreference, SpawnOptions, TerminalSize, select_shell,
};

use crate::{
    health::HealthChecker,
    runtime::{ManagedProcess, ProcessBackend, ProcessStopResult, RuntimeError, SpawnRequest},
};

#[derive(Debug, Clone)]
pub struct WindowsProcessBackend {
    health: HealthChecker,
    terminal_size: TerminalSize,
}

impl Default for WindowsProcessBackend {
    fn default() -> Self {
        Self {
            health: HealthChecker::default(),
            terminal_size: TerminalSize::new(100, 30),
        }
    }
}

#[async_trait]
impl ProcessBackend for WindowsProcessBackend {
    async fn spawn(&self, request: SpawnRequest) -> Result<Box<dyn ManagedProcess>, RuntimeError> {
        let terminal_size = self.terminal_size;
        tokio::task::spawn_blocking(move || spawn_windows_process(request, terminal_size))
            .await
            .map_err(|error| RuntimeError::Backend(format!("spawn task failed: {error}")))?
    }

    async fn check_health(&self, config: &ServerConfig) -> bool {
        self.health.check(config).await
    }
}

struct WindowsManagedProcess(ConPtyProcess);

impl ManagedProcess for WindowsManagedProcess {
    fn pid(&self) -> u32 {
        self.0.process_id()
    }

    fn try_wait(&mut self) -> Result<Option<i32>, RuntimeError> {
        self.0
            .wait_for_exit(Duration::ZERO)
            .map(|code| code.map(|value| value as i32))
            .map_err(backend_error)
    }

    fn stop(self: Box<Self>) -> ProcessStopResult {
        let stopped = self.0.stop_and_drain();
        ProcessStopResult {
            final_output: stopped.final_output,
            cleanup_error: stopped.cleanup_error.map(backend_error),
            output_error: stopped.output_error.map(backend_error),
        }
    }

    fn input(&mut self, bytes: &[u8]) -> Result<(), RuntimeError> {
        self.0.write_input(bytes).map_err(backend_error)
    }

    fn resize(&mut self, columns: u16, rows: u16) -> Result<(), RuntimeError> {
        self.0
            .resize(TerminalSize::new(columns, rows))
            .map_err(backend_error)
    }

    fn drain_output(&mut self) -> Result<Vec<u8>, RuntimeError> {
        self.0.read_available().map_err(backend_error)
    }
}

fn spawn_windows_process(
    request: SpawnRequest,
    terminal_size: TerminalSize,
) -> Result<Box<dyn ManagedProcess>, RuntimeError> {
    let shell = select_shell(shell_preference(&request.preferred_shell), &search_paths())
        .map_err(backend_error)?;
    let mut options = if shell.kind() == ShellKind::Cmd {
        SpawnOptions::for_cmd_shell(
            shell.executable().to_path_buf(),
            request.command.clone().into(),
            request.cwd.clone(),
            terminal_size,
        )
        .map_err(backend_error)?
    } else {
        SpawnOptions::new(
            shell.executable().to_path_buf(),
            shell
                .command_arguments(request.command.as_ref())
                .map_err(backend_error)?,
            request.cwd.clone(),
            terminal_size,
        )
    };
    for (name, value) in &request.environment {
        options
            .set_environment(name.into(), value.into())
            .map_err(backend_error)?;
    }
    for (name, value) in [
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("FORCE_COLOR", "1"),
        ("CLICOLOR", "1"),
        ("CLICOLOR_FORCE", "1"),
        ("TERM_PROGRAM", "NetsuStack"),
        ("NETSUSTACK", "1"),
    ] {
        options
            .set_environment(name.into(), value.into())
            .map_err(backend_error)?;
    }
    options
        .set_environment(
            "NETSUSTACK_SERVER".into(),
            request.server_name.clone().into(),
        )
        .map_err(backend_error)?;
    options.remove_environment("NETSUSTACK_SERVER_NAME".as_ref());
    options.remove_environment("NO_COLOR".as_ref());
    if let Some(port) = request.port {
        options
            .set_environment("PORT".into(), port.to_string().into())
            .map_err(backend_error)?;
    } else {
        options.remove_environment("PORT".as_ref());
    }
    let process = ConPtyProcess::spawn(options).map_err(backend_error)?;
    Ok(Box::new(WindowsManagedProcess(process)))
}

fn shell_preference(preference: &PreferredShell) -> ShellPreference {
    match preference {
        PreferredShell::Auto => ShellPreference::Auto,
        PreferredShell::Powershell7 => ShellPreference::PowerShell7,
        PreferredShell::WindowsPowershell => ShellPreference::WindowsPowerShell,
        PreferredShell::Cmd => ShellPreference::Cmd,
        PreferredShell::Custom(path) => ShellPreference::Custom(path.into()),
    }
}

fn search_paths() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default()
}

fn backend_error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Backend(error.to_string())
}
