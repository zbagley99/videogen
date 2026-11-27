//! The Audio tab: clean up the narration before it drives the video. Cut out
//! mistakes and long pauses, run the voice through an effect chain (high-pass,
//! noise reduction, EQ, de-esser, compressor, gate), mix in other tracks and
//! master to a loudness target. Edits are kept in a `.vgmix` next to the
//! recording (the recording itself is never changed) and rendered to
//! `<name>.mix.wav`, which becomes the script's narration.

use super::util::{file_name, plural};
use super::{VgApp, audio, theme};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vg_audio::{Effect, Fx, Mix, Preset, RATE, Rendered, Track};

/// How long after the last change before re-rendering.
const DEBOUNCE: Duration = Duration::from_millis(120);

/// Stretches of time, (start, end) in seconds.
type Spans = Vec<(f64, f64)>;

pub struct Studio {
    /// The `.vgmix` being edited (where it will be saved).
    pub path: Option<PathBuf>,
    pub mix: Mix,
    saved: Mix,
    /// Undo history. Changes are committed once the pointer is released, so a
    /// slider drag is one step.
    undo: Vec<Mix>,
    redo: Vec<Mix>,
    undo_base: Mix,
    /// Selected track.
    pub(super) track: usize,
    /// Selected range (mix seconds).
    sel: Option<(f64, f64)>,
    view_start: f64,
    view_span: Option<f64>,
    drag: Option<Drag>,
    cache: Arc<Mutex<vg_audio::Cache>>,
    job: Option<Job>,
    changed_at: Instant,
    out: Option<Arc<Output>>,
    error: Option<String>,
    /// The narration this was opened for (so switching scripts reloads).
    pub(super) loaded_for: Option<PathBuf>,
    /// Opened by hand (Open recording) while the script's narration was this:
    /// don't switch back to the narration until it changes.
    pub(super) manual: Option<Option<PathBuf>>,
    pub time: f64,
    playing: bool,
    last_tick: Option<Instant>,
    player: Option<audio::BufferPlayer>,
    /// A/B: play the original (same cuts, same loudness, no effects).
    original: bool,
    /// Pauses longer than this are marked and can be tightened to it.
    tighten_max: f64,
    /// The gain the Selection section's slider sets (dB).
    sel_gain: f32,
    /// The mix last written to the narration file (so it's only written again after a change).
    written: Option<Mix>,
    /// "Use the edited audio" was asked for before the render was ready.
    pub(super) pending_sync: bool,
    /// Undo for cuts made on the Video tab's timeline (each restores the edit,
    /// the script and the word timings as they were).
    cut_undo: Vec<CutSnapshot>,
    /// Tighten pauses: the pauses found, for (the length kept, the track).
    pauses: Option<(f64, usize, Spans)>,
    transcribe_after: bool,
    /// EQ band being dragged in the curve.
    eq_drag: Option<usize>,
    /// Lane heights (px) and the vertical scroll of the track area.
    heights: Vec<f32>,
    vscroll: f32,
    /// Draw amplitudes in dB instead of linear.
    db_scale: bool,
    /// Playback meter: peak and RMS (dBFS), smoothed.
    meter: (f32, f32),
    /// Resizing a lane: (track, height at start, pointer y at start).
    resize: Option<(usize, f32, f32)>,
}

const TRACK_H: f32 = 170.0;

#[derive(Clone, Copy)]
enum Drag {
    /// Selecting from this time.
    Select(f64),
    /// Moving a track: its offset and the pointer time at the start.
    Move { track: usize, offset: f64, from: f64 },
}

struct Job {
    mix: Mix,
    cancel: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<anyhow::Result<Option<Output>>>,
}

/// A finished render with waveforms ready to draw.
struct Output {
    mix: Mix,
    r: Rendered,
    tracks: Vec<Pyramid>,
    tracks_wet: Vec<Pyramid>,
    took: Duration,
}

/// Min, max and RMS at a few resolutions, so any zoom draws quickly.
struct Pyramid {
    levels: Vec<(usize, Vec<Bucket>)>,
    len: usize,
}

#[derive(Clone, Copy)]
struct Bucket {
    lo: f32,
    hi: f32,
    /// Sum of squares.
    ss: f32,
}

impl Bucket {
    fn merge(a: Bucket, b: Bucket) -> Bucket {
        Bucket { lo: a.lo.min(b.lo), hi: a.hi.max(b.hi), ss: a.ss + b.ss }
    }
}

impl Pyramid {
    fn new(buf: &[f32]) -> Self {
        let base: Vec<Bucket> = buf
            .chunks(64)
            .map(|c| c.iter().fold(Bucket { lo: 0.0, hi: 0.0, ss: 0.0 }, |b, &s| Bucket { lo: b.lo.min(s), hi: b.hi.max(s), ss: b.ss + s * s }))
            .collect();
        let mut levels = vec![];
        let mut cur = (64usize, base);
        for _ in 0..3 {
            let next = (cur.0 * 8, cur.1.chunks(8).map(|c| c.iter().copied().reduce(Bucket::merge).unwrap()).collect());
            levels.push(cur);
            cur = next;
        }
        levels.push(cur);
        Pyramid { levels, len: buf.len() }
    }

    /// Min, max and RMS of samples `a..b`.
    fn range(&self, a: usize, b: usize) -> Option<(f32, f32, f32)> {
        if a >= self.len || b <= a {
            return None;
        }
        let span = b - a;
        let (size, lv) = self.levels.iter().rev().find(|(s, _)| *s <= span.max(64)).unwrap_or(&self.levels[0]);
        let i0 = (a / size).min(lv.len() - 1);
        let i1 = b.div_ceil(*size).clamp(i0 + 1, lv.len());
        let m = lv[i0..i1].iter().copied().reduce(Bucket::merge)?;
        let n = ((i1 - i0) * size).min(self.len - i0 * size).max(1);
        Some((m.lo, m.hi, (m.ss / n as f32).sqrt()))
    }
}

impl Default for Studio {
    fn default() -> Self {
        Studio {
            path: None,
            mix: Mix::default(),
            saved: Mix::default(),
            undo: vec![],
            redo: vec![],
            undo_base: Mix::default(),
            track: 0,
            sel: None,
            view_start: 0.0,
            view_span: None,
            drag: None,
            cache: Default::default(),
            job: None,
            changed_at: Instant::now(),
            out: None,
            error: None,
            loaded_for: None,
            manual: None,
            time: 0.0,
            playing: false,
            last_tick: None,
            player: None,
            original: false,
            tighten_max: 0.45,
            sel_gain: 3.0,
            written: None,
            pending_sync: false,
            cut_undo: vec![],
            pauses: None,
            transcribe_after: false,
            eq_drag: None,
            heights: vec![],
            vscroll: 0.0,
            db_scale: false,
            meter: (-90.0, -90.0),
            resize: None,
        }
    }
}

impl Studio {
    pub fn dirty(&self) -> bool {
        self.path.is_some() && self.mix != self.saved && !self.mix.tracks.is_empty()
    }

    /// Memory held by the render cache, or `None` while a render is using it.
    pub fn cache_bytes(&self) -> Option<usize> {
        self.cache.try_lock().ok().map(|c| c.bytes())
    }

    /// Empty the render cache. `false` if a render is using it right now.
    pub fn clear_cache(&mut self) -> bool {
        match self.cache.try_lock() {
            Ok(mut c) => {
                c.clear();
                true
            }
            Err(_) => false,
        }
    }

    /// A summary for `vg screenshot` checks.
    pub fn report(&self) -> String {
        let mut out = format!("studio: {} track(s), length {:.3} s, time {:.3}, selection {:?}, original {}\n", self.mix.tracks.len(), self.length(), self.time, self.sel.map(|(a, b)| (format!("{a:.3}"), format!("{b:.3}"))), self.original);
        for (i, t) in self.mix.tracks.iter().enumerate() {
            let clips: Vec<String> = t.clips.iter().map(|c| format!("{:.3}-{:.3}", c.start, c.end)).collect();
            let on: Vec<&str> = t.effects.iter().filter(|e| e.on).map(|e| e.fx.name()).collect();
            let regions: Vec<String> = t.regions.iter().map(|r| format!("{:.2}-{:.2} {}", r.start, r.end, r.adjust.label())).collect();
            out += &format!("  track {} {:?} offset {:.2} gain {:.1} clips [{}] fx [{}] regions [{}]{}\n", i + 1, t.name, t.offset, t.gain_db, clips.join(", "), on.join(", "), regions.join(", "), if t.music.is_some() { " music" } else { "" });
        }
        if let Some(o) = &self.out {
            let st = &o.r.stats;
            out += &format!("  rendered {:.3} s in {:.2} s: in {:?} LUFS, out {:?} LUFS, peak {:.2} dBFS, current {}", o.r.wet.len() as f64 / RATE as f64, o.took.as_secs_f64(), st.input_lufs, st.output_lufs, st.output_peak_db, o.mix == self.mix);
        }
        out
    }

    pub fn busy(&self) -> bool {
        self.job.is_some() || (!self.mix.tracks.is_empty() && self.out.as_ref().is_none_or(|o| o.mix != self.mix))
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        if let Some(p) = &self.path {
            self.mix.save(p)?;
            self.saved = self.mix.clone();
        }
        Ok(())
    }

    pub(super) fn base(&self) -> PathBuf {
        self.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default()
    }

