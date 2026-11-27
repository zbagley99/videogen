//! Rendering a [`Mix`]: decode the sources, lay out each track's clips with
//! short crossfades at the cuts, run its effects, sum, then master (loudness
//! and limiter). Also renders a level-matched "original" for fair A/B.

use crate::dsp;
use crate::edit::{Fx, LEVEL_LUFS, Mix, Music, RATE, Track};
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Decode any audio/video file's sound to mono f32 at [`RATE`] (with ffmpeg).
pub fn decode(path: &Path) -> Result<Vec<f32>> {
    vg_media::decode_mono(path, RATE)
}

/// Decoded sources and the output of every effect stage of the last render,
/// so a tweak only re-runs the effects from the one that changed.
#[derive(Default)]
pub struct Cache {
    sources: HashMap<PathBuf, (Option<std::time::SystemTime>, Arc<Vec<f32>>)>,
    stages: Mutex<HashMap<u64, Stage>>,
    used: Mutex<HashSet<u64>>,
}

/// A stage's output and the most compression applied up to it.
#[derive(Clone)]
struct Stage {
    buf: Arc<Vec<f32>>,
    gr: f32,
}

/// Stop caching stages past this much memory.
const CACHE_BYTES: usize = 1 << 30;

impl Cache {
    pub fn source(&mut self, path: &Path) -> Result<Arc<Vec<f32>>> {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some((m, buf)) = self.sources.get(path)
            && *m == mtime {
                return Ok(buf.clone());
            }
        let buf = Arc::new(decode(path)?);
        self.sources.insert(path.to_path_buf(), (mtime, buf.clone()));
        Ok(buf)
    }

    fn get(&self, key: u64) -> Option<Stage> {
        let hit = self.stages.lock().unwrap().get(&key).cloned();
        if hit.is_some() {
            self.used.lock().unwrap().insert(key);
        }
        hit
    }

    fn put(&self, key: u64, stage: Stage) {
        let mut stages = self.stages.lock().unwrap();
        let bytes: usize = stages.values().map(|s| s.buf.len() * 4).sum();
        if bytes + stage.buf.len() * 4 <= CACHE_BYTES {
            stages.insert(key, stage);
        }
        self.used.lock().unwrap().insert(key);
    }

    /// Memory held: decoded sources and effect stages.
    pub fn bytes(&self) -> usize {
        let src: usize = self.sources.values().map(|(_, b)| b.len() * 4).sum();
        src + self.stages.lock().unwrap().values().map(|s| s.buf.len() * 4).sum::<usize>()
    }

    /// Drop everything (the next render decodes and processes from scratch).
    pub fn clear(&mut self) {
        self.sources.clear();
        self.stages.lock().unwrap().clear();
        self.used.lock().unwrap().clear();
    }

    /// Forget stages the last render didn't use.
    fn sweep(&self) {
        let used = std::mem::take(&mut *self.used.lock().unwrap());
        self.stages.lock().unwrap().retain(|k, _| used.contains(k));
    }
}

fn key(parts: &[&dyn std::fmt::Debug]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in parts {
        format!("{p:?}").hash(&mut h);
    }
    h.finish()
}

const XFADE: f64 = 0.008;

/// A track's clips back to back, with an equal-power crossfade at each cut.
pub fn assemble(track: &Track, src: &[f32]) -> Vec<f32> {
    let r = RATE as f64;
    let idx = |t: f64| ((t * r).round().max(0.0) as usize).min(src.len());
    let h = ((XFADE * r) as usize / 2).max(1);
    let at = |i: isize| if i >= 0 { src.get(i as usize).copied().unwrap_or(0.0) } else { 0.0 };
    let mut out: Vec<f32> = Vec::with_capacity((track.length() * r) as usize + 1);
    let mut prev_end: Option<usize> = None;
    for clip in &track.clips {
        let (a, b) = (idx(clip.start), idx(clip.end));
        if b <= a {
            continue;
        }
        // The first clip, or one that carries straight on from the last (a
        // split): no crossfade.
        let Some(pe) = prev_end.filter(|&pe| pe != a) else {
            out.extend_from_slice(&src[a..b]);
            prev_end = Some(b);
            continue;
        };
        let f = 2 * h;
        let fi = |k: usize| (std::f32::consts::FRAC_PI_2 * k as f32 / f as f32).sin();
        let fo = |k: usize| (std::f32::consts::FRAC_PI_2 * k as f32 / f as f32).cos();
        // Before the join: the old clip fades out while the new one's lead-in fades in.
        let n = out.len();
        let hb = h.min(n);
        for k in 0..hb {
            let j = k + (h - hb);
            let o = n - hb + k;
            out[o] = out[o] * fo(j) + at(a as isize - (hb - k) as isize) * fi(j);
        }
        // After it: the new clip continues fading in over the old clip's tail.
        for (k, i) in (a..b).enumerate() {
            let v = if k < h { src[i] * fi(h + k) + at((pe + k) as isize) * fo(h + k) } else { src[i] };
            out.push(v);
        }
        prev_end = Some(b);
    }
    out
}

