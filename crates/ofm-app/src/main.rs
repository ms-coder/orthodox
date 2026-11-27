//! OFM — Orthodox File Manager (GTK 4 / libadwaita front end). All logic lives in the library.

fn main() -> anyhow::Result<std::process::ExitCode> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    // --help / --version / --dump-keys run without a display and never reach GTK.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(code) = ofm_app::cli::run(&args) {
        return Ok(code);
    }
    ofm_app::app::run()
}
