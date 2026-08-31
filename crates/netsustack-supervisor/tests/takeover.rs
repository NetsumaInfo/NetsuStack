use std::{
    collections::VecDeque,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use netsustack_supervisor::takeover::{
    ConfiguredServerStarter, PortTakeoverBackend, TAKEOVER_POLL_ATTEMPTS, TAKEOVER_POLL_INTERVAL,
    TakeoverCoordinator, TakeoverError,
};
use netsustack_windows::{DockerContainer, DockerProcessProvenance, ProcessIdentity};

#[tokio::test(start_paused = true)]
async fn native_takeover_revalidates_terminates_waits_and_starts_server() {
    let expected = identity(101, 1, "node.exe", 5173);
    let backend = FakeBackend::new([Some(expected.clone()), None], None);
    let starter = FakeStarter::default();
    let start = tokio::time::Instant::now();

    TakeoverCoordinator::new(&backend)
        .take_over(&expected, &starter)
        .await
        .expect("native takeover succeeds");

    assert_eq!(backend.events(), ["terminate:101"]);
    assert_eq!(starter.starts(), 1);
    assert_eq!(backend.snapshot_calls(), 2);
    assert_eq!(tokio::time::Instant::now() - start, TAKEOVER_POLL_INTERVAL);
}

#[tokio::test(start_paused = true)]
async fn docker_takeover_stops_exact_container_instead_of_backend_process() {
    let expected = identity(202, 2, "com.docker.backend.exe", 5173);
    let container = DockerContainer {
        id: "container-a".into(),
        name: "web-1".into(),
        host_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        host_port: 5173,
        compose_project: Some("sample".into()),
        compose_service: Some("web".into()),
    };
    let backend = FakeBackend::new(
        [
            Some(expected.clone()),
            Some(expected.clone()),
            Some(expected.clone()),
            None,
        ],
        Some(container),
    );
    let starter = FakeStarter::default();

    TakeoverCoordinator::new(&backend)
        .take_over(&expected, &starter)
        .await
        .expect("Docker takeover succeeds");

    assert_eq!(
        backend.events(),
        [
            "reinspect-container:container-a:5173",
            "stop-container:container-a:5173"
        ]
    );
    assert_eq!(starter.starts(), 1);
}

#[tokio::test(start_paused = true)]
async fn docker_takeover_refuses_listener_changed_during_container_inspection() {
    let expected = identity(202, 2, "com.docker.backend.exe", 5173);
    let replacement = identity(203, 3, "com.docker.backend.exe", 5173);
    let container = DockerContainer {
        id: "replacement-container".into(),
        name: "replacement-web".into(),
        host_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        host_port: 5173,
        compose_project: Some("replacement".into()),
        compose_service: Some("web".into()),
    };
    let backend = FakeBackend::new(
        [Some(expected.clone()), Some(replacement.clone())],
        Some(container),
    );
    let starter = FakeStarter::default();

    let error = TakeoverCoordinator::new(&backend)
        .take_over(&expected, &starter)
        .await
        .expect_err("a replacement listener must not be stopped");

    assert!(matches!(
        error,
        TakeoverError::ListenerChanged { actual: Some(actual), .. } if *actual == replacement
    ));
    assert!(backend.events().is_empty());
    assert_eq!(starter.starts(), 0);
}

#[tokio::test(start_paused = true)]
async fn docker_takeover_rechecks_listener_after_slow_reinspection_before_stop() {
    let expected = identity(202, 2, "com.docker.backend.exe", 5173);
    let replacement = identity(203, 3, "com.docker.backend.exe", 5173);
    let container = DockerContainer {
        id: "container-a".into(),
        name: "web-1".into(),
        host_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        host_port: 5173,
        compose_project: Some("sample".into()),
        compose_service: Some("web".into()),
    };
    let backend = FakeBackend::new(
        [
            Some(expected.clone()),
            Some(expected.clone()),
            Some(replacement.clone()),
        ],
        Some(container),
    );
    let starter = FakeStarter::default();

    let error = TakeoverCoordinator::new(&backend)
        .take_over(&expected, &starter)
        .await
        .expect_err("listener ownership changed during Docker reinspection");

    assert!(matches!(
        error,
        TakeoverError::ListenerChanged { actual: Some(actual), .. } if *actual == replacement
    ));
    assert_eq!(backend.events(), ["reinspect-container:container-a:5173"]);
    assert_eq!(starter.starts(), 0);
}

#[tokio::test(start_paused = true)]
async fn native_takeover_ignores_an_unrelated_container_publishing_the_same_port() {
    let expected = identity(303, 3, "node.exe", 5173);
    let unrelated = DockerContainer {
        id: "unrelated-container".into(),
        name: "other-web".into(),
        host_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        host_port: 5173,
        compose_project: Some("other".into()),
        compose_service: Some("web".into()),
    };
    let backend = FakeBackend::new([Some(expected.clone()), None], Some(unrelated));
    let starter = FakeStarter::default();

    TakeoverCoordinator::new(&backend)
        .take_over(&expected, &starter)
        .await
        .expect("native takeover succeeds without touching Docker");

    assert_eq!(backend.events(), ["terminate:303"]);
    assert_eq!(starter.starts(), 1);
}

#[tokio::test(start_paused = true)]
async fn changed_listener_is_refused_before_any_action_is_sent() {
    let expected = identity(303, 3, "node.exe", 5173);
    let replacement = identity(304, 4, "bun.exe", 5173);
    let backend = FakeBackend::new([Some(replacement.clone())], None);
    let starter = FakeStarter::default();

    let error = TakeoverCoordinator::new(&backend)
        .take_over(&expected, &starter)
        .await
        .expect_err("changed listener must be refused");

    assert!(matches!(
        error,
        TakeoverError::ListenerChanged { actual: Some(actual), .. } if *actual == replacement
    ));
    assert!(backend.events().is_empty());
    assert_eq!(starter.starts(), 0);
}

#[tokio::test(start_paused = true)]
async fn protected_process_refusal_does_not_start_server() {
    let expected = identity(4, 5, "ntoskrnl.exe", 80);
    let backend = FakeBackend::new([Some(expected.clone())], None).refuse_termination();
    let starter = FakeStarter::default();

    let error = TakeoverCoordinator::new(&backend)
        .take_over(&expected, &starter)
        .await
        .expect_err("protected process must be refused");

    assert!(matches!(error, TakeoverError::ProtectedProcess { pid: 4 }));
    assert!(backend.events().is_empty());
    assert_eq!(starter.starts(), 0);
}

#[tokio::test(start_paused = true)]
async fn untrusted_docker_like_process_is_refused_without_any_action() {
    let expected = identity(606, 7, "com.docker.backend.exe", 5173);
    let backend = FakeBackend::new([Some(expected.clone())], None)
        .with_provenance(DockerProcessProvenance::DockerLikeUntrustedOrInconclusive);
    let starter = FakeStarter::default();

    let error = TakeoverCoordinator::new(&backend)
        .take_over(&expected, &starter)
        .await
        .expect_err("untrusted Docker-like provenance must fail closed");

    assert!(matches!(
        error,
        TakeoverError::ProtectedProcess { pid: 606 }
    ));
    assert!(backend.events().is_empty());
    assert_eq!(backend.snapshot_calls(), 1);
    assert_eq!(starter.starts(), 0);
}

#[tokio::test(start_paused = true)]
async fn occupied_port_is_polled_exactly_fifty_times_at_two_hundred_ms() {
    let expected = identity(505, 6, "node.exe", 5173);
    let backend = FakeBackend::new(std::iter::repeat_n(Some(expected.clone()), 51), None);
    let starter = FakeStarter::default();
    let start = tokio::time::Instant::now();

    let error = TakeoverCoordinator::new(&backend)
        .take_over(&expected, &starter)
        .await
        .expect_err("occupied port times out");

    assert!(matches!(
        error,
        TakeoverError::PortNotReleased { port: 5173 }
    ));
    assert_eq!(backend.snapshot_calls(), TAKEOVER_POLL_ATTEMPTS + 1);
    assert_eq!(
        tokio::time::Instant::now() - start,
        TAKEOVER_POLL_INTERVAL * TAKEOVER_POLL_ATTEMPTS as u32
    );
    assert_eq!(starter.starts(), 0);
}

fn identity(pid: u32, creation_time: u64, executable: &str, port: u16) -> ProcessIdentity {
    ProcessIdentity::new(
        pid,
        creation_time,
        PathBuf::from(executable),
        [IpAddr::V4(Ipv4Addr::LOCALHOST)],
        port,
    )
}

struct FakeBackend {
    snapshots: Mutex<VecDeque<Option<ProcessIdentity>>>,
    snapshot_calls: Mutex<usize>,
    container: Option<DockerContainer>,
    events: Mutex<Vec<String>>,
    refuse_termination: bool,
    provenance: Option<DockerProcessProvenance>,
}

impl FakeBackend {
    fn new(
        snapshots: impl IntoIterator<Item = Option<ProcessIdentity>>,
        container: Option<DockerContainer>,
    ) -> Self {
        Self {
            snapshots: Mutex::new(snapshots.into_iter().collect()),
            snapshot_calls: Mutex::new(0),
            container,
            events: Mutex::new(Vec::new()),
            refuse_termination: false,
            provenance: None,
        }
    }

    fn refuse_termination(mut self) -> Self {
        self.refuse_termination = true;
        self
    }

    fn with_provenance(mut self, provenance: DockerProcessProvenance) -> Self {
        self.provenance = Some(provenance);
        self
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().expect("events lock").clone()
    }

    fn snapshot_calls(&self) -> usize {
        *self.snapshot_calls.lock().expect("calls lock")
    }
}

#[async_trait]
impl PortTakeoverBackend for FakeBackend {
    fn docker_process_provenance(&self, expected: &ProcessIdentity) -> DockerProcessProvenance {
        self.provenance.unwrap_or_else(|| {
            if expected
                .executable
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("com.docker.backend.exe"))
            {
                DockerProcessProvenance::TrustedDocker
            } else {
                DockerProcessProvenance::DefinitelyNonDocker
            }
        })
    }

    async fn current_process(
        &self,
        _expected: &ProcessIdentity,
    ) -> Result<Option<ProcessIdentity>, TakeoverError> {
        *self.snapshot_calls.lock().expect("calls lock") += 1;
        let mut snapshots = self.snapshots.lock().expect("snapshots lock");
        Ok(snapshots.pop_front().flatten())
    }

    async fn container_for_port(
        &self,
        _expected: &ProcessIdentity,
    ) -> Result<Option<DockerContainer>, TakeoverError> {
        Ok(self.container.clone())
    }

    async fn reinspect_container(&self, container: &DockerContainer) -> Result<(), TakeoverError> {
        self.events.lock().expect("events lock").push(format!(
            "reinspect-container:{}:{}",
            container.id, container.host_port
        ));
        Ok(())
    }

    async fn terminate_process(&self, expected: &ProcessIdentity) -> Result<(), TakeoverError> {
        self.events
            .lock()
            .expect("events lock")
            .push(format!("terminate:{}", expected.pid));
        if self.refuse_termination {
            return Err(TakeoverError::ProtectedProcess { pid: expected.pid });
        }
        Ok(())
    }

    async fn stop_container(&self, container: &DockerContainer) -> Result<(), TakeoverError> {
        self.events.lock().expect("events lock").push(format!(
            "stop-container:{}:{}",
            container.id, container.host_port
        ));
        Ok(())
    }
}

#[derive(Default)]
struct FakeStarter(Arc<Mutex<usize>>);

impl FakeStarter {
    fn starts(&self) -> usize {
        *self.0.lock().expect("starts lock")
    }
}

#[async_trait]
impl ConfiguredServerStarter for FakeStarter {
    async fn start_configured_server(&self) -> Result<(), TakeoverError> {
        *self.0.lock().expect("starts lock") += 1;
        Ok(())
    }
}
