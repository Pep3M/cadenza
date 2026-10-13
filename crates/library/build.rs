//! Embeds the hand-written `core` pack (`packs/core/**/*.json`) into the binary: generates
//! a table of `(path inside the pack, include_str!(file))`.

use std::path::{Path, PathBuf};

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
	let Ok(entries) = std::fs::read_dir(dir) else {
		return;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		if path.is_dir() {
			collect(root, &path, out);
		} else if path.extension().is_some_and(|e| e == "json") {
			let rel = path
				.strip_prefix(root)
				.expect("inside the pack")
				.components()
				.map(|c| c.as_os_str().to_string_lossy().into_owned())
				.collect::<Vec<_>>()
				.join("/");
			out.push((rel, path));
		}
	}
}

fn main() {
	let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
	let root = manifest.join("packs").join("core");
	println!("cargo:rerun-if-changed=packs/core");
	let mut files = Vec::new();
	collect(&root, &root, &mut files);
	files.sort();
	let mut code = String::from("pub(crate) static CORE_FILES: &[(&str, &str)] = &[\n");
	for (rel, path) in &files {
		println!("cargo:rerun-if-changed={}", path.display());
		code.push_str(&format!(
			"\t({rel:?}, include_str!({:?})),\n",
			path.display().to_string()
		));
	}
	code.push_str("];\n");
	let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets it")).join("core_pack.rs");
	std::fs::write(out, code).expect("write the embedded pack table");
}
