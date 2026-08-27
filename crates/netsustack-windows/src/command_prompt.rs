use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

use crate::WindowsError;

pub(crate) fn system_command_prompt_path() -> Result<PathBuf, WindowsError> {
    let mut capacity = 260_usize;
    loop {
        let mut buffer = vec![0_u16; capacity];
        // SAFETY: `buffer` is writable for the full slice passed to Win32.
        let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
        if length == 0 {
            return Err(WindowsError::api(
                "GetSystemDirectoryW",
                windows::core::Error::from_win32(),
            ));
        }
        if length < capacity {
            buffer.truncate(length);
            let mut path = PathBuf::from(OsString::from_wide(&buffer));
            path.push("cmd.exe");
            return Ok(path);
        }
        capacity = length.saturating_add(1);
        if capacity > 32_768 {
            return Err(WindowsError::BufferLimit {
                operation: "system command interpreter path",
                limit: 32_768,
            });
        }
    }
}
