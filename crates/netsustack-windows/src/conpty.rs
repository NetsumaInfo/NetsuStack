use std::{
    collections::VecDeque,
    ffi::c_void,
    mem::size_of,
    ptr,
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{
            HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, STILL_ACTIVE, SetHandleInformation,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{ReadFile, WriteFile},
        System::{
            Console::{ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole},
            Pipes::CreatePipe,
            Threading::{
                CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
                EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
                InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
                PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
                UpdateProcThreadAttribute, WaitForSingleObject,
            },
        },
    },
    core::{HRESULT, PCWSTR, PWSTR},
};

mod launch;
mod options;

pub use options::{SpawnOptions, TerminalSize};

use crate::{
    WindowsError,
    handles::{PipeHandle, ProcessHandle, ThreadHandle},
    job::{Job, STOP_GRACE_PERIOD, StopOutcome},
};
use launch::{environment_block, launch_buffers, normalized_current_directory};

const READ_UNTIL_LIMIT: usize = 768 * 1024;
const INPUT_CHUNK_SIZE: usize = 64 * 1024;
const INPUT_MESSAGE_LIMIT: usize = 1024 * 1024;
const INPUT_QUEUE_DEPTH: usize = 16;

#[derive(Debug)]
struct PseudoConsole(HPCON);

impl PseudoConsole {
    fn resize(&self, size: TerminalSize) -> Result<(), WindowsError> {
        // SAFETY: The HPCON is live and uniquely owned by this wrapper.
        unsafe { ResizePseudoConsole(self.0, size.coord()?) }
            .map_err(|error| WindowsError::api("ResizePseudoConsole", error))
    }
}

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        // SAFETY: This wrapper uniquely owns the HPCON and closes it once.
        unsafe { ClosePseudoConsole(self.0) };
    }
}

struct AttributeList {
    _storage: Vec<usize>,
    _job_handles: Box<[HANDLE; 1]>,
    raw: LPPROC_THREAD_ATTRIBUTE_LIST,
}

impl AttributeList {
    fn for_process(console: &PseudoConsole, job: &Job) -> Result<Self, WindowsError> {
        let mut bytes = 0_usize;
        // SAFETY: A null-list call is the documented size probe.
        let _ = unsafe { InitializeProcThreadAttributeList(None, 2, None, &mut bytes) };
        if bytes == 0 {
            return Err(WindowsError::api(
                "InitializeProcThreadAttributeList(size)",
                windows::core::Error::from_win32(),
            ));
        }
        let word = size_of::<usize>();
        let mut storage = vec![0_usize; bytes.div_ceil(word)];
        let raw = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        // SAFETY: `storage` is aligned and retains the requested number of bytes.
        unsafe { InitializeProcThreadAttributeList(Some(raw), 2, None, &mut bytes) }
            .map_err(|error| WindowsError::api("InitializeProcThreadAttributeList", error))?;
        let job_handles = Box::new([job.raw_handle()]);
        let list = Self {
            _storage: storage,
            _job_handles: job_handles,
            raw,
        };
        // SAFETY: The initialized list owns its storage and the HPCON remains live.
        unsafe {
            UpdateProcThreadAttribute(
                list.raw,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                Some(console.0.0 as *const c_void),
                size_of::<HPCON>(),
                None,
                None,
            )
        }
        .map_err(|error| WindowsError::api("UpdateProcThreadAttribute", error))?;
        // SAFETY: `_job_handles` is the one-element HANDLE list referenced by
        // the attribute, and both it and the Job outlive CreateProcessW.
        unsafe {
            UpdateProcThreadAttribute(
                list.raw,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                Some(list._job_handles.as_ptr().cast::<c_void>()),
                size_of::<HANDLE>(),
                None,
                None,
            )
        }
        .map_err(|error| WindowsError::api("UpdateProcThreadAttribute(Job)", error))?;
        Ok(list)
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: `raw` was initialized once and its storage is still live.
        unsafe { DeleteProcThreadAttributeList(self.raw) };
    }
}

#[derive(Debug)]
struct InputPump {
    sender: Option<SyncSender<Vec<u8>>>,
    worker: Option<JoinHandle<()>>,
}

impl InputPump {
    fn start(pipe: PipeHandle) -> Self {
        let (sender, receiver) = sync_channel::<Vec<u8>>(INPUT_QUEUE_DEPTH);
        let worker = thread::spawn(move || {
            while let Ok(bytes) = receiver.recv() {
                let mut remaining = bytes.as_slice();
                while !remaining.is_empty() {
                    let current = &remaining[..remaining.len().min(INPUT_CHUNK_SIZE)];
                    let mut written = 0_u32;
                    // SAFETY: The worker uniquely owns the synchronous pipe and
                    // `remaining` stays readable for the duration of the call.
                    let result =
                        unsafe { WriteFile(pipe.raw(), Some(current), Some(&mut written), None) };
                    if result.is_err() || written == 0 {
                        return;
                    }
                    remaining = &remaining[written as usize..];
                }
            }
        });
        Self {
            sender: Some(sender),
            worker: Some(worker),
        }
    }

