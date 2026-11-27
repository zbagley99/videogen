//! Microphone recording: a cpal input stream, downmixed to mono and handed to a
//! writer thread that streams a 32-bit float WAV to disk. The header is refreshed
//! every second, so a crash mid-take still leaves a playable file.

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Waveform resolution, for recordings and loaded audio alike: one peak per 1/`PEAK_RATE` s.
pub const PEAK_RATE: u32 = 100;

/// State the writer thread publishes for the level meter and waveform.
#[derive(Default)]
pub struct Shared {
    /// Loudest sample since the meter last read it (`f32` bits).
    peak: AtomicU32,
    /// Frames written to the file.
    frames: AtomicU64,
    /// Samples are only written once armed (after a countdown).
    armed: AtomicBool,
    clipped: AtomicBool,
    peaks: Mutex<Vec<f32>>,
    error: Mutex<Option<String>>,
}

impl Shared {
    /// Loudest sample since the last call (0–1+), resetting it.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    fn bump_peak(&self, v: f32) {
        let mut cur = self.peak.load(Ordering::Relaxed);
        while v > f32::from_bits(cur) {
            match self.peak.compare_exchange_weak(cur, v.to_bits(), Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(c) => cur = c,
            }
        }
    }

    pub fn clipped(&self) -> bool {
        self.clipped.load(Ordering::Relaxed)
    }

    /// The most recent `n` waveform peaks.
    pub fn recent_peaks(&self, n: usize) -> Vec<f32> {
        let p = self.peaks.lock().unwrap();
        p[p.len().saturating_sub(n)..].to_vec()
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }
}

/// A take in progress.
pub struct Recording {
    stream: Option<cpal::Stream>,
    writer: Option<std::thread::JoinHandle<Result<()>>>,
    shared: Arc<Shared>,
    pub path: PathBuf,
    pub rate: u32,
    pub device: String,
}

/// A finished take.
pub struct Take {
    pub path: PathBuf,
    pub duration: f64,
}

/// Names of the input devices ("default" first when there is one).
pub fn input_names() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let mut names: Vec<String> = host.input_devices().context("listing input devices")?.filter_map(|d| d.name().ok()).collect();
    if let Some(def) = host.default_input_device().and_then(|d| d.name().ok()) {
        names.retain(|n| *n != def);
        names.insert(0, def);
    }
    Ok(names)
}

fn find_device(name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    match name {
        None => host.default_input_device().ok_or_else(|| anyhow!("no microphone found (no default input device)")),
        Some(n) => host
            .input_devices()
            .context("listing input devices")?
            .find(|d| d.name().is_ok_and(|x| x == n))
            .ok_or_else(|| anyhow!("no input device named {n:?} (see `vg record --list`)")),
    }
}

