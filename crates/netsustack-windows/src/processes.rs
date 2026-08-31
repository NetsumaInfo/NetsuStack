use std::{
    collections::BTreeSet,
    net::IpAddr,
    path::{Path, PathBuf},
};

#[cfg(windows)]
use std::{ffi::OsString, os::windows::ffi::OsStringExt};

use thiserror::Error;

#[cfg(windows)]
use crate::handles::ProcessHandle;
use crate::{TcpListenerEntry, WindowsError, list_tcp_listeners};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub creation_time: u64,
    pub executable: PathBuf,
    pub local_addresses: Vec<IpAddr>,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockerProcessProvenance {
    TrustedDocker,
    DockerLikeUntrustedOrInconclusive,
    DefinitelyNonDocker,
}

impl ProcessIdentity {
    pub fn new(
        pid: u32,
        creation_time: u64,
        executable: PathBuf,
        local_addresses: impl IntoIterator<Item = IpAddr>,
        port: u16,
    ) -> Self {
        Self {
            pid,
            creation_time,
            executable,
            local_addresses: local_addresses
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            port,
        }
    }
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error(transparent)]
    Windows(#[from] WindowsError),
    #[error("no listener for PID {pid} owns TCP port {port}")]
    ListenerNotFound { pid: u32, port: u16 },
    #[error("the listener on TCP port {port} changed after it was inspected")]
    ListenerChanged {
        port: u16,
        expected: Box<ProcessIdentity>,
        actual: Option<Box<ProcessIdentity>>,
    },
    #[error("PID {pid} is a protected process and cannot be terminated")]
    ProtectedProcess { pid: u32 },
}

pub fn is_protected_process(identity: &ProcessIdentity) -> bool {
    if identity.pid <= 4 {
        return true;
    }
    let executable = identity
        .executable
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        executable.as_str(),
        "system"
            | "registry"
            | "smss.exe"
            | "csrss.exe"
            | "wininit.exe"
            | "services.exe"
            | "lsass.exe"
            | "winlogon.exe"
            | "secure system"
    ) || docker_process_provenance(identity) != DockerProcessProvenance::DefinitelyNonDocker
}

pub fn docker_process_provenance(identity: &ProcessIdentity) -> DockerProcessProvenance {
    docker_process_provenance_with(identity, trusted_docker_installation_root())
}

fn docker_process_provenance_with(
    identity: &ProcessIdentity,
    trusted_root: Option<PathBuf>,
) -> DockerProcessProvenance {
    if !has_docker_like_basename(identity) {
        return DockerProcessProvenance::DefinitelyNonDocker;
    }
    match trusted_root {
        Some(root) if has_trusted_docker_provenance(identity, std::slice::from_ref(&root)) => {
            DockerProcessProvenance::TrustedDocker
        }
        Some(_) | None => DockerProcessProvenance::DockerLikeUntrustedOrInconclusive,
    }
}

fn has_docker_like_basename(identity: &ProcessIdentity) -> bool {
    let Some(name) = identity
        .executable
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return false;
    };
    name.eq_ignore_ascii_case("com.docker.backend.exe")
        || name.eq_ignore_ascii_case("vpnkit.exe")
        || name.eq_ignore_ascii_case("docker-proxy.exe")
}

fn has_trusted_docker_provenance(identity: &ProcessIdentity, roots: &[PathBuf]) -> bool {
    let Ok(executable) = identity.executable.canonicalize() else {
        return false;
    };
    roots.iter().any(|root| {
        let Ok(root) = root.canonicalize() else {
            return false;
        };
        let Ok(relative) = executable.strip_prefix(root) else {
            return false;
        };
        matches_trusted_docker_relative_path(relative)
    })
}

fn matches_trusted_docker_relative_path(relative: &Path) -> bool {
    [
        Path::new("resources/com.docker.backend.exe"),
        Path::new("resources/vpnkit.exe"),
        Path::new("resources/bin/docker-proxy.exe"),
    ]
    .contains(&relative)
}