    /// Open (or start) the edit of `audio`.
    pub(super) fn open_for(&mut self, audio: &Path) -> anyhow::Result<()> {
        let dir = audio.parent().map(Path::to_path_buf).unwrap_or_default();
        let name = file_name(audio);
        // A rendered mix points back at its edit.
        let stem = name.strip_suffix(".mix.wav").map(str::to_string).unwrap_or_else(|| audio.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
        let path = dir.join(format!("{stem}.vgmix"));
        let mix = if path.exists() {
            Mix::load(&path)?
        } else {
            let dur = vg_media::probe_duration(audio)?;
            Mix::for_recording(PathBuf::from(&name), dur)
        };
        self.stop();
        *self = Studio { cache: self.cache.clone(), tighten_max: self.tighten_max, ..Default::default() };
        self.saved = if path.exists() { mix.clone() } else { Mix::default() };
        self.undo_base = mix.clone();
        self.mix = mix;
        self.path = Some(path);
        self.loaded_for = Some(audio.to_path_buf());
        Ok(())
    }

    /// Commit pending changes to the undo history once the pointer is up.
    fn settle(&mut self, pointer_down: bool) {
        if !pointer_down && self.mix != self.undo_base {
            self.undo.push(std::mem::replace(&mut self.undo_base, self.mix.clone()));
            self.redo.clear();
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
        }
    }

    fn undo(&mut self) {
        self.settle(false);
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.mix, prev));
            self.undo_base = self.mix.clone();
            self.track = self.track.min(self.mix.tracks.len().saturating_sub(1));
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.mix, next));
            self.undo_base = self.mix.clone();
        }
    }

    // ---- rendering --------------------------------------------------------

    fn poll(&mut self, ctx: &egui::Context) {
        if self.job.as_ref().is_some_and(|j| j.handle.is_finished()) {
            let job = self.job.take().unwrap();
            match job.handle.join() {
                Ok(Ok(Some(out))) => {
                    self.error = None;
                    self.out = Some(Arc::new(out));
                    self.pauses = None;
                    if self.playing {
                        self.start_audio();
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(e)) => self.error = Some(format!("{e:#}")),
                Err(_) => self.error = Some("the render crashed".into()),
            }
        }
        if self.mix.tracks.is_empty() {
            return;
        }
        let current = self.out.as_ref().is_some_and(|o| o.mix == self.mix);
        let running = self.job.as_ref().map(|j| &j.mix);
        if current || running == Some(&self.mix) {
            if self.job.is_some() {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            return;
        }
        if let Some(j) = &self.job {
            j.cancel.store(true, Ordering::Relaxed);
            ctx.request_repaint_after(Duration::from_millis(30));
            return;
        }
        if self.changed_at.elapsed() < DEBOUNCE {
            ctx.request_repaint_after(DEBOUNCE);
            return;
        }
        let (mix, base, cache, cancel, ctx2) = (self.mix.clone(), self.base(), self.cache.clone(), Arc::new(AtomicBool::new(false)), ctx.clone());
        let c2 = cancel.clone();
        let m2 = mix.clone();
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            let r = vg_audio::render(&m2, &base, &mut cache.lock().unwrap(), &|| c2.load(Ordering::Relaxed));
            ctx2.request_repaint();
            let Some(r) = r? else { return Ok(None) };
            let out = Output {
                tracks: r.tracks.iter().map(|b| Pyramid::new(b)).collect(),
                tracks_wet: r.tracks_wet.iter().map(|b| Pyramid::new(b)).collect(),
                took: started.elapsed(),
                mix: m2,
                r,
            };
            Ok(Some(out))
        });
        self.job = Some(Job { mix, cancel, handle });
    }

    /// Pauses in the selected track longer than the tighten length (track-local).
    fn pauses(&mut self) -> Option<&[(f64, f64)]> {
        let out = self.out.as_ref().filter(|o| o.mix == self.mix)?;
        let buf = out.r.tracks.get(self.track)?;
        let key = (self.tighten_max, self.track);
        if self.pauses.as_ref().is_none_or(|(m, t, _)| (*m, *t) != key) {
            let found = vg_audio::edit::pauses(buf, RATE as f32, self.tighten_max + 0.05).into_iter().filter(|(a, b)| b - a > self.tighten_max).collect();
            self.pauses = Some((self.tighten_max, self.track, found));
        }
        self.pauses.as_ref().map(|p| p.2.as_slice())
    }

    // ---- playback ---------------------------------------------------------

    fn length(&self) -> f64 {
        self.mix.length()
    }

    fn start_audio(&mut self) {
        let Some(out) = &self.out else { return };
        if self.player.is_none() {
            self.player = audio::BufferPlayer::new().ok();
        }
        let buf = if self.original { out.r.dry.clone() } else { out.r.wet.clone() };
        if let Some(p) = &self.player {
            p.play(buf, RATE, self.time, 1.0);
        }
    }

    fn toggle_play(&mut self) {
        if self.playing {
            self.stop();
        } else {
            if self.time >= self.length() - 1e-3 {
                self.time = 0.0;
            }
            self.playing = true;
            self.last_tick = Some(Instant::now());
            self.start_audio();
        }
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.last_tick = None;
        if let Some(p) = &self.player {
            p.pause();
        }
    }

    fn seek(&mut self, t: f64) {
        self.time = t.clamp(0.0, self.length());
        if self.playing {
            self.last_tick = Some(Instant::now());
            self.start_audio();
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        if !self.playing {
            return;
        }
        let now = Instant::now();
        if let Some(l) = self.last_tick {
            self.time += (now - l).as_secs_f64();
        }
        self.last_tick = Some(now);
        if self.time >= self.length() {
            self.time = self.length();
            self.stop();
        }
        ctx.request_repaint();
    }

    fn fit_view(&mut self) {
        self.view_start = 0.0;
        self.view_span = None;
    }

    fn set_volume(&self, v: f32) {
        if let Some(p) = &self.player {
            p.set_volume(v);
        }
    }

    // ---- edits ------------------------------------------------------------

    pub(super) fn changed(&mut self) {
        self.changed_at = Instant::now();
    }

    fn cut_selection(&mut self) {
        let Some((a, b)) = self.sel.take() else { return };
        if let Some(t) = self.mix.tracks.get_mut(self.track) {
            t.cut(a, b);
            self.time = a.min(self.mix.length());
            self.changed();
        }
    }

    /// Mute, turn up or down, or fade the selection (after the effects).
    fn adjust_selection(&mut self, adjust: vg_audio::Adjust) {
        let Some((a, b)) = self.sel else { return };
        if let Some(t) = self.mix.tracks.get_mut(self.track)
            && t.adjust(a, b, adjust) {
                self.changed();
            }
    }

    /// Remove the adjustments in the selection.
    fn reset_selection(&mut self) {
        let Some((a, b)) = self.sel else { return };
        if let Some(t) = self.mix.tracks.get_mut(self.track)
            && t.reset(a, b) {
                self.changed();
            }
    }

    /// Cut everything on the track outside the selection.
    fn trim_to_selection(&mut self) {
        let Some((a, b)) = self.sel.take() else { return };
        if let Some(t) = self.mix.tracks.get_mut(self.track) {
            let end = t.end();
            t.cut(b, end);
            t.cut(t.offset, a);
            self.time = t.offset;
            self.changed();
        }
    }

    /// Use the selection as the room tone the noise reduction learns from.
    fn learn_room_tone(&mut self) -> Result<(), String> {
        let (a, b) = self.sel.ok_or("select a stretch of silence (just room noise) first")?;
        let track = self.mix.tracks.get_mut(self.track).ok_or("no track")?;
        // Across a cut, use the longest uncut stretch of the selection.
        let (sa, sb) = track.pieces(a, b).into_iter().max_by(|x, y| (x.1 - x.0).total_cmp(&(y.1 - y.0))).ok_or("the selection is outside the track")?;
        if sb - sa < 0.15 {
            return Err("select at least 0.15 s of room noise (without a cut in it)".into());
        }
        let mut set = false;
        for e in &mut track.effects {
            if let Fx::Denoise { room_tone, .. } = &mut e.fx {
                *room_tone = Some((sa, sb));
                e.on = true;
                set = true;
            }
        }
        if !set {
            return Err("this track has no noise reduction (apply a preset first)".into());
        }
        self.changed();
        Ok(())
    }
}

// ---- the tab ---------------------------------------------------------------

/// Everything the Audio tab needs each frame, then its panels.
pub fn show(app: &mut VgApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    ensure_loaded(app);
    app.studio.tick(&ctx);
    app.studio.poll(&ctx);
    shortcuts(app, &ctx);
    let pointer_down = ctx.input(|i| i.pointer.any_down());
    app.studio.settle(pointer_down);

    egui::Panel::left("studio-tracks").resizable(true).default_size(290.0).min_size(240.0).frame(theme::panel_frame()).show_inside(ui, |ui| tracks_panel(app, ui));
    egui::Panel::right("studio-fx").resizable(true).default_size(400.0).min_size(330.0).frame(theme::panel_frame()).show_inside(ui, |ui| fx_panel(app, ui));
    egui::CentralPanel::default().frame(theme::panel_frame()).show_inside(ui, |ui| editor(app, ui));
}

/// Follow the script's narration: open its edit when it changes.
pub(super) fn ensure_loaded(app: &mut VgApp) {
    if let Some(was) = &app.studio.manual {
        if *was == app.parsed.audio_path {
            return;
        }
        app.studio.manual = None;
    }
    let Some(audio) = app.parsed.audio_path.clone() else { return };
    if app.studio.loaded_for.as_ref() == Some(&audio) || app.studio.dirty() || !audio.exists() {
        return;
    }
    // Rendering to the edit's own mix file keeps the same edit open.
    if app.studio.mix_wav().is_some_and(|m| same_file(&m, &audio)) {
        app.studio.loaded_for = Some(audio);
        return;
    }
    if let Err(e) = app.studio.open_for(&audio) {
        app.studio.error = Some(format!("{e:#}"));
        app.studio.loaded_for = Some(audio);
    }
}

/// How the Audio tab's edit relates to the narration the Video tab plays.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Narration {
    /// Nothing to reconcile (no edit, or the script plays something else).
    Unrelated,
    /// The script plays this edit's mix and it's up to date.
    InSync,
    /// Only the sound changed (effects, gain, music): the file can be rewritten as is.
    Stale,
    /// Cuts moved the timing: the script's times must move too (Use as narration).
    NeedsSync,
    /// The script still plays the raw take, but there's an edit of it.
    Unused,
}

impl Studio {
    /// Where this edit's rendered mix is written.
    fn mix_wav(&self) -> Option<PathBuf> {
        self.path.as_ref().map(|p| p.with_extension("mix.wav"))
    }

    /// Whether the voice track's cuts still match what the script was synced to.
    fn timing_synced(&self) -> bool {
        match &self.mix.synced {
            Some(sy) => self.mix.tracks.get(sy.track).is_some_and(|t| t.clips == sy.clips && (t.offset - sy.offset).abs() < 1e-9),
            None => false,
        }
    }
}

pub fn narration_state(app: &VgApp) -> Narration {
    let s = &app.studio;
    match clock(app) {
        Some(Clock::Mix) if !s.timing_synced() => Narration::NeedsSync,
        Some(Clock::Mix) => {
            let file_matches = s.written.as_ref() == Some(&s.mix) || (s.written.is_none() && !s.dirty() && s.saved == s.mix);
            if file_matches { Narration::InSync } else { Narration::Stale }
        }
        Some(Clock::Raw) if s.dirty() || s.path.as_ref().is_some_and(|p| p.exists()) => Narration::Unused,
        _ => Narration::Unrelated,
    }
}

/// Rewrite the narration file from the current render when only the sound
/// changed. Returns whether it was written.
pub fn refresh_narration(app: &mut VgApp) -> bool {
    if narration_state(app) != Narration::Stale {
        return false;
    }
    let Some(out) = app.studio.out.clone().filter(|o| o.mix == app.studio.mix) else { return false };
    let Some(wav) = app.studio.mix_wav() else { return false };
    app.stop();
    match vg_audio::write_wav(&wav, &out.r.wet) {
        Ok(()) => {
            app.studio.written = Some(app.studio.mix.clone());
            // Reload what the Video tab plays and draws.
            app.audio = None;
            app.player = None;
            app.reparse();
            true
        }
        Err(e) => {
            app.error(format!("{e:#}"));
            false
        }
    }
}

/// Each track's clips on the project timeline (for the Video tab), when the
/// script plays this edit's mix: (name, is music, spans).
pub fn clip_strips(app: &VgApp) -> Vec<(String, bool, Spans)> {
    if !matches!(narration_state(app), Narration::InSync | Narration::Stale) {
        return vec![];
    }
    let mix = &app.studio.mix;
    let end = mix.length();
    mix.tracks
        .iter()
        .enumerate()
        .filter(|(i, _)| mix.audible(*i))
        .map(|(_, t)| {
            let spans = if t.music.as_ref().is_some_and(|m| m.loop_it) {
                vec![(t.offset, end)]
            } else {
                let mut pos = t.offset;
                t.clips.iter().map(|c| {
                    let s = (pos, (pos + c.len()).min(end));
                    pos += c.len();
                    s
                }).collect()
            };
            (t.name.clone(), t.music.is_some(), spans)
        })
        .collect()
}

