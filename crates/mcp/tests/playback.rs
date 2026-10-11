//! Playback through the dedicated player thread. Sandboxed/CI environments often have no
//! audio output device, so a failure to open one must be a clean, actionable error — never
//! a panic or crash — and the test skips the rest of the scenario in that case.

use std::collections::BTreeMap;

use cadenza_mcp::Session;
use cadenza_mcp::convert::DeviceSpec;
use cadenza_mcp::player::PlayerHandle;
use cadenza_mcp::tools::tracks;
use cadenza_model::TrackKind;

#[test]
fn play_stop_round_trip_succeeds_or_fails_cleanly_without_a_device() {
	let handle = match PlayerHandle::start() {
		Ok(handle) => handle,
		Err(err) => {
			// No audio device available in this environment: the error must be clear and
			// the process must not have crashed to get here.
			assert!(!err.to_string().is_empty());
			eprintln!("skipping playback test: {err}");
			return;
		}
	};

	let mut session = Session::new();
	tracks::create_track(
		&mut session,
		tracks::CreateTrackArgs {
			name: "Synth".into(),
			kind: TrackKind::Instrument,
			instrument: Some(DeviceSpec {
				device: "poly_synth".into(),
				params: BTreeMap::new(),
			}),
			color: None,
			output: None,
		},
	)
	.unwrap();
	let project = session.document().project().clone();

	handle
		.load(project, None)
		.expect("loading an empty project should not fail");
	assert!(!handle.is_playing_hint());

	handle
		.play(Some(0.0))
		.expect("play should succeed once a device is open");
	assert!(handle.is_playing_hint());
	let _ = handle
		.position_beats()
		.expect("position should be queryable while playing");

	handle.stop().expect("stop should succeed");
	assert!(!handle.is_playing_hint());
}

#[test]
fn transport_status_tool_without_a_player_reports_stopped() {
	use cadenza_mcp::tools::playback;
	let status = playback::transport_status(None).unwrap();
	assert!(!status.playing);
	assert_eq!(status.position_beats, 0.0);

	let status = playback::stop(None).unwrap();
	assert!(!status.playing);
}
