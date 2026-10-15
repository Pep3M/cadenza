//! Framing for VST3 state blobs: a VST3 plugin has two independent states
//! (component and edit controller) that Cadenza stores as one opaque blob.
//!
//! Layout: `b"CDZV3"`, a version byte (1), then the component state and the
//! controller state, each prefixed by its length as a little-endian `u32`.

const MAGIC: &[u8; 5] = b"CDZV3";
const VERSION: u8 = 1;

pub(crate) fn encode(component: &[u8], controller: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(MAGIC.len() + 1 + 8 + component.len() + controller.len());
	out.extend_from_slice(MAGIC);
	out.push(VERSION);
	for part in [component, controller] {
		out.extend_from_slice(&(part.len() as u32).to_le_bytes());
		out.extend_from_slice(part);
	}
	out
}

/// Splits a blob into `(component, controller)` states.
pub(crate) fn decode(data: &[u8]) -> Result<(&[u8], &[u8]), String> {
	let rest = data
		.strip_prefix(MAGIC.as_slice())
		.ok_or("not a Cadenza VST3 state blob (bad magic)")?;
	let (&version, mut rest) = rest.split_first().ok_or("truncated state header")?;
	if version != VERSION {
		return Err(format!("unsupported VST3 state version {version}"));
	}
	let mut take = || -> Result<&[u8], String> {
		if rest.len() < 4 {
			return Err("truncated state length".into());
		}
		let (len, tail) = rest.split_at(4);
		let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
		if tail.len() < len {
			return Err("truncated state data".into());
		}
		let (part, tail) = tail.split_at(len);
		rest = tail;
		Ok(part)
	};
	let component = take()?;
	let controller = take()?;
	Ok((component, controller))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trips_and_rejects_garbage() {
		let blob = encode(b"comp", b"");
		assert_eq!(&blob[..6], b"CDZV3\x01");
		assert_eq!(decode(&blob).unwrap(), (&b"comp"[..], &b""[..]));
		assert!(decode(b"CDZV3").is_err());
		assert!(decode(b"XXXXX\x01").is_err());
		assert!(decode(&blob[..blob.len() - 1]).is_err());
		let mut v2 = blob.clone();
		v2[5] = 2;
		assert!(decode(&v2).unwrap_err().contains("version 2"));
	}
}