/// What a render measured.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Loudness of the raw voice (track 0) before anything, LUFS.
    pub input_lufs: Option<f32>,
    pub output_lufs: Option<f32>,
    pub output_peak_db: f32,
    /// Most compressor gain reduction on any track (dB).
    pub compression_db: f32,
    /// Gain the master applied to reach the loudness target (dB).
    pub master_gain_db: f32,
}

pub struct Rendered {
    /// The processed mix.
    pub wet: Arc<Vec<f32>>,
    /// The same cuts and mix without effects, matched to `wet`'s loudness.
    pub dry: Arc<Vec<f32>>,
    /// Each track's cut audio before effects (track-local time), for waveforms.
    pub tracks: Vec<Arc<Vec<f32>>>,
    /// Each track after its effects, before its fader (track-local time).
    pub tracks_wet: Vec<Arc<Vec<f32>>>,
    pub stats: Stats,
}

/// Run one effect in place. Returns the compressor's most gain reduction.
fn apply(fx: &Fx, track: &Track, buf: &mut [f32]) -> f32 {
    let rate = RATE as f32;
    // Every effect forgets within a second, so long tracks run in parallel pieces.
    let par = |buf: &mut [f32], f: &(dyn Fn(&mut [f32]) + Sync)| dsp::par_ctx(buf, rate, 1.0, f);
    match fx {
        Fx::HighPass { freq } => par(buf, &|b| dsp::highpass(b, rate, *freq)),
        Fx::Denoise { reduction_db, threshold_db, room_tone } => {
            let region = room_tone.and_then(|(a, b)| {
                let (a, b) = (track.src_to_out(a)? - track.offset, track.src_to_out(b)? - track.offset);
                let (a, b) = ((a * RATE as f64) as usize, ((b * RATE as f64) as usize).min(buf.len()));
                (b > a + 4096).then(|| buf[a..b].to_vec())
            });
            let profile = match &region {
                Some(r) => crate::denoise::noise_profile(r, true),
                None => crate::denoise::noise_profile(buf, false),
            };
            par(buf, &|b| crate::denoise::denoise(b, &profile, *reduction_db, *threshold_db));
        }
        Fx::Gate { above_floor_db, range_db, release_ms } => {
            let threshold = noise_floor_db(buf).map(|(floor, loud)| (floor + above_floor_db).min(loud - 15.0)).unwrap_or(-60.0);
            par(buf, &|b| dsp::gate(b, rate, threshold, *range_db, *release_ms))
        }
        Fx::Eq { bands } => par(buf, &|b| {
            for mut f in eq_filters(bands) {
                f.run(b);
            }
        }),
        Fx::DeEsser { freq, threshold_db, max_db } => par(buf, &|b| dsp::deess(b, rate, *freq, *threshold_db, *max_db)),
        Fx::Compressor { threshold_db, ratio, attack_ms, release_ms } => {
            let most = Mutex::new(0.0f32);
            par(buf, &|b| {
                let gr = dsp::compress(b, rate, *threshold_db, *ratio, *attack_ms, *release_ms);
                let mut m = most.lock().unwrap();
                *m = m.max(gr);
            });
            return most.into_inner().unwrap();
        }
    }
    0.0
}

/// A track's cut audio, its processed audio (before the fader) and the most
/// gain reduction applied.
type Chain = (Arc<Vec<f32>>, Arc<Vec<f32>>, f32);