    fn send(&self, bytes: &[u8]) -> Result<(), WindowsError> {
        let sender = self.sender.as_ref().ok_or(WindowsError::EndOfStream {
            operation: "ConPTY input",
        })?;
        if bytes.len() > INPUT_MESSAGE_LIMIT {
            return Err(WindowsError::BufferLimit {
                operation: "ConPTY input message",
                limit: INPUT_MESSAGE_LIMIT,
            });
        }
        match sender.try_send(bytes.to_vec()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(WindowsError::BufferLimit {
                operation: "ConPTY input queue",
                limit: INPUT_MESSAGE_LIMIT * INPUT_QUEUE_DEPTH,
            }),
            Err(TrySendError::Disconnected(_)) => Err(WindowsError::EndOfStream {
                operation: "ConPTY input",
            }),
        }
    }

    fn begin_shutdown(&mut self) -> Option<JoinHandle<()>> {
        self.sender.take();
        self.worker.take()
    }
}

#[derive(Debug, Default)]
struct OutputState {
    chunks: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    dropped: bool,
    closed: bool,
    error: Option<WindowsError>,
}

#[derive(Debug)]
struct OutputShared {
    state: Mutex<OutputState>,
    changed: Condvar,
}

#[derive(Debug)]
struct OutputPump {
    shared: Arc<OutputShared>,
    worker: Option<JoinHandle<()>>,
}

impl OutputPump {
    fn start(pipe: PipeHandle) -> Self {
        let shared = Arc::new(OutputShared {
            state: Mutex::new(OutputState::default()),
            changed: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::spawn(move || {
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                let mut read = 0_u32;
                // SAFETY: The worker uniquely owns the synchronous output pipe
                // and the fixed buffer remains writable for the call.
                let result =
                    unsafe { ReadFile(pipe.raw(), Some(&mut buffer), Some(&mut read), None) };
                let mut state = worker_shared
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match result {
                    Ok(()) if read > 0 => {
                        let chunk = buffer[..read as usize].to_vec();
                        state.queued_bytes += chunk.len();
                        state.chunks.push_back(chunk);
                        while state.queued_bytes > READ_UNTIL_LIMIT {
                            if let Some(dropped) = state.chunks.pop_front() {
                                state.queued_bytes -= dropped.len();
                                state.dropped = true;
                            } else {
                                break;
                            }
                        }
                    }
                    Ok(()) => {
                        state.closed = true;
                        worker_shared.changed.notify_all();
                        return;
                    }
                    Err(error) if error.code() == HRESULT::from_win32(109) => {
                        state.closed = true;
                        worker_shared.changed.notify_all();
                        return;
                    }
                    Err(error) => {
                        state.error = Some(WindowsError::api("ReadFile(ConPTY output)", error));
                        state.closed = true;
                        worker_shared.changed.notify_all();
                        return;
                    }
                }
                worker_shared.changed.notify_all();
            }
        });
        Self {
            shared,
            worker: Some(worker),
        }
    }

    fn take_worker(&mut self) -> Option<JoinHandle<()>> {
        self.worker.take()
    }
}

#[derive(Debug)]
pub struct ConPtyProcess {
    job: Option<Job>,
    input: Option<InputPump>,
    retired_input_workers: Vec<JoinHandle<()>>,
    output: Option<OutputPump>,
    console: Option<PseudoConsole>,
    process: Option<ProcessHandle>,
    process_id: u32,
    exit_code: Option<u32>,
    stop_outcome: Option<StopOutcome>,
}

impl ConPtyProcess {
    pub fn spawn(options: SpawnOptions) -> Result<Self, WindowsError> {
        let cwd = options
            .cwd
            .canonicalize()
            .map_err(|source| WindowsError::io("canonicalize cwd", source))?;
        let (console_input, input) = pipe_pair(false)?;
        let (output, console_output) = pipe_pair(true)?;
        // SAFETY: The terminal-side pipe handles remain live for CreatePseudoConsole.
        let raw_console = unsafe {
            CreatePseudoConsole(
                options.size.coord()?,
                console_input.raw(),
                console_output.raw(),
                0,
            )
        }
        .map_err(|error| WindowsError::api("CreatePseudoConsole", error))?;
        let console = PseudoConsole(raw_console);
        let attached = (|| {
            let job = Job::create_kill_on_close()?;
            let attributes = AttributeList::for_process(&console, &job)?;
            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            // ConPTY must override redirected handles inherited from test runners
            // and GUI hosts. This null-handle pattern is the established ConPTY
            // host workaround while bInheritHandles remains false.
            startup.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
            startup.lpAttributeList = attributes.raw;
            let (program, mut command_line) = match &options.raw_cmd_command {
                Some(command) => launch::raw_cmd_launch_buffers(&options.program, command)?,
                None => launch_buffers(&options.program, &options.arguments)?,
            };
            let cwd = normalized_current_directory(cwd.as_os_str())?;
            let environment = environment_block(&options.environment)?;
            let mut information = PROCESS_INFORMATION::default();
            let flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT;
            // SAFETY: All UTF-16 buffers and the initialized attribute list outlive this call.
            unsafe {
                CreateProcessW(
                    PCWSTR(program.as_ptr()),
                    Some(PWSTR(command_line.as_mut_ptr())),
                    None,
                    None,
                    false,
                    flags,
                    Some(environment.as_ptr().cast()),
                    PCWSTR(cwd.as_ptr()),
                    &startup.StartupInfo,
                    &mut information,
                )
            }
            .map_err(|error| WindowsError::api("CreateProcessW", error))?;
            let process = ProcessHandle::new(information.hProcess);
            let thread_handle = ThreadHandle::new(information.hThread);
            debug_assert!(!thread_handle.raw().is_invalid());
            drop(thread_handle);
            Ok((job, process, information.dwProcessId))
        })();
        // ConPTY keeps the terminal-side handles during client attachment. Release
        // our copies after the CreateProcessW attempt on both success and failure.
        drop(console_input);
        drop(console_output);
        let (job, process, process_id) = match attached {
            Ok(attached) => attached,
            Err(error) => {
                drop(output);
                drop(input);
                drop(console);
                return Err(error);
            }
        };
        let input = InputPump::start(input);
        let output = OutputPump::start(output);
        Ok(Self {
            job: Some(job),
            input: Some(input),
            retired_input_workers: Vec::new(),
            output: Some(output),
            console: Some(console),
            process: Some(process),
            process_id,
            exit_code: None,
            stop_outcome: None,
        })
    }

