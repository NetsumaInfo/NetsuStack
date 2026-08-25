//! Configuration persistence and migration boundaries.

mod migrate;
mod paths;
mod store;
mod token;
mod watch;

pub use paths::ConfigPaths;
pub use store::{ConfigError, ConfigStore, ReloadOutcome};
pub use token::{
    ApiToken, TokenError, load_or_create_token, token_file_is_restricted_to_current_user,
};
pub use watch::{ConfigWatchError, ConfigWatchEvent, ConfigWatcher, DEBOUNCE_DURATION};