/// A track's `Chain`, reusing cached stages. `None` if cancelled.
fn track_chain(track: &Track, src: &[f32], src_key: u64, cache: &Cache, cancel: &dyn Fn() -> bool) -> Option<Chain> {
    let debug = std::env::var_os("VG_DEBUG").is_some();
    let cut_key = key(&[&src_key, &track.clips]);
    let cut = match cache.get(cut_key) {
        Some(s) => s.buf,
        None => {
            let b = Arc::new(assemble(track, src));
            cache.put(cut_key, Stage { buf: b.clone(), gr: 0.0 });
            b
        }
    };
    let mut k = key(&[&cut_key, &track.level]);
    let mut stage = match cache.get(k) {
        Some(s) => s,
        None => {
            let mut b = (*cut).clone();
            if track.level {
                dsp::normalize_loudness(&mut b, RATE as f32, LEVEL_LUFS);
            }
            let s = Stage { buf: Arc::new(b), gr: 0.0 };
            cache.put(k, s.clone());
            s
        }
    };
    for e in track.effects.iter().filter(|e| e.on) {
        k = key(&[&k, &e.fx]);
        if let Some(s) = cache.get(k) {
            stage = s;
            continue;
        }
        if cancel() {
            return None;
        }
        let t0 = std::time::Instant::now();
        let mut b = (*stage.buf).clone();
        let gr = apply(&e.fx, track, &mut b).max(stage.gr);
        if debug {
            eprintln!("  {:<16} {:>6.1} ms", e.fx.name(), t0.elapsed().as_secs_f64() * 1000.0);
        }
        stage = Stage { buf: Arc::new(b), gr };
        cache.put(k, stage.clone());
    }
    Some((cut, stage.buf, stage.gr))
}

/// A music track fitted to `len` samples: looped (with a short crossfade at
/// each seam) or cut, faded in at the start and out at the end.
pub fn music_bed(src: &[f32], len: usize, m: &Music, rate: f32) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::with_capacity(len);
    if src.is_empty() || len == 0 {
        return vec![0.0; len];
    }
    let xf = ((0.05 * rate) as usize).min(src.len() / 4);
    out.extend_from_slice(&src[..src.len().min(len)]);
    while m.loop_it && out.len() < len {
        let n = out.len();
        for k in 0..xf {
            let x = (k as f32 + 0.5) / xf as f32;
            let (fi, fo) = ((std::f32::consts::FRAC_PI_2 * x).sin(), (std::f32::consts::FRAC_PI_2 * x).cos());
            out[n - xf + k] = out[n - xf + k] * fo + src[k] * fi;
        }
        out.extend_from_slice(&src[xf..src.len().min(xf + len - n)]);
    }
    let end = out.len();
    out.resize(len, 0.0);
    let fin = ((m.fade_in * rate as f64) as usize).min(end);
    for (k, v) in out[..fin].iter_mut().enumerate() {
        *v *= (std::f32::consts::FRAC_PI_2 * k as f32 / fin as f32).sin();
    }
    let fout = ((m.fade_out * rate as f64) as usize).min(end);
    for k in 0..fout {
        out[end - fout + k] *= (std::f32::consts::FRAC_PI_2 * (k + 1) as f32 / fout as f32).cos();
    }
    out
}

/// Dip `bed` by `duck_db` wherever `key` (the voice) is talking, starting a
/// little before each phrase and letting go slowly after it.
pub fn duck(bed: &mut [f32], key: &[f32], duck_db: f32, rate: f32) {
    const FRAME: f32 = 0.01;
    if duck_db <= 0.0 || bed.is_empty() {
        return;
    }
    let n = (FRAME * rate) as usize;
    let levels = dsp::rms_frames(key, rate, FRAME);
    let threshold = noise_floor_db(key).map(|(floor, loud)| (floor + 10.0).min(loud - 12.0)).unwrap_or(-50.0);
    let active: Vec<bool> = levels.iter().map(|d| *d > threshold).collect();
    let (look, att, rel) = (15usize, 1.0 - (-FRAME / 0.08f32).exp(), 1.0 - (-FRAME / 0.6f32).exp());
    let frames = bed.len().div_ceil(n);
    let mut g = 0.0f32;
    let mut prev = 0.0f32;
    for f in 0..frames {
        let talking = (f..(f + look).min(active.len())).any(|j| active[j]);
        let target = if talking { -duck_db } else { 0.0 };
        g += (target - g) * if target < g { att } else { rel };
        let (a, b) = (f * n, ((f + 1) * n).min(bed.len()));
        for (k, v) in bed[a..b].iter_mut().enumerate() {
            let db = prev + (g - prev) * k as f32 / n as f32;
            *v *= dsp::db_to_gain(db);
        }
        prev = g;
    }
}

