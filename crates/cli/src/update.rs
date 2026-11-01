//! `cadenza update [--check]`: the app's update check and self-update, from a terminal.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context as _, bail};
use cadenza_update::{
	CURRENT_VERSION, Context, HttpFetcher, InstallKind, Prepared, Progress, RealProbe,
};

pub fn run(check_only: bool) -> anyhow::Result<()> {
	let fetcher = HttpFetcher::new()?;
	let release = cadenza_update::check(&fetcher)?;
	if !release.is_newer_than(CURRENT_VERSION) {
		println!(
			"Cadenza {CURRENT_VERSION} is up to date (latest release: {})",
			release.tag
		);
		return Ok(());
	}
	let latest = release.version().map(|v| v.to_string()).unwrap_or_default();
	println!(
		"Cadenza {latest} is available (this is {CURRENT_VERSION}): {}",
		release.html_url
	);
	if check_only {
		return Ok(());
	}
	let ctx = Context::current().context("finding this executable")?;
	let kind = cadenza_update::detect(&ctx, &RealProbe);
	if let InstallKind::Manual(reason) = kind {
		bail!("cannot update this copy: {}", reason.advice());
	}
	let progress = Arc::new(Progress::default());
	let work = std::env::temp_dir().join(format!("cadenza-update-{}", std::process::id()));
	let worker = {
		let progress = progress.clone();
		let release = release.clone();
		let kind = kind.clone();
		std::thread::spawn(move || {
			cadenza_update::install(&kind, &release, &fetcher, &work, &progress)
		})
	};
	while !worker.is_finished() {
		if let Some(f) = progress.fraction() {
			eprint!("\rdownloading… {:3.0}%", f * 100.0);
		} else {
			let mb = progress.done.load(Ordering::Relaxed) as f64 / 1e6;
			eprint!("\rdownloading… {mb:.1} MB");
		}
		std::thread::sleep(Duration::from_millis(200));
	}
	eprintln!();
	let prepared = worker
		.join()
		.map_err(|_| anyhow::anyhow!("the update worker stopped"))??;
	match &prepared {
		Prepared::Restart { .. } => println!(
			"updated to {latest}; a running Cadenza keeps the old version until it restarts"
		),
		Prepared::RunInstaller { setup } => {
			std::process::Command::new(setup)
				.args(cadenza_update::INSTALLER_ARGS)
				.spawn()
				.context("starting the installer")?;
			println!("started the {latest} installer; it closes a running Cadenza and upgrades it");
		}
		Prepared::OpenFile { file } => {
			cadenza_update::launch(&prepared, None).context("opening the disk image")?;
			println!(
				"opened {}: drag Cadenza.app onto Applications to finish",
				file.display()
			);
		}
	}
	Ok(())
}
