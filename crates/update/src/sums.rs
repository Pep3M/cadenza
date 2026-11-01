//! `SHA256SUMS` as `sha256sum` writes it: `<hex>  <name>` (or `<hex> *<name>`).

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::UpdateError;

/// `(lowercase hash, file name)` per valid line; other lines are skipped.
pub fn parse_sums(text: &str) -> Vec<(String, String)> {
	text.lines()
		.filter_map(|line| {
			let (hash, rest) = line.trim_end().split_once(' ')?;
			let name = rest.trim_start_matches(' ').trim_start_matches('*');
			(hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) && !name.is_empty())
				.then(|| (hash.to_ascii_lowercase(), name.to_string()))
		})
		.collect()
}

/// The hash `SHA256SUMS` lists for `name`.
pub fn expected_hash(sums: &str, name: &str) -> Option<String> {
	parse_sums(sums)
		.into_iter()
		.find(|(_, n)| n == name)
		.map(|(h, _)| h)
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
	let mut file = std::fs::File::open(path)?;
	let mut hasher = Sha256::new();
	let mut buf = vec![0u8; 1 << 16];
	loop {
		let n = file.read(&mut buf)?;
		if n == 0 {
			break;
		}
		hasher.update(&buf[..n]);
	}
	Ok(hasher
		.finalize()
		.iter()
		.map(|b| format!("{b:02x}"))
		.collect())
}

/// Checks `path` (downloaded as `name`) against `sums`.
pub fn verify_file(path: &Path, sums: &str, name: &str) -> Result<(), UpdateError> {
	let expected = expected_hash(sums, name).ok_or_else(|| {
		UpdateError::Install(format!(
			"{name} is not listed in the release's SHA256SUMS, so it cannot be verified; nothing was changed"
		))
	})?;
	let actual = sha256_file(path).map_err(UpdateError::io(format!("reading {name}")))?;
	if actual == expected {
		Ok(())
	} else {
		Err(UpdateError::Checksum(name.to_string()))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// sha256("hello\n")
	const HELLO: &str = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";

	#[test]
	fn parses_both_sha256sum_forms() {
		let text = format!(
			"{HELLO}  a.tar.gz\n{}\n{} *b.dmg\nnot a line\n",
			"x".repeat(64),
			HELLO.to_uppercase()
		);
		let sums = parse_sums(&text);
		assert_eq!(sums.len(), 2);
		assert_eq!(expected_hash(&text, "a.tar.gz").as_deref(), Some(HELLO));
		assert_eq!(expected_hash(&text, "b.dmg").as_deref(), Some(HELLO));
		assert_eq!(expected_hash(&text, "c"), None);
	}

	#[test]
	fn verifies_a_file() {
		let dir = tempfile::tempdir().unwrap();
		let f = dir.path().join("a.tar.gz");
		std::fs::write(&f, "hello\n").unwrap();
		let sums = format!("{HELLO}  a.tar.gz\n");
		verify_file(&f, &sums, "a.tar.gz").unwrap();
		std::fs::write(&f, "tampered\n").unwrap();
		assert!(matches!(
			verify_file(&f, &sums, "a.tar.gz"),
			Err(UpdateError::Checksum(_))
		));
		assert!(matches!(
			verify_file(&f, &sums, "other"),
			Err(UpdateError::Install(_))
		));
	}
}
