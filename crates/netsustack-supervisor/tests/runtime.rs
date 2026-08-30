use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use netsustack_domain::{PreferredShell, ServerConfig, ServerState};
use netsustack_supervisor::backoff::RestartBackoff;
use netsustack_supervisor::health::{HealthChecker, resolved_health_url};
use netsustack_supervisor::runtime::{
    ManagedProcess, ProcessBackend, ProcessStopResult, RuntimeError, RuntimeSettings, RuntimeSpec,
    ServerRuntime, SpawnRequest, WindowsProcessBackend,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard, Notify},
};

#[test]
fn crash_backoff_is_exponential_and_capped_at_thirty_seconds() {
    let delays = (0..7)
        .map(|attempt| RestartBackoff::delay(attempt).as_secs())
        .collect::<Vec<_>>();

    assert_eq!(delays, vec![1, 2, 4, 8, 16, 30, 30]);
    assert_eq!(RestartBackoff::HEALTHY_RESET_AFTER, Duration::from_secs(30));
}

fn server_config(port: Option<u16>, health_url: Option<&str>, status: Option<u16>) -> ServerConfig {
    ServerConfig {
        id: "srv_health".into(),
        name: "health".into(),
        command: "fixture".into(),
        port,
        directory: None,
        env: HashMap::new(),
        health_url: health_url.map(str::to_owned),
        health_status: status,
        auto_restart: true,
        actions: Vec::new(),
    }
}

async fn http_server(status: u16, delay: Duration) -> SocketAddr {
    http_server_with_hits(status, delay).await.0
}

async fn http_server_with_hits(status: u16, delay: Duration) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let server_hits = hits.clone();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        server_hits.fetch_add(1, Ordering::SeqCst);
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await;
        tokio::time::sleep(delay).await;
        let response = format!("HTTP/1.1 {status} test\r\nContent-Length: 0\r\n\r\n");
        let _ = stream.write_all(response.as_bytes()).await;
    });
    (address, hits)
}

async fn redirect_server() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await;
        let _ = stream
            .write_all(
                b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/unreachable\r\nContent-Length: 0\r\n\r\n",
            )
            .await;
    });
    address
}

#[tokio::test]
async fn port_health_accepts_ipv4_and_ipv6_localhost_listeners() {
    let checker = HealthChecker::default();
    let ipv4 = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let ipv4_config = server_config(Some(ipv4.local_addr().unwrap().port()), None, None);
    assert!(checker.check(&ipv4_config).await);

    if let Ok(ipv6) = TcpListener::bind(("::1", 0)).await {
        let ipv6_config = server_config(Some(ipv6.local_addr().unwrap().port()), None, None);
        assert!(checker.check(&ipv6_config).await);
    }
}

#[tokio::test]
async fn http_health_accepts_default_range_and_requires_configured_status() {
    for status in [200, 204, 302, 399] {
        let address = http_server(status, Duration::ZERO).await;
        let config = server_config(None, Some(&format!("http://{address}/health")), None);
        assert!(
            HealthChecker::default().check(&config).await,
            "status {status}"
        );
    }

    let address = http_server(418, Duration::ZERO).await;
    let exact = server_config(None, Some(&format!("http://{address}/health")), Some(418));
    assert!(HealthChecker::default().check(&exact).await);

    let address = http_server(204, Duration::ZERO).await;
    let mismatch = server_config(None, Some(&format!("http://{address}/health")), Some(200));
    assert!(!HealthChecker::default().check(&mismatch).await);

    let address = http_server(400, Duration::ZERO).await;
    let outside_default_range =
        server_config(None, Some(&format!("http://{address}/health")), None);
    assert!(!HealthChecker::default().check(&outside_default_range).await);

    let address = redirect_server().await;
    let redirect = server_config(None, Some(&format!("http://{address}/health")), None);
    assert!(HealthChecker::default().check(&redirect).await);
}

#[tokio::test]
async fn http_health_honors_timeout_and_relative_urls_use_the_server_port() {
    let address = http_server(200, Duration::from_millis(100)).await;
    let config = server_config(None, Some(&format!("http://{address}/health")), None);
    let checker = HealthChecker::new(Duration::from_secs(2), Duration::from_millis(20));
    assert!(!checker.check(&config).await);

    let relative = server_config(Some(4321), Some("ready"), None);
    assert_eq!(
        resolved_health_url(&relative)
            .unwrap()
            .as_ref()
            .map(reqwest::Url::as_str),
        Some("http://localhost:4321/ready")
    );

    let closed = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let closed_port = server_config(Some(port), None, None);
    let checker = HealthChecker::new(Duration::from_millis(20), Duration::from_secs(5));
    assert!(!checker.check(&closed_port).await);
}

