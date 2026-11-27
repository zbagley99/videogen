//! Sound out and the narration's waveform. [`Player`] plays the narration for
//! the preview: the GUI's own clock drives the preview, and the audio is
//! (re)started at the playhead whenever playback starts or seeks.
//! [`BufferPlayer`] plays the Audio tab's rendered mix from memory.

use crate::recorder::PEAK_RATE;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Player {
    _stream: rodio::OutputStream,
    sink: rodio::Sink,
    path: PathBuf,
}

/// A paused sink on the default output, and the stream it needs kept alive.
fn open_output() -> Result<(rodio::OutputStream, rodio::Sink)> {
    let mut stream = rodio::OutputStreamBuilder::open_default_stream().context("opening the audio output")?;
    stream.log_on_drop(false);
    let sink = rodio::Sink::connect_new(stream.mixer());
    sink.pause();
    Ok((stream, sink))
}

impl Player {
    pub fn new(path: &Path) -> Result<Self> {
        let (stream, sink) = open_output()?;
        Ok(Player { _stream: stream, sink, path: path.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn play_from(&self, t: f64, volume: f32) -> Result<()> {
        self.sink.clear();
        let file = std::fs::File::open(&self.path).with_context(|| format!("opening {}", self.path.display()))?;
        let decoder = rodio::Decoder::try_from(file).with_context(|| format!("decoding {}", self.path.display()))?;
        self.sink.append(decoder);
        let _ = self.sink.try_seek(Duration::from_secs_f64(t.max(0.0)));
        self.sink.set_volume(volume);
        self.sink.play();
        Ok(())
    }

    pub fn set_volume(&self, volume: f32) {
        self.sink.set_volume(volume);
    }

    pub fn pause(&self) {
        self.sink.pause();
    }
}

/// Loaded narration: where it is, how long, and its waveform for the timeline.
pub struct AudioInfo {
    pub path: PathBuf,
    pub duration: f64,
    /// Peak per 1/`PEAK_RATE` s.
    pub peaks: Vec<f32>,
    /// When the file was last changed (it's reloaded when that moves).
    pub mtime: Option<std::time::SystemTime>,
}

impl AudioInfo {
    pub fn load(path: &Path) -> Result<Self> {
        let duration = vg_media::probe_duration(path)?;
        let mut peaks = vg_media::waveform_peaks(path, PEAK_RATE).unwrap_or_default();
        // Normalise so quiet recordings still show a readable waveform.
        let max = peaks.iter().copied().fold(0.0f32, f32::max);
        if max > 1e-4 {
            peaks.iter_mut().for_each(|p| *p /= max);
        }
        Ok(AudioInfo { path: path.to_path_buf(), duration, peaks, mtime: super::util::mtime(path) })
    }
}

/// Plays a rendered buffer (mono) from memory, e.g. the audio editor's mix.
pub struct BufferPlayer {
    _stream: rodio::OutputStream,
    sink: rodio::Sink,
}

/// A mono `f32` buffer shared with the renderer, played from `pos`.
struct ArcSource {
    data: std::sync::Arc<Vec<f32>>,
    pos: usize,
    rate: u32,
}

impl Iterator for ArcSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let v = self.data.get(self.pos).copied();
        self.pos += 1;
        v
    }
}

impl rodio::Source for ArcSource {
    fn current_span_len(&self) -> Option<usize> {
        Some(self.data.len().saturating_sub(self.pos))
    }
    fn channels(&self) -> rodio::ChannelCount {
        1
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs_f64(self.data.len() as f64 / self.rate as f64))
    }
}

impl BufferPlayer {
    pub fn new() -> Result<Self> {
        let (stream, sink) = open_output()?;
        Ok(BufferPlayer { _stream: stream, sink })
    }

    pub fn play(&self, data: std::sync::Arc<Vec<f32>>, rate: u32, t: f64, volume: f32) {
        self.sink.clear();
        let pos = ((t.max(0.0) * rate as f64) as usize).min(data.len());
        self.sink.append(ArcSource { data, pos, rate });
        self.sink.set_volume(volume);
        self.sink.play();
    }

    pub fn set_volume(&self, volume: f32) {
        self.sink.set_volume(volume);
    }

    pub fn pause(&self) {
        self.sink.pause();
    }
}
