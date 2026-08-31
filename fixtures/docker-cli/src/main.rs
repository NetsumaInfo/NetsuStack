use std::{
    io::Write,
    process::{Command, ExitCode},
    thread,
    time::Duration,
};

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    match arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["inspect", "--", "--malicious-container"] => {
            let inspect = r#"[{"Id":"--malicious-container","Name":"/web-1","Config":{"Labels":{"com.docker.compose.project":"sample","com.docker.compose.service":"web"}},"NetworkSettings":{"Ports":{"5173/tcp":[{"HostIp":"127.0.0.1","HostPort":"5173"}]}}}]"#;
            println!("{inspect}");
            ExitCode::SUCCESS
        }
        ["stop", "--time", "10", "--", "--malicious-container"] => ExitCode::SUCCESS,
        ["ps", "--filter", "publish=65534", "--format", "{{.ID}}"] => {
            spawn_inherited_child("pipe-holder");
            ExitCode::SUCCESS
        }
        ["ps", "--filter", "publish=65532", "--format", "{{.ID}}"] => {
            let mut stdout = std::io::stdout().lock();
            let block = [b'n'; 64 * 1024];
            for _ in 0..144 {
                if stdout.write_all(&block).is_err() {
                    break;
                }
            }
            ExitCode::SUCCESS
        }
        ["pipe-holder"] => {
            thread::sleep(Duration::from_secs(30));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("unsafe or unexpected Docker arguments: {arguments:?}");
            ExitCode::FAILURE
        }
    }
}

#[allow(clippy::zombie_processes)]
fn spawn_inherited_child(mode: &str) {
    // The probe records and explicitly terminates/waits this descendant after
    // measuring while it keeps inherited stdout/stderr open.
    let child = Command::new(std::env::current_exe().expect("fixture executable"))
        .arg(mode)
        .spawn()
        .expect("spawn inherited-pipe holder");
    let path = std::env::var_os("NETSUSTACK_DOCKER_FIXTURE_PID_FILE")
        .expect("pipe-holder requires a probe PID file");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open fixture PID file");
    writeln!(file, "{}", child.id()).expect("record fixture descendant PID");
}
