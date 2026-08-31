use std::{
    collections::{BTreeSet, HashMap},
    io::{self, Read},
    net::IpAddr,
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerContainer {
    pub id: String,
    pub name: String,
    pub host_ip: IpAddr,
    pub host_port: u16,
    pub compose_project: Option<String>,
    pub compose_service: Option<String>,
}

#[derive(Debug, Error)]
pub enum DockerError {
    #[error("Docker CLI was not found")]
    NotFound,
    #[error("Docker command failed: {0}")]
    Command(String),
    #[error("invalid Docker inspect JSON: {0}")]
    InvalidInspect(#[from] serde_json::Error),
    #[error("Docker command `{command}` timed out after {timeout:?}")]
    Timeout { command: String, timeout: Duration },
    #[error("Docker command output exceeded the {limit}-byte limit")]
    OutputLimitExceeded { limit: usize },
    #[error(
        "{count} Docker containers publish port {host_port} compatibly with listener addresses {local_addresses:?}; refusing an ambiguous target"
    )]
    AmbiguousBinding {
        local_addresses: Vec<IpAddr>,
        host_port: u16,
        count: usize,
    },
}

#[derive(Debug, Clone)]
pub struct DockerCli {
    executable: PathBuf,
}

impl DockerCli {
    pub fn discover() -> Result<Self, DockerError> {
        discover_docker_executable()
            .map(|executable| Self { executable })
            .ok_or(DockerError::NotFound)
    }

    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    pub fn container_for_listener(
        &self,
        local_addresses: &[IpAddr],
        port: u16,
    ) -> Result<Option<DockerContainer>, DockerError> {
        let output = self.run(&[
            "ps",
            "--filter",
            &format!("publish={port}"),
            "--format",
            "{{.ID}}",
        ])?;
        let ids: Vec<_> = output
            .lines()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .collect();
        if ids.is_empty() {
            return Ok(None);
        }
        let arguments = container_arguments(["inspect"], ids.iter().copied());
        let inspect = self.run(&arguments)?;
        select_docker_container(&inspect, local_addresses, port)
    }

    pub fn reinspect_container(&self, expected: &DockerContainer) -> Result<(), DockerError> {
        let arguments = container_arguments(["inspect"], [expected.id.as_str()]);
        let inspect = self.run(&arguments)?;
        let exact_matches = parse_docker_inspect(&inspect, expected.host_port)?
            .into_iter()
            .filter(|container| container == expected)
            .count();
        if exact_matches != 1 {
            return Err(DockerError::Command(format!(
                "container {} no longer publishes port {} with the inspected identity",
                expected.id, expected.host_port
            )));
        }
        Ok(())
    }

    pub fn stop_container(&self, expected: &DockerContainer) -> Result<(), DockerError> {
        let arguments = container_arguments(["stop", "--time", "10"], [expected.id.as_str()]);
        self.run(&arguments)?;
        Ok(())
    }

    fn run(&self, arguments: &[&str]) -> Result<String, DockerError> {
        let mut child = Command::new(&self.executable)
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| DockerError::Command(error.to_string()))?;
        let clock = SystemWaitClock::new();

        let Some(stdout) = child.stdout.take() else {
            bounded_cleanup(&mut child, &clock);
            return Err(DockerError::Command(
                "Docker stdout pipe was not created".into(),
            ));
        };
        let Some(stderr) = child.stderr.take() else {
            bounded_cleanup(&mut child, &clock);
            return Err(DockerError::Command(
                "Docker stderr pipe was not created".into(),
            ));
        };
        let mut pipes = DockerPipes::new(stdout, stderr);
        let command = arguments.first().copied().unwrap_or("docker");
        let status =
            wait_for_exit_with_pump(&mut child, &clock, command, DOCKER_COMMAND_TIMEOUT, || {
                pipes.drain_available()
            })?;
        pipes.finish()?;

