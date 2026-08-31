#![cfg(windows)]

use std::{
    ffi::OsString,
    fs,
    io::{BufRead, BufReader},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6, TcpListener},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use netsustack_windows::{
    DOCKER_OUTPUT_LIMIT, DockerCli, DockerContainer, DockerError, DockerProcessProvenance,
    ProcessIdentity, SnapshotError, TcpListenerEntry, deduplicate_tcp_listeners,
    docker_process_provenance, is_protected_process, list_tcp_listeners, parse_docker_inspect,
    select_docker_container, snapshot_process_for_port, terminate_process,
};

#[test]
fn deduplicates_ipv4_and_ipv6_rows_by_port_and_pid() {
    let rows = vec![
        TcpListenerEntry::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5173, 41),
        TcpListenerEntry::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 5173, 41),
        TcpListenerEntry::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5173, 42),
        TcpListenerEntry::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4173, 41),
        TcpListenerEntry::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5173, 41),
    ];

    let listeners = deduplicate_tcp_listeners(rows);

    assert_eq!(listeners.len(), 3);
    assert!(
        listeners
            .iter()
            .any(|row| row.port == 5173 && row.pid == 41)
    );
    assert!(
        listeners
            .iter()
            .any(|row| row.port == 5173 && row.pid == 42)
    );
    assert!(
        listeners
            .iter()
            .any(|row| row.port == 4173 && row.pid == 41)
    );
    let dual_bind = listeners
        .iter()
        .find(|row| row.port == 5173 && row.pid == 41)
        .expect("dual-bind owner");
    assert_eq!(
        dual_bind.local_addresses,
        [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        ]
    );
}

#[test]
fn get_extended_tcp_table_finds_ipv4_and_ipv6_listeners() {
    let ipv4 =
        TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).expect("bind IPv4 fixture");
    let ipv4_port = ipv4.local_addr().expect("IPv4 address").port();
    let current_pid = std::process::id();

    let listeners = list_tcp_listeners().expect("inspect Windows TCP tables");

    assert!(listeners.iter().any(|row| {
        row.port == ipv4_port
            && row.pid == current_pid
            && row
                .local_addresses
                .contains(&IpAddr::V4(Ipv4Addr::LOCALHOST))
    }));
    if let Ok(ipv6) = TcpListener::bind(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0)) {
        let ipv6_port = ipv6.local_addr().expect("IPv6 address").port();
        let listeners = list_tcp_listeners().expect("inspect Windows IPv6 TCP table");
        assert!(listeners.iter().any(|row| {
            row.port == ipv6_port
                && row.pid == current_pid
                && row
                    .local_addresses
                    .contains(&IpAddr::V6(Ipv6Addr::LOCALHOST))
        }));
    }
}

#[test]
fn process_identity_matches_pid_creation_time_executable_and_exact_port() {
    let base = ProcessIdentity::new(
        50,
        100,
        PathBuf::from(r"C:\tools\node.exe"),
        [IpAddr::V4(Ipv4Addr::LOCALHOST)],
        5173,
    );

    assert_eq!(base, base.clone());
    assert_ne!(
        base,
        ProcessIdentity::new(
            51,
            100,
            base.executable.clone(),
            base.local_addresses.clone(),
            5173
        )
    );
    assert_ne!(
        base,
        ProcessIdentity::new(
            50,
            101,
            base.executable.clone(),
            base.local_addresses.clone(),
            5173
        )
    );
    assert_ne!(
        base,
        ProcessIdentity::new(
            50,
            100,
            PathBuf::from(r"C:\tools\bun.exe"),
            base.local_addresses.clone(),
            5173,
        )
    );
    assert_ne!(
        base,
        ProcessIdentity::new(
            50,
            100,
            base.executable.clone(),
            base.local_addresses.clone(),
            5174
        )
    );
}

#[test]
fn process_snapshot_preserves_distinct_invalid_utf16_executable_paths() {
    let directory = tempfile::tempdir().expect("create invalid UTF-16 fixture directory");
    let fixture = PathBuf::from(env!("CARGO_BIN_EXE_port-reuse-fixture"));
    let first_path = directory.path().join(OsString::from_wide(&[
        0xd800,
        b'.' as u16,
        b'e' as u16,
        b'x' as u16,
        b'e' as u16,
    ]));
    let second_path = directory.path().join(OsString::from_wide(&[
        0xd801,
        b'.' as u16,
        b'e' as u16,
        b'x' as u16,
        b'e' as u16,
    ]));
    fs::copy(&fixture, &first_path).expect("copy first invalid UTF-16 executable");
    fs::copy(&fixture, &second_path).expect("copy second invalid UTF-16 executable");

    let mut first = PortFixture::spawn_with_executable(&first_path, None);
    let first_identity =
        snapshot_process_for_port(first.port, first.child.id()).expect("first process snapshot");
    first.stop();

    let mut second = PortFixture::spawn_with_executable(&second_path, None);
    let second_identity =
        snapshot_process_for_port(second.port, second.child.id()).expect("second process snapshot");
    second.stop();

    let first_wide: Vec<_> = first_identity
        .executable
        .as_os_str()
        .encode_wide()
        .collect();
    let second_wide: Vec<_> = second_identity
        .executable
        .as_os_str()
        .encode_wide()
        .collect();
    assert!(first_wide.contains(&0xd800));
    assert!(second_wide.contains(&0xd801));
    assert_ne!(first_identity.executable, second_identity.executable);
}