#[tokio::test]
async fn configured_http_health_is_parsed_case_insensitively_and_routes_only_to_http() {
    let (address, uppercase_hits) = http_server_with_hits(204, Duration::ZERO).await;
    let uppercase = server_config(None, Some(&format!("HTTP://{address}/health")), None);
    assert!(HealthChecker::default().check(&uppercase).await);
    assert_eq!(uppercase_hits.load(Ordering::SeqCst), 1);

    let closed = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);
    let (address, absolute_hits) = http_server_with_hits(204, Duration::ZERO).await;
    let absolute = server_config(
        Some(closed_port),
        Some(&format!("http://{address}/health")),
        None,
    );
    assert!(HealthChecker::default().check(&absolute).await);
    assert_eq!(absolute_hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn malformed_or_unresolvable_configured_health_urls_fail_closed() {
    let malformed = server_config(None, Some("http://["), None);
    assert!(!HealthChecker::default().check(&malformed).await);

    let relative_without_port = server_config(None, Some("health"), None);
    assert!(!HealthChecker::default().check(&relative_without_port).await);
}

#[derive(Debug, Default)]
struct FakeState {
    active: usize,
    max_active: usize,
    spawn_count: usize,
    next_pid: u32,
    fail_next_spawn: bool,
    health: VecDeque<bool>,
    exits: Vec<Arc<Mutex<Option<i32>>>>,
    input_count: usize,
    resize_count: usize,
    health_checks: usize,
    fail_next_stop: bool,
    fail_next_stop_output: bool,
    fail_next_wait: bool,
    block_health: bool,
}

#[derive(Debug, Clone, Default)]
struct FakeBackend(Arc<Mutex<FakeState>>, Arc<Notify>);

impl FakeBackend {
    fn queue_health(&self, values: impl IntoIterator<Item = bool>) {
        self.0.lock().unwrap().health.extend(values);
    }

    fn crash_latest(&self, code: i32) {
        let exit = self.0.lock().unwrap().exits.last().unwrap().clone();
        *exit.lock().unwrap() = Some(code);
    }

    fn fail_next_spawn(&self) {
        self.0.lock().unwrap().fail_next_spawn = true;
    }

    fn fail_next_stop(&self) {
        self.0.lock().unwrap().fail_next_stop = true;
    }

    fn fail_next_stop_output(&self) {
        self.0.lock().unwrap().fail_next_stop_output = true;
    }

    fn fail_next_wait(&self) {
        self.0.lock().unwrap().fail_next_wait = true;
    }

    fn snapshot(&self) -> (usize, usize, usize, usize, usize) {
        let state = self.0.lock().unwrap();
        (
            state.active,
            state.max_active,
            state.spawn_count,
            state.input_count,
            state.resize_count,
        )
    }

    fn health_checks(&self) -> usize {
        self.0.lock().unwrap().health_checks
    }

    fn block_health(&self) {
        self.0.lock().unwrap().block_health = true;
    }

    fn release_health(&self) {
        self.1.notify_one();
    }
}

struct FakeProcess {
    backend: Arc<Mutex<FakeState>>,
    pid: u32,
    exit: Arc<Mutex<Option<i32>>>,
    active: bool,
}

impl FakeProcess {
    fn mark_inactive(&mut self) {
        if self.active {
            self.backend.lock().unwrap().active -= 1;
            self.active = false;
        }
    }
}

impl Drop for FakeProcess {
    fn drop(&mut self) {
        self.mark_inactive();
    }
}

impl ManagedProcess for FakeProcess {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn try_wait(&mut self) -> Result<Option<i32>, RuntimeError> {
        let mut state = self.backend.lock().unwrap();
        if state.fail_next_wait {
            state.fail_next_wait = false;
            return Err(RuntimeError::Backend("expected wait failure".into()));
        }
        drop(state);
        let exit = *self.exit.lock().unwrap();
        if exit.is_some() {
            self.mark_inactive();
        }
        Ok(exit)
    }

    fn stop(mut self: Box<Self>) -> ProcessStopResult {
        let mut state = self.backend.lock().unwrap();
        if state.fail_next_stop {
            state.fail_next_stop = false;
            return ProcessStopResult {
                final_output: Vec::new(),
                cleanup_error: Some(RuntimeError::Backend("expected stop failure".into())),
                output_error: None,
            };
        }
        if state.fail_next_stop_output {
            state.fail_next_stop_output = false;
            drop(state);
            self.mark_inactive();
            return ProcessStopResult {
                final_output: Vec::new(),
                cleanup_error: None,
                output_error: Some(RuntimeError::Backend("expected output failure".into())),
            };
        }
        drop(state);
        self.mark_inactive();
        ProcessStopResult::default()
    }

    fn input(&mut self, _bytes: &[u8]) -> Result<(), RuntimeError> {
        self.backend.lock().unwrap().input_count += 1;
        Ok(())
    }

    fn resize(&mut self, _columns: u16, _rows: u16) -> Result<(), RuntimeError> {
        self.backend.lock().unwrap().resize_count += 1;
        Ok(())
    }
}

#[async_trait]
impl ProcessBackend for FakeBackend {
    async fn spawn(&self, _request: SpawnRequest) -> Result<Box<dyn ManagedProcess>, RuntimeError> {
        let mut state = self.0.lock().unwrap();
        if state.fail_next_spawn {
            state.fail_next_spawn = false;
            return Err(RuntimeError::Backend("expected spawn failure".into()));
        }
        state.spawn_count += 1;
        state.next_pid += 1;
        state.active += 1;
        state.max_active = state.max_active.max(state.active);
        let exit = Arc::new(Mutex::new(None));
        state.exits.push(exit.clone());
        Ok(Box::new(FakeProcess {
            backend: self.0.clone(),
            pid: state.next_pid,
            exit,
            active: true,
        }))
    }

    async fn check_health(&self, _config: &ServerConfig) -> bool {
        let (block, result) = {
            let mut state = self.0.lock().unwrap();
            state.health_checks += 1;
            (state.block_health, state.health.pop_front().unwrap_or(true))
        };
        if block {
            self.1.notified().await;
        }
        result
    }
}

#[derive(Debug, Default)]
struct BlockingStopTracker {
    active: AtomicUsize,
    max_active: AtomicUsize,
}

struct BlockingStopProcess {
    tracker: Arc<BlockingStopTracker>,
}

impl ManagedProcess for BlockingStopProcess {
    fn pid(&self) -> u32 {
        42
    }

    fn try_wait(&mut self) -> Result<Option<i32>, RuntimeError> {
        Ok(None)
    }

    fn stop(self: Box<Self>) -> ProcessStopResult {
        let active = self.tracker.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.tracker.max_active.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(120));
        self.tracker.active.fetch_sub(1, Ordering::SeqCst);
        ProcessStopResult::default()
    }

    fn input(&mut self, _bytes: &[u8]) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn resize(&mut self, _columns: u16, _rows: u16) -> Result<(), RuntimeError> {
        Ok(())
    }
}

