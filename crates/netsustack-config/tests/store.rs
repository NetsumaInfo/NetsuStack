use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use netsustack_config::{
    ConfigPaths, ConfigStore, ConfigWatchEvent, ConfigWatcher, load_or_create_token,
    token_file_is_restricted_to_current_user,
};
use netsustack_domain::NetsuStackConfig;
use tempfile::TempDir;

#[test]
fn paths_are_derived_from_an_injected_user_profile() {
    let paths = ConfigPaths::from_user_profile(Path::new(r"C:\Users\Ada"));

    assert_eq!(paths.root(), Path::new(r"C:\Users\Ada\.config\netsustack"));
    assert_eq!(
        paths.config_file(),
        Path::new(r"C:\Users\Ada\.config\netsustack\config.json")
    );
    assert_eq!(
        paths.logs_dir(),
        Path::new(r"C:\Users\Ada\.config\netsustack\logs")
    );
    assert_eq!(
        paths.token_file(),
        Path::new(r"C:\Users\Ada\.config\netsustack\api-token")
    );
    assert_eq!(
        paths.resume_after_update_file(),
        Path::new(r"C:\Users\Ada\.config\netsustack\resume-after-update.json")
    );
}

fn test_paths(temp: &TempDir) -> ConfigPaths {
    ConfigPaths::from_user_profile(temp.path())
}

#[test]
fn opening_a_missing_store_creates_the_default_config_and_directories() {
    let temp = TempDir::new().expect("temporary user profile");
    let paths = test_paths(&temp);

    let store = ConfigStore::open(paths.clone()).expect("store opens");

    assert_eq!(
        store.snapshot().expect("snapshot"),
        NetsuStackConfig::default()
    );
    assert!(paths.config_file().is_file());
    assert!(paths.logs_dir().is_dir());
}

#[test]
fn store_writes_pretty_json_with_recursively_sorted_keys() {
    let temp = TempDir::new().expect("temporary user profile");
    let paths = test_paths(&temp);
    let store = ConfigStore::open(paths.clone()).expect("store opens");
    let config = NetsuStackConfig {
        api_port: 8811,
        ..NetsuStackConfig::default()
    };

    store.write(&config).expect("config write succeeds");

    let actual = fs::read_to_string(paths.config_file()).expect("config is readable");
    let expected = concat!(
        "{\n",
        "  \"apiPort\": 8811,\n",
        "  \"globalMemoryLimitBytes\": null,\n",
        "  \"healthIntervalSeconds\": 10,\n",
        "  \"logBufferLines\": 5000,\n",
        "  \"logFileMaxMB\": 10,\n",
        "  \"maxRestartAttempts\": 5,\n",
        "  \"preferredShell\": \"auto\",\n",
        "  \"projects\": [],\n",
        "  \"version\": 1\n",
        "}\n"
    );
    assert_eq!(actual, expected);
}

#[test]
fn atomic_write_leaves_no_staging_file_behind() {
    let temp = TempDir::new().expect("temporary user profile");
    let paths = test_paths(&temp);
    let store = ConfigStore::open(paths.clone()).expect("store opens");

    store
        .write(&NetsuStackConfig::default())
        .expect("config write succeeds");

    let mut entries = fs::read_dir(paths.root())
        .expect("config directory is readable")
        .map(|entry| entry.expect("directory entry").file_name())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(
        entries,
        vec!["config.json", "logs"],
        "unexpected staging artifact: {entries:?}"
    );
}

#[test]
fn opening_a_legacy_config_backs_it_up_before_migrating() {
    let temp = TempDir::new().expect("temporary user profile");
    let paths = test_paths(&temp);
    fs::create_dir_all(paths.root()).expect("config directory");
    let legacy = br#"{"version":0,"apiPort":8811,"projects":[]}"#;
    fs::write(paths.config_file(), legacy).expect("legacy config written");

    let store = ConfigStore::open(paths.clone()).expect("legacy config migrates");

    assert_eq!(store.snapshot().expect("snapshot").version, 1);
    let backups = fs::read_dir(paths.root())
        .expect("config directory is readable")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("config.backup-")
        })
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        fs::read(backups[0].path()).expect("backup readable"),
        legacy
    );
}

#[test]
fn invalid_external_edit_stays_on_disk_without_replacing_the_snapshot() {
    let temp = TempDir::new().expect("temporary user profile");
    let paths = test_paths(&temp);
    let store = ConfigStore::open(paths.clone()).expect("store opens");
    let before = store.snapshot().expect("initial snapshot");
    let invalid = b"{ definitely not JSON";
    fs::write(paths.config_file(), invalid).expect("external edit written");

    let error = store
        .reload_external()
        .expect_err("invalid external edit is rejected");

    assert!(error.to_string().contains("JSON"));
    assert_eq!(store.snapshot().expect("last-known-good snapshot"), before);
    assert_eq!(
        fs::read(paths.config_file()).expect("invalid file remains"),
        invalid
    );
}