/// While the Video tab is showing: finish a pending render of the edit, then
/// bring the narration file up to date if only the sound changed.
pub fn background(app: &mut VgApp, ctx: &egui::Context) {
    // Know the narration's edit (a file read) so the Video tab can say how they relate.
    ensure_loaded(app);
    if app.studio.pending_sync {
        app.studio.poll(ctx);
        if app.studio.out.as_ref().is_some_and(|o| o.mix == app.studio.mix) {
            app.studio.pending_sync = false;
            use_as_narration(app, ctx);
        }
        return;
    }
    if narration_state(app) != Narration::Stale {
        return;
    }
    app.studio.poll(ctx);
    if app.studio.job.is_none() {
        refresh_narration(app);
    }
}

// ---- cutting on the Video tab's timeline -----------------------------------

/// Which clock the Video tab's timeline runs on, when its narration is this
/// edit's: the raw take (track 1's source) or the edit's mix.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Clock {
    Raw,
    Mix,
}

impl Studio {
    /// Maps video time to edit time on `clock`: the raw take's, or the one the
    /// script was last synced to.
    fn mapper(&self, clock: Clock) -> Mix {
        let mut m = self.mix.clone();
        if clock == Clock::Raw {
            m.synced = None;
        }
        m
    }
}

fn clock(app: &VgApp) -> Option<Clock> {
    let s = &app.studio;
    let current = app.parsed.audio_path.as_ref()?;
    if s.mix.tracks.is_empty() {
        return None;
    }
    if same_file(current, &s.mix_wav()?) {
        return Some(Clock::Mix);
    }
    let raw = vg_project::resolve(&s.base(), &s.mix.tracks.first()?.source);
    same_file(current, &raw).then_some(Clock::Raw)
}

/// The narration's clips on the Video tab's timeline (video seconds), or
/// `None` when it can't be cut there (no narration, or the Audio tab has
/// another recording open).
pub fn narration_segments(app: &VgApp) -> Option<Vec<(f64, f64)>> {
    match clock(app)? {
        Clock::Raw => Some(vec![(0.0, app.audio.as_ref()?.duration)]),
        Clock::Mix => {
            // The clips the script was last synced to are the timeline's clock.
            let sy = app.studio.mix.synced.as_ref()?;
            let mut pos = sy.offset;
            Some(
                sy.clips
                    .iter()
                    .map(|c| {
                        let s = (pos, pos + c.len());
                        pos += c.len();
                        s
                    })
                    .collect(),
            )
        }
    }
}

/// A cut made on the Video tab's timeline (video seconds).
#[derive(Clone, Copy, Debug)]
pub enum Cut {
    /// Remove a stretch; everything after it moves up.
    Delete(f64, f64),
    /// Remove everything but this stretch.
    Keep(f64, f64),
    /// Cut the clip in two here (nothing is removed).
    Split(f64),
}

struct CutSnapshot {
    mix: Mix,
    cue_text: String,
    /// The script's word timings file and its contents.
    words: Option<(PathBuf, Vec<u8>)>,
}

/// Apply `cut` to the narration's edit, then make the script follow: its
/// times, word timings and pose tags move with the audio. Returns a status line.
pub(super) fn timeline_cut(app: &mut VgApp, ctx: &egui::Context, cut: Cut) -> Result<String, String> {
    if app.studio.pending_sync {
        return Err("the last edit is still being applied".into());
    }
    let clock = clock(app).ok_or("cutting works on the script's narration (the Audio tab has another recording open)")?;
    let before = narration_state(app);
    let mapper = app.studio.mapper(clock);
    let f = |t: f64| mapper.retime(t);
    let words = vg_project::words_path(&app.cue_text, &app.cue_dir()).and_then(|p| std::fs::read(&p).ok().map(|b| (p, b)));
    let snapshot = CutSnapshot { mix: app.studio.mix.clone(), cue_text: app.cue_text.clone(), words };
    let track = &mut app.studio.mix.tracks[0];
    let msg = match cut {
        Cut::Delete(a, b) => {
            let (a, b) = (f(a.min(b)), f(a.max(b)));
            if b - a < 0.01 {
                return Err("select something to delete".into());
            }
            track.cut(a, b);
            format!("cut out {}", vg_core::format_time(b - a))
        }
        Cut::Keep(a, b) => {
            let (a, b) = (f(a.min(b)), f(a.max(b)));
            if b - a < 0.05 {
                return Err("select what to keep".into());
            }
            let (off, end) = (track.offset, track.end());
            track.cut(b, end);
            track.cut(off, a);
            format!("kept {}", vg_core::format_time(b - a))
        }
        Cut::Split(t) => {
            if !track.split(f(t)) {
                return Err("nothing to split there (it's already a cut, or past the narration)".into());
            }
            format!("split at {}", vg_core::format_time(t))
        }
    };
    app.studio.changed();
    app.studio.settle(false);
    app.studio.cut_undo.push(snapshot);
    if app.studio.cut_undo.len() > 50 {
        app.studio.cut_undo.remove(0);
    }
    super::video::note(app, super::video::EditKind::NarrationCut);
    if matches!(cut, Cut::Split(_)) && clock == Clock::Mix && matches!(before, Narration::InSync | Narration::Stale) {
        // Nothing moved and it sounds the same: the script is still in step.
        app.studio.mix.mark_synced(0);
        if before == Narration::InSync {
            app.studio.written = Some(app.studio.mix.clone());
        }
        app.studio.undo_base = app.studio.mix.clone();
        app.studio.save().map_err(|e| format!("{e:#}"))?;
    } else {
        // Render, write the mix and move the script's times to match.
        sync_narration(app, ctx);
    }
    Ok(msg)
}

/// Whether there's a timeline cut to undo (even if it can't be undone right now).
pub(super) fn has_cut_undo(app: &VgApp) -> bool {
    !app.studio.cut_undo.is_empty()
}

/// Undo the last timeline cut: the edit, the script and the word timings go
/// back; the narration file is rewritten in the background.
pub(super) fn undo_cut(app: &mut VgApp) -> Result<(), String> {
    if app.studio.pending_sync {
        return Err("the last edit is still being applied".into());
    }
    let snap = app.studio.cut_undo.pop().ok_or("no cut to undo")?;
    super::video::forget(app, super::video::EditKind::NarrationCut);
    app.stop();
    app.studio.stop();
    app.studio.mix = snap.mix;
    app.studio.changed();
    app.studio.settle(false);
    if let Some((p, bytes)) = &snap.words {
        std::fs::write(p, bytes).map_err(|e| format!("restoring {}: {e}", p.display()))?;
    }
    app.studio.save().map_err(|e| format!("{e:#}"))?;
    app.cue_text = snap.cue_text;
    app.words = None;
    app.words_dirty = false;
    app.reparse();
    if narration_state(app) == Narration::Stale {
        // The file still has the cut in it: rewrite it once rendered.
        app.studio.changed_at = Instant::now() - DEBOUNCE;
    }
    Ok(())
}

pub(super) fn sync_narration(app: &mut VgApp, ctx: &egui::Context) {
    if app.studio.out.as_ref().is_some_and(|o| o.mix == app.studio.mix) {
        use_as_narration(app, ctx);
    } else {
        // Render first (in the background); `background` finishes the job.
        app.studio.pending_sync = true;
        app.studio.changed_at = Instant::now() - DEBOUNCE;
        ctx.request_repaint();
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn shortcuts(app: &mut VgApp, ctx: &egui::Context) {
    let typing = ctx.egui_wants_keyboard_input();
    let s = &mut app.studio;
    // Shift+Z first: egui's Ctrl+Z check would also match it.
    let (redo, undo, redo2) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z),
            i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z),
            i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y),
        )
    });
    if undo {
        s.undo();
        s.changed();
    }
    if redo || redo2 {
        s.redo();
        s.changed();
    }
    if typing {
        return;
    }
    let (zin, zout, fit) = ctx.input_mut(|i| {
        let none = egui::Modifiers::NONE;
        (
            i.consume_key(none, egui::Key::Plus) || i.consume_key(none, egui::Key::Equals),
            i.consume_key(none, egui::Key::Minus),
            i.consume_key(none, egui::Key::F),
        )
    });
    let length = s.length();
    if zin {
        let c = s.time;
        zoom(s, c, 0.5, length);
    }
    if zout {
        let c = s.time;
        zoom(s, c, 2.0, length);
    }
    if fit {
        if s.sel.is_some() {
            zoom_to_selection(s);
        } else {
            s.fit_view();
        }
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::M)) {
        s.adjust_selection(vg_audio::Adjust::Mute);
    }
    let (play, cut, ab, home, back, fwd, esc) = ctx.input_mut(|i| {
        let none = egui::Modifiers::NONE;
        (
            i.consume_key(none, egui::Key::Space),
            i.consume_key(none, egui::Key::Delete) || i.consume_key(none, egui::Key::Backspace) || i.consume_key(none, egui::Key::X),
            i.consume_key(none, egui::Key::B),
            i.consume_key(none, egui::Key::Home),
            i.consume_key(none, egui::Key::ArrowLeft),
            i.consume_key(none, egui::Key::ArrowRight),
            i.consume_key(none, egui::Key::Escape),
        )
    });
    if play {
        s.toggle_play();
    }
    if cut {
        s.cut_selection();
    }
    if ab {
        s.original = !s.original;
        if s.playing {
            s.start_audio();
        }
    }
    if home {
        s.seek(0.0);
    }
    if back {
        s.seek(s.time - 1.0);
    }
    if fwd {
        s.seek(s.time + 1.0);
    }
    if esc {
        s.sel = None;
    }
}

// ---- left: tracks and edit tools ------------------------------------------

fn tracks_panel(app: &mut VgApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| sidebar(app, ui));
}

/// A section heading in the sidebar.
fn section(ui: &mut egui::Ui, title: &str, meta: &str) {
    ui.add_space(10.0);
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(theme::caption(title).color(theme::WHITE));
        if !meta.is_empty() {
            ui.label(egui::RichText::new(meta).size(10.5).color(theme::MUTED));
        }
    });
    ui.add_space(2.0);
}

