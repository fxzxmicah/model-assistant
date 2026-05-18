use std::rc::Rc;

use crate::core::config::AppConfig;
use crate::runner::manager::RunnerManager;
use crate::runner::paths::ResolvedPaths;

#[derive(Clone)]
pub struct AppContext {
    pub config: AppConfig,
    pub paths: ResolvedPaths,
    pub runner: Rc<RunnerManager>,
}

impl AppContext {
    pub fn new(config: AppConfig, paths: ResolvedPaths) -> Self {
        let runner = Rc::new(RunnerManager::new(paths.clone(), config.runner.env.clone()));
        Self { config, paths, runner }
    }
}
