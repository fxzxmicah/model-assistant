use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

#[derive(Debug, Clone)]
pub struct ResolvedPaths {
    pub models_root: PathBuf,
    pub runner_root: PathBuf,
    pub files_root: PathBuf,
    pub config_path: PathBuf,
}

#[derive(Debug, Default)]
pub struct StartupValidation {
    pub errors: Vec<String>,
}

impl ResolvedPaths {
    pub fn discover() -> Result<Self> {
        let models_root = env::var_os("MODELS_ROOT")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("MODELS_ROOT environment variable is not set"))?;

        let runner_root = models_root.join("Runner");
        let files_root = models_root.join("Files");
        let config_path = files_root.join("assistant.toml");

        Ok(Self {
            models_root,
            runner_root,
            files_root,
            config_path,
        })
    }

    pub fn validate(&self) -> StartupValidation {
        let mut validation = StartupValidation::default();

        if !self.models_root.is_dir() {
            validation.push(format!(
                "MODELS_ROOT does not exist or is not a directory: {}",
                self.models_root.display()
            ));
        }

        if !self.runner_root.is_dir() {
            validation.push(format!(
                "Runner directory does not exist: {}",
                self.runner_root.display()
            ));
        } else if !looks_like_chroot(&self.runner_root) {
            validation.push(format!(
                "Runner does not look like a chroot rootfs: {}",
                self.runner_root.display()
            ));
        }

        if !self.files_root.is_dir() {
            validation.push(format!(
                "Files directory does not exist: {}",
                self.files_root.display()
            ));
        }

        if !self.config_path.is_file() {
            validation.push(format!(
                "config file does not exist: {}",
                self.config_path.display()
            ));
        }

        validation
    }
}

impl StartupValidation {
    pub fn from_single(error: anyhow::Error) -> Self {
        Self::from_message(error.to_string())
    }

    pub fn from_message(message: impl Into<String>) -> Self {
        Self {
            errors: vec![message.into()],
        }
    }

    pub fn push(&mut self, error: impl Into<String>) {
        self.errors.push(error.into());
    }

    pub fn extend(&mut self, other: StartupValidation) {
        self.errors.extend(other.errors);
    }

    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

}

impl fmt::Display for StartupValidation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.errors.join("\n"))
    }
}

fn looks_like_chroot(root: &Path) -> bool {
    ["dev", "proc", "tmp"]
        .into_iter()
        .all(|required_dir| root.join(required_dir).is_dir())
        && (root.join("usr/lib").is_dir() || root.join("usr/lib64").is_dir())
        && has_dynamic_loader(root)
}

fn has_dynamic_loader(root: &Path) -> bool {
    for relative_dir in ["lib", "lib64", "usr/lib", "usr/lib64"] {
        let dir = root.join(relative_dir);
        let Ok(entries) = dir.read_dir() else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("ld-linux-") || name.starts_with("ld-musl-") {
                return true;
            }
        }
    }

    false
}