    pub fn process_id(&self) -> u32 {
        self.process_id
    }

    pub fn write_input(&self, bytes: &[u8]) -> Result<(), WindowsError> {
        self.input
            .as_ref()
            .ok_or(WindowsError::EndOfStream {
                operation: "ConPTY input",
            })?
            .send(bytes)
    }

    pub fn close_input(&mut self) {
        if let Some(mut input) = self.input.take()
            && let Some(worker) = input.begin_shutdown()
        {
            self.retired_input_workers.push(worker);
        }
    }

    pub fn read_until(
        &mut self,
        needle: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, WindowsError> {
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        let shared = Arc::clone(
            &self
                .output
                .as_ref()
                .ok_or(WindowsError::EndOfStream {
                    operation: "ConPTY output",
                })?
                .shared,
        );
        loop {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.dropped {
                return Err(WindowsError::BufferLimit {
                    operation: "ConPTY output queue",
                    limit: READ_UNTIL_LIMIT,
                });
            }
            while let Some(chunk) = state.chunks.pop_front() {
                state.queued_bytes -= chunk.len();
                if output.len().saturating_add(chunk.len()) > READ_UNTIL_LIMIT {
                    return Err(WindowsError::BufferLimit {
                        operation: "ConPTY output",
                        limit: READ_UNTIL_LIMIT,
                    });
                }
                output.extend_from_slice(&chunk);
                if needle.is_empty() || output.windows(needle.len()).any(|part| part == needle) {
                    return Ok(output);
                }
            }
            if let Some(error) = state.error.take() {
                return Err(error);
            }
            if state.closed {
                drop(state);
                self.finalize_terminal();
                return Err(WindowsError::EndOfStream {
                    operation: "ConPTY output",
                });
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(WindowsError::Timeout {
                    operation: "ConPTY output",
                });
            }
            let wait = (deadline - now).min(Duration::from_millis(25));
            let (guard, _) = shared
                .changed
                .wait_timeout(state, wait)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            drop(guard);

            if self.active_processes()? == 0 && self.console.is_some() {
                self.finalize_terminal();
            }
        }
    }

    pub fn resize(&self, size: TerminalSize) -> Result<(), WindowsError> {
        self.console
            .as_ref()
            .ok_or(WindowsError::EndOfStream {
                operation: "ConPTY resize",
            })?
            .resize(size)
    }

    pub fn active_processes(&self) -> Result<u32, WindowsError> {
        self.job.as_ref().map_or(Ok(0), Job::active_processes)
    }

    pub fn wait_for_exit(&mut self, timeout: Duration) -> Result<Option<u32>, WindowsError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.capture_root_exit_code()?;
            if self.active_processes()? == 0 {
                self.finalize_terminal();
                return Ok(self.exit_code);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
    }

    pub fn stop(&mut self) -> Result<StopOutcome, WindowsError> {
        if let Some(outcome) = self.stop_outcome {
            return Ok(outcome);
        }
        if self.job.is_none() {
            return Ok(StopOutcome::Cooperative);
        }
        // Ctrl+C delivery is best effort. A broken input channel must never
        // prevent the Job Object fallback from reclaiming remaining descendants.
        let _ = self.write_input(&[0x03]);
        let result = (|| {
            let job = self.job.as_ref().expect("job checked above");
            if job.wait_empty(STOP_GRACE_PERIOD)? {
                Ok(StopOutcome::Cooperative)
            } else {
                job.terminate(1)?;
                if !job.wait_empty(Duration::from_secs(2))? {
                    return Err(WindowsError::Timeout {
                        operation: "terminated Job Object",
                    });
                }
                Ok(StopOutcome::Forced)
            }
        })();
        if result.is_err()
            && let Some(job) = self.job.as_ref()
        {
            let _ = job.terminate(1);
        }
        self.finalize_terminal();
        if let Ok(outcome) = result {
            self.stop_outcome = Some(outcome);
        }
        result
    }

    fn capture_root_exit_code(&mut self) -> Result<(), WindowsError> {
        if self.exit_code.is_some() {
            return Ok(());
        }
        let Some(process) = self.process.as_ref() else {
            return Ok(());
        };
        // SAFETY: The process handle is live for the duration of this poll.
        let wait = unsafe { WaitForSingleObject(process.raw(), 0) };
        if wait == WAIT_TIMEOUT {
            return Ok(());
        }
        if wait != WAIT_OBJECT_0 {
            return Err(WindowsError::api(
                "WaitForSingleObject",
                windows::core::Error::from_win32(),
            ));
        }
        let mut exit_code = 0_u32;
        // SAFETY: The process is signaled and the output pointer is writable.
        unsafe { GetExitCodeProcess(process.raw(), &mut exit_code) }
            .map_err(|error| WindowsError::api("GetExitCodeProcess", error))?;
        if exit_code != STILL_ACTIVE.0 as u32 {
            self.exit_code = Some(exit_code);
        }
        Ok(())
    }

    fn finalize_terminal(&mut self) {
        if self.console.is_none() {
            return;
        }
        if let Some(worker) = self.input.as_mut().and_then(InputPump::begin_shutdown) {
            self.retired_input_workers.push(worker);
        }
        let output_worker = self.output.as_mut().and_then(OutputPump::take_worker);
        self.input.take();
        // Closing a kill-on-close Job is the unconditional cleanup fallback,
        // including when native wait or terminate calls failed in `stop`.
        self.job.take();
        // The reader stays active while ClosePseudoConsole emits its final frame.
        self.console.take();
        for worker in self.retired_input_workers.drain(..) {
            let _ = worker.join();
        }
        if let Some(worker) = output_worker {
            let _ = worker.join();
        }
        if self.exit_code.is_none()
            && let Some(process) = self.process.as_ref()
        {
            let mut code = 0_u32;
            // SAFETY: The process has left its Job and the output pointer is valid.
            if unsafe { GetExitCodeProcess(process.raw(), &mut code) }.is_ok()
                && code != STILL_ACTIVE.0 as u32
            {
                self.exit_code = Some(code);
            }
        }
        self.process.take();
    }
}

impl Drop for ConPtyProcess {
    fn drop(&mut self) {
        // Terminate the process tree before tearing down its terminal. Input is
        // disconnected first; the output worker stays alive to drain the final
        // ConPTY frame while ClosePseudoConsole runs on pre-24H2 Windows.
        if let Some(job) = self.job.as_ref() {
            let _ = job.terminate(1);
            let _ = job.wait_empty(Duration::from_secs(2));
        }
        self.finalize_terminal();
        self.output.take();
    }
}

fn pipe_pair(parent_reads: bool) -> Result<(PipeHandle, PipeHandle), WindowsError> {
    let mut first = HANDLE::default();
    let mut second = HANDLE::default();
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    // SAFETY: Output pointers and security attributes are valid.
    unsafe { CreatePipe(&mut first, &mut second, Some(&attributes), 0) }
        .map_err(|error| WindowsError::api("CreatePipe", error))?;
    let first = PipeHandle::new(first);
    let second = PipeHandle::new(second);
    let parent = if parent_reads { &first } else { &second };
    // SAFETY: `parent` is a live handle; clearing inheritance affects only its flags.
    unsafe { SetHandleInformation(parent.raw(), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
        .map_err(|error| WindowsError::api("SetHandleInformation", error))?;
    Ok((first, second))
}
