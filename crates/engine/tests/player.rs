//! Realtime player smoke test. Skips when the machine has no usable audio output.

mod common;

use cadenza_core::Tick;
use cadenza_engine::Player;
use common::*;

#[test]
fn player_controls_work_when_a_device_exists() {
	let mut player = match Player::new() {
		Ok(p) => p,
		Err(e) => {
			eprintln!("skipping player test: {e}");
			return;
		}
	};
	// A silent project: exercises the stream without making noise.
	let warnings = player.load(&project(vec![])).unwrap();
	assert!(warnings.is_empty());
	assert!(player.sample_rate() >= 8_000);
	assert!(!player.is_playing());
	player.seek(beats(8.0)).unwrap();
	assert_eq!(player.position(), beats(8.0));
	player.play().unwrap();
	assert!(player.is_playing());
	std::thread::sleep(std::time::Duration::from_millis(150));
	player.stop().unwrap();
	assert!(!player.is_playing());
	assert!(player.position() >= beats(8.0));
	player.load(&mini_song()).unwrap();
	player.seek(Tick::ZERO).unwrap();
}
