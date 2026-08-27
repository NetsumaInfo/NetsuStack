use std::{
    cmp::Ordering,
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use windows::Win32::System::Console::COORD;

use super::launch::{compare_environment_names, validate_environment_entry};
use crate::WindowsError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    columns: u16,
    rows: u16,
}

impl TerminalSize {
    pub const fn new(columns: u16, rows: u16) -> Self {
        Self { columns, rows }
    }

    pub(super) fn coord(self) -> Result<COORD, WindowsError> {
        let Ok(x) = i16::try_from(self.columns) else {
            return Err(WindowsError::InvalidTerminalSize {
                columns: self.columns,
                rows: self.rows,
            });
        };
        let Ok(y) = i16::try_from(self.rows) else {
            return Err(WindowsError::InvalidTerminalSize {
                columns: self.columns,
                rows: self.rows,
            });
        };
        if x == 0 || y == 0 {
            return Err(WindowsError::InvalidTerminalSize {
                columns: self.columns,
                rows: self.rows,
            });
        }
        Ok(COORD { X: x, Y: y })
    }
}

#[derive(Debug, Clone)]
pub struct SpawnOptions {
    pub(super) program: PathBuf,
    pub(super) arguments: Vec<OsString>,
    pub(super) raw_cmd_command: Option<OsString>,
    pub(super) cwd: PathBuf,
    pub(super) environment: Vec<(OsString, OsString)>,
    pub(super) size: TerminalSize,
}

impl SpawnOptions {
    pub fn new<I>(program: PathBuf, arguments: I, cwd: PathBuf, size: TerminalSize) -> Self
    where
        I: IntoIterator<Item = OsString>,
    {
        Self {
            program,
            arguments: arguments.into_iter().collect(),
            raw_cmd_command: None,
            cwd,
            environment: std::env::vars_os().collect(),
            size,
        }
    }

    pub fn for_cmd_shell(
        program: PathBuf,
        command: OsString,
        cwd: PathBuf,
        size: TerminalSize,
    ) -> Result<Self, WindowsError> {
        super::launch::validate_raw_cmd_command(&command)?;
        Ok(Self {
            program,
            arguments: Vec::new(),
            raw_cmd_command: Some(command),
            cwd,
            environment: std::env::vars_os().collect(),
            size,
        })
    }

    pub fn clear_environment(&mut self) {
        self.environment.clear();
    }

    pub fn set_environment(&mut self, name: OsString, value: OsString) -> Result<(), WindowsError> {
        validate_environment_entry(&name, &value)?;
        if let Some(entry) = self
            .environment
            .iter_mut()
            .find(|(existing, _)| compare_environment_names(existing, &name) == Ordering::Equal)
        {
            *entry = (name, value);
        } else {
            self.environment.push((name, value));
        }
        Ok(())
    }

    pub fn remove_environment(&mut self, name: &OsStr) {
        self.environment
            .retain(|(existing, _)| compare_environment_names(existing, name) != Ordering::Equal);
    }
}