fn sidebar(app: &mut VgApp, ui: &mut egui::Ui) {
    let s = &app.studio;
    let name = s.path.as_deref().map(file_name).unwrap_or_default();
    theme::panel_title(ui, "Audio", &name);
    ui.add_space(4.0);
    if s.mix.tracks.is_empty() {
        theme::weak(ui, "No narration yet.");
    } else {
        theme::weak(ui, format!("{} track{} · {} · 48 kHz mono", s.mix.tracks.len(), plural(s.mix.tracks.len()), vg_core::format_time(s.length())));
    }
    ui.add_space(4.0);
    let has_mix = !app.studio.mix.tracks.is_empty();
    let recording = app.record.recording().is_some();
    ui.horizontal_wrapped(|ui| {
        let label = if has_mix { "Record new track" } else { "Record narration" };
        if theme::tool_button_enabled(ui, label, None, false, !recording).clicked() {
            if has_mix {
                super::record::start_track(app);
            } else {
                app.record.open = true;
                super::record::start(app);
            }
        }
        if theme::tool_button(ui, "Open recording", None, app.recordings.open).clicked() {
            app.recordings.open = !app.recordings.open;
        }
    });
    theme::weak(ui, if has_mix {
        "Record new track: a pickup or retake at the playhead. Open recording: earlier takes and their transcripts."
    } else {
        "Record narration, or open an earlier recording."
    });

    section(ui, "Background music", "");
    super::music::section(app, ui);

    // What the selection can do.
    let s = &mut app.studio;
    let sel = s.sel;
    match sel {
        Some((a, b)) => section(ui, "Selection", &format!("{} – {} · {:.2} s · track {}", vg_core::format_time(a), vg_core::format_time(b), b - a, s.track + 1)),
        None => section(ui, "Selection", ""),
    }
    let on = sel.is_some();
    let mut err = None;
    ui.horizontal_wrapped(|ui| {
        if theme::tool_button_enabled(ui, "Cut", Some("Del"), false, on).clicked() {
            s.cut_selection();
        }
        if theme::tool_button_enabled(ui, "Mute", Some("M"), false, on).clicked() {
            s.adjust_selection(vg_audio::Adjust::Mute);
        }
        if theme::tool_button_enabled(ui, "Keep only this", None, false, on).clicked() {
            s.trim_to_selection();
        }
    });
    ui.horizontal(|ui| {
        ui.add_enabled(on, egui::Slider::new(&mut s.sel_gain, -24.0..=12.0).suffix(" dB").fixed_decimals(1).step_by(0.5));
        if theme::tool_button_enabled(ui, "Set gain", None, false, on).clicked() {
            let g = s.sel_gain;
            s.adjust_selection(vg_audio::Adjust::Gain(g));
        }
    });
    ui.horizontal_wrapped(|ui| {
        if theme::tool_button_enabled(ui, "Fade in", None, false, on).clicked() {
            s.adjust_selection(vg_audio::Adjust::FadeIn);
        }
        if theme::tool_button_enabled(ui, "Fade out", None, false, on).clicked() {
            s.adjust_selection(vg_audio::Adjust::FadeOut);
        }
        if theme::tool_button_enabled(ui, "Reset", None, false, on).clicked() {
            s.reset_selection();
        }
    });
    ui.horizontal_wrapped(|ui| {
        if theme::tool_button_enabled(ui, "Learn room tone", None, false, on).clicked() {
            err = s.learn_room_tone().err();
        }
        if theme::tool_button_enabled(ui, "Zoom to it", Some("F"), false, on).clicked() {
            zoom_to_selection(s);
        }
    });
    theme::weak(ui, if on {
        "Cut removes it and closes the gap; Mute silences it and keeps the timing. Gain and fades apply after the effects. Learn room tone: a second of just room noise teaches the noise reduction."
    } else {
        "Drag across a track to select part of it, then cut, mute, turn it up or down, or fade it here."
    });
    if let Some(e) = err {
        app.error(e);
    }

    // The selected track's adjustments.
    let s = &mut app.studio;
    let regions: Vec<(usize, vg_audio::Region, (f64, f64))> = s.mix.tracks.get(s.track).map(|t| t.regions.iter().enumerate().map(|(k, r)| (k, *r, t.region_span(r))).collect()).unwrap_or_default();
    section(ui, "Adjustments", &if regions.is_empty() { String::new() } else { format!("{} on track {}", regions.len(), s.track + 1) });
    if regions.is_empty() {
        theme::weak(ui, "None on this track.");
    }
    let mut remove = None;
    for (k, r, (a, b)) in &regions {
        ui.horizontal(|ui| {
            let text = format!("{} – {}", vg_core::format_time(*a), vg_core::format_time(*b));
            if ui.add(egui::Button::new(egui::RichText::new(text).monospace().size(11.0)).frame(false)).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
                && b > a {
                    s.sel = Some((*a, *b));
                    s.seek(*a);
                }
            ui.label(egui::RichText::new(r.adjust.label()).size(11.0).color(if b > a { theme::TEXT2 } else { theme::MUTED }));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").clicked() {
                    remove = Some(*k);
                }
            });
        });
    }
    if let Some(k) = remove
        && let Some(t) = s.mix.tracks.get_mut(s.track) {
            t.regions.remove(k);
            s.changed();
        }

    section(ui, "History", "");
    ui.horizontal(|ui| {
        if theme::tool_button_enabled(ui, "Undo", Some("Ctrl+Z"), false, !s.undo.is_empty() || s.mix != s.undo_base).clicked() {
            s.undo();
            s.changed();
        }
        if theme::tool_button_enabled(ui, "Redo", Some("Ctrl+Y"), false, !s.redo.is_empty()).clicked() {
            s.redo();
            s.changed();
        }
    });

    section(ui, "Tighten pauses", "");
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(&mut s.tighten_max, 0.15..=1.5).suffix(" s").fixed_decimals(2).text("longest"));
    });
    let tm = s.tighten_max;
    let found = s.pauses().map(|p| (p.len(), p.iter().map(|(a, b)| b - a - tm).sum::<f64>()));
    ui.horizontal(|ui| {
        let (n, saves) = found.unwrap_or((0, 0.0));
        if theme::tool_button_enabled(ui, "Tighten", None, false, n > 0).clicked()
            && let Some(out) = s.out.clone()
                && let (Some(t), Some(buf)) = (s.mix.tracks.get_mut(s.track), out.r.tracks.get(s.track)) {
                    vg_audio::edit::tighten(t, buf, RATE as f32, s.tighten_max);
                    s.changed();
                }
        theme::weak(ui, match found {
            Some(_) if n > 0 => format!("{n} longer pause{} (hatched) · saves {saves:.1} s", plural(n)),
            Some(_) => "No pauses longer than that.".into(),
            None => "Waiting for the render…".into(),
        });
    });
}

/// Add `path` as a new track at `at` (default: the playhead). A `voice` track
/// gets the same clean-up as the first track (a pickup line, a retake); other
/// audio (music, effects) goes in plain and a bit under the voice. With no edit
/// open yet, the recording is opened instead.
pub(super) fn add_track(app: &mut VgApp, path: &Path, voice: bool, at: Option<f64>) {
    if app.studio.mix.tracks.is_empty() {
        if let Err(e) = app.studio.open_for(path) {
            app.error(format!("{e:#}"));
        }
        return;
    }
    let s = &mut app.studio;
    let base = s.base();
    let rel = super::relative_path(path, &base);
    match vg_media::probe_duration(path) {
        Ok(dur) => {
            let name = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "track".into());
            let mut t = Track::new(&name, rel, dur);
            t.offset = at.unwrap_or(s.time);
            if voice {
                t.effects = s.mix.tracks.first().map(|f| f.effects.clone()).unwrap_or_else(|| Preset::Condenser.effects());
                t.level = s.mix.tracks.first().is_none_or(|f| f.level);
            } else {
                t.gain_db = -12.0;
                t.level = true;
            }
            s.mix.tracks.push(t);
            s.track = s.mix.tracks.len() - 1;
            s.changed();
        }
        Err(e) => app.error(format!("{e:#}")),
    }
}

// ---- centre: the editor ------------------------------------------------------
//
// Laid out like Audacity: a toolbar, then per track a control panel on the left,
// an amplitude scale, and the waveform (min/max with RMS inside); a ruler over
// the waveforms, a scrollbar under them, and a selection bar at the bottom.

const RULER_H: f32 = 24.0;
const TCP_W: f32 = 184.0;
const SCALE_W: f32 = 34.0;
const SCROLL_H: f32 = 12.0;
const SELBAR_H: f32 = 30.0;
/// The view reaches a little past the end of the mix.
const VIEW_PAD: f64 = 1.02;

/// Waveform colours: peaks (min..max) and RMS.
const PEAK: egui::Color32 = egui::Color32::from_gray(0x8c);
const RMS: egui::Color32 = egui::Color32::from_gray(0xd8);

