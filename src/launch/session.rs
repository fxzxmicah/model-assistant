use std::collections::BTreeMap;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, anyhow};

use crate::core::config::{AppConfig, ModelConfig, ResolvedModeBinding};
use crate::runner::ipc::{ClientMessage, KeeperMessage, KeeperStatus, KeeperFrame, read_json_line, write_json_line};
use crate::runner::manager::RunnerManager;
use crate::runner::paths::ResolvedPaths;

const MODEL_MOUNT_POINT: &str = "/mnt";

#[derive(Debug, Clone)]
pub enum ProcessEvent {
    KeeperStatus(KeeperStatus),
    KeeperMessage(KeeperMessage),
}

impl ProcessEvent {
    fn child(message: KeeperMessage) -> Self {
        Self::KeeperMessage(message)
    }

    fn keeper_status(status: KeeperStatus) -> Self {
        Self::KeeperStatus(status)
    }

    fn from_keeper_frame(frame: KeeperFrame) -> Self {
        match frame {
            KeeperFrame::KeeperStatus { status } => Self::keeper_status(status),
            KeeperFrame::KeeperMessage { message } => Self::child(message),
        }
    }

    pub fn child_output(&self) -> Option<&[u8]> {
        match self {
            Self::KeeperStatus(_) => None,
            Self::KeeperMessage(message) => message.output_bytes(),
        }
    }

    pub fn keeper_error_message(&self) -> Option<&str> {
        match self {
            Self::KeeperStatus(status) => status.error_message(),
            Self::KeeperMessage(_) => None,
        }
    }

    pub fn is_child_exited(&self) -> bool {
        matches!(self, Self::KeeperMessage(message) if message.is_child_exited())
    }

    fn ends_process_stream(&self) -> bool {
        self.keeper_error_message().is_some() || self.is_child_exited()
    }
}

#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub interactive: bool,
    pub command_preview: String,
    pub command: Vec<String>,
    pub env: BTreeMap<String, String>,
}

pub struct RunningProcess {
    control: Arc<Mutex<UnixStream>>,
}

impl LaunchPlan {
    pub fn build(
        config: &AppConfig,
        model_id: &str,
        model: &ModelConfig,
        runtime_name: &str,
        mode_name: &str,
        paths: &ResolvedPaths,
        runner: &RunnerManager,
    ) -> Result<Self> {
        let binding = config.bind_mode(model_id, model, runtime_name, mode_name)?;
        let command = Self::build_command(paths, model, &binding);

        Ok(Self {
            interactive: binding.interactive(),
            command_preview: Self::build_command_preview(model_id, runtime_name, mode_name, &command),
            command,
            env: Self::build_env(runner, &binding),
        })
    }

    fn build_env(
        runner: &RunnerManager,
        binding: &ResolvedModeBinding<'_>,
    ) -> BTreeMap<String, String> {
        let mut env = base_child_env();
        env.extend(runner.runner_env().clone());
        env.extend(binding.runtime_env().clone());
        if let Some(mode_override) = binding.mode_override() {
            env.extend(mode_override.env.clone());
        }
        env
    }

    fn build_command(
        paths: &ResolvedPaths,
        model: &ModelConfig,
        binding: &ResolvedModeBinding<'_>,
    ) -> Vec<String> {
        let mut command = vec![
            "chroot".to_string(),
            paths.runner_root.display().to_string(),
            binding.executable().to_string(),
        ];
        command.extend(binding.mode_args().iter().cloned());
        command.extend(binding.runtime_args().iter().cloned());
        command.push(binding.model_arg().to_string());
        command.push(format!("{}/{}", MODEL_MOUNT_POINT.trim_end_matches('/'), model.file));
        if let Some(mode_override) = binding.mode_override() {
            command.extend(mode_override.args.iter().cloned());
        }
        command
    }

    fn build_command_preview(
        model_id: &str,
        runtime_name: &str,
        mode_name: &str,
        command: &[String],
    ) -> String {
        format!(
            "[{model_id}/{runtime_name}/{mode_name}] {}",
            command.iter().skip(2).cloned().collect::<Vec<_>>().join(" ")
        )
    }
}

impl RunningProcess {
    pub fn spawn(plan: LaunchPlan, sender: Sender<ProcessEvent>, runner: &RunnerManager) -> Result<Self> {
        let socket_path = runner.keeper_socket_path()?;
        let stream = UnixStream::connect(&socket_path)
            .with_context(|| format!("failed to connect runner keeper at {}", socket_path.display()))?;
        let control = Arc::new(Mutex::new(stream));

        let LaunchPlan { command, env, .. } = plan;
        Self::write_control_message(&control, &ClientMessage::Spawn { argv: command, env })?;

        let read_stream = Self::clone_control_stream(&control)?;
        let mut reader = BufReader::new(read_stream);
        Self::wait_for_spawned(&mut reader)?;
        let _ = sender.send(ProcessEvent::child(KeeperMessage::ChildSpawned));
        Self::spawn_event_forwarder(reader, sender);

        Ok(Self { control })
    }

    pub fn send_line(&self, text: &str) -> Result<()> {
        Self::write_control_message(
            &self.control,
            &ClientMessage::Stdin {
                data: format!("{text}\n"),
            },
        )
    }

    pub fn stop(&self) -> Result<()> {
        Self::write_control_message(&self.control, &ClientMessage::Stop)
    }

    fn clone_control_stream(control: &Arc<Mutex<UnixStream>>) -> Result<UnixStream> {
        let writer = control.lock().expect("control mutex poisoned");
        writer.try_clone().context("failed to clone runner stream")
    }

    fn wait_for_spawned(reader: &mut BufReader<UnixStream>) -> Result<()> {
        match read_json_line(reader)? {
            KeeperFrame::KeeperMessage {
                message: KeeperMessage::ChildSpawned,
            } => Ok(()),
            KeeperFrame::KeeperStatus { status } => {
                let message = status
                    .error_message()
                    .unwrap_or("runner keeper reported an unknown error")
                    .to_string();
                Err(anyhow!(message))
            },
            other => Err(anyhow!("unexpected first keeper response: {other:?}")),
        }
    }

    fn spawn_event_forwarder(mut reader: BufReader<UnixStream>, sender: Sender<ProcessEvent>) {
        thread::spawn(move || loop {
            match read_json_line(&mut reader) {
                Ok(frame) => {
                    let event = ProcessEvent::from_keeper_frame(frame);
                    let should_break = event.ends_process_stream();
                    let _ = sender.send(event);
                    if should_break {
                        break;
                    }
                }
                Err(_) => break,
            }
        });
    }

    fn write_control_message(control: &Arc<Mutex<UnixStream>>, message: &ClientMessage) -> Result<()> {
        let mut writer = control.lock().expect("control mutex poisoned");
        write_json_line(&mut *writer, message)
    }
}

fn base_child_env() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "PATH".to_string(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
        ),
        ("HOME".to_string(), "/root".to_string()),
        ("USER".to_string(), "root".to_string()),
        ("LOGNAME".to_string(), "root".to_string()),
        ("LANG".to_string(), "C.UTF-8".to_string()),
    ])
}
