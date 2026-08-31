use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use netsustack_windows::{
    DockerCli, DockerContainer, DockerProcessProvenance, ProcessIdentity, SnapshotError,
    TcpListenerEntry, docker_process_provenance, is_protected_process, list_tcp_listeners,
    snapshot_process_for_listener, terminate_process,
};
use thiserror::Error;

use crate::runtime::ServerRuntime;

pub const TAKEOVER_POLL_ATTEMPTS: usize = 50;
pub const TAKEOVER_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Error)]
pub enum TakeoverError {
    #[error("TCP port {port} is no longer occupied")]
    PortNotOccupied { port: u16 },
    #[error("the listener on TCP port {port} changed after it was inspected")]
    ListenerChanged {
        port: u16,
        expected: Box<ProcessIdentity>,
        actual: Option<Box<ProcessIdentity>>,
    },
    #[error("PID {pid} is a protected process and cannot be terminated")]
    ProtectedProcess { pid: u32 },
    #[error("TCP port {port} was not released within 10 seconds")]
    PortNotReleased { port: u16 },
    #[error("takeover backend failed: {0}")]
    Backend(String),
    #[error("configured server failed to start: {0}")]
    Start(String),
}

#[async_trait]
pub trait PortTakeoverBackend: Send + Sync {
    fn docker_process_provenance(&self, expected: &ProcessIdentity) -> DockerProcessProvenance {
        docker_process_provenance(expected)
    }

    async fn current_process(
        &self,
        expected: &ProcessIdentity,
    ) -> Result<Option<ProcessIdentity>, TakeoverError>;
    async fn container_for_port(
        &self,
        expected: &ProcessIdentity,
    ) -> Result<Option<DockerContainer>, TakeoverError>;
    async fn reinspect_container(&self, container: &DockerContainer) -> Result<(), TakeoverError>;
    async fn terminate_process(&self, expected: &ProcessIdentity) -> Result<(), TakeoverError>;
    async fn stop_container(&self, container: &DockerContainer) -> Result<(), TakeoverError>;
}

#[async_trait]
pub trait ConfiguredServerStarter: Send + Sync {
    async fn start_configured_server(&self) -> Result<(), TakeoverError>;
}

#[async_trait]
impl ConfiguredServerStarter for ServerRuntime {
    async fn start_configured_server(&self) -> Result<(), TakeoverError> {
        self.start()
            .await
            .map_err(|error| TakeoverError::Start(error.to_string()))
    }
}

pub struct TakeoverCoordinator<'a, B> {
    backend: &'a B,
}

impl<'a, B> TakeoverCoordinator<'a, B>
where
    B: PortTakeoverBackend,
{
    pub const fn new(backend: &'a B) -> Self {
        Self { backend }
    }

    pub async fn take_over<S>(
        &self,
        expected: &ProcessIdentity,
        starter: &S,
    ) -> Result<(), TakeoverError>
    where
        S: ConfiguredServerStarter,
    {
        let actual = self.backend.current_process(expected).await?;
        if actual.as_ref() != Some(expected) {
            return Err(TakeoverError::ListenerChanged {
                port: expected.port,
                expected: Box::new(expected.clone()),
                actual: actual.map(Box::new),
            });
        }

        let container = match self.backend.docker_process_provenance(expected) {
            DockerProcessProvenance::TrustedDocker => {
                self.backend.container_for_port(expected).await?
            }
            DockerProcessProvenance::DockerLikeUntrustedOrInconclusive => {
                return Err(TakeoverError::ProtectedProcess { pid: expected.pid });
            }
            DockerProcessProvenance::DefinitelyNonDocker => None,
        };
        if let Some(container) = container {
            let actual = self.backend.current_process(expected).await?;
            if actual.as_ref() != Some(expected) {
                return Err(TakeoverError::ListenerChanged {
                    port: expected.port,
                    expected: Box::new(expected.clone()),
                    actual: actual.map(Box::new),
                });
            }
            self.backend.reinspect_container(&container).await?;
            let actual = self.backend.current_process(expected).await?;
            if actual.as_ref() != Some(expected) {
                return Err(TakeoverError::ListenerChanged {
                    port: expected.port,
                    expected: Box::new(expected.clone()),
                    actual: actual.map(Box::new),
                });
            }
            self.backend.stop_container(&container).await?;
        } else {
            if is_protected_process(expected) {
                return Err(TakeoverError::ProtectedProcess { pid: expected.pid });
            }
            self.backend.terminate_process(expected).await?;
        }

        for _ in 0..TAKEOVER_POLL_ATTEMPTS {
            tokio::time::sleep(TAKEOVER_POLL_INTERVAL).await;
            match self.backend.current_process(expected).await? {
                None => {
                    starter.start_configured_server().await?;
                    return Ok(());
                }
                Some(current) if current == *expected => {}
                actual => {
                    return Err(TakeoverError::ListenerChanged {
                        port: expected.port,
                        expected: Box::new(expected.clone()),
                        actual: actual.map(Box::new),
                    });
                }
            }
        }
        Err(TakeoverError::PortNotReleased {
            port: expected.port,
        })
    }
}

#[derive(Debug, Clone)]
pub struct WindowsTakeoverBackend {
    docker: Option<Arc<DockerCli>>,
}

impl Default for WindowsTakeoverBackend {
    fn default() -> Self {
        Self {
            docker: DockerCli::discover().ok().map(Arc::new),
        }
    }
}

impl WindowsTakeoverBackend {
    pub fn new(docker: Option<DockerCli>) -> Self {
        Self {
            docker: docker.map(Arc::new),
        }
    }
}