#[derive(Clone)]
struct BlockingStopBackend(Arc<BlockingStopTracker>);

#[async_trait]
impl ProcessBackend for BlockingStopBackend {
    async fn spawn(&self, _request: SpawnRequest) -> Result<Box<dyn ManagedProcess>, RuntimeError> {
        Ok(Box::new(BlockingStopProcess {
            tracker: self.0.clone(),
        }))
    }

    async fn check_health(&self, _config: &ServerConfig) -> bool {
        true
    }
}

fn runtime_spec(auto_restart: bool, max_restart_attempts: u32) -> RuntimeSpec {
    let mut config = server_config(None, None, None);
    config.auto_restart = auto_restart;
    RuntimeSpec {
        config,
        project_id: "prj_runtime".into(),
        project_name: "runtime".into(),
        project_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        settings: RuntimeSettings {
            health_interval: Duration::from_secs(2),
            max_restart_attempts,
            preferred_shell: PreferredShell::Cmd,
        },
    }
}

async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(20)).await;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    tokio::task::spawn_blocking(|| {})
        .await
        .expect("blocking task barrier");
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn no_probe_server_is_immediately_running_and_never_schedules_health() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_health([false]);
    let runtime = ServerRuntime::spawn(runtime_spec(true, 5), backend.clone());

    runtime.start().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Running);
    assert!(runtime.status().healthy);
    tokio::time::advance(Duration::from_secs(10)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Running);
    assert_eq!(backend.health_checks(), 0);
}

#[tokio::test(start_paused = true)]
async fn live_config_updates_reschedule_changed_probes_and_disable_removed_probes() {
    let backend = Arc::new(FakeBackend::default());
    let mut spec = runtime_spec(true, 5);
    let runtime = ServerRuntime::spawn(spec.clone(), backend.clone());
    runtime.start().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Running);
    assert!(runtime.status().healthy);

    spec.config.health_url = Some("http://localhost:1/first".into());
    runtime.update_config(spec.clone()).await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Unhealthy);
    assert!(!runtime.status().healthy);
    tokio::time::advance(Duration::from_millis(999)).await;
    tokio::task::yield_now().await;
    assert_eq!(backend.health_checks(), 0);
    tokio::time::advance(Duration::from_millis(1)).await;
    settle().await;
    assert!(runtime.status().healthy);
    assert_eq!(backend.health_checks(), 1);

    backend.queue_health([false]);
    spec.config.health_status = Some(204);
    runtime.update_config(spec.clone()).await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Unhealthy);
    assert!(!runtime.status().healthy);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Unhealthy);
    assert_eq!(backend.health_checks(), 2);

    spec.config.health_url = None;
    spec.config.health_status = None;
    runtime.update_config(spec).await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Running);
    assert!(runtime.status().healthy);
    tokio::time::advance(Duration::from_secs(10)).await;
    settle().await;
    assert_eq!(backend.health_checks(), 2);
}

#[tokio::test(start_paused = true)]
async fn manual_start_during_backoff_spawns_now_and_cancels_the_old_deadline() {
    let backend = Arc::new(FakeBackend::default());
    let runtime = ServerRuntime::spawn(runtime_spec(true, 5), backend.clone());
    runtime.start().await.unwrap();
    backend.crash_latest(7);
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Restarting);
    assert_eq!(runtime.status().restart_count, 1);

    runtime.start().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Running);
    assert_eq!(runtime.status().restart_count, 0);
    assert_eq!(backend.snapshot().2, 2);
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert_eq!(backend.snapshot().2, 2);
}

