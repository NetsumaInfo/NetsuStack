use std::collections::VecDeque;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Local;

const MAX_PARTIAL_LINE_BYTES: usize = 8_192;
const MAX_ESCAPE_SCALARS: usize = 4_096;

trait LogFileHandle: Write + fmt::Debug + Send {
    fn len(&self) -> io::Result<u64>;
}

impl LogFileHandle for File {
    fn len(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }
}

trait LogFileSystem: fmt::Debug + Send + Sync {
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn open_append(&self, path: &Path) -> io::Result<Box<dyn LogFileHandle>>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn exists(&self, path: &Path) -> io::Result<bool>;
}

#[derive(Debug, Default)]
struct StdLogFileSystem;

impl LogFileSystem for StdLogFileSystem {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn open_append(&self, path: &Path) -> io::Result<Box<dyn LogFileHandle>> {
        Ok(Box::new(
            OpenOptions::new().create(true).append(true).open(path)?,
        ))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn exists(&self, path: &Path) -> io::Result<bool> {
        match fs::metadata(path) {
            Ok(_) => Ok(true),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(source),
        }
    }
}

/// A typed failure raised while persisting plain logs.
#[derive(Debug)]
pub struct PlainLogError {
    operation: &'static str,
    path: PathBuf,
    source: io::Error,
}

impl fmt::Display for PlainLogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to {} log file {}: {}",
            self.operation,
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for PlainLogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl PlainLogError {
    #[must_use]
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ControlStringKind {
    Osc,
    St,
}

#[derive(Debug, Default)]
enum EscapeState {
    #[default]
    Ground,
    Escape(usize),
    Csi(usize),
    ControlString {
        kind: ControlStringKind,
        length: usize,
        saw_escape: bool,
    },
    DiscardEscape,
    DiscardCsi,
    DiscardControlString {
        kind: ControlStringKind,
        saw_escape: bool,
    },
}

/// Incrementally converts arbitrary process bytes into bounded, readable lines.
#[derive(Debug)]
pub struct PlainLogStore {
    lines: VecDeque<String>,
    max_lines: usize,
    current: String,
    pending_utf8: Vec<u8>,
    pending_cr: bool,
    escape: EscapeState,
    file: Option<LogFile>,
}

#[derive(Debug)]
struct LogFile {
    path: PathBuf,
    handle: Option<Box<dyn LogFileHandle>>,
    bytes_written: u64,
    max_bytes: u64,
    file_system: Arc<dyn LogFileSystem>,
    pending_cleanup: Option<PathBuf>,
}

impl PlainLogStore {
    #[must_use]
    pub fn memory(max_lines: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            max_lines: max_lines.max(1),
            current: String::new(),
            pending_utf8: Vec::with_capacity(3),
            pending_cr: false,
            escape: EscapeState::Ground,
            file: None,
        }
    }

    pub fn file(
        path: impl AsRef<Path>,
        max_lines: usize,
        max_file_bytes: u64,
    ) -> Result<Self, PlainLogError> {
        Self::file_with_file_system(path, max_lines, max_file_bytes, Arc::new(StdLogFileSystem))
    }

    fn file_with_file_system(
        path: impl AsRef<Path>,
        max_lines: usize,
        max_file_bytes: u64,
        file_system: Arc<dyn LogFileSystem>,
    ) -> Result<Self, PlainLogError> {
        let path = path.as_ref();
        let mut store = Self::memory(max_lines);
        store.file = Some(Self::open_file(path, max_file_bytes.max(1), file_system)?);
        Ok(store)
    }

    pub fn ingest(&mut self, bytes: &[u8]) -> Result<(), PlainLogError> {
        let timestamp = Local::now().format("%H:%M:%S").to_string();
        self.ingest_at(bytes, &timestamp)
    }

    pub fn ingest_at(&mut self, bytes: &[u8], timestamp: &str) -> Result<(), PlainLogError> {
        let mut input = std::mem::take(&mut self.pending_utf8);
        input.extend_from_slice(bytes);
        let mut offset = 0;
        let mut first_error = None;

        while offset < input.len() {
            match std::str::from_utf8(&input[offset..]) {
                Ok(valid) => {
                    self.ingest_text(valid, timestamp, &mut first_error);
                    offset = input.len();
                }
                Err(error) => {
                    let valid_end = offset + error.valid_up_to();
                    if valid_end > offset {
                        if let Ok(valid) = std::str::from_utf8(&input[offset..valid_end]) {
                            self.ingest_text(valid, timestamp, &mut first_error);
                        }
                    }
                    match error.error_len() {
                        Some(invalid_length) => {
                            Self::record_first_error(
                                &mut first_error,
                                self.ingest_scalar('\u{fffd}', timestamp),
                            );
                            offset = valid_end + invalid_length;
                        }
                        None => {
                            self.pending_utf8.extend_from_slice(&input[valid_end..]);
                            break;
                        }
                    }
                }
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    /// Finalizes any incomplete decoder state and emits a trailing nonempty line.
    pub fn finish(&mut self) -> Result<(), PlainLogError> {
        let timestamp = Local::now().format("%H:%M:%S").to_string();
        self.finish_at(&timestamp)
    }

    /// Finalizes the stream using an explicit timestamp for persisted output.
    pub fn finish_at(&mut self, timestamp: &str) -> Result<(), PlainLogError> {
        let mut first_error = None;
        if !self.pending_utf8.is_empty() {
            self.pending_utf8.clear();
            Self::record_first_error(&mut first_error, self.ingest_scalar('\u{fffd}', timestamp));
        }
        self.escape = EscapeState::Ground;
        self.pending_cr = false;
        if !self.current.is_empty() {
            Self::record_first_error(&mut first_error, self.emit_current(timestamp));
        }
        first_error.map_or(Ok(()), Err)
    }

    pub fn note(&mut self, message: &str) -> Result<(), PlainLogError> {
        let timestamp = Local::now().format("%H:%M:%S").to_string();
        self.note_at(message, &timestamp)
    }

    pub fn note_at(&mut self, message: &str, timestamp: &str) -> Result<(), PlainLogError> {
        let mut first_error = self.finish_at(timestamp).err();
        Self::record_first_error(
            &mut first_error,
            self.ingest_at(format!("[netsustack] {message}\n").as_bytes(), timestamp),
        );
        first_error.map_or(Ok(()), Err)
    }

    pub fn update_limits(
        &mut self,
        max_lines: usize,
        max_file_bytes: u64,
    ) -> Result<(), PlainLogError> {
        self.max_lines = max_lines.max(1);
        while self.lines.len() > self.max_lines {
            self.lines.pop_front();
        }

        let rotate = if let Some(file) = &mut self.file {
            file.max_bytes = max_file_bytes.max(1);
            file.bytes_written > file.max_bytes
        } else {
            false
        };
        if rotate {
            self.rotate_file()?;
        }
        Ok(())
    }

    #[must_use]
    pub fn tail(&self, count: usize) -> Vec<String> {
        self.lines
            .iter()
            .skip(self.lines.len().saturating_sub(count))
            .cloned()
            .collect()
    }

    fn ingest_text(
        &mut self,
        text: &str,
        timestamp: &str,
        first_error: &mut Option<PlainLogError>,
    ) {
        for scalar in text.chars() {
            Self::record_first_error(first_error, self.ingest_scalar(scalar, timestamp));
        }
    }

    fn record_first_error(
        first_error: &mut Option<PlainLogError>,
        result: Result<(), PlainLogError>,
    ) {
        if first_error.is_none() {
            *first_error = result.err();
        }
    }

    fn ingest_scalar(&mut self, scalar: char, timestamp: &str) -> Result<(), PlainLogError> {
        if matches!(scalar, '\u{0018}' | '\u{001a}') {
            self.escape = EscapeState::Ground;
            return Ok(());
        }
        let state = std::mem::take(&mut self.escape);
        match state {
            EscapeState::Ground => self.ingest_ground(scalar, timestamp)?,
            EscapeState::Escape(length) => {
                self.escape = match (length, scalar) {
                    (_, '\u{001b}') => EscapeState::Escape(0),
                    (0, '[') => EscapeState::Csi(0),
                    (0, ']') => EscapeState::ControlString {
                        kind: ControlStringKind::Osc,
                        length: 0,
                        saw_escape: false,
                    },
                    (0, 'P' | 'X' | '^' | '_') => EscapeState::ControlString {
                        kind: ControlStringKind::St,
                        length: 0,
                        saw_escape: false,
                    },
                    (_, '\u{20}'..='\u{2f}') if length >= MAX_ESCAPE_SCALARS => {
                        EscapeState::DiscardEscape
                    }
                    (_, '\u{20}'..='\u{2f}') => EscapeState::Escape(length + 1),
                    (_, '\u{30}'..='\u{7e}') => EscapeState::Ground,
                    (_, control) if control.is_control() => EscapeState::Escape(length),
                    _ => EscapeState::Ground,
                };
            }
            EscapeState::Csi(length) => {
                self.escape = if scalar == '\u{001b}' {
                    EscapeState::Escape(0)
                } else if ('\u{40}'..='\u{7e}').contains(&scalar) {
                    EscapeState::Ground
                } else if length >= MAX_ESCAPE_SCALARS {
                    EscapeState::DiscardCsi
                } else {
                    EscapeState::Csi(length + 1)
                };
            }
            EscapeState::ControlString {
                kind,
                length,
                saw_escape,
            } => {
                self.escape = if Self::ends_control_string(kind, saw_escape, scalar) {
                    EscapeState::Ground
                } else if length >= MAX_ESCAPE_SCALARS {
                    EscapeState::DiscardControlString {
                        kind,
                        saw_escape: scalar == '\u{1b}',
                    }
                } else {
                    EscapeState::ControlString {
                        kind,
                        length: length + 1,
                        saw_escape: scalar == '\u{1b}',
                    }
                };
            }
            EscapeState::DiscardEscape => {
                self.escape = match scalar {
                    '\u{001b}' => EscapeState::Escape(0),
                    '\u{30}'..='\u{7e}' => EscapeState::Ground,
                    _ => EscapeState::DiscardEscape,
                };
            }
            EscapeState::DiscardCsi => {
                self.escape = if scalar == '\u{001b}' {
                    EscapeState::Escape(0)
                } else if ('\u{40}'..='\u{7e}').contains(&scalar) {
                    EscapeState::Ground
                } else {
                    EscapeState::DiscardCsi
                };
            }
            EscapeState::DiscardControlString { kind, saw_escape } => {
                self.escape = if Self::ends_control_string(kind, saw_escape, scalar) {
                    EscapeState::Ground
                } else {
                    EscapeState::DiscardControlString {
                        kind,
                        saw_escape: scalar == '\u{1b}',
                    }
                };
            }
        }
        Ok(())
    }

    fn ends_control_string(kind: ControlStringKind, saw_escape: bool, scalar: char) -> bool {
        scalar == '\u{009c}'
            || (matches!(kind, ControlStringKind::Osc) && scalar == '\u{0007}')
            || (saw_escape && scalar == '\\')
    }

    fn ingest_ground(&mut self, scalar: char, timestamp: &str) -> Result<(), PlainLogError> {
        if self.pending_cr {
            self.pending_cr = false;
            if scalar == '\n' {
                self.emit_current(timestamp)?;
                return Ok(());
            }
            self.current.clear();
        }

        match scalar {
            '\u{001b}' => self.escape = EscapeState::Escape(0),
            '\u{009b}' => self.escape = EscapeState::Csi(0),
            '\u{009d}' => {
                self.escape = EscapeState::ControlString {
                    kind: ControlStringKind::Osc,
                    length: 0,
                    saw_escape: false,
                };
            }
            '\u{0090}' | '\u{0098}' | '\u{009e}' | '\u{009f}' => {
                self.escape = EscapeState::ControlString {
                    kind: ControlStringKind::St,
                    length: 0,
                    saw_escape: false,
                };
            }
            '\u{009c}' => {}
            '\r' => self.pending_cr = true,
            '\n' => self.emit_current(timestamp)?,
            '\u{7}' => {}
            '\u{8}' => {
                self.current.pop();
            }
            '\t' => self.push_text("\t", timestamp)?,
            control if control.is_control() => {}
            printable => {
                let mut encoded = [0; 4];
                self.push_text(printable.encode_utf8(&mut encoded), timestamp)?;
            }
        }
        Ok(())
    }

    fn push_text(&mut self, text: &str, timestamp: &str) -> Result<(), PlainLogError> {
        let mut first_error = None;
        if !self.current.is_empty()
            && self.current.len().saturating_add(text.len()) > MAX_PARTIAL_LINE_BYTES
        {
            Self::record_first_error(&mut first_error, self.emit_current(timestamp));
        }
        self.current.push_str(text);
        if self.current.len() >= MAX_PARTIAL_LINE_BYTES {
            Self::record_first_error(&mut first_error, self.emit_current(timestamp));
        }
        first_error.map_or(Ok(()), Err)
    }

    fn emit_current(&mut self, timestamp: &str) -> Result<(), PlainLogError> {
        let line = std::mem::take(&mut self.current);
        self.lines.push_back(line.clone());
        while self.lines.len() > self.max_lines {
            self.lines.pop_front();
        }
        self.write_line(&line, timestamp)
    }

    fn open_file(
        path: &Path,
        max_bytes: u64,
        file_system: Arc<dyn LogFileSystem>,
    ) -> Result<LogFile, PlainLogError> {
        if let Some(parent) = path.parent() {
            file_system
                .create_dir_all(parent)
                .map_err(|source| PlainLogError::io("create parent directory for", path, source))?;
        }
        let pending_cleanup = Self::reconcile_rotation(path, file_system.as_ref())?;
        let handle = file_system
            .open_append(path)
            .map_err(|source| PlainLogError::io("open", path, source))?;
        let bytes_written = handle
            .len()
            .map_err(|source| PlainLogError::io("read metadata for", path, source))?;
        Ok(LogFile {
            path: path.to_path_buf(),
            handle: Some(handle),
            bytes_written,
            max_bytes,
            file_system,
            pending_cleanup,
        })
    }

    fn write_line(&mut self, line: &str, timestamp: &str) -> Result<(), PlainLogError> {
        if self.file.is_none() {
            return Ok(());
        }
        let stamped = format!("{timestamp} {line}\n");
        let stamped_len = stamped.len() as u64;
        let rotate = {
            let Some(file) = self.file.as_mut() else {
                return Ok(());
            };
            Self::ensure_open(file, "reopen")?;
            if stamped_len > file.max_bytes {
                return Err(PlainLogError::io(
                    "persist line within configured limit",
                    &file.path,
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "stamped log line exceeds configured file limit",
                    ),
                ));
            }
            let rotate = file.bytes_written.saturating_add(stamped_len) > file.max_bytes;
            if rotate {
                if let Err(error) = Self::retry_pending_cleanup(file) {
                    return Err(PlainLogError::io(
                        "rotate before bounded write",
                        &file.path,
                        error.source,
                    ));
                }
            }
            rotate
        };
        if rotate {
            self.rotate_file()?;
        }

        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        let write_result = file
            .handle
            .as_mut()
            .ok_or_else(|| PlainLogError::io("reopen", &file.path, io::ErrorKind::NotFound.into()))?
            .write_all(stamped.as_bytes());
        if let Err(source) = write_result {
            Self::reconcile_file_length(file);
            return Err(PlainLogError::io("write", &file.path, source));
        }
        let flush_result = file
            .handle
            .as_mut()
            .ok_or_else(|| PlainLogError::io("reopen", &file.path, io::ErrorKind::NotFound.into()))?
            .flush();
        if let Err(source) = flush_result {
            Self::reconcile_file_length(file);
            return Err(PlainLogError::io("flush", &file.path, source));
        }
        file.bytes_written = file.bytes_written.saturating_add(stamped_len);
        Self::retry_pending_cleanup(file)?;
        Ok(())
    }

    fn rotate_file(&mut self) -> Result<(), PlainLogError> {
        let Some(file) = &mut self.file else {
            return Ok(());
        };
        Self::retry_pending_cleanup(file)?;
        let handle = file.handle.as_mut().ok_or_else(|| {
            PlainLogError::io("reopen", &file.path, io::ErrorKind::NotFound.into())
        })?;
        handle
            .flush()
            .map_err(|source| PlainLogError::io("flush before rotating", &file.path, source))?;
        let path = file.path.clone();
        let incoming = path.with_extension("rotate.log");
        let rotated = path.with_extension("1.log");
        let previous = path.with_extension("1.previous.log");
        file.handle.take();

        if let Err(source) = file.file_system.rename(&path, &incoming) {
            let error = PlainLogError::io("stage active for rotating", &path, source);
            Self::restore_open_handle(file);
            return Err(error);
        }

        let had_backup = match file.file_system.exists(&rotated) {
            Ok(exists) => exists,
            Err(source) => {
                let error = PlainLogError::io("inspect rotated", &rotated, source);
                let _ = file.file_system.rename(&incoming, &path);
                Self::restore_open_handle(file);
                return Err(error);
            }
        };
        if had_backup {
            if let Err(source) = file.file_system.rename(&rotated, &previous) {
                let error = PlainLogError::io("stage previous rotated", &rotated, source);
                let _ = file.file_system.rename(&incoming, &path);
                Self::restore_open_handle(file);
                return Err(error);
            }
        }

        if let Err(source) = file.file_system.rename(&incoming, &rotated) {
            let error = PlainLogError::io("install rotated", &rotated, source);
            if had_backup {
                let _ = file.file_system.rename(&previous, &rotated);
            }
            let _ = file.file_system.rename(&incoming, &path);
            Self::restore_open_handle(file);
            return Err(error);
        }

        file.bytes_written = 0;
        if had_backup {
            file.pending_cleanup = Some(previous);
        }
        file.handle = Some(
            file.file_system
                .open_append(&path)
                .map_err(|source| PlainLogError::io("reopen after rotating", &path, source))?,
        );
        Ok(())
    }

    fn ensure_open(file: &mut LogFile, operation: &'static str) -> Result<(), PlainLogError> {
        if file.handle.is_none() {
            file.pending_cleanup = Self::reconcile_rotation(&file.path, file.file_system.as_ref())?;
            let handle = file
                .file_system
                .open_append(&file.path)
                .map_err(|source| PlainLogError::io(operation, &file.path, source))?;
            file.bytes_written = handle
                .len()
                .map_err(|source| PlainLogError::io("read metadata for", &file.path, source))?;
            file.handle = Some(handle);
        }
        Ok(())
    }

    fn reconcile_file_length(file: &mut LogFile) {
        let conservative_length = file.max_bytes.saturating_add(1);
        file.bytes_written = file
            .handle
            .as_ref()
            .and_then(|handle| handle.len().ok())
            .unwrap_or(conservative_length);
    }

    fn reconcile_rotation(
        path: &Path,
        file_system: &dyn LogFileSystem,
    ) -> Result<Option<PathBuf>, PlainLogError> {
        let staging = path.with_extension("rotate.log");
        let backup = path.with_extension("1.log");
        let previous = path.with_extension("1.previous.log");
        let active_exists = Self::path_exists(file_system, path, "inspect active during startup")?;
        let staging_exists =
            Self::path_exists(file_system, &staging, "inspect staging during startup")?;
        let mut backup_exists =
            Self::path_exists(file_system, &backup, "inspect backup during startup")?;
        let mut previous_exists =
            Self::path_exists(file_system, &previous, "inspect previous during startup")?;

        if staging_exists {
            if active_exists {
                return Err(PlainLogError::io(
                    "reconcile ambiguous staging during startup",
                    &staging,
                    io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "active and rotation staging files both exist",
                    ),
                ));
            }
            if !backup_exists && previous_exists {
                file_system.rename(&previous, &backup).map_err(|source| {
                    PlainLogError::io("restore previous backup during startup", &previous, source)
                })?;
                backup_exists = true;
                previous_exists = false;
            }
            file_system.rename(&staging, path).map_err(|source| {
                PlainLogError::io("restore active during startup", &staging, source)
            })?;
        } else if !backup_exists && previous_exists {
            file_system.rename(&previous, &backup).map_err(|source| {
                PlainLogError::io("restore previous backup during startup", &previous, source)
            })?;
            backup_exists = true;
            previous_exists = false;
        }

        Ok((backup_exists && previous_exists).then_some(previous))
    }

    fn path_exists(
        file_system: &dyn LogFileSystem,
        path: &Path,
        operation: &'static str,
    ) -> Result<bool, PlainLogError> {
        file_system
            .exists(path)
            .map_err(|source| PlainLogError::io(operation, path, source))
    }

    fn restore_open_handle(file: &mut LogFile) {
        if !matches!(file.file_system.exists(&file.path), Ok(true)) {
            return;
        }
        if let Ok(handle) = file.file_system.open_append(&file.path) {
            file.bytes_written = handle.len().unwrap_or(file.bytes_written);
            file.handle = Some(handle);
        }
    }

    fn retry_pending_cleanup(file: &mut LogFile) -> Result<(), PlainLogError> {
        let Some(path) = file.pending_cleanup.clone() else {
            return Ok(());
        };
        match file.file_system.remove_file(&path) {
            Ok(()) => file.pending_cleanup = None,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                file.pending_cleanup = None;
            }
            Err(source) => {
                return Err(PlainLogError::io("remove previous rotated", &path, source));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Fault {
        Write,
        Flush,
        Remove,
        Rename,
        Open,
    }

    #[derive(Debug, Default)]
    struct FakeFileState {
        files: HashMap<PathBuf, Vec<u8>>,
        failure: Option<(Fault, usize)>,
        persistent_failure: Option<Fault>,
    }

    #[derive(Clone, Debug, Default)]
    struct FakeFileSystem {
        state: Arc<Mutex<FakeFileState>>,
    }

    impl FakeFileSystem {
        fn fail_on(&self, fault: Fault, occurrence: usize) {
            self.state.lock().unwrap().failure = Some((fault, occurrence));
        }

        fn fail_always(&self, fault: Fault) {
            self.state.lock().unwrap().persistent_failure = Some(fault);
        }

        fn clear_persistent_failure(&self) {
            self.state.lock().unwrap().persistent_failure = None;
        }

        fn seed(&self, path: &Path, data: &[u8]) {
            self.state
                .lock()
                .unwrap()
                .files
                .insert(path.to_path_buf(), data.to_vec());
        }

        fn read(&self, path: &Path) -> Option<Vec<u8>> {
            self.state.lock().unwrap().files.get(path).cloned()
        }

        fn should_fail(state: &mut FakeFileState, fault: Fault) -> bool {
            if state.persistent_failure == Some(fault) {
                return true;
            }
            let Some((planned, remaining)) = state.failure.as_mut() else {
                return false;
            };
            if *planned != fault {
                return false;
            }
            if *remaining > 1 {
                *remaining -= 1;
                return false;
            }
            state.failure = None;
            true
        }
    }

    impl LogFileSystem for FakeFileSystem {
        fn create_dir_all(&self, _path: &Path) -> io::Result<()> {
            Ok(())
        }

        fn open_append(&self, path: &Path) -> io::Result<Box<dyn LogFileHandle>> {
            let mut state = self.state.lock().unwrap();
            if Self::should_fail(&mut state, Fault::Open) {
                return Err(io::Error::other("injected open failure"));
            }
            state.files.entry(path.to_path_buf()).or_default();
            drop(state);
            Ok(Box::new(FakeFileHandle {
                path: path.to_path_buf(),
                state: Arc::clone(&self.state),
            }))
        }

        fn remove_file(&self, path: &Path) -> io::Result<()> {
            let mut state = self.state.lock().unwrap();
            if Self::should_fail(&mut state, Fault::Remove) {
                return Err(io::Error::other("injected remove failure"));
            }
            state
                .files
                .remove(path)
                .map(|_| ())
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }

        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            let mut state = self.state.lock().unwrap();
            if Self::should_fail(&mut state, Fault::Rename) {
                return Err(io::Error::other("injected rename failure"));
            }
            if state.files.contains_key(to) {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            let data = state
                .files
                .remove(from)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            state.files.insert(to.to_path_buf(), data);
            Ok(())
        }

        fn exists(&self, path: &Path) -> io::Result<bool> {
            Ok(self.state.lock().unwrap().files.contains_key(path))
        }
    }

    #[derive(Debug)]
    struct FakeFileHandle {
        path: PathBuf,
        state: Arc<Mutex<FakeFileState>>,
    }

    impl Write for FakeFileHandle {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let mut state = self.state.lock().unwrap();
            if FakeFileSystem::should_fail(&mut state, Fault::Write) {
                let partial_length = (buffer.len() / 2).max(1).min(buffer.len());
                state
                    .files
                    .entry(self.path.clone())
                    .or_default()
                    .extend_from_slice(&buffer[..partial_length]);
                return Err(io::Error::other("injected partial write failure"));
            }
            state
                .files
                .entry(self.path.clone())
                .or_default()
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            let mut state = self.state.lock().unwrap();
            if FakeFileSystem::should_fail(&mut state, Fault::Flush) {
                return Err(io::Error::other("injected flush failure"));
            }
            Ok(())
        }
    }

    impl LogFileHandle for FakeFileHandle {
        fn len(&self) -> io::Result<u64> {
            Ok(self
                .state
                .lock()
                .unwrap()
                .files
                .get(&self.path)
                .map_or(0, |data| data.len()) as u64)
        }
    }

    fn fake_file_store(file_system: &FakeFileSystem, max_file_bytes: u64) -> PlainLogStore {
        PlainLogStore::file_with_file_system(
            "server.log",
            10,
            max_file_bytes,
            Arc::new(file_system.clone()),
        )
        .unwrap()
    }

    #[test]
    fn flush_failure_keeps_tail_and_later_write_retries_rotation() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(path, b"existing\n");
        let mut logs = fake_file_store(&file_system, 20);
        file_system.fail_on(Fault::Flush, 1);

        let error = logs.ingest_at(b"first\n", "00:00:00").unwrap_err();
        assert_eq!(error.operation(), "flush before rotating");
        assert_eq!(logs.tail(10), ["first"]);
        assert_eq!(file_system.read(path).unwrap(), b"existing\n");

        logs.ingest_at(b"second\n", "00:00:01").unwrap();
        assert_eq!(logs.tail(10), ["first", "second"]);
        assert_eq!(file_system.read(backup).unwrap(), b"existing\n");
        assert_eq!(file_system.read(path).unwrap(), b"00:00:01 second\n");
    }

    #[test]
    fn persistence_failure_does_not_drop_later_completed_lines_from_tail() {
        let file_system = FakeFileSystem::default();
        let mut logs = fake_file_store(&file_system, 1_000);
        file_system.fail_on(Fault::Flush, 1);

        let error = logs.ingest_at(b"first\nsecond\n", "00:00:00").unwrap_err();

        assert_eq!(error.operation(), "flush");
        assert_eq!(logs.tail(10), ["first", "second"]);
    }

    #[test]
    fn note_stays_separate_when_finalizing_partial_output_fails_to_persist() {
        let path = Path::new("server.log");
        let file_system = FakeFileSystem::default();
        let mut logs = fake_file_store(&file_system, 100_000);
        logs.ingest_at(b"partial", "00:00:00").unwrap();
        file_system.fail_on(Fault::Flush, 1);

        let error = logs.note_at("restart", "00:00:01").unwrap_err();

        assert_eq!(error.operation(), "flush");
        assert_eq!(logs.tail(10), ["partial", "[netsustack] restart"]);
        assert_eq!(
            file_system.read(path).unwrap(),
            b"00:00:01 partial\n00:00:01 [netsustack] restart\n"
        );
    }

    #[test]
    fn boundary_emit_failure_does_not_skip_the_triggering_utf8_scalar() {
        let file_system = FakeFileSystem::default();
        let mut logs = fake_file_store(&file_system, 100_000);
        file_system.fail_on(Fault::Flush, 1);
        let mut input = vec![b'a'; MAX_PARTIAL_LINE_BYTES - 1];
        input.extend_from_slice("🦀\n".as_bytes());

        let error = logs.ingest_at(&input, "00:00:00").unwrap_err();

        assert_eq!(error.operation(), "flush");
        assert_eq!(logs.tail(10).concat(), format!("{}🦀", "a".repeat(8_191)));
    }

    #[test]
    fn transient_flush_failure_reconciles_length_before_the_next_rotation() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let file_system = FakeFileSystem::default();
        let mut logs = fake_file_store(&file_system, 20);
        file_system.fail_on(Fault::Flush, 1);

        let error = logs.ingest_at(b"first\n", "00:00:00").unwrap_err();
        assert_eq!(error.operation(), "flush");
        logs.ingest_at(b"second\n", "00:00:01").unwrap();

        assert_eq!(file_system.read(path).unwrap(), b"00:00:01 second\n");
        assert_eq!(file_system.read(backup).unwrap(), b"00:00:00 first\n");
    }

    #[test]
    fn partial_write_failure_reconciles_length_before_the_next_rotation() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let file_system = FakeFileSystem::default();
        let mut logs = fake_file_store(&file_system, 20);
        file_system.fail_on(Fault::Write, 1);

        let error = logs.ingest_at(b"first\n", "00:00:00").unwrap_err();
        assert_eq!(error.operation(), "write");
        logs.ingest_at(b"second\n", "00:00:01").unwrap();

        assert_eq!(file_system.read(path).unwrap(), b"00:00:01 second\n");
        assert_eq!(file_system.read(backup).unwrap(), &b"00:00:00 first\n"[..7]);
    }