#[cfg(windows)]
fn trusted_docker_installation_root() -> Option<PathBuf> {
    use windows::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
    };

    let folder_id = FOLDERID_ProgramFiles;
    let pointer = unsafe { SHGetKnownFolderPath(&folder_id, KF_FLAG_DEFAULT, None).ok()? };
    let program_files = unsafe { pointer.to_string().ok() };
    unsafe { CoTaskMemFree(Some(pointer.0.cast())) };
    program_files
        .map(PathBuf::from)
        .map(|root| root.join("Docker").join("Docker"))
}

#[cfg(not(windows))]
fn trusted_docker_installation_root() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
pub fn snapshot_process_for_port(port: u16, pid: u32) -> Result<ProcessIdentity, SnapshotError> {
    let listener = list_tcp_listeners()?
        .into_iter()
        .find(|listener| listener.port == port && listener.pid == pid)
        .ok_or(SnapshotError::ListenerNotFound { pid, port })?;
    snapshot_process_for_listener(listener)
}

#[cfg(windows)]
pub fn snapshot_process_for_listener(
    listener: TcpListenerEntry,
) -> Result<ProcessIdentity, SnapshotError> {
    snapshot_process_for_listener_with(listener, list_tcp_listeners, |listener| {
        let handle = open_process(listener.pid, false)?;
        identity_from_handle(
            &handle,
            listener.pid,
            listener.local_addresses.clone(),
            listener.port,
        )
    })
}

fn snapshot_process_for_listener_with(
    listener: TcpListenerEntry,
    mut listeners: impl FnMut() -> Result<Vec<TcpListenerEntry>, WindowsError>,
    mut inspect: impl FnMut(&TcpListenerEntry) -> Result<ProcessIdentity, WindowsError>,
) -> Result<ProcessIdentity, SnapshotError> {
    if !listeners()?.contains(&listener) {
        return Err(SnapshotError::ListenerNotFound {
            pid: listener.pid,
            port: listener.port,
        });
    }
    match inspect(&listener) {
        Ok(identity) => Ok(identity),
        Err(error) => {
            let still_present = listeners()
                .map(|listeners| listeners.contains(&listener))
                .unwrap_or(true);
            if still_present {
                Err(error.into())
            } else {
                Err(SnapshotError::ListenerNotFound {
                    pid: listener.pid,
                    port: listener.port,
                })
            }
        }
    }
}

#[cfg(windows)]
pub fn terminate_process(expected: &ProcessIdentity) -> Result<(), SnapshotError> {
    use windows::Win32::System::Threading::TerminateProcess;

    if is_protected_process(expected) {
        return Err(SnapshotError::ProtectedProcess { pid: expected.pid });
    }

    terminate_process_with(
        expected,
        || open_process(expected.pid, true),
        |handle| {
            identity_from_handle(
                handle,
                expected.pid,
                expected.local_addresses.clone(),
                expected.port,
            )
        },
        list_tcp_listeners,
        |handle| {
            // SAFETY: `handle` is a live process handle opened with
            // PROCESS_TERMINATE; identity and exact port ownership were
            // revalidated while it was pinned.
            unsafe { TerminateProcess(handle.raw(), 1) }
                .map_err(|error| WindowsError::api("TerminateProcess", error))
        },
        || listener_changed(expected),
    )
}

fn terminate_process_with<Handle>(
    expected: &ProcessIdentity,
    mut open: impl FnMut() -> Result<Handle, WindowsError>,
    mut inspect: impl FnMut(&Handle) -> Result<ProcessIdentity, WindowsError>,
    mut listeners: impl FnMut() -> Result<Vec<TcpListenerEntry>, WindowsError>,
    mut terminate: impl FnMut(&Handle) -> Result<(), WindowsError>,
    mut changed: impl FnMut() -> SnapshotError,
) -> Result<(), SnapshotError> {
    // Keeping this handle open pins the process object. Even if the target
    // exits after validation, this handle can never refer to a reused PID.
    let handle = match open() {
        Ok(handle) => handle,
        Err(error) => {
            return Err(classify_process_access_failure(
                expected,
                error,
                &mut listeners,
                &mut changed,
            ));
        }
    };
    let actual = match inspect(&handle) {
        Ok(actual) => actual,
        Err(error) => {
            return Err(classify_process_access_failure(
                expected,
                error,
                &mut listeners,
                &mut changed,
            ));
        }
    };
    let owns_port = listeners()?.iter().any(|listener| {
        listener.local_addresses == expected.local_addresses
            && listener.port == expected.port
            && listener.pid == expected.pid
    });
    if actual != *expected || !owns_port {
        return Err(changed());
    }
    terminate(&handle)?;
    Ok(())
}

