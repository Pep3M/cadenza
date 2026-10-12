//! Crash-isolated plugin host process.
//!
//! Started by `cadenza-plugin-host` (one process per sandboxed plugin
//! instance) with `--shm <name> --parent <pid>`; it loads the plugin, then
//! processes audio through the shared memory block and answers control
//! requests on stdin/stdout. If the plugin crashes, only this process dies.

fn main() -> std::process::ExitCode {
	cadenza_plugin_host::sandbox::run_sandbox_child()
}
