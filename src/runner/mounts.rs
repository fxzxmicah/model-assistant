use std::cell::RefCell;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use nix::mount::{MntFlags, MsFlags, mount, umount, umount2};
use nix::sched::{CloneFlags, unshare};
use nix::unistd::{ForkResult, Gid, Uid, fork};

use crate::runner::paths::ResolvedPaths;
use crate::runner::report::RunnerWarnings;

pub fn enter_keeper_context() -> Result<ForkResult> {
    let uid = Uid::current();
    let gid = Gid::current();

    unshare(CloneFlags::CLONE_NEWUSER).context("unshare(CLONE_NEWUSER) failed")?;

    if uid.as_raw() != 0 {
        write_proc_file("/proc/self/setgroups", "deny\n")
            .context("failed to write /proc/self/setgroups")?;
    }

    write_proc_file("/proc/self/uid_map", &format!("0 {} 1\n", uid.as_raw()))
        .context("failed to write /proc/self/uid_map")?;
    write_proc_file("/proc/self/gid_map", &format!("0 {} 1\n", gid.as_raw()))
        .context("failed to write /proc/self/gid_map")?;

    unshare(CloneFlags::CLONE_NEWNS | CloneFlags::CLONE_NEWPID)
        .context("unshare(CLONE_NEWNS | CLONE_NEWPID) failed")?;

    let fork_result = unsafe { fork() }.context("fork() failed after entering runner namespaces")?;
    Ok(fork_result)
}

pub struct KeeperRuntime {
    paths: ResolvedPaths,
    mounts: RefCell<Vec<PathBuf>>,
}

impl KeeperRuntime {
    pub fn new(paths: ResolvedPaths) -> Self {
        Self {
            paths,
            mounts: RefCell::new(Vec::new()),
        }
    }

    pub fn initialize(&self) -> RunnerWarnings {
        let mut warnings = RunnerWarnings::new();

        if let Err(error) = self.make_rprivate() {
            warnings.push(format!("failed to make mounts private: {error}"));
        }

        self.mount_proc(&mut warnings);
        self.bind_device_files(&mut warnings);
        self.mount_devpts(&mut warnings);
        self.mount_tmpfs(&mut warnings, "dev/shm", "size=8G");
        self.mount_tmpfs(&mut warnings, "tmp", "size=8G");
        self.bind_optional_node(&mut warnings, "/dev/kfd");
        self.bind_optional_tree(&mut warnings, "/dev/dri");
        self.bind_optional_tree(&mut warnings, "/dev/accel");
        self.bind_glob_like(&mut warnings, "/dev", "nvidia");
        self.bind_files_tree(&mut warnings);
        self.bind_home(&mut warnings);

        warnings
    }

    fn make_rprivate(&self) -> Result<()> {
        mount(
            None::<&str>,
            Path::new("/"),
            None::<&str>,
            MsFlags::MS_REC | MsFlags::MS_PRIVATE,
            None::<&str>,
        )?;
        Ok(())
    }

    fn mount_proc(&self, warnings: &mut RunnerWarnings) {
        let target = self.paths.runner_root.join("proc");
        self.ensure_dir(&target, warnings);
        if let Err(error) = mount(
            Some("proc"),
            &target,
            Some("proc"),
            MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
            None::<&str>,
        ) {
            warnings.push(format!("failed to mount proc at {}: {error}", target.display()));
            return;
        }
        self.remember_mount(target);
    }

    fn bind_device_files(&self, warnings: &mut RunnerWarnings) {
        for device in ["null", "zero", "full", "random", "urandom"] {
            let source = PathBuf::from(format!("/dev/{device}"));
            let target = self.paths.runner_root.join("dev").join(device);
            self.ensure_file_parent(&target, warnings);
            self.ensure_file_target(&target, warnings);
            self.bind_mount(&source, &target, MsFlags::MS_BIND, warnings);
        }
    }

    fn mount_devpts(&self, warnings: &mut RunnerWarnings) {
        let target = self.paths.runner_root.join("dev/pts");
        self.ensure_dir(&target, warnings);
        if let Err(error) = mount(
            Some("devpts"),
            &target,
            Some("devpts"),
            MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC,
            Some("newinstance,ptmxmode=0666,mode=0620"),
        ) {
            warnings.push(format!("failed to mount devpts at {}: {error}", target.display()));
            return;
        }
        self.remember_mount(target);
    }

