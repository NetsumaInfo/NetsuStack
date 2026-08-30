use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use netsustack_domain::{PreferredShell, ServerAction, ServerConfig, TemporaryJobState};
use netsustack_supervisor::{
    runtime::{
        ManagedProcess, ProcessBackend, ProcessStopResult, RuntimeError, RuntimeSettings,
        RuntimeSpec, SpawnRequest,
    },
    temporary::{
        COMPLETED_RETENTION, MAX_TIMEOUT, TemporaryClock, TemporaryError, TemporaryJobSpec,
        TemporarySupervisor,
    },
};
use tokio::{sync::watch, time::Instant};

const TEST_LOG_BUFFER_LINES: usize = 5_000;

#[derive(Debug, Default)]
struct ProcessPlan {
    exit: Option<i32>,
    wait_error: Option<String>,
    output: Vec<u8>,
    cleanup_output: Vec<u8>,
    cleanup_error: Option<String>,
    output_error: Option<String>,
    stop_gate: Option<Arc<StopGate>>,
}

#[derive(Debug, Default)]
struct FakeState {
    plans: VecDeque<ProcessPlan>,
    requests: Vec<SpawnRequest>,
    stop_count: usize,
    health_checks: usize,
}

#[derive(Debug, Clone, Default)]
struct FakeBackend(Arc<Mutex<FakeState>>);

impl FakeBackend {
    fn queue_exit(&self, exit: Option<i32>) {
        self.0.lock().unwrap().plans.push_back(ProcessPlan {
            exit,
            ..ProcessPlan::default()
        });
    }

    fn queue_job(&self, exit: Option<i32>, output: impl Into<Vec<u8>>) {
        self.0.lock().unwrap().plans.push_back(ProcessPlan {
            exit,
            wait_error: None,
            output: output.into(),
            cleanup_output: Vec::new(),
            cleanup_error: None,
            output_error: None,
            stop_gate: None,
        });
    }

    fn queue_cleanup_output(&self, output: impl Into<Vec<u8>>) {
        self.0.lock().unwrap().plans.push_back(ProcessPlan {
            exit: None,
            wait_error: None,
            output: Vec::new(),
            cleanup_output: output.into(),
            cleanup_error: None,
            output_error: None,
            stop_gate: None,
        });
    }

    fn queue_blocking_stop(&self, gate: Arc<StopGate>) {
        self.0.lock().unwrap().plans.push_back(ProcessPlan {
            exit: None,
            wait_error: None,
            output: Vec::new(),
            cleanup_output: Vec::new(),
            cleanup_error: None,
            output_error: None,
            stop_gate: Some(gate),
        });
    }

    fn queue_blocking_stop_with_output(&self, gate: Arc<StopGate>, output: impl Into<Vec<u8>>) {
        self.0.lock().unwrap().plans.push_back(ProcessPlan {
            exit: None,
            wait_error: None,
            output: Vec::new(),
            cleanup_output: output.into(),
            cleanup_error: None,
            output_error: None,
            stop_gate: Some(gate),
        });
    }

    fn queue_wait_error_with_cleanup_output(&self, output: impl Into<Vec<u8>>) {
        self.0.lock().unwrap().plans.push_back(ProcessPlan {
            exit: None,
            wait_error: Some("expected wait failure".into()),
            output: Vec::new(),
            cleanup_output: output.into(),
            cleanup_error: Some("expected cleanup failure".into()),
            output_error: Some("expected output failure".into()),
            stop_gate: None,
        });
    }

    fn snapshot(&self) -> (Vec<SpawnRequest>, usize, usize) {
        let state = self.0.lock().unwrap();
        (
            state.requests.clone(),
            state.stop_count,
            state.health_checks,
        )
    }
}

struct FakeProcess {
    state: Arc<Mutex<FakeState>>,
    exit: Option<i32>,
    wait_error: Option<String>,
    output: Arc<Mutex<Vec<u8>>>,
    cleanup_output: Vec<u8>,
    cleanup_error: Option<String>,
    output_error: Option<String>,
    stop_gate: Option<Arc<StopGate>>,
}

impl ManagedProcess for FakeProcess {
    fn pid(&self) -> u32 {
        42
    }

    fn try_wait(&mut self) -> Result<Option<i32>, RuntimeError> {
        if let Some(error) = self.wait_error.take() {
            return Err(RuntimeError::Backend(error));
        }
        Ok(self.exit)
    }

