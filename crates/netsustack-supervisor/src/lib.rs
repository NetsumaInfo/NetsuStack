//! Application service and process-supervision boundaries.

pub mod backoff;
pub mod health;
pub mod logs;
pub mod runtime;
pub mod takeover;
pub mod temporary;
mod windows_backend;