#[tokio::test(start_paused = true)]
async fn config_update_cancels_or_fails_an_ineligible_pending_restart() {
    let disabled_backend = Arc::new(FakeBackend::default());
    let mut disabled_spec = runtime_spec(true, 5);
    let disabled = ServerRuntime::spawn(disabled_spec.clone(), disabled_backend.clone());
    disabled.start().await.unwrap();
    disabled_backend.crash_latest(7);
    settle().await;
    assert_eq!(disabled.status().state, ServerState::Restarting);
    disabled_spec.config.auto_restart = false;
    disabled.update_config(disabled_spec).await.unwrap();
    assert_eq!(disabled.status().state, ServerState::Stopped);
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert_eq!(disabled_backend.snapshot().2, 1);

    let equal_backend = Arc::new(FakeBackend::default());
    let mut equal_spec = runtime_spec(true, 5);
    let equal = ServerRuntime::spawn(equal_spec.clone(), equal_backend.clone());
    equal.start().await.unwrap();
    equal_backend.crash_latest(8);
    settle().await;
    assert_eq!(equal.status().restart_count, 1);
    equal_spec.settings.max_restart_attempts = 1;
    equal.update_config(equal_spec).await.unwrap();
    assert_eq!(equal.status().state, ServerState::Restarting);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(equal_backend.snapshot().2, 2);

    let excessive_backend = Arc::new(FakeBackend::default());
    let mut excessive_spec = runtime_spec(true, 5);
    let excessive = ServerRuntime::spawn(excessive_spec.clone(), excessive_backend.clone());
    excessive.start().await.unwrap();
    excessive_backend.crash_latest(9);
    settle().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    excessive_backend.crash_latest(9);
    settle().await;
    assert_eq!(excessive.status().restart_count, 2);
    excessive_spec.settings.max_restart_attempts = 1;
    excessive.update_config(excessive_spec).await.unwrap();
    assert_eq!(excessive.status().state, ServerState::Failed);
    assert!(
        excessive
            .status()
            .last_error
            .as_deref()
            .unwrap()
            .contains("restart budget")
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert_eq!(excessive_backend.snapshot().2, 2);
}

fn is_legal_state_transition(from: ServerState, to: ServerState) -> bool {
    matches!(
        (from, to),
        (ServerState::Stopped, ServerState::Starting)
            | (ServerState::Starting, ServerState::Running)
            | (ServerState::Starting, ServerState::Failed)
            | (ServerState::Starting, ServerState::Stopped)
            | (ServerState::Running, ServerState::Unhealthy)
            | (ServerState::Running, ServerState::Restarting)
            | (ServerState::Running, ServerState::Stopped)
            | (ServerState::Running, ServerState::Failed)
            | (ServerState::Unhealthy, ServerState::Running)
            | (ServerState::Unhealthy, ServerState::Restarting)
            | (ServerState::Unhealthy, ServerState::Stopped)
            | (ServerState::Restarting, ServerState::Starting)
            | (ServerState::Restarting, ServerState::Stopped)
            | (ServerState::Restarting, ServerState::Failed)
            | (ServerState::Failed, ServerState::Starting)
            | (ServerState::Failed, ServerState::Stopped)
    )
}

#[tokio::test(start_paused = true)]
async fn exhausted_health_restart_budget_publishes_only_legal_state_transitions() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_health([true, false, false, false, true, false, false, false]);
    let mut spec = runtime_spec(true, 1);
    spec.config.health_url = Some("http://localhost:1/health".into());
    let runtime = ServerRuntime::spawn(spec, backend);
    let mut statuses = runtime.subscribe();
    let states = tokio::spawn(async move {
        let mut observed = vec![statuses.borrow().state];
        loop {
            statuses.changed().await.expect("runtime status publisher");
            let state = statuses.borrow().state;
            if observed.last() != Some(&state) {
                observed.push(state);
            }
            if state == ServerState::Failed {
                return observed;
            }
        }
    });
    tokio::task::yield_now().await;

    runtime.start().await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
    }

    let observed = states.await.unwrap();
    assert_eq!(
        observed,
        [
            ServerState::Stopped,
            ServerState::Starting,
            ServerState::Running,
            ServerState::Unhealthy,
            ServerState::Restarting,
            ServerState::Starting,
            ServerState::Running,
            ServerState::Unhealthy,
            ServerState::Restarting,
            ServerState::Failed,
        ]
    );
    for transition in observed.windows(2) {
        assert!(
            is_legal_state_transition(transition[0], transition[1]),
            "illegal state transition: {:?} -> {:?}",
            transition[0],
            transition[1]
        );
    }
    assert_eq!(runtime.status().restart_count, 1);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn exhausted_process_exit_from_unhealthy_publishes_restart_before_failed() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_health([true, true, false]);
    let mut spec = runtime_spec(true, 1);
    spec.config.health_url = Some("http://localhost:1/health".into());
    let runtime = ServerRuntime::spawn(spec, backend.clone());

    runtime.start().await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Running);

    backend.crash_latest(17);
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Restarting);
    assert_eq!(runtime.status().restart_count, 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Running);
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Unhealthy);

    let mut statuses = runtime.subscribe();
    let states = tokio::spawn(async move {
        let mut observed = vec![statuses.borrow().state];
        loop {
            statuses.changed().await.expect("runtime status publisher");
            let state = statuses.borrow().state;
            if observed.last() != Some(&state) {
                observed.push(state);
            }
            if state == ServerState::Failed {
                return observed;
            }
        }
    });
    tokio::task::yield_now().await;

    backend.crash_latest(18);
    settle().await;

    let observed = states.await.unwrap();
    assert_eq!(
        observed,
        [
            ServerState::Unhealthy,
            ServerState::Restarting,
            ServerState::Failed,
        ]
    );
    for transition in observed.windows(2) {
        assert!(
            is_legal_state_transition(transition[0], transition[1]),
            "illegal state transition: {:?} -> {:?}",
            transition[0],
            transition[1]
        );
    }
    assert_eq!(runtime.status().restart_count, 1);
    assert_eq!(backend.snapshot().2, 2);
    runtime.shutdown().await.unwrap();
}

