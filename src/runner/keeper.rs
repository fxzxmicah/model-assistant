use std::io::{BufReader, BufWriter, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, anyhow, bail};
use nix::libc;
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, Pid};

use crate::runner::ipc::{ClientMessage, KeeperMessage, KeeperStatus, KeeperFrame, read_json_line, write_json_line};
use crate::runner::mounts::{KeeperRuntime, enter_keeper_context};
use crate::runner::paths::ResolvedPaths;

pub fn run_keeper() -> std::process::ExitCode {
    let socket_path = std::env::args_os().nth(1).map(PathBuf::from);

    match run_keeper_service(socket_path) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            let _ = print_fatal(&error.to_string());
            std::process::ExitCode::FAILURE
        }
    }
}

fn run_keeper_service(socket_path: Option<PathBuf>) -> Result<()> {
    let socket_path = socket_path.ok_or_else(|| anyhow!("missing socket path for runner keeper"))?;
    let paths = ResolvedPaths::discover()?;

    match enter_keeper_context()? {
        ForkResult::Parent { child } => wait_for_keeper_child(child),
        ForkResult::Child => run_namespaced_keeper(socket_path, paths),
    }
}

pub fn spawn_keeper_process(
    program: &str,
    helper_path: std::ffi::OsString,
    models_root: PathBuf,
    socket_path: PathBuf,
) -> Result<super::manager::KeeperHandle> {
    let _ = std::fs::remove_file(&socket_path);
    let mut child = Command::new(program)
        .env("PATH", helper_path)
        .env("MODELS_ROOT", models_root)
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn runner keeper")?;

    let mut stdout = child
        .stdout
        .take()
        .context("runner keeper stdout pipe was not available")?;
    let mut stderr = child
        .stderr
        .take()
        .context("runner keeper stderr pipe was not available")?;

    let warnings = read_keeper_ready(&mut child, &mut stdout, &mut stderr)?;
    Ok(super::manager::KeeperHandle {
        child,
        socket_path,
        warnings,
    })
}

fn run_namespaced_keeper(socket_path: PathBuf, paths: ResolvedPaths) -> Result<()> {
    set_parent_death_signal()?;

    let runtime = KeeperRuntime::new(paths);
    let warnings = runtime.initialize();
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind runner keeper socket {}", socket_path.display()))?;
    print_ready(&warnings)?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || {
                    let _ = handle_client(stream);
                });
            }
            Err(_) => break,
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

fn read_keeper_ready(
    child: &mut std::process::Child,
    stdout: &mut std::process::ChildStdout,
    stderr: &mut std::process::ChildStderr,
) -> Result<crate::runner::report::RunnerWarnings> {
    let mut stdout_buffer = Vec::new();
    let mut stderr_buffer = Vec::new();

    loop {
        let mut poll_fds = [
            libc::pollfd {
                fd: stdout.as_raw_fd(),
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            },
            libc::pollfd {
                fd: stderr.as_raw_fd(),
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            },
        ];

        let poll_result = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as _, 250) };
        if poll_result < 0 {
            return Err(std::io::Error::last_os_error()).context("failed to poll runner keeper pipes");
        }

        if poll_result > 0 {
            if poll_fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                read_pipe_chunk(stdout, &mut stdout_buffer)?;
                if let Some(line) = take_line(&mut stdout_buffer) {
                    return Ok(serde_json::from_slice(&line)?);
                }
            }

            if poll_fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                read_pipe_chunk(stderr, &mut stderr_buffer)?;
                if let Some(line) = take_line(&mut stderr_buffer) {
                    let message = String::from_utf8_lossy(&line).trim().to_string();
                    if !message.is_empty() {
                        bail!(message);
                    }
                }
            }
        }

        if let Some(status) = child.try_wait().context("failed to poll runner keeper process")? {
            let _ = stdout.read_to_end(&mut stdout_buffer);
            let _ = stderr.read_to_end(&mut stderr_buffer);
            if let Some(line) = take_line(&mut stdout_buffer) {
                return Ok(serde_json::from_slice(&line)?);
            }
            let message = String::from_utf8_lossy(&stderr_buffer).trim().to_string();
            if !message.is_empty() {
                bail!(message);
            }
            bail!("runner keeper exited before handshake with status {status}");
        }
    }
}

fn read_pipe_chunk<R: Read>(reader: &mut R, buffer: &mut Vec<u8>) -> Result<()> {
    let mut chunk = [0_u8; 4096];
    match reader.read(&mut chunk) {
        Ok(0) => {}
        Ok(size) => buffer.extend_from_slice(&chunk[..size]),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(error) => return Err(error).context("failed to read runner keeper pipe"),
    }
    Ok(())
}

fn take_line(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let line_end = buffer.iter().position(|byte| *byte == b'\n')?;
    let mut line = buffer.drain(..=line_end).collect::<Vec<_>>();
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    Some(line)
}

