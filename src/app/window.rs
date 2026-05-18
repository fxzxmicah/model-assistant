use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{self, TryRecvError};

use anyhow::Result;
use gtk4::glib::{self, ControlFlow};
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::app::console::ConsoleView;
use crate::app::context::AppContext;
use crate::core::config::{AppConfig, ModeChoices, ModelConfig, ResolvedModeBinding, RuntimeChoices};
use crate::launch::session::{LaunchPlan, ProcessEvent, RunningProcess};
use crate::runner::ipc::KeeperStatus;

const APP_ID: &str = "org.gnome.modelassistant";
const WINDOW_TITLE: &str = "Model Assistant";
const ABOUT_DEVELOPER_NAME: &str = "Model Assistant Contributors";
const ABOUT_WEBSITE_URL: &str = "https://github.com/fxzxmicah/model-assistant";
const ABOUT_ISSUES_URL: &str = "https://github.com/fxzxmicah/model-assistant/issues";
const RUNNING_ICON: &str = "media-playback-start-symbolic";
const STOPPED_ICON: &str = "media-playback-stop-symbolic";

pub fn build(app: &adw::Application, bootstrap: AppContext) {
    let state = Rc::new(AppState::new(bootstrap.clone()));
    let toast_overlay = adw::ToastOverlay::new();
    let split_view = adw::NavigationSplitView::new();
    let stack = gtk4::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk4::StackTransitionType::Crossfade)
        .build();
    let list_box = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::Single)
        .css_classes(["navigation-sidebar"])
        .build();

    for (model_id, model) in &bootstrap.config.models {
        let runtime = build_runtime(
            state.clone(),
            model_id.clone(),
            model.clone(),
            toast_overlay.clone(),
        );
        list_box.append(&runtime.view.sidebar_row);
        stack.add_titled(&runtime.view.page, Some(model_id), &model.title);
    }

    if let Some(first_row) = list_box.row_at_index(0) {
        list_box.select_row(Some(&first_row));
    }

    let stack_for_selection = stack.clone();
    list_box.connect_row_selected(move |_, row| {
        if let Some(name) = row.and_then(|row| row.widget_name().strip_prefix("model-row-").map(str::to_string)) {
            stack_for_selection.set_visible_child_name(&name);
        }
    });

    let sidebar_toolbar = adw::ToolbarView::new();
    let sidebar_header = adw::HeaderBar::new();
    let sidebar_title = adw::WindowTitle::builder()
        .title("Models")
        .subtitle(bootstrap.paths.files_root.display().to_string())
        .build();
    sidebar_header.set_title_widget(Some(&sidebar_title));
    sidebar_toolbar.add_top_bar(&sidebar_header);
    sidebar_toolbar.set_content(Some(
        &gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vexpand(true)
            .child(&list_box)
            .build(),
    ));

    let content_toolbar = adw::ToolbarView::new();
    let content_header = adw::HeaderBar::new();
    let content_title = adw::WindowTitle::builder()
        .title("Model Console")
        .subtitle(bootstrap.paths.runner_root.display().to_string())
        .build();
    let about_button = gtk4::Button::builder()
        .icon_name("help-about-symbolic")
        .tooltip_text("About Model Assistant")
        .build();
    content_header.pack_end(&about_button);
    content_header.set_title_widget(Some(&content_title));
    content_toolbar.add_top_bar(&content_header);
    content_toolbar.set_content(Some(&stack));

    let sidebar_page = adw::NavigationPage::builder()
        .title("Models")
        .child(&sidebar_toolbar)
        .build();
    let content_page = adw::NavigationPage::builder()
        .title("Console")
        .child(&content_toolbar)
        .build();

    split_view.set_sidebar(Some(&sidebar_page));
    split_view.set_content(Some(&content_page));
    toast_overlay.set_child(Some(&split_view));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(WINDOW_TITLE)
        .default_width(1440)
        .default_height(900)
        .content(&toast_overlay)
        .build();

    let window_weak_for_about = window.downgrade();
    about_button.connect_clicked(move |_| {
        if let Some(window) = window_weak_for_about.upgrade() {
            present_about_dialog(&window);
        }
    });

    let state_for_close = state.clone();
    window.connect_close_request(move |window| {
        if !state_for_close.has_running_models() {
            return glib::Propagation::Proceed;
        }

        present_alert_dialog(
            window,
            "Stop all models before closing",
            "At least one model is still running. Stop every running model, then close the application again.",
        );
        glib::Propagation::Stop
    });

    window.present();

    let runner = bootstrap.runner.clone();
    let window_weak = window.downgrade();
    glib::idle_add_local_once(move || {
        present_runner_initialization(window_weak.clone(), runner.initialize());
    });
}