async fn wait_for_health_check(backend: &FakeBackend) {
    for _ in 0..20 {
        if backend.health_checks() != 0 {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("health check did not start");
}

async fn assert_finishes_without_advancing_time<T>(task: &tokio::task::JoinHandle<T>, name: &str) {
    for _ in 0..50 {
        if task.is_finished() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("{name} waited for health");
}

#[tokio::test(start_paused = true)]
async fn inflight_health_is_cancellable_for_update_stop_and_shutdown() {
    let update_backend = Arc::new(FakeBackend::default());
    update_backend.queue_health([false]);
    update_backend.block_health();
    let mut update_spec = runtime_spec(true, 5);
    update_spec.config.health_url = Some("http://localhost:1/health".into());
    let updated = ServerRuntime::spawn(update_spec.clone(), update_backend.clone());
    updated.start().await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    wait_for_health_check(&update_backend).await;
    update_spec.config.health_url = None;
    let update_task = tokio::spawn({
        let updated = updated.clone();
        async move { updated.update_config(update_spec).await }
    });
    assert_finishes_without_advancing_time(&update_task, "UpdateConfig").await;
    update_task.await.unwrap().unwrap();
    update_backend.release_health();
    settle().await;
    assert_eq!(updated.status().state, ServerState::Running);
    assert!(updated.status().healthy);
    updated.stop().await.unwrap();

    for shutdown in [false, true] {
        let backend = Arc::new(FakeBackend::default());
        backend.block_health();
        let mut spec = runtime_spec(true, 5);
        spec.config.health_url = Some("http://localhost:1/health".into());
        let runtime = ServerRuntime::spawn(spec, backend.clone());
        runtime.start().await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        wait_for_health_check(&backend).await;
        let command = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                if shutdown {
                    runtime.shutdown().await
                } else {
                    runtime.stop().await
                }
            }
        });
        assert_finishes_without_advancing_time(&command, "lifecycle command").await;
        command.await.unwrap().unwrap();
        backend.release_health();
        settle().await;
        assert_eq!(runtime.status().state, ServerState::Stopped);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn blocking_stops_are_offloaded_across_independent_runtimes() {
    let tracker = Arc::new(BlockingStopTracker::default());
    let backend = Arc::new(BlockingStopBackend(tracker.clone()));
    let runtimes = (0..3)
        .map(|_| ServerRuntime::spawn(runtime_spec(false, 5), backend.clone()))
        .collect::<Vec<_>>();
    for runtime in &runtimes {
        runtime.start().await.unwrap();
    }

    let [first, second, third] = runtimes.as_slice() else {
        unreachable!()
    };
    let (first, second, third) =
        tokio::join!(first.shutdown(), second.shutdown(), third.shutdown());
    first.unwrap();
    second.unwrap();
    third.unwrap();
    assert!(
        tracker.max_active.load(Ordering::SeqCst) >= 2,
        "blocking stops ran serially on Tokio workers"
    );
}

#[tokio::test(start_paused = true)]
async fn process_exit_during_health_startup_fails_even_with_auto_restart() {
    let backend = Arc::new(FakeBackend::default());
    let mut spec = runtime_spec(true, 5);
    spec.config.health_url = Some("http://localhost:1/health".into());
    let runtime = ServerRuntime::spawn(spec, backend.clone());

    runtime.start().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Starting);
    backend.crash_latest(23);
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Failed);
    assert_eq!(runtime.status().restart_count, 0);
    assert_eq!(runtime.status().last_exit_code, Some(23));
}

#[tokio::test(start_paused = true)]
async fn manual_stop_cleanup_error_still_publishes_stopped_with_the_error() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_health([true, false]);
    let mut spec = runtime_spec(true, 5);
    spec.config.health_url = Some("http://localhost:1/health".into());
    let runtime = ServerRuntime::spawn(spec, backend.clone());
    runtime.start().await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Running);
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Unhealthy);
    backend.fail_next_stop();

    assert!(runtime.stop().await.is_err());
    assert_eq!(runtime.status().state, ServerState::Stopped);
    assert!(
        runtime
            .status()
            .last_error
            .as_deref()
            .unwrap()
            .contains("stop failure")
    );
    assert_eq!(backend.snapshot().0, 0);
}

#[tokio::test(start_paused = true)]
async fn unhealthy_wait_error_restarts_only_when_auto_restart_is_enabled() {
    for (auto_restart, expected) in [
        (true, ServerState::Restarting),
        (false, ServerState::Stopped),
    ] {
        let backend = Arc::new(FakeBackend::default());
        backend.queue_health([true, false]);
        let mut spec = runtime_spec(auto_restart, 5);
        spec.config.health_url = Some("http://localhost:1/health".into());
        let runtime = ServerRuntime::spawn(spec, backend.clone());
        runtime.start().await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        assert_eq!(runtime.status().state, ServerState::Unhealthy);

        backend.fail_next_wait();
        settle().await;
        assert_eq!(runtime.status().state, expected);
        assert!(
            runtime
                .status()
                .last_error
                .as_deref()
                .unwrap()
                .contains("wait failure")
        );
        assert_eq!(backend.snapshot().0, 0);
    }
}