fn editor(app: &mut VgApp, ui: &mut egui::Ui) {
    transport(app, ui);
    ui.add_space(4.0);
    app.studio.set_volume(app.prefs.out_volume());
    let s = &mut app.studio;
    if s.mix.tracks.is_empty() {
        let r = ui.available_rect_before_wrap();
        ui.painter().text(r.center() - egui::vec2(0.0, 10.0), egui::Align2::CENTER_CENTER, "NO NARRATION", egui::FontId::monospace(12.0), theme::WHITE);
        ui.painter().text(r.center() + egui::vec2(0.0, 10.0), egui::Align2::CENTER_CENTER, "Record narration or Open recording on the left", egui::FontId::monospace(10.5), theme::MUTED);
        return;
    }
    let n = s.mix.tracks.len();
    s.heights.resize(n, TRACK_H);

    let full = ui.available_rect_before_wrap();
    let full = egui::Rect::from_min_max(full.min, egui::pos2(full.max.x, full.max.y - SELBAR_H - 20.0));
    ui.allocate_rect(full, egui::Sense::hover());
    let wave_x = full.left() + TCP_W + SCALE_W;
    let ruler = egui::Rect::from_min_max(egui::pos2(wave_x, full.top()), egui::pos2(full.right(), full.top() + RULER_H));
    let area = egui::Rect::from_min_max(egui::pos2(full.left(), ruler.bottom()), egui::pos2(full.right(), full.bottom() - SCROLL_H));
    let waves = egui::Rect::from_min_max(egui::pos2(wave_x, area.top()), area.max);
    let scrollbar = egui::Rect::from_min_max(egui::pos2(wave_x, area.bottom()), full.max);

    let painter = ui.painter_at(full);
    painter.rect_filled(full, 0.0, theme::BLACK);
    // The empty area below the tracks, like Audacity's grey backdrop.
    painter.rect_filled(area, 0.0, egui::Color32::from_gray(0x06));

    let length = s.length().max(0.1);
    let span = s.view_span.unwrap_or(length * VIEW_PAD).clamp(0.05, (length * VIEW_PAD).max(0.05));
    s.view_start = s.view_start.clamp(0.0, (length * VIEW_PAD - span).max(0.0));
    let start = s.view_start;
    let x_of = |t: f64| waves.left() + ((t - start) / span) as f32 * waves.width();
    let t_of = |x: f32| start + ((x - waves.left()) / waves.width()) as f64 * span;
    let px_per_s = waves.width() as f64 / span;

    // Vertical layout of the lanes (scrolled).
    let total_h: f32 = s.heights.iter().sum::<f32>() + n as f32 * 2.0;
    s.vscroll = s.vscroll.clamp(0.0, (total_h - area.height()).max(0.0));
    let mut y = area.top() - s.vscroll;
    let lanes: Vec<egui::Rect> = s
        .heights
        .iter()
        .map(|h| {
            let r = egui::Rect::from_min_size(egui::pos2(full.left(), y), egui::vec2(full.width(), *h));
            y += h + 2.0;
            r
        })
        .collect();

    // Ruler.
    painter.rect_filled(egui::Rect::from_min_max(full.left_top(), egui::pos2(full.right(), ruler.bottom())), 0.0, egui::Color32::from_gray(0x0a));
    let step = [0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0].into_iter().find(|st| st * px_per_s >= 90.0).unwrap_or(600.0);
    let minor = step / 5.0;
    let mut t = (start / minor).floor() * minor;
    while t <= start + span {
        let x = x_of(t);
        let major = ((t / step).round() * step - t).abs() < minor * 0.01;
        painter.line_segment([egui::pos2(x, ruler.bottom() - if major { 9.0 } else { 4.0 }), egui::pos2(x, ruler.bottom())], egui::Stroke::new(1.0_f32, if major { theme::BORDER } else { theme::RULE }));
        if major {
            let label = if step < 1.0 { format!("{:.2}", t) } else { vg_core::format_time(t).trim_end_matches(".00").to_string() };
            painter.text(egui::pos2(x + 3.0, ruler.top() + 7.0), egui::Align2::LEFT_CENTER, label, egui::FontId::monospace(9.5), theme::TEXT2);
        }
        t += minor;
    }
    painter.line_segment([egui::pos2(full.left(), ruler.bottom()), ruler.right_bottom()], egui::Stroke::new(1.0_f32, theme::BORDER));
    painter.text(egui::pos2(full.left() + 8.0, ruler.center().y), egui::Align2::LEFT_CENTER, if s.original { "ORIGINAL · AS RECORDED" } else { "PROCESSED · AS EXPORTED" }, egui::FontId::monospace(9.0), theme::MUTED);

    let out = s.out.clone();
    let fresh = out.as_ref().is_some_and(|o| o.mix == s.mix);
    let pauses: Vec<(f64, f64)> = s.pauses().map(<[_]>::to_vec).unwrap_or_default();
    let lane_painter = painter.with_clip_rect(area);
    let db_scale = s.db_scale;
    // Amplitude → lane offset (0..1 of the half height), linear or dB (−60..0).
    let amp = move |v: f32| -> f32 {
        if db_scale {
            let d = vg_audio::dsp::gain_to_db(v.abs());
            (((d + 60.0) / 60.0).clamp(0.0, 1.0)).copysign(v)
        } else {
            v.clamp(-1.0, 1.0)
        }
    };
    let mut tcp_rects = vec![];
    for i in 0..n {
        let lane = lanes[i];
        if lane.bottom() < area.top() || lane.top() > area.bottom() {
            tcp_rects.push(None);
            continue;
        }
        let selected = i == s.track;
        let tcp = egui::Rect::from_min_size(lane.min, egui::vec2(TCP_W, lane.height()));
        let scale = egui::Rect::from_min_size(egui::pos2(tcp.right(), lane.top()), egui::vec2(SCALE_W, lane.height()));
        let wave = egui::Rect::from_min_max(egui::pos2(wave_x, lane.top()), egui::pos2(full.right(), lane.bottom()));
        tcp_rects.push(Some(tcp));
        lane_painter.rect_filled(lane, 0.0, theme::BLACK);
        lane_painter.rect_filled(tcp, 0.0, if selected { egui::Color32::from_gray(0x14) } else { egui::Color32::from_gray(0x0b) });
        lane_painter.rect_stroke(tcp, 0.0, egui::Stroke::new(1.0_f32, if selected { theme::FOCUS } else { theme::RULE }), egui::StrokeKind::Inside);
        let track = &s.mix.tracks[i];
        // The clip: a header bar and a body from where the track starts to where it ends.
        // Music plays (looped or not) until the mix ends.
        let shown_end = match &track.music {
            Some(m) if m.loop_it => s.mix.length(),
            Some(_) => track.end().min(s.mix.length()),
            None => track.end(),
        };
        let (cx0, cx1) = (x_of(track.offset).max(wave.left()), x_of(shown_end).min(wave.right()));
        if cx1 > cx0 {
            let head = egui::Rect::from_min_max(egui::pos2(cx0, wave.top()), egui::pos2(cx1, wave.top() + 14.0));
            let body = egui::Rect::from_min_max(egui::pos2(cx0, head.bottom()), egui::pos2(cx1, wave.bottom()));
            lane_painter.rect_filled(body, 0.0, if selected { egui::Color32::from_gray(0x10) } else { egui::Color32::from_gray(0x0c) });
            lane_painter.rect_filled(head, 0.0, if selected { egui::Color32::from_gray(0x2a) } else { egui::Color32::from_gray(0x1c) });
            lane_painter.with_clip_rect(head.intersect(area)).text(head.left_center() + egui::vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, &track.name, egui::FontId::monospace(9.0), if selected { theme::WHITE } else { theme::TEXT2 });
            // Amplitude scale and zero line.
            let wmid = body.center().y;
            let whalf = body.height() / 2.0 - 3.0;
            let ticks: Vec<(f32, String)> = if db_scale {
                // dB: 0 at the edges down to −∞ at the centre line.
                let mut t = vec![(0.0f32, "-∞".to_string())];
                for d in [0.0f32, -12.0, -24.0, -36.0, -48.0] {
                    let v = (d + 60.0) / 60.0;
                    t.push((v, format!("{d:.0}")));
                    t.push((-v, format!("{d:.0}")));
                }
                t
            } else {
                [1.0f32, 0.5, 0.0, -0.5, -1.0].iter().map(|v| (*v, format!("{v:.1}"))).collect()
            };
            let every = if body.height() > 110.0 { 1 } else { 2 };
            for (k, (v, label)) in ticks.iter().enumerate() {
                let y = wmid - v * whalf;
                lane_painter.line_segment([egui::pos2(scale.right() - 4.0, y), egui::pos2(scale.right(), y)], egui::Stroke::new(1.0_f32, theme::MUTED));
                if k % every == 0 || *v == 0.0 {
                    lane_painter.text(egui::pos2(scale.right() - 6.0, y), egui::Align2::RIGHT_CENTER, label, egui::FontId::monospace(8.0), theme::MUTED);
                }
            }
            lane_painter.line_segment([egui::pos2(cx0, wmid), egui::pos2(cx1, wmid)], egui::Stroke::new(1.0_f32, egui::Color32::from_gray(0x2e)));
            // The waveform: what you'll hear (processed, at its exported level) or the take as recorded.
            if let Some(o) = &out {
                let shown = o.mix.tracks.get(i);
                let buf = if s.original { o.tracks.get(i) } else { o.tracks_wet.get(i) };
                if let (Some(pyr), Some(shown)) = (buf, shown) {
                    let gain = if s.original { 1.0 } else { vg_audio::dsp::db_to_gain(shown.gain_db + o.r.stats.master_gain_db) };
                    let dim = !o.mix.audible(i);
                    let (pk, rm) = if dim { (egui::Color32::from_gray(0x3a), egui::Color32::from_gray(0x55)) } else { (PEAK, RMS) };
                    let mut clipped = vec![];
                    for px in (cx0.max(body.left()) as i32)..(cx1 as i32) {
                        let x = px as f32;
                        let (t0, t1) = (t_of(x) - shown.offset, t_of(x + 1.0) - shown.offset);
                        if t1 <= 0.0 {
                            continue;
                        }
                        let (a, b) = ((t0.max(0.0) * RATE as f64) as usize, (t1 * RATE as f64).ceil() as usize);
                        let Some((lo, hi, rms)) = pyr.range(a, b.max(a + 1)) else { break };
                        if s.original && (hi >= 0.999 || lo <= -0.999) {
                            clipped.push(x);
                        }
                        let (lo, hi, rms) = (lo * gain, hi * gain, rms * gain);
                        let (ylo, yhi) = (wmid - amp(lo) * whalf, wmid - amp(hi) * whalf);
                        lane_painter.line_segment([egui::pos2(x + 0.5, yhi), egui::pos2(x + 0.5, ylo.max(yhi + 1.0))], egui::Stroke::new(1.0_f32, pk));
                        let r = amp(rms).abs().min(amp(hi).abs().max(amp(lo).abs()));
                        if r * whalf >= 0.5 {
                            lane_painter.line_segment([egui::pos2(x + 0.5, wmid - r * whalf), egui::pos2(x + 0.5, wmid + r * whalf)], egui::Stroke::new(1.0_f32, rm));
                        }
                    }
                    // Clipping in the recording: bright ticks at the lane edges.
                    for x in clipped {
                        for yy in [body.top(), body.bottom() - 4.0] {
                            lane_painter.line_segment([egui::pos2(x + 0.5, yy), egui::pos2(x + 0.5, yy + 4.0)], egui::Stroke::new(1.0_f32, theme::WHITE));
                        }
                    }
                }
            }
            // Long pauses the Tighten button would shorten.
            if selected && fresh {
                for &(a, b) in &pauses {
                    let (x0, x1) = (x_of(track.offset + a + s.tighten_max / 2.0).max(body.left()), x_of(track.offset + b - s.tighten_max / 2.0).min(body.right()));
                    if x1 <= x0 {
                        continue;
                    }
                    let pr = egui::Rect::from_min_max(egui::pos2(x0, body.top()), egui::pos2(x1, body.bottom()));
                    let hp = lane_painter.with_clip_rect(pr.intersect(area));
                    let mut x = pr.left() - pr.height();
                    while x < pr.right() {
                        hp.line_segment([egui::pos2(x, pr.bottom()), egui::pos2(x + pr.height(), pr.top())], egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(22)));
                        x += 7.0;
                    }
                }
            }
            // Adjustments: muted stretches darkened, gain labelled, fades drawn as ramps.
            for r in &track.regions {
                let (a, b) = track.region_span(r);
                let (x0, x1) = (x_of(a).max(body.left()), x_of(b).min(body.right()));
                if x1 <= x0 {
                    continue;
                }
                let rr = egui::Rect::from_min_max(egui::pos2(x0, body.top()), egui::pos2(x1, body.bottom()));
                let (fx0, fx1) = (x_of(a), x_of(b));
                let lp = lane_painter.with_clip_rect(rr.intersect(area));
                match r.adjust {
                    vg_audio::Adjust::Mute => {
                        lp.rect_filled(rr, 0.0, egui::Color32::from_black_alpha(150));
                        lp.text(rr.left_top() + egui::vec2(4.0, 3.0), egui::Align2::LEFT_TOP, "MUTED", egui::FontId::monospace(8.5), theme::TEXT2);
                    }
                    vg_audio::Adjust::Gain(db) => {
                        lp.rect_filled(rr, 0.0, egui::Color32::from_white_alpha(if db > 0.0 { 14 } else { 6 }));
                        lp.text(rr.left_top() + egui::vec2(4.0, 3.0), egui::Align2::LEFT_TOP, format!("{db:+.1} dB"), egui::FontId::monospace(8.5), theme::TEXT2);
                    }
                    vg_audio::Adjust::FadeIn | vg_audio::Adjust::FadeOut => {
                        let (y0, y1) = if r.adjust == vg_audio::Adjust::FadeIn { (body.bottom() - 2.0, body.top() + 2.0) } else { (body.top() + 2.0, body.bottom() - 2.0) };
                        lp.line_segment([egui::pos2(fx0, y0), egui::pos2(fx1, y1)], egui::Stroke::new(1.0_f32, theme::FOCUS));
                    }
                }
                lp.line_segment([egui::pos2(x0, body.top()), egui::pos2(x1, body.top())], egui::Stroke::new(2.0_f32, egui::Color32::from_white_alpha(90)));
            }
            // Cuts: a split line with a marker in the header.
            for j in track.joins() {
                let x = x_of(j);
                if x >= body.left() && x <= body.right() {
                    theme::dashed_rect(&lane_painter, egui::Rect::from_min_max(egui::pos2(x, body.top()), egui::pos2(x, body.bottom())), egui::Stroke::new(1.0_f32, theme::FOCUS));
                    lane_painter.add(egui::Shape::convex_polygon(vec![egui::pos2(x - 4.0, head.top() + 3.0), egui::pos2(x + 4.0, head.top() + 3.0), egui::pos2(x, head.bottom() - 2.0)], theme::WHITE, egui::Stroke::NONE));
                }
            }
        }
        // Selection: Audacity-style, a light wash over the selected track.
        if let (Some((a, b)), true) = (s.sel, selected) {
            let sr = egui::Rect::from_min_max(egui::pos2(x_of(a).max(wave.left()), wave.top()), egui::pos2(x_of(b).min(wave.right()), wave.bottom()));
            if sr.width() > 0.0 {
                lane_painter.rect_filled(sr, 0.0, egui::Color32::from_white_alpha(34));
                for x in [x_of(a), x_of(b)] {
                    if x >= wave.left() && x <= wave.right() {
                        lane_painter.line_segment([egui::pos2(x, wave.top()), egui::pos2(x, wave.bottom())], egui::Stroke::new(1.0_f32, theme::WHITE));
                    }
                }
            }
        }
        // Resize grip along the bottom edge.
        lane_painter.line_segment([egui::pos2(lane.left(), lane.bottom() + 1.0), egui::pos2(lane.right(), lane.bottom() + 1.0)], egui::Stroke::new(1.0_f32, theme::RULE));
    }

    // Selection in the ruler, and the playhead.
    if let Some((a, b)) = s.sel {
        let r = egui::Rect::from_min_max(egui::pos2(x_of(a).max(ruler.left()), ruler.top()), egui::pos2(x_of(b).min(ruler.right()), ruler.bottom()));
        if r.width() > 0.0 {
            painter.rect_filled(r, 0.0, egui::Color32::from_white_alpha(30));
        }
    }
    let px = x_of(s.time);
    if px >= waves.left() && px <= waves.right() {
        let bottom = lanes.last().map(|l| l.bottom()).unwrap_or(area.bottom()).min(area.bottom());
        painter.with_clip_rect(egui::Rect::from_min_max(waves.min, area.max)).line_segment([egui::pos2(px, area.top()), egui::pos2(px, bottom)], egui::Stroke::new(1.0_f32, theme::WHITE));
        painter.add(egui::Shape::convex_polygon(vec![egui::pos2(px - 5.0, ruler.bottom() - 9.0), egui::pos2(px + 5.0, ruler.bottom() - 9.0), egui::pos2(px, ruler.bottom())], theme::WHITE, egui::Stroke::NONE));
    }

    // Horizontal scrollbar.
    painter.rect_filled(scrollbar, 0.0, theme::BLACK);
    let total = length * VIEW_PAD;
    let thumb = egui::Rect::from_min_max(
        egui::pos2(scrollbar.left() + (start / total) as f32 * scrollbar.width(), scrollbar.center().y - 3.0),
        egui::pos2(scrollbar.left() + ((start + span) / total).min(1.0) as f32 * scrollbar.width(), scrollbar.center().y + 3.0),
    );
    let sb = ui.interact(scrollbar, ui.id().with("studio-hscroll"), egui::Sense::click_and_drag());
    // Like every scrollbar: a thin rule-grey handle, brighter under the pointer, white while dragged.
    let handle = if sb.dragged() { theme::WHITE } else if sb.hovered() { theme::FOCUS } else { theme::RULE };
    painter.rect_filled(thumb, 1.0, handle);
    if sb.dragged() {
        s.view_start = (s.view_start + sb.drag_delta().x as f64 / scrollbar.width() as f64 * total).max(0.0);
    } else if sb.clicked()
        && let Some(p) = sb.interact_pointer_pos() {
            s.view_start = ((p.x - scrollbar.left()) as f64 / scrollbar.width() as f64 * total - span / 2.0).max(0.0);
        }

    // Track control panels (real widgets, clipped to the track area).
    for i in 0..n {
        let Some(tcp) = tcp_rects[i] else { continue };
        let inner = tcp.shrink2(egui::vec2(8.0, 6.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
        child.set_clip_rect(tcp.intersect(area));
        track_controls(&mut child, s, i);
        // Clicking the panel's background selects the track.
        let bg = ui.interact(tcp.intersect(area), ui.id().with(("tcp", i)), egui::Sense::click());
        if bg.clicked() {
            s.track = i;
        }
    }

    // Lane resize grips.
    for i in 0..n {
        let grip = egui::Rect::from_min_max(egui::pos2(lanes[i].left(), lanes[i].bottom() - 2.0), egui::pos2(lanes[i].right(), lanes[i].bottom() + 4.0)).intersect(area);
        if grip.height() <= 0.0 {
            continue;
        }
        let r = ui.interact(grip, ui.id().with(("grip", i)), egui::Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeVertical);
        if r.drag_started()
            && let Some(p) = r.interact_pointer_pos() {
                s.resize = Some((i, s.heights[i], p.y));
            }
        if let (Some((t, h0, y0)), Some(p)) = (s.resize, r.interact_pointer_pos())
            && t == i && r.dragged() {
                s.heights[i] = (h0 + p.y - y0).clamp(60.0, 700.0);
            }
        if r.drag_stopped() {
            s.resize = None;
        }
    }

    // Waveform interaction: click seeks; drag selects; Ctrl+drag slides a track.
    let response = ui.interact(egui::Rect::from_min_max(egui::pos2(wave_x, ruler.top()), area.max), ui.id().with("studio-waves"), egui::Sense::click_and_drag());
    let lane_at = |y: f32| lanes.iter().position(|l| y >= l.top() && y <= l.bottom() + 2.0);
    if response.drag_started()
        && let Some(p) = response.interact_pointer_pos() {
            let t = t_of(p.x).clamp(0.0, length);
            if let Some(i) = lane_at(p.y) {
                s.track = i;
            }
            s.drag = Some(match (ui.input(|i| i.modifiers.command), lane_at(p.y)) {
                (true, Some(track)) => Drag::Move { track, offset: s.mix.tracks[track].offset, from: t },
                _ => Drag::Select(t),
            });
        }
    if let (Some(d), Some(p)) = (s.drag, response.interact_pointer_pos()) {
        let t = t_of(p.x).clamp(0.0, length);
        match d {
            Drag::Select(a) => s.sel = Some((a.min(t), a.max(t))).filter(|(a, b)| b - a > 0.002),
            Drag::Move { track, offset, from } => {
                s.mix.tracks[track].offset = (offset + t - from).max(0.0);
                s.changed();
            }
        }
        // Keep the drag in view.
        if p.x > waves.right() - 8.0 {
            s.view_start += span * 0.02;
        } else if p.x < waves.left() + 8.0 {
            s.view_start = (s.view_start - span * 0.02).max(0.0);
        }
    }
    if response.drag_stopped() {
        if let (Some(Drag::Select(_)), Some((a, _))) = (s.drag, s.sel) {
            s.seek(a);
        }
        s.drag = None;
    }
    if response.clicked()
        && let Some(p) = response.interact_pointer_pos() {
            if let Some(i) = lane_at(p.y) {
                s.track = i;
            }
            s.sel = None;
            s.seek(t_of(p.x));
        }
    // The wheel, as in Audacity: scroll the tracks, Shift pans, Ctrl zooms.
    if ui.rect_contains_pointer(full) {
        let (scroll, shift, ctrl) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.shift, i.modifiers.command));
        let dy = if scroll.y != 0.0 { scroll.y } else { scroll.x };
        if dy != 0.0 {
            if ctrl {
                let anchor = ui.input(|i| i.pointer.hover_pos()).map(|p| t_of(p.x.max(waves.left()))).unwrap_or(start + span / 2.0);
                zoom(s, anchor, 1.0 - dy as f64 * 0.003, length);
            } else if shift || scroll.x != 0.0 || total_h <= area.height() {
                s.view_start = (s.view_start - dy as f64 / px_per_s).max(0.0);
            } else {
                s.vscroll -= dy;
            }
        }
    }

    selection_bar(ui, s);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (k, what) in [("drag", "select"), ("Del", "cut"), ("Ctrl+drag", "slide track"), ("B", "A/B"), ("Ctrl+wheel", "zoom"), ("Shift+wheel", "pan"), ("+ − F", "zoom · fit"), ("Ctrl+Z", "undo")] {
            theme::key_chip(ui, k);
            ui.label(egui::RichText::new(what).size(10.5).color(theme::MUTED));
            ui.add_space(6.0);
        }
    });
}

