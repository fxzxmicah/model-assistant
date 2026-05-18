use std::collections::BTreeMap;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, anyhow};

use crate::core::config::{AppConfig, ModelConfig};
use crate::runner::ipc::{ClientMessage, ServerMessage, read_json_line, write_json_line};
use crate::runner::manager::RunnerManager;
use crate::runner::paths::ResolvedPaths;

const MODEL_MOUNT_POINT: &str = "/mnt";

#[derive(Debug, Clone)]
pub enum ProcessEvent {
    Output { data: Vec<u8> },
    Exited,
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
        let runtime_binding = config.bind_runtime(model_id, model, runtime_name)?;
        let mode_binding = runtime_binding.bind_mode(mode_name)?;

        let mut env = base_child_env();
        env.extend(runner.runner_env().clone());
        env.extend(runtime_binding.model_runtime.env.clone());
        if let Some(mode_override) = mode_binding.model_override {
            env.extend(mode_override.env.clone());
        }

        let mut command = vec![
            "chroot".to_string(),
            paths.runner_root.display().to_string(),
            mode_binding.mode.executable.clone(),
        ];
        command.extend(mode_binding.mode.args.iter().cloned());
        command.extend(runtime_binding.model_runtime.args.iter().cloned());
        command.push(runtime_binding.runtime.model_arg.clone());
        command.push(format!("{}/{}", MODEL_MOUNT_POINT.trim_end_matches('/'), model.file));
        if let Some(mode_override) = mode_binding.model_override {
            command.extend(mode_override.args.iter().cloned());
        }

        Ok(Self {
            interactive: mode_binding.mode.interactive,
            command_preview: format!("[{model_id}/{runtime_name}/{mode_name}] {}", command.iter().skip(2).cloned().collect::<Vec<_>>().join(" ")),
            command,
            env,
        })
    }
}

impl RunningProcess {
    pub fn spawn(plan: LaunchPlan, sender: Sender<ProcessEvent>, runner: &RunnerManager) -> Result<Self> {
        let socket_path = runner.keeper_socket_path()?;
        let stream = UnixStream::connect(&socket_path)
            .with_context(|| format!("failed to connect runner keeper at {}", socket_path.display()))?;
        let control = Arc::new(Mutex::new(stream));

        let LaunchPlan { command, env, .. } = plan;
        {
            let mut writer = control.lock().expect("control mutex poisoned");
            write_json_line(&mut *writer, &ClientMessage::Spawn { argv: command, env })?;
        }

        let read_stream = {
            let writer = control.lock().expect("control mutex poisoned");
            writer.try_clone().context("failed to clone runner stream")?
        };
        let mut reader = BufReader::new(read_stream);
        match read_json_line(&mut reader)? {
            ServerMessage::Spawned { .. } => {}
            ServerMessage::Error { message } => return Err(anyhow!(message)),
            other => return Err(anyhow!("unexpected first runner response: {other:?}")),
        }

        thread::spawn(move || loop {
            match read_json_line(&mut reader) {
                Ok(ServerMessage::Output { data }) => {
                    let _ = sender.send(ProcessEvent::Output { data });
                }
                Ok(ServerMessage::Exited) => {
                    let _ = sender.send(ProcessEvent::Exited);
                    break;
                }
                Ok(ServerMessage::Error { message }) => {
                    let _ = sender.send(ProcessEvent::Output {
                        data: format!("== runner error ==\n{message}\n").into_bytes(),
                    });
                    break;
                }
                Ok(ServerMessage::Spawned { .. }) => {}
                Err(_) => break,
            }
        });

        Ok(Self { control })
    }

    pub fn send_line(&self, text: &str) -> Result<()> {
        let mut writer = self.control.lock().expect("control mutex poisoned");
        write_json_line(
            &mut *writer,
            &ClientMessage::Stdin {
                data: format!("{text}\n"),
            },
        )
    }

    pub fn stop(&self) -> Result<()> {
        let mut writer = self.control.lock().expect("control mutex poisoned");
        write_json_line(&mut *writer, &ClientMessage::Stop)
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