    fn stop(self: Box<Self>) -> ProcessStopResult {
        if let Some(gate) = &self.stop_gate {
            gate.block_stop();
        }
        self.output.lock().unwrap().extend(&self.cleanup_output);
        self.state.lock().unwrap().stop_count += 1;
        ProcessStopResult {
            final_output: std::mem::take(&mut *self.output.lock().unwrap()),
            cleanup_error: self.cleanup_error.map(RuntimeError::Backend),
            output_error: self.output_error.map(RuntimeError::Backend),
        }
    }

    fn input(&mut self, _bytes: &[u8]) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn resize(&mut self, _columns: u16, _rows: u16) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn drain_output(&mut self) -> Result<Vec<u8>, RuntimeError> {
        Ok(std::mem::take(&mut *self.output.lock().unwrap()))
    }
}

#[async_trait]
impl ProcessBackend for FakeBackend {
    async fn spawn(&self, request: SpawnRequest) -> Result<Box<dyn ManagedProcess>, RuntimeError> {
        let mut state = self.0.lock().unwrap();
        state.requests.push(request);
        let plan = state.plans.pop_front().unwrap_or_default();
        Ok(Box::new(FakeProcess {
            state: self.0.clone(),
            exit: plan.exit,
            wait_error: plan.wait_error,
            output: Arc::new(Mutex::new(plan.output)),
            cleanup_output: plan.cleanup_output,
            cleanup_error: plan.cleanup_error,
            output_error: plan.output_error,
            stop_gate: plan.stop_gate,
        }))
    }

    async fn check_health(&self, _config: &ServerConfig) -> bool {
        self.0.lock().unwrap().health_checks += 1;
        true
    }
}