/// Zoom by `k` (< 1 zooms in) around `anchor`.
fn zoom(s: &mut Studio, anchor: f64, k: f64, length: f64) {
    let span = s.view_span.unwrap_or(length * VIEW_PAD);
    let new_span = (span * k).clamp(0.05, (length * VIEW_PAD).max(0.05));
    s.view_start = (anchor - (anchor - s.view_start) * new_span / span).max(0.0);
    s.view_span = Some(new_span);
}

fn zoom_to_selection(s: &mut Studio) {
    if let Some((a, b)) = s.sel {
        let pad = (b - a) * 0.05;
        s.view_start = (a - pad).max(0.0);
        s.view_span = Some((b - a + 2.0 * pad).max(0.05));
    }
}

/// Audacity's track control panel: name, mute/solo, gain and where it starts.
fn track_controls(ui: &mut egui::Ui, s: &mut Studio, i: usize) {
    let n = s.mix.tracks.len();
    let selected = i == s.track;
    let mut remove = false;
    let mut changed = false;
    let t = &mut s.mix.tracks[i];
    ui.spacing_mut().item_spacing = egui::vec2(4.0, 5.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{:02}", i + 1)).monospace().size(10.0).color(theme::MUTED));
        ui.add(egui::Label::new(egui::RichText::new(&t.name).size(11.5).color(if selected { theme::WHITE } else { theme::TEXT2 })).truncate());
        if i > 0 && n > 1 {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::tool_button(ui, "×", None, false).clicked() {
                    remove = true;
                }
            });
        }
    });
    ui.horizontal(|ui| {
        if theme::tool_button(ui, "Mute", None, t.mute).clicked() {
            t.mute = !t.mute;
            changed = true;
        }
        if theme::tool_button(ui, "Solo", None, t.solo).clicked() {
            t.solo = !t.solo;
            changed = true;
        }
    });
    if ui.available_height() > 20.0 {
        ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(40.0);
        changed |= ui.add(egui::Slider::new(&mut t.gain_db, -24.0..=12.0).suffix(" dB").fixed_decimals(1)).changed();
    }
    if ui.available_height() > 20.0 {
        ui.horizontal(|ui| {
            ui.label(theme::caption("Starts"));
            changed |= ui.add(egui::DragValue::new(&mut t.offset).speed(0.01).range(0.0..=36_000.0).suffix(" s").fixed_decimals(2)).changed();
        });
    }
    if ui.available_height() > 14.0 {
        let src = file_name(&t.source);
        ui.add(egui::Label::new(egui::RichText::new(format!("{src} · {} cut{}", t.clips.len().saturating_sub(1), plural(t.clips.len().saturating_sub(1)))).size(9.5).color(theme::MUTED)).truncate());
    }
    if remove {
        s.mix.tracks.remove(i);
        s.heights.remove(i);
        s.track = s.track.min(s.mix.tracks.len().saturating_sub(1));
        changed = true;
    }
    if changed {
        s.track = i.min(s.mix.tracks.len().saturating_sub(1));
        s.changed();
    }
}

