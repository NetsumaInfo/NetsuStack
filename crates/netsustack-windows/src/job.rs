use std::{
    ffi::c_void,
    mem::size_of,
    thread,
    time::{Duration, Instant},
};

use windows::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};

use crate::{WindowsError, handles::JobHandle};

pub const STOP_GRACE_PERIOD: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    Cooperative,
    Forced,
}

#[derive(Debug)]
pub(crate) struct Job {
    handle: JobHandle,
}

impl Job {
    pub(crate) fn create_kill_on_close() -> Result<Self, WindowsError> {
        // SAFETY: No security attributes or shared name are supplied; the returned handle is owned.
        let raw = unsafe { CreateJobObjectW(None, None) }
            .map_err(|error| WindowsError::api("CreateJobObjectW", error))?;
        let job = Self {
            handle: JobHandle::new(raw),
        };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: The handle is a live Job and `limits` has the required layout and size.
        unsafe {
            SetInformationJobObject(
                job.handle.raw(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast::<c_void>(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(|error| WindowsError::api("SetInformationJobObject", error))?;
        Ok(job)
    }

    pub(crate) fn raw_handle(&self) -> windows::Win32::Foundation::HANDLE {
        self.handle.raw()
    }

    pub(crate) fn terminate(&self, exit_code: u32) -> Result<(), WindowsError> {
        // SAFETY: The Job handle remains live for the call.
        unsafe { TerminateJobObject(self.handle.raw(), exit_code) }
            .map_err(|error| WindowsError::api("TerminateJobObject", error))
    }

    pub(crate) fn active_processes(&self) -> Result<u32, WindowsError> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: The Job is live and the accounting buffer matches the requested information class.
        unsafe {
            QueryInformationJobObject(
                Some(self.handle.raw()),
                JobObjectBasicAccountingInformation,
                (&raw mut accounting).cast::<c_void>(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                None,
            )
        }
        .map_err(|error| WindowsError::api("QueryInformationJobObject", error))?;
        Ok(accounting.ActiveProcesses)
    }

    pub(crate) fn wait_empty(&self, timeout: Duration) -> Result<bool, WindowsError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.active_processes()? == 0 {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
