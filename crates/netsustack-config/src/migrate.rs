use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use netsustack_domain::{ConfigValidationError, NetsuStackConfig};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug)]
pub(crate) struct Migration {
    pub(crate) config: NetsuStackConfig,
    pub(crate) changed: bool,
}

#[derive(Debug, Error)]
pub(crate) enum MigrationError {
    #[error("configuration JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("configuration validation failed: {0}")]
    Validation(#[from] ConfigValidationError),
    #[error("configuration version is missing or is not an unsigned integer")]
    MissingVersion,
}

pub(crate) fn decode_and_migrate(bytes: &[u8]) -> Result<Migration, MigrationError> {
    let mut value: Value = serde_json::from_slice(bytes)?;
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or(MigrationError::MissingVersion)?;
    let changed = version == 0;
    if changed {
        value["version"] = Value::from(1_u64);
    }

    let config: NetsuStackConfig = serde_json::from_value(value)?;
    config.validate()?;
    Ok(Migration { config, changed })
}

pub(crate) fn next_backup_path(config_path: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    config_path.with_file_name(format!("config.backup-{stamp:020}.json"))
}
