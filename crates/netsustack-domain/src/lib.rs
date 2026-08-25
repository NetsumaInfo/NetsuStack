//! Shared domain contracts for every NetsuStack adapter.

pub mod config;
pub mod ids;
pub mod memory;
pub mod models;
pub mod timeouts;

pub use config::{
    ConfigValidationError, MAX_HEALTH_INTERVAL_SECONDS, MAX_HTTP_STATUS, MAX_LOG_BUFFER_LINES,
    MAX_LOG_FILE_MAX_MB, MAX_RESTART_ATTEMPTS, MIN_HEALTH_INTERVAL_SECONDS, MIN_HTTP_STATUS,
    MIN_LOG_BUFFER_LINES, MIN_LOG_FILE_MAX_MB, MIN_RESTART_ATTEMPTS, MemoryLimitMode,
    NetsuStackConfig, PreferredShell, Project, ResolvedServer, ServerAction, ServerConfig,
};
pub use ids::{
    is_project_id, is_server_id, is_temporary_id, new_project_id, new_server_id, new_temporary_id,
};
pub use memory::{
    MAXIMUM_MEMORY_LIMIT_BYTES, MINIMUM_MEMORY_LIMIT_BYTES, MemoryParseError, parse_memory_size,
};
pub use models::{
    NetsuStackStatus, PortOccupant, ProjectStatus, ServerState, ServerStatus, TemporaryJobState,
    TemporaryJobStatus,
};
pub use timeouts::{
    DEFAULT_TIMEOUT_SECONDS, MAXIMUM_TIMEOUT_SECONDS, TimeoutParseError, parse_timeout,
};
