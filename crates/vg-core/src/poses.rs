//! The pose library: one transparent PNG per pose, addressed by tag.
//!
//! `poses.ron` lives next to (or above) the art; `file` paths are relative to it.

use crate::paths::relative_path;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Pose {
    /// Primary tag, e.g. `friendly_wave`.
    pub tag: String,
    /// Extra names that resolve to this pose, e.g. `wave`.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Image path relative to the pose file.
    pub file: PathBuf,
    /// Image size in pixels (filled in by `generate`, refreshed on load).
    #[serde(default)]
    pub size: (u32, u32),
    /// Point of the image (0..1, 0..1 from the top-left) that sits on the layout anchor.
    /// Defaults to the bottom-center of the opaque pixels, so poses don't jump on a switch.
    pub anchor: (f32, f32),
    /// Per-pose size multiplier, for art drawn at a different scale.
    #[serde(default = "one")]
    pub scale: f32,
    /// Nudge in units of the avatar's display height (x right, y down).
    #[serde(default)]
    pub offset: (f32, f32),
    /// What the pose looks like (written by `vg poses describe` or by hand), so
    /// a text-only LLM can pick poses.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Moments in a narration the pose suits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub use_when: Vec<String>,
}

fn one() -> f32 {
    1.0
}

impl Pose {
    /// The name to write in a script: the shortest alias, if it's shorter than the tag.
    pub fn short_name(&self) -> &str {
        self.aliases.iter().min_by_key(|a| a.len()).filter(|a| a.len() < self.tag.len()).unwrap_or(&self.tag)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PoseSet {
    pub poses: Vec<Pose>,
    /// Directory `file` paths are resolved against (not serialized).
    #[serde(skip)]
    pub base: PathBuf,
}

/// Tag name for an image file: `01_friendly_wave.png` -> `friendly_wave`.
pub fn tag_from_file(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    strip_number(&stem).chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// A file stem without its leading number: `01_friendly_wave` -> `friendly_wave`
/// (unless that leaves nothing).
fn strip_number(stem: &str) -> &str {
    let rest = stem.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(['_', '-', ' ']);
    if rest.is_empty() { stem } else { rest }
}

/// The `.png` files in `dir`, sorted; an error if there are none.
fn pngs_in(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
        .collect();
    files.sort();
    if files.is_empty() {
        bail!("no .png files in {}", dir.display());
    }
    Ok(files)
}

impl PoseSet {
    /// The library as JSON for other tools (an LLM prompt, a spreadsheet):
    /// each pose's tag, the shortest name to write, aliases, what it looks
    /// like, when it fits, its number key and its image.
    pub fn summary_json(&self) -> String {
        #[derive(serde::Serialize)]
        struct Entry<'a> {
            tag: &'a str,
            write: String,
            aliases: &'a [String],
            description: &'a str,
            good_for: &'a [String],
            key: Option<usize>,
            image: Option<String>,
        }
        #[derive(serde::Serialize)]
        struct Library<'a> {
            poses: Vec<Entry<'a>>,
            special: [&'static str; 2],
        }
        let poses = self
            .poses
            .iter()
            .enumerate()
            .map(|(i, p)| Entry {
                tag: &p.tag,
                write: format!("[{}]", p.short_name()),
                aliases: &p.aliases,
                description: &p.description,
                good_for: &p.use_when,
                key: (i < 9).then_some(i + 1),
                image: self.path_of(i).file_name().map(|n| n.to_string_lossy().to_string()),
            })
            .collect();
        serde_json::to_string_pretty(&Library { poses, special: ["[hide]", "[show]"] }).unwrap_or_default()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut set: PoseSet = ron::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        set.base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        set.check()?;
        Ok(set)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let pretty = ron::ser::PrettyConfig::new().struct_names(false);
        let text = ron::ser::to_string_pretty(self, pretty)?;
        std::fs::write(path, text + "\n").with_context(|| format!("writing {}", path.display()))
    }

    /// Scan `art_dir` for PNGs and build a pose set whose paths are relative to `pose_file`'s directory.
    pub fn generate(art_dir: &Path, pose_file: &Path) -> Result<Self> {
        let base = pose_file.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut poses = Vec::new();
        for file in &pngs_in(art_dir)? {
            let img = image::open(file).with_context(|| format!("loading {}", file.display()))?.to_rgba8();
            poses.push(Pose {
                tag: tag_from_file(file),
                file: relative_path(file, &base),
                size: img.dimensions(),
                anchor: bottom_center_anchor(&img),
                scale: 1.0,
                ..Default::default()
            });
        }
        let mut set = PoseSet { poses, base };
        set.add_default_aliases();
        set.check()?;
        Ok(set)
    }

    /// Move a folder of new pose PNGs into the art folder (numbered on from the
    /// highest number there), add them to the library and return their indices.
    /// Existing poses (and their tweaks) are untouched.
    pub fn import_folder(&mut self, src: &Path) -> Result<Vec<usize>> {
        let art_dir = match self.poses.first() {
            Some(_) => self.path_of(0).parent().map(Path::to_path_buf).context("no art folder")?,
            None => bail!("the library is empty; use `vg poses init` for the first batch"),
        };
        let number = |p: &Path| -> Option<u32> {
            let name = p.file_name()?.to_string_lossy().to_string();
            name.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
        };
        let first = std::fs::read_dir(&art_dir)?.filter_map(|e| e.ok()).filter_map(|e| number(&e.path())).max().unwrap_or(0) + 1;
        let files = pngs_in(src)?;
        // Check every name first, so nothing moves if one would clash.
        let width = if first + files.len() as u32 > 99 { 3 } else { 2 };
        let mut plan = vec![];
        for (next, f) in (first..).zip(&files) {
            let stem = f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let dest = art_dir.join(format!("{next:0width$}_{}.png", strip_number(&stem)));
            let tag = tag_from_file(&dest);
            if dest.exists() {
                bail!("{} already exists", dest.display());
            }
            if self.find(&tag).is_some() {
                bail!("a pose called `{tag}` is already in the library ({})", f.display());
            }
            plan.push((f.clone(), dest));
        }
        let first_new = self.poses.len();
        for (from, to) in &plan {
            if std::fs::rename(from, to).is_err() {
                std::fs::copy(from, to).with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
                std::fs::remove_file(from)?;
            }
            let img = image::open(to).with_context(|| format!("loading {}", to.display()))?.to_rgba8();
            self.poses.push(Pose {
                tag: tag_from_file(to),
                file: relative_path(to, &self.base),
                size: img.dimensions(),
                anchor: bottom_center_anchor(&img),
                scale: 1.0,
                ..Default::default()
            });
        }
        self.add_aliases_from(first_new);
        self.check()?;
        // Leave no empty batch folder behind.
        let _ = std::fs::remove_dir(src);
        Ok((first_new..self.poses.len()).collect())
    }

    /// Short names for the poses from `first` on, without touching older ones:
    /// the tag minus a category prefix several tags share (`reaction_wait_what`
    /// → `wait_what`), and words of 5+ letters that no other tag uses.
    fn add_aliases_from(&mut self, first: usize) {
        let tags: Vec<String> = self.poses.iter().map(|p| p.tag.clone()).collect();
        let mut word_count: HashMap<String, usize> = HashMap::new();
        let mut prefix_count: HashMap<String, usize> = HashMap::new();
        for tag in &tags {
            for word in tag.split('_') {
                *word_count.entry(word.to_string()).or_default() += 1;
            }
            if let Some((p, _)) = tag.split_once('_') {
                *prefix_count.entry(p.to_string()).or_default() += 1;
            }
        }
        let mut used: std::collections::HashSet<String> = self.poses.iter().flat_map(|p| std::iter::once(p.tag.clone()).chain(p.aliases.iter().cloned())).collect();
        for pose in &mut self.poses[first..] {
            let mut add = |name: String, pose: &mut Pose| {
                if used.insert(name.clone()) {
                    pose.aliases.push(name);
                }
            };
            if let Some((p, rest)) = pose.tag.clone().split_once('_')
                && prefix_count[p] >= 2 && rest.len() >= 4 {
                    add(rest.to_string(), pose);
                }
            // Five letters or more: shorter words ("tiny", "what") make vague tags.
            for word in pose.tag.clone().split('_') {
                if word.len() >= 5 && word_count[word] == 1 {
                    add(word.to_string(), pose);
                }
            }
        }
    }

    /// Give each multi-word tag the aliases of its words that are unambiguous:
    /// `confused_shrug` gets `confused` and `shrug`, but `thumbs_up` doesn't get `up`.
    fn add_default_aliases(&mut self) {
        let tags: Vec<String> = self.poses.iter().map(|p| p.tag.clone()).collect();
        let mut word_count: HashMap<String, usize> = HashMap::new();
        for tag in &tags {
            for word in tag.split('_') {
                *word_count.entry(word.to_string()).or_default() += 1;
            }
        }
        for pose in &mut self.poses {
            if !pose.tag.contains('_') {
                continue;
            }
            for word in pose.tag.split('_') {
                if word.len() >= 4 && word_count[word] == 1 && !tags.iter().any(|t| t == word) {
                    pose.aliases.push(word.to_string());
                }
            }
        }
    }

    /// Duplicate names would make tags ambiguous.
    pub fn check(&self) -> Result<()> {
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for pose in &self.poses {
            for name in std::iter::once(&pose.tag).chain(&pose.aliases) {
                if let Some(other) = seen.insert(name, &pose.tag) {
                    bail!("pose name `{name}` is used by both `{other}` and `{}`", pose.tag);
                }
            }
        }
        Ok(())
    }

    pub fn path_of(&self, index: usize) -> PathBuf {
        self.base.join(&self.poses[index].file)
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        let name = name.to_lowercase();
        self.poses.iter().position(|p| p.tag == name || p.aliases.contains(&name))
    }

    /// Like `find`, but the error names the closest known pose.
    pub fn resolve(&self, name: &str) -> Result<usize> {
        self.find(name).ok_or_else(|| {
            let best = self
                .names()
                .map(|n| (strsim::jaro_winkler(&name.to_lowercase(), n), n))
                .max_by(|a, b| a.0.total_cmp(&b.0));
            match best {
                Some((_, n)) => anyhow::anyhow!("unknown pose `{name}` (did you mean `{n}`?)"),
                None => anyhow::anyhow!("unknown pose `{name}` (the pose set is empty)"),
            }
        })
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.poses.iter().flat_map(|p| std::iter::once(p.tag.as_str()).chain(p.aliases.iter().map(|a| a.as_str())))
    }
}

/// Bottom-center of the pixels with visible alpha, as a fraction of the image size.
pub fn bottom_center_anchor(img: &image::RgbaImage) -> (f32, f32) {
    let (w, h) = img.dimensions();
    let (mut x0, mut x1, mut y1) = (w, 0, 0);
    for (x, y, px) in img.enumerate_pixels() {
        if px[3] > 16 {
            x0 = x0.min(x);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x1 < x0 {
        return (0.5, 1.0);
    }
    (((x0 + x1 + 1) as f32 / 2.0) / w as f32, (y1 + 1) as f32 / h as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_numbers_on_and_keeps_existing_poses() {
        let dir = std::env::temp_dir().join(format!("vg-import-{}", std::process::id()));
        let (art, new) = (dir.join("art"), dir.join("art/new"));
        std::fs::create_dir_all(&new).unwrap();
        let png = |p: &Path| image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 255])).save(p).unwrap();
        png(&art.join("01_friendly_wave.png"));
        png(&art.join("07_victory.png"));
        png(&new.join("01_reaction_wait_what.png"));
        png(&new.join("04_reaction_tiny_victory_dance.png"));
        let lib = dir.join("poses.ron");
        let mut set = PoseSet::generate(&art, &lib).unwrap();
        set.poses[0].scale = 1.5;
        let added = set.import_folder(&new).unwrap();
        assert_eq!(added, vec![2, 3]);
        // In the batch's own order, numbered on from 07.
        assert!(art.join("08_reaction_wait_what.png").exists() && art.join("09_reaction_tiny_victory_dance.png").exists());
        assert!(!new.exists());
        assert_eq!(set.poses[0].scale, 1.5);
        assert_eq!(set.poses[2].tag, "reaction_wait_what");
        assert_eq!(set.poses[2].aliases, vec!["wait_what"]);
        assert_eq!(set.poses[3].aliases, vec!["tiny_victory_dance", "dance"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn set(tags: &[&str]) -> PoseSet {
        let poses = tags
            .iter()
            .map(|t| Pose { tag: t.to_string(), file: PathBuf::from(format!("{t}.png")), size: (10, 10), anchor: (0.5, 1.0), scale: 1.0, ..Default::default() })
            .collect();
        let mut s = PoseSet { poses, base: PathBuf::new() };
        s.add_default_aliases();
        s
    }

    #[test]
    fn tags_from_filenames() {
        assert_eq!(tag_from_file(Path::new("art/01_friendly_wave.png")), "friendly_wave");
        assert_eq!(tag_from_file(Path::new("18_debugging_panic.png")), "debugging_panic");
        assert_eq!(tag_from_file(Path::new("Thumbs Up.png")), "thumbs_up");
        assert_eq!(tag_from_file(Path::new("42.png")), "42");
    }

    #[test]
    fn aliases_skip_ambiguous_and_short_words() {
        let s = set(&["friendly_wave", "thumbs_up", "thinking", "deep_thinking", "confused_shrug"]);
        assert_eq!(s.find("wave"), Some(0));
        assert_eq!(s.find("up"), None);
        assert_eq!(s.find("thinking"), Some(2));
        assert_eq!(s.find("deep"), Some(3));
        assert_eq!(s.find("SHRUG"), Some(4));
    }

    #[test]
    fn resolve_suggests_closest() {
        let s = set(&["facepalm", "victory"]);
        let err = s.resolve("facepalmm").unwrap_err().to_string();
        assert!(err.contains("did you mean `facepalm`"), "{err}");
    }

    #[test]
    fn duplicate_names_rejected() {
        let mut s = set(&["wave", "big_hello"]);
        s.poses[1].aliases.push("wave".into());
        assert!(s.check().is_err());
    }
}
