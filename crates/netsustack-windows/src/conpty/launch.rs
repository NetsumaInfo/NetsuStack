use std::{
    cmp::Ordering,
    ffi::{OsStr, OsString},
    os::windows::ffi::OsStrExt,
    path::Path,
};

use windows::Win32::{
    Globalization::{CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringOrdinal},
    System::SystemInformation::GetSystemDirectoryW,
};

use crate::WindowsError;

fn command_line(program: &Path, arguments: &[OsString]) -> Result<Vec<u16>, WindowsError> {
    let mut command = quote_windows_argument(program.as_os_str())?;
    for argument in arguments {
        command.push(b' ' as u16);
        command.extend(quote_windows_argument(argument)?);
    }
    command.push(0);
    Ok(command)
}

pub(super) fn launch_buffers(
    program: &Path,
    arguments: &[OsString],
) -> Result<(Vec<u16>, Vec<u16>), WindowsError> {
    let resolved = program
        .canonicalize()
        .unwrap_or_else(|_| program.to_owned());
    let script = if is_batch_file(&resolved) {
        Some(resolved)
    } else if is_batch_file(program) {
        Some(program.to_owned())
    } else {
        None
    };
    let Some(script) = script else {
        return Ok((
            wide_nul(program.as_os_str(), "program path")?,
            command_line(program, arguments)?,
        ));
    };

    let interpreter = system_command_prompt()?;
    let script = user_path_wide(script.as_os_str());
    if script.contains(&(b'"' as u16)) || script.last() == Some(&(b'\\' as u16)) {
        return Err(WindowsError::InvalidInput {
            field: "batch file path",
            reason: "contains a quote or ends with a backslash",
        });
    }
    // This encoding mirrors Rust's hardened `std::process::Command` batch
    // launcher and prevents cmd metacharacters and `%VAR%` expansion from
    // changing the command being executed (CVE-2024-24576 class issues).
    let mut line: Vec<u16> = "cmd.exe /e:ON /v:OFF /d /c \"".encode_utf16().collect();
    line.push(b'"' as u16);
    line.extend(script);
    line.push(b'"' as u16);
    for argument in arguments {
        line.push(b' ' as u16);
        append_batch_argument(&mut line, argument)?;
    }
    line.push(b'"' as u16);
    line.push(0);
    Ok((interpreter, line))
}