fn classify_process_access_failure(
    expected: &ProcessIdentity,
    original: WindowsError,
    listeners: &mut impl FnMut() -> Result<Vec<TcpListenerEntry>, WindowsError>,
    changed: &mut impl FnMut() -> SnapshotError,
) -> SnapshotError {
    match listeners() {
        Ok(current)
            if current
                .iter()
                .any(|listener| listener_matches(expected, listener)) =>
        {
            SnapshotError::Windows(original)
        }
        Ok(_) => changed(),
        Err(_) => SnapshotError::Windows(original),
    }
}

fn listener_matches(expected: &ProcessIdentity, listener: &TcpListenerEntry) -> bool {
    listener.local_addresses == expected.local_addresses
        && listener.port == expected.port
        && listener.pid == expected.pid
}

#[cfg(windows)]
fn listener_changed(expected: &ProcessIdentity) -> SnapshotError {
    let actual = current_identity_for_port(expected.port, expected.pid)
        .ok()
        .flatten();
    SnapshotError::ListenerChanged {
        port: expected.port,
        expected: Box::new(expected.clone()),
        actual: actual.map(Box::new),
    }
}

#[cfg(windows)]
fn current_identity_for_port(
    port: u16,
    excluded_pid: u32,
) -> Result<Option<ProcessIdentity>, SnapshotError> {
    let candidate = list_tcp_listeners()?
        .into_iter()
        .find(|listener| listener.port == port && listener.pid != excluded_pid);
    candidate
        .map(|listener| snapshot_process_for_port(port, listener.pid))
        .transpose()
}

#[cfg(windows)]
fn open_process(pid: u32, terminate: bool) -> Result<ProcessHandle, WindowsError> {
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };

    let access = if terminate {
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE
    } else {
        PROCESS_QUERY_LIMITED_INFORMATION
    };
    // SAFETY: No handle is inherited and `pid` is provided by the kernel TCP
    // table. Success returns a uniquely owned process handle.
    let handle = unsafe { OpenProcess(access, false, pid) }
        .map_err(|error| WindowsError::api("OpenProcess", error))?;
    Ok(ProcessHandle::new(handle))
}

