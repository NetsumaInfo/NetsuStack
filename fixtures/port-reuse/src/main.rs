use std::{
    io,
    net::{Ipv4Addr, SocketAddrV4, TcpListener},
    time::Duration,
};

fn main() -> io::Result<()> {
    let port = std::env::args_os()
        .nth(1)
        .map(|value| {
            value.to_string_lossy().parse::<u16>().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid port: {error}"),
                )
            })
        })
        .transpose()?
        .unwrap_or(0);
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))?;
    println!("READY {}", listener.local_addr()?.port());
    let _listener = listener;
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