fn is_batch_file(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

fn system_command_prompt() -> Result<Vec<u16>, WindowsError> {
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
            buffer.extend("\\cmd.exe".encode_utf16());
            buffer.push(0);
            return Ok(buffer);
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

fn append_batch_argument(command: &mut Vec<u16>, argument: &OsStr) -> Result<(), WindowsError> {
    let value: Vec<u16> = argument.encode_wide().collect();
    if value.contains(&0) {
        return Err(WindowsError::InvalidInput {
            field: "batch file argument",
            reason: "contains an embedded NUL",
        });
    }
    if value
        .iter()
        .any(|unit| *unit == b'\r' as u16 || *unit == b'\n' as u16)
    {
        return Err(WindowsError::InvalidInput {
            field: "batch file argument",
            reason: "contains a carriage return or newline",
        });
    }

    const UNQUOTED: &[u8] = b"#$*+-./:?@\\_";
    let mut quote = value.is_empty() || value.last() == Some(&(b'\\' as u16));
    quote |= value.iter().any(|unit| {
        if *unit > 0x7f {
            return false;
        }
        let byte = *unit as u8;
        !(byte.is_ascii_alphanumeric() || UNQUOTED.contains(&byte)) || byte.is_ascii_control()
    });
    if quote {
        command.push(b'"' as u16);
    }

    let mut backslashes = 0_usize;
    for unit in value {
        if unit == b'\\' as u16 {
            backslashes += 1;
        } else {
            if unit == b'"' as u16 {
                command.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
                command.push(b'"' as u16);
            } else if unit == b'%' as u16 {
                command.extend("%%cd:~,".encode_utf16());
            }
            backslashes = 0;
        }
        command.push(unit);
    }
    if quote {
        command.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
        command.push(b'"' as u16);
    }
    Ok(())
}

fn quote_windows_argument(argument: &OsStr) -> Result<Vec<u16>, WindowsError> {
    let value: Vec<u16> = argument.encode_wide().collect();
    if value.contains(&0) {
        return Err(WindowsError::InvalidInput {
            field: "process argument",
            reason: "contains an embedded NUL",
        });
    }
    if !value.is_empty()
        && !value
            .iter()
            .any(|unit| *unit == b' ' as u16 || *unit == b'\t' as u16 || *unit == b'"' as u16)
    {
        return Ok(value);
    }

    let mut quoted = vec![b'"' as u16];
    let mut backslashes = 0;
    for unit in value {
        if unit == b'\\' as u16 {
            backslashes += 1;
        } else {
            if unit == b'"' as u16 {
                quoted.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
            } else {
                quoted.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
            }
            quoted.push(unit);
            backslashes = 0;
        }
    }
    quoted.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
    quoted.push(b'"' as u16);
    Ok(quoted)
}

pub(super) fn environment_block(
    environment: &[(OsString, OsString)],
) -> Result<Vec<u16>, WindowsError> {
    let mut entries = environment.to_vec();
    for (name, value) in &entries {
        validate_environment_entry(name, value)?;
    }
    entries.sort_by(|left, right| compare_environment_names(&left.0, &right.0));
    if entries
        .windows(2)
        .any(|pair| compare_environment_names(&pair[0].0, &pair[1].0) == Ordering::Equal)
    {
        return Err(WindowsError::InvalidInput {
            field: "environment",
            reason: "contains duplicate case-insensitive variable names",
        });
    }

    let mut block = Vec::new();
    for (name, value) in entries {
        block.extend(name.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

pub(super) fn validate_environment_entry(name: &OsStr, value: &OsStr) -> Result<(), WindowsError> {
    let name: Vec<u16> = name.encode_wide().collect();
    let value: Vec<u16> = value.encode_wide().collect();
    if name.is_empty() {
        return Err(WindowsError::InvalidInput {
            field: "environment variable name",
            reason: "is empty",
        });
    }
    if name.contains(&0) || value.contains(&0) {
        return Err(WindowsError::InvalidInput {
            field: "environment",
            reason: "contains an embedded NUL",
        });
    }
    let equals = b'=' as u16;
    let invalid_equals = if name.first() == Some(&equals) {
        name[1..].contains(&equals)
    } else {
        name.contains(&equals)
    };
    if invalid_equals {
        return Err(WindowsError::InvalidInput {
            field: "environment variable name",
            reason: "contains an invalid equals sign",
        });
    }
    Ok(())
}

pub(super) fn compare_environment_names(left: &OsStr, right: &OsStr) -> Ordering {
    let left: Vec<u16> = left.encode_wide().collect();
    let right: Vec<u16> = right.encode_wide().collect();
    // SAFETY: Both slices are valid UTF-16 buffers for the duration of the call.
    match unsafe { CompareStringOrdinal(&left, &right, true) } {
        result if result == CSTR_LESS_THAN => Ordering::Less,
        result if result == CSTR_GREATER_THAN => Ordering::Greater,
        result if result == CSTR_EQUAL => Ordering::Equal,
        _ => left.cmp(&right),
    }
}

pub(super) fn normalized_current_directory(value: &OsStr) -> Result<Vec<u16>, WindowsError> {
    nul_terminate(user_path_wide(value), "working directory")
}

fn user_path_wide(value: &OsStr) -> Vec<u16> {
    let wide: Vec<u16> = value.encode_wide().collect();
    let verbatim = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    let unc = [b'U' as u16, b'N' as u16, b'C' as u16, b'\\' as u16];
    if wide.starts_with(&verbatim) {
        let rest = &wide[verbatim.len()..];
        if rest.len() >= unc.len()
            && rest[..unc.len()]
                .iter()
                .zip(unc)
                .all(|(left, right)| ascii_uppercase(*left) == right)
        {
            let mut path = vec![b'\\' as u16, b'\\' as u16];
            path.extend_from_slice(&rest[unc.len()..]);
            path
        } else {
            rest.to_vec()
        }
    } else {
        wide
    }
}

fn ascii_uppercase(unit: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&unit) {
        unit - (b'a' - b'A') as u16
    } else {
        unit
    }
}

fn wide_nul(value: &OsStr, field: &'static str) -> Result<Vec<u16>, WindowsError> {
    nul_terminate(value.encode_wide().collect(), field)
}

fn nul_terminate(mut value: Vec<u16>, field: &'static str) -> Result<Vec<u16>, WindowsError> {
    if value.contains(&0) {
        return Err(WindowsError::InvalidInput {
            field,
            reason: "contains an embedded NUL",
        });
    }
    value.push(0);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::os::windows::ffi::OsStringExt;

    use super::*;

    #[test]
    fn command_line_quotes_crt_backslashes_without_changing_non_unicode_units() {
        let non_unicode = OsString::from_wide(&[b'x' as u16, 0xd800, b'y' as u16]);
        let line = command_line(
            Path::new(r"C:\Program Files\tool.exe"),
            &[
                OsString::from(r#"ends with\"#),
                OsString::from(r#"a\"b"#),
                non_unicode.clone(),
            ],
        )
        .expect("valid command line");

        assert!(
            line.windows(3)
                .any(|part| part == [b'x' as u16, 0xd800, b'y' as u16])
        );
        assert_eq!(line.last(), Some(&0));
        assert!(
            line.windows(3)
                .any(|part| { part == [b'\\' as u16, b'\\' as u16, b'"' as u16] })
        );
    }

    #[test]
    fn encoding_rejects_embedded_nuls_and_empty_environment_is_double_terminated() {
        assert_eq!(environment_block(&[]).unwrap(), [0, 0]);
        let nul = OsString::from_wide(&[b'a' as u16, 0, b'b' as u16]);
        assert!(quote_windows_argument(&nul).is_err());
        assert!(environment_block(&[(OsString::from("KEY"), nul)]).is_err());
    }

    #[test]
    fn environment_rejects_case_insensitive_duplicates_and_invalid_names() {
        assert!(
            environment_block(&[
                (OsString::from("Path"), OsString::from("one")),
                (OsString::from("PATH"), OsString::from("two")),
            ])
            .is_err()
        );
        assert!(
            environment_block(&[
                (OsString::from("ÉDITION"), OsString::from("one")),
                (OsString::from("édition"), OsString::from("two")),
            ])
            .is_err()
        );
        assert!(
            environment_block(&[(OsString::from("BAD=NAME"), OsString::from("value"))]).is_err()
        );
    }

    #[test]
    fn current_directory_strips_verbatim_drive_and_unc_prefixes() {
        let drive = normalized_current_directory(OsStr::new(r"\\?\C:\work")).unwrap();
        let unc = normalized_current_directory(OsStr::new(r"\\?\UNC\server\share")).unwrap();

        assert_eq!(
            String::from_utf16(&drive[..drive.len() - 1]).unwrap(),
            r"C:\work"
        );
        assert_eq!(
            String::from_utf16(&unc[..unc.len() - 1]).unwrap(),
            r"\\server\share"
        );
    }
}