#[derive(Debug)]
struct FakeClockState {
    monotonic: Instant,
    wall: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct FakeClock {
    state: Arc<Mutex<FakeClockState>>,
    changed: watch::Sender<Instant>,
    active_sleeps: Arc<AtomicUsize>,
}

impl FakeClock {
    fn new() -> Self {
        let state = FakeClockState {
            monotonic: Instant::now(),
            wall: Utc.with_ymd_and_hms(2026, 8, 30, 12, 0, 0).unwrap(),
        };
        let (changed, _) = watch::channel(state.monotonic);
        Self {
            state: Arc::new(Mutex::new(state)),
            changed,
            active_sleeps: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn advance_monotonic(&self, duration: Duration) {
        let now = {
            let mut state = self.state.lock().unwrap();
            state.monotonic += duration;
            state.monotonic
        };
        self.changed.send_replace(now);
    }

    fn move_wall_back(&self, duration: chrono::Duration) {
        self.state.lock().unwrap().wall -= duration;
    }

    fn active_sleeps(&self) -> usize {
        self.active_sleeps.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl TemporaryClock for FakeClock {
    fn monotonic_now(&self) -> Instant {
        self.state.lock().unwrap().monotonic
    }

    fn utc_now(&self) -> DateTime<Utc> {
        self.state.lock().unwrap().wall
    }

    async fn sleep_until(&self, deadline: Instant) {
        self.active_sleeps.fetch_add(1, Ordering::SeqCst);
        let _guard = SleepGuard(self.active_sleeps.clone());
        let mut changed = self.changed.subscribe();
        while *changed.borrow_and_update() < deadline {
            changed.changed().await.unwrap();
        }
    }
}

struct SleepGuard(Arc<AtomicUsize>);

impl Drop for SleepGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Default)]
struct StopGate {
    started: AtomicBool,
    released: Mutex<bool>,
    changed: Condvar,
}

impl StopGate {
    fn block_stop(&self) {
        self.started.store(true, Ordering::SeqCst);
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.changed.wait(released).unwrap();
        }
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

fn temporary_spec(command: &str, timeout: Duration) -> TemporaryJobSpec {
    TemporaryJobSpec {
        name: None,
        command: command.into(),
        directory: PathBuf::from(r"C:\workspace"),
        environment: HashMap::new(),
        port: None,
        timeout,
        preferred_shell: PreferredShell::Cmd,
    }
}

async fn wait_until_finished(supervisor: &TemporarySupervisor, id: &str) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if supervisor
                .status(id)
                .await
                .is_some_and(|status| status.state.is_finished())
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("job did not finish");
}

#[tokio::test]
async fn reports_success_nonzero_exit_manual_stop_and_timeout_exit_codes() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_exit(Some(0));
    backend.queue_exit(Some(23));
    backend.queue_exit(None);
    backend.queue_exit(None);
    let clock = Arc::new(FakeClock::new());
    let supervisor =
        TemporarySupervisor::with_clock(backend.clone(), clock.clone(), TEST_LOG_BUFFER_LINES);

    let succeeded = supervisor
        .run(temporary_spec("quick success", Duration::from_secs(30)))
        .await
        .unwrap();
    let succeeded = supervisor.wait(&succeeded.id).await.unwrap();
    assert_eq!(succeeded.status.state, TemporaryJobState::Succeeded);
    assert_eq!(succeeded.status.process_exit_code(), 0);

    let failed = supervisor
        .run(temporary_spec("quick failure", Duration::from_secs(30)))
        .await
        .unwrap();
    let failed = supervisor.wait(&failed.id).await.unwrap();
    assert_eq!(failed.status.state, TemporaryJobState::Failed);
    assert_eq!(failed.status.exit_code, Some(23));
    assert_eq!(failed.status.process_exit_code(), 23);

    let stopped = supervisor
        .run(temporary_spec("long running", Duration::from_secs(30)))
        .await
        .unwrap();
    let stopped = supervisor.stop(&stopped.id).await.unwrap();
    assert_eq!(stopped.state, TemporaryJobState::Stopped);
    assert_eq!(stopped.process_exit_code(), 130);

    let timed_out = supervisor
        .run(temporary_spec("times out", Duration::from_secs(5)))
        .await
        .unwrap();
    clock.advance_monotonic(Duration::from_secs(5));
    wait_until_finished(&supervisor, &timed_out.id).await;
    let timed_out = supervisor.wait(&timed_out.id).await.unwrap();
    assert_eq!(timed_out.status.state, TemporaryJobState::TimedOut);
    assert_eq!(timed_out.status.process_exit_code(), 124);
    assert_eq!(backend.snapshot().1, 2);
}

#[tokio::test]
async fn timeout_uses_a_monotonic_deadline_and_is_capped_at_seven_days() {
    let backend = Arc::new(FakeBackend::default());
    let clock = Arc::new(FakeClock::new());
    let supervisor = TemporarySupervisor::with_clock(backend, clock.clone(), TEST_LOG_BUFFER_LINES);

    let invalid = supervisor
        .run(temporary_spec(
            "too long",
            MAX_TIMEOUT + Duration::from_secs(1),
        ))
        .await;
    assert!(matches!(
        invalid,
        Err(TemporaryError::InvalidTimeout { .. })
    ));

    let job = supervisor
        .run(temporary_spec("maximum", MAX_TIMEOUT))
        .await
        .unwrap();
    assert_eq!(job.timeout_seconds, 7 * 24 * 60 * 60);
    assert_eq!(
        job.deadline.unwrap() - job.started_at.unwrap(),
        chrono::Duration::days(7)
    );

    clock.move_wall_back(chrono::Duration::days(30));
    clock.advance_monotonic(MAX_TIMEOUT);
    wait_until_finished(&supervisor, &job.id).await;
    assert_eq!(
        supervisor.wait(&job.id).await.unwrap().status.state,
        TemporaryJobState::TimedOut
    );
}

#[tokio::test]
async fn completed_jobs_are_retained_for_exactly_one_hour_by_the_injected_clock() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_exit(Some(0));
    let clock = Arc::new(FakeClock::new());
    let supervisor = TemporarySupervisor::with_clock(backend, clock.clone(), TEST_LOG_BUFFER_LINES);
    let job = supervisor
        .run(temporary_spec("retained", Duration::from_secs(30)))
        .await
        .unwrap();
    supervisor.wait(&job.id).await.unwrap();

