use std::fmt;

use thiserror::Error;
use windows::Win32::Foundation::{CloseHandle, HANDLE};

#[derive(Debug, Error)]
pub enum WindowsError {
    #[error("{operation} failed with Windows code {code}: {message}")]
    Api {
        operation: &'static str,
        code: i32,
        message: String,
    },
    #[error("{operation} failed: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid terminal size {columns}x{rows}")]
    InvalidTerminalSize { columns: u16, rows: u16 },
    #[error("invalid {field}: {reason}")]
    InvalidInput {
        field: &'static str,
        reason: &'static str,
    },
    #[error("{operation} reached end of stream before the requested output")]
    EndOfStream { operation: &'static str },
    #[error("{operation} exceeded its {limit}-byte buffer limit")]
    BufferLimit {
        operation: &'static str,
        limit: usize,
    },
    #[error("timed out while waiting for {operation}")]
    Timeout { operation: &'static str },
}

impl WindowsError {
    pub(crate) fn api(operation: &'static str, error: windows::core::Error) -> Self {
        Self::Api {
            operation,
            code: error.code().0,
            message: error.message(),
        }
    }

    pub(crate) fn io(operation: &'static str, source: std::io::Error) -> Self {
        Self::Io { operation, source }
    }
}

pub(crate) struct OwnedHandle(HANDLE);

// SAFETY: A kernel HANDLE is process-global and may be used or closed from any
// thread. `OwnedHandle` preserves unique ownership when it is moved.
unsafe impl Send for OwnedHandle {}

impl OwnedHandle {
    pub(crate) fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl fmt::Debug for OwnedHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("OwnedHandle").field(&self.0).finish()
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: This wrapper is the unique owner of a valid Win32 handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

macro_rules! distinct_handle {
    ($name:ident) => {
        #[derive(Debug)]
        pub(crate) struct $name(OwnedHandle);

        impl $name {
            pub(crate) fn new(handle: HANDLE) -> Self {
                Self(OwnedHandle::new(handle))
            }

            pub(crate) fn raw(&self) -> HANDLE {
                self.0.raw()
            }
        }
    };
}

distinct_handle!(PipeHandle);
distinct_handle!(ProcessHandle);
distinct_handle!(ThreadHandle);
distinct_handle!(JobHandle);
