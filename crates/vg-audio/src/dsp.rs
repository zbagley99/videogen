//! Offline DSP building blocks. Everything works on whole mono buffers: the
//! narration is rendered in one go, so effects can look ahead freely.

use std::f64::consts::PI;

/// A second-order IIR filter (RBJ cookbook), run in f64 so low cut-offs stay stable.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn new(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0, z1: 0.0, z2: 0.0 }
    }

    fn w(rate: f32, f: f32) -> (f64, f64) {
        let w0 = 2.0 * PI * (f as f64).clamp(1.0, rate as f64 * 0.49) / rate as f64;
        (w0.cos(), w0.sin())
    }

    pub fn highpass(rate: f32, f: f32, q: f32) -> Self {
        let (c, s) = Self::w(rate, f);
        let alpha = s / (2.0 * q as f64);
        Self::new((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    pub fn lowpass(rate: f32, f: f32, q: f32) -> Self {
        let (c, s) = Self::w(rate, f);
        let alpha = s / (2.0 * q as f64);
        Self::new((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    pub fn peaking(rate: f32, f: f32, q: f32, gain_db: f32) -> Self {
        let (c, s) = Self::w(rate, f);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let alpha = s / (2.0 * q as f64);
        Self::new(1.0 + alpha * a, -2.0 * c, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * c, 1.0 - alpha / a)
    }

    /// Shelves with slope 1 (the cookbook's S = 1).
    pub fn low_shelf(rate: f32, f: f32, gain_db: f32) -> Self {
        let (c, s) = Self::w(rate, f);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let alpha = s / 2.0 * 2f64.sqrt();
        let sa = 2.0 * a.sqrt() * alpha;
        Self::new(
            a * ((a + 1.0) - (a - 1.0) * c + sa),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - sa),
            (a + 1.0) + (a - 1.0) * c + sa,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - sa,
        )
    }

    pub fn high_shelf(rate: f32, f: f32, gain_db: f32) -> Self {
        let (c, s) = Self::w(rate, f);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let alpha = s / 2.0 * 2f64.sqrt();
        let sa = 2.0 * a.sqrt() * alpha;
        Self::new(
            a * ((a + 1.0) + (a - 1.0) * c + sa),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - sa),
            (a + 1.0) - (a - 1.0) * c + sa,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - sa,
        )
    }

    /// The BS.1770 K-weighting pre-filter (a high shelf around 1.7 kHz).
    fn k_shelf(rate: f32) -> Self {
        let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
        let k = (PI * f0 / rate as f64).tan();
        let vh = 10f64.powf(g / 20.0);
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / q + k * k;
        Self::new(vh + vb * k / q + k * k, 2.0 * (k * k - vh), vh - vb * k / q + k * k, a0, 2.0 * (k * k - 1.0), 1.0 - k / q + k * k)
    }

    /// The BS.1770 K-weighting high-pass (RLB, ~38 Hz).
    fn k_highpass(rate: f32) -> Self {
        let (f0, q) = (38.13547087602444, 0.5003270373238773);
        let k = (PI * f0 / rate as f64).tan();
        Self::new(1.0, -2.0, 1.0, 1.0 + k / q + k * k, 2.0 * (k * k - 1.0), 1.0 - k / q + k * k)
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y as f32
    }

    pub fn run(&mut self, buf: &mut [f32]) {
        for s in buf {
            *s = self.process(*s);
        }
    }

    /// Magnitude response in dB at `f` (for drawing EQ curves).
    pub fn response_db(&self, rate: f32, f: f32) -> f32 {
        let w = 2.0 * PI * f as f64 / rate as f64;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let (nr, ni) = (self.b0 + self.b1 * c1 + self.b2 * c2, -(self.b1 * s1 + self.b2 * s2));
        let (dr, di) = (1.0 + self.a1 * c1 + self.a2 * c2, -(self.a1 * s1 + self.a2 * s2));
        (10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10()) as f32
    }
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn gain_to_db(g: f32) -> f32 {
    20.0 * g.max(1e-9).log10()
}

/// log2 to about ±0.005 (±0.015 dB), for per-sample level detection.
#[inline]
pub fn fast_log2(x: f32) -> f32 {
    let bits = x.max(1e-30).to_bits();
    let e = ((bits >> 23) & 0xff) as f32 - 128.0; // the polynomial gives 1 + log2(m)
    let m = f32::from_bits((bits & 0x007f_ffff) | 0x3f80_0000);
    e + (-0.344_848_43 * m + 2.024_665_8) * m - 0.674_877_6
}

/// 2^x to about 1e-4 relative.
#[inline]
pub fn fast_exp2(x: f32) -> f32 {
    let x = x.clamp(-126.0, 126.0);
    let xi = x.floor();
    let f = x - xi;
    let p = 1.0 + f * (0.696_065_6 + f * (0.224_463_5 + f * 0.079_440_2));
    f32::from_bits(((xi as i32 + 127) as u32) << 23) * p
}

/// 10·log10 of a power (mean square), fast.
#[inline]
fn power_db(ms: f32) -> f32 {
    3.010_3 * fast_log2(ms)
}

/// Gain for a level change in dB, fast.
#[inline]
fn fast_gain(db: f32) -> f32 {
    fast_exp2(db * 0.166_096_4)
}

/// One-pole smoothing coefficient for a time constant in ms.
fn coef(rate: f32, ms: f32) -> f32 {
    if ms <= 0.0 { 0.0 } else { (-1.0 / (rate * ms / 1000.0)).exp() }
}

pub fn peak_db(buf: &[f32]) -> f32 {
    gain_to_db(buf.iter().fold(0.0f32, |m, s| m.max(s.abs())))
}

/// Integrated loudness in LUFS (ITU-R BS.1770-4 / EBU R128), or `None` for silence.
pub fn loudness(buf: &[f32], rate: f32) -> Option<f32> {
    let (hs0, hp0) = (Biquad::k_shelf(rate), Biquad::k_highpass(rate));
    let block = (0.4 * rate) as usize;
    let step = (0.1 * rate) as usize;
    if buf.len() < block || step == 0 {
        return None;
    }
    // Mean square per 100 ms step, so each 400 ms block is four steps. Long
    // buffers are filtered in parallel pieces, each warmed up on the 0.5 s
    // before it (the K-weighting filters forget far faster than that).
    let steps: Vec<std::ops::Range<usize>> = (0..buf.len()).step_by(step).map(|a| a..(a + step).min(buf.len())).collect();
    let warm = (0.5 * rate) as usize;
    let parts = threads().min(steps.len().div_ceil(50)).max(1);
    let per = steps.len().div_ceil(parts);
    let squares: Vec<f64> = std::thread::scope(|sc| {
        let jobs: Vec<_> = steps
            .chunks(per)
            .map(|group| {
                sc.spawn(move || {
                    let (mut hs, mut hp) = (hs0, hp0);
                    let from = group[0].start;
                    for &x in &buf[from.saturating_sub(warm)..from] {
                        hp.process(hs.process(x));
                    }
                    group
                        .iter()
                        .map(|r| {
                            buf[r.clone()]
                                .iter()
                                .map(|&x| {
                                    let y = hp.process(hs.process(x)) as f64;
                                    y * y
                                })
                                .sum::<f64>()
                                / r.len() as f64
                        })
                        .collect::<Vec<f64>>()
                })
            })
            .collect();
        jobs.into_iter().flat_map(|j| j.join().unwrap()).collect()
    });
    let blocks: Vec<f64> = squares.windows(4).map(|w| w.iter().sum::<f64>() / 4.0).collect();
    let lufs = |z: f64| -0.691 + 10.0 * z.log10();
    let gated: Vec<f64> = blocks.iter().copied().filter(|&z| z > 0.0 && lufs(z) > -70.0).collect();
    if gated.is_empty() {
        return None;
    }
    let rel = lufs(gated.iter().sum::<f64>() / gated.len() as f64) - 10.0;
    let kept: Vec<f64> = gated.into_iter().filter(|&z| lufs(z) > rel).collect();
    Some(lufs(kept.iter().sum::<f64>() / kept.len() as f64) as f32)
}

/// Scale `buf` so its integrated loudness is `target` LUFS; returns the gain in dB.
pub fn normalize_loudness(buf: &mut [f32], rate: f32, target: f32) -> f32 {
    let Some(l) = loudness(buf, rate) else { return 0.0 };
    let db = (target - l).clamp(-40.0, 40.0);
    let g = db_to_gain(db);
    buf.iter_mut().for_each(|s| *s *= g);
    db
}

/// Butterworth high-pass, 24 dB/oct (two cascaded sections).
pub fn highpass(buf: &mut [f32], rate: f32, freq: f32) {
    Biquad::highpass(rate, freq, 0.541_196_1).run(buf);
    Biquad::highpass(rate, freq, 1.306_563).run(buf);
}

/// Feed-forward compressor with a soft knee. Returns the most gain reduction (dB).
pub fn compress(buf: &mut [f32], rate: f32, threshold_db: f32, ratio: f32, attack_ms: f32, release_ms: f32) -> f32 {
    let (ca, cr) = (coef(rate, attack_ms), coef(rate, release_ms));
    let knee = 6.0f32;
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    // Detector: a fast RMS-ish level so single peaks don't pump.
    let mut ms = 0.0f32;
    let cd = coef(rate, 5.0);
    let mut gr = 0.0f32;
    let mut most = 0.0f32;
    for s in buf.iter_mut() {
        ms = cd * ms + (1.0 - cd) * *s * *s;
        let over = power_db(ms.max(1e-12)) - threshold_db;
        let target = if over <= -knee / 2.0 {
            0.0
        } else if over >= knee / 2.0 {
            over * slope
        } else {
            slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
        };
        let c = if target > gr { ca } else { cr };
        gr = c * gr + (1.0 - c) * target;
        most = most.max(gr);
        *s *= fast_gain(-gr);
    }
    most
}

/// Downward expander / gate: below `threshold_db` the signal is turned down by
/// up to `range_db`, opening fast and closing over `release_ms` after a short hold.
pub fn gate(buf: &mut [f32], rate: f32, threshold_db: f32, range_db: f32, release_ms: f32) {
    // An RMS detector (20 ms), steady on noise so it doesn't flutter open, run
    // 10 ms ahead of the audio so it's open before a word starts.
    let ce = coef(rate, 20.0);
    let look = (0.01 * rate) as usize;
    let mut level = vec![0.0f32; buf.len()];
    let mut ms = 0.0f32;
    for (i, s) in buf.iter().enumerate() {
        ms = ce * ms + (1.0 - ce) * s * s;
        level[i.saturating_sub(look)] = ms;
    }
    let threshold = 10f32.powf(threshold_db / 10.0);
    for i in buf.len().saturating_sub(look)..buf.len() {
        level[i] = level[buf.len().saturating_sub(look + 1)];
    }
    let (open, close) = (coef(rate, 1.0), coef(rate, release_ms));
    let hold = (0.08 * rate) as usize;
    let mut g = 0.0f32; // dB of reduction currently applied
    let mut held = 0usize;
    for (s, l) in buf.iter_mut().zip(level) {
        let above = l > threshold;
        if above {
            held = hold;
        } else {
            held = held.saturating_sub(1);
        }
        let target = if above || held > 0 { 0.0 } else { range_db };
        let c = if target < g { open } else { close };
        g = c * g + (1.0 - c) * target;
        if g > 1e-3 {
            *s *= fast_gain(-g);
        }
    }
}

/// Split-band de-esser: only the band above `freq` is turned down, by up to
/// `max_db`, when it gets louder than `threshold_db`.
pub fn deess(buf: &mut [f32], rate: f32, freq: f32, threshold_db: f32, max_db: f32) {
    // Complementary split: high = x - lowpass(x), so low + high is exactly x and
    // turning `high` down can only take energy away (a separate high-pass would
    // be phase-shifted against x and could add some back).
    let mut low: Vec<f32> = buf.to_vec();
    Biquad::lowpass(rate, freq, 0.541_196_1).run(&mut low);
    Biquad::lowpass(rate, freq, 1.306_563).run(&mut low);
    let high: Vec<f32> = buf.iter().zip(&low).map(|(x, l)| x - l).collect();
    let (ca, cr) = (coef(rate, 0.5), coef(rate, 40.0));
    let cd = coef(rate, 3.0);
    let mut ms = 0.0f32;
    let mut gr = 0.0f32;
    for (s, h) in buf.iter_mut().zip(&high) {
        ms = cd * ms + (1.0 - cd) * h * h;
        let target = ((power_db(ms.max(1e-12)) - threshold_db) * 0.75).clamp(0.0, max_db);
        let c = if target > gr { ca } else { cr };
        gr = c * gr + (1.0 - c) * target;
        // x = low + high, so turning the high band down is x - (1 - g)·high.
        if gr > 1e-3 {
            *s -= (1.0 - fast_gain(-gr)) * h;
        }
    }
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16)
}

/// Run `f` over `buf` in parallel pieces, each given `ctx_secs` of the audio on
/// both sides to settle on (and look ahead into). Exact for effects whose state
/// forgets within that time: filters, envelopes, the limiter.
pub fn par_ctx(buf: &mut [f32], rate: f32, ctx_secs: f32, f: impl Fn(&mut [f32]) + Sync) {
    let ctx = (ctx_secs * rate) as usize;
    let parts = threads().min(buf.len() / (ctx * 8).max(1)).max(1);
    if parts == 1 {
        return f(buf);
    }
    let n = buf.len();
    let per = n.div_ceil(parts);
    let src: &[f32] = buf;
    let f = &f;
    let outs: Vec<(usize, Vec<f32>)> = std::thread::scope(|sc| {
        let jobs: Vec<_> = (0..parts)
            .map(|p| {
                sc.spawn(move || {
                    let (a, b) = (p * per, ((p + 1) * per).min(n));
                    let (wa, wb) = (a.saturating_sub(ctx), (b + ctx).min(n));
                    let mut piece = src[wa..wb].to_vec();
                    f(&mut piece);
                    (a, piece[a - wa..b - wa].to_vec())
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().unwrap()).collect()
    });
    for (a, piece) in outs {
        buf[a..a + piece.len()].copy_from_slice(&piece);
    }
}

/// Look-ahead brickwall limiter: no sample exceeds `ceiling_db`. Offline, so the
/// gain can start ramping down before a peak arrives.
pub fn limit(buf: &mut [f32], rate: f32, ceiling_db: f32, release_ms: f32) {
    par_ctx(buf, rate, 1.0, |b| limit_serial(b, rate, ceiling_db, release_ms));
}

fn limit_serial(buf: &mut [f32], rate: f32, ceiling_db: f32, release_ms: f32) {
    let ceil = db_to_gain(ceiling_db);
    let n = buf.len();
    if n == 0 {
        return;
    }
    let look = ((0.003 * rate) as usize).max(1);
    let req: Vec<f32> = buf.iter().map(|s| if s.abs() > ceil { ceil / s.abs() } else { 1.0 }).collect();
    // Sliding minimum over ±look (monotonic deque).
    let mut minf = vec![1.0f32; n];
    let mut dq: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for j in 0..n + look {
        if j < n {
            while dq.back().is_some_and(|&b| req[b] >= req[j]) {
                dq.pop_back();
            }
            dq.push_back(j);
        }
        if j >= look {
            let i = j - look;
            while dq.front().is_some_and(|&f| f + look < i) {
                dq.pop_front();
            }
            minf[i] = dq.front().map(|&f| req[f]).unwrap_or(1.0);
        }
    }
    // Box average over ±look/2 of values that are each ≤ req within ±look keeps
    // the peak under the ceiling while turning the step into a ramp.
    let half = look / 2;
    let mut prefix = vec![0.0f64; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + minf[i] as f64;
    }
    let cr = coef(rate, release_ms);
    let mut g = 1.0f32;
    for i in 0..n {
        let (a, b) = (i.saturating_sub(half), (i + half + 1).min(n));
        let smooth = ((prefix[b] - prefix[a]) / (b - a) as f64) as f32;
        let target = smooth.min(minf[i]);
        g = if target < g { target } else { (cr * g + (1.0 - cr) * target).min(target) };
        buf[i] *= g;
    }
}

/// Frames of `frame` seconds whose RMS (dBFS) is given, for pause detection.
pub fn rms_frames(buf: &[f32], rate: f32, frame: f32) -> Vec<f32> {
    let n = ((frame * rate) as usize).max(1);
    buf.chunks(n).map(|c| 10.0 * (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).max(1e-12).log10()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: f32, f: f32, secs: f32, amp: f32) -> Vec<f32> {
        (0..(rate * secs) as usize).map(|i| amp * (2.0 * std::f32::consts::PI * f * i as f32 / rate).sin()).collect()
    }

    #[test]
    fn loudness_matches_the_bs1770_reference() {
        // A 1 kHz sine at -20 dBFS reads -20 LUFS... minus 3.01 for mono RMS vs peak, plus K-weighting (~0).
        let s = sine(48_000.0, 1000.0, 5.0, db_to_gain(-20.0));
        let l = loudness(&s, 48_000.0).unwrap();
        assert!((l - (-23.0)).abs() < 0.1, "{l}");
        let mut s2 = s.clone();
        normalize_loudness(&mut s2, 48_000.0, -14.0);
        assert!((loudness(&s2, 48_000.0).unwrap() + 14.0).abs() < 0.05);
    }

    #[test]
    fn filters_do_what_they_say() {
        let rate = 48_000.0;
        let hp = Biquad::highpass(rate, 100.0, std::f32::consts::FRAC_1_SQRT_2);
        assert!((hp.response_db(rate, 100.0) + 3.01).abs() < 0.1);
        assert!(hp.response_db(rate, 10_000.0).abs() < 0.05);
        let pk = Biquad::peaking(rate, 3000.0, 1.0, 6.0);
        assert!((pk.response_db(rate, 3000.0) - 6.0).abs() < 0.01);
        let ls = Biquad::low_shelf(rate, 200.0, -4.0);
        assert!((ls.response_db(rate, 20.0) + 4.0).abs() < 0.1);
        let hs = Biquad::high_shelf(rate, 10_000.0, 3.0);
        assert!((hs.response_db(rate, 20_000.0) - 3.0).abs() < 0.3);
        let mut hum = sine(rate, 50.0, 1.0, 0.5);
        highpass(&mut hum, rate, 100.0);
        assert!(peak_db(&hum[24_000..]) < -6.0 - 20.0, "{}", peak_db(&hum[24_000..]));
    }

    #[test]
    fn limiter_holds_the_ceiling_and_leaves_quiet_audio_alone() {
        let rate = 48_000.0;
        let mut s = sine(rate, 440.0, 1.0, 0.3);
        for i in 20_000..20_100 {
            s[i] = 1.5 * (i as f32).sin();
        }
        let before = s[1000];
        limit(&mut s, rate, -1.0, 80.0);
        assert!(peak_db(&s) <= -1.0 + 1e-4, "{}", peak_db(&s));
        assert_eq!(s[1000], before);
    }

    #[test]
    fn parallel_paths_match_serial() {
        let rate = 48_000.0;
        let mut x: Vec<f32> = (0..(40.0 * rate) as usize).map(|i| (0.4 + 0.7 * ((i / 30_000) % 3) as f32 / 2.0) * (i as f32 * 0.031).sin()).collect();
        let mut y = x.clone();
        limit(&mut x, rate, -1.0, 80.0);
        limit_serial(&mut y, rate, -1.0, 80.0);
        let d = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(d < 5e-4, "{d}"); // below -66 dB
        // Loudness of a 1 kHz sine is unchanged whatever the split.
        let s = sine(rate, 1000.0, 60.0, db_to_gain(-20.0));
        assert!((loudness(&s, rate).unwrap() + 23.0).abs() < 0.05);
    }

    #[test]
    fn fast_math_is_close() {
        for x in [1e-9f32, 1e-4, 0.013, 0.5, 1.0, 3.7, 1000.0] {
            assert!((fast_log2(x) - x.log2()).abs() < 0.006, "{x}");
        }
        for x in [-20.0f32, -3.3, -0.5, 0.0, 0.7, 5.0] {
            assert!((fast_exp2(x) / x.exp2() - 1.0).abs() < 2e-4, "{x}");
        }
    }

    #[test]
    fn dynamics() {
        let rate = 48_000.0;
        let mut loud = sine(rate, 300.0, 1.0, 0.9);
        let gr = compress(&mut loud, rate, -20.0, 4.0, 5.0, 100.0);
        assert!(gr > 10.0, "{gr}");
        let mut quiet = sine(rate, 300.0, 1.0, 0.001);
        gate(&mut quiet, rate, -40.0, 20.0, 50.0);
        assert!(peak_db(&quiet[40_000..]) < -60.0 - 18.0);
        let mut ess = sine(rate, 7000.0, 1.0, 0.5);
        let mut vowel = sine(rate, 300.0, 1.0, 0.5);
        deess(&mut ess, rate, 5000.0, -30.0, 8.0);
        deess(&mut vowel, rate, 5000.0, -30.0, 8.0);
        let red = gain_to_db(0.5) - peak_db(&ess[24_000..]);
        assert!(red > 5.0, "de-essed by {red} dB");
        assert!((peak_db(&vowel[24_000..]) - gain_to_db(0.5)).abs() < 0.5);
    }
}