        if !status.success() {
            let stderr = String::from_utf8_lossy(&pipes.stderr).trim().to_owned();
            return Err(DockerError::Command(if stderr.is_empty() {
                format!("exit status {status}")
            } else {
                stderr
            }));
        }
        Ok(String::from_utf8_lossy(&pipes.stdout).into_owned())
    }
}

const DOCKER_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum combined stdout and stderr retained for one Docker CLI invocation.
pub const DOCKER_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const DOCKER_COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(25);
const DOCKER_TERMINATION_GRACE: Duration = Duration::from_millis(100);
const DOCKER_PIPE_READER_TIMEOUT: Duration = Duration::from_millis(100);
const DOCKER_PIPE_PUMP_BYTE_BUDGET: usize = 256 * 1024;

trait WaitableChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
    fn terminate(&mut self) -> io::Result<()>;
}

impl WaitableChild for Child {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Child::try_wait(self)
    }

    fn terminate(&mut self) -> io::Result<()> {
        self.kill()
    }
}

trait WaitClock {
    fn elapsed(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}

struct SystemWaitClock(Instant);

impl SystemWaitClock {
    fn new() -> Self {
        Self(Instant::now())
    }
}

impl WaitClock for SystemWaitClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

#[cfg(test)]
fn wait_for_exit(
    child: &mut impl WaitableChild,
    clock: &impl WaitClock,
    command: &str,
    timeout: Duration,
) -> Result<ExitStatus, DockerError> {
    wait_for_exit_with_pump(child, clock, command, timeout, || Ok(()))
}

fn wait_for_exit_with_pump(
    child: &mut impl WaitableChild,
    clock: &impl WaitClock,
    command: &str,
    timeout: Duration,
    mut pump: impl FnMut() -> Result<(), DockerError>,
) -> Result<ExitStatus, DockerError> {
    loop {
        if let Err(error) = pump() {
            bounded_cleanup(child, clock);
            return Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                let error = DockerError::Command(error.to_string());
                bounded_cleanup(child, clock);
                return Err(error);
            }
        }
        let elapsed = clock.elapsed();
        if elapsed >= timeout {
            let error = DockerError::Timeout {
                command: command.to_owned(),
                timeout,
            };
            bounded_cleanup(child, clock);
            return Err(error);
        }
        clock.sleep(DOCKER_COMMAND_POLL_INTERVAL.min(timeout - elapsed));
    }
}

fn bounded_cleanup(child: &mut impl WaitableChild, clock: &impl WaitClock) {
    let _ = child.terminate();
    let deadline = clock.elapsed().saturating_add(DOCKER_TERMINATION_GRACE);
    loop {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        let elapsed = clock.elapsed();
        if elapsed >= deadline {
            return;
        }
        clock.sleep(DOCKER_COMMAND_POLL_INTERVAL.min(deadline - elapsed));
    }
}

