#![cfg(windows)]

use std::{
    ffi::c_void,
    fs,
    mem::size_of,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    process::ExitCode,
};

use netsustack_windows::{DockerCli, DockerError};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, SetInformationJobObject,
        },
        Threading::{
            GetCurrentProcess, GetCurrentProcessId, GetProcessHandleCount, OpenProcess,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
            TerminateProcess, WaitForSingleObject,
        },
    },
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let mode = arguments
        .next()
        .ok_or_else(|| "missing docker-pipe-probe mode".to_owned())?;
    let job = contain_current_process_in_kill_on_close_job()?;
    if mode == "--reap-only" {
        let pid_file = arguments
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| "missing descendant PID file".to_owned())?;
        reject_extra_arguments(&mut arguments)?;
        return reap_descendants(&pid_file, job);
    }
    if mode != "--require-job" {
        return Err(format!(
            "unsupported docker-pipe-probe mode: {}",
            mode.to_string_lossy()
        ));
    }
    let executable = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "missing docker-cli-fixture path".to_owned())?;
    reject_extra_arguments(&mut arguments)?;
    let pid_file = std::env::temp_dir().join(format!(
        "netsustack-docker-pipe-probe-{}.pids",
        std::process::id()
    ));
    fs::write(&pid_file, []).map_err(|error| error.to_string())?;
    // SAFETY: This dedicated single-threaded probe mutates only its own
    // environment before spawning any fixture processes.
    unsafe { std::env::set_var("NETSUSTACK_DOCKER_FIXTURE_PID_FILE", &pid_file) };

    let result = run_measurements(&executable, &pid_file, job);
    let cleanup = reap_descendants(&pid_file, job);
    // SAFETY: No fixture process is spawned after this point.
    unsafe { std::env::remove_var("NETSUSTACK_DOCKER_FIXTURE_PID_FILE") };
    let remove = fs::remove_file(&pid_file)
        .map_err(|error| format!("remove probe PID file failed: {error}"));
    combine_results(combine_results(result, cleanup), remove)
}

fn reject_extra_arguments(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    if arguments.next().is_some() {
        Err("unexpected docker-pipe-probe argument".to_owned())
    } else {
        Ok(())
    }
}

fn run_measurements(executable: &Path, pid_file: &Path, job: HANDLE) -> Result<(), String> {
    let docker = DockerCli::new(executable.to_path_buf());

    expect_hanging_timeout(&docker)?;
    expect_noisy_output_limit(&docker)?;
    reap_descendants(pid_file, job)?;
    fs::write(pid_file, []).map_err(|error| error.to_string())?;

    let initial_threads = current_thread_count()?;
    let initial_handles = current_handle_count()?;
    for _ in 0..3 {
        expect_hanging_timeout(&docker)?;
        expect_noisy_output_limit(&docker)?;
    }
    let final_threads = current_thread_count()?;
    let final_handles = current_handle_count()?;

    if final_threads != initial_threads {
        return Err(format!(
            "reader thread count changed after warmup: {initial_threads} -> {final_threads}"
        ));
    }
    if final_handles != initial_handles {
        return Err(format!(
            "handle count changed after warmup: {initial_handles} -> {final_handles}"
        ));
    }
    Ok(())
}

fn expect_hanging_timeout(docker: &DockerCli) -> Result<(), String> {
    match docker.container_for_listener(&[IpAddr::V4(Ipv4Addr::LOCALHOST)], 65534) {
        Err(DockerError::Timeout { command, .. }) if command.ends_with("reader") => Ok(()),
        other => Err(format!("unexpected hanging-pipe result: {other:?}")),
    }
}

fn expect_noisy_output_limit(docker: &DockerCli) -> Result<(), String> {
    match docker.container_for_listener(&[IpAddr::V4(Ipv4Addr::LOCALHOST)], 65532) {
        Err(DockerError::OutputLimitExceeded { .. }) => Ok(()),
        other => Err(format!("unexpected noisy-pipe result: {other:?}")),
    }
}

