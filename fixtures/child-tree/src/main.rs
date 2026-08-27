use std::{
    env, fs,
    io::{self, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

fn main() -> io::Result<()> {
    let mut arguments = env::args();
    let executable = arguments
        .next()
        .ok_or_else(|| io::Error::other("missing executable"))?;
    let role = arguments.next().unwrap_or_else(|| "parent".to_owned());
    let port = arguments
        .next()
        .ok_or_else(|| io::Error::other("missing port"))?;
    let gate = arguments.next().map(PathBuf::from);

    match role.as_str() {
        "parent" | "parent-exits" => {
            let gate = gate_path("parent", &port);
            let child = Command::new(&executable)
                .args(["child", &port])
                .arg(&gate)
                .spawn()?;
            println!("PARENT:{} CHILD:{}", std::process::id(), child.id());
            io::stdout().flush()?;
            fs::write(gate, b"go")?;
            if role == "parent-exits" {
                return Ok(());
            }
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }
        "child" => {
            wait_for_gate(gate.as_deref())?;
            let gate = gate_path("child", &port);
            let grandchild = Command::new(&executable)
                .args(["grandchild", &port])
                .arg(&gate)
                .spawn()?;
            println!(
                "CHILD:{} GRANDCHILD:{}",
                std::process::id(),
                grandchild.id()
            );
            io::stdout().flush()?;
            fs::write(gate, b"go")?;
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }
        "grandchild" => {
            wait_for_gate(gate.as_deref())?;
            let listener = TcpListener::bind(format!("127.0.0.1:{port}"))?;
            println!(
                "LISTENING:{}:{}",
                std::process::id(),
                listener.local_addr()?.port()
            );
            io::stdout().flush()?;
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }
        _ => Err(io::Error::other("unknown child-tree role")),
    }
}

fn gate_path(stage: &str, port: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "netsustack-child-tree-{}-{port}-{stage}.gate",
        std::process::id()
    ))
}

fn wait_for_gate(gate: Option<&Path>) -> io::Result<()> {
    let gate = gate.ok_or_else(|| io::Error::other("missing ordering gate"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match fs::remove_file(gate) {
            Ok(()) => return Ok(()),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && std::time::Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
}