fn present_alert_dialog(window: &adw::ApplicationWindow, heading: &str, body: &str) {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .close_response("ok")
        .default_response("ok")
        .build();
    dialog.add_response("ok", "OK");
    dialog.present(Some(window));
}

fn present_about_dialog(window: &adw::ApplicationWindow) {
    let about = adw::AboutDialog::new();
    about.set_application_name(WINDOW_TITLE);
    about.set_application_icon(APP_ID);
    about.set_developer_name(ABOUT_DEVELOPER_NAME);
    about.set_version(env!("CARGO_PKG_VERSION"));
    about.set_license_type(gtk4::License::MitX11);
    about.set_developers(&[ABOUT_DEVELOPER_NAME]);
    about.set_website(ABOUT_WEBSITE_URL);
    about.set_issue_url(ABOUT_ISSUES_URL);
    about.present(Some(window));
}

fn present_runner_initialization(
    window: glib::WeakRef<adw::ApplicationWindow>,
    initialization: KeeperStatus,
) {
    if let Some(warnings) = initialization.warnings() {
        if warnings.is_empty() {
            return;
        }

        if let Some(window) = window.upgrade() {
            present_alert_dialog(&window, "Runner initialized with warnings", &warnings.to_string());
        }
        return;
    }

    if let Some(message) = initialization.error_message() {
        if let Some(window) = window.upgrade() {
            present_alert_dialog(&window, "Runner failed to initialize", message);
        }
    }
}

struct AppState {
    bootstrap: AppContext,
    running_count: Cell<u32>,
}

impl AppState {
    fn new(bootstrap: AppContext) -> Self {
        Self {
            bootstrap,
            running_count: Cell::new(0),
        }
    }

    fn has_running_models(&self) -> bool {
        self.running_count.get() > 0
    }

    fn mark_process_started(&self) {
        self.running_count
            .set(self.running_count.get().saturating_add(1));
    }

    fn mark_process_stopped(&self) {
        self.running_count
            .set(self.running_count.get().saturating_sub(1));
    }
}

#[derive(Debug, Clone)]
struct ModelSelection {
    runtime_name: String,
    mode_name: String,
}

impl ModelSelection {
    fn new(runtime_name: String, mode_name: String) -> Self {
        Self {
            runtime_name,
            mode_name,
        }
    }
}

struct RuntimeControls {
    runtime_dropdown: gtk4::DropDown,
    mode_dropdown: gtk4::DropDown,
    mode_model: gtk4::StringList,
    start_button: gtk4::Button,
    stop_button: gtk4::Button,
    initial_selection: ModelSelection,
    widget: gtk4::Box,
}

struct InputControls {
    input_entry: gtk4::Entry,
    send_button: gtk4::Button,
    widget: gtk4::Box,
}

struct ModelView {
    page: gtk4::Box,
    sidebar_row: gtk4::ListBoxRow,
    status_icon: gtk4::Image,
    command_label: gtk4::Label,
    start_button: gtk4::Button,
    stop_button: gtk4::Button,
    runtime_dropdown: gtk4::DropDown,
    mode_dropdown: gtk4::DropDown,
    mode_model: gtk4::StringList,
    send_button: gtk4::Button,
    input_entry: gtk4::Entry,
    console_view: ConsoleView,
    toast_overlay: adw::ToastOverlay,
}

struct ModelRuntime {
    state: Rc<AppState>,
    model_id: String,
    model: ModelConfig,
    process: RefCell<Option<RunningProcess>>,
    selection: RefCell<ModelSelection>,
    view: ModelView,
    process_running: Cell<bool>,
}

fn build_runtime(
    state: Rc<AppState>,
    model_id: String,
    model: ModelConfig,
    toast_overlay: adw::ToastOverlay,
) -> Rc<ModelRuntime> {
    let (view, selection) = ModelView::build(&state.bootstrap, &model_id, &model, toast_overlay);

    let runtime = Rc::new(ModelRuntime {
        state,
        model_id,
        model,
        process: RefCell::new(None),
        selection: RefCell::new(selection),
        view,
        process_running: Cell::new(false),
    });

    runtime.refresh_command_preview();
    runtime.bind_events();
    runtime
}

