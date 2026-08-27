use std::{env, process::ExitCode, thread, time::Duration};

fn main() -> ExitCode {
    let mut arguments = env::args().skip(1);
    let code = arguments
        .next()
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(1);
    let delay_ms = arguments
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(20);
    thread::sleep(Duration::from_millis(delay_ms));
    ExitCode::from(code)
}