    #[test]
    fn active_staging_rename_failure_is_typed_and_recovers() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(path, b"existing\n");
        let mut logs = fake_file_store(&file_system, 20);
        file_system.fail_on(Fault::Rename, 1);

        let error = logs.ingest_at(b"first\n", "00:00:00").unwrap_err();
        assert_eq!(error.operation(), "stage active for rotating");
        assert_eq!(file_system.read(path).unwrap(), b"existing\n");

        logs.ingest_at(b"second\n", "00:00:01").unwrap();
        assert_eq!(file_system.read(backup).unwrap(), b"existing\n");
        assert_eq!(file_system.read(path).unwrap(), b"00:00:01 second\n");
    }

    #[test]
    fn backup_staging_rename_failure_preserves_files_and_recovers() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(path, b"existing\n");
        file_system.seed(backup, b"older\n");
        let mut logs = fake_file_store(&file_system, 20);
        file_system.fail_on(Fault::Rename, 2);

        let error = logs.ingest_at(b"first\n", "00:00:00").unwrap_err();
        assert_eq!(error.operation(), "stage previous rotated");
        assert_eq!(file_system.read(path).unwrap(), b"existing\n");
        assert_eq!(file_system.read(backup).unwrap(), b"older\n");

        logs.ingest_at(b"second\n", "00:00:01").unwrap();
        assert_eq!(file_system.read(backup).unwrap(), b"existing\n");
        assert_eq!(file_system.read(path).unwrap(), b"00:00:01 second\n");
    }

    #[test]
    fn installing_backup_rename_failure_preserves_files_and_recovers() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(path, b"existing\n");
        file_system.seed(backup, b"older\n");
        let mut logs = fake_file_store(&file_system, 20);
        file_system.fail_on(Fault::Rename, 3);

        let error = logs.ingest_at(b"first\n", "00:00:00").unwrap_err();
        assert_eq!(error.operation(), "install rotated");
        assert_eq!(logs.tail(10), ["first"]);
        assert_eq!(file_system.read(path).unwrap(), b"existing\n");
        assert_eq!(file_system.read(backup).unwrap(), b"older\n");

        logs.ingest_at(b"second\n", "00:00:01").unwrap();
        assert_eq!(file_system.read(backup).unwrap(), b"existing\n");
        assert_eq!(file_system.read(path).unwrap(), b"00:00:01 second\n");
    }

    #[test]
    fn backup_cleanup_failure_is_typed_and_retried_by_the_next_write() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let previous = Path::new("server.1.previous.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(path, b"existing\n");
        file_system.seed(backup, b"older\n");
        let mut logs = fake_file_store(&file_system, 20);
        file_system.fail_on(Fault::Remove, 1);

        let error = logs.ingest_at(b"first\n", "00:00:00").unwrap_err();
        assert_eq!(error.operation(), "remove previous rotated");
        assert_eq!(logs.tail(10), ["first"]);
        assert_eq!(file_system.read(path).unwrap(), b"00:00:00 first\n");
        assert_eq!(file_system.read(backup).unwrap(), b"existing\n");
        assert_eq!(file_system.read(previous).unwrap(), b"older\n");

        logs.ingest_at(b"second\n", "00:00:01").unwrap();
        assert_eq!(logs.tail(10), ["first", "second"]);
        assert_eq!(file_system.read(backup).unwrap(), b"00:00:00 first\n");
        assert_eq!(file_system.read(path).unwrap(), b"00:00:01 second\n");
        assert!(file_system.read(previous).is_none());
    }

    #[test]
    fn persistent_backup_cleanup_failure_never_blocks_active_writes() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let previous = Path::new("server.1.previous.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(path, b"active\n");
        file_system.seed(backup, b"current backup\n");
        file_system.seed(previous, b"older backup\n");
        file_system.fail_always(Fault::Remove);
        let mut logs = fake_file_store(&file_system, 100_000);

        for (line, timestamp) in [
            (b"first\n".as_slice(), "00:00:00"),
            (b"second\n", "00:00:01"),
        ] {
            let error = logs.ingest_at(line, timestamp).unwrap_err();
            assert_eq!(error.operation(), "remove previous rotated");
        }

        assert_eq!(logs.tail(10), ["first", "second"]);
        assert_eq!(
            file_system.read(path).unwrap(),
            b"active\n00:00:00 first\n00:00:01 second\n"
        );
        assert_eq!(file_system.read(backup).unwrap(), b"current backup\n");
        assert_eq!(file_system.read(previous).unwrap(), b"older backup\n");
    }

    #[test]
    fn persistent_cleanup_failure_blocks_only_writes_that_would_exceed_the_limit() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let previous = Path::new("server.1.previous.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(path, b"active\n");
        file_system.seed(backup, b"current backup\n");
        file_system.seed(previous, b"older backup\n");
        file_system.fail_always(Fault::Remove);
        let mut logs = fake_file_store(&file_system, 40);

        for (line, timestamp) in [
            (b"first\n".as_slice(), "00:00:00"),
            (b"second\n", "00:00:01"),
        ] {
            let error = logs.ingest_at(line, timestamp).unwrap_err();
            assert_eq!(error.operation(), "remove previous rotated");
        }
        for (line, timestamp) in [
            (b"third\n".as_slice(), "00:00:02"),
            (b"fourth\n", "00:00:03"),
        ] {
            let error = logs.ingest_at(line, timestamp).unwrap_err();
            assert_eq!(error.operation(), "rotate before bounded write");
        }

        let active = file_system.read(path).unwrap();
        assert_eq!(active, b"active\n00:00:00 first\n00:00:01 second\n");
        assert!(active.len() <= 40);
        assert_eq!(logs.tail(10), ["first", "second", "third", "fourth"]);

        file_system.clear_persistent_failure();
        logs.ingest_at(b"fifth\n", "00:00:04").unwrap();
        assert_eq!(file_system.read(path).unwrap(), b"00:00:04 fifth\n");
        assert_eq!(file_system.read(backup).unwrap(), active);
        assert!(file_system.read(previous).is_none());
    }

    #[test]
    fn oversized_stamped_line_stays_memory_only_to_preserve_the_file_limit() {
        let path = Path::new("server.log");
        let file_system = FakeFileSystem::default();
        let mut logs = fake_file_store(&file_system, 10);

        let error = logs.ingest_at(b"line\n", "00:00:00").unwrap_err();

        assert_eq!(error.operation(), "persist line within configured limit");
        assert_eq!(logs.tail(10), ["line"]);
        assert_eq!(file_system.read(path).unwrap(), b"");
    }

    #[test]
    fn startup_recovers_after_active_was_staged() {
        let path = Path::new("server.log");
        let staging = Path::new("server.rotate.log");
        let backup = Path::new("server.1.log");
        let previous = Path::new("server.1.previous.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(staging, b"current\n");
        file_system.seed(backup, b"older\n");

        let mut logs = fake_file_store(&file_system, 20);
        assert_eq!(file_system.read(path).unwrap(), b"current\n");
        assert_eq!(file_system.read(backup).unwrap(), b"older\n");
        assert!(file_system.read(staging).is_none());

        logs.ingest_at(b"next\n", "00:00:00").unwrap();
        assert_eq!(file_system.read(backup).unwrap(), b"current\n");
        assert_eq!(file_system.read(path).unwrap(), b"00:00:00 next\n");
        assert!(file_system.read(previous).is_none());
    }

    #[test]
    fn retry_reconciles_staging_before_reopening_a_missing_active_file() {
        let path = Path::new("server.log");
        let staging = Path::new("server.rotate.log");
        let file_system = FakeFileSystem::default();
        let mut logs = fake_file_store(&file_system, 100_000);
        logs.ingest_at(b"current\n", "00:00:00").unwrap();
        logs.file.as_mut().unwrap().handle.take();
        file_system.rename(path, staging).unwrap();

        logs.ingest_at(b"next\n", "00:00:01").unwrap();

        assert_eq!(
            file_system.read(path).unwrap(),
            b"00:00:00 current\n00:00:01 next\n"
        );
        assert!(file_system.read(staging).is_none());
    }

    #[test]
    fn startup_recovers_after_backup_was_staged() {
        let path = Path::new("server.log");
        let staging = Path::new("server.rotate.log");
        let backup = Path::new("server.1.log");
        let previous = Path::new("server.1.previous.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(staging, b"current\n");
        file_system.seed(previous, b"older\n");

        let mut logs = fake_file_store(&file_system, 20);
        assert_eq!(file_system.read(path).unwrap(), b"current\n");
        assert_eq!(file_system.read(backup).unwrap(), b"older\n");
        assert!(file_system.read(staging).is_none());
        assert!(file_system.read(previous).is_none());

        logs.ingest_at(b"next\n", "00:00:00").unwrap();
        assert_eq!(file_system.read(backup).unwrap(), b"current\n");
        assert_eq!(file_system.read(path).unwrap(), b"00:00:00 next\n");
    }

    #[test]
    fn startup_recovers_after_staging_was_installed_as_backup() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let previous = Path::new("server.1.previous.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(backup, b"current\n");
        file_system.seed(previous, b"older\n");

        let mut logs = fake_file_store(&file_system, 20);
        assert_eq!(file_system.read(path).unwrap(), b"");
        assert_eq!(file_system.read(backup).unwrap(), b"current\n");
        assert_eq!(file_system.read(previous).unwrap(), b"older\n");

        logs.ingest_at(b"next\n", "00:00:00").unwrap();
        assert_eq!(file_system.read(path).unwrap(), b"00:00:00 next\n");
        assert_eq!(file_system.read(backup).unwrap(), b"current\n");
        assert!(file_system.read(previous).is_none());
        logs.ingest_at(b"more\n", "00:00:01").unwrap();
        assert_eq!(file_system.read(backup).unwrap(), b"00:00:00 next\n");
        assert_eq!(file_system.read(path).unwrap(), b"00:00:01 more\n");
    }

    #[test]
    fn startup_recovers_after_new_active_was_created() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let previous = Path::new("server.1.previous.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(path, b"new active\n");
        file_system.seed(backup, b"current\n");
        file_system.seed(previous, b"older\n");

        let mut logs = fake_file_store(&file_system, 20);
        assert_eq!(file_system.read(path).unwrap(), b"new active\n");
        assert_eq!(file_system.read(backup).unwrap(), b"current\n");
        assert_eq!(file_system.read(previous).unwrap(), b"older\n");

        logs.ingest_at(b"next\n", "00:00:00").unwrap();
        assert_eq!(file_system.read(backup).unwrap(), b"new active\n");
        assert_eq!(file_system.read(path).unwrap(), b"00:00:00 next\n");
        assert!(file_system.read(previous).is_none());
    }

    #[test]
    fn reopen_failure_keeps_rotated_data_and_the_next_write_recovers() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(path, b"existing\n");
        let mut logs = fake_file_store(&file_system, 20);
        file_system.fail_on(Fault::Open, 1);

        let error = logs.ingest_at(b"first\n", "00:00:00").unwrap_err();
        assert_eq!(error.operation(), "reopen after rotating");
        assert_eq!(logs.tail(10), ["first"]);
        assert_eq!(file_system.read(backup).unwrap(), b"existing\n");

        logs.ingest_at(b"second\n", "00:00:01").unwrap();
        assert_eq!(logs.tail(10), ["first", "second"]);
        assert_eq!(file_system.read(path).unwrap(), b"00:00:01 second\n");
    }

    #[test]
    fn reopen_failure_with_an_existing_backup_cleans_staging_before_retry() {
        let path = Path::new("server.log");
        let backup = Path::new("server.1.log");
        let previous = Path::new("server.1.previous.log");
        let file_system = FakeFileSystem::default();
        file_system.seed(path, b"existing\n");
        file_system.seed(backup, b"older\n");
        let mut logs = fake_file_store(&file_system, 20);
        file_system.fail_on(Fault::Open, 1);

        let error = logs.ingest_at(b"first\n", "00:00:00").unwrap_err();
        assert_eq!(error.operation(), "reopen after rotating");
        assert_eq!(file_system.read(backup).unwrap(), b"existing\n");
        assert_eq!(file_system.read(previous).unwrap(), b"older\n");

        logs.ingest_at(b"second\n", "00:00:01").unwrap();
        assert_eq!(logs.tail(10), ["first", "second"]);
        assert_eq!(file_system.read(path).unwrap(), b"00:00:01 second\n");
        assert_eq!(file_system.read(backup).unwrap(), b"existing\n");
        assert!(file_system.read(previous).is_none());
    }
}
