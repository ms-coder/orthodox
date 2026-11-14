//! OFM — Orthodox File Manager (GTK 4 / libadwaita front end).
use adw::prelude::*;

const APP_ID: &str = "io.github.mscoder.Ofm";

fn main() -> anyhow::Result<std::process::ExitCode> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("OFM")
            .default_width(1200)
            .default_height(800)
            .content(
                &adw::StatusPage::builder()
                    .title("OFM")
                    .description("Orthodox File Manager")
                    .build(),
            )
            .build();
        window.present();
    });
    Ok(app.run().into())
}
