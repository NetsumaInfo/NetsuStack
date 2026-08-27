use std::{
    env, fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn main() -> std::io::Result<()> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--args-file")
    {
        let path = arguments.get(index + 1).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing --args-file path")
        })?;
        atomic_write(path, arguments[index + 2..].join("\n"))?;
    }
    let port = env::var("NETSUSTACK_BIND_PORT")
        .or_else(|_| env::var("PORT"))
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    if let Some(path) = env::var_os("NETSUSTACK_ENV_FILE") {
        let names = [
            "PATH",
            "SYSTEMROOT",
            "TERM",
            "COLORTERM",
            "FORCE_COLOR",
            "CLICOLOR",
            "CLICOLOR_FORCE",
            "TERM_PROGRAM",
            "NETSUSTACK",
            "NETSUSTACK_SERVER",
            "NETSUSTACK_SERVER_NAME",
            "PORT",
            "NO_COLOR",
        ];
        let snapshot = names
            .into_iter()
            .map(|name| {
                format!(
                    "{name}={}\n",
                    env::var(name).unwrap_or_else(|_| "<missing>".into())
                )
            })
            .collect::<String>();
        atomic_write(path, snapshot)?;
    }
    if let Some(path) = env::var_os("NETSUSTACK_READY_FILE") {
        atomic_write(path, port.to_string())?;
    }
    println!("READY {port}");

    loop {
        match listener.accept() {
            Ok((stream, _)) => serve(stream)?,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
}

fn atomic_write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    let path = path.as_ref();
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(".tmp-{}-{sequence}", std::process::id()));
    let temporary = std::path::PathBuf::from(temporary);
    fs::write(&temporary, contents)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(temporary);
        return Err(error);
    }
    Ok(())
}

fn serve(mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut request = [0_u8; 1024];
    let _ = stream.read(&mut request);
    stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
}