#[cfg(windows)]
fn identity_from_handle(
    handle: &ProcessHandle,
    pid: u32,
    local_addresses: Vec<IpAddr>,
    port: u16,
) -> Result<ProcessIdentity, WindowsError> {
    use windows::{
        Win32::{
            Foundation::FILETIME,
            System::Threading::{GetProcessTimes, QueryFullProcessImageNameW},
        },
        core::PWSTR,
    };

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: All FILETIME pointers are valid and writable; the process handle
    // has PROCESS_QUERY_LIMITED_INFORMATION access.
    unsafe {
        GetProcessTimes(
            handle.raw(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    }
    .map_err(|error| WindowsError::api("GetProcessTimes", error))?;

    let mut path = vec![0_u16; 32_768];
    let mut length = path.len() as u32;
    // SAFETY: `path` is writable for `length` UTF-16 code units and the handle
    // carries process-query access.
    unsafe {
        QueryFullProcessImageNameW(
            handle.raw(),
            Default::default(),
            PWSTR(path.as_mut_ptr()),
            &mut length,
        )
    }
    .map_err(|error| WindowsError::api("QueryFullProcessImageNameW", error))?;
    path.truncate(length as usize);
    let executable = PathBuf::from(OsString::from_wide(&path));
    let creation_time =
        u64::from(creation.dwLowDateTime) | (u64::from(creation.dwHighDateTime) << 32);
    Ok(ProcessIdentity::new(
        pid,
        creation_time,
        executable,
        local_addresses,
        port,
    ))
}

#[cfg(not(windows))]
pub fn snapshot_process_for_port(port: u16, pid: u32) -> Result<ProcessIdentity, SnapshotError> {
    Err(SnapshotError::ListenerNotFound { pid, port })
}

#[cfg(not(windows))]
pub fn snapshot_process_for_listener(
    listener: TcpListenerEntry,
) -> Result<ProcessIdentity, SnapshotError> {
    Err(SnapshotError::ListenerNotFound {
        pid: listener.pid,
        port: listener.port,
    })
}

#[cfg(not(windows))]
pub fn terminate_process(expected: &ProcessIdentity) -> Result<(), SnapshotError> {
    if is_protected_process(expected) {
        Err(SnapshotError::ProtectedProcess { pid: expected.pid })
    } else {
        Err(SnapshotError::ListenerNotFound {
            pid: expected.pid,
            port: expected.port,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, net::Ipv4Addr};

    use super::*;

    #[test]
    fn exact_executables_under_an_injected_docker_desktop_root_are_trusted() {
        let directory = tempfile::tempdir().expect("create Docker root");
        for relative in [
            PathBuf::from("resources/com.docker.backend.exe"),
            PathBuf::from("resources/vpnkit.exe"),
            PathBuf::from("resources/bin/docker-proxy.exe"),
        ] {
            let executable = directory.path().join(relative);
            std::fs::create_dir_all(executable.parent().expect("executable parent"))
                .expect("create Docker directory");
            std::fs::write(&executable, b"fixture").expect("create Docker executable");
            let identity =
                ProcessIdentity::new(500, 2, executable, [IpAddr::V4(Ipv4Addr::LOCALHOST)], 5173);

            assert_eq!(
                docker_process_provenance_with(&identity, Some(directory.path().to_path_buf())),
                DockerProcessProvenance::TrustedDocker
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn undecodable_component_inside_trusted_root_cannot_collapse_into_trusted_path() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};

        let directory = tempfile::tempdir().expect("create Docker root");
        let invalid_component = OsString::from_wide(&[0xd800]);
        let executable = directory
            .path()
            .join(invalid_component)
            .join("resources")
            .join("com.docker.backend.exe");
        std::fs::create_dir_all(executable.parent().expect("executable parent"))
            .expect("create invalid-WTF-16 directory");
        std::fs::write(&executable, b"fixture").expect("create spoof executable");
        let identity =
            ProcessIdentity::new(500, 2, executable, [IpAddr::V4(Ipv4Addr::LOCALHOST)], 5173);

        assert_eq!(
            docker_process_provenance_with(&identity, Some(directory.path().to_path_buf())),
            DockerProcessProvenance::DockerLikeUntrustedOrInconclusive
        );
    }

    #[test]
    fn docker_like_identity_is_inconclusive_when_canonicalization_or_root_lookup_fails() {
        let identity = ProcessIdentity::new(
            500,
            2,
            PathBuf::from(r"C:\missing\com.docker.backend.exe"),
            [IpAddr::V4(Ipv4Addr::LOCALHOST)],
            5173,
        );
        let directory = tempfile::tempdir().expect("create injected Docker root");

        assert_eq!(
            docker_process_provenance_with(&identity, Some(directory.path().to_path_buf())),
            DockerProcessProvenance::DockerLikeUntrustedOrInconclusive
        );
        assert_eq!(
            docker_process_provenance_with(&identity, None),
            DockerProcessProvenance::DockerLikeUntrustedOrInconclusive
        );
        assert!(is_protected_process(&identity));
    }

    #[test]
    fn ordinary_process_is_definitely_not_docker() {
        let identity = identity();

        assert_eq!(
            docker_process_provenance_with(&identity, None),
            DockerProcessProvenance::DefinitelyNonDocker
        );
        assert!(!is_protected_process(&identity));
    }

    #[test]
    fn failed_process_inspection_becomes_not_found_when_exact_listener_disappears() {
        let listener = listener();
        let list_calls = Cell::new(0);

        let error = snapshot_process_for_listener_with(
            listener.clone(),
            || {
                let call = list_calls.get();
                list_calls.set(call + 1);
                Ok(if call == 0 {
                    vec![listener.clone()]
                } else {
                    Vec::new()
                })
            },
            |_| Err(inspection_error()),
        )
        .expect_err("disappeared listener must be reported as released");

        assert!(matches!(
            error,
            SnapshotError::ListenerNotFound {
                pid: 900,
                port: 5173
            }
        ));
        assert_eq!(list_calls.get(), 2);
    }

    #[test]
    fn failed_process_inspection_preserves_windows_error_while_listener_still_exists() {
        let listener = listener();
        let list_calls = Cell::new(0);

        let error = snapshot_process_for_listener_with(
            listener.clone(),
            || {
                list_calls.set(list_calls.get() + 1);
                Ok(vec![listener.clone()])
            },
            |_| Err(inspection_error()),
        )
        .expect_err("live inaccessible listener must preserve the inspection error");

        assert!(matches!(
            error,
            SnapshotError::Windows(WindowsError::Api {
                operation: "OpenProcess",
                code: 5,
                ..
            })
        ));
        assert_eq!(list_calls.get(), 2);
    }

    #[test]
    fn terminate_open_error_is_preserved_when_the_exact_listener_still_exists() {
        let listener = listener();
        let expected = identity();
        let list_calls = Cell::new(0);

        let error = terminate_process_with(
            &expected,
            || Err::<(), _>(inspection_error()),
            |_| unreachable!("there is no opened handle"),
            || {
                list_calls.set(list_calls.get() + 1);
                Ok(vec![listener.clone()])
            },
            |_| unreachable!("there is no opened handle"),
            || changed_error(&expected),
        )
        .expect_err("a live inaccessible owner must preserve OpenProcess failure");

        assert!(matches!(
            error,
            SnapshotError::Windows(WindowsError::Api {
                operation: "OpenProcess",
                code: 5,
                ..
            })
        ));
        assert_eq!(list_calls.get(), 1);
    }

    #[test]
    fn terminate_identity_error_is_changed_only_after_exact_listener_disappears() {
        let expected = identity();
        let list_calls = Cell::new(0);

        let error = terminate_process_with(
            &expected,
            || Ok(()),
            |_| Err(inspection_error()),
            || {
                list_calls.set(list_calls.get() + 1);
                Ok(Vec::new())
            },
            |_| unreachable!("identity inspection failed"),
            || changed_error(&expected),
        )
        .expect_err("a proven disappearance must be classified as changed");

        assert!(matches!(error, SnapshotError::ListenerChanged { .. }));
        assert_eq!(list_calls.get(), 1);
    }

    #[test]
    fn terminate_recheck_error_does_not_hide_the_original_open_error() {
        let expected = identity();

        let error = terminate_process_with(
            &expected,
            || Err::<(), _>(inspection_error()),
            |_| unreachable!("there is no opened handle"),
            || Err(WindowsError::api_code("GetExtendedTcpTable", 87)),
            |_| unreachable!("there is no opened handle"),
            || changed_error(&expected),
        )
        .expect_err("an inconclusive recheck must preserve the original failure");

        assert!(matches!(
            error,
            SnapshotError::Windows(WindowsError::Api {
                operation: "OpenProcess",
                code: 5,
                ..
            })
        ));
    }

    fn listener() -> TcpListenerEntry {
        TcpListenerEntry::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5173, 900)
    }

    fn identity() -> ProcessIdentity {
        ProcessIdentity::new(
            900,
            123,
            PathBuf::from(r"C:\fixture.exe"),
            [IpAddr::V4(Ipv4Addr::LOCALHOST)],
            5173,
        )
    }

    fn changed_error(expected: &ProcessIdentity) -> SnapshotError {
        SnapshotError::ListenerChanged {
            port: expected.port,
            expected: Box::new(expected.clone()),
            actual: None,
        }
    }

    fn inspection_error() -> WindowsError {
        WindowsError::Api {
            operation: "OpenProcess",
            code: 5,
            message: "access denied".into(),
        }
    }
}
