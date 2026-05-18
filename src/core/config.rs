use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::runner::paths::{ResolvedPaths, StartupValidation};

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub config_version: u32,
    pub runner: RunnerConfig,
    pub runtimes: BTreeMap<String, RuntimeConfig>,
    pub models: BTreeMap<String, ModelConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerConfig {
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default = "fallback_model_arg")]
    pub model_arg: String,
    pub modes: BTreeMap<String, ModeConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModeConfig {
    pub executable: String,
    #[serde(default)]
    pub interactive: bool,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    pub title: String,
    pub file: String,
    pub default_runtime: String,
    pub runtimes: BTreeMap<String, ModelRuntimeConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelRuntimeConfig {
    pub default_mode: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub modes: BTreeMap<String, ModelModeOverride>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ModelModeOverride {
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

pub struct ModelRuntimeBinding<'a> {
    pub runtime_name: String,
    pub runtime: &'a RuntimeConfig,
    pub model_runtime: &'a ModelRuntimeConfig,
}

pub struct ModelModeBinding<'a> {
    pub mode: &'a ModeConfig,
    pub model_override: Option<&'a ModelModeOverride>,
}

fn fallback_model_arg() -> String {
    "--model".to_string()
}

impl AppConfig {
    pub fn load(paths: &ResolvedPaths) -> Result<Self> {
        let content = fs::read_to_string(&paths.config_path).with_context(|| {
            format!("failed to read config file at {}", paths.config_path.display())
        })?;

        toml::from_str(&content).context("failed to parse config file")
    }

    pub fn validate(&self, paths: &ResolvedPaths) -> StartupValidation {
        let mut validation = StartupValidation::default();

        if self.config_version != 1 {
            validation.push(format!(
                "unsupported config_version {}, expected 1",
                self.config_version
            ));
        }

        if self.runtimes.is_empty() {
            validation.push("config must declare at least one runtime");
        }

        if self.models.is_empty() {
            validation.push("config must declare at least one model");
        }

        for (runtime_name, runtime) in &self.runtimes {
            if !runtime.model_arg.starts_with('-') {
                validation.push(format!(
                    "runtime `{runtime_name}` has invalid model_arg `{}`",
                    runtime.model_arg
                ));
            }

            if runtime.modes.is_empty() {
                validation.push(format!(
                    "runtime `{runtime_name}` must declare at least one mode"
                ));
            }

            for (mode_name, mode) in &runtime.modes {
                if mode.executable.trim().is_empty() {
                    validation.push(format!(
                        "runtime `{runtime_name}` mode `{mode_name}` must define a non-empty executable"
                    ));
                }
            }
        }

        let mut titles = BTreeSet::new();
        for (model_id, model) in &self.models {
            if model.title.trim().is_empty() {
                validation.push(format!("model `{model_id}` must define a title"));
            }

            if !titles.insert(model.title.clone()) {
                validation.push(format!("duplicate model title `{}`", model.title));
            }

            if model.runtimes.is_empty() {
                validation.push(format!(
                    "model `{model_id}` must declare at least one runtime mapping"
                ));
            }

            let file_path = paths.files_root.join(&model.file);
            if !file_path.is_file() {
                validation.push(format!(
                    "model `{model_id}` file is missing: {}",
                    file_path.display()
                ));
            }

            if !model.runtimes.contains_key(&model.default_runtime) {
                validation.push(format!(
                    "model `{model_id}` references unknown default runtime `{}` in its runtime mappings",
                    model.default_runtime
                ));
            }

            for (runtime_name, model_runtime) in &model.runtimes {
                let Some(runtime) = self.runtimes.get(runtime_name) else {
                    validation.push(format!(
                        "model `{model_id}` references unknown runtime `{runtime_name}`"
                    ));
                    continue;
                };

                if !runtime.modes.contains_key(&model_runtime.default_mode) {
                    validation.push(format!(
                        "model `{model_id}` references unknown default mode `{}` for runtime `{runtime_name}`",
                        model_runtime.default_mode
                    ));
                }

                for mode_name in model_runtime.modes.keys() {
                    if !runtime.modes.contains_key(mode_name) {
                        validation.push(format!(
                            "model `{model_id}` references unknown mode override `{mode_name}` for runtime `{runtime_name}`"
                        ));
                    }
                }
            }
        }

        validation
    }

    pub fn bind_runtime<'a>(
        &'a self,
        model_id: &str,
        model: &'a ModelConfig,
        runtime_name: &str,
    ) -> Result<ModelRuntimeBinding<'a>> {
        let model_runtime = model
            .runtimes
            .get(runtime_name)
            .with_context(|| format!("model `{model_id}` has no runtime mapping `{runtime_name}`"))?;
        let runtime = self
            .runtimes
            .get(runtime_name)
            .with_context(|| format!("unknown runtime `{runtime_name}`"))?;

        Ok(ModelRuntimeBinding {
            runtime_name: runtime_name.to_string(),
            runtime,
            model_runtime,
        })
    }
}

impl ModelConfig {
    pub fn runtime_names(&self) -> Vec<String> {
        self.runtimes.keys().cloned().collect()
    }

    pub fn default_runtime_name(&self) -> String {
        if self.runtimes.contains_key(&self.default_runtime) {
            return self.default_runtime.clone();
        }

        self.runtimes.keys().next().cloned().unwrap_or_default()
    }
}

impl<'a> ModelRuntimeBinding<'a> {
    pub fn mode_names(&self) -> Vec<String> {
        self.runtime.modes.keys().cloned().collect()
    }

    pub fn default_mode_name(&self) -> String {
        if self.runtime.modes.contains_key(&self.model_runtime.default_mode) {
            return self.model_runtime.default_mode.clone();
        }

        self.runtime.modes.keys().next().cloned().unwrap_or_default()
    }

    pub fn bind_mode(&self, mode_name: &str) -> Result<ModelModeBinding<'a>> {
        let mode = self.runtime.modes.get(mode_name).with_context(|| {
            format!(
                "unknown mode `{mode_name}` for runtime `{}`",
                self.runtime_name
            )
        })?;

        Ok(ModelModeBinding {
            mode,
            model_override: self.model_runtime.modes.get(mode_name),
        })
    }
}