fn reap_descendants(pid_file: &Path, job: HANDLE) -> Result<(), String> {
    let contents = fs::read_to_string(pid_file).map_err(|error| error.to_string())?;
    for (index, line) in contents.lines().enumerate() {
        if !line.is_empty() {
            let pid = line.parse::<u32>().map_err(|error| {
                format!(
                    "invalid fixture descendant PID on line {}: {line}: {error}",
                    index + 1
                )
            })?;
            reap_descendant(pid, job)?;
        }
    }
    Ok(())
}

fn reap_descendant(pid: u32, job: HANDLE) -> Result<(), String> {
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    }
    .map_err(|error| format!("OpenProcess failed for fixture descendant {pid}: {error}"))?;

    let mut contained = windows::core::BOOL::default();
    if let Err(error) = unsafe { IsProcessInJob(handle, Some(job), &mut contained) } {
        return close_process_handle(
            handle,
            Err(format!(
                "IsProcessInJob failed for fixture descendant {pid}: {error}"
            )),
        );
    }
    if !contained.as_bool() {
        return close_process_handle(
            handle,
            Err(format!(
                "fixture descendant {pid} escaped the probe Job Object"
            )),
        );
    }
    if let Err(error) = unsafe { TerminateProcess(handle, 1) } {
        return close_process_handle(
            handle,
            Err(format!(
                "TerminateProcess failed for fixture descendant {pid}: {error}"
            )),
        );
    }
    let wait = unsafe { WaitForSingleObject(handle, 5_000) };
    let result = if wait == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err(format!(
            "fixture descendant {pid} was not signaled after termination: wait result {wait:?}"
        ))
    };
    close_process_handle(handle, result)
}

fn close_process_handle(handle: HANDLE, result: Result<(), String>) -> Result<(), String> {
    let close = unsafe { CloseHandle(handle) }
        .map_err(|error| format!("CloseHandle failed for fixture descendant: {error}"));
    combine_results(result, close)
}

fn combine_results(first: Result<(), String>, second: Result<(), String>) -> Result<(), String> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(format!("{first}; {second}")),
    }
}

fn contain_current_process_in_kill_on_close_job() -> Result<HANDLE, String> {
    let job = unsafe { CreateJobObjectW(None, None) }.map_err(|error| error.to_string())?;
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if let Err(error) = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast::<c_void>(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } {
        return Err(close_job_after_setup_error(job, error.to_string()));
    }
    if let Err(error) = unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) } {
        return Err(close_job_after_setup_error(job, error.to_string()));
    }
    let mut contained = windows::core::BOOL::default();
    unsafe { IsProcessInJob(GetCurrentProcess(), Some(job), &mut contained) }
        .map_err(|error| error.to_string())?;
    if !contained.as_bool() {
        return Err("probe process was not assigned to its Job Object".to_owned());
    }

    // Keep the last Job handle open until process teardown. Closing it while
    // the probe is still a member would terminate the probe itself; Windows
    // closes it at process exit and kills any descendant missed by reaping.
    Ok(job)
}

fn close_job_after_setup_error(job: HANDLE, error: String) -> String {
    match unsafe { CloseHandle(job) } {
        Ok(()) => error,
        Err(close_error) => {
            format!("{error}; CloseHandle failed for probe Job Object: {close_error}")
        }
    }
}

fn current_handle_count() -> Result<u32, String> {
    let mut count = 0;
    unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) }
        .map_err(|error| error.to_string())?;
    Ok(count)
}

fn current_thread_count() -> Result<usize, String> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }
        .map_err(|error| error.to_string())?;
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let pid = unsafe { GetCurrentProcessId() };
    let mut count = 0;
    if unsafe { Thread32First(snapshot, &mut entry) }.is_ok() {
        loop {
            if entry.th32OwnerProcessID == pid {
                count += 1;
            }
            if unsafe { Thread32Next(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }
    unsafe { CloseHandle(snapshot) }
        .map_err(|error| format!("CloseHandle failed for thread snapshot: {error}"))?;
    Ok(count)
}
