//! OFM — Orthodox File Manager (GTK 4 / libadwaita front end). All logic lives in the library.

fn main() -> anyhow::Result<std::process::ExitCode> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    ofm_app::app::run()
}
