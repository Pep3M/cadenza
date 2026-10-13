//! Tool logic, as plain functions over a [`crate::session::Session`]. Each function is
//! independently testable; `server.rs` only adapts these to the MCP tool-call protocol.

pub mod arrange;
pub mod audio;
pub mod clips;
pub mod compose;
pub mod controllers;
pub mod devices;
pub mod midi;
pub mod playback;
pub mod portable;
pub mod presets;
pub mod project;
pub mod render;
pub mod samples;
pub mod tracks;