    clock.advance_monotonic(COMPLETED_RETENTION - Duration::from_nanos(1));
    assert!(supervisor.status(&job.id).await.is_some());
    clock.advance_monotonic(Duration::from_nanos(1));
    tokio::time::timeout(Duration::from_secs(1), async {
        while supervisor.status(&job.id).await.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("job remained retained at the exact one-hour boundary");
    assert!(matches!(
        supervisor.wait(&job.id).await,
        Err(TemporaryError::NotFound { .. })
    ));
}

#[tokio::test]
async fn wait_retrieves_a_short_job_that_finished_before_wait_was_called() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_exit(Some(0));
    let supervisor = TemporarySupervisor::new(backend, TEST_LOG_BUFFER_LINES);
    let job = supervisor
        .run(temporary_spec("already done", Duration::from_secs(30)))
        .await
        .unwrap();
    wait_until_finished(&supervisor, &job.id).await;

    let result = tokio::time::timeout(Duration::from_millis(100), supervisor.wait(&job.id))
        .await
        .expect("wait must not miss an earlier completion")
        .unwrap();
    assert_eq!(result.status.state, TemporaryJobState::Succeeded);
}

#[tokio::test]
async fn action_inherits_server_context_without_restart_or_config_mutation() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_exit(Some(17));
    let supervisor = TemporarySupervisor::new(backend.clone(), TEST_LOG_BUFFER_LINES);
    let config = ServerConfig {
        id: "srv_action".into(),
        name: "web".into(),
        command: "serve".into(),
        port: Some(4321),
        directory: Some("frontend".into()),
        env: HashMap::from([("APP_MODE".into(), "test".into())]),
        health_url: Some("health".into()),
        health_status: None,
        auto_restart: true,
        actions: vec![ServerAction::new("migrate", "run migrations")],
    };
    let original_config = config.clone();
    let runtime = RuntimeSpec {
        config,
        project_id: "prj_example".into(),
        project_name: "Example".into(),
        project_root: PathBuf::from(r"C:\projects\example"),
        settings: RuntimeSettings {
            preferred_shell: PreferredShell::Powershell7,
            ..RuntimeSettings::default()
        },
    };

    let job = supervisor
        .run_action(&runtime, "MIGRATE", Duration::from_secs(60))
        .await
        .unwrap();
    let result = supervisor.wait(&job.id).await.unwrap();
    assert_eq!(result.status.state, TemporaryJobState::Failed);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (requests, _, health_checks) = backend.snapshot();
    assert_eq!(requests.len(), 1, "actions must never auto-restart");
    assert_eq!(health_checks, 0, "actions do not run server health probes");
    assert_eq!(requests[0].command, "run migrations");
    assert_eq!(
        requests[0].cwd,
        PathBuf::from(r"C:\projects\example\frontend")
    );
    assert_eq!(requests[0].environment.get("APP_MODE").unwrap(), "test");
    assert_eq!(requests[0].port, Some(4321));
    assert_eq!(requests[0].server_name, "web");
    assert_eq!(requests[0].preferred_shell, PreferredShell::Powershell7);
    assert_eq!(
        runtime.config, original_config,
        "actions cannot mutate config"
    );
}

#[tokio::test]
async fn timeout_is_published_before_cleanup_and_wait_finishes_after_cleanup() {
    let backend = Arc::new(FakeBackend::default());
    let gate = Arc::new(StopGate::default());
    backend.queue_blocking_stop_with_output(gate.clone(), b"timeout cleanup complete\n".to_vec());
    let clock = Arc::new(FakeClock::new());
    let supervisor = TemporarySupervisor::with_clock(backend, clock.clone(), TEST_LOG_BUFFER_LINES);
    let job = supervisor
        .run(temporary_spec("blocking cleanup", Duration::from_secs(1)))
        .await
        .unwrap();

    clock.advance_monotonic(Duration::from_secs(1));
    tokio::time::timeout(Duration::from_secs(1), async {
        while !gate.started.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timeout cleanup did not start");

    let published = supervisor.status(&job.id).await.unwrap();
    assert_eq!(published.state, TemporaryJobState::TimedOut);
    assert_eq!(published.exit_code, Some(124));
    assert_eq!(published.pid, Some(42), "cleanup is still in progress");
    assert_eq!(published.finished_at, None);
    assert!(
        tokio::time::timeout(Duration::from_millis(25), supervisor.wait(&job.id))
            .await
            .is_err(),
        "wait returned before the process tree was reclaimed"
    );

    gate.release();
    let result = supervisor.wait(&job.id).await.unwrap();
    assert_eq!(result.status.state, TemporaryJobState::TimedOut);
    assert_eq!(result.status.pid, None);
    assert!(result.status.finished_at.is_some());
    assert_eq!(result.logs, ["timeout cleanup complete"]);
}

#[tokio::test]
async fn wait_returns_logs_from_a_short_job_that_finished_before_wait() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_job(
        Some(0),
        b"\x1b[31mstdout line\x1b[0m\r\nstderr line\n".to_vec(),
    );
    let supervisor = TemporarySupervisor::new(backend, TEST_LOG_BUFFER_LINES);
    let job = supervisor
        .run(temporary_spec("logged command", Duration::from_secs(30)))
        .await
        .unwrap();
    wait_until_finished(&supervisor, &job.id).await;

    let result = supervisor.wait(&job.id).await.unwrap();
    assert_eq!(result.status.state, TemporaryJobState::Succeeded);
    assert_eq!(result.logs, ["stdout line", "stderr line"]);
    assert_eq!(supervisor.logs(&job.id, 1).await.unwrap(), ["stderr line"]);
}

#[tokio::test]
async fn wait_error_recovery_retains_output_emitted_during_cleanup() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_wait_error_with_cleanup_output(b"recovery output\r\n".to_vec());
    let supervisor = TemporarySupervisor::new(backend.clone(), TEST_LOG_BUFFER_LINES);
    let job = supervisor
        .run(temporary_spec(
            "wait error recovery",
            Duration::from_secs(30),
        ))
        .await
        .unwrap();

    let result = supervisor.wait(&job.id).await.unwrap();

    assert_eq!(result.status.state, TemporaryJobState::Failed);
    assert_eq!(result.status.exit_code, Some(1));
    assert_eq!(result.logs, ["recovery output"]);
    assert!(
        result
            .status
            .error
            .as_deref()
            .is_some_and(|error| error.contains("expected wait failure"))
    );
    let error = result.status.error.as_deref().unwrap();
    assert!(error.contains("process cleanup failed: runtime backend: expected cleanup failure"));
    assert!(error.contains("runtime backend: expected output failure"));
    assert_eq!(backend.snapshot().1, 1, "recovery did not stop the process");
}

#[tokio::test]
async fn completed_job_is_evicted_in_background_at_the_exact_retention_boundary() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_exit(Some(0));
    let clock = Arc::new(FakeClock::new());
    let supervisor = TemporarySupervisor::with_clock(backend, clock.clone(), TEST_LOG_BUFFER_LINES);
    let job = supervisor
        .run(temporary_spec(
            "proactive eviction",
            Duration::from_secs(30),
        ))
        .await
        .unwrap();
    supervisor.wait(&job.id).await.unwrap();
    assert_eq!(supervisor.retained_job_count().await, 1);

