# Model weights

`basic_pitch.bin`: the weights of **Basic Pitch** (Bittner, Bosch, Rubinstein, Meseguer-Brocal,
Ewert: "A Lightweight Instrument-Agnostic Model for Polyphonic Note Transcription and
Multipitch Estimation", ICASSP 2022), © 2022 Spotify AB, licensed under the Apache License 2.0
(`LICENSE-basic-pitch`). Source: `basic_pitch/saved_models/icassp_2022/nmp.onnx` of
https://github.com/spotify/basic-pitch (main at fa5997af, ONNX sha256
2c3c1d144bfa61ad236e92e169c13535c880469a12a047d4e73451f2c059a0ec).

Modification: the tensors were copied out of the ONNX graph unchanged into a flat
little-endian f32 file by `tools/basic-pitch/export.py` (layout documented there); the network
is re-implemented in Rust in `crates/notes/src/poly.rs` and matches the ONNX outputs within
2e-4 (`one_window_matches_the_onnx_model`, run with `CADENZA_BASIC_PITCH_REFERENCE`).
