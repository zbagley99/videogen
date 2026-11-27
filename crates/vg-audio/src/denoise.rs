//! Spectral noise reduction: learn the noise's spectrum (from the quietest
//! moments, or a stretch of room tone), then turn each frequency down where it
//! isn't clearly above that noise. The gain uses the decision-directed estimate
//! (Ephraim & Malah): the speech-to-noise ratio is tracked smoothly over time,
//! so what's left of the noise stays steady instead of warbling ("musical noise").

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

const N: usize = 2048;
const HOP: usize = N / 4;

fn window() -> Vec<f32> {
    // sqrt-Hann for analysis and synthesis: their product (Hann) overlap-adds to
    // a constant 2 at 75 % overlap.
    (0..N).map(|i| (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos()).sqrt()).collect()
}

/// Per-bin magnitude spectra of the given frames of `buf`.
fn spectra(buf: &[f32], frames: impl Iterator<Item = usize>) -> Vec<Vec<f32>> {
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N);
    let win = window();
    let mut input = vec![0.0f32; N];
    let mut out = fft.make_output_vec();
    frames
        .map(|f| {
            for (i, v) in input.iter_mut().enumerate() {
                *v = buf.get(f * HOP + i).copied().unwrap_or(0.0) * win[i];
            }
            fft.process(&mut input, &mut out).unwrap();
            out.iter().map(|c| c.norm()).collect()
        })
        .collect()
}

/// The noise spectrum: the mean magnitude of the quietest 10 % of frames (at
/// least a few), or of every frame when `buf` is a room-tone selection.
pub fn noise_profile(buf: &[f32], whole: bool) -> Vec<f32> {
    // Pick the frames by their energy (cheap, in the time domain), then take
    // spectra of just those.
    let frames = buf.len().div_ceil(HOP);
    let energy = |f: usize| buf[(f * HOP).min(buf.len())..(f * HOP + N).min(buf.len())].iter().map(|x| x * x).sum::<f32>();
    let mut order: Vec<(f32, usize)> = (0..frames).map(|f| (energy(f), f)).collect();
    // Frames of digital silence (a cut's crossfade, the file's padding) aren't noise.
    order.retain(|(e, _)| *e > 1e-12);
    order.sort_by(|a, b| a.0.total_cmp(&b.0));
    let take = if whole { order.len() } else { (order.len() / 10).max(4).min(order.len()) };
    let mut prof = vec![0.0f32; N / 2 + 1];
    for spec in spectra(buf, order[..take].iter().map(|&(_, f)| f)) {
        for (p, m) in prof.iter_mut().zip(&spec) {
            *p += m;
        }
    }
    prof.iter_mut().for_each(|p| *p /= take.max(1) as f32);
    prof
}

/// Reduce noise in `buf` by up to `reduction_db`, keeping bins more than
/// `threshold_db` above the `profile`.
pub fn denoise(buf: &mut [f32], profile: &[f32], reduction_db: f32, threshold_db: f32) {
    let n = buf.len();
    if n == 0 || profile.len() != N / 2 + 1 {
        return;
    }
    let mut planner = RealFftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(N);
    let inv = planner.plan_fft_inverse(N);
    let win = window();
    let floor = 10f32.powf(-reduction_db.max(0.0) / 20.0);
    // Noise power per bin: a Rayleigh magnitude's mean power is 4/π × its mean²,
    // raised by the threshold (6 dB ≈ ×2 over-estimate, a safe default).
    let over = 10f32.powf((threshold_db - 3.0) / 10.0);
    let bins = N / 2 + 1;
    let lambda: Vec<f32> = profile.iter().map(|p| (4.0 / std::f32::consts::PI * p * p * over).max(1e-20)).collect();
    // How much of the last frame's clean estimate carries into this one.
    const ALPHA: f32 = 0.98;
    let mut last_clean = vec![0.0f32; bins];

    // Pad so the first and last samples get full overlap.
    let pad = N;
    let mut src = vec![0.0f32; pad];
    src.extend_from_slice(buf);
    src.extend(std::iter::repeat_n(0.0, pad + N));
    let mut out = vec![0.0f32; src.len()];

    let mut frame = vec![0.0f32; N];
    let mut spec = fwd.make_output_vec();
    let mut g = vec![0.0f32; bins];
    let mut smooth = vec![0.0f32; bins];
    let frames = (src.len() - N) / HOP;
    for f in 0..frames {
        let at = f * HOP;
        for i in 0..N {
            frame[i] = src[at + i] * win[i];
        }
        fwd.process(&mut frame, &mut spec).unwrap();
        for k in 0..bins {
            let power = spec[k].norm_sqr();
            let post = power / lambda[k];
            let prio = ALPHA * last_clean[k] / lambda[k] + (1.0 - ALPHA) * (post - 1.0).max(0.0);
            let gain = prio / (1.0 + prio);
            last_clean[k] = gain * gain * power;
            g[k] = gain.max(floor);
        }
        // A little smoothing across neighbouring bins (wider up high).
        for k in 0..bins {
            let w = k / 256;
            let (a, b) = (k.saturating_sub(w), (k + w + 1).min(bins));
            smooth[k] = g[a..b].iter().sum::<f32>() / (b - a) as f32;
        }
        for k in 0..bins {
            spec[k] *= smooth[k];
        }
        spec[0] = Complex::new(spec[0].re, 0.0);
        spec[bins - 1] = Complex::new(spec[bins - 1].re, 0.0);
        inv.process(&mut spec, &mut frame).unwrap();
        for i in 0..N {
            // realfft's inverse is unnormalised (×N); the windows overlap-add to 2.
            out[at + i] += frame[i] * win[i] / (N as f32 * 2.0);
        }
    }
    buf.copy_from_slice(&out[pad..pad + n]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{gain_to_db, peak_db};

    /// Deterministic white-ish noise.
    fn noise(n: usize, amp: f32, seed: u32) -> Vec<f32> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                amp * ((x as f32 / u32::MAX as f32) * 2.0 - 1.0)
            })
            .collect()
    }

    fn rms_db(b: &[f32]) -> f32 {
        gain_to_db((b.iter().map(|x| x * x).sum::<f32>() / b.len() as f32).sqrt())
    }

    #[test]
    fn transparent_at_zero_reduction() {
        let rate = 48_000.0;
        let sig: Vec<f32> = (0..48_000).map(|i| 0.3 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate).sin()).collect();
        let mut out = sig.clone();
        denoise(&mut out, &vec![0.0; N / 2 + 1], 0.0, 6.0);
        let err: f32 = sig.iter().zip(&out).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(err < 1e-4, "{err}");
    }

    #[test]
    fn removes_hiss_and_keeps_the_tone() {
        let rate = 48_000.0;
        let n = 96_000;
        let hiss = noise(n, 0.02, 7);
        // Tone in the second half only; the first half is room tone.
        let mut buf: Vec<f32> = (0..n).map(|i| hiss[i] + if i >= n / 2 { 0.3 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate).sin() } else { 0.0 }).collect();
        let prof = noise_profile(&buf, false);
        let tone_before = peak_db(&buf[70_000..80_000]);
        denoise(&mut buf, &prof, 18.0, 6.0);
        let quiet = rms_db(&buf[10_000..40_000]);
        assert!(quiet < rms_db(&hiss[10_000..40_000]) - 14.0, "room tone only {quiet} dB");
        assert!((peak_db(&buf[70_000..80_000]) - tone_before).abs() < 1.0);
    }
}
