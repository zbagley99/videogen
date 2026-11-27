//! The audio edit (`.vgmix`, RON): tracks made of clips cut from source files,
//! each with an effect chain, summed and mastered. Nothing touches the sources;
//! rendering writes a new file.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Everything is rendered at this rate, mono.
pub const RATE: u32 = 48_000;

/// A stretch of the source, in source seconds.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    pub start: f64,
    pub end: f64,
}

impl Clip {
    pub fn len(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct EqBand {
    pub freq: f32,
    pub gain_db: f32,
    /// Bandwidth of the two middle (peaking) bands; unused by the shelves.
    pub q: f32,
}

/// An effect and its settings. The chain runs top to bottom.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Fx {
    /// Removes rumble, handling noise and plosive thumps below `freq`.
    HighPass { freq: f32 },
    /// Spectral noise reduction. `room_tone` (source seconds) is a stretch of
    /// just room noise to learn from; otherwise the quietest moments are used.
    Denoise { reduction_db: f32, threshold_db: f32, room_tone: Option<(f64, f64)> },
    /// Turns pauses (breaths, room) down by `range_db`. It opens when the level
    /// is `above_floor_db` over the noise floor, measured where it sits in the chain.
    Gate { above_floor_db: f32, range_db: f32, release_ms: f32 },
    /// Low shelf, two peaking bands, high shelf.
    Eq { bands: [EqBand; 4] },
    /// Tames harsh "s" and "t" sounds above `freq`.
    DeEsser { freq: f32, threshold_db: f32, max_db: f32 },
    /// Evens out the level.
    Compressor { threshold_db: f32, ratio: f32, attack_ms: f32, release_ms: f32 },
}

impl Fx {
    pub fn name(&self) -> &'static str {
        match self {
            Fx::HighPass { .. } => "High-pass",
            Fx::Denoise { .. } => "Noise reduction",
            Fx::Gate { .. } => "Gate",
            Fx::Eq { .. } => "EQ",
            Fx::DeEsser { .. } => "De-esser",
            Fx::Compressor { .. } => "Compressor",
        }
    }

    /// Defaults for a narration voice leveled to [`LEVEL_LUFS`].
    pub fn defaults() -> Vec<Fx> {
        vec![
            Fx::HighPass { freq: 80.0 },
            Fx::Denoise { reduction_db: 18.0, threshold_db: 6.0, room_tone: None },
            Fx::Eq {
                bands: [
                    EqBand { freq: 120.0, gain_db: 0.0, q: 0.7 },
                    EqBand { freq: 300.0, gain_db: -3.0, q: 1.0 },
                    EqBand { freq: 3500.0, gain_db: 2.0, q: 1.0 },
                    EqBand { freq: 10_000.0, gain_db: 2.0, q: 0.7 },
                ],
            },
            Fx::DeEsser { freq: 6000.0, threshold_db: -32.0, max_db: 6.0 },
            Fx::Compressor { threshold_db: -24.0, ratio: 3.0, attack_ms: 8.0, release_ms: 120.0 },
            // Last, so it also catches the room the compressor brings up.
            Fx::Gate { above_floor_db: 8.0, range_db: 12.0, release_ms: 180.0 },
        ]
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Effect {
    pub on: bool,
    pub fx: Fx,
}

/// A change to part of a track, applied after its effects (so the compressor
/// doesn't undo it).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum Adjust {
    /// Louder or quieter by this many dB.
    Gain(f32),
    /// Silent (the length stays; a cut removes it instead).
    Mute,
    /// From silence up to full level across the region.
    FadeIn,
    /// From full level down to silence.
    FadeOut,
}

impl Adjust {
    pub fn label(&self) -> String {
        match self {
            Adjust::Gain(db) => format!("{db:+.1} dB"),
            Adjust::Mute => "Muted".into(),
            Adjust::FadeIn => "Fade in".into(),
            Adjust::FadeOut => "Fade out".into(),
        }
    }

