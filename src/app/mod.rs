pub mod console;
pub mod context;
pub mod startup;
pub mod window;

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;

use crate::app::context::AppContext;
use crate::app::startup::{preflight, present_startup_failure_dialog};
use crate::runner::paths::StartupValidation;

const APP_ID: &str = "org.gnome.ModelAssistant";

enum StartupState {
    Pending,
    Ready(AppContext),
    Failed(StartupValidation),
}

type SharedStartupState = Rc<RefCell<StartupState>>;

pub fn run() -> glib::ExitCode {
    adw::init().expect("failed to initialize libadwaita");

    let app = adw::Application::builder().application_id(APP_ID).build();
    let startup_state: SharedStartupState = Rc::new(RefCell::new(StartupState::Pending));

    let startup_state_for_startup = startup_state.clone();
    app.connect_startup(move |app| startup(app, &startup_state_for_startup));

    let startup_state_for_activate = startup_state.clone();
    app.connect_activate(move |app| activate(app, &startup_state_for_activate));

    app.run()
}

fn startup(app: &adw::Application, startup_state: &SharedStartupState) {
    match preflight() {
        Ok(context) => {
            let runner = context.runner.clone();
            app.connect_shutdown(move |_| runner.cleanup());
            startup_state.replace(StartupState::Ready(context));
        }
        Err(validation) => {
            startup_state.replace(StartupState::Failed(validation));
        }
    }
}

fn activate(app: &adw::Application, startup_state: &SharedStartupState) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }

    match &*startup_state.borrow() {
        StartupState::Ready(context) => window::build(app, context.clone()),
        StartupState::Failed(validation) => present_startup_failure_dialog(app, validation),
        StartupState::Pending => present_startup_failure_dialog(
            app,
            &StartupValidation::from_message("Internal startup state was unavailable"),
        ),
    }
}