impl Recording {
    /// Open the input and start writing to `path`. With `armed = false` the meter
    /// runs but nothing is written until [`Recording::arm`] (for a countdown).
    pub fn start(device: Option<&str>, path: &Path, armed: bool) -> Result<Self> {
        let dev = find_device(device)?;
        let name = dev.name().unwrap_or_else(|_| "input".into());
        let supported = dev.default_input_config().with_context(|| format!("reading {name}'s input format"))?;
        let config: cpal::StreamConfig = supported.config();
        let (rate, channels) = (config.sample_rate.0, config.channels as usize);

        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let shared = Arc::new(Shared::default());
        shared.armed.store(armed, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        let writer = {
            let (shared, path) = (shared.clone(), path.to_path_buf());
            std::thread::spawn(move || write_take(rx, &path, rate, &shared))
        };

        let err_shared = shared.clone();
        let on_error = move |e: cpal::StreamError| {
            *err_shared.error.lock().unwrap() = Some(e.to_string());
        };
        use cpal::SampleFormat as F;
        let stream = match supported.sample_format() {
            F::F32 => build::<f32>(&dev, &config, channels, tx, on_error),
            F::I16 => build::<i16>(&dev, &config, channels, tx, on_error),
            F::I32 => build::<i32>(&dev, &config, channels, tx, on_error),
            F::U16 => build::<u16>(&dev, &config, channels, tx, on_error),
            F::U8 => build::<u8>(&dev, &config, channels, tx, on_error),
            other => Err(cpal::BuildStreamError::BackendSpecific { err: cpal::BackendSpecificError { description: format!("unsupported sample format {other:?}") } }),
        };
        let stream = match stream.map_err(anyhow::Error::from).and_then(|s| s.play().map(|_| s).map_err(Into::into)) {
            Ok(s) => s,
            Err(e) => {
                // The sender is gone, so the writer ends; don't leave an empty file.
                let _ = writer.join();
                let _ = std::fs::remove_file(path);
                return Err(e.context(format!("opening {name}")));
            }
        };
        Ok(Recording { stream: Some(stream), writer: Some(writer), shared, path: path.to_path_buf(), rate, device: name })
    }

    pub fn shared(&self) -> &Shared {
        &self.shared
    }

    pub fn arm(&self) {
        self.shared.armed.store(true, Ordering::Relaxed);
    }

    pub fn armed(&self) -> bool {
        self.shared.armed.load(Ordering::Relaxed)
    }

    /// Seconds written so far.
    pub fn elapsed(&self) -> f64 {
        self.shared.frames.load(Ordering::Relaxed) as f64 / self.rate as f64
    }

    /// Stop and finalise the file.
    pub fn finish(mut self) -> Result<Take> {
        self.close()?;
        Ok(Take { path: self.path.clone(), duration: self.elapsed() })
    }

    /// Stop and delete the file.
    pub fn discard(mut self) {
        let _ = self.close();
        let _ = std::fs::remove_file(&self.path);
    }

    fn close(&mut self) -> Result<()> {
        // Dropping the stream drops its sender, which ends the writer.
        drop(self.stream.take());
        match self.writer.take().map(|w| w.join()) {
            Some(Ok(r)) => r,
            Some(Err(_)) => Err(anyhow!("the recording writer crashed")),
            None => Ok(()),
        }
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn build<T>(dev: &cpal::Device, config: &cpal::StreamConfig, channels: usize, tx: mpsc::Sender<Vec<f32>>, on_error: impl FnMut(cpal::StreamError) + Send + 'static) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = channels.max(1);
    dev.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            // Downmix to mono; the writer does everything else.
            let mono: Vec<f32> = data.chunks(channels).map(|f| f.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / f.len() as f32).collect();
            let _ = tx.send(mono);
        },
        on_error,
        None,
    )
}

/// The writer thread: mono chunks in, WAV out, meter and waveform published.
fn write_take(rx: mpsc::Receiver<Vec<f32>>, path: &Path, rate: u32, shared: &Shared) -> Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut wav = hound::WavWriter::create(path, spec).with_context(|| format!("creating {}", path.display()))?;
    let bucket = (rate / PEAK_RATE).max(1) as usize;
    let (mut acc, mut n) = (0.0f32, 0usize);
    let mut since_flush = 0usize;
    for chunk in rx {
        let loudest = chunk.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        // Whether this chunk is kept is decided before its level is published,
        // so anything that sees the level also sees the decision.
        let armed = shared.armed.load(Ordering::Acquire);
        shared.bump_peak(loudest);
        if !armed {
            continue;
        }
        if loudest >= 0.999 {
            shared.clipped.store(true, Ordering::Relaxed);
        }
        let mut new_peaks = Vec::new();
        for &s in &chunk {
            wav.write_sample(s)?;
            acc = acc.max(s.abs());
            n += 1;
            if n == bucket {
                new_peaks.push(acc);
                (acc, n) = (0.0, 0);
            }
        }
        if !new_peaks.is_empty() {
            shared.peaks.lock().unwrap().extend(new_peaks);
        }
        shared.frames.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        since_flush += chunk.len();
        if since_flush >= rate as usize {
            wav.flush()?;
            since_flush = 0;
        }
    }
    wav.finalize().with_context(|| format!("finishing {}", path.display()))?;
    Ok(())
}

/// Length of a WAV file from its header (fast; no decoding).
pub fn wav_duration(path: &Path) -> Option<f64> {
    let r = hound::WavReader::open(path).ok()?;
    Some(r.duration() as f64 / r.spec().sample_rate as f64)
}

/// Takes for `stem` in `dir`: `<stem>-take<N>.wav`, in order.
pub fn takes(dir: &Path, stem: &str) -> Vec<Take> {
    let prefix = format!("{stem}-take");
    let mut out: Vec<(u32, Take)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_stem()?.to_str()?.to_string();
            let n: u32 = name.strip_prefix(&prefix)?.parse().ok()?;
            (p.extension()? == "wav").then(|| (n, Take { duration: wav_duration(&p).unwrap_or(0.0), path: p }))
        })
        .collect();
    out.sort_by_key(|(n, _)| *n);
    out.into_iter().map(|(_, t)| t).collect()
}

/// The next free `<stem>-take<N>.wav` in `dir`.
pub fn next_take_path(dir: &Path, stem: &str) -> PathBuf {
    (1..).map(|n| dir.join(format!("{stem}-take{n}.wav"))).find(|p| !p.exists()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_armed_audio_and_publishes_levels() {
        let dir = std::env::temp_dir().join(format!("vg-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = next_take_path(&dir, "narration");
        assert!(path.ends_with("narration-take1.wav"));
        let shared = Shared::default();
        let (tx, rx) = mpsc::channel();
        let rate = 48_000;
        let tone: Vec<f32> = (0..rate / 2).map(|i| 0.5 * (i as f32 * 0.05).sin()).collect();
        // Before arming (a countdown): metered but not written.
        tx.send(vec![0.9; 480]).unwrap();
        let _ = std::thread::scope(|s| {
            let h = s.spawn(|| write_take(rx, &path, rate, &shared));
            while shared.take_peak() == 0.0 {
                std::thread::yield_now();
            }
            shared.armed.store(true, Ordering::Relaxed);
            tx.send(tone.clone()).unwrap();
            tx.send(tone).unwrap();
            drop(tx);
            h.join().unwrap()
        });
        assert!((wav_duration(&path).unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(shared.frames.load(Ordering::Relaxed), rate as u64);
        assert_eq!(shared.recent_peaks(1000).len(), 100);
        assert!(shared.recent_peaks(1).first().is_some_and(|p| (*p - 0.5).abs() < 0.01));
        assert!(!shared.clipped());
        let found = takes(&dir, "narration");
        assert_eq!(found.len(), 1);
        assert_eq!(next_take_path(&dir, "narration").file_name().unwrap(), "narration-take2.wav");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