    /// Gain and mute set the level; fades shape it. A new one replaces older
    /// ones of the same family where they overlap.
    fn is_level(&self) -> bool {
        matches!(self, Adjust::Gain(_) | Adjust::Mute)
    }
}

/// An [`Adjust`] over a stretch of the source (source seconds, so it stays on
/// the same words when earlier audio is cut).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub start: f64,
    pub end: f64,
    pub adjust: Adjust,
}

/// A background music track: it plays under the voice until the narration
/// ends (looping if it's shorter), fading in and out and ducking while you talk.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Music {
    /// How far it dips while the voice is talking (dB).
    pub duck_db: f32,
    pub loop_it: bool,
    /// Seconds.
    pub fade_in: f64,
    pub fade_out: f64,
}

impl Default for Music {
    fn default() -> Self {
        Music { duck_db: 10.0, loop_it: true, fade_in: 1.0, fade_out: 2.5 }
    }
}

/// Music plays on this long after the last word.
pub const MUSIC_TAIL: f64 = 1.5;

/// Edges of gain and mute regions ramp over this long, so they don't click.
const EDGE: f64 = 0.006;

/// Before the chain, each track is brought to this loudness so the effects'
/// thresholds mean the same thing however hot the mic was.
pub const LEVEL_LUFS: f32 = -20.0;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Track {
    pub name: String,
    /// Relative to the `.vgmix` file's folder (or absolute).
    pub source: PathBuf,
    /// Where the track starts in the mix (seconds).
    pub offset: f64,
    /// Kept stretches of the source, played back to back.
    pub clips: Vec<Clip>,
    pub gain_db: f32,
    pub mute: bool,
    pub solo: bool,
    /// Level the track to [`LEVEL_LUFS`] before the effects.
    pub level: bool,
    pub effects: Vec<Effect>,
    /// Muted, louder or faded stretches.
    pub regions: Vec<Region>,
    /// Set for a background music track.
    pub music: Option<Music>,
}

impl Default for Track {
    fn default() -> Self {
        Track { name: String::new(), source: PathBuf::new(), offset: 0.0, clips: vec![], gain_db: 0.0, mute: false, solo: false, level: true, effects: vec![], regions: vec![], music: None }
    }
}

impl Track {
    pub fn new(name: &str, source: PathBuf, duration: f64) -> Self {
        Track { name: name.into(), source, clips: vec![Clip { start: 0.0, end: duration }], ..Default::default() }
    }

    /// Length after cuts.
    pub fn length(&self) -> f64 {
        self.clips.iter().map(Clip::len).sum()
    }

    pub fn end(&self) -> f64 {
        self.offset + self.length()
    }

    /// Mix times where one clip meets the next (the cuts).
    pub fn joins(&self) -> Vec<f64> {
        let mut t = self.offset;
        let mut out = vec![];
        for (i, c) in self.clips.iter().enumerate() {
            t += c.len();
            if i + 1 < self.clips.len() {
                out.push(t);
            }
        }
        out
    }

    /// Remove mix time `a..b` from this track; later audio moves up.
    pub fn cut(&mut self, a: f64, b: f64) {
        let (a, b) = (a.min(b) - self.offset, a.max(b) - self.offset);
        if b <= 0.0 || a >= self.length() || b - a < 1e-4 {
            return;
        }
        let mut out = vec![];
        let mut t = 0.0;
        for c in &self.clips {
            let (s, e) = (t, t + c.len());
            // The part before the cut, then the part after it.
            if s < a {
                out.push(Clip { start: c.start, end: c.start + (a.min(e) - s) });
            }
            if e > b {
                out.push(Clip { start: c.start + (b.max(s) - s), end: c.end });
            }
            t = e;
        }
        out.retain(|c| c.len() > 1e-4);
        self.clips = out;
    }

    /// Split the clip playing at mix time `t` in two (a cut that removes
    /// nothing). `false` if no clip plays there, or `t` is on a clip's edge.
    pub fn split(&mut self, t: f64) -> bool {
        let local = t - self.offset;
        let mut pos = 0.0;
        for i in 0..self.clips.len() {
            let c = self.clips[i];
            if local > pos + 1e-3 && local < pos + c.len() - 1e-3 {
                let s = c.start + (local - pos);
                self.clips[i].end = s;
                self.clips.insert(i + 1, Clip { start: s, end: c.end });
                return true;
            }
            pos += c.len();
        }
        false
    }

