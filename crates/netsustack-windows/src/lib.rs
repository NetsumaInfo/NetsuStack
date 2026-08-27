//! Windows-specific process, terminal, port, and metrics adapters.

mod command_prompt;
mod handles;

pub mod conpty;
pub mod job;
pub mod shell;

pub use conpty::{ConPtyProcess, SpawnOptions, TerminalSize};
pub use handles::WindowsError;
pub use job::{STOP_GRACE_PERIOD, StopOutcome};
pub use shell::{ResolvedShell, ShellKind, ShellPreference, resolve_executable, select_shell};