/// The noise floor and speech level (dBFS RMS of 20 ms frames). Speech is the
/// 95th percentile; the floor is the median of the frames nearer the quietest
/// than the speech (the pauses), ignoring digital silence.
pub fn noise_floor_db(buf: &[f32]) -> Option<(f32, f32)> {
    let mut frames: Vec<f32> = dsp::rms_frames(buf, RATE as f32, 0.02).into_iter().filter(|d| *d > -115.0).collect();
    if frames.len() < 10 {
        return None;
    }
    frames.sort_by(f32::total_cmp);
    let (quietest, loud) = (frames[frames.len() / 20], frames[frames.len() * 95 / 100]);
    let mid = (quietest + loud) / 2.0;
    let quiet: Vec<f32> = frames.iter().copied().take_while(|d| *d < mid).collect();
    Some((quiet[quiet.len() / 2], loud))
}

/// The EQ's four filters (bands at 0 dB are skipped).
pub fn eq_filters(bands: &[crate::edit::EqBand; 4]) -> Vec<dsp::Biquad> {
    let rate = RATE as f32;
    bands
        .iter()
        .enumerate()
        .filter(|(_, b)| b.gain_db.abs() > 0.01)
        .map(|(i, b)| match i {
            0 => dsp::Biquad::low_shelf(rate, b.freq, b.gain_db),
            3 => dsp::Biquad::high_shelf(rate, b.freq, b.gain_db),
            _ => dsp::Biquad::peaking(rate, b.freq, b.q.max(0.1), b.gain_db),
        })
        .collect()
}