    /// Where source time `t` plays in the mix, or `None` if it was cut.
    pub fn src_to_out(&self, t: f64) -> Option<f64> {
        let mut pos = self.offset;
        for c in &self.clips {
            if t >= c.start && t <= c.end {
                return Some(pos + t - c.start);
            }
            pos += c.len();
        }
        None
    }

    /// Like [`Track::src_to_out`], but a time inside a cut lands on the cut.
    pub fn src_to_out_snapped(&self, t: f64) -> f64 {
        let mut pos = self.offset;
        for c in &self.clips {
            if t < c.start {
                return pos;
            }
            if t <= c.end {
                return pos + t - c.start;
            }
            pos += c.len();
        }
        pos
    }

    /// The stretches of source that play during mix time `a..b` (source seconds).
    pub fn pieces(&self, a: f64, b: f64) -> Vec<(f64, f64)> {
        let mut out = vec![];
        let mut pos = self.offset;
        for c in &self.clips {
            let (s, e) = (a.max(pos), b.min(pos + c.len()));
            if e > s {
                out.push((c.start + s - pos, c.start + e - pos));
            }
            pos += c.len();
        }
        out
    }

    /// Mix time `a..b` as a source stretch: from the first source moment
    /// playing in it to the last. `None` if nothing of the track plays there.
    pub fn src_span(&self, a: f64, b: f64) -> Option<(f64, f64)> {
        let p = self.pieces(a.min(b), a.max(b));
        Some((p.first()?.0, p.last()?.1))
    }

    /// Where a region plays now (mix seconds). A region whose audio was cut
    /// entirely has no length.
    pub fn region_span(&self, r: &Region) -> (f64, f64) {
        (self.src_to_out_snapped(r.start), self.src_to_out_snapped(r.end))
    }

    /// Apply `adjust` to mix time `a..b`. It replaces what it overlaps of the
    /// same family (a new gain or mute over an old one; a fade over a fade).
    pub fn adjust(&mut self, a: f64, b: f64, adjust: Adjust) -> bool {
        let Some((s, e)) = self.src_span(a, b) else { return false };
        if e - s < 1e-3 {
            return false;
        }
        self.clear_regions(s, e, Some(adjust.is_level()));
        if !matches!(adjust, Adjust::Gain(g) if g.abs() < 0.05) {
            self.regions.push(Region { start: s, end: e, adjust });
            self.regions.sort_by(|x, y| x.start.total_cmp(&y.start));
        }
        true
    }

    /// Remove adjustments in mix time `a..b` (level ones are trimmed to what's
    /// outside it; fades that overlap it go).
    pub fn reset(&mut self, a: f64, b: f64) -> bool {
        let Some((s, e)) = self.src_span(a, b) else { return false };
        let old = self.regions.clone();
        self.clear_regions(s, e, None);
        old != self.regions
    }

    /// Clear source `s..e` of regions: level ones (`Some(true)`), fades
    /// (`Some(false)`) or both (`None`).
    fn clear_regions(&mut self, s: f64, e: f64, family: Option<bool>) {
        let mut out = vec![];
        for r in self.regions.drain(..) {
            let hit = r.end > s && r.start < e && family.is_none_or(|lvl| r.adjust.is_level() == lvl);
            if !hit {
                out.push(r);
            } else if r.adjust.is_level() {
                if r.start < s {
                    out.push(Region { end: s, ..r });
                }
                if r.end > e {
                    out.push(Region { start: e, ..r });
                }
            }
        }
        self.regions = out;
    }

