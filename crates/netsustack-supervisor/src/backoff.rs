use std::time::Duration;

/// Exponential delay used for automatic server restarts.
pub struct RestartBackoff;

impl RestartBackoff {
    pub const HEALTHY_RESET_AFTER: Duration = Duration::from_secs(30);

    pub fn delay(attempt: u32) -> Duration {
        Duration::from_secs(1_u64.checked_shl(attempt.min(5)).unwrap_or(32).min(30))
    }
}
