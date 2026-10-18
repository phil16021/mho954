//! Live view: a thin eframe client of the `mho954` REST/WebSocket API.

mod app;
mod bode;
mod generator;
mod plot_tools;
mod prefs;
mod settings;
mod state;
mod tools;

use std::path::PathBuf;

pub use app::ScopeApp;

/// Runs the GUI on the current (main) thread. `url` points at a running server; `rt` drives
/// the network tasks.
pub fn run(
    url: String,
    token: Option<String>,
    rt: tokio::runtime::Handle,
    capture_dir: PathBuf,
) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("MHO954")
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([900.0, 600.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "mho954",
        options,
        Box::new(move |cc| Ok(Box::new(ScopeApp::new(cc, url, token, rt, capture_dir)))),
    )
    .map_err(|e| anyhow::anyhow!("GUI failed: {e} (under xrdp try LIBGL_ALWAYS_SOFTWARE=1)"))
}