    /// The regions' combined gain for each sample of the track's output
    /// (track-local time), or `None` if there are none.
    pub fn envelope(&self, len: usize, rate: f64) -> Option<Vec<f32>> {
        if self.regions.is_empty() {
            return None;
        }
        let mut env = vec![1.0f32; len];
        let edge = (EDGE * rate).max(1.0);
        for r in &self.regions {
            let (a, b) = self.region_span(r);
            let (a, b) = ((a - self.offset) * rate, (b - self.offset) * rate);
            let (i0, i1) = ((a.floor().max(0.0) as usize).min(len), (b.ceil().max(0.0) as usize).min(len));
            if i1 <= i0 {
                continue;
            }
            for (i, v) in env[i0..i1].iter_mut().enumerate() {
                let t = (i0 + i) as f64;
                let x = ((t - a) / (b - a).max(1.0)).clamp(0.0, 1.0) as f32;
                let g = match r.adjust {
                    Adjust::FadeIn => (std::f32::consts::FRAC_PI_2 * x).sin(),
                    Adjust::FadeOut => (std::f32::consts::FRAC_PI_2 * x).cos(),
                    Adjust::Gain(_) | Adjust::Mute => {
                        let level = match r.adjust {
                            Adjust::Gain(db) => crate::dsp::db_to_gain(db),
                            _ => 0.0,
                        };
                        // Ramp in and out at the edges.
                        let k = (((t - a) / edge).min((b - t) / edge)).clamp(0.0, 1.0) as f32;
                        let k = k * k * (3.0 - 2.0 * k);
                        1.0 + (level - 1.0) * k
                    }
                };
                *v *= g;
            }
        }
        Some(env)
    }

    /// The source time playing at mix time `t`.
    pub fn out_to_src(&self, t: f64) -> Option<f64> {
        let mut pos = self.offset;
        for c in &self.clips {
            if t >= pos && t <= pos + c.len() {
                return Some(c.start + t - pos);
            }
            pos += c.len();
        }
        None
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Master {
    /// Integrated loudness target (LUFS); YouTube plays back at about -14.
    pub loudness: Option<f32>,
    pub limiter: bool,
    /// Sample peak ceiling (dBFS).
    pub ceiling_db: f32,
}

impl Default for Master {
    fn default() -> Self {
        Master { loudness: Some(-14.0), limiter: true, ceiling_db: -1.0 }
    }
}

/// The voice track's clips when the script's timings were last matched to the
/// mix, so later cuts can move them again.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Synced {
    pub track: usize,
    pub offset: f64,
    pub clips: Vec<Clip>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Mix {
    pub tracks: Vec<Track>,
    pub master: Master,
    pub synced: Option<Synced>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// For a cardioid condenser like the AT2020: rumble and room cut, less
    /// boxiness, a little presence and air, sibilance tamed, gently levelled.
    Condenser,
    /// Lighter touch: for a quiet, treated room.
    Gentle,
    /// More compression and presence, for fast, punchy narration.
    Punchy,
    /// Everything off (still levelled and mastered).
    Off,
}

impl Preset {
    pub const ALL: [Preset; 4] = [Preset::Condenser, Preset::Gentle, Preset::Punchy, Preset::Off];

    pub fn name(self) -> &'static str {
        match self {
            Preset::Condenser => "Condenser voice (AT2020)",
            Preset::Gentle => "Gentle",
            Preset::Punchy => "Punchy",
            Preset::Off => "All off",
        }
    }

    /// A preset by its command-line name (condenser, gentle, punchy, off) or
    /// the start of its display name.
    pub fn parse(s: &str) -> Option<Preset> {
        let s = s.trim().to_lowercase();
        let key = |p: Preset| match p {
            Preset::Condenser => "condenser",
            Preset::Gentle => "gentle",
            Preset::Punchy => "punchy",
            Preset::Off => "off",
        };
        Self::ALL.into_iter().find(|&p| key(p) == s).or_else(|| Self::ALL.into_iter().find(|p| !s.is_empty() && p.name().to_lowercase().starts_with(&s)))
    }

    pub fn effects(self) -> Vec<Effect> {
        let mut fx = Fx::defaults();
        match self {
            Preset::Condenser => {}
            Preset::Gentle => {
                for f in &mut fx {
                    match f {
                        Fx::Denoise { reduction_db, .. } => *reduction_db = 10.0,
                        Fx::Gate { range_db, .. } => *range_db = 6.0,
                        Fx::Eq { bands } => {
                            bands[1].gain_db = -2.0;
                            bands[2].gain_db = 1.0;
                            bands[3].gain_db = 1.0;
                        }
                        Fx::Compressor { ratio, threshold_db, .. } => {
                            *ratio = 2.0;
                            *threshold_db = -22.0;
                        }
                        _ => {}
                    }
                }
            }
            Preset::Punchy => {
                for f in &mut fx {
                    match f {
                        Fx::HighPass { freq } => *freq = 100.0,
                        Fx::Gate { range_db, .. } => *range_db = 18.0,
                        Fx::Eq { bands } => {
                            bands[0] = EqBand { freq: 150.0, gain_db: 1.5, q: 0.7 };
                            bands[1].gain_db = -4.0;
                            bands[2].gain_db = 3.0;
                            bands[3].gain_db = 3.0;
                        }
                        Fx::DeEsser { max_db, .. } => *max_db = 8.0,
                        Fx::Compressor { ratio, threshold_db, attack_ms, .. } => {
                            *ratio = 4.5;
                            *threshold_db = -26.0;
                            *attack_ms = 4.0;
                        }
                        _ => {}
                    }
                }
            }
            Preset::Off => {}
        }
        fx.into_iter().map(|fx| Effect { on: self != Preset::Off, fx }).collect()
    }
}

impl Mix {
    /// A new edit of one recording, with the condenser-voice chain.
    pub fn for_recording(source: PathBuf, duration: f64) -> Self {
        let name = source.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "voice".into());
        let mut t = Track::new(&name, source, duration);
        t.effects = Preset::Condenser.effects();
        Mix { tracks: vec![t], ..Default::default() }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        ron::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default().struct_names(false))?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    /// The mix's length: the voice tracks' (plus a little tail when there's
    /// music, which is trimmed or looped to fit), or the music's on its own.
    pub fn length(&self) -> f64 {
        let voice = self.voice_end();
        if voice > 0.0 {
            voice + if self.tracks.iter().any(|t| t.music.is_some()) { MUSIC_TAIL } else { 0.0 }
        } else {
            self.tracks.iter().map(Track::end).fold(0.0, f64::max)
        }
    }

