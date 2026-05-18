use std::cell::RefCell;
use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::Child;

use anyhow::{Result, anyhow};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use crate::runner::keeper::spawn_keeper_process;
use crate::runner::paths::ResolvedPaths;
use crate::runner::report::RunnerWarnings;

const KEEPER_BIN_NAME: &str = "runner-keeper";
const KEEPER_SOCKET_PREFIX: &str = "model-assistant-runner";
const KEEPER_LIBEXECDIR: &str = "model-assistant";

pub struct KeeperHandle {
    pub child: Child,
    pub socket_path: PathBuf,
    pub warnings: RunnerWarnings,
}

pub enum RunnerInitialization {
    Ready(RunnerWarnings),
    Fatal(String),
}

pub struct RunnerManager {
    paths: ResolvedPaths,
    runner_env: BTreeMap<String, String>,
    keeper: RefCell<Option<KeeperHandle>>,
}

impl RunnerManager {
    pub fn new(paths: ResolvedPaths, runner_env: BTreeMap<String, String>) -> Self {
        Self {
            paths,
            runner_env,
            keeper: RefCell::new(None),
        }
    }

    pub fn initialize(&self) -> RunnerInitialization {
        if self.keeper.borrow().is_some() {
            return RunnerInitialization::Ready(RunnerWarnings::new());
        }

        let helper_path = match keeper_search_path() {
            Ok(path) => path,
            Err(error) => {
                return RunnerInitialization::Fatal(format!(
                    "failed to resolve runner keeper search path: {error}"
                ));
            }
        };

        let socket_path = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(format!("{}-{}.sock", KEEPER_SOCKET_PREFIX, std::process::id()));

        match spawn_keeper_process(
            KEEPER_BIN_NAME,
            helper_path,
            self.paths.models_root.clone(),
            socket_path,
        ) {
            Ok(handle) => {
                let warnings = handle.warnings.clone();
                self.keeper.replace(Some(handle));
                RunnerInitialization::Ready(warnings)
            }
            Err(error) => RunnerInitialization::Fatal(format!(
                "failed to start runner keeper: {error}"
            )),
        }
    }

    pub fn cleanup(&self) {
        let Some(mut handle) = self.keeper.borrow_mut().take() else {
            return;
        };

        if let Ok(pid) = i32::try_from(handle.child.id()) {
            let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
        }
        let _ = handle.child.wait();
        let _ = fs::remove_file(&handle.socket_path);
    }

    pub fn runner_env(&self) -> &BTreeMap<String, String> {
        &self.runner_env
    }

    pub fn keeper_socket_path(&self) -> Result<PathBuf> {
        self.keeper
            .borrow()
            .as_ref()
            .map(|handle| handle.socket_path.clone())
            .ok_or_else(|| anyhow!("runner keeper is not initialized"))
    }
}

#[cfg(debug_assertions)]
fn keeper_search_path() -> Result<OsString> {
    let current_exe = env::current_exe()?;
    let bindir = current_exe
        .parent()
        .ok_or_else(|| anyhow!("current executable has no parent directory"))?;
    let prefix = bindir
        .parent()
        .ok_or_else(|| anyhow!("current executable has no install prefix parent"))?;
    let libexec_dir = prefix.join("libexec").join(KEEPER_LIBEXECDIR);

    env::join_paths(
        [bindir.to_path_buf(), libexec_dir]
            .into_iter()
            .chain(
                env::var_os("PATH")
                    .as_ref()
                    .into_iter()
                    .flat_map(|value| env::split_paths(value)),
            ),
    )
    .map_err(|error| anyhow!(error))
}

#[cfg(not(debug_assertions))]
fn keeper_search_path() -> Result<OsString> {
    let current_exe = env::current_exe()?;
    let bindir = current_exe
        .parent()
        .ok_or_else(|| anyhow!("current executable has no parent directory"))?;
    let prefix = bindir
        .parent()
        .ok_or_else(|| anyhow!("current executable has no install prefix parent"))?;
    let libexec_dir = prefix.join("libexec").join(KEEPER_LIBEXECDIR);

    env::join_paths(
        std::iter::once(libexec_dir).chain(
            env::var_os("PATH")
                .as_ref()
                .into_iter()
                .flat_map(|value| env::split_paths(value)),
        ),
    )
    .map_err(|error| anyhow!(error))
}
