//! `scan_plugins` end to end: the out-of-process scan, the saved cache, the blocklist
//! report, and the session picking up the new catalog. One test per binary because it
//! sets the scanner environment variable.

mod plugin_support;

use std::sync::Mutex;

use cadenza_mcp::Session;
use cadenza_mcp::tools::devices;
use cadenza_plugin_host::SCANNER_ENV;

#[tokio::test]
async fn scan_plugins_updates_cache_and_session() {
	let scanner = plugin_support::scanner();
	// SAFETY: the only test in this binary, set before any other thread reads the env.
	unsafe { std::env::set_var(SCANNER_ENV, &scanner) };

	let plugins = plugin_support::plugin_dir("scan");
	std::fs::write(plugins.join("broken.clap"), b"not a plugin").unwrap();
	let cache = tempfile::tempdir().unwrap();
	let cache_path = cache.path().join("plugins.json");
	let session = Mutex::new(Session::with_plugin_cache(&cache_path));
	assert_eq!(session.lock().unwrap().plugins().descriptors().count(), 0);

	let out = devices::scan_plugins(
		&session,
		devices::ScanPluginsArgs {
			paths: Some(vec![plugins.display().to_string()]),
		},
	)
	.await
	.unwrap();
	assert_eq!((out.files_found, out.files_scanned), (2, 2));
	assert_eq!((out.added, out.total_plugins), (2, 2));
	assert_eq!((out.instruments, out.effects), (1, 1));
	assert_eq!(out.blocked.len(), 1);
	assert!(out.blocked[0].path.ends_with("broken.clap"));
	assert!(!out.blocked[0].reason.is_empty());
	assert!(cache_path.is_file());

	let catalog = session.lock().unwrap().plugins();
	assert_eq!(catalog.descriptors().count(), 2);
	let found = devices::list_devices(
		Some(&catalog),
		serde_json::from_value(serde_json::json!({ "query": "sine", "source": "plugin" })).unwrap(),
	);
	assert_eq!(found.devices[0].id, "clap:dev.cadenza.test.sine");

	// Rescans skip unchanged and blocked files.
	let again = devices::scan_plugins(
		&session,
		devices::ScanPluginsArgs {
			paths: Some(vec![plugins.display().to_string()]),
		},
	)
	.await
	.unwrap();
	assert_eq!((again.files_scanned, again.blocked_total), (0, 1));
}
