//! Output must not depend on the host block size.

mod common;

use cadenza_engine::{RenderOptions, render_offline};
use common::*;

#[test]
fn render_is_identical_for_any_block_size() {
	let p = mini_song();
	let base = RenderOptions {
		end: Some(beats(12.0)),
		..RenderOptions::default()
	};
	let reference = render_offline(
		&p,
		&RenderOptions {
			block_size: 512,
			..base.clone()
		},
	)
	.unwrap();
	assert!(peak(&reference.left) > 0.05);
	for block_size in [1, 64, 1000] {
		let out = render_offline(
			&p,
			&RenderOptions {
				block_size,
				..base.clone()
			},
		)
		.unwrap();
		assert_eq!(out, reference, "block size {block_size}");
	}
}

#[test]
fn rendering_is_deterministic() {
	let p = mini_song();
	let a = render_offline(&p, &opts(8.0)).unwrap();
	let b = render_offline(&p, &opts(8.0)).unwrap();
	assert_eq!(a, b);
}
