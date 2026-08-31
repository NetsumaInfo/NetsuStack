//! Windows-specific process, terminal, port, and metrics adapters.

mod command_prompt;
mod docker;
mod handles;
mod ports;
mod processes;

pub mod conpty;
pub mod job;
pub mod shell;

pub use conpty::{ConPtyProcess, ConPtyStopResult, SpawnOptions, TerminalSize};
pub use docker::{
    DOCKER_OUTPUT_LIMIT, DockerCli, DockerContainer, DockerError, parse_docker_inspect,
    select_docker_container,
};
pub use handles::WindowsError;
pub use job::{STOP_GRACE_PERIOD, StopOutcome};
pub use ports::{AddressFamily, TcpListenerEntry, deduplicate_tcp_listeners, list_tcp_listeners};
pub use processes::{
    DockerProcessProvenance, ProcessIdentity, SnapshotError, docker_process_provenance,
    is_protected_process, snapshot_process_for_listener, snapshot_process_for_port,
    terminate_process,
};
pub use shell::{ResolvedShell, ShellKind, ShellPreference, resolve_executable, select_shell};