/// Audacity's selection toolbar: start, end and length, editable.
fn selection_bar(ui: &mut egui::Ui, s: &mut Studio) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(theme::caption("Selection").color(theme::TEXT2));
        let len = s.length();
        let (mut a, mut b) = s.sel.unwrap_or((s.time, s.time));
        let fmt = |v: f64, _: std::ops::RangeInclusive<usize>| {
            let m = (v / 60.0).floor();
            format!("{}:{:06.3}", m, v - m * 60.0)
        };
        let mut changed = false;
        ui.label(theme::caption("Start").color(theme::MUTED));
        changed |= ui.add(egui::DragValue::new(&mut a).range(0.0..=len).speed(0.005).custom_formatter(fmt).custom_parser(vg_core::parse_time)).changed();
        ui.label(theme::caption("End").color(theme::MUTED));
        changed |= ui.add(egui::DragValue::new(&mut b).range(0.0..=len).speed(0.005).custom_formatter(fmt).custom_parser(vg_core::parse_time)).changed();
        let mut l = b - a;
        ui.label(theme::caption("Length").color(theme::MUTED));
        let lchanged = ui.add(egui::DragValue::new(&mut l).range(0.0..=len).speed(0.005).custom_formatter(|v, _| format!("{v:.3} s")).custom_parser(|t| t.trim_end_matches('s').trim().parse().ok())).changed();
        if lchanged {
            b = (a + l).min(len);
            changed = true;
        }
        if changed {
            s.sel = Some((a.min(b), a.max(b))).filter(|(a, b)| b - a > 0.0005);
            if s.sel.is_none() {
                s.seek(a);
            }
        }
        ui.add_space(12.0);
        ui.label(theme::caption("Cursor").color(theme::MUTED));
        ui.label(egui::RichText::new(fmt(s.time, 0..=0)).monospace().color(theme::WHITE));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::tool_button_enabled(ui, "Zoom to selection", None, false, s.sel.is_some()).clicked() {
                zoom_to_selection(s);
            }
            let (text, col) = match (&s.error, &s.job, &s.out) {
                (Some(e), _, _) => (format!("Error · {e}"), theme::WHITE),
                (None, Some(_), _) => ("RENDERING…".to_string(), theme::TEXT2),
                (None, None, Some(o)) if o.mix == s.mix => (format!("READY · {:.2} s", o.took.as_secs_f64()), theme::MUTED),
                _ => ("…".into(), theme::MUTED),
            };
            ui.add(egui::Label::new(egui::RichText::new(text).size(10.5).color(col)).truncate());
        });
    });
}

fn transport(app: &mut VgApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let s = &mut app.studio;
        if theme::icon_button(ui, theme::Icon::ToStart, "Start · Home").clicked() {
            s.seek(0.0);
        }
        let (icon, tip) = if s.playing { (theme::Icon::Pause, "Pause · Space") } else { (theme::Icon::Play, "Play · Space") };
        if theme::icon_button(ui, icon, tip).clicked() {
            s.toggle_play();
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new(vg_core::format_time(s.time)).size(16.0).color(theme::WHITE));
        ui.label(egui::RichText::new(format!("/ {}", vg_core::format_time(s.length()))).size(12.0).color(theme::MUTED));
        ui.add_space(8.0);
        let before = s.original;
        theme::segmented(ui, &mut s.original, &[(false, "Processed"), (true, "Original")]);
        if before != s.original && s.playing {
            s.start_audio();
        }
        ui.add_space(8.0);
        theme::segmented(ui, &mut s.db_scale, &[(false, "Linear"), (true, "dB")]);
        ui.add_space(8.0);
        let length = s.length();
        let centre = s.time;
        if theme::tool_button(ui, "+", None, false).clicked() {
            zoom(s, centre, 0.5, length);
        }
        if theme::tool_button(ui, "−", None, false).clicked() {
            zoom(s, centre, 2.0, length);
        }
        if theme::tool_button(ui, "Fit", Some("F"), false).clicked() {
            s.fit_view();
        }
        ui.add_space(8.0);
        meter(ui, s);
    });
}

/// Playback level: RMS bar with the peak as a tick, −60..0 dBFS.
fn meter(ui: &mut egui::Ui, s: &mut Studio) {
    let (peak, rms) = match (&s.out, s.playing) {
        (Some(o), true) => {
            let buf = if s.original { &o.r.dry } else { &o.r.wet };
            let i = (s.time * RATE as f64) as usize;
            let w = &buf[i.saturating_sub(2400).min(buf.len())..i.min(buf.len())];
            let p = w.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let r = (w.iter().map(|v| v * v).sum::<f32>() / w.len().max(1) as f32).sqrt();
            (vg_audio::dsp::gain_to_db(p), vg_audio::dsp::gain_to_db(r))
        }
        _ => (-90.0, -90.0),
    };
    // Fast up, slow down.
    let fall = |old: f32, new: f32| if new > old { new } else { (old - 1.0).max(new) };
    s.meter = (fall(s.meter.0, peak), fall(s.meter.1, rms));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(170.0, 18.0), egui::Sense::hover());
    let p = ui.painter();
    let bar = egui::Rect::from_min_max(rect.min + egui::vec2(0.0, 3.0), egui::pos2(rect.right() - 44.0, rect.bottom() - 3.0));
    p.rect_stroke(bar, 0.0, egui::Stroke::new(1.0_f32, theme::BORDER), egui::StrokeKind::Inside);
    let x = |db: f32| bar.left() + ((db + 60.0) / 60.0).clamp(0.0, 1.0) * bar.width();
    if s.meter.1 > -60.0 {
        p.rect_filled(egui::Rect::from_min_max(bar.min + egui::vec2(1.0, 1.0), egui::pos2(x(s.meter.1), bar.bottom() - 1.0)), 0.0, theme::TEXT2);
    }
    if s.meter.0 > -60.0 {
        let px = x(s.meter.0);
        p.line_segment([egui::pos2(px, bar.top()), egui::pos2(px, bar.bottom())], egui::Stroke::new(2.0_f32, theme::WHITE));
    }
    for db in [-48.0f32, -36.0, -24.0, -12.0, -6.0] {
        let tx = x(db);
        p.line_segment([egui::pos2(tx, bar.bottom() - 3.0), egui::pos2(tx, bar.bottom())], egui::Stroke::new(1.0_f32, theme::RULE));
    }
    let label = if s.meter.0 > -60.0 { format!("{:>5.1}", s.meter.0) } else { "  —".into() };
    p.text(egui::pos2(rect.right(), rect.center().y), egui::Align2::RIGHT_CENTER, format!("{label} dB"), egui::FontId::monospace(9.5), if s.meter.0 > -1.0 { theme::WHITE } else { theme::MUTED });
}

// ---- right: effects, master, output ------------------------------------------

fn fx_panel(app: &mut VgApp, ui: &mut egui::Ui) {
    let s = &mut app.studio;
    let Some(track) = s.mix.tracks.get(s.track) else {
        theme::panel_title(ui, "Effects", "");
        return;
    };
    theme::panel_title(ui, "Effects", &track.name);
    ui.add_space(4.0);
    let mut changed = false;
    if track.music.is_some() {
        let t = &mut s.mix.tracks[s.track];
        ui.label(theme::caption("Background music").color(theme::WHITE));
        let (m, g) = (t.music.as_mut().unwrap(), &mut t.gain_db);
        changed |= super::music::settings(m, g, ui);
        theme::weak(ui, "Ducking follows the voice tracks. Cut, mute or adjust parts of it on the left like any track; the effects below are off until you pick a preset.");
        ui.add_space(8.0);
    }
    egui::ScrollArea::vertical().max_height(ui.available_height() - 190.0).auto_shrink([false, false]).show(ui, |ui| {
        let t = &mut s.mix.tracks[s.track];
        ui.horizontal(|ui| {
            ui.menu_button(theme::caption("Preset"), |ui| {
                for p in Preset::ALL {
                    if ui.button(p.name()).clicked() {
                        let room = t.effects.iter().find_map(|e| if let Fx::Denoise { room_tone, .. } = e.fx { room_tone } else { None });
                        t.effects = p.effects();
                        for e in &mut t.effects {
                            if let Fx::Denoise { room_tone, .. } = &mut e.fx {
                                *room_tone = room;
                            }
                        }
                        changed = true;
                        ui.close();
                    }
                }
            });
            changed |= ui.checkbox(&mut t.level, "Auto-level input").changed();
        });
        theme::weak(ui, "Auto-level brings the take to -20 LUFS first, so the thresholds below work whatever your mic gain was.");
        ui.add_space(6.0);
        let mut swap = None;
        let count = t.effects.len();
        for i in 0..count {
            let e = &mut t.effects[i];
            let frame = egui::Frame::new().stroke(egui::Stroke::new(1.0_f32, if e.on { theme::BORDER } else { theme::RULE })).inner_margin(egui::Margin::symmetric(8, 6));
            frame.show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if theme::tool_button(ui, if e.on { "On" } else { "Off" }, None, e.on).clicked() {
                        e.on = !e.on;
                        changed = true;
                    }
                    ui.label(egui::RichText::new(e.fx.name().to_uppercase()).monospace().size(11.0).color(if e.on { theme::WHITE } else { theme::MUTED }));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if i + 1 < count && theme::tool_button(ui, "↓", None, false).clicked() {
                            swap = Some((i, i + 1));
                        }
                        if i > 0 && theme::tool_button(ui, "↑", None, false).clicked() {
                            swap = Some((i - 1, i));
                        }
                    });
                });
                if e.on {
                    ui.add_space(2.0);
                    changed |= effect_params(ui, e, &mut s.eq_drag);
                }
            });
            ui.add_space(4.0);
        }
        if let Some((a, b)) = swap {
            t.effects.swap(a, b);
            changed = true;
        }
    });
    if changed {
        s.changed();
    }

    // Master and output.
    ui.separator();
    let m = &mut s.mix.master;
    let mut mchanged = false;
    ui.horizontal(|ui| {
        ui.label(theme::caption("Loudness").color(theme::TEXT2));
        let mut target = m.loudness.map(|l| l.round() as i32);
        mchanged |= theme::segmented(ui, &mut target, &[(Some(-14), "-14 YouTube"), (Some(-16), "-16"), (Some(-19), "-19"), (None, "Off")]);
        m.loudness = target.map(|t| t as f32);
    });
    ui.horizontal(|ui| {
        mchanged |= ui.checkbox(&mut m.limiter, "Limiter").changed();
        mchanged |= ui.add_enabled(m.limiter, egui::Slider::new(&mut m.ceiling_db, -6.0..=-0.1).suffix(" dBFS").fixed_decimals(1).text("ceiling")).changed();
    });
    if mchanged {
        s.changed();
    }
    if let Some(o) = s.out.as_ref() {
        let st = &o.r.stats;
        let f = |l: Option<f32>| l.map(|l| format!("{l:.1}")).unwrap_or_else(|| "—".into());
        ui.label(egui::RichText::new(format!("IN {} LUFS → OUT {} LUFS · PEAK {:.1} dBFS · COMP ≤ {:.1} dB", f(st.input_lufs), f(st.output_lufs), st.output_peak_db, st.compression_db)).monospace().size(10.5).color(theme::TEXT2));
    }
    ui.add_space(6.0);
    let ready = s.out.as_ref().is_some_and(|o| o.mix == s.mix);
    let target = s.mix_wav();
    ui.horizontal(|ui| {
        if theme::tool_button_enabled(ui, "Use as narration", None, true, ready && target.is_some()).clicked() {
            use_as_narration(app, ui.ctx());
        }
        ui.checkbox(&mut app.studio.transcribe_after, "then transcribe");
    });
    let name = target.as_deref().map(file_name).unwrap_or_default();
    theme::weak(ui, format!("Writes {name} (48 kHz, 24-bit) and points the script at it. Cuts move the script's times and word timings with them. The recording itself is never changed."));
}