#[async_trait]
impl PortTakeoverBackend for WindowsTakeoverBackend {
    async fn current_process(
        &self,
        expected: &ProcessIdentity,
    ) -> Result<Option<ProcessIdentity>, TakeoverError> {
        let expected = expected.clone();
        tokio::task::spawn_blocking(move || {
            let listeners = list_tcp_listeners().map_err(backend_error)?;
            resolve_current_process(&expected, listeners, snapshot_process_for_listener)
        })
        .await
        .map_err(|error| TakeoverError::Backend(format!("port snapshot task failed: {error}")))?
    }

    async fn container_for_port(
        &self,
        expected: &ProcessIdentity,
    ) -> Result<Option<DockerContainer>, TakeoverError> {
        let Some(docker) = self.docker.clone() else {
            return Ok(None);
        };
        let local_addresses = expected.local_addresses.clone();
        let port = expected.port;
        tokio::task::spawn_blocking(move || {
            docker
                .container_for_listener(&local_addresses, port)
                .map_err(backend_error)
        })
        .await
        .map_err(|error| {
            TakeoverError::Backend(format!("Docker inspection task failed: {error}"))
        })?
    }

    async fn reinspect_container(&self, container: &DockerContainer) -> Result<(), TakeoverError> {
        let docker = self.docker.clone().ok_or_else(|| {
            TakeoverError::Backend("Docker CLI disappeared after inspection".into())
        })?;
        let container = container.clone();
        tokio::task::spawn_blocking(move || {
            docker
                .reinspect_container(&container)
                .map_err(backend_error)
        })
        .await
        .map_err(|error| {
            TakeoverError::Backend(format!("Docker reinspection task failed: {error}"))
        })?
    }

    async fn terminate_process(&self, expected: &ProcessIdentity) -> Result<(), TakeoverError> {
        let expected = expected.clone();
        tokio::task::spawn_blocking(move || terminate_process(&expected).map_err(snapshot_error))
            .await
            .map_err(|error| {
                TakeoverError::Backend(format!("process termination task failed: {error}"))
            })?
    }

    async fn stop_container(&self, container: &DockerContainer) -> Result<(), TakeoverError> {
        let docker = self.docker.clone().ok_or_else(|| {
            TakeoverError::Backend("Docker CLI disappeared after inspection".into())
        })?;
        let container = container.clone();
        tokio::task::spawn_blocking(move || {
            docker.stop_container(&container).map_err(backend_error)
        })
        .await
        .map_err(|error| TakeoverError::Backend(format!("Docker stop task failed: {error}")))?
    }
}

fn snapshot_error(error: SnapshotError) -> TakeoverError {
    match error {
        SnapshotError::ProtectedProcess { pid } => TakeoverError::ProtectedProcess { pid },
        SnapshotError::ListenerChanged {
            port,
            expected,
            actual,
        } => TakeoverError::ListenerChanged {
            port,
            expected,
            actual,
        },
        error => backend_error(error),
    }
}

fn backend_error(error: impl std::fmt::Display) -> TakeoverError {
    TakeoverError::Backend(error.to_string())
}

fn resolve_current_process(
    expected: &ProcessIdentity,
    listeners: impl IntoIterator<Item = TcpListenerEntry>,
    mut snapshot: impl FnMut(TcpListenerEntry) -> Result<ProcessIdentity, SnapshotError>,
) -> Result<Option<ProcessIdentity>, TakeoverError> {
    let mut candidates: Vec<_> = listeners
        .into_iter()
        .filter(|listener| listener.port == expected.port)
        .collect();
    candidates.sort_by_key(|listener| {
        (
            listener.pid != expected.pid,
            listener.local_addresses != expected.local_addresses,
        )
    });
    for listener in candidates {
        match snapshot(listener) {
            Ok(identity) => return Ok(Some(identity)),
            Err(SnapshotError::ListenerNotFound { .. }) => {}
            Err(error) => return Err(snapshot_error(error)),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        path::PathBuf,
    };

    use netsustack_windows::TcpListenerEntry;

    use super::*;

    #[test]
    fn current_process_prefers_expected_pid_among_shared_port_candidates() {
        let expected = identity(900, 10, "com.docker.backend.exe");
        let other = identity(100, 11, "other.exe");
        let listeners = [listener(other.pid), listener(expected.pid)];
        let mut calls = Vec::new();

        let actual = resolve_current_process(&expected, listeners, |listener| {
            calls.push(listener.pid);
            Ok(if listener.pid == expected.pid {
                expected.clone()
            } else {
                other.clone()
            })
        })
        .expect("resolve shared port owner");

        assert_eq!(actual, Some(expected.clone()));
        assert_eq!(calls, [expected.pid]);
    }

    #[test]
    fn disappearing_listener_between_enumeration_and_snapshot_is_released() {
        let expected = identity(900, 10, "com.docker.backend.exe");

        let actual = resolve_current_process(&expected, [listener(expected.pid)], |listener| {
            Err(SnapshotError::ListenerNotFound {
                pid: listener.pid,
                port: listener.port,
            })
        })
        .expect("disappearing listener is not a backend failure");

        assert_eq!(actual, None);
    }

    fn identity(pid: u32, creation_time: u64, executable: &str) -> ProcessIdentity {
        ProcessIdentity::new(
            pid,
            creation_time,
            PathBuf::from(executable),
            [IpAddr::V4(Ipv4Addr::LOCALHOST)],
            5173,
        )
    }

    fn listener(pid: u32) -> TcpListenerEntry {
        TcpListenerEntry::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5173, pid)
    }
}
