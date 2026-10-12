//! Cadenza desktop application (egui/eframe, wgpu renderer).
//!
//! Layers:
//! - [`state`]: [`state::AppState`] and pure editing logic (no window; unit-tested).
//! - [`audio`]: the realtime player on its own thread, polled without blocking.
//! - [`jobs`]: background work (waveform peaks, plugin scans, renders, file dialogs).
//! - [`ui`]: egui views that read the state and call its methods; every edit becomes a
//!   `cadenza_model::Command` through `AppState::apply`.

pub mod audio;
pub mod jobs;
pub mod state;
pub mod theme;
pub mod ui;

use std::path::PathBuf;

pub use ui::CadenzaApp;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
	#[error("could not start the window: {0}")]
	Window(String),
}

/// Opens the main window (optionally with a project) and runs until it is closed.
pub fn run(project: Option<PathBuf>) -> Result<(), AppError> {
	let options = eframe::NativeOptions {
		renderer: eframe::Renderer::Wgpu,
		viewport: egui::ViewportBuilder::default()
			.with_title("Cadenza")
			.with_app_id("cadenza")
			.with_inner_size([1480.0, 920.0])
			.with_min_inner_size([960.0, 600.0]),
		..Default::default()
	};
	eframe::run_native(
		"Cadenza",
		options,
		Box::new(move |cc| Ok(Box::new(CadenzaApp::new(&cc.egui_ctx, project)))),
	)
	.map_err(|e| AppError::Window(e.to_string()))
}
