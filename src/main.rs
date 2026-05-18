use lib::app::run;

fn main() -> gtk4::glib::ExitCode {
    tracing_subscriber::fmt().with_target(false).init();
    run()
}