#[tokio::test(start_paused = true)]
async fn nonzero_restart_counter_is_reset_by_manual_restart_and_manual_start() {
    let backend = Arc::new(FakeBackend::default());
    let runtime = ServerRuntime::spawn(runtime_spec(true, 5), backend.clone());
    runtime.start().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Running);

    backend.crash_latest(7);
    settle().await;
    assert_eq!(runtime.status().restart_count, 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Running);
    runtime.restart().await.unwrap();
    assert_eq!(runtime.status().restart_count, 0);

    backend.crash_latest(8);
    settle().await;
    assert_eq!(runtime.status().restart_count, 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    runtime.stop().await.unwrap();
    assert_eq!(runtime.status().restart_count, 1);
    runtime.start().await.unwrap();
    assert_eq!(runtime.status().restart_count, 0);

    let failed_backend = Arc::new(FakeBackend::default());
    let failed_runtime = ServerRuntime::spawn(runtime_spec(true, 1), failed_backend.clone());
    failed_runtime.start().await.unwrap();
    failed_backend.crash_latest(9);
    settle().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    failed_backend.crash_latest(9);
    settle().await;
    assert_eq!(failed_runtime.status().state, ServerState::Failed);
    assert_eq!(failed_runtime.status().restart_count, 1);
    failed_runtime.start().await.unwrap();
    assert_eq!(failed_runtime.status().state, ServerState::Running);
    assert_eq!(failed_runtime.status().restart_count, 0);
}

#[tokio::test(start_paused = true)]
async fn manual_restart_resets_counter_even_when_cleanup_reports_an_error() {
    let backend = Arc::new(FakeBackend::default());
    let runtime = ServerRuntime::spawn(runtime_spec(true, 5), backend.clone());
    runtime.start().await.unwrap();
    backend.crash_latest(7);
    settle().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().restart_count, 1);
    backend.fail_next_stop();

    assert!(runtime.restart().await.is_err());
    assert_eq!(runtime.status().state, ServerState::Stopped);
    assert_eq!(runtime.status().restart_count, 0);
}

#[tokio::test]
async fn terminal_output_error_after_successful_cleanup_does_not_block_manual_restart() {
    let backend = Arc::new(FakeBackend::default());
    let runtime = ServerRuntime::spawn(runtime_spec(true, 5), backend.clone());
    runtime.start().await.unwrap();
    backend.fail_next_stop_output();

    runtime
        .restart()
        .await
        .expect("terminal capture failure must not make cleanup fatal");

    assert_eq!(runtime.status().state, ServerState::Running);
    assert_eq!(runtime.status().last_error, None);
    assert_eq!(backend.snapshot().2, 2);
}

#[tokio::test(start_paused = true)]
async fn runtime_covers_start_failure_manual_start_health_restart_and_stop_transitions() {
    let backend = Arc::new(FakeBackend::default());
    backend.fail_next_spawn();
    backend.queue_health([true, false, false, false, true]);
    let mut spec = runtime_spec(true, 5);
    spec.config.health_url = Some("http://localhost:1/health".into());
    let runtime = ServerRuntime::spawn(spec, backend.clone());

    assert!(runtime.start().await.is_err());
    assert_eq!(runtime.status().state, ServerState::Failed);
    runtime.start().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Starting);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Running);

    for expected in [
        ServerState::Unhealthy,
        ServerState::Unhealthy,
        ServerState::Restarting,
    ] {
        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        assert_eq!(runtime.status().state, expected);
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Starting);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Running);

    runtime.stop().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Stopped);
    assert_eq!(backend.snapshot().0, 0);
}

#[tokio::test(start_paused = true)]
async fn restart_budget_fails_and_more_than_thirty_healthy_seconds_resets_it() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_health([true; 64]);
    let runtime = ServerRuntime::spawn(runtime_spec(true, 2), backend.clone());
    runtime.start().await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;

    backend.crash_latest(9);
    settle().await;
    assert_eq!(runtime.status().restart_count, 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Running);

    tokio::time::advance(Duration::from_secs(31)).await;
    settle().await;
    assert_eq!(runtime.status().restart_count, 0);

    backend.crash_latest(9);
    settle().await;
    assert_eq!(runtime.status().restart_count, 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    backend.crash_latest(9);
    settle().await;
    assert_eq!(runtime.status().restart_count, 2);
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    backend.crash_latest(9);
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Failed);
}

#[tokio::test(start_paused = true)]
async fn auto_restart_false_and_manual_restart_obey_serialized_commands() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_health([true, false, false, false]);
    let mut spec = runtime_spec(false, 5);
    spec.config.health_url = Some("http://localhost:1/health".into());
    let runtime = ServerRuntime::spawn(spec, backend.clone());
    runtime.start().await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
    }
    assert_eq!(runtime.status().state, ServerState::Unhealthy);
    assert_eq!(backend.snapshot().2, 1);

    runtime.restart().await.unwrap();
    assert_eq!(runtime.status().restart_count, 0);
    assert_eq!(backend.snapshot().0, 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Running);
    runtime.input(b"hello").await.unwrap();
    runtime.resize(120, 40).await.unwrap();
    assert_eq!(backend.snapshot().3, 1);
    assert_eq!(backend.snapshot().4, 1);

    backend.crash_latest(7);
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Stopped);
    runtime.start().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Starting);
    runtime.shutdown().await.unwrap();
    assert_eq!(backend.snapshot().0, 0);
}

