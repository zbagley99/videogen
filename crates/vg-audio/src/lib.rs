//! Narration clean-up for videogen: cut a recording, run it through a voice
//! chain (high-pass, noise reduction, gate, EQ, de-esser, compressor), mix in
//! other tracks, and master to a loudness target. All offline, all in Rust.

pub mod denoise;
pub mod dsp;
pub mod edit;
pub mod render;

pub use edit::{Adjust, Effect, EqBand, Fx, Mix, Preset, RATE, Region, Track};
pub use render::{Cache, Rendered, render, write_wav};
