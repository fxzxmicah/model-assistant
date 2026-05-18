use std::rc::Rc;

use libadwaita as adw;
use libadwaita::prelude::{AdwDialogExt, AlertDialogExt, ApplicationExt};

use crate::app::context::AppContext;
use crate::core::config::AppConfig;
use crate::runner::manager::RunnerManager;
use crate::runner::paths::{ResolvedPaths, StartupValidation};

pub fn preflight() -> Result<AppContext, StartupValidation> {
    let paths = ResolvedPaths::discover().map_err(StartupValidation::from_single)?;
    let mut validation = paths.validate();
    if validation.has_errors() {
        return Err(validation);
    }

    let config = AppConfig::load(&paths).map_err(StartupValidation::from_single)?;
    validation.extend(config.validate(&paths));
    if validation.has_errors() {
        return Err(validation);
    }

    let runner = Rc::new(RunnerManager::new(paths.clone(), config.runner.env.clone()));
    Ok(AppContext { config, paths, runner })
}

pub fn present_startup_failure_dialog(app: &adw::Application, validation: &StartupValidation) {
    let dialog = adw::AlertDialog::builder()
        .heading("Startup checks failed")
        .body(validation.body())
        .close_response("close")
        .default_response("close")
        .build();
    dialog.add_response("close", "Close");
    dialog.set_response_appearance("close", adw::ResponseAppearance::Suggested);

    let app = app.clone();
    dialog.connect_response(None, move |_, _| {
        app.quit();
    });

    dialog.present(Option::<&gtk4::Widget>::None);
}
