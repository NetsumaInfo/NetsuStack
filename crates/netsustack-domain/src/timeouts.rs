use thiserror::Error;

pub const DEFAULT_TIMEOUT_SECONDS: u64 = 30 * 60;
pub const MAXIMUM_TIMEOUT_SECONDS: u64 = 7 * 24 * 60 * 60;

pub fn parse_timeout(raw: &str) -> Result<u64, TimeoutParseError> {
    let normalized = raw.trim().to_lowercase();
    if normalized.is_empty() {
        return Err(TimeoutParseError::Empty);
    }

    let (number, multiplier) = match normalized.as_bytes().last() {
        Some(b's') => (&normalized[..normalized.len() - 1], 1.0),
        Some(b'm') => (&normalized[..normalized.len() - 1], 60.0),
        Some(b'h') => (&normalized[..normalized.len() - 1], 3_600.0),
        Some(byte) if byte.is_ascii_alphabetic() => {
            return Err(TimeoutParseError::UnsupportedUnit);
        }
        Some(_) => (normalized.as_str(), 1.0),
        None => return Err(TimeoutParseError::Empty),
    };
    let amount = number
        .parse::<f64>()
        .map_err(|_| TimeoutParseError::InvalidNumber)?;
    let seconds = (amount * multiplier).ceil();
    if !seconds.is_finite() || seconds < 1.0 || seconds > MAXIMUM_TIMEOUT_SECONDS as f64 {
        return Err(TimeoutParseError::OutOfRange);
    }
    Ok(seconds as u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TimeoutParseError {
    #[error("timeout is empty")]
    Empty,
    #[error("timeout has an invalid number")]
    InvalidNumber,
    #[error("timeout unit must be s, m, or h")]
    UnsupportedUnit,
    #[error("timeout must round to 1-604800 seconds")]
    OutOfRange,
}