fn wait_for_keeper_child(child: Pid) -> Result<()> {
    match waitpid(child, None).context("failed to wait for runner keeper child")? {
        WaitStatus::Exited(_, 0) => Ok(()),
        WaitStatus::Exited(_, code) => bail!("runner keeper child exited with status {code}"),
        WaitStatus::Signaled(_, signal, _) => bail!("runner keeper child terminated by signal {signal}"),
        status => bail!("runner keeper child ended unexpectedly: {status:?}"),
    }
}

fn set_parent_death_signal() -> Result<()> {
    let result = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to set parent death signal");
    }
    Ok(())
}

fn handle_client(stream: UnixStream) -> Result<()> {
    let reader_stream = stream
        .try_clone()
        .context("failed to clone runner stream for reading")?;
    let writer = Arc::new(Mutex::new(stream));
    let mut reader = BufReader::new(reader_stream);
    let request: ClientMessage = read_json_line(&mut reader)?;
    let ClientMessage::Spawn { argv, env } = request else {
        send_keeper_status(&writer, KeeperStatus::error("first runner message must be spawn"))?;
        return Ok(());
    };
    if argv.is_empty() {
        send_keeper_status(&writer, KeeperStatus::error("spawn argv is empty"))?;
        return Ok(());
    }

    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command.env_clear();
    command.envs(&env);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.process_group(0);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            send_keeper_status(
                &writer,
                KeeperStatus::error(format!("failed to spawn {}: {error}", argv.join(" "))),
            )?;
            return Ok(());
        }
    };

    let pid = i32::try_from(child.id()).context("child pid overflowed i32")?;
    send_child_message(&writer, KeeperMessage::ChildSpawned)?;

    let stdin = Arc::new(Mutex::new(
        child
            .stdin
            .take()
            .context("child stdin pipe was not available")?,
    ));
    let stdout = child
        .stdout
        .take()
        .context("child stdout pipe was not available")?;
    let stderr = child
        .stderr
        .take()
        .context("child stderr pipe was not available")?;
    {
        let writer = writer.clone();
        thread::spawn(move || relay_output(stdout, writer, KeeperMessage::child_stdout));
    }
    {
        let writer = writer.clone();
        thread::spawn(move || relay_output(stderr, writer, KeeperMessage::child_stderr));
    }
    {
        let stdin = stdin.clone();
        thread::spawn(move || handle_client_commands(reader, pid, stdin));
    }

    child.wait()?;
    send_child_message(&writer, KeeperMessage::ChildExited)?;
    Ok(())
}

fn handle_client_commands(
    mut reader: BufReader<UnixStream>,
    pid: i32,
    stdin: Arc<Mutex<std::process::ChildStdin>>,
) {
    loop {
        let message: ClientMessage = match read_json_line(&mut reader) {
            Ok(message) => message,
            Err(_) => break,
        };
        match message {
            ClientMessage::Spawn { .. } => {}
            ClientMessage::Stdin { data } => {
                if let Ok(mut writer) = stdin.lock() {
                    let _ = writer.write_all(data.as_bytes());
                    let _ = writer.flush();
                }
            }
            ClientMessage::Stop => {
                let _ = kill(Pid::from_raw(-pid), Signal::SIGTERM);
            }
        }
    }
}


fn send_keeper_status(writer: &Arc<Mutex<UnixStream>>, status: KeeperStatus) -> Result<()> {
    send_keeper_frame(writer, &KeeperFrame::status(status))
}

fn send_child_message(writer: &Arc<Mutex<UnixStream>>, message: KeeperMessage) -> Result<()> {
    send_keeper_frame(writer, &KeeperFrame::child(message))
}

fn send_keeper_frame(writer: &Arc<Mutex<UnixStream>>, message: &KeeperFrame) -> Result<()> {
    let mut writer = writer.lock().expect("writer mutex poisoned");
    write_json_line(&mut *writer, message)
}

fn relay_output<R>(
    mut reader: R,
    writer: Arc<Mutex<UnixStream>>,
    message_for_chunk: fn(Vec<u8>) -> KeeperMessage,
)
where
    R: Read + Send + 'static,
{
    let mut buffer = [0_u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(size) => {
                let message = message_for_chunk(buffer[..size].to_vec());
                let _ = send_child_message(&writer, message);
            }
            Err(_) => break,
        }
    }
}

fn print_ready(warnings: &crate::runner::report::RunnerWarnings) -> Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = BufWriter::new(stdout.lock());
    write_json_line(&mut stdout, warnings)?;
    Ok(())
}

fn print_fatal(message: &str) -> Result<()> {
    let stderr = std::io::stderr();
    let mut stderr = BufWriter::new(stderr.lock());
    stderr.write_all(message.as_bytes())?;
    stderr.write_all(b"\n")?;
    stderr.flush()?;
    Ok(())
}