impl ModelView {
    fn build(
        bootstrap: &AppContext,
        model_id: &str,
        model: &ModelConfig,
        toast_overlay: adw::ToastOverlay,
    ) -> (Self, ModelSelection) {
        let page = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(16)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();

        let controls = Self::build_runtime_controls(&bootstrap.config, model_id, model);

        let command_label = gtk4::Label::builder()
            .label("Command preview will appear here")
            .halign(gtk4::Align::Start)
            .wrap(true)
            .selectable(true)
            .css_classes(["caption", "dim-label"])
            .build();

        let console_view = Self::build_console_view();
        let input_controls = Self::build_input_controls();

        page.append(&controls.widget);
        page.append(&command_label);
        page.append(&console_view);
        page.append(&input_controls.widget);

        let (sidebar_row, status_icon) = Self::build_sidebar_row(model_id, &model.title);

        (
            Self {
                page,
                sidebar_row,
                status_icon,
                command_label,
                start_button: controls.start_button,
                stop_button: controls.stop_button,
                runtime_dropdown: controls.runtime_dropdown,
                mode_dropdown: controls.mode_dropdown,
                mode_model: controls.mode_model,
                send_button: input_controls.send_button,
                input_entry: input_controls.input_entry,
                console_view,
                toast_overlay,
            },
            controls.initial_selection,
        )
    }

    fn build_runtime_controls(config: &AppConfig, model_id: &str, model: &ModelConfig) -> RuntimeControls {
        let widget = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(12)
            .build();

        let RuntimeChoices {
            names: runtime_names,
            selected_runtime: initial_runtime,
            modes,
        } = config.runtime_choices(model_id, model);
        let ModeChoices {
            names: mode_names,
            selected: selected_mode,
        } = modes;

        let runtime_model = gtk4::StringList::new(&[]);
        populate_string_list(&runtime_model, &runtime_names);
        let runtime_dropdown = gtk4::DropDown::builder().model(&runtime_model).build();
        select_dropdown_value(&runtime_dropdown, &runtime_names, &initial_runtime);

        let mode_model = gtk4::StringList::new(&[]);
        populate_string_list(&mode_model, &mode_names);
        let mode_dropdown = gtk4::DropDown::builder().model(&mode_model).build();
        select_dropdown_value(&mode_dropdown, &mode_names, &selected_mode);

        let start_button = gtk4::Button::builder()
            .label("Start")
            .css_classes(["suggested-action"])
            .width_request(96)
            .build();
        let stop_button = gtk4::Button::builder()
            .label("Stop")
            .sensitive(false)
            .width_request(96)
            .build();

        widget.append(&gtk4::Label::builder().label("Runtime").halign(gtk4::Align::Start).build());
        widget.append(&runtime_dropdown);
        widget.append(&gtk4::Label::builder().label("Mode").halign(gtk4::Align::Start).build());
        widget.append(&mode_dropdown);
        widget.append(&start_button);
        widget.append(&stop_button);

        RuntimeControls {
            runtime_dropdown,
            mode_dropdown,
            mode_model,
            start_button,
            stop_button,
            initial_selection: ModelSelection::new(initial_runtime, selected_mode),
            widget,
        }
    }

