//! `Contents/Resources/moduleinfo.json` (VST3 SDK 3.7.5+): a static
//! description of a bundle's classes. Used to augment what the factory
//! reports; the scanner still loads the module to verify every class.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct ModuleInfo {
	#[serde(rename = "Name", default)]
	pub name: String,
	#[serde(rename = "Version", default)]
	pub version: String,
	#[serde(rename = "Factory Info", default)]
	pub factory: FactoryInfo,
	#[serde(rename = "Classes", default)]
	pub classes: Vec<ClassEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct FactoryInfo {
	#[serde(rename = "Vendor", default)]
	pub vendor: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct ClassEntry {
	/// Class id as 32 hex digits (memory byte order on non-Windows hosts).
	#[serde(rename = "CID", default)]
	pub cid: String,
	#[serde(rename = "Category", default)]
	pub category: String,
	#[serde(rename = "Name", default)]
	pub name: String,
	#[serde(rename = "Vendor", default)]
	pub vendor: String,
	#[serde(rename = "Version", default)]
	pub version: String,
	#[serde(rename = "Sub Categories", default)]
	pub sub_categories: Vec<String>,
}

impl ModuleInfo {
	/// Parses moduleinfo text. The SDK's own parser accepts JSON5, so `//`
	/// comments and trailing commas are tolerated.
	pub fn parse(text: &str) -> Option<ModuleInfo> {
		serde_json::from_str(text)
			.ok()
			.or_else(|| serde_json::from_str(&relax_json(text)).ok())
	}

	pub fn load(path: &Path) -> Option<ModuleInfo> {
		Self::parse(&std::fs::read_to_string(path).ok()?)
	}

	/// The entry for the class id `hex` (as printed by `tuid_to_hex`).
	pub fn class(&self, hex: &str) -> Option<&ClassEntry> {
		self.classes
			.iter()
			.find(|c| c.cid.trim().eq_ignore_ascii_case(hex.trim()))
	}
}

/// Strips `//` line comments and trailing commas outside strings.
fn relax_json(text: &str) -> String {
	let mut out = String::with_capacity(text.len());
	let mut chars = text.chars().peekable();
	let mut in_string = false;
	while let Some(c) = chars.next() {
		if in_string {
			out.push(c);
			if c == '\\' {
				if let Some(n) = chars.next() {
					out.push(n);
				}
			} else if c == '"' {
				in_string = false;
			}
			continue;
		}
		match c {
			'"' => {
				in_string = true;
				out.push(c);
			}
			'/' if chars.peek() == Some(&'/') => {
				for n in chars.by_ref() {
					if n == '\n' {
						out.push('\n');
						break;
					}
				}
			}
			',' => {
				let rest: String = chars.clone().take_while(|c| c.is_whitespace()).collect();
				let next = chars.clone().nth(rest.chars().count());
				if !matches!(next, Some('}') | Some(']')) {
					out.push(c);
				}
			}
			_ => out.push(c),
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	const FIXTURE: &str = r#"{
	"Name": "lsp-plugins",
	"Version": "1.2.35",
	"Factory Info": {
		"Vendor": "LSP VST3",
		"URL": "https://lsp-plug.in/",
		"Flags": { "Unicode": true }
	},
	"Compatibility": [],
	"Classes": [
		{
			"CID": "64737020616270326D2020206162326D",
			"Category": "Audio Module Class",
			"Name": "A/B Tester x2 Mono",
			"Vendor": "LSP VST3",
			"Version": "1.0.27",
			"SDKVersion": "VST 3.8.0",
			"Sub Categories": ["Fx", "Tools", "Mono"],
			"Class Flags": 1,
			"Cardinality": 2147483647,
			"Snapshots": []
		},
		{
			"CID": "41424344454647484950515253545556",
			"Category": "Component Controller Class",
			"Name": "A/B Tester x2 Mono"
		}
	]
}"#;

	#[test]
	fn parses_sdk_moduleinfo() {
		let info = ModuleInfo::parse(FIXTURE).unwrap();
		assert_eq!(info.name, "lsp-plugins");
		assert_eq!(info.factory.vendor, "LSP VST3");
		assert_eq!(info.classes.len(), 2);
		let c = info.class("64737020616270326d2020206162326d").unwrap();
		assert_eq!(c.category, "Audio Module Class");
		assert_eq!(c.sub_categories, ["Fx", "Tools", "Mono"]);
		assert_eq!(c.version, "1.0.27");
		assert!(info.class("00000000000000000000000000000000").is_none());
	}

	#[test]
	fn tolerates_json5_comments_and_trailing_commas() {
		let text = r#"{
			// generated
			"Name": "x, y", // trailing
			"Classes": [ { "CID": "AB", "Name": "a//b", }, ],
		}"#;
		let info = ModuleInfo::parse(text).unwrap();
		assert_eq!(info.name, "x, y");
		assert_eq!(info.classes[0].name, "a//b");
		assert!(ModuleInfo::parse("not json").is_none());
	}
}