/// Render `mix`. Source paths are resolved against `base`. Returns `None` if
/// `cancel` turned true part-way.
pub fn render(mix: &Mix, base: &Path, cache: &mut Cache, cancel: &(dyn Fn() -> bool + Sync)) -> Result<Option<Rendered>> {
    let rate = RATE as f32;
    let len = (mix.length() * RATE as f64).ceil() as usize;
    let debug = std::env::var_os("VG_DEBUG").is_some();
    let t0 = std::time::Instant::now();
    let lap = |what: &str| {
        if debug {
            eprintln!("{what:<18} {:>7.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
        }
    };
    let mut srcs = vec![];
    for track in &mix.tracks {
        let path = if track.source.is_absolute() { track.source.clone() } else { base.join(&track.source) };
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        srcs.push((cache.source(&path)?, key(&[&path, &mtime])));
    }
    lap("decoded");
    let cache: &Cache = cache;
    // Tracks in parallel.
    let chains: Vec<Option<Chain>> = std::thread::scope(|s| {
        let handles: Vec<_> = mix.tracks.iter().zip(&srcs).map(|(t, (src, k))| s.spawn(move || track_chain(t, src, *k, cache, cancel))).collect();
        handles.into_iter().map(|h| h.join().unwrap_or(None)).collect()
    });
    if chains.iter().any(Option::is_none) || cancel() {
        return Ok(None);
    }
    cache.sweep();
    lap("tracks");
    let chains: Vec<_> = chains.into_iter().flatten().collect();
    let mut stats = Stats { input_lufs: chains.first().and_then(|c| dsp::loudness(&c.0, rate)), ..Default::default() };
    // Adjustments (mute, gain, fades) come after the effects: onto the processed
    // tracks (so the waveforms show them) and into the A/B original.
    let envs: Vec<Option<Vec<f32>>> = mix.tracks.iter().zip(&chains).map(|(t, c)| t.envelope(c.1.len(), RATE as f64)).collect();
    let mut chains: Vec<Chain> = chains
        .into_iter()
        .zip(&envs)
        .map(|((cut, processed, gr), env)| match env {
            Some(env) => (cut, Arc::new(processed.iter().zip(env).map(|(s, g)| s * g).collect()), gr),
            None => (cut, processed, gr),
        })
        .collect();
    let mut wet = vec![0.0f32; len];
    let mut dry = vec![0.0f32; len];
    // The voice first; then music, ducked under it.
    for (i, (track, (cut, processed, gr))) in mix.tracks.iter().zip(&chains).enumerate() {
        stats.compression_db = stats.compression_db.max(*gr);
        if !mix.audible(i) || track.music.is_some() {
            continue;
        }
        let env = envs[i].as_deref();
        let at = (track.offset * RATE as f64).round() as usize;
        let g = dsp::db_to_gain(track.gain_db);
        // The A/B original: the same level stage and fader, no effects.
        let lvl = if track.level { dsp::loudness(cut, rate).map(|l| dsp::db_to_gain((LEVEL_LUFS - l).clamp(-40.0, 40.0))).unwrap_or(1.0) } else { 1.0 };
        for (k, (w, d)) in processed.iter().zip(cut.iter()).enumerate() {
            if let Some(o) = wet.get_mut(at + k) {
                *o += w * g;
                dry[at + k] += d * lvl * g * env.and_then(|e| e.get(k)).copied().unwrap_or(1.0);
            }
        }
    }
    let has_music = mix.tracks.iter().enumerate().any(|(i, t)| t.music.is_some() && mix.audible(i));
    let key = has_music.then(|| wet.clone());
    for (i, track) in mix.tracks.iter().enumerate() {
        let Some(music) = &track.music else { continue };
        let at = ((track.offset * RATE as f64).round() as usize).min(len);
        let mut bed = music_bed(&chains[i].1, len - at, music, rate);
        if let Some(key) = &key {
            duck(&mut bed, &key[at..], music.duck_db, rate);
        }
        if mix.audible(i) {
            let g = dsp::db_to_gain(track.gain_db);
            for (k, v) in bed.iter().enumerate() {
                wet[at + k] += v * g;
                dry[at + k] += v * g;
            }
        }
        // Show it as it plays: looped, faded and ducked.
        chains[i].1 = Arc::new(bed);
    }
    lap("summed");
    let m = &mix.master;
    let dry = std::thread::scope(|s| {
        let d = s.spawn(move || {
            // Match the original to the target too, so A/B compares sound, not volume.
            if let Some(t) = m.loudness {
                dsp::normalize_loudness(&mut dry, rate, t);
            }
            dsp::limit(&mut dry, rate, m.ceiling_db, 80.0);
            dry
        });
        master(&mut wet, m, &mut stats);
        d.join().unwrap()
    });
    lap("mastered");
    stats.output_lufs = dsp::loudness(&wet, rate);
    stats.output_peak_db = dsp::peak_db(&wet);
    lap("measured");
    let (tracks, tracks_wet) = chains.into_iter().map(|(c, p, _)| (c, p)).unzip();
    Ok(Some(Rendered { wet: Arc::new(wet), dry: Arc::new(dry), tracks, tracks_wet, stats }))
}

/// Loudness target, then the limiter. Limiting takes some loudness away, so
/// the gain is refined until the limited result lands on the target.
fn master(buf: &mut Vec<f32>, m: &crate::edit::Master, stats: &mut Stats) {
    let rate = RATE as f32;
    let Some(target) = m.loudness else {
        if m.limiter {
            dsp::limit(buf, rate, m.ceiling_db, 80.0);
        }
        return;
    };
    let Some(start) = dsp::loudness(buf, rate) else { return };
    let mut gain = (target - start).clamp(-40.0, 40.0);
    let mut out = buf.clone();
    // The limited loudness rises more slowly than the gain once the limiter
    // works hard, so step by the measured slope (secant), not 1 dB per dB.
    let mut prev: Option<(f32, f32)> = None;
    for _ in 0..8 {
        let g = dsp::db_to_gain(gain);
        out.iter_mut().zip(buf.iter()).for_each(|(o, s)| *o = s * g);
        if !m.limiter {
            break;
        }
        dsp::limit(&mut out, rate, m.ceiling_db, 80.0);
        let Some(l) = dsp::loudness(&out, rate) else { break };
        if (l - target).abs() < 0.1 {
            break;
        }
        let slope = match prev {
            Some((g0, l0)) if (gain - g0).abs() > 1e-3 => ((l - l0) / (gain - g0)).clamp(0.1, 1.0),
            _ => 1.0,
        };
        prev = Some((gain, l));
        // Don't chase a target the ceiling can't allow (+18 dB past the start).
        gain = (gain + (target - l) / slope).min(target - start + 18.0);
    }
    stats.master_gain_db = gain;
    *buf = out;
}

/// Write mono 24-bit PCM.
pub fn write_wav(path: &Path, buf: &[f32]) -> Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: RATE, bits_per_sample: 24, sample_format: hound::SampleFormat::Int };
    let tmp = path.with_extension("wav.part");
    {
        let mut w = hound::WavWriter::create(&tmp, spec).with_context(|| format!("creating {}", tmp.display()))?;
        let full = (1 << 23) as f32 - 1.0;
        for &s in buf {
            w.write_sample((s.clamp(-1.0, 1.0) * full).round() as i32)?;
        }
        w.finalize()?;
    }
    // Replace atomically so a player reading the old file never sees half a new one.
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{Clip, Mix};

    #[test]
    fn music_loops_fades_and_ducks() {
        let rate = 1000.0;
        let src: Vec<f32> = vec![0.5; 400];
        let m = Music { duck_db: 12.0, loop_it: true, fade_in: 0.1, fade_out: 0.2 };
        let bed = music_bed(&src, 1000, &m, rate);
        assert_eq!(bed.len(), 1000);
        assert!(bed[0] < 0.01 && (bed[500] - 0.5).abs() < 0.02 && bed[999] < 0.02, "{} {} {}", bed[0], bed[500], bed[999]);
        let once = music_bed(&src, 1000, &Music { loop_it: false, ..m }, rate);
        assert_eq!(once[600], 0.0);
        // Voice in the middle third: the bed dips there, not before or long after.
        let mut key = vec![0.001f32; 1000];
        for (i, k) in key.iter_mut().enumerate().skip(400).take(300) {
            *k = if i % 2 == 0 { 0.3 } else { -0.3 };
        }
        let mut b = vec![1.0f32; 1000];
        duck(&mut b, &key, 12.0, rate);
        assert!(b[100] > 0.98, "{}", b[100]);
        assert!(b[600] < 0.3, "{}", b[600]);
    }

    #[test]
    fn assemble_keeps_length_and_crossfades() {
        let src: Vec<f32> = vec![1.0; 48_000];
        let mut t = Track::new("v", "v.wav".into(), 1.0);
        t.cut(0.25, 0.5);
        let out = assemble(&t, &src);
        assert_eq!(out.len(), 36_000);
        // A constant through an equal-power crossfade dips no lower than cos(π/4)+sin(π/4) ≥ 1.
        assert!(out.iter().all(|v| *v >= 0.999 && *v <= 1.4143), "{:?}", out.iter().fold((9f32, 0f32), |(a, b), v| (a.min(*v), b.max(*v))));
        t.clips = vec![Clip { start: 0.0, end: 0.1 }];
        assert_eq!(assemble(&t, &src).len(), 4800);
        // A split plays straight through: no crossfade bump.
        let ramp: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.0).collect();
        let mut s = Track::new("v", "v.wav".into(), 1.0);
        s.split(0.5);
        assert_eq!(assemble(&s, &ramp), ramp);
    }

    #[test]
    fn renders_a_mix_to_the_target() {
        let dir = std::env::temp_dir().join(format!("vg-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("voice.wav");
        // 3 s of a "voice": a buzzy 150 Hz tone that comes and goes, over hiss.
        let buf: Vec<f32> = (0..3 * 48_000)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                let on = if ((t * 2.0) as u32).is_multiple_of(2) { 1.0 } else { 0.0 };
                on * 0.05 * ((2.0 * std::f32::consts::PI * 150.0 * t).sin() + 0.5 * (2.0 * std::f32::consts::PI * 450.0 * t).sin()) + 0.001 * ((i * 7919 % 997) as f32 / 498.0 - 1.0)
            })
            .collect();
        write_wav(&wav, &buf).unwrap();
        let mut mix = Mix::for_recording("voice.wav".into(), 3.0);
        mix.tracks[0].cut(1.0, 1.5);
        let r = render(&mix, &dir, &mut Cache::default(), &|| false).unwrap().unwrap();
        assert_eq!(r.wet.len(), (2.5 * 48_000.0) as usize);
        assert!((r.stats.output_lufs.unwrap() + 14.0).abs() < 0.15, "{:?}", r.stats);
        assert!(r.stats.output_peak_db <= -1.0 + 1e-3);
        let dry_l = dsp::loudness(&r.dry, 48_000.0).unwrap();
        assert!((dry_l - r.stats.output_lufs.unwrap()).abs() < 0.6, "dry {dry_l}");
        let out = dir.join("mix.wav");
        write_wav(&out, &r.wet).unwrap();
        assert_eq!(hound::WavReader::open(&out).unwrap().duration(), r.wet.len() as u32);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