    fn build_console_view() -> ConsoleView {
        ConsoleView::builder()
            .orientation(gtk4::Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .focusable(false)
            .margin_top(4)
            .margin_bottom(4)
            .margin_start(4)
            .margin_end(4)
            .monospace(true)
            .build()
    }

    fn build_input_controls() -> InputControls {
        let input_entry = gtk4::Entry::builder()
            .hexpand(true)
            .sensitive(false)
            .input_purpose(gtk4::InputPurpose::FreeForm)
            .build();
        let send_button = gtk4::Button::builder()
            .label("Send")
            .css_classes(["suggested-action"])
            .sensitive(false)
            .build();
        let widget = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(8)
            .build();
        widget.append(&input_entry);
        widget.append(&send_button);

        InputControls {
            input_entry,
            send_button,
            widget,
        }
    }

    fn build_sidebar_row(model_id: &str, title: &str) -> (gtk4::ListBoxRow, gtk4::Image) {
        let status_icon = gtk4::Image::from_icon_name(STOPPED_ICON);
        status_icon.set_icon_size(gtk4::IconSize::Normal);
        status_icon.add_css_class("dim-label");

        let sidebar_label = gtk4::Label::builder()
            .label(title)
            .halign(gtk4::Align::Start)
            .hexpand(true)
            .build();
        let sidebar_content = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(10)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(10)
            .margin_end(10)
            .build();
        sidebar_content.append(&status_icon);
        sidebar_content.append(&sidebar_label);
        let sidebar_row = gtk4::ListBoxRow::builder().child(&sidebar_content).build();
        sidebar_row.set_widget_name(&format!("model-row-{model_id}"));
        sidebar_row.set_selectable(true);
        sidebar_row.set_activatable(true);

        (sidebar_row, status_icon)
    }

    fn replace_modes(&self, choices: &ModeChoices) {
        populate_string_list(&self.mode_model, &choices.names);
        select_dropdown_value(&self.mode_dropdown, &choices.names, &choices.selected);
    }
}

impl ModelRuntime {
    fn bind_events(self: &Rc<Self>) {
        let runtime = self.clone();
        self.view.runtime_dropdown.connect_selected_notify(move |dropdown| {
            if let Some(runtime_name) = selected_string(dropdown) {
                runtime.select_runtime(runtime_name);
            }
        });

        let runtime = self.clone();
        self.view.mode_dropdown.connect_selected_notify(move |dropdown| {
            if let Some(mode_name) = selected_string(dropdown) {
                runtime.select_mode(mode_name);
            }
        });

        let runtime = self.clone();
        self.view.start_button.connect_clicked(move |_| {
            runtime.handle_action_result("Failed to start model", runtime.start_process());
        });

        let runtime = self.clone();
        self.view.stop_button.connect_clicked(move |_| {
            runtime.handle_action_result("Failed to stop model", runtime.stop_process());
        });

        let runtime = self.clone();
        self.view.send_button.connect_clicked(move |_| {
            runtime.handle_action_result("Failed to send input", runtime.submit_input());
        });

        let runtime = self.clone();
        self.view.input_entry.connect_activate(move |_| {
            runtime.handle_action_result("Failed to send input", runtime.submit_input());
        });
    }

    fn select_runtime(&self, runtime_name: String) {
        self.selection.borrow_mut().runtime_name = runtime_name;
        self.reload_modes_for_selected_runtime();
        self.refresh_command_preview();
        self.update_input_state();
    }

    fn select_mode(&self, mode_name: String) {
        self.selection.borrow_mut().mode_name = mode_name;
        self.refresh_command_preview();
        self.update_input_state();
    }

    fn handle_action_result(&self, heading: &str, result: Result<()>) {
        if let Err(error) = result {
            self.show_error(heading, &error.to_string());
        }
    }

    fn refresh_command_preview(&self) {
        match self.build_launch_plan() {
            Ok(plan) => self.view.set_command_preview(&plan.command_preview),
            Err(error) => self.view.set_command_preview_error(&error.to_string()),
        }
    }

    fn with_selection<T>(&self, f: impl FnOnce(&ModelSelection) -> T) -> T {
        let selection = self.selection.borrow();
        f(&selection)
    }

    fn build_launch_plan(&self) -> Result<LaunchPlan> {
        self.with_selection(|selection| {
            LaunchPlan::build(
                &self.state.bootstrap.config,
                &self.model_id,
                &self.model,
                &selection.runtime_name,
                &selection.mode_name,
                &self.state.bootstrap.paths,
                &self.state.bootstrap.runner,
            )
        })
    }

    fn selected_mode_binding(&self) -> Result<ResolvedModeBinding<'_>> {
        self.with_selection(|selection| {
            self.state.bootstrap.config.bind_mode(
                &self.model_id,
                &self.model,
                &selection.runtime_name,
                &selection.mode_name,
            )
        })
    }

    fn reload_modes_for_selected_runtime(&self) {
        let mode_choices = self.with_selection(|selection| {
            self.state
                .bootstrap
                .config
                .mode_choices(&self.model_id, &self.model, &selection.runtime_name)
                .unwrap_or_default()
        });
        self.selection.borrow_mut().mode_name = mode_choices.selected.clone();
        self.view.replace_modes(&mode_choices);
    }

    fn start_process(self: &Rc<Self>) -> Result<()> {
        if self.process_running.get() {
            return Ok(());
        }

        self.view.clear_output();
        let plan = self.build_launch_plan()?;
        let interactive = plan.interactive;
        let (sender, receiver) = mpsc::channel();
        let process = RunningProcess::spawn(plan, sender, &self.state.bootstrap.runner)?;

        self.process.replace(Some(process));
        self.set_running_state(true, interactive);
        self.state.mark_process_started();

        let runtime = self.clone();
        glib::source::idle_add_local(move || match receiver.try_recv() {
            Ok(event) => runtime.handle_process_event(event),
            Err(TryRecvError::Empty) => ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => ControlFlow::Break,
        });
        Ok(())
    }