    fn mount_tmpfs(&self, warnings: &mut RunnerWarnings, relative: &str, data: &str) {
        let target = self.paths.runner_root.join(relative);
        self.ensure_dir(&target, warnings);
        if let Err(error) = mount(
            Some("tmpfs"),
            &target,
            Some("tmpfs"),
            MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
            Some(data),
        ) {
            warnings.push(format!("failed to mount tmpfs at {}: {error}", target.display()));
            return;
        }
        self.remember_mount(target);
    }

    fn bind_optional_tree(&self, warnings: &mut RunnerWarnings, source: &str) {
        let source = PathBuf::from(source);
        if !source.exists() {
            return;
        }
        let relative = source.strip_prefix("/").unwrap_or(source.as_path());
        let target = self.paths.runner_root.join(relative);
        self.ensure_dir(&target, warnings);
        self.bind_mount(&source, &target, MsFlags::MS_BIND | MsFlags::MS_REC, warnings);
    }

    fn bind_optional_node(&self, warnings: &mut RunnerWarnings, source: &str) {
        let source = PathBuf::from(source);
        if !source.exists() {
            return;
        }
        let relative = source.strip_prefix("/").unwrap_or(source.as_path());
        let target = self.paths.runner_root.join(relative);
        self.ensure_file_parent(&target, warnings);
        self.ensure_file_target(&target, warnings);
        self.bind_mount(&source, &target, MsFlags::MS_BIND, warnings);
    }

    fn bind_glob_like(&self, warnings: &mut RunnerWarnings, base: &str, prefix: &str) {
        let Ok(entries) = fs::read_dir(base) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(prefix) {
                continue;
            }
            let source = entry.path();
            if source.is_dir() {
                continue;
            }
            let relative = source.strip_prefix("/").unwrap_or(source.as_path());
            let target = self.paths.runner_root.join(relative);
            self.ensure_file_parent(&target, warnings);
            self.ensure_file_target(&target, warnings);
            self.bind_mount(&source, &target, MsFlags::MS_BIND, warnings);
        }
    }

    fn bind_files_tree(&self, warnings: &mut RunnerWarnings) {
        let target = self.paths.runner_root.join("mnt");
        self.ensure_dir(&target, warnings);
        self.bind_mount(
            &self.paths.files_root,
            &target,
            MsFlags::MS_BIND | MsFlags::MS_REC,
            warnings,
        );
    }

    fn bind_home(&self, warnings: &mut RunnerWarnings) {
        let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
            warnings.push(anyhow!("HOME environment variable is not set").to_string());
            return;
        };
        if !home.is_dir() {
            warnings.push(format!("HOME does not point to a directory: {}", home.display()));
            return;
        }
        let target = self.paths.runner_root.join("root");
        self.ensure_dir(&target, warnings);
        self.bind_mount(&home, &target, MsFlags::MS_BIND | MsFlags::MS_REC, warnings);
    }

    fn bind_mount(&self, source: &Path, target: &Path, flags: MsFlags, warnings: &mut RunnerWarnings) {
        if let Err(error) = mount(Some(source), target, None::<&str>, flags, None::<&str>) {
            warnings.push(format!("failed to bind {} -> {}: {error}", source.display(), target.display()));
            return;
        }
        self.remember_mount(target.to_path_buf());
    }

    fn ensure_dir(&self, path: &Path, warnings: &mut RunnerWarnings) {
        if let Err(error) = fs::create_dir_all(path) {
            warnings.push(format!("failed to create directory {}: {error}", path.display()));
        }
    }

    fn ensure_file_parent(&self, path: &Path, warnings: &mut RunnerWarnings) {
        if let Some(parent) = path.parent() {
            self.ensure_dir(parent, warnings);
        }
    }

    fn ensure_file_target(&self, path: &Path, warnings: &mut RunnerWarnings) {
        if path.exists() {
            return;
        }
        if let Err(error) = fs::File::create(path) {
            warnings.push(format!("failed to create file target {}: {error}", path.display()));
        }
    }

    fn remember_mount(&self, target: PathBuf) {
        self.mounts.borrow_mut().push(target);
    }
}

impl Drop for KeeperRuntime {
    fn drop(&mut self) {
        let mut mounts = self.mounts.borrow_mut();
        while let Some(target) = mounts.pop() {
            if umount(&target).is_err() {
                let _ = umount2(&target, MntFlags::MNT_DETACH);
            }
        }
    }
}

fn write_proc_file(path: &str, contents: &str) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("failed to open {path}"))?;
    file.write_all(contents.as_bytes())
        .with_context(|| format!("failed to write {path}"))?;
    file.flush()
        .with_context(|| format!("failed to flush {path}"))?;
    Ok(())
}