#[test]
fn protected_system_processes_are_never_terminable() {
    let identity = ProcessIdentity::new(
        4,
        1,
        PathBuf::from(r"C:\Windows\System32\ntoskrnl.exe"),
        [IpAddr::V4(Ipv4Addr::UNSPECIFIED)],
        80,
    );
    assert!(is_protected_process(&identity));
    assert!(matches!(
        terminate_process(&identity),
        Err(SnapshotError::ProtectedProcess { .. })
    ));
}

#[test]
fn docker_named_executable_outside_the_trusted_installation_is_not_docker_provenance() {
    let directory = tempfile::tempdir().expect("create spoof directory");
    for relative in [
        PathBuf::from("resources/com.docker.backend.exe"),
        PathBuf::from("resources/vpnkit.exe"),
        PathBuf::from("resources/bin/docker-proxy.exe"),
    ] {
        let path = directory.path().join(relative);
        fs::create_dir_all(path.parent().expect("spoof parent"))
            .expect("create spoof directory layout");
        fs::write(&path, b"not Docker").expect("create spoof executable");
        let identity = ProcessIdentity::new(500, 2, path, [IpAddr::V4(Ipv4Addr::LOCALHOST)], 5173);

        assert_eq!(
            docker_process_provenance(&identity),
            DockerProcessProvenance::DockerLikeUntrustedOrInconclusive
        );
        assert!(is_protected_process(&identity));
    }
}

#[test]
fn docker_named_executable_in_a_custom_location_is_protected_as_inconclusive() {
    let directory = tempfile::tempdir().expect("create custom directory");
    let executable = directory.path().join("com.docker.backend.exe");
    fs::write(&executable, b"custom Docker-compatible backend").expect("create executable");
    let identity =
        ProcessIdentity::new(500, 2, executable, [IpAddr::V4(Ipv4Addr::LOCALHOST)], 5173);

    assert_eq!(
        docker_process_provenance(&identity),
        DockerProcessProvenance::DockerLikeUntrustedOrInconclusive
    );
    assert!(is_protected_process(&identity));
}

#[test]
fn docker_inspect_requires_exact_host_port_and_keeps_compose_labels() {
    let inspect = r#"[
      {
        "Id": "container-a",
        "Name": "/web-1",
        "Config": {"Labels": {
          "com.docker.compose.project": "sample",
          "com.docker.compose.service": "web"
        }},
        "NetworkSettings": {"Ports": {
          "3000/tcp": [{"HostIp": "0.0.0.0", "HostPort": "15173"}],
          "5173/tcp": [{"HostIp": "127.0.0.1", "HostPort": "5173"}]
        }}
      },
      {
        "Id": "container-b",
        "Name": "/api-1",
        "Config": {"Labels": {}},
        "NetworkSettings": {"Ports": {
          "5173/tcp": [{"HostIp": "0.0.0.0", "HostPort": "51730"}]
        }}
      }
    ]"#;

    let containers = parse_docker_inspect(inspect, 5173).expect("parse Docker inspect fixture");

    assert_eq!(containers.len(), 1);
    assert_eq!(containers[0].id, "container-a");
    assert_eq!(containers[0].name, "web-1");
    assert_eq!(containers[0].host_ip, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(containers[0].host_port, 5173);
    assert_eq!(containers[0].compose_project.as_deref(), Some("sample"));
    assert_eq!(containers[0].compose_service.as_deref(), Some("web"));
}

#[test]
fn docker_inspect_accepts_containers_without_labels() {
    let inspect = r#"[{
      "Id": "container-without-labels",
      "Name": "/plain-container",
      "Config": {"Labels": null},
      "NetworkSettings": {"Ports": {
        "8080/tcp": [{"HostIp": "127.0.0.1", "HostPort": "8080"}]
      }}
    }]"#;

    let containers =
        parse_docker_inspect(inspect, 8080).expect("Docker containers may have a null Labels map");

    assert_eq!(containers.len(), 1);
    assert_eq!(containers[0].id, "container-without-labels");
    assert_eq!(containers[0].compose_project, None);
    assert_eq!(containers[0].compose_service, None);
}

