use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use netsustack_domain::{ConfigValidationError, NetsuStackConfig};
use serde_json::Value;
use thiserror::Error;

use crate::{
    ConfigPaths,
    migrate::{decode_and_migrate, next_backup_path},
};

static STAGING_NONCE: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionOperation {
    Write,
}

#[cfg(test)]
type TransactionHook = Arc<dyn Fn(TransactionOperation) + Send + Sync>;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("configuration JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("configuration validation failed: {0}")]
    Validation(#[from] ConfigValidationError),
    #[error("configuration migration failed: {0}")]
    Migration(String),
    #[cfg(windows)]
    #[error("Windows configuration operation failed: {0}")]
    Windows(#[from] windows::core::Error),
    #[error("configuration state lock is poisoned")]
    StatePoisoned,
    #[error("configuration watcher failed: {0}")]
    Watch(String),
}

struct StoreState {
    // Lock order for mutations: transaction -> current -> last_internal_bytes.
    transaction: Mutex<()>,
    current: RwLock<NetsuStackConfig>,
    last_internal_bytes: Mutex<Option<Vec<u8>>>,
    #[cfg(test)]
    transaction_hook: Mutex<Option<TransactionHook>>,
}

#[derive(Clone)]
pub struct ConfigStore {
    paths: ConfigPaths,
    state: Arc<StoreState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadOutcome {
    Updated(NetsuStackConfig),
    IgnoredInternal,
}

impl ConfigStore {
    pub fn open(paths: ConfigPaths) -> Result<Self, ConfigError> {
        fs::create_dir_all(paths.logs_dir())?;
        let config_path = paths.config_file();
        if config_path.exists() {
            let bytes = fs::read(&config_path)?;
            let migration = decode_and_migrate(&bytes)
                .map_err(|error| ConfigError::Migration(error.to_string()))?;
            if migration.changed {
                atomic_write(&next_backup_path(&config_path), &bytes)?;
                let migrated_bytes = encode_config(&migration.config)?;
                atomic_write(&config_path, &migrated_bytes)?;
            }
            return Ok(Self {
                paths,
                state: Arc::new(StoreState {
                    transaction: Mutex::new(()),
                    current: RwLock::new(migration.config),
                    last_internal_bytes: Mutex::new(None),
                    #[cfg(test)]
                    transaction_hook: Mutex::new(None),
                }),
            });
        }

        let config = NetsuStackConfig::default();
        let store = Self {
            paths,
            state: Arc::new(StoreState {
                transaction: Mutex::new(()),
                current: RwLock::new(config.clone()),
                last_internal_bytes: Mutex::new(None),
                #[cfg(test)]
                transaction_hook: Mutex::new(None),
            }),
        };
        store.write(&config)?;
        Ok(store)
    }

    pub fn paths(&self) -> &ConfigPaths {
        &self.paths
    }

    pub fn snapshot(&self) -> Result<NetsuStackConfig, ConfigError> {
        self.state
            .current
            .read()
            .map(|config| config.clone())
            .map_err(|_| ConfigError::StatePoisoned)
    }

    pub fn write(&self, config: &NetsuStackConfig) -> Result<(), ConfigError> {
        config.validate()?;
        let bytes = encode_config(config)?;
        let _transaction = self
            .state
            .transaction
            .lock()
            .map_err(|_| ConfigError::StatePoisoned)?;
        atomic_write(&self.paths.config_file(), &bytes)?;
        #[cfg(test)]
        self.call_transaction_hook(TransactionOperation::Write)?;
        *self
            .state
            .current
            .write()
            .map_err(|_| ConfigError::StatePoisoned)? = config.clone();
        *self
            .state
            .last_internal_bytes
            .lock()
            .map_err(|_| ConfigError::StatePoisoned)? = Some(bytes);
        Ok(())
    }

    pub fn reload_external(&self) -> Result<ReloadOutcome, ConfigError> {
        self.reload_external_with_reader(|path| fs::read(path))
            .map(|(outcome, _bytes)| outcome)
    }

    pub(crate) fn reload_external_with_reader<F>(
        &self,
        reader: F,
    ) -> Result<(ReloadOutcome, Vec<u8>), ConfigError>
    where
        F: FnOnce(&Path) -> std::io::Result<Vec<u8>>,
    {
        let _transaction = self
            .state
            .transaction
            .lock()
            .map_err(|_| ConfigError::StatePoisoned)?;
        let bytes = match reader(&self.paths.config_file()) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.state
                    .last_internal_bytes
                    .lock()
                    .map_err(|_| ConfigError::StatePoisoned)?
                    .take();
                return Err(error.into());
            }
        };
        let mut current = self
            .state
            .current
            .write()
            .map_err(|_| ConfigError::StatePoisoned)?;
        let mut last_internal = self
            .state
            .last_internal_bytes
            .lock()
            .map_err(|_| ConfigError::StatePoisoned)?;
        if last_internal.take().as_deref() == Some(bytes.as_slice()) {
            return Ok((ReloadOutcome::IgnoredInternal, bytes));
        }
        let config = decode_config(&bytes)?;
        *current = config.clone();
        Ok((ReloadOutcome::Updated(config), bytes))
    }

    pub(crate) fn read_and_acknowledge_watcher_baseline<F>(
        &self,
        reader: F,
    ) -> Result<(Vec<u8>, Option<NetsuStackConfig>), ConfigError>
    where
        F: FnOnce(&Path) -> std::io::Result<Vec<u8>>,
    {
        let _transaction = self
            .state
            .transaction
            .lock()
            .map_err(|_| ConfigError::StatePoisoned)?;
        let bytes = match reader(&self.paths.config_file()) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.state
                    .last_internal_bytes
                    .lock()
                    .map_err(|_| ConfigError::StatePoisoned)?
                    .take();
                return Err(error.into());
            }
        };
        let mut current = self
            .state
            .current
            .write()
            .map_err(|_| ConfigError::StatePoisoned)?;
        let mut last_internal = self
            .state
            .last_internal_bytes
            .lock()
            .map_err(|_| ConfigError::StatePoisoned)?;
        let updated = if last_internal.take().as_deref() == Some(bytes.as_slice()) {
            None
        } else {
            let config = decode_config(&bytes)?;
            let changed = (*current != config).then(|| config.clone());
            *current = config;
            changed
        };
        Ok((bytes, updated))
    }

    #[cfg(test)]
    fn set_transaction_hook(&self, hook: TransactionHook) -> Result<(), ConfigError> {
        *self
            .state
            .transaction_hook
            .lock()
            .map_err(|_| ConfigError::StatePoisoned)? = Some(hook);
        Ok(())
    }

    #[cfg(test)]
    fn call_transaction_hook(&self, operation: TransactionOperation) -> Result<(), ConfigError> {
        let hook = self
            .state
            .transaction_hook
            .lock()
            .map_err(|_| ConfigError::StatePoisoned)?
            .clone();
        if let Some(hook) = hook {
            hook(operation);
        }
        Ok(())
    }
}