#[tokio::test(start_paused = true)]
async fn concurrent_start_stop_restart_commands_never_overlap_process_trees() {
    let backend = Arc::new(FakeBackend::default());
    let runtime = ServerRuntime::spawn(runtime_spec(true, 5), backend.clone());
    let mut tasks = Vec::new();
    for index in 0..30 {
        let runtime = runtime.clone();
        tasks.push(tokio::spawn(async move {
            match index % 3 {
                0 => runtime.start().await,
                1 => runtime.restart().await,
                _ => runtime.stop().await,
            }
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    runtime.shutdown().await.unwrap();
    let (active, max_active, ..) = backend.snapshot();
    assert_eq!(active, 0);
    assert_eq!(max_active, 1);
}

#[tokio::test(start_paused = true)]
async fn remaining_allowed_state_transitions_are_reachable() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_health([true, false, true, true, true]);
    let mut spec = runtime_spec(true, 3);
    spec.config.health_url = Some("http://localhost:1/health".into());
    let runtime = ServerRuntime::spawn(spec, backend.clone());

    runtime.start().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Starting);
    runtime.stop().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Stopped);

    runtime.start().await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Unhealthy);
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Running);

    backend.crash_latest(3);
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Restarting);
    runtime.stop().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Stopped);

    runtime.start().await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Running);
    backend.fail_next_wait();
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Failed);
    runtime.stop().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Stopped);

    runtime.start().await.unwrap();
    backend.fail_next_stop();
    assert!(runtime.stop().await.is_err());
    assert_eq!(runtime.status().state, ServerState::Stopped);

    runtime.start().await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Running);
    backend.crash_latest(4);
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Restarting);
    backend.fail_next_spawn();
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(runtime.status().state, ServerState::Failed);
    runtime.start().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Starting);
    runtime.shutdown().await.unwrap();
    assert_eq!(backend.snapshot().0, 0);
}

#[tokio::test(start_paused = true)]
async fn shutdown_closes_the_runtime_even_when_process_cleanup_reports_an_error() {
    let backend = Arc::new(FakeBackend::default());
    let runtime = ServerRuntime::spawn(runtime_spec(true, 3), backend.clone());
    runtime.start().await.unwrap();
    backend.fail_next_stop();

    assert!(runtime.shutdown().await.is_err());
    assert!(matches!(runtime.start().await, Err(RuntimeError::Closed)));
    assert_eq!(backend.snapshot().0, 0);
}

#[cfg(windows)]
async fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[cfg(windows)]
fn quoted(path: &str) -> String {
    if path.contains([' ', '\t']) {
        format!("\"{}\"", path.replace('"', "\\\""))
    } else {
        path.to_owned()
    }
}

#[cfg(windows)]
static WINDOWS_ENVIRONMENT_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

#[cfg(windows)]
struct ScopedEnvironment {
    _lock: AsyncMutexGuard<'static, ()>,
    name: &'static str,
    original: Option<std::ffi::OsString>,
}

#[cfg(windows)]
impl ScopedEnvironment {
    async fn set(name: &'static str, value: &str) -> Self {
        let lock = WINDOWS_ENVIRONMENT_LOCK.lock().await;
        let original = std::env::var_os(name);
        // SAFETY: every Windows backend test in this binary holds the same
        // process-environment lock, including for the entire spawned runtime.
        unsafe { std::env::set_var(name, value) };
        Self {
            _lock: lock,
            name,
            original,
        }
    }
}

