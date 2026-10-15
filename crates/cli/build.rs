//! Embeds the application icon into the Windows executable.

fn main() {
	const ICON: &str = "../../packaging/icons/cadenza.ico";
	println!("cargo:rerun-if-changed={ICON}");
	if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
		return;
	}
	let mut res = winresource::WindowsResource::new();
	res.set_icon(ICON);
	// Cross-checks from other hosts may lack a resource compiler: the binary just has no icon.
	if let Err(e) = res.compile() {
		println!("cargo:warning=Windows icon not embedded: {e}");
	}
}
