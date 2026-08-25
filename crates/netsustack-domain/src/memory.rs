use thiserror::Error;

pub const MINIMUM_MEMORY_LIMIT_BYTES: u64 = 128 * 1_024 * 1_024;
pub const MAXIMUM_MEMORY_LIMIT_BYTES: u64 = 1_024 * 1_024 * 1_024 * 1_024;

pub fn parse_memory_size(raw: &str) -> Result<u64, MemoryParseError> {
    let normalized: String = raw
        .trim()
        .to_lowercase()
        .replace(',', ".")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if normalized.is_empty() {
        return Err(MemoryParseError::Empty);
    }

    let units = [
        ("tib", 1_099_511_627_776_f64),
        ("tb", 1_099_511_627_776_f64),
        ("to", 1_099_511_627_776_f64),
        ("gib", 1_073_741_824_f64),
        ("gb", 1_073_741_824_f64),
        ("go", 1_073_741_824_f64),
        ("mib", 1_048_576_f64),
        ("mb", 1_048_576_f64),
        ("mo", 1_048_576_f64),
    ];
    let (number, multiplier) = units
        .iter()
        .find_map(|(suffix, multiplier)| {
            normalized
                .strip_suffix(suffix)
                .map(|number| (number, *multiplier))
        })
        .ok_or(MemoryParseError::UnsupportedUnit)?;
    let amount = number
        .parse::<f64>()
        .map_err(|_| MemoryParseError::InvalidNumber)?;
    let bytes = (amount * multiplier).round();
    if !bytes.is_finite()
        || bytes < MINIMUM_MEMORY_LIMIT_BYTES as f64
        || bytes > MAXIMUM_MEMORY_LIMIT_BYTES as f64
    {
        return Err(MemoryParseError::OutOfRange);
    }
    Ok(bytes as u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MemoryParseError {
    #[error("memory size is empty")]
    Empty,
    #[error("memory size has an invalid number")]
    InvalidNumber,
    #[error("memory unit must be MB/MiB/Mo, GB/GiB/Go, or TB/TiB/To")]
    UnsupportedUnit,
    #[error("memory size must be between 128 MiB and 1 TiB")]
    OutOfRange,
}