#[cfg(windows)]
struct DockerPipes {
    stdout_pipe: Option<std::process::ChildStdout>,
    stderr_pipe: Option<std::process::ChildStderr>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[cfg(windows)]
impl DockerPipes {
    fn new(stdout: std::process::ChildStdout, stderr: std::process::ChildStderr) -> Self {
        Self {
            stdout_pipe: Some(stdout),
            stderr_pipe: Some(stderr),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    fn drain_available(&mut self) -> Result<(), DockerError> {
        let mut total = self.stdout.len() + self.stderr.len();
        let mut pump_budget = DOCKER_PIPE_PUMP_BYTE_BUDGET;
        drain_pipe(
            &mut self.stdout_pipe,
            &mut self.stdout,
            "stdout",
            &mut total,
            &mut pump_budget,
        )?;
        drain_pipe(
            &mut self.stderr_pipe,
            &mut self.stderr,
            "stderr",
            &mut total,
            &mut pump_budget,
        )?;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), DockerError> {
        let started = Instant::now();
        loop {
            self.drain_available()?;
            if self.stdout_pipe.is_none() && self.stderr_pipe.is_none() {
                return Ok(());
            }
            let elapsed = started.elapsed();
            if elapsed >= DOCKER_PIPE_READER_TIMEOUT {
                self.stdout_pipe.take();
                self.stderr_pipe.take();
                return Err(DockerError::Timeout {
                    command: "stdout/stderr reader".into(),
                    timeout: DOCKER_PIPE_READER_TIMEOUT,
                });
            }
            thread::sleep(DOCKER_COMMAND_POLL_INTERVAL.min(DOCKER_PIPE_READER_TIMEOUT - elapsed));
        }
    }
}

#[cfg(windows)]
fn drain_pipe<P: Read + std::os::windows::io::AsRawHandle>(
    pipe: &mut Option<P>,
    output: &mut Vec<u8>,
    stream: &str,
    total_output: &mut usize,
    pump_budget: &mut usize,
) -> Result<(), DockerError> {
    use windows::{
        Win32::{
            Foundation::{ERROR_BROKEN_PIPE, HANDLE},
            System::Pipes::PeekNamedPipe,
        },
        core::HRESULT,
    };

    let Some(open_pipe) = pipe.as_mut() else {
        return Ok(());
    };
    loop {
        let raw = HANDLE(open_pipe.as_raw_handle());
        let mut available = 0_u32;
        let peek = unsafe { PeekNamedPipe(raw, None, 0, None, Some(&mut available), None) };
        if let Err(error) = peek {
            if error.code() == HRESULT::from_win32(ERROR_BROKEN_PIPE.0) {
                pipe.take();
                return Ok(());
            }
            return Err(DockerError::Command(format!(
                "Docker {stream} pipe inspection failed: {error}"
            )));
        }
        if available == 0 {
            return Ok(());
        }
        if *total_output >= DOCKER_OUTPUT_LIMIT {
            return Err(DockerError::OutputLimitExceeded {
                limit: DOCKER_OUTPUT_LIMIT,
            });
        }
        if *pump_budget == 0 {
            return Ok(());
        }

        let start = output.len();
        let requested = usize::try_from(available)
            .unwrap_or(usize::MAX)
            .min(*pump_budget)
            .min(DOCKER_OUTPUT_LIMIT - *total_output);
        output.resize(start + requested, 0);
        let read = open_pipe.read(&mut output[start..]).map_err(|error| {
            DockerError::Command(format!("Docker {stream} read failed: {error}"))
        })?;
        output.truncate(start + read);
        if read == 0 {
            pipe.take();
            return Ok(());
        }
        *total_output += read;
        *pump_budget -= read;
    }
}

#[cfg(not(windows))]
struct DockerPipes {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[cfg(not(windows))]
impl DockerPipes {
    fn new(_: std::process::ChildStdout, _: std::process::ChildStderr) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    fn drain_available(&mut self) -> Result<(), DockerError> {
        Err(DockerError::Command(
            "Docker pipe polling requires Windows".into(),
        ))
    }

    fn finish(&mut self) -> Result<(), DockerError> {
        Ok(())
    }
}

fn container_arguments<'a>(
    prefix: impl IntoIterator<Item = &'a str>,
    identifiers: impl IntoIterator<Item = &'a str>,
) -> Vec<&'a str> {
    let mut arguments: Vec<_> = prefix.into_iter().collect();
    arguments.push("--");
    arguments.extend(identifiers);
    arguments
}

pub fn parse_docker_inspect(
    json: &str,
    host_port: u16,
) -> Result<Vec<DockerContainer>, DockerError> {
    let inspected: Vec<InspectContainer> = serde_json::from_str(json)?;
    let mut matching = Vec::new();
    for container in inspected {
        let compose_project = container
            .config
            .labels
            .as_ref()
            .and_then(|labels| labels.get("com.docker.compose.project").cloned());
        let compose_service = container
            .config
            .labels
            .as_ref()
            .and_then(|labels| labels.get("com.docker.compose.service").cloned());
        for (container_port, bindings) in container.network_settings.ports {
            if !container_port.ends_with("/tcp") {
                continue;
            }
            for binding in bindings.into_iter().flatten() {
                if binding.host_port.parse::<u16>() != Ok(host_port) {
                    continue;
                }
                let Ok(host_ip) = binding.host_ip.parse() else {
                    continue;
                };
                matching.push(DockerContainer {
                    id: container.id.clone(),
                    name: container.name.trim_start_matches('/').to_owned(),
                    host_ip,
                    host_port,
                    compose_project: compose_project.clone(),
                    compose_service: compose_service.clone(),
                });
            }
        }
    }
    Ok(matching)
}

pub fn select_docker_container(
    json: &str,
    local_addresses: &[IpAddr],
    host_port: u16,
) -> Result<Option<DockerContainer>, DockerError> {
    let local_addresses: Vec<_> = local_addresses
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut matches: Vec<_> = parse_docker_inspect(json, host_port)?
        .into_iter()
        .filter(|container| {
            local_addresses
                .iter()
                .copied()
                .any(|address| addresses_are_compatible(container.host_ip, address))
        })
        .collect();
    matches.sort_by(|left, right| {
        binding_rank(left.host_ip, &local_addresses)
            .cmp(&binding_rank(right.host_ip, &local_addresses))
            .then_with(|| left.host_ip.cmp(&right.host_ip))
            .then_with(|| left.id.cmp(&right.id))
    });
    let container_ids: BTreeSet<_> = matches.iter().map(|container| &container.id).collect();
    match container_ids.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        count => Err(DockerError::AmbiguousBinding {
            local_addresses,
            host_port,
            count,
        }),
    }
}

fn addresses_are_compatible(binding: IpAddr, listener: IpAddr) -> bool {
    matches!(
        (binding, listener),
        (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
    ) && (binding == listener || binding.is_unspecified() || listener.is_unspecified())
}

fn binding_rank(binding: IpAddr, local_addresses: &[IpAddr]) -> u8 {
    if local_addresses.contains(&binding) {
        0
    } else if binding.is_unspecified() {
        1
    } else {
        2
    }
}

fn discover_docker_executable() -> Option<PathBuf> {
    let from_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join("docker.exe"))
            .find(|candidate| candidate.is_file())
    });
    from_path.or_else(|| {
        ["ProgramFiles", "ProgramW6432"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .map(|root| {
                root.join("Docker")
                    .join("Docker")
                    .join("resources")
                    .join("bin")
                    .join("docker.exe")
            })
            .find(|candidate| candidate.is_file())
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectContainer {
    id: String,
    name: String,
    #[serde(default)]
    config: InspectConfig,
    #[serde(default)]
    network_settings: InspectNetworkSettings,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectConfig {
    #[serde(default)]
    labels: Option<HashMap<String, String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectNetworkSettings {
    #[serde(default)]
    ports: HashMap<String, Option<Vec<PortBinding>>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PortBinding {
    host_ip: String,
    host_port: String,
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, io, process::ExitStatus, time::Duration};

    use super::{
        DOCKER_COMMAND_POLL_INTERVAL, DockerError, WaitClock, WaitableChild, container_arguments,
        wait_for_exit,
    };

    const EXPECTED_TERMINATION_GRACE: Duration = Duration::from_millis(100);

    #[test]
    fn container_identifiers_follow_an_options_terminator() {
        assert_eq!(
            container_arguments(["inspect"], ["--malicious-container"]),
            ["inspect", "--", "--malicious-container"]
        );
        assert_eq!(
            container_arguments(["stop", "--time", "10"], ["--malicious-container"]),
            ["stop", "--time", "10", "--", "--malicious-container"]
        );
    }

    #[test]
    fn blocking_docker_command_times_out_with_a_fake_clock() {
        let clock = FakeClock::default();
        let mut child = BlockingChild::default();

        let error = wait_for_exit(&mut child, &clock, "inspect", Duration::ZERO)
            .expect_err("blocking Docker command must time out");

        assert!(matches!(
            error,
            DockerError::Timeout {
                command,
                timeout,
            } if command == "inspect" && timeout == Duration::ZERO
        ));
        assert_eq!(child.terminate_calls, 1);
        assert_eq!(child.try_wait_calls, 6);
        assert_eq!(clock.elapsed.get(), EXPECTED_TERMINATION_GRACE);
        assert_eq!(clock.sleep_calls.get(), 4);
    }

    #[test]
    fn try_wait_error_still_uses_bounded_best_effort_cleanup() {
        let clock = FakeClock::default();
        let mut child = TryWaitErrorChild::default();

        let error = wait_for_exit(&mut child, &clock, "inspect", Duration::from_secs(2))
            .expect_err("try_wait failure must be returned after bounded cleanup");

        assert!(matches!(error, DockerError::Command(message) if message == "try_wait failed"));
        assert_eq!(child.terminate_calls, 1);
        assert_eq!(child.try_wait_calls, 6);
        assert_eq!(clock.elapsed.get(), EXPECTED_TERMINATION_GRACE);
        assert_eq!(clock.sleep_calls.get(), 4);
    }

    #[test]
    fn terminate_error_does_not_replace_timeout_or_escape_the_cleanup_bound() {
        let clock = FakeClock::default();
        let mut child = TerminateErrorChild::default();

        let error = wait_for_exit(&mut child, &clock, "inspect", Duration::ZERO)
            .expect_err("termination failure must retain the typed timeout");

        assert!(matches!(
            error,
            DockerError::Timeout {
                command,
                timeout: Duration::ZERO,
            } if command == "inspect"
        ));
        assert_eq!(child.terminate_calls, 1);
        assert_eq!(child.try_wait_calls, 6);
        assert_eq!(clock.elapsed.get(), EXPECTED_TERMINATION_GRACE);
        assert_eq!(clock.sleep_calls.get(), 4);
        assert_eq!(EXPECTED_TERMINATION_GRACE, DOCKER_COMMAND_POLL_INTERVAL * 4);
    }

    #[derive(Default)]
    struct FakeClock {
        elapsed: Cell<Duration>,
        sleep_calls: Cell<usize>,
    }

    impl WaitClock for FakeClock {
        fn elapsed(&self) -> Duration {
            self.elapsed.get()
        }

        fn sleep(&self, duration: Duration) {
            self.elapsed.set(self.elapsed.get() + duration);
            self.sleep_calls.set(self.sleep_calls.get() + 1);
        }
    }

    #[derive(Default)]
    struct BlockingChild {
        try_wait_calls: usize,
        terminate_calls: usize,
    }

    impl WaitableChild for BlockingChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.try_wait_calls += 1;
            Ok(None)
        }

        fn terminate(&mut self) -> io::Result<()> {
            self.terminate_calls += 1;
            Ok(())
        }
    }

    #[derive(Default)]
    struct TryWaitErrorChild {
        try_wait_calls: usize,
        terminate_calls: usize,
    }

    impl WaitableChild for TryWaitErrorChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.try_wait_calls += 1;
            Err(io::Error::other("try_wait failed"))
        }

        fn terminate(&mut self) -> io::Result<()> {
            self.terminate_calls += 1;
            Ok(())
        }
    }

    #[derive(Default)]
    struct TerminateErrorChild {
        try_wait_calls: usize,
        terminate_calls: usize,
    }

    impl WaitableChild for TerminateErrorChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.try_wait_calls += 1;
            Ok(None)
        }

        fn terminate(&mut self) -> io::Result<()> {
            self.terminate_calls += 1;
            Err(io::Error::other("terminate failed"))
        }
    }
}
