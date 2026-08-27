#![cfg(windows)]

use std::{
    ffi::{OsStr, OsString},
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use netsustack_windows::{
    ConPtyProcess, ShellKind, ShellPreference, SpawnOptions, StopOutcome, TerminalSize,
    resolve_executable, select_shell,
};
use tempfile::TempDir;
use windows::Win32::{
    Foundation::{CloseHandle, STILL_ACTIVE},
    System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, GetProcessHandleCount, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION,
    },
};

const ANSI_FIXTURE: &str = env!("CARGO_BIN_EXE_ansi-terminal-fixture");
const CHILD_TREE_FIXTURE: &str = env!("CARGO_BIN_EXE_child-tree-fixture");
static WINDOWS_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn conpty_transports_vt_utf8_input_and_initial_terminal_size() {
    let _guard = test_guard();
    let mut process = spawn_ansi(&[], TerminalSize::new(93, 31));

    let startup = process
        .read_until(b"READY", Duration::from_secs(3))
        .expect("fixture startup output");
    assert!(
        startup
            .windows(b"\x1b[31m".len())
            .any(|window| window == b"\x1b[31m")
    );
    assert!(String::from_utf8_lossy(&startup).contains("VT:café-🦀"));
    assert!(String::from_utf8_lossy(&startup).contains("SIZE:93x31"));

    process
        .write_input("héllo-世界\r\n".as_bytes())
        .expect("UTF-8 input write");
    let echoed = process
        .read_until("INPUT:héllo-世界".as_bytes(), Duration::from_secs(3))
        .expect("UTF-8 echo");
    assert!(String::from_utf8_lossy(&echoed).contains("INPUT:héllo-世界"));
}

#[test]
fn conpty_accepts_a_valid_double_nul_empty_environment() {
    let _guard = test_guard();
    let mut options = SpawnOptions::new(
        PathBuf::from(ANSI_FIXTURE),
        std::iter::empty(),
        fixture_cwd(),
        TerminalSize::new(80, 24),
    );
    options.clear_environment();
    let mut process = ConPtyProcess::spawn(options).expect("empty environment spawn");

    process
        .read_until(b"READY", Duration::from_secs(3))
        .expect("fixture ready");
}

#[test]
fn spawn_environment_overrides_and_removes_names_case_insensitively() {
    let _guard = test_guard();
    let mut options = SpawnOptions::new(
        PathBuf::from(ANSI_FIXTURE),
        [
            OsString::from("--print-env"),
            OsString::from("MIXED_NAME"),
            OsString::from("NO_COLOR"),
        ],
        fixture_cwd(),
        TerminalSize::new(80, 24),
    );
    options.clear_environment();
    options
        .set_environment("Mixed_Name".into(), "inherited".into())
        .expect("initial environment value");
    options
        .set_environment("mixed_name".into(), "server".into())
        .expect("case-insensitive override");
    options
        .set_environment("NO_COLOR".into(), "1".into())
        .expect("NO_COLOR setup");
    options.remove_environment(OsStr::new("no_color"));

    let mut process = ConPtyProcess::spawn(options).expect("environment fixture spawn");
    let output = process
        .read_until(b"ENV:NO_COLOR=<missing>", Duration::from_secs(3))
        .expect("environment output");
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("ENV:MIXED_NAME=server"));
}

#[test]
fn read_until_reports_eof_and_bounds_output_without_the_needle() {
    let _guard = test_guard();
    let mut exited = spawn_ansi(&["--exit-immediately"], TerminalSize::new(80, 24));
    let eof = exited
        .read_until(b"never", Duration::from_secs(3))
        .expect_err("exited process reports EOF");
    assert!(matches!(
        eof,
        netsustack_windows::WindowsError::EndOfStream { .. }
    ));

    let mut noisy = spawn_ansi(&["--spam"], TerminalSize::new(80, 24));
    let bounded = noisy
        .read_until(b"never", Duration::from_secs(10))
        .expect_err("noisy output is bounded");
    assert!(
        matches!(
            bounded,
            netsustack_windows::WindowsError::BufferLimit { .. }
        ),
        "unexpected noisy-output error: {bounded:?}"
    );
}

