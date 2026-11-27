//! Data model shared by the GUI, the CLI and the renderer: poses, cue sheets,
//! presets, and the pure `t -> avatar state` evaluation both front-ends use.

pub mod anim;
pub mod autotag;
pub mod captions;
pub mod cues;
pub mod paths;
pub mod poses;
pub mod preset;
pub mod timeline;
pub mod transcript;

pub use captions::CaptionStyle;
pub use anim::{Motion, avatar_at, sprite_quad};
pub use cues::{CueSheet, Item, format_time, parse_time};
pub use poses::{Pose, PoseSet};
pub use preset::{AvatarLayout, Preset};
pub use timeline::{Action, Event, Timeline};
pub use transcript::{Transcript, Word};