    /// Where the last voice (non-music) track ends.
    pub fn voice_end(&self) -> f64 {
        self.tracks.iter().filter(|t| t.music.is_none()).map(Track::end).fold(0.0, f64::max)
    }

    /// Whether track `i` is heard (mute and solo).
    pub fn audible(&self, i: usize) -> bool {
        let any_solo = self.tracks.iter().any(|t| t.solo);
        let t = &self.tracks[i];
        !t.mute && (!any_solo || t.solo)
    }

    /// Map a time on the script's clock to the current mix. The script's clock is
    /// the mix as last synced, or before any sync, the raw recording on track 0.
    /// Times inside a cut land on the cut.
    pub fn retime(&self, t: f64) -> f64 {
        let (track, src) = match &self.synced {
            Some(s) => {
                let old = Track { offset: s.offset, clips: s.clips.clone(), ..Default::default() };
                (s.track, old.out_to_src(t).unwrap_or(t))
            }
            None => (0, t),
        };
        self.tracks.get(track).map(|tr| tr.src_to_out_snapped(src)).unwrap_or(t)
    }

    /// Record that the script now matches the current cuts of `track`.
    pub fn mark_synced(&mut self, track: usize) {
        if let Some(t) = self.tracks.get(track) {
            self.synced = Some(Synced { track, offset: t.offset, clips: t.clips.clone() });
        }
    }
}