/// The controls for one effect. Returns whether anything changed.
fn effect_params(ui: &mut egui::Ui, e: &mut Effect, eq_drag: &mut Option<usize>) -> bool {
    let mut c = false;
    let mut slider = |ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str, dec: usize| {
        ui.label(theme::caption(label));
        c |= ui.add(egui::Slider::new(v, range).suffix(suffix).fixed_decimals(dec)).changed();
        ui.end_row();
    };
    let id = ui.id().with(e.fx.name());
    match &mut e.fx {
        Fx::HighPass { freq } => {
            egui::Grid::new(id).num_columns(2).show(ui, |ui| slider(ui, "Cut below", freq, 20.0..=250.0, " Hz", 0));
        }
        Fx::Denoise { reduction_db, threshold_db, room_tone } => {
            egui::Grid::new(id).num_columns(2).show(ui, |ui| {
                slider(ui, "Reduce by", reduction_db, 0.0..=36.0, " dB", 1);
                slider(ui, "Sensitivity", threshold_db, 0.0..=15.0, " dB", 1);
            });
            ui.horizontal(|ui| {
                match room_tone {
                    Some((a, b)) => {
                        theme::weak(ui, format!("Learned from room tone {:.2}–{:.2} s", a, b));
                        if theme::tool_button(ui, "Clear", None, false).clicked() {
                            *room_tone = None;
                            c = true;
                        }
                    }
                    None => theme::weak(ui, "Learned from the quietest moments."),
                }
            });
        }
        Fx::Gate { above_floor_db, range_db, release_ms } => {
            egui::Grid::new(id).num_columns(2).show(ui, |ui| {
                slider(ui, "Opens above floor", above_floor_db, 2.0..=24.0, " dB", 1);
                slider(ui, "Turns down by", range_db, 0.0..=40.0, " dB", 1);
                slider(ui, "Release", release_ms, 20.0..=600.0, " ms", 0);
            });
        }
        Fx::Eq { bands } => {
            c |= eq_curve(ui, bands, eq_drag);
            egui::Grid::new(id).num_columns(4).spacing([6.0, 3.0]).show(ui, |ui| {
                for (i, (b, name)) in bands.iter_mut().zip(["Low shelf", "Mud", "Presence", "Air shelf"]).enumerate() {
                    ui.label(theme::caption(name));
                    let speed = b.freq * 0.01;
                    c |= ui.add(egui::DragValue::new(&mut b.freq).range(20.0..=20_000.0).speed(speed).suffix(" Hz").fixed_decimals(0)).changed();
                    c |= ui.add(egui::DragValue::new(&mut b.gain_db).range(-18.0..=18.0).speed(0.1).suffix(" dB").fixed_decimals(1)).changed();
                    if i == 1 || i == 2 {
                        c |= ui.add(egui::DragValue::new(&mut b.q).range(0.2..=8.0).speed(0.02).prefix("Q ").fixed_decimals(2)).changed();
                    } else {
                        ui.label("");
                    }
                    ui.end_row();
                }
            });
        }
        Fx::DeEsser { freq, threshold_db, max_db } => {
            egui::Grid::new(id).num_columns(2).show(ui, |ui| {
                slider(ui, "Above", freq, 3000.0..=12_000.0, " Hz", 0);
                slider(ui, "Threshold", threshold_db, -60.0..=0.0, " dB", 1);
                slider(ui, "At most", max_db, 0.0..=18.0, " dB", 1);
            });
        }
        Fx::Compressor { threshold_db, ratio, attack_ms, release_ms } => {
            egui::Grid::new(id).num_columns(2).show(ui, |ui| {
                slider(ui, "Threshold", threshold_db, -50.0..=0.0, " dB", 1);
                slider(ui, "Ratio", ratio, 1.0..=12.0, ":1", 1);
                slider(ui, "Attack", attack_ms, 0.5..=80.0, " ms", 1);
                slider(ui, "Release", release_ms, 20.0..=800.0, " ms", 0);
            });
        }
    }
    c
}

/// The EQ's response, 20 Hz–20 kHz, ±18 dB, with a handle per band: drag to
/// move it (frequency and gain), scroll over it to change its width.
fn eq_curve(ui: &mut egui::Ui, bands: &mut [vg_audio::EqBand; 4], drag: &mut Option<usize>) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 110.0), egui::Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, theme::RULE), egui::StrokeKind::Inside);
    let (lo, hi) = (20f32.ln(), 20_000f32.ln());
    let x_of = |f: f32| rect.left() + (f.max(20.0).ln() - lo) / (hi - lo) * rect.width();
    let f_of = |x: f32| ((x - rect.left()) / rect.width() * (hi - lo) + lo).exp();
    let y_of = |db: f32| rect.center().y - db / 18.0 * (rect.height() / 2.0 - 6.0);
    let db_of = |y: f32| (rect.center().y - y) / (rect.height() / 2.0 - 6.0) * 18.0;
    for f in [100.0, 1000.0, 10_000.0] {
        let x = x_of(f);
        p.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0_f32, theme::RULE_SOFT));
        p.text(egui::pos2(x + 3.0, rect.bottom() - 3.0), egui::Align2::LEFT_BOTTOM, if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") }, egui::FontId::monospace(8.5), theme::MUTED);
    }
    for db in [-12.0, 0.0, 12.0] {
        let y = y_of(db);
        p.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], egui::Stroke::new(1.0_f32, if db == 0.0 { theme::RULE } else { theme::RULE_SOFT }));
    }
    let filters = vg_audio::render::eq_filters(bands);
    let pts: Vec<egui::Pos2> = (0..rect.width() as usize)
        .step_by(2)
        .map(|px| {
            let x = rect.left() + px as f32;
            let f = f_of(x);
            let db: f32 = filters.iter().map(|b| b.response_db(RATE as f32, f)).sum();
            egui::pos2(x, y_of(db.clamp(-18.0, 18.0)))
        })
        .collect();
    p.add(egui::Shape::line(pts, egui::Stroke::new(1.5_f32, theme::WHITE)));
    let handle = |b: &vg_audio::EqBand| egui::pos2(x_of(b.freq), y_of(b.gain_db));
    let mut changed = false;
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos() {
            *drag = bands.iter().enumerate().map(|(i, b)| (i, handle(b).distance(pos))).filter(|(_, d)| *d < 14.0).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i);
        }
    if let (Some(i), Some(pos)) = (*drag, resp.interact_pointer_pos())
        && resp.dragged() {
            bands[i].freq = f_of(pos.x).clamp(20.0, 20_000.0).round();
            bands[i].gain_db = (db_of(pos.y).clamp(-18.0, 18.0) * 2.0).round() / 2.0;
            changed = true;
        }
    if resp.drag_stopped() {
        *drag = None;
    }
    if let Some(pos) = resp.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if let Some(i) = [1usize, 2].into_iter().find(|&i| handle(&bands[i]).distance(pos) < 14.0)
            && scroll != 0.0 {
                bands[i].q = (bands[i].q * (1.0 + scroll * 0.003)).clamp(0.2, 8.0);
                changed = true;
            }
    }
    for (i, b) in bands.iter().enumerate() {
        let c = handle(b);
        let active = *drag == Some(i);
        p.circle_filled(c, if active { 5.0 } else { 4.0 }, theme::BLACK);
        p.circle_stroke(c, if active { 5.0 } else { 4.0 }, egui::Stroke::new(1.0_f32, theme::WHITE));
        p.text(c + egui::vec2(6.0, -6.0), egui::Align2::LEFT_BOTTOM, format!("{}", i + 1), egui::FontId::monospace(8.5), theme::MUTED);
    }
    changed
}

/// Write the mix, point the script at it and move the script's times with the cuts.
fn use_as_narration(app: &mut VgApp, ctx: &egui::Context) {
    let Some(out) = app.studio.out.clone().filter(|o| o.mix == app.studio.mix) else { return };
    let Some(wav) = app.studio.mix_wav() else { return };
    app.stop();
    app.studio.stop();
    if let Err(e) = vg_audio::write_wav(&wav, &out.r.wet) {
        app.error(format!("{e:#}"));
        return;
    }
    app.audio = None;
    app.player = None;
    let mut moved = 0usize;
    // The script's times move only when it's on the raw take or this mix.
    if let Some(clock) = clock(app) {
        let mapper = app.studio.mapper(clock);
        let before = app.cue_text.clone();
        match vg_core::cues::remap_times(&app.cue_text, |t| mapper.retime(t)) {
            Ok(text) => {
                moved = before.lines().zip(text.lines()).filter(|(a, b)| a != b).count();
                app.cue_text = text;
            }
            Err(e) => app.error(format!("script times not moved: {e}")),
        }
        // Word timings: move them, and drop words that were cut out.
        if let Some((wpath, _, words)) = app.words.clone()
            && !words.is_empty() {
                let old = |t: f64| match &mapper.synced {
                    Some(sy) => Track { offset: sy.offset, clips: sy.clips.clone(), ..Default::default() }.out_to_src(t),
                    None => Some(t),
                };
                let t0 = &app.studio.mix.tracks[0];
                let moved_words: Vec<vg_core::Word> = words
                    .iter()
                    .filter_map(|w| {
                        let (s, e) = (old(w.start)?, old(w.end)?);
                        let ns = t0.src_to_out(s).or_else(|| t0.src_to_out(e).map(|_| t0.src_to_out_snapped(s)))?;
                        Some(vg_core::Word { start: ns, end: t0.src_to_out_snapped(e).max(ns + 0.01), word: w.word.clone() })
                    })
                    .collect();
                let new_path = wav.with_extension("words.json");
                let mut tr = vg_core::Transcript::load(&wpath).unwrap_or_default();
                let text = moved_words.iter().map(|w| w.word.as_str()).collect::<Vec<_>>().join(" ");
                tr.segments = vec![vg_core::transcript::Segment { start: moved_words.first().map(|w| w.start).unwrap_or(0.0), end: moved_words.last().map(|w| w.end).unwrap_or(0.0), text, words: moved_words }];
                match tr.save(&new_path) {
                    Ok(()) => {
                        let rel = super::relative_path(&new_path, &app.cue_dir());
                        super::set_directive(&mut app.cue_text, "words", &rel.to_string_lossy());
                        app.words = None;
                        app.words_dirty = false;
                    }
                    Err(e) => app.error(format!("word timings not moved: {e:#}")),
                }
            }
    }
    app.studio.mix.mark_synced(0);
    // The file just written is this mix (syncing changes no sound).
    app.studio.written = Some(app.studio.mix.clone());
    if let Err(e) = app.studio.save() {
        app.error(format!("{e:#}"));
        return;
    }
    app.set_narration(&wav);
    app.studio.loaded_for = Some(wav.clone());
    let name = file_name(&wav);
    app.info(if moved > 0 { format!("wrote {name} and moved the times on {moved} script lines") } else { format!("wrote {name}; it's now the narration") });
    if app.studio.transcribe_after {
        super::transcribe::open_for(app, wav);
        super::transcribe::start(app, ctx);
    }
}
