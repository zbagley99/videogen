//! Output presets: canvas size, frame rate, where the avatar sits, and its motion.

use crate::anim::Motion;
use crate::captions::CaptionStyle;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Where the pose's anchor point lands on the canvas.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct AvatarLayout {
    /// Anchor position as a fraction of canvas width / height.
    pub x: f32,
    pub y: f32,
    /// Height of a pose image (at pose scale 1) as a fraction of canvas height.
    pub height: f32,
    /// Mirror the avatar horizontally (e.g. to face into the frame from the right).
    #[serde(default)]
    pub flip: bool,
    /// Poses placed on their own (by tag); the others use `x`, `y` and `height`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub poses: BTreeMap<String, Spot>,
}

/// One pose's own place on the canvas (same units as [`AvatarLayout`]).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub x: f32,
    pub y: f32,
    pub height: f32,
}

impl AvatarLayout {
    /// The default spot, for poses without their own.
    pub fn default_spot(&self) -> Spot {
        Spot { x: self.x, y: self.y, height: self.height }
    }

    /// Where the pose `tag` goes.
    pub fn spot(&self, tag: &str) -> Spot {
        self.poses.get(tag).copied().unwrap_or_else(|| self.default_spot())
    }

    /// The pose's own spot, made from the default the first time.
    pub fn spot_mut(&mut self, tag: &str) -> &mut Spot {
        let d = self.default_spot();
        self.poses.entry(tag.to_string()).or_insert(d)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub avatar: AvatarLayout,
    #[serde(default)]
    pub motion: Motion,
    #[serde(default)]
    pub captions: CaptionStyle,
}

impl Preset {
    /// 9:16 Shorts: avatar in the lower left.
    pub fn shorts() -> Self {
        Preset {
            name: "shorts".into(),
            width: 1080,
            height: 1920,
            fps: 30,
            avatar: AvatarLayout { x: 0.27, y: 1.0, height: 0.28, ..Default::default() },
            motion: Motion::default(),
            captions: CaptionStyle::default(),
        }
    }

    /// 16:9 long-form: avatar in the lower right.
    pub fn long() -> Self {
        Preset {
            name: "long".into(),
            width: 1920,
            height: 1080,
            fps: 30,
            avatar: AvatarLayout { x: 0.85, y: 1.0, height: 0.46, ..Default::default() },
            motion: Motion::default(),
            captions: CaptionStyle::long_form(),
        }
    }

    pub fn builtin() -> Vec<Preset> {
        vec![Self::shorts(), Self::long()]
    }

    /// A built-in name (`shorts`, `long`) or a path to a `.ron` preset.
    pub fn from_name_or_path(s: &str) -> Result<Self> {
        if let Some(p) = Self::builtin().into_iter().find(|p| p.name == s) {
            return Ok(p);
        }
        let path = Path::new(s);
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("`{s}` is not a built-in preset (shorts, long) or a readable file"))?;
        ron::from_str(&text).with_context(|| format!("parsing {s}"))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::new().struct_names(false))?;
        std::fs::write(path, text + "\n").with_context(|| format!("writing {}", path.display()))
    }
}
