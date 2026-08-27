use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use crate::WindowsError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellPreference {
    Auto,
    PowerShell7,
    WindowsPowerShell,
    Cmd,
    Custom(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    PowerShell7,
    WindowsPowerShell,
    Cmd,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedShell {
    kind: ShellKind,
    executable: PathBuf,
}

impl ResolvedShell {
    pub fn kind(&self) -> ShellKind {
        self.kind
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn command_arguments(&self, command: &OsStr) -> Vec<OsString> {
        match self.kind {
            ShellKind::PowerShell7 | ShellKind::WindowsPowerShell => [
                OsString::from("-NoLogo"),
                OsString::from("-NoProfile"),
                OsString::from("-Command"),
                command.to_owned(),
            ]
            .into(),
            ShellKind::Cmd => [
                OsString::from("/D"),
                OsString::from("/S"),
                OsString::from("/C"),
                command.to_owned(),
            ]
            .into(),
            ShellKind::Custom => vec![command.to_owned()],
        }
    }
}

pub fn select_shell(
    preference: ShellPreference,
    search_paths: &[PathBuf],
) -> Result<ResolvedShell, WindowsError> {
    let resolved = match preference {
        ShellPreference::Auto => resolve_executable(OsStr::new("pwsh.exe"), search_paths)
            .map(|executable| ResolvedShell {
                kind: ShellKind::PowerShell7,
                executable,
            })
            .unwrap_or_else(|| ResolvedShell {
                kind: ShellKind::Cmd,
                executable: PathBuf::from("cmd.exe"),
            }),
        ShellPreference::PowerShell7 => ResolvedShell {
            kind: ShellKind::PowerShell7,
            executable: resolve_executable(OsStr::new("pwsh.exe"), search_paths)
                .unwrap_or_else(|| PathBuf::from("pwsh.exe")),
        },
        ShellPreference::WindowsPowerShell => ResolvedShell {
            kind: ShellKind::WindowsPowerShell,
            executable: PathBuf::from("powershell.exe"),
        },
        ShellPreference::Cmd => ResolvedShell {
            kind: ShellKind::Cmd,
            executable: PathBuf::from("cmd.exe"),
        },
        ShellPreference::Custom(executable) => {
            if executable.as_os_str().is_empty() {
                return Err(WindowsError::io(
                    "select custom shell",
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty shell path"),
                ));
            }
            ResolvedShell {
                kind: ShellKind::Custom,
                executable,
            }
        }
    };
    Ok(resolved)
}

pub fn resolve_executable(name: &OsStr, search_paths: &[PathBuf]) -> Option<PathBuf> {
    let name_path = Path::new(name);
    let has_extension = name_path.extension().is_some();
    search_paths.iter().find_map(|directory| {
        let candidate = directory.join(name_path);
        if candidate.is_file() {
            return Some(candidate);
        }
        if has_extension {
            return None;
        }
        ["exe", "cmd", "bat", "com"]
            .iter()
            .map(|extension| candidate.with_extension(extension))
            .find(|extended| extended.is_file())
    })
}
