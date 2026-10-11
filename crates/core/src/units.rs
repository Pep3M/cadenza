pub fn db_to_gain(db: f32) -> f32 {
	if db <= -144.0 {
		0.0
	} else {
		10f32.powf(db / 20.0)
	}
}

pub fn gain_to_db(gain: f32) -> f32 {
	if gain <= 0.0 {
		-144.0
	} else {
		20.0 * gain.log10()
	}
}
