use std::{
    env,
    io::{self, BufRead, Write},
    thread,
    time::Duration,
};

#[cfg(windows)]
use windows::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, GetConsoleMode, GetConsoleScreenBufferInfo,
    GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetConsoleMode,
};
#[cfg(windows)]
use windows::core::BOOL;

#[cfg(windows)]
unsafe extern "system" fn ignore_control(_control_type: u32) -> BOOL {
    true.into()
}

#[cfg(windows)]
fn terminal_size() -> io::Result<(i16, i16)> {
    // SAFETY: The standard output handle is borrowed and the output structure is writable.
    let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }
        .map_err(|error| io::Error::other(error.to_string()))?;
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    // SAFETY: `handle` is a live console output handle and `info` is valid for writes.
    unsafe { GetConsoleScreenBufferInfo(handle, &mut info) }
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok((
        info.srWindow.Right - info.srWindow.Left + 1,
        info.srWindow.Bottom - info.srWindow.Top + 1,
    ))
}

#[cfg(windows)]
fn disable_input_echo() -> io::Result<()> {
    // SAFETY: The fixture is attached to ConPTY and borrows its standard input.
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) }
        .map_err(|error| io::Error::other(error.to_string()))?;
    let mut mode = Default::default();
    // SAFETY: `handle` is the live console input handle and `mode` is writable.
    unsafe { GetConsoleMode(handle, &mut mode) }
        .map_err(|error| io::Error::other(error.to_string()))?;
    // SAFETY: The updated flags remain a valid console input mode.
    unsafe { SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT) }
        .map_err(|error| io::Error::other(error.to_string()))
}

#[cfg(not(windows))]
fn terminal_size() -> io::Result<(i16, i16)> {
    Ok((0, 0))
}

fn main() -> io::Result<()> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--print-env")
    {
        for name in &arguments[index + 1..] {
            let value = env::var(name).unwrap_or_else(|_| "<missing>".into());
            println!("ENV:{name}={value}");
        }
        return Ok(());
    }
    if arguments
        .iter()
        .any(|argument| argument == "--duplex-stress")
    {
        #[cfg(windows)]
        disable_input_echo()?;
        let output = vec![b'o'; 512 * 1024];
        io::stdout().write_all(&output)?;
        io::stdout().flush()?;
        for line in io::stdin().lock().lines() {
            if line? == "DUPLEX-END" {
                break;
            }
        }
        print!("DUPLEX-READY\r\n");
        io::stdout().flush()?;
        return Ok(());
    }
    if arguments.iter().any(|argument| argument == "--spam") {
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..32 {
            io::stdout().write_all(&chunk)?;
        }
        io::stdout().flush()?;
        return Ok(());
    }
    if arguments
        .iter()
        .any(|argument| argument == "--exit-immediately")
    {
        return Ok(());
    }
    #[cfg(windows)]
    if arguments
        .iter()
        .any(|argument| argument == "--ignore-ctrl-c")
    {
        // SAFETY: The static callback remains valid for the process lifetime.
        unsafe { SetConsoleCtrlHandler(Some(ignore_control), true) }
            .map_err(|error| io::Error::other(error.to_string()))?;
    }

    let (columns, rows) = terminal_size()?;
    print!("\x1b[31mVT:café-🦀\x1b[0m\r\nSIZE:{columns}x{rows}\r\nREADY\r\n");
    io::stdout().flush()?;

    for line in io::stdin().lock().lines() {
        let line = line?;
        if line == "size" {
            thread::sleep(Duration::from_millis(100));
            let (columns, rows) = terminal_size()?;
            print!("SIZE:{columns}x{rows}\r\n");
        } else {
            print!("INPUT:{line}\r\n");
        }
        io::stdout().flush()?;
    }
    Ok(())
}