#[test]
fn docker_selection_does_not_match_the_same_port_on_a_different_host_ip() {
    let inspect = r#"[{
      "Id": "different-address",
      "Name": "/other-web",
      "Config": {"Labels": {}},
      "NetworkSettings": {"Ports": {
        "5173/tcp": [{"HostIp": "127.0.0.2", "HostPort": "5173"}]
      }}
    }]"#;

    let selected = select_docker_container(inspect, &[IpAddr::V4(Ipv4Addr::LOCALHOST)], 5173)
        .expect("select exact Docker binding");

    assert_eq!(selected, None);
}

#[test]
fn docker_wildcard_binding_matches_a_specific_listener_of_the_same_family() {
    let inspect = r#"[{
      "Id": "wildcard-container",
      "Name": "/web",
      "Config": {"Labels": {}},
      "NetworkSettings": {"Ports": {
        "5173/tcp": [{"HostIp": "0.0.0.0", "HostPort": "5173"}]
      }}
    }]"#;

    let selected = select_docker_container(inspect, &[IpAddr::V4(Ipv4Addr::LOCALHOST)], 5173)
        .expect("select wildcard Docker binding");

    assert_eq!(
        selected.map(|container| container.id),
        Some("wildcard-container".into())
    );
}

#[test]
fn docker_specific_binding_matches_a_wildcard_listener_of_the_same_family() {
    let inspect = r#"[{
      "Id": "specific-container",
      "Name": "/web",
      "Config": {"Labels": {}},
      "NetworkSettings": {"Ports": {
        "5173/tcp": [{"HostIp": "127.0.0.1", "HostPort": "5173"}]
      }}
    }]"#;

    let selected = select_docker_container(inspect, &[IpAddr::V4(Ipv4Addr::UNSPECIFIED)], 5173)
        .expect("select specific Docker binding through wildcard listener");

    assert_eq!(
        selected.map(|container| container.id),
        Some("specific-container".into())
    );
}

#[test]
fn docker_selection_rejects_multiple_exact_container_bindings() {
    let inspect = r#"[
      {
        "Id": "container-a",
        "Name": "/web-a",
        "Config": {"Labels": {}},
        "NetworkSettings": {"Ports": {
          "5173/tcp": [{"HostIp": "127.0.0.1", "HostPort": "5173"}]
        }}
      },
      {
        "Id": "container-b",
        "Name": "/web-b",
        "Config": {"Labels": {}},
        "NetworkSettings": {"Ports": {
          "5173/tcp": [{"HostIp": "127.0.0.1", "HostPort": "5173"}]
        }}
      }
    ]"#;

    let error = select_docker_container(inspect, &[IpAddr::V4(Ipv4Addr::LOCALHOST)], 5173)
        .expect_err("ambiguous Docker binding must be refused");

    assert!(matches!(
        error,
        DockerError::AmbiguousBinding {
            local_addresses,
            host_port: 5173,
            count: 2,
        } if local_addresses == [IpAddr::V4(Ipv4Addr::LOCALHOST)]
    ));
}

#[test]
fn docker_stop_reinspection_terminates_options_before_option_looking_id() {
    let docker = DockerCli::new(PathBuf::from(env!("CARGO_BIN_EXE_docker-cli-fixture")));
    let expected = DockerContainer {
        id: "--malicious-container".into(),
        name: "web-1".into(),
        host_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        host_port: 5173,
        compose_project: Some("sample".into()),
        compose_service: Some("web".into()),
    };

    docker
        .reinspect_container(&expected)
        .expect("safe Docker reinspection");
    docker.stop_container(&expected).expect("safe Docker stop");
}

