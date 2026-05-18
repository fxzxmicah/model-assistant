use lib::runner::keeper::run_keeper;

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt().with_target(false).init();
    run_keeper()
}