#[cfg(windows)]
impl Drop for ScopedEnvironment {
    fn drop(&mut self) {
        // SAFETY: the lock remains held until after the original value is
        // restored, and all environment-reading backend tests share it.
        unsafe {
            if let Some(value) = &self.original {
                std::env::set_var(self.name, value);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }
}

#[cfg(windows)]
#[tokio::test]
async fn windows_backend_runs_echo_fixture_and_releases_its_process_tree() {
    let _environment_lock = WINDOWS_ENVIRONMENT_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let ready_file = temp.path().join("echo-port.txt");
    let mut spec = runtime_spec(false, 5);
    spec.config.command = quoted(env!("CARGO_BIN_EXE_echo-server-fixture"));
    spec.config.env.insert(
        "NETSUSTACK_READY_FILE".into(),
        ready_file.display().to_string(),
    );
    let runtime = ServerRuntime::spawn(spec.clone(), Arc::new(WindowsProcessBackend::default()));
    runtime.start().await.unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || ready_file.is_file()).await,
        "echo fixture never reported its port"
    );
    let port = std::fs::read_to_string(&ready_file)
        .unwrap()
        .trim()
        .parse::<u16>()
        .unwrap();
    spec.config.port = Some(port);
    spec.config.health_url = Some("health".into());
    runtime.update_config(spec).await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Unhealthy);
    assert!(!runtime.status().healthy);
    assert!(
        wait_until(Duration::from_secs(5), || runtime.status().healthy).await,
        "echo fixture never became healthy: {:?}",
        runtime.status()
    );
    runtime.stop().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Stopped);
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn windows_backend_builds_the_documented_effective_child_environment() {
    let _environment_lock = WINDOWS_ENVIRONMENT_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let env_file = temp.path().join("child-env.txt");
    let mut spec = runtime_spec(false, 5);
    spec.config.name = "environment-fixture".into();
    spec.config.command = quoted(env!("CARGO_BIN_EXE_echo-server-fixture"));
    spec.config.port = Some(4321);
    spec.config.env = HashMap::from([
        ("PATH".into(), "server-path".into()),
        ("TERM".into(), "server-term".into()),
        ("COLORTERM".into(), "server-colorterm".into()),
        ("FORCE_COLOR".into(), "server-force-color".into()),
        ("CLICOLOR".into(), "server-clicolor".into()),
        ("CLICOLOR_FORCE".into(), "server-clicolor-force".into()),
        ("TERM_PROGRAM".into(), "server-term-program".into()),
        ("NETSUSTACK".into(), "server-netsustack".into()),
        ("NETSUSTACK_SERVER".into(), "server-name".into()),
        ("nEtSuStAcK_sErVeR_nAmE".into(), "wrong-name".into()),
        ("nO_cOlOr".into(), "server-no-color".into()),
        ("NETSUSTACK_BIND_PORT".into(), "0".into()),
        ("NETSUSTACK_ENV_FILE".into(), env_file.display().to_string()),
    ]);
    let runtime = ServerRuntime::spawn(spec, Arc::new(WindowsProcessBackend::default()));

    runtime.start().await.unwrap();
    assert!(wait_until(Duration::from_secs(5), || env_file.is_file()).await);
    let environment = std::fs::read_to_string(env_file).unwrap();
    for expected in [
        "PATH=server-path",
        "TERM=xterm-256color",
        "COLORTERM=truecolor",
        "FORCE_COLOR=1",
        "CLICOLOR=1",
        "CLICOLOR_FORCE=1",
        "TERM_PROGRAM=NetsuStack",
        "NETSUSTACK=1",
        "NETSUSTACK_SERVER=environment-fixture",
        "NETSUSTACK_SERVER_NAME=<missing>",
        "PORT=4321",
        "NO_COLOR=<missing>",
    ] {
        assert!(
            environment.lines().any(|line| line == expected),
            "missing {expected:?} in {environment:?}"
        );
    }
    let inherited_system_root = format!(
        "SYSTEMROOT={}",
        std::env::var("SYSTEMROOT").expect("Windows SYSTEMROOT")
    );
    assert!(
        environment
            .lines()
            .any(|line| line.eq_ignore_ascii_case(&inherited_system_root)),
        "missing inherited {inherited_system_root:?} in {environment:?}"
    );
    runtime.stop().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_backend_removes_an_inherited_port_when_none_is_configured() {
    let _environment = ScopedEnvironment::set("pOrT", "6543").await;
    let temp = tempfile::tempdir().unwrap();
    let env_file = temp.path().join("child-env.txt");
    let mut spec = runtime_spec(false, 5);
    spec.config.command = quoted(env!("CARGO_BIN_EXE_echo-server-fixture"));
    spec.config.env = HashMap::from([
        ("NETSUSTACK_BIND_PORT".into(), "0".into()),
        ("NETSUSTACK_ENV_FILE".into(), env_file.display().to_string()),
    ]);
    assert_eq!(spec.config.port, None);
    let runtime = ServerRuntime::spawn(spec, Arc::new(WindowsProcessBackend::default()));

    runtime.start().await.unwrap();
    assert!(wait_until(Duration::from_secs(5), || env_file.is_file()).await);
    let environment = std::fs::read_to_string(env_file).unwrap();
    assert!(
        environment.lines().any(|line| line == "PORT=<missing>"),
        "inherited PORT leaked into {environment:?}"
    );
    runtime.stop().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_cmd_preserves_a_raw_quoted_command_in_a_directory_with_spaces() {
    let _environment_lock = WINDOWS_ENVIRONMENT_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let program_directory = temp.path().join("program directory");
    std::fs::create_dir(&program_directory).unwrap();
    let program = program_directory.join("echo fixture.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_echo-server-fixture"), &program).unwrap();
    let argument_file = temp.path().join("argument marker.txt");
    let mut spec = runtime_spec(false, 5);
    spec.config.command = format!(
        "{} --args-file {} {}",
        quoted(&program.display().to_string()),
        quoted(&argument_file.display().to_string()),
        quoted("literal & metacharacter")
    );

    let runtime = ServerRuntime::spawn(spec, Arc::new(WindowsProcessBackend::default()));
    runtime.start().await.unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || argument_file.is_file()).await,
        "quoted raw cmd command did not launch: {:?}",
        runtime.status()
    );
    assert_eq!(
        std::fs::read_to_string(argument_file).unwrap(),
        "literal & metacharacter"
    );
    runtime.stop().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_backend_applies_crash_restart_budget_to_real_fixture() {
    let _environment_lock = WINDOWS_ENVIRONMENT_LOCK.lock().await;
    let mut spec = runtime_spec(true, 2);
    spec.config.command = format!("{} 17 20", quoted(env!("CARGO_BIN_EXE_crash-loop-fixture")));
    let runtime = ServerRuntime::spawn(spec, Arc::new(WindowsProcessBackend::default()));
    runtime.start().await.unwrap();
    assert_eq!(runtime.status().state, ServerState::Running);
    assert!(runtime.status().healthy);
    assert!(
        wait_until(Duration::from_secs(2), || runtime.status().state
            == ServerState::Restarting)
        .await,
        "crash fixture did not transition from running to restarting: {:?}",
        runtime.status()
    );
    assert!(
        wait_until(Duration::from_secs(8), || runtime.status().state
            == ServerState::Failed)
        .await,
        "crash fixture did not exhaust its budget: {:?}",
        runtime.status()
    );
    assert_eq!(runtime.status().restart_count, 2);
    assert_eq!(runtime.status().last_exit_code, Some(17));
    runtime.shutdown().await.unwrap();
}