/// Pauses in a track's audio (track-local seconds): stretches at least
/// `min_len` long that stay near the recording's noise floor.
pub fn pauses(buf: &[f32], rate: f32, min_len: f64) -> Vec<(f64, f64)> {
    const FRAME: f32 = 0.02;
    let db = crate::dsp::rms_frames(buf, rate, FRAME);
    if db.is_empty() {
        return vec![];
    }
    let mut sorted: Vec<f32> = db.iter().copied().filter(|d| *d > -115.0).collect();
    if sorted.is_empty() {
        return vec![];
    }
    sorted.sort_by(f32::total_cmp);
    let floor = sorted[sorted.len() / 10];
    let loud = sorted[sorted.len() * 95 / 100];
    // Between the floor and speech, nearer the floor.
    let thresh = floor + ((loud - floor) * 0.3).clamp(6.0, 20.0);
    let mut out = vec![];
    let mut start = None;
    for (i, &d) in db.iter().chain(std::iter::once(&f32::INFINITY)).enumerate() {
        let t = i as f64 * FRAME as f64;
        match (d < thresh, start) {
            (true, None) => start = Some(t),
            (false, Some(s)) => {
                if t - s >= min_len {
                    out.push((s, t.min(buf.len() as f64 / rate as f64)));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// Shorten every pause longer than `max_len` in `track` to `max_len` (keeping
/// equal breathing room on each side). `buf` is the track's audio as it is now.
/// Returns how many pauses were shortened.
pub fn tighten(track: &mut Track, buf: &[f32], rate: f32, max_len: f64) -> usize {
    let found: Vec<(f64, f64)> = pauses(buf, rate, max_len + 0.05).into_iter().filter(|(a, b)| b - a > max_len).collect();
    // From the end, so earlier times stay valid.
    for &(a, b) in found.iter().rev() {
        let keep = max_len / 2.0;
        track.cut(track.offset + a + keep, track.offset + b - keep);
    }
    found.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_parse_by_their_command_line_names() {
        assert_eq!(Preset::parse("off"), Some(Preset::Off));
        assert_eq!(Preset::parse("Condenser"), Some(Preset::Condenser));
        assert_eq!(Preset::parse("gentle"), Some(Preset::Gentle));
        assert_eq!(Preset::parse("punchy"), Some(Preset::Punchy));
        assert_eq!(Preset::parse("all off"), Some(Preset::Off));
        assert_eq!(Preset::parse("loud"), None);
        assert_eq!(Preset::parse(""), None);
    }

    fn track(len: f64) -> Track {
        Track::new("v", "v.wav".into(), len)
    }

    #[test]
    fn cuts_ripple_and_map_times() {
        let mut t = track(10.0);
        t.cut(2.0, 3.0);
        assert_eq!(t.clips, vec![Clip { start: 0.0, end: 2.0 }, Clip { start: 3.0, end: 10.0 }]);
        assert!((t.length() - 9.0).abs() < 1e-9);
        t.cut(4.0, 5.0); // mix 4..5 = source 5..6
        assert_eq!(t.clips.len(), 3);
        assert_eq!(t.clips[1], Clip { start: 3.0, end: 5.0 });
        assert_eq!(t.src_to_out(7.0), Some(5.0));
        assert_eq!(t.src_to_out(2.5), None);
        assert_eq!(t.src_to_out_snapped(2.5), 2.0);
        assert_eq!(t.out_to_src(5.0), Some(7.0));
        assert_eq!(t.joins(), vec![2.0, 4.0]);
        assert_eq!(t.pieces(1.0, 4.5), vec![(1.0, 2.0), (3.0, 5.0), (6.0, 6.5)]);
        // A cut spanning a join merges the removed ranges.
        t.cut(1.5, 2.5);
        assert!((t.length() - 7.0).abs() < 1e-9);
        assert!((t.out_to_src(1.6).unwrap() - 3.6).abs() < 1e-9);
        // Offsets shift everything.
        t.offset = 1.0;
        assert_eq!(t.src_to_out(0.0), Some(1.0));
    }

    #[test]
    fn split_adds_a_cut_without_removing_anything() {
        let mut t = track(10.0);
        assert!(t.split(4.0));
        assert_eq!(t.clips, vec![Clip { start: 0.0, end: 4.0 }, Clip { start: 4.0, end: 10.0 }]);
        assert!((t.length() - 10.0).abs() < 1e-9);
        assert_eq!(t.joins(), vec![4.0]);
        // Not on an edge, nor outside the track.
        assert!(!t.split(4.0) && !t.split(0.0) && !t.split(11.0));
        t.cut(2.0, 3.0);
        assert!(t.split(8.0)); // source 9
        assert_eq!(t.clips.last(), Some(&Clip { start: 9.0, end: 10.0 }));
    }

    #[test]
    fn retime_follows_later_cuts() {
        let mut m = Mix::for_recording("v.wav".into(), 10.0);
        assert_eq!(m.retime(4.0), 4.0);
        m.tracks[0].cut(1.0, 2.0);
        // Not synced yet: the script is on the raw recording's clock.
        assert_eq!(m.retime(4.0), 3.0);
        m.mark_synced(0); // the script is now on the cut clock
        assert_eq!(m.retime(4.0), 4.0);
        m.tracks[0].cut(2.0, 3.0); // mix 2..3 = source 3..4
        assert_eq!(m.retime(4.5), 3.5); // source 5.5
        assert_eq!(m.retime(2.5), 2.0); // was in the cut: lands on it
    }

    #[test]
    fn finds_and_tightens_pauses() {
        let rate = 48_000.0;
        // 1 s tone, 2 s near-silence, 1 s tone.
        let mut buf: Vec<f32> = (0..(4.0 * rate) as usize).map(|i| 0.3 * (i as f32 * 0.05).sin()).collect();
        for (i, s) in buf[(1.0 * rate) as usize..(3.0 * rate) as usize].iter_mut().enumerate() {
            *s = 0.0005 * ((i * 7919 % 1000) as f32 / 500.0 - 1.0);
        }
        let p = pauses(&buf, rate, 0.3);
        assert_eq!(p.len(), 1);
        assert!((p[0].0 - 1.0).abs() < 0.03 && (p[0].1 - 3.0).abs() < 0.03, "{p:?}");
        let mut t = track(4.0);
        assert_eq!(tighten(&mut t, &buf, rate, 0.4), 1);
        assert!((t.length() - 2.4).abs() < 0.05, "{}", t.length());
    }

    #[test]
    fn adjustments_follow_cuts_and_replace_each_other() {
        let mut t = track(10.0);
        assert!(t.adjust(2.0, 4.0, Adjust::Mute));
        assert!(t.adjust(3.0, 5.0, Adjust::Gain(6.0)));
        // The gain replaced the overlapping part of the mute.
        assert_eq!(t.regions, vec![Region { start: 2.0, end: 3.0, adjust: Adjust::Mute }, Region { start: 3.0, end: 5.0, adjust: Adjust::Gain(6.0) }]);
        t.adjust(0.0, 1.0, Adjust::FadeIn);
        let env = t.envelope(10 * 100, 100.0).unwrap();
        assert!(env[0] < 0.05 && env[99] > 0.95);
        assert!(env[250] < 1e-3, "muted: {}", env[250]);
        assert!((env[400] - 2.0).abs() < 0.01, "+6 dB: {}", env[400]);
        assert_eq!(env[700], 1.0);
        // A cut before the regions moves them with the audio.
        t.cut(0.5, 1.5);
        assert_eq!(t.region_span(&t.regions[1]), (1.0, 2.0));
        // Reset in the middle of the gain splits it.
        t.reset(2.4, 2.6);
        assert_eq!(t.regions.iter().filter(|r| r.adjust == Adjust::Gain(6.0)).count(), 2);
        // A gain of 0 dB just clears.
        t.adjust(0.0, 9.0, Adjust::Gain(0.0));
        assert!(t.regions.iter().all(|r| !r.adjust.is_level()));
    }

    #[test]
    fn round_trips_as_ron() {
        let m = Mix::for_recording("take1.wav".into(), 3.0);
        let text = ron::ser::to_string_pretty(&m, Default::default()).unwrap();
        assert_eq!(ron::from_str::<Mix>(&text).unwrap(), m);
    }
}