pub(crate) fn decode_config(bytes: &[u8]) -> Result<NetsuStackConfig, ConfigError> {
    let config: NetsuStackConfig = serde_json::from_slice(bytes)?;
    config.validate()?;
    Ok(config)
}

fn encode_config(config: &NetsuStackConfig) -> Result<Vec<u8>, ConfigError> {
    let mut value = serde_json::to_value(config)?;
    sort_json(&mut value);
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn sort_json(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let mut entries = std::mem::take(object).into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            for (key, mut value) in entries {
                sort_json(&mut value);
                object.insert(key, value);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(sort_json),
        _ => {}
    }
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let staging = staging_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        replace_file(&staging, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
}

fn staging_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.json");
    let nonce = STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.{}.{nonce}.tmp", std::process::id()))
}

#[cfg(windows)]
fn replace_file(staging: &Path, target: &Path) -> Result<(), ConfigError> {
    use std::{iter, os::windows::ffi::OsStrExt};

    use windows::{
        Win32::Storage::FileSystem::{REPLACE_FILE_FLAGS, ReplaceFileW},
        core::PCWSTR,
    };

    if !target.exists() {
        fs::rename(staging, target)?;
        return Ok(());
    }

    let staging_wide = staging
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: Both UTF-16 buffers are nul-terminated and live for the duration of the call.
    unsafe {
        ReplaceFileW(
            PCWSTR(target_wide.as_ptr()),
            PCWSTR(staging_wide.as_ptr()),
            PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(staging: &Path, target: &Path) -> Result<(), ConfigError> {
    fs::rename(staging, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };

    use netsustack_domain::NetsuStackConfig;
    use tempfile::TempDir;

    use super::{ConfigStore, ReloadOutcome, TransactionOperation};
    use crate::ConfigPaths;

    #[test]
    fn concurrent_writes_serialize_disk_snapshot_and_suppression_state() {
        let (_temp, store) = test_store();
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let release_receiver = Arc::new(Mutex::new(release_receiver));
        let calls = Arc::new(AtomicUsize::new(0));
        store
            .set_transaction_hook(Arc::new({
                let calls = Arc::clone(&calls);
                let release_receiver = Arc::clone(&release_receiver);
                move |operation| {
                    if operation == TransactionOperation::Write
                        && calls.fetch_add(1, Ordering::SeqCst) == 0
                    {
                        entered_sender
                            .send(())
                            .expect("first write announces pause");
                        release_receiver
                            .lock()
                            .expect("release receiver lock")
                            .recv()
                            .expect("first write released");
                    }
                }
            }))
            .expect("test hook installed");
        let first = config_with_port(8801);
        let second = config_with_port(8802);
        let first_store = store.clone();
        let first_thread = thread::spawn(move || first_store.write(&first));
        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("first write reaches post-disk pause");
        let second_store = store.clone();
        let second_expected = second.clone();
        let (done_sender, done_receiver) = mpsc::channel();
        let second_thread = thread::spawn(move || {
            let result = second_store.write(&second);
            done_sender.send(()).expect("second completion announced");
            result
        });

        let second_was_serialized = done_receiver
            .recv_timeout(Duration::from_millis(150))
            .is_err();
        release_sender.send(()).expect("release first write");
        first_thread
            .join()
            .expect("first thread joins")
            .expect("first write");
        second_thread
            .join()
            .expect("second thread joins")
            .expect("second write");

        assert!(
            second_was_serialized,
            "second write bypassed active transaction"
        );
        assert_eq!(store.snapshot().expect("snapshot"), second_expected);
        assert_eq!(read_disk_config(&store), second_expected);
        assert_eq!(
            store.reload_external().expect("internal marker reload"),
            ReloadOutcome::IgnoredInternal
        );
    }

    #[test]
    fn concurrent_write_and_external_reload_converge_without_stale_suppression() {
        let (_temp, store) = test_store();
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let release_receiver = Arc::new(Mutex::new(release_receiver));
        store
            .set_transaction_hook(Arc::new(move |operation| {
                if operation == TransactionOperation::Write {
                    entered_sender.send(()).expect("write announces pause");
                    release_receiver
                        .lock()
                        .expect("release receiver lock")
                        .recv()
                        .expect("write released");
                }
            }))
            .expect("test hook installed");
        let internal = config_with_port(8803);
        let internal_store = store.clone();
        let internal_for_thread = internal.clone();
        let write_thread = thread::spawn(move || internal_store.write(&internal_for_thread));
        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("write reaches post-disk pause");
        let internal_bytes = fs::read(store.paths().config_file()).expect("internal bytes");
        let external = config_with_port(8804);
        fs::write(
            store.paths().config_file(),
            serde_json::to_vec(&external).expect("external config encodes"),
        )
        .expect("external edit written");
        let reload_store = store.clone();
        let (done_sender, done_receiver) = mpsc::channel();
        let reload_thread = thread::spawn(move || {
            let result = reload_store.reload_external();
            done_sender.send(()).expect("reload completion announced");
            result
        });

        let reload_was_serialized = done_receiver
            .recv_timeout(Duration::from_millis(150))
            .is_err();
        release_sender.send(()).expect("release write");
        write_thread
            .join()
            .expect("write thread joins")
            .expect("write");
        let reload = reload_thread
            .join()
            .expect("reload thread joins")
            .expect("reload");

        assert!(reload_was_serialized, "reload bypassed active transaction");
        assert_eq!(reload, ReloadOutcome::Updated(external.clone()));
        assert_eq!(store.snapshot().expect("snapshot"), external);
        assert_eq!(read_disk_config(&store), external);
        fs::write(store.paths().config_file(), internal_bytes).expect("matching external edit");
        assert_eq!(
            store.reload_external().expect("matching edit reload"),
            ReloadOutcome::Updated(internal)
        );
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

    fn read_disk_config(store: &ConfigStore) -> NetsuStackConfig {
        serde_json::from_slice(
            &fs::read(store.paths().config_file()).expect("disk config readable"),
        )
        .expect("disk config decodes")
    }
}