    clock.advance_monotonic(COMPLETED_RETENTION - Duration::from_nanos(1));
    tokio::task::yield_now().await;
    assert_eq!(supervisor.retained_job_count().await, 1);

    clock.advance_monotonic(Duration::from_nanos(1));
    tokio::time::timeout(Duration::from_secs(1), async {
        while supervisor.retained_job_count().await != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background eviction did not remove the completed job");
}

#[tokio::test]
async fn dropping_the_supervisor_cancels_pending_background_eviction() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_exit(Some(0));
    let clock = Arc::new(FakeClock::new());
    let supervisor = TemporarySupervisor::with_clock(backend, clock.clone(), TEST_LOG_BUFFER_LINES);
    let job = supervisor
        .run(temporary_spec("shutdown eviction", Duration::from_secs(30)))
        .await
        .unwrap();
    supervisor.wait(&job.id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while clock.active_sleeps() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("eviction sleep did not start");

    drop(supervisor);
    tokio::time::timeout(Duration::from_secs(1), async {
        while clock.active_sleeps() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("eviction sleep survived supervisor shutdown");
}

#[tokio::test]
async fn manual_stop_captures_output_emitted_during_cleanup() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_cleanup_output(b"cleanup complete\r\n".to_vec());
    let supervisor = TemporarySupervisor::new(backend, TEST_LOG_BUFFER_LINES);
    let job = supervisor
        .run(temporary_spec("cleanup logger", Duration::from_secs(30)))
        .await
        .unwrap();

    supervisor.stop(&job.id).await.unwrap();
    let result = supervisor.wait(&job.id).await.unwrap();

    assert_eq!(result.logs, ["cleanup complete"]);
}

#[tokio::test]
async fn an_exit_observed_at_the_deadline_wins_over_timeout() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_exit(Some(0));
    let clock = Arc::new(FakeClock::new());
    let supervisor = TemporarySupervisor::with_clock(backend, clock.clone(), TEST_LOG_BUFFER_LINES);
    let job = supervisor
        .run(temporary_spec("boundary exit", Duration::from_secs(1)))
        .await
        .unwrap();

    clock.advance_monotonic(Duration::from_secs(1));
    let result = supervisor.wait(&job.id).await.unwrap();

    assert_eq!(result.status.state, TemporaryJobState::Succeeded);
    assert_eq!(result.status.exit_code, Some(0));
}

#[tokio::test]
async fn simultaneous_stops_share_one_retained_result_and_stop_once() {
    let backend = Arc::new(FakeBackend::default());
    let gate = Arc::new(StopGate::default());
    backend.queue_blocking_stop(gate.clone());
    let supervisor = TemporarySupervisor::new(backend.clone(), TEST_LOG_BUFFER_LINES);
    let job = supervisor
        .run(temporary_spec("concurrent stop", Duration::from_secs(30)))
        .await
        .unwrap();

    let stops = (0..8)
        .map(|_| {
            let supervisor = supervisor.clone();
            let id = job.id.clone();
            tokio::spawn(async move { supervisor.stop(&id).await })
        })
        .collect::<Vec<_>>();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !gate.started.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first stop did not begin");
    gate.release();

    for stop in stops {
        let status = stop.await.unwrap().expect("stop shares final status");
        assert_eq!(status.state, TemporaryJobState::Stopped);
        assert_eq!(status.exit_code, Some(130));
    }
    assert_eq!(backend.snapshot().1, 1, "process stopped more than once");
}

#[tokio::test]
async fn stop_during_timeout_cleanup_waits_for_the_retained_final_status() {
    let backend = Arc::new(FakeBackend::default());
    let gate = Arc::new(StopGate::default());
    backend.queue_blocking_stop(gate.clone());
    let clock = Arc::new(FakeClock::new());
    let supervisor = TemporarySupervisor::with_clock(backend, clock.clone(), TEST_LOG_BUFFER_LINES);
    let job = supervisor
        .run(temporary_spec("timeout stop race", Duration::from_secs(1)))
        .await
        .unwrap();
    clock.advance_monotonic(Duration::from_secs(1));
    tokio::time::timeout(Duration::from_secs(1), async {
        while !gate.started.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timeout cleanup did not begin");

    let stop = tokio::spawn({
        let supervisor = supervisor.clone();
        let id = job.id.clone();
        async move { supervisor.stop(&id).await }
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    let returned_interim_status = stop.is_finished();
    gate.release();
    let status = stop.await.unwrap().unwrap();
    assert!(
        !returned_interim_status,
        "stop returned an interim timeout status"
    );
    assert_eq!(status.state, TemporaryJobState::TimedOut);
    assert_eq!(status.pid, None);
    assert!(status.finished_at.is_some());
}

#[tokio::test]
async fn list_preserves_job_creation_order() {
    let backend = Arc::new(FakeBackend::default());
    let supervisor = TemporarySupervisor::new(backend, TEST_LOG_BUFFER_LINES);
    let mut expected = Vec::new();
    for index in 0..20 {
        let job = supervisor
            .run(temporary_spec(
                &format!("ordered job {index}"),
                Duration::from_secs(30),
            ))
            .await
            .unwrap();
        expected.push(job.id);
    }

    let actual = supervisor
        .list()
        .await
        .into_iter()
        .map(|status| status.id)
        .collect::<Vec<_>>();

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn temporary_logs_use_the_configured_log_buffer_line_limit() {
    let backend = Arc::new(FakeBackend::default());
    let output = (0..501)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    backend.queue_job(Some(0), output.into_bytes());
    let supervisor = TemporarySupervisor::new(backend, 500);
    let job = supervisor
        .run(temporary_spec("bounded logs", Duration::from_secs(30)))
        .await
        .unwrap();

    let result = supervisor.wait(&job.id).await.unwrap();

    assert_eq!(result.logs.len(), 500);
    assert_eq!(result.logs.first().map(String::as_str), Some("line 1"));
    assert_eq!(result.logs.last().map(String::as_str), Some("line 500"));
}

#[tokio::test]
async fn completed_retained_history_never_blocks_a_new_temporary_job() {
    let backend = Arc::new(FakeBackend::default());
    let supervisor = TemporarySupervisor::new(backend.clone(), TEST_LOG_BUFFER_LINES);
    for index in 0..128 {
        backend.queue_exit(Some(0));
        let job = supervisor
            .run(temporary_spec(
                &format!("retained job {index}"),
                Duration::from_secs(30),
            ))
            .await
            .unwrap();
        supervisor.wait(&job.id).await.unwrap();
    }
    assert_eq!(supervisor.retained_job_count().await, 128);

    backend.queue_exit(Some(0));
    let next = supervisor
        .run(temporary_spec(
            "job after retained history",
            Duration::from_secs(30),
        ))
        .await
        .expect("completed retained history must not reject new work");
    let result = supervisor.wait(&next.id).await.unwrap();

    assert_eq!(result.status.state, TemporaryJobState::Succeeded);
    assert_eq!(supervisor.retained_job_count().await, 129);
}
