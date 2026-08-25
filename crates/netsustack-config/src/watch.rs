use std::{
    fs, io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use netsustack_domain::NetsuStackConfig;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;

use crate::{ConfigError, ConfigStore, ReloadOutcome};

pub const DEBOUNCE_DURATION: Duration = Duration::from_millis(350);
const DEBOUNCE_TICK: Duration = Duration::from_millis(20);
const SHARING_RETRY_DELAY: Duration = Duration::from_millis(25);
const SHARING_RETRY_LIMIT: usize = 4;
type ConfigReader = Arc<dyn Fn(&Path) -> std::io::Result<Vec<u8>> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWatchEvent {
    Reloaded(NetsuStackConfig),
    Invalid(ConfigWatchError),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigWatchError {
    #[error("configuration read failed ({kind:?}, OS code {raw_os_error:?}): {message}")]
    Read {
        kind: io::ErrorKind,
        raw_os_error: Option<i32>,
        message: String,
    },
    #[error("configuration content is invalid: {0}")]
    InvalidConfig(String),
    #[error("configuration watcher backend failed: {0}")]
    Backend(String),
}

#[derive(Debug)]
pub struct ConfigWatcher {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    pub fn spawn(store: ConfigStore) -> Result<(Self, Receiver<ConfigWatchEvent>), ConfigError> {
        Self::spawn_with_reader(store, Arc::new(|path: &Path| fs::read(path)))
    }

    fn spawn_with_reader(
        store: ConfigStore,
        reader: ConfigReader,
    ) -> Result<(Self, Receiver<ConfigWatchEvent>), ConfigError> {
        Self::spawn_with_reader_and_startup_hook(store, reader, None)
    }

    fn spawn_with_reader_and_startup_hook(
        store: ConfigStore,
        reader: ConfigReader,
        startup_hook: Option<Box<dyn FnOnce()>>,
    ) -> Result<(Self, Receiver<ConfigWatchEvent>), ConfigError> {
        let watched_path = store.paths().config_file();
        let watched_root = store.paths().root().to_owned();
        let (raw_sender, raw_receiver) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(
            move |event| {
                let _ = raw_sender.send(event);
            },
            Config::default(),
        )
        .map_err(|error| ConfigError::Watch(error.to_string()))?;
        watcher
            .watch(&watched_root, RecursiveMode::NonRecursive)
            .map_err(|error| ConfigError::Watch(error.to_string()))?;
        if let Some(startup_hook) = startup_hook {
            startup_hook();
        }
        let baseline = store.read_and_acknowledge_watcher_baseline(|path| {
            read_with_sharing_retry(reader.as_ref(), path)
        });
        let (mut observed, startup_update, startup_pending) = match baseline {
            Ok((bytes, update)) => (bytes, update, false),
            Err(ConfigError::Io(_) | ConfigError::Json(_) | ConfigError::Validation(_)) => {
                (Vec::new(), None, true)
            }
            Err(error) => return Err(error),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let (sender, receiver) = mpsc::channel();
        if let Some(config) = startup_update {
            sender
                .send(ConfigWatchEvent::Reloaded(config))
                .map_err(|error| ConfigError::Watch(error.to_string()))?;
        }
        let thread = thread::Builder::new()
            .name("netsustack-config-watch".to_owned())
            .spawn(move || {
                let mut pending_since = startup_pending.then(Instant::now);
                let mut last_visible_error = None;
                while !thread_stop.load(Ordering::Acquire) {
                    match raw_receiver.recv_timeout(DEBOUNCE_TICK) {
                        Ok(Ok(event)) if touches_config(&event, &watched_path) => {
                            pending_since = Some(Instant::now());
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => {
                            if sender
                                .send(ConfigWatchEvent::Invalid(ConfigWatchError::Backend(
                                    error.to_string(),
                                )))
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }

                    if pending_since.is_none_or(|since| since.elapsed() < DEBOUNCE_DURATION) {
                        continue;
                    }
                    let previous = observed.clone();
                    let reload = store.reload_external_with_reader(|path| {
                        read_with_sharing_retry(reader.as_ref(), path)
                    });
                    pending_since = None;
                    let event = match reload {
                        Ok((outcome, bytes)) => {
                            last_visible_error = None;
                            observed = bytes;
                            if observed == previous {
                                None
                            } else {
                                match outcome {
                                    ReloadOutcome::Updated(config) => {
                                        Some(ConfigWatchEvent::Reloaded(config))
                                    }
                                    ReloadOutcome::IgnoredInternal => None,
                                }
                            }
                        }
                        Err(error) => {
                            let visible_error = visible_watch_error(error);
                            if last_visible_error.as_ref() == Some(&visible_error) {
                                None
                            } else {
                                last_visible_error = Some(visible_error.clone());
                                Some(ConfigWatchEvent::Invalid(visible_error))
                            }
                        }
                    };
                    if event.is_some_and(|event| sender.send(event).is_err()) {
                        break;
                    }
                }
            })
            .map_err(|error| ConfigError::Watch(error.to_string()))?;

        Ok((
            Self {
                stop,
                thread: Some(thread),
                _watcher: watcher,
            },
            receiver,
        ))
    }
}

fn visible_watch_error(error: ConfigError) -> ConfigWatchError {
    match error {
        ConfigError::Io(error) => ConfigWatchError::Read {
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
            message: error.to_string(),
        },
        error @ (ConfigError::Json(_) | ConfigError::Validation(_)) => {
            ConfigWatchError::InvalidConfig(error.to_string())
        }
        error => ConfigWatchError::Backend(error.to_string()),
    }
}

fn read_with_sharing_retry(
    reader: &(dyn Fn(&Path) -> io::Result<Vec<u8>> + Send + Sync),
    path: &Path,
) -> io::Result<Vec<u8>> {
    for attempt in 0..=SHARING_RETRY_LIMIT {
        match reader(path) {
            Err(error) if is_windows_sharing_violation(&error) && attempt < SHARING_RETRY_LIMIT => {
                thread::sleep(SHARING_RETRY_DELAY);
            }
            result => return result,
        }
    }
    Err(io::Error::other("sharing retry loop exhausted"))
}

fn is_windows_sharing_violation(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(32 | 33))
}

fn touches_config(event: &Event, config_path: &Path) -> bool {
    event.paths.iter().any(|path| path == config_path)
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    use netsustack_domain::NetsuStackConfig;
    use tempfile::TempDir;

    use super::{ConfigWatchError, ConfigWatchEvent, ConfigWatcher, DEBOUNCE_DURATION};
    use crate::{ConfigPaths, ConfigStore};

    #[test]
    fn transient_windows_sharing_violations_are_retried_until_reload_succeeds() {
        let (_temp, store) = test_store();
        let calls = Arc::new(AtomicUsize::new(0));
        let reader = Arc::new({
            let calls = Arc::clone(&calls);
            move |path: &Path| match calls.fetch_add(1, Ordering::SeqCst) {
                1 | 2 => Err(io::Error::from_raw_os_error(32)),
                _ => fs::read(path),
            }
        });
        let (_watcher, events) = ConfigWatcher::spawn_with_reader(store.clone(), reader)
            .expect("watcher starts with injected reader");
        let external = config_with_port(8810);
        fs::write(
            store.paths().config_file(),
            serde_json::to_vec(&external).expect("external config encodes"),
        )
        .expect("external config written");

        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("reload after transient sharing violations"),
            ConfigWatchEvent::Reloaded(external.clone())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert_eq!(store.snapshot().expect("snapshot"), external);
    }

    #[test]
    fn edit_in_startup_registration_gap_is_observed_and_converges() {
        let (_temp, store) = test_store();
        let external = config_with_port(8809);
        let config_path = store.paths().config_file();
        let external_for_hook = external.clone();
        let (_watcher, events) = ConfigWatcher::spawn_with_reader_and_startup_hook(
            store.clone(),
            Arc::new(|path: &Path| fs::read(path)),
            Some(Box::new(move || {
                fs::write(
                    config_path,
                    serde_json::to_vec(&external_for_hook).expect("external config encodes"),
                )
                .expect("startup-gap edit written");
            })),
        )
        .expect("watcher starts");

        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("startup-gap edit is reported"),
            ConfigWatchEvent::Reloaded(external.clone())
        );
        assert_eq!(store.snapshot().expect("converged snapshot"), external);
    }

    #[test]
    fn unchanged_startup_does_not_emit_a_duplicate_reload() {
        let (_temp, store) = test_store();
        let before = store.snapshot().expect("initial snapshot");
        let (_watcher, events) = ConfigWatcher::spawn_with_reader_and_startup_hook(
            store.clone(),
            Arc::new(|path: &Path| fs::read(path)),
            Some(Box::new(|| {})),
        )
        .expect("watcher starts");

        assert!(
            events.recv_timeout(Duration::from_millis(700)).is_err(),
            "unchanged startup must not emit a reload"
        );
        assert_eq!(store.snapshot().expect("unchanged snapshot"), before);
    }

    #[test]
    fn partial_json_in_startup_gap_is_debounced_once_then_recovers() {
        let (_temp, store) = test_store();
        let before = store.snapshot().expect("initial snapshot");
        let config_path = store.paths().config_file();
        let started = Instant::now();
        let (_watcher, events) = ConfigWatcher::spawn_with_reader_and_startup_hook(
            store.clone(),
            Arc::new(|path: &Path| fs::read(path)),
            Some(Box::new(move || {
                fs::write(config_path, b"{").expect("partial startup edit written");
            })),
        )
        .expect("watcher survives partial startup edit");

        assert!(
            events
                .recv_timeout(DEBOUNCE_DURATION - Duration::from_millis(75))
                .is_err(),
            "startup error must not bypass debounce"
        );
        assert!(matches!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("partial JSON becomes visible"),
            ConfigWatchEvent::Invalid(ConfigWatchError::InvalidConfig(_))
        ));
        assert!(started.elapsed() >= DEBOUNCE_DURATION);
        assert_eq!(store.snapshot().expect("last-known-good snapshot"), before);
        assert!(
            events.recv_timeout(DEBOUNCE_DURATION * 2).is_err(),
            "stable invalid content emits exactly one error"
        );

        let valid = config_with_port(8814);
        fs::write(
            store.paths().config_file(),
            serde_json::to_vec(&valid).expect("valid config encodes"),
        )
        .expect("valid recovery written");
        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("valid recovery reloads"),
            ConfigWatchEvent::Reloaded(valid.clone())
        );
        assert_eq!(store.snapshot().expect("converged snapshot"), valid);
    }

    #[test]
    fn missing_file_in_startup_gap_is_debounced_once_then_recovers() {
        let (_temp, store) = test_store();
        let before = store.snapshot().expect("initial snapshot");
        let config_path = store.paths().config_file();
        let started = Instant::now();
        let (_watcher, events) = ConfigWatcher::spawn_with_reader_and_startup_hook(
            store.clone(),
            Arc::new(|path: &Path| fs::read(path)),
            Some(Box::new(move || {
                fs::remove_file(config_path).expect("startup config removed");
            })),
        )
        .expect("watcher survives missing startup file");

        assert!(
            events
                .recv_timeout(DEBOUNCE_DURATION - Duration::from_millis(75))
                .is_err(),
            "startup read error must not bypass debounce"
        );
        assert!(matches!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("missing file becomes visible"),
            ConfigWatchEvent::Invalid(ConfigWatchError::Read {
                kind: io::ErrorKind::NotFound,
                ..
            })
        ));
        assert!(started.elapsed() >= DEBOUNCE_DURATION);
        assert_eq!(store.snapshot().expect("last-known-good snapshot"), before);
        assert!(
            events.recv_timeout(DEBOUNCE_DURATION * 2).is_err(),
            "stable missing state emits exactly one error"
        );

        let valid = config_with_port(8815);
        fs::write(
            store.paths().config_file(),
            serde_json::to_vec(&valid).expect("valid config encodes"),
        )
        .expect("valid recovery written");
        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("valid recovery reloads"),
            ConfigWatchEvent::Reloaded(valid.clone())
        );
        assert_eq!(store.snapshot().expect("converged snapshot"), valid);
    }

    #[test]
    fn deleted_config_emits_visible_read_error_then_later_valid_edit_converges() {
        let (_temp, store) = test_store();
        let before = store.snapshot().expect("initial snapshot");
        let (_watcher, events) = ConfigWatcher::spawn(store.clone()).expect("watcher starts");
        fs::remove_file(store.paths().config_file()).expect("config deleted");

        let error = events
            .recv_timeout(Duration::from_secs(2))
            .expect("visible deletion error");

        assert!(matches!(
            error,
            ConfigWatchEvent::Invalid(ConfigWatchError::Read {
                kind: io::ErrorKind::NotFound,
                ..
            })
        ));
        assert_eq!(store.snapshot().expect("last-known-good snapshot"), before);
        let external = config_with_port(8811);
        fs::write(
            store.paths().config_file(),
            serde_json::to_vec(&external).expect("external config encodes"),
        )
        .expect("valid config restored");
        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("valid reload after deletion"),
            ConfigWatchEvent::Reloaded(external.clone())
        );
        assert_eq!(store.snapshot().expect("converged snapshot"), external);
    }

    #[test]
    fn access_denial_is_visible_until_a_later_valid_edit_converges() {
        let (_temp, store) = test_store();
        let before = store.snapshot().expect("initial snapshot");
        let deny_reads = Arc::new(AtomicBool::new(false));
        let reader = Arc::new({
            let deny_reads = Arc::clone(&deny_reads);
            move |path: &Path| {
                if deny_reads.load(Ordering::SeqCst) {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected access denial",
                    ))
                } else {
                    fs::read(path)
                }
            }
        });
        let (_watcher, events) = ConfigWatcher::spawn_with_reader(store.clone(), reader)
            .expect("watcher starts with injected reader");
        deny_reads.store(true, Ordering::SeqCst);
        let denied = config_with_port(8812);
        fs::write(
            store.paths().config_file(),
            serde_json::to_vec(&denied).expect("denied config encodes"),
        )
        .expect("edit triggering denied read written");

        assert!(matches!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("visible access error"),
            ConfigWatchEvent::Invalid(ConfigWatchError::Read {
                kind: io::ErrorKind::PermissionDenied,
                ..
            })
        ));
        assert_eq!(store.snapshot().expect("last-known-good snapshot"), before);

        deny_reads.store(false, Ordering::SeqCst);
        let valid = config_with_port(8813);
        fs::write(
            store.paths().config_file(),
            serde_json::to_vec(&valid).expect("valid config encodes"),
        )
        .expect("valid edit written");
        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("valid reload after access denial"),
            ConfigWatchEvent::Reloaded(valid.clone())
        );
        assert_eq!(store.snapshot().expect("converged snapshot"), valid);
    }

    fn test_store() -> (TempDir, ConfigStore) {
        let temp = TempDir::new().expect("temporary user profile");
        let paths = ConfigPaths::from_user_profile(temp.path());
        let store = ConfigStore::open(paths).expect("store opens");
        (temp, store)
    }

    fn config_with_port(api_port: u16) -> NetsuStackConfig {
        NetsuStackConfig {
            api_port,
            ..NetsuStackConfig::default()
        }
    }
}