#[test]
fn eof_finalizes_terminal_resources_while_the_runtime_is_retained() {
    let _guard = test_guard();
    let mut warmup = spawn_ansi(&["--exit-immediately"], TerminalSize::new(40, 10));
    let _ = warmup.read_until(b"never", Duration::from_secs(3));
    drop(warmup);
    std::thread::sleep(Duration::from_millis(100));
    let before = current_process_handle_count();

    let mut process = spawn_ansi(&["--exit-immediately"], TerminalSize::new(40, 10));
    assert!(matches!(
        process.read_until(b"never", Duration::from_secs(3)),
        Err(netsustack_windows::WindowsError::EndOfStream { .. })
    ));
    assert!(
        process.resize(TerminalSize::new(41, 11)).is_err(),
        "EOF must close the retained pseudo-console"
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    while current_process_handle_count() > before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(current_process_handle_count(), before);
    drop(process);
}

#[test]
fn dedicated_input_and_output_pumps_avoid_bidirectional_pipe_deadlock() {
    let _guard = test_guard();
    let mut process = spawn_ansi(&["--duplex-stress"], TerminalSize::new(80, 24));
    let mut input = Vec::with_capacity(32 * 1024);
    for _ in 0..16 * 1024 {
        input.extend_from_slice(b"i\r\n");
    }
    input.extend_from_slice(b"DUPLEX-END\r\n");
    process
        .write_input(&input)
        .expect("large input is queued without blocking output drainage");

    let output = process
        .read_until(b"DUPLEX-READY", Duration::from_secs(10))
        .expect("duplex fixture completes");
    assert!(
        output
            .windows(b"DUPLEX-READY".len())
            .any(|part| part == b"DUPLEX-READY")
    );
}

#[test]
fn rejected_oversized_input_is_atomic_and_does_not_enqueue_a_prefix() {
    let _guard = test_guard();
    let mut process = spawn_ansi(&[], TerminalSize::new(80, 24));
    process
        .read_until(b"READY", Duration::from_secs(3))
        .expect("fixture ready");

    let oversized = vec![b'x'; 2 * 1024 * 1024];
    assert!(matches!(
        process.write_input(&oversized),
        Err(netsustack_windows::WindowsError::BufferLimit { .. })
    ));
    process
        .write_input(b"after-rejection\r\n")
        .expect("valid input after rejection");
    process
        .read_until(b"INPUT:after-rejection", Duration::from_secs(3))
        .expect("no oversized prefix was queued");
}

#[test]
fn resize_pseudo_console_changes_the_size_observed_by_the_child() {
    let _guard = test_guard();
    let mut process = spawn_ansi(&[], TerminalSize::new(80, 24));
    process
        .read_until(b"READY", Duration::from_secs(3))
        .expect("fixture ready");

    process
        .resize(TerminalSize::new(121, 42))
        .expect("real ConPTY resize");
    process.write_input(b"size\r\n").expect("size query input");
    let resized = process
        .read_until(b"SIZE:121x42", Duration::from_secs(3))
        .expect("resized terminal report");
    assert!(String::from_utf8_lossy(&resized).contains("SIZE:121x42"));
}

#[test]
fn shell_auto_prefers_pwsh_and_falls_back_to_cmd_and_resolves_cmd_shims() {
    let _guard = test_guard();
    let temp = TempDir::new().expect("temporary PATH");
    let pwsh = temp.path().join("pwsh.exe");
    fs::write(&pwsh, b"").expect("pwsh fixture");

    let selected = select_shell(ShellPreference::Auto, &[temp.path().to_owned()])
        .expect("auto shell with pwsh");
    assert_eq!(selected.kind(), ShellKind::PowerShell7);
    assert_eq!(selected.executable(), pwsh);

    let empty = TempDir::new().expect("empty PATH");
    let trusted_cmd = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"))
        .join("System32")
        .join("cmd.exe");
    let fallback = select_shell(ShellPreference::Auto, &[empty.path().to_owned()])
        .expect("auto shell fallback");
    assert_eq!(fallback.kind(), ShellKind::Cmd);
    assert!(
        fallback
            .executable()
            .to_string_lossy()
            .eq_ignore_ascii_case(&trusted_cmd.to_string_lossy())
    );

    let cmd = temp.path().join("cmd.exe");
    fs::write(&cmd, b"").expect("fake cmd executable");
    let selected_cmd = select_shell(ShellPreference::Cmd, &[temp.path().to_owned()])
        .expect("explicit cmd selection");
    assert!(
        selected_cmd
            .executable()
            .to_string_lossy()
            .eq_ignore_ascii_case(&trusted_cmd.to_string_lossy())
    );
    assert_ne!(selected_cmd.executable(), cmd);

    let shim_dir = temp.path().join("shim folder");
    fs::create_dir(&shim_dir).expect("cmd shim directory");
    let shim = shim_dir.join("tool.cmd");
    fs::write(&shim, b"@echo off\r\necho CMD-SHIM-READY:%~1\r\n").expect("cmd shim");
    assert_eq!(
        resolve_executable(OsStr::new("tool"), std::slice::from_ref(&shim_dir)),
        Some(shim.clone())
    );
    let mut shim_process = ConPtyProcess::spawn(SpawnOptions::new(
        shim,
        [OsString::from("hello world")],
        temp.path().to_owned(),
        TerminalSize::new(80, 24),
    ))
    .expect("cmd shim launches through cmd.exe");
    shim_process
        .read_until(b"CMD-SHIM-READY:hello world", Duration::from_secs(3))
        .expect("cmd shim output");
}

#[test]
fn cmd_shim_arguments_cannot_expand_variables_or_inject_commands() {
    let _guard = test_guard();
    let temp = TempDir::new().expect("temporary cmd shim directory");
    let shim = temp.path().join("hostile.cmd");
    let marker = temp.path().join("injected.txt");
    fs::write(
        &shim,
        b"@echo off\r\necho ARG1:%~1\r\necho ARG2:%~2\r\necho SAFE\r\n",
    )
    .expect("cmd shim");
    let injection = OsString::from(format!(
        r#"\" & echo PWNED>\"{}\" & rem \""#,
        marker.display()
    ));
    let mut process = ConPtyProcess::spawn(SpawnOptions::new(
        shim,
        [OsString::from("%PATH%"), injection],
        temp.path().to_owned(),
        TerminalSize::new(80, 24),
    ))
    .expect("hostile cmd arguments launch safely");

    let output = process
        .read_until(b"SAFE", Duration::from_secs(3))
        .expect("cmd shim output");
    let output = String::from_utf8_lossy(&output);
    assert!(
        output.contains("ARG1:%PATH%"),
        "unexpected output: {output}"
    );
    assert!(!marker.exists(), "argument escaped cmd.exe quoting");
}

#[test]
fn ctrl_c_stops_a_cooperative_console_without_forcing_the_job() {
    let _guard = test_guard();
    let mut process = spawn_ansi(&[], TerminalSize::new(80, 24));
    process
        .read_until(b"READY", Duration::from_secs(3))
        .expect("fixture ready");

    assert_eq!(
        process.stop().expect("cooperative stop"),
        StopOutcome::Cooperative
    );
    assert_eq!(process.active_processes().expect("empty job"), 0);
}

#[test]
fn stop_releases_terminal_job_process_and_worker_handles_before_runtime_drop() {
    let _guard = test_guard();
    let mut warmup = spawn_ansi(&["--exit-immediately"], TerminalSize::new(40, 10));
    assert!(
        warmup
            .wait_for_exit(Duration::from_secs(2))
            .unwrap()
            .is_some()
    );
    drop(warmup);
    std::thread::sleep(Duration::from_millis(100));
    let before = current_process_handle_count();

    let mut process = spawn_ansi(&[], TerminalSize::new(80, 24));
    process
        .read_until(b"READY", Duration::from_secs(3))
        .expect("fixture ready");
    assert_eq!(
        process.stop().expect("cooperative stop"),
        StopOutcome::Cooperative
    );
    assert_eq!(process.active_processes().expect("closed job"), 0);
    assert!(process.wait_for_exit(Duration::ZERO).unwrap().is_some());
    assert!(process.write_input(b"after-stop").is_err());

    let deadline = Instant::now() + Duration::from_secs(2);
    while current_process_handle_count() > before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(current_process_handle_count(), before);
    drop(process);
}

#[test]
fn ignored_ctrl_c_falls_back_to_terminate_job_after_five_seconds() {
    let _guard = test_guard();
    let mut process = spawn_ansi(&["--ignore-ctrl-c"], TerminalSize::new(80, 24));
    process
        .read_until(b"READY", Duration::from_secs(3))
        .expect("fixture ready");
    let started = Instant::now();

    assert_eq!(process.stop().expect("forced stop"), StopOutcome::Forced);
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(process.active_processes().expect("empty job"), 0);
}

#[test]
fn stop_still_completes_when_the_cooperative_input_pipe_is_already_closed() {
    let _guard = test_guard();
    let mut process = ConPtyProcess::spawn(SpawnOptions::new(
        PathBuf::from(CHILD_TREE_FIXTURE),
        [OsString::from("parent"), OsString::from("0")],
        fixture_cwd(),
        TerminalSize::new(80, 24),
    ))
    .expect("child tree starts");
    process
        .read_until(b"LISTENING", Duration::from_secs(3))
        .expect("grandchild ready");
    process.close_input();

    assert_eq!(
        process.stop().expect("stop after closed input"),
        StopOutcome::Cooperative
    );
    assert_eq!(process.active_processes().expect("empty job"), 0);
}

#[test]
fn wait_for_exit_tracks_the_whole_job_after_the_root_process_exits() {
    let _guard = test_guard();
    let mut process = ConPtyProcess::spawn(SpawnOptions::new(
        PathBuf::from(CHILD_TREE_FIXTURE),
        [OsString::from("parent-exits"), OsString::from("0")],
        fixture_cwd(),
        TerminalSize::new(80, 24),
    ))
    .expect("detached child tree starts");
    process
        .read_until(b"LISTENING", Duration::from_secs(3))
        .expect("descendant remains alive after the root exits");

    assert_eq!(
        process
            .wait_for_exit(Duration::from_millis(100))
            .expect("job wait"),
        None,
        "root exit must not be reported as workload exit"
    );
    assert!(process.active_processes().expect("job population") >= 2);
}

#[test]
fn failed_spawns_release_every_partial_conpty_handle_without_hanging() {
    let _guard = test_guard();
    let mut warmup = spawn_ansi(&["--exit-immediately"], TerminalSize::new(40, 10));
    assert!(
        warmup
            .wait_for_exit(Duration::from_secs(2))
            .unwrap()
            .is_some()
    );
    drop(warmup);
    let _ = ConPtyProcess::spawn(SpawnOptions::new(
        fixture_cwd().join("missing-warmup.exe"),
        std::iter::empty(),
        fixture_cwd(),
        TerminalSize::new(40, 10),
    ));
    std::thread::sleep(Duration::from_millis(100));
    let before = current_process_handle_count();
    let started = Instant::now();

    for attempt in 0..25 {
        let error = ConPtyProcess::spawn(SpawnOptions::new(
            fixture_cwd().join(format!("missing-fixture-{attempt}.exe")),
            std::iter::empty(),
            fixture_cwd(),
            TerminalSize::new(40, 10),
        ))
        .expect_err("missing executable must fail");
        assert!(error.to_string().contains("CreateProcessW"));
    }

    assert!(started.elapsed() < Duration::from_secs(5));
    let settle_deadline = Instant::now() + Duration::from_secs(2);
    while current_process_handle_count() > before && Instant::now() < settle_deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(current_process_handle_count(), before);
}

#[test]
fn closing_the_job_kills_parent_child_and_grandchild_and_releases_the_port() {
    let _guard = test_guard();
    let options = SpawnOptions::new(
        PathBuf::from(CHILD_TREE_FIXTURE),
        [OsString::from("parent"), OsString::from("0")],
        fixture_cwd(),
        TerminalSize::new(80, 24),
    );
    let mut process = ConPtyProcess::spawn(options).expect("child tree starts");
    let output = process
        .read_until(b"LISTENING", Duration::from_secs(5))
        .expect("grandchild listener ready");
    let pids = child_tree_pids(&output);
    let port = listening_port(&output);
    assert!(process.active_processes().expect("job population") >= 3);

    drop(process);
    wait_for_processes_to_exit(&pids);

    wait_for_port_release(port);
}

#[test]
fn one_hundred_cycles_leave_no_process_or_handle_behind() {
    let _guard = test_guard();
    // ConHost initializes one process-wide synchronization handle on the first
    // ConPTY session. Warm it before measuring per-session ownership.
    let mut warmup = ConPtyProcess::spawn(SpawnOptions::new(
        PathBuf::from(CHILD_TREE_FIXTURE),
        [OsString::from("parent"), OsString::from("0")],
        fixture_cwd(),
        TerminalSize::new(40, 10),
    ))
    .expect("warmup tree starts");
    let warmup_output = warmup
        .read_until(b"LISTENING", Duration::from_secs(3))
        .expect("warmup listener");
    let warmup_pids = child_tree_pids(&warmup_output);
    let warmup_port = listening_port(&warmup_output);
    drop(warmup);
    wait_for_processes_to_exit(&warmup_pids);
    wait_for_port_release(warmup_port);
    std::thread::sleep(Duration::from_millis(100));
    let before = current_process_handle_count();

    for _ in 0..100 {
        let mut process = ConPtyProcess::spawn(SpawnOptions::new(
            PathBuf::from(CHILD_TREE_FIXTURE),
            [OsString::from("parent"), OsString::from("0")],
            fixture_cwd(),
            TerminalSize::new(40, 10),
        ))
        .expect("cycle tree starts");
        let output = process
            .read_until(b"LISTENING", Duration::from_secs(3))
            .expect("cycle grandchild ready");
        let pids = child_tree_pids(&output);
        let port = listening_port(&output);
        drop(process);
        wait_for_processes_to_exit(&pids);
        wait_for_port_release(port);
    }

    assert_eq!(current_process_handle_count(), before);
}

fn child_tree_pids(output: &[u8]) -> [u32; 3] {
    let output = String::from_utf8_lossy(output);
    [
        pid_after(&output, "PARENT:"),
        pid_after(&output, "CHILD:"),
        pid_after(&output, "GRANDCHILD:"),
    ]
}

fn listening_port(output: &[u8]) -> u16 {
    let output = String::from_utf8_lossy(output);
    let marker = "LISTENING:";
    let start = output.find(marker).expect("listener marker") + marker.len();
    output[start..]
        .split_once(':')
        .expect("listener PID separator")
        .1
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("listener port")
}

fn pid_after(output: &str, marker: &str) -> u32 {
    let start = output.find(marker).expect("PID marker") + marker.len();
    output[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("numeric PID")
}

fn wait_for_processes_to_exit(pids: &[u32]) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while pids.iter().any(|pid| process_is_active(*pid)) {
        assert!(
            Instant::now() < deadline,
            "process tree remains active: {pids:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn process_is_active(pid: u32) -> bool {
    // SAFETY: The returned handle is uniquely owned locally and closed below.
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return false;
    };
    let mut exit_code = 0_u32;
    // SAFETY: `handle` is live and `exit_code` is writable.
    let active = unsafe { GetExitCodeProcess(handle, &mut exit_code) }.is_ok()
        && exit_code == STILL_ACTIVE.0 as u32;
    // SAFETY: This function uniquely owns the process handle.
    let _ = unsafe { CloseHandle(handle) };
    active
}

fn wait_for_port_release(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => {
                drop(listener);
                return;
            }
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("port {port} remains owned: {error}"),
        }
    }
}

fn spawn_ansi(arguments: &[&str], size: TerminalSize) -> ConPtyProcess {
    ConPtyProcess::spawn(SpawnOptions::new(
        PathBuf::from(ANSI_FIXTURE),
        arguments.iter().map(OsString::from),
        fixture_cwd(),
        size,
    ))
    .expect("ANSI fixture starts")
}

fn fixture_cwd() -> PathBuf {
    Path::new(ANSI_FIXTURE)
        .parent()
        .expect("fixture parent")
        .to_owned()
}

fn current_process_handle_count() -> u32 {
    let mut count = 0;
    // SAFETY: The pseudo-handle is always valid and `count` is writable.
    unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) }.expect("handle count query");
    count
}

fn test_guard() -> MutexGuard<'static, ()> {
    WINDOWS_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