#[test]
fn dedicated_docker_pipe_probe_has_exactly_stable_threads_and_handles() {
    let mut probe = Command::new(env!("CARGO_BIN_EXE_docker-pipe-probe"))
        .arg("--require-job")
        .arg(env!("CARGO_BIN_EXE_docker-cli-fixture"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn isolated Docker pipe probe");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if probe.try_wait().expect("query Docker pipe probe").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            probe.kill().expect("kill timed-out Docker pipe probe");
            let _ = probe.wait();
            panic!("Docker pipe probe exceeded its 15-second bound");
        }
        thread::sleep(Duration::from_millis(25));
    }
    let output = probe
        .wait_with_output()
        .expect("collect Docker pipe probe output");
    assert!(
        output.status.success(),
        "Docker pipe probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn docker_pipe_probe_rejects_a_malformed_descendant_pid() {
    let directory = tempfile::tempdir().expect("create probe directory");
    let pid_file = directory.path().join("descendants.pids");
    fs::write(&pid_file, "not-a-pid\n").expect("write malformed PID file");

    let output = run_reap_only_probe(&pid_file);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("invalid fixture descendant PID on line 1: not-a-pid"),
        "unexpected probe error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn docker_pipe_probe_rejects_a_descendant_pid_that_cannot_be_opened() {
    let directory = tempfile::tempdir().expect("create probe directory");
    let pid_file = directory.path().join("descendants.pids");
    fs::write(&pid_file, format!("{}\n", u32::MAX)).expect("write stale PID file");

    let output = run_reap_only_probe(&pid_file);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("OpenProcess failed for fixture descendant 4294967295"),
        "unexpected probe error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn docker_pipe_probe_never_terminates_an_uncontained_live_process() {
    let mut outsider = Command::new(env!("CARGO_BIN_EXE_docker-cli-fixture"))
        .arg("pipe-holder")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn process outside the probe Job Object");
    let directory = tempfile::tempdir().expect("create probe directory");
    let pid_file = directory.path().join("descendants.pids");
    fs::write(&pid_file, format!("{}\n", outsider.id())).expect("write outsider PID file");

    let output = run_reap_only_probe(&pid_file);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("escaped the probe Job Object"),
        "unexpected probe error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        outsider
            .try_wait()
            .expect("query uncontained process")
            .is_none(),
        "probe terminated a process before proving Job Object membership"
    );
    outsider.kill().expect("terminate uncontained fixture");
    outsider.wait().expect("reap uncontained fixture");
}

fn run_reap_only_probe(pid_file: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_docker-pipe-probe"))
        .arg("--reap-only")
        .arg(pid_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run isolated Docker reap probe")
}

#[test]
fn noisy_docker_output_hits_the_total_limit_with_a_typed_error() {
    let docker = DockerCli::new(PathBuf::from(env!("CARGO_BIN_EXE_docker-cli-fixture")));
    let started = Instant::now();

    let error = docker
        .container_for_listener(&[IpAddr::V4(Ipv4Addr::LOCALHOST)], 65532)
        .expect_err("noisy Docker output must be bounded");

    assert!(matches!(
        error,
        DockerError::OutputLimitExceeded { limit } if limit == DOCKER_OUTPUT_LIMIT
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn reused_port_refuses_the_stale_snapshot_without_terminating_new_owner() {
    let mut first = PortFixture::spawn(None);
    let original = snapshot_process_for_port(first.port, first.child.id()).expect("first snapshot");
    first.stop();

    let mut replacement = PortFixture::spawn(Some(first.port));
    let error = terminate_process(&original).expect_err("stale snapshot must be refused");

    assert!(
        matches!(error, SnapshotError::ListenerChanged { .. }),
        "unexpected stale-snapshot error: {error:?}"
    );
    assert!(
        replacement
            .child
            .try_wait()
            .expect("query replacement")
            .is_none()
    );
    replacement.stop();
}

#[test]
fn exact_native_termination_releases_only_the_target_listener() {
    let mut target = PortFixture::spawn(None);
    let mut unrelated = PortFixture::spawn(None);
    let expected =
        snapshot_process_for_port(target.port, target.child.id()).expect("target snapshot");

    terminate_process(&expected).expect("terminate exact native listener owner");
    let deadline = Instant::now() + Duration::from_secs(2);
    while target.child.try_wait().expect("query target").is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }

    assert!(target.child.try_wait().expect("query target").is_some());
    assert!(
        unrelated
            .child
            .try_wait()
            .expect("query unrelated")
            .is_none()
    );
    assert!(
        !list_tcp_listeners()
            .expect("inspect listeners after termination")
            .iter()
            .any(|listener| listener.port == target.port && listener.pid == expected.pid)
    );
    unrelated.stop();
}

struct PortFixture {
    child: Child,
    port: u16,
}

impl PortFixture {
    fn spawn(port: Option<u16>) -> Self {
        Self::spawn_with_executable(env!("CARGO_BIN_EXE_port-reuse-fixture"), port)
    }

    fn spawn_with_executable(executable: impl AsRef<std::ffi::OsStr>, port: Option<u16>) -> Self {
        let mut command = Command::new(executable);
        if let Some(port) = port {
            command.arg(port.to_string());
        }
        let mut child = command
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn port reuse fixture");
        let stdout = child.stdout.take().expect("fixture stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read fixture port");
        let port = line
            .trim()
            .strip_prefix("READY ")
            .expect("fixture ready line")
            .parse()
            .expect("fixture port number");
        Self { child, port }
    }

    fn stop(&mut self) {
        if self.child.try_wait().expect("query fixture").is_none() {
            self.child.kill().expect("kill fixture");
        }
        self.child.wait().expect("wait fixture");
    }
}

impl Drop for PortFixture {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
