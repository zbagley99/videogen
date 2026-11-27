//! The project's video track: clips of video files (or still images) placed on
//! the project timeline, with the edits the timeline offers (split, trim,
//! move, delete). Times are seconds; `start` is on the project timeline and
//! `src_in..src_out` is the part of the source file that plays.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How long a still image lasts when it's added.
pub const STILL_SECONDS: f64 = 5.0;
/// The shortest a clip can be trimmed to.
pub const MIN_CLIP: f64 = 0.1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct VideoClip {
    /// Relative to the project file (or absolute).
    pub source: PathBuf,
    /// Where it starts on the project timeline.
    pub start: f64,
    pub src_in: f64,
    pub src_out: f64,
    /// A still image (any `src_in..src_out` shows the same picture).
    #[serde(default, skip_serializing_if = "is_false")]
    pub still: bool,
    /// Length of the source (None for stills), so trims can't run past it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_len: Option<f64>,
    /// The clip's own sound is left out.
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    /// Fades from and to black, in seconds.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_in: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_out: f64,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

impl VideoClip {
    pub fn new(source: PathBuf, start: f64, source_len: Option<f64>, still: bool) -> Self {
        let len = if still { STILL_SECONDS } else { source_len.unwrap_or(STILL_SECONDS) };
        VideoClip { source, start, src_in: 0.0, src_out: len, still, source_len: if still { None } else { source_len }, muted: false, fade_in: 0.0, fade_out: 0.0 }
    }

    pub fn len(&self) -> f64 {
        self.src_out - self.src_in
    }

    pub fn is_empty(&self) -> bool {
        self.len() <= 0.0
    }

    pub fn end(&self) -> f64 {
        self.start + self.len()
    }

    /// The source time shown at timeline time `t` (None outside the clip).
    pub fn src_at(&self, t: f64) -> Option<f64> {
        (t >= self.start && t < self.end()).then_some(self.src_in + (t - self.start))
    }

    /// How opaque the clip is at timeline time `t` (its fades).
    pub fn opacity_at(&self, t: f64) -> f32 {
        let a = if self.fade_in > 0.0 { ((t - self.start) / self.fade_in).clamp(0.0, 1.0) } else { 1.0 };
        let b = if self.fade_out > 0.0 { ((self.end() - t) / self.fade_out).clamp(0.0, 1.0) } else { 1.0 };
        (a * b) as f32
    }

    /// Longest the clip can reach at its end (sources have a length; stills don't).
    fn max_src_out(&self) -> f64 {
        if self.still { f64::INFINITY } else { self.source_len.unwrap_or(f64::INFINITY) }
    }

    fn clamp_fades(&mut self) {
        let half = self.len() / 2.0;
        self.fade_in = self.fade_in.clamp(0.0, half);
        self.fade_out = self.fade_out.clamp(0.0, half);
    }
}

/// Clips in time order, never overlapping.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(transparent)]
pub struct VideoTrack {
    pub clips: Vec<VideoClip>,
}

impl VideoTrack {
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    pub fn end(&self) -> f64 {
        self.clips.iter().map(VideoClip::end).fold(0.0, f64::max)
    }

    fn sort(&mut self) {
        self.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    }

    /// The clip at timeline time `t`.
    pub fn at(&self, t: f64) -> Option<usize> {
        self.clips.iter().position(|c| t >= c.start && t < c.end())
    }

    /// Clip edges (starts and ends), for snapping.
    pub fn edges(&self) -> Vec<f64> {
        let mut v: Vec<f64> = self.clips.iter().flat_map(|c| [c.start, c.end()]).collect();
        v.sort_by(f64::total_cmp);
        v.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        v
    }

    /// The free room around `i`: (earliest start, latest end) it can grow into.
    pub fn room(&self, i: usize) -> (f64, f64) {
        let lo = if i > 0 { self.clips[i - 1].end() } else { 0.0 };
        let hi = self.clips.get(i + 1).map(|c| c.start).unwrap_or(f64::INFINITY);
        (lo, hi)
    }

    /// Put `clip` at `t`, or at the first gap after `t` it fits in. Returns its index.
    pub fn insert(&mut self, mut clip: VideoClip, t: f64) -> usize {
        let len = clip.len();
        let mut at = t.max(0.0);
        for c in &self.clips {
            if c.end() <= at {
                continue;
            }
            if c.start >= at + len {
                break;
            }
            at = c.end();
        }
        clip.start = at;
        self.clips.push(clip);
        self.sort();
        self.clips.iter().position(|c| c.start == at).unwrap_or(0)
    }

    /// Cut the clip under `t` in two. False if there's no clip there or `t`
    /// is too close to an edge.
    pub fn split(&mut self, t: f64) -> bool {
        let Some(i) = self.at(t) else { return false };
        let c = &self.clips[i];
        if t - c.start < MIN_CLIP || c.end() - t < MIN_CLIP {
            return false;
        }
        let mut right = c.clone();
        let cut = c.src_in + (t - c.start);
        right.start = t;
        right.src_in = cut;
        right.fade_in = 0.0;
        let left = &mut self.clips[i];
        left.src_out = cut;
        left.fade_out = 0.0;
        left.clamp_fades();
        right.clamp_fades();
        self.clips.insert(i + 1, right);
        true
    }

    /// Remove timeline `a..b` from the track. With `ripple`, everything after
    /// `b` moves left to close the gap; otherwise a gap is left.
    pub fn delete_range(&mut self, a: f64, b: f64, ripple: bool) {
        let (a, b) = (a.min(b), a.max(b));
        if b - a <= 0.0 {
            return;
        }
        let mut out = vec![];
        for c in self.clips.drain(..) {
            if c.end() <= a || c.start >= b {
                out.push(c);
                continue;
            }
            if c.start < a {
                let mut l = c.clone();
                l.src_out = c.src_in + (a - c.start);
                l.fade_out = 0.0;
                l.clamp_fades();
                if l.len() >= MIN_CLIP / 2.0 {
                    out.push(l);
                }
            }
            if c.end() > b {
                let mut r = c.clone();
                r.src_in = c.src_in + (b - c.start);
                r.start = b;
                r.fade_in = 0.0;
                r.clamp_fades();
                if r.len() >= MIN_CLIP / 2.0 {
                    out.push(r);
                }
            }
        }
        if ripple {
            for c in out.iter_mut().filter(|c| c.start >= b - 1e-9) {
                c.start -= b - a;
            }
        }
        self.clips = out;
        self.sort();
    }

    /// Remove clip `i`; with `ripple`, later clips move left by its length.
    pub fn remove(&mut self, i: usize, ripple: bool) {
        if i >= self.clips.len() {
            return;
        }
        let c = self.clips.remove(i);
        if ripple {
            for d in self.clips.iter_mut().skip(i) {
                d.start -= c.len();
            }
        }
    }

    /// Move clip `i`'s start edge to timeline `t` (the source in-point moves
    /// with it; the end stays). Clamped to the source, its neighbour and a
    /// minimum length. Returns the start it got.
    pub fn trim_start(&mut self, i: usize, t: f64) -> f64 {
        let (lo, _) = self.room(i);
        let c = &mut self.clips[i];
        // Video can't start before its source does; a still can stretch back to its neighbour.
        let earliest = if c.still { lo } else { (c.start - c.src_in).max(lo) };
        let t = t.clamp(earliest.max(0.0), c.end() - MIN_CLIP);
        let d = t - c.start;
        c.src_in += d;
        if c.still && c.src_in < 0.0 {
            // Stills have no in-point to run out of.
            c.src_out -= c.src_in;
            c.src_in = 0.0;
        }
        c.start = t;
        c.clamp_fades();
        t
    }

    /// Move clip `i`'s end edge to timeline `t`. Returns the end it got.
    pub fn trim_end(&mut self, i: usize, t: f64) -> f64 {
        let (_, hi) = self.room(i);
        let c = &mut self.clips[i];
        let latest = (c.start + c.max_src_out() - c.src_in).min(hi);
        let t = t.clamp(c.start + MIN_CLIP, latest);
        c.src_out = c.src_in + (t - c.start);
        c.clamp_fades();
        t
    }

    /// Slide clip `i` to start at `t`, stopping at its neighbours. Returns the start it got.
    pub fn move_to(&mut self, i: usize, t: f64) -> f64 {
        let (lo, hi) = self.room(i);
        let c = &mut self.clips[i];
        let t = t.clamp(lo, (hi - c.len()).max(lo));
        c.start = t;
        t
    }

    /// Set a clip's fades (clamped to half its length each).
    pub fn set_fades(&mut self, i: usize, fade_in: f64, fade_out: f64) {
        let c = &mut self.clips[i];
        c.fade_in = fade_in;
        c.fade_out = fade_out;
        c.clamp_fades();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(start: f64, len: f64) -> VideoClip {
        let mut c = VideoClip::new(PathBuf::from("a.mp4"), start, Some(100.0), false);
        c.src_in = 10.0;
        c.src_out = 10.0 + len;
        c
    }

    #[test]
    fn insert_finds_a_gap() {
        let mut t = VideoTrack::default();
        t.insert(clip(0.0, 4.0), 0.0);
        let i = t.insert(clip(0.0, 3.0), 2.0);
        assert_eq!(i, 1);
        assert_eq!(t.clips[1].start, 4.0);
        // Fits in the gap between 4+3=7 and a clip at 20.
        t.insert(clip(0.0, 2.0), 20.0);
        let i = t.insert(clip(0.0, 2.0), 8.0);
        assert_eq!(t.clips[i].start, 8.0);
    }

    #[test]
    fn split_and_delete_ripple() {
        let mut t = VideoTrack::default();
        t.insert(clip(0.0, 10.0), 0.0);
        t.insert(clip(0.0, 5.0), 12.0);
        assert!(t.split(4.0));
        assert_eq!(t.clips.len(), 3);
        assert_eq!((t.clips[0].src_out, t.clips[1].src_in, t.clips[1].start), (14.0, 14.0, 4.0));
        assert!(!t.split(4.05));
        // Delete 2..6 with ripple: the second clip at 12 moves to 8.
        t.delete_range(2.0, 6.0, true);
        assert_eq!(t.clips.iter().map(|c| (c.start, c.len())).collect::<Vec<_>>(), vec![(0.0, 2.0), (2.0, 4.0), (8.0, 5.0)]);
        assert_eq!(t.clips[1].src_in, 16.0);
        // Without ripple a gap stays.
        t.delete_range(0.0, 1.0, false);
        assert_eq!(t.clips[0].start, 1.0);
    }

    #[test]
    fn trims_and_moves_stop_at_neighbours_and_sources() {
        let mut t = VideoTrack::default();
        t.insert(clip(0.0, 4.0), 0.0);
        t.insert(clip(0.0, 4.0), 6.0);
        // The end can't run into the next clip.
        assert_eq!(t.trim_end(0, 9.0), 6.0);
        // The start can't go before the source's beginning (src_in 10 at start 6 → 0 would be -4, but the neighbour ends at 6).
        assert_eq!(t.trim_start(1, 1.0), 6.0);
        assert_eq!(t.trim_start(1, 8.0), 8.0);
        assert_eq!(t.clips[1].src_in, 12.0);
        // Moving stops at the neighbour.
        assert_eq!(t.move_to(1, 3.0), 6.0);
        // The end can't pass the source's length (100).
        let mut u = VideoTrack::default();
        u.insert(clip(0.0, 4.0), 0.0);
        assert_eq!(u.trim_end(0, 200.0), 90.0);
        // Stills stretch freely.
        let mut s = VideoTrack::default();
        s.insert(VideoClip::new(PathBuf::from("a.png"), 0.0, None, true), 0.0);
        assert_eq!(s.trim_end(0, 30.0), 30.0);
        assert_eq!(s.clips[0].src_at(12.0), Some(12.0));
    }

    #[test]
    fn removing_a_clip_lifts_or_ripples() {
        let mut t = VideoTrack::default();
        t.insert(clip(0.0, 4.0), 0.0);
        t.insert(clip(0.0, 3.0), 5.0);
        t.insert(clip(0.0, 2.0), 10.0);
        // Lift: a gap is left where it was.
        let mut lift = t.clone();
        lift.remove(1, false);
        assert_eq!(lift.clips.iter().map(|c| c.start).collect::<Vec<_>>(), vec![0.0, 10.0]);
        // Ripple: later clips move up by its length; the gaps around it stay.
        t.remove(1, true);
        assert_eq!(t.clips.iter().map(|c| c.start).collect::<Vec<_>>(), vec![0.0, 7.0]);
    }

    #[test]
    fn fades_are_clamped_and_ramp() {
        let mut t = VideoTrack::default();
        t.insert(clip(0.0, 4.0), 0.0);
        t.set_fades(0, 1.0, 9.0);
        assert_eq!((t.clips[0].fade_in, t.clips[0].fade_out), (1.0, 2.0));
        assert_eq!(t.clips[0].opacity_at(0.5), 0.5);
        assert_eq!(t.clips[0].opacity_at(1.5), 1.0);
        assert!(t.clips[0].opacity_at(3.0) < 0.51);
    }

    #[test]
    fn a_still_stretches_back_to_its_neighbour() {
        let mut t = VideoTrack::default();
        t.insert(clip(0.0, 2.0), 0.0);
        t.insert(VideoClip::new(PathBuf::from("a.png"), 5.0, None, true), 5.0);
        // Its start moves left past its in-point, and its end stays put.
        assert_eq!(t.trim_start(1, 3.0), 3.0);
        assert_eq!((t.clips[1].start, t.clips[1].src_in, t.clips[1].end()), (3.0, 0.0, 10.0));
        // It stops at the clip before it.
        assert_eq!(t.trim_start(1, 1.0), 2.0);
        assert_eq!(t.clips[1].end(), 10.0);
    }
}