#[test]
fn watcher_debounces_external_edits_for_350_milliseconds() {
    let temp = TempDir::new().expect("temporary user profile");
    let paths = test_paths(&temp);
    let store = ConfigStore::open(paths.clone()).expect("store opens");
    let (_watcher, events) = ConfigWatcher::spawn(store.clone()).expect("watcher starts");
    let first = NetsuStackConfig {
        api_port: 8801,
        ..NetsuStackConfig::default()
    };
    fs::write(
        paths.config_file(),
        serde_json::to_vec(&first).expect("first edit encodes"),
    )
    .expect("first edit written");
    std::thread::sleep(Duration::from_millis(100));
    let second = NetsuStackConfig {
        api_port: 8802,
        ..NetsuStackConfig::default()
    };
    let last_edit_at = Instant::now();
    fs::write(
        paths.config_file(),
        serde_json::to_vec(&second).expect("second edit encodes"),
    )
    .expect("second edit written");

    let event = events
        .recv_timeout(Duration::from_secs(2))
        .expect("debounced event arrives");

    assert!(last_edit_at.elapsed() >= Duration::from_millis(325));
    assert_eq!(event, ConfigWatchEvent::Reloaded(second));
    assert!(events.recv_timeout(Duration::from_millis(500)).is_err());
}

#[test]
fn watcher_excludes_internal_store_writes() {
    let temp = TempDir::new().expect("temporary user profile");
    let paths = test_paths(&temp);
    let store = ConfigStore::open(paths).expect("store opens");
    let (_watcher, events) = ConfigWatcher::spawn(store.clone()).expect("watcher starts");
    let config = NetsuStackConfig {
        api_port: 8803,
        ..NetsuStackConfig::default()
    };

    store.write(&config).expect("internal write succeeds");

    assert!(events.recv_timeout(Duration::from_millis(800)).is_err());
    assert_eq!(store.snapshot().expect("snapshot"), config);
}

#[test]
fn external_reload_consumes_stale_internal_marker_and_converges_on_later_matching_edit() {
    let temp = TempDir::new().expect("temporary user profile");
    let paths = test_paths(&temp);
    let store = ConfigStore::open(paths.clone()).expect("store opens");
    let (_watcher, events) = ConfigWatcher::spawn(store.clone()).expect("watcher starts");
    let internal = NetsuStackConfig {
        api_port: 8804,
        ..NetsuStackConfig::default()
    };
    store.write(&internal).expect("internal config written");
    let internal_bytes = fs::read(paths.config_file()).expect("internal bytes readable");
    let external = NetsuStackConfig {
        api_port: 8805,
        ..NetsuStackConfig::default()
    };
    fs::write(
        paths.config_file(),
        serde_json::to_vec(&external).expect("external config encodes"),
    )
    .expect("external config written");
    assert_eq!(
        events
            .recv_timeout(Duration::from_secs(2))
            .expect("external reload"),
        ConfigWatchEvent::Reloaded(external)
    );
    fs::write(paths.config_file(), &internal_bytes).expect("matching external edit written");

    assert_eq!(
        events
            .recv_timeout(Duration::from_secs(2))
            .expect("matching external edit reload"),
        ConfigWatchEvent::Reloaded(internal.clone())
    );
    assert_eq!(store.snapshot().expect("converged snapshot"), internal);
    assert_eq!(
        fs::read(paths.config_file()).expect("converged file readable"),
        internal_bytes
    );
}

#[test]
fn watcher_reports_invalid_external_edit_and_keeps_last_known_good_snapshot() {
    let temp = TempDir::new().expect("temporary user profile");
    let paths = test_paths(&temp);
    let store = ConfigStore::open(paths.clone()).expect("store opens");
    let before = store.snapshot().expect("initial snapshot");
    let (_watcher, events) = ConfigWatcher::spawn(store.clone()).expect("watcher starts");
    let invalid = b"not valid JSON";

    fs::write(paths.config_file(), invalid).expect("invalid edit written");

    assert!(matches!(
        events.recv_timeout(Duration::from_secs(2)),
        Ok(ConfigWatchEvent::Invalid(_))
    ));
    assert_eq!(store.snapshot().expect("last-known-good snapshot"), before);
    assert_eq!(
        fs::read(paths.config_file()).expect("invalid edit remains"),
        invalid
    );
}

#[test]
fn token_is_256_bits_and_persists_across_loads() {
    let temp = TempDir::new().expect("temporary user profile");
    let paths = test_paths(&temp);

    let first = load_or_create_token(&paths).expect("token is created");
    let second = load_or_create_token(&paths).expect("token is loaded");

    assert_eq!(first.as_str().len(), 64);
    assert!(first.as_str().bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(second, first);
    assert_eq!(
        fs::read_to_string(paths.token_file()).expect("token file is readable"),
        format!("{}\n", first.as_str())
    );
}

#[cfg(windows)]
#[test]
fn token_acl_is_restricted_to_the_current_user() {
    let temp = TempDir::new().expect("temporary user profile");
    let paths = test_paths(&temp);
    load_or_create_token(&paths).expect("token is created");

    assert!(
        token_file_is_restricted_to_current_user(&paths.token_file())
            .expect("token ACL is readable")
    );
}
