//! Embeds the application icon into the Windows executable and compiles the UI
//! translations (`locales/<lang>.toml`) into sorted static tables (`$OUT_DIR/locales.rs`).

use std::fmt::Write as _;
use std::path::Path;

fn main() {
	locales();
	icon();
}

/// Each `locales/<code>.toml` maps an English source string to its translation (a string,
/// or an array of plural forms in the language's CLDR order) and becomes
/// `pub static <CODE>: &[(&str, &[&str])]`, sorted by key for binary search.
fn locales() {
	let dir = Path::new("locales");
	println!("cargo:rerun-if-changed=locales");
	let mut out = String::new();
	let mut files: Vec<_> = std::fs::read_dir(dir)
		.expect("crates/app/locales")
		.flatten()
		.map(|e| e.path())
		.filter(|p| p.extension().is_some_and(|x| x == "toml"))
		.collect();
	files.sort();
	for path in files {
		println!("cargo:rerun-if-changed={}", path.display());
		// The file stem may carry a region suffix to stay unambiguous ("pt-BR"); the
		// generated static needs a valid Rust identifier, so hyphens become underscores.
		let code = path
			.file_stem()
			.unwrap()
			.to_string_lossy()
			.to_uppercase()
			.replace('-', "_");
		let text = std::fs::read_to_string(&path).unwrap();
		let table: toml::Table = text
			.parse()
			.unwrap_or_else(|e| panic!("{}: {e}", path.display()));
		let mut entries: Vec<(String, Vec<String>)> = Vec::new();
		for (key, value) in table {
			let forms = match value {
				toml::Value::String(s) => vec![s],
				toml::Value::Array(a) => a
					.into_iter()
					.map(|v| match v {
						toml::Value::String(s) => s,
						other => panic!(
							"{}: {key:?}: form {other:?} is not a string",
							path.display()
						),
					})
					.collect(),
				other => panic!(
					"{}: {key:?} = {other:?} is not a string or array",
					path.display()
				),
			};
			assert!(
				!forms.is_empty(),
				"{}: {key:?} has no forms",
				path.display()
			);
			entries.push((key, forms));
		}
		entries.sort_by(|a, b| a.0.cmp(&b.0));
		writeln!(out, "pub static {code}: &[(&str, &[&str])] = &[").unwrap();
		for (key, forms) in entries {
			write!(out, "\t({key:?}, &[").unwrap();
			for f in forms {
				write!(out, "{f:?}, ").unwrap();
			}
			writeln!(out, "]),").unwrap();
		}
		writeln!(out, "];").unwrap();
	}
	let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("locales.rs");
	std::fs::write(dest, out).unwrap();
}

fn icon() {
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