    fn stop_process(&self) -> Result<()> {
        if let Some(process) = self.process.borrow().as_ref() {
            process.stop()?;
        }
        Ok(())
    }

    fn submit_input(&self) -> Result<()> {
        let Some(text) = self.view.input_text() else {
            return Ok(());
        };

        let process = self.process.borrow();
        let Some(process) = process.as_ref() else {
            return Ok(());
        };

        process.send_line(&text)?;
        self.view.clear_input();
        Ok(())
    }

    fn handle_process_event(self: &Rc<Self>, event: ProcessEvent) -> ControlFlow {
        if let Some(data) = event.child_output() {
            self.append_output(data);
            return ControlFlow::Continue;
        }

        if let Some(message) = event.keeper_error_message() {
            self.view.show_error_dialog("Runner error", message);
            self.finish_process();
            return ControlFlow::Break;
        }

        if event.is_child_exited() {
            self.finish_process();
            return ControlFlow::Break;
        }

        ControlFlow::Continue
    }

    fn finish_process(&self) {
        self.process.replace(None);
        self.set_running_state(false, false);
        self.state.mark_process_stopped();
    }

    fn set_running_state(&self, running: bool, interactive: bool) {
        self.process_running.set(running);
        self.view.set_running_state(running, interactive);
    }

    fn update_input_state(&self) {
        self.view
            .set_input_enabled(self.process_running.get() && self.selected_mode_is_interactive());
    }

    fn selected_mode_is_interactive(&self) -> bool {
        self.selected_mode_binding()
            .map(|binding| binding.interactive())
            .unwrap_or(false)
    }

    fn append_output(&self, data: &[u8]) {
        self.view.append_output(data);
    }

    fn show_error(&self, heading: &str, body: &str) {
        self.view.show_error(heading, body);
    }
}

impl ModelView {
    fn set_command_preview(&self, text: &str) {
        self.command_label.set_label(text);
    }

    fn set_command_preview_error(&self, error: &str) {
        self.command_label.set_label(&format!("Config error: {error}"));
    }

    fn input_text(&self) -> Option<String> {
        let text = self.input_entry.text();
        (!text.trim().is_empty()).then(|| text.to_string())
    }

    fn clear_input(&self) {
        self.input_entry.set_text("");
    }

    fn set_running_state(&self, running: bool, interactive: bool) {
        self.start_button.set_sensitive(!running);
        self.stop_button.set_sensitive(running);
        self.runtime_dropdown.set_sensitive(!running);
        self.mode_dropdown.set_sensitive(!running);
        self.status_icon
            .set_icon_name(Some(if running { RUNNING_ICON } else { STOPPED_ICON }));
        self.set_input_enabled(running && interactive);
    }

    fn set_input_enabled(&self, enabled: bool) {
        self.input_entry.set_sensitive(enabled);
        self.send_button.set_sensitive(enabled);
    }

    fn clear_output(&self) {
        self.console_view.clear();
    }

    fn append_output(&self, data: &[u8]) {
        self.console_view.append_output(data);
    }

    fn show_error(&self, heading: &str, body: &str) {
        self.toast_overlay
            .add_toast(adw::Toast::new(&format!("{heading}: {body}")));
    }

    fn show_error_dialog(&self, heading: &str, body: &str) {
        let Some(window) = self
            .toast_overlay
            .root()
            .and_then(|root| root.downcast::<adw::ApplicationWindow>().ok())
        else {
            self.show_error(heading, body);
            return;
        };

        present_alert_dialog(&window, heading, body);
    }
}

fn populate_string_list(model: &gtk4::StringList, values: &[String]) {
    while model.n_items() > 0 {
        model.remove(0);
    }

    for value in values {
        model.append(value);
    }
}

fn select_dropdown_value(dropdown: &gtk4::DropDown, values: &[String], selected: &str) {
    dropdown.set_selected(index_of(values, selected).unwrap_or(0) as u32);
}

fn index_of(items: &[String], target: &str) -> Option<usize> {
    items.iter().position(|item| item == target)
}

fn selected_string(dropdown: &gtk4::DropDown) -> Option<String> {
    dropdown
        .selected_item()
        .and_then(|item| item.downcast::<gtk4::StringObject>().ok())
        .map(|item| item.string().to_string())
}
