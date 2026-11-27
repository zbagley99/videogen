//! A videogen project ties together a pose set, a cue sheet and a preset, and
//! knows how to export them. Shared by the GUI and the `vg` CLI.

pub mod projects;
pub mod transcripts;
pub mod video;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use vg_core::cues::CueError;
use vg_core::paths::relative_path;
use vg_core::{CueSheet, PoseSet, Preset, Timeline, Transcript, Word};
use vg_media::{EncodeSettings, Encoder, OutputFormat};
use vg_render::{Compositor, frame_at};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ExportOptions {
    pub format: OutputFormat,
    /// Output file (or folder, for PNG sequences). Relative paths are relative to the project.
    pub output: PathBuf,
    /// Background colour for the preview MP4, `0xRRGGBB`.
    #[serde(default = "default_bg")]
    pub background: u32,
}

fn default_bg() -> u32 {
    0x1f7a3a
}

impl Default for ExportOptions {
    fn default() -> Self {
        ExportOptions { format: OutputFormat::ProRes4444, output: PathBuf::from("out/avatar.mov"), background: default_bg() }
    }
}

/// What's saved in `*.vgproj` (RON). Paths are relative to the project file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProjectFile {
    pub poses: PathBuf,
    pub cues: PathBuf,
    pub preset: Preset,
    #[serde(default)]
    pub export: ExportOptions,
    /// Images, video and audio imported into the project (the sidebar's Media tab).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<PathBuf>,
    /// The video track: clips of the imported video and images on the timeline.
    #[serde(default, skip_serializing_if = "video::VideoTrack::is_empty")]
    pub video: video::VideoTrack,
}

impl ProjectFile {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        ron::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::new().struct_names(false))?;
        std::fs::write(path, text + "\n").with_context(|| format!("writing {}", path.display()))
    }
}

/// `path` resolved against `base` unless it's absolute.
pub fn resolve(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() { path.to_path_buf() } else { base.join(path) }
}

/// Parse a cue sheet and resolve it against the poses. `cue_dir` resolves `@audio`.
/// Returns the sheet, the audio path (if any), and the timeline or every error found.
pub fn build_timeline(
    cue_text: &str,
    cue_dir: &Path,
    poses: &PoseSet,
    audio_duration: Option<f64>,
    words: &[Word],
) -> (Option<CueSheet>, Option<PathBuf>, Result<Timeline, Vec<CueError>>) {
    let sheet = match CueSheet::parse(cue_text) {
        Ok(s) => s,
        Err(e) => return (None, None, Err(vec![e])),
    };
    let audio = sheet.audio.as_ref().map(|a| resolve(cue_dir, a));
    let tl = Timeline::build_with_words(&sheet, poses, audio_duration, words);
    (Some(sheet), audio, tl)
}

/// The words file a cue sheet points at (`@words`), resolved, without a full parse.
pub fn words_path(cue_text: &str, cue_dir: &Path) -> Option<PathBuf> {
    cue_text.lines().find_map(|l| directive(l, "words").map(|(v, _)| v).filter(|v| !v.is_empty()).map(|v| resolve(cue_dir, Path::new(v))))
}

/// A `@key value` line's value and what follows it (spacing and a trailing
/// ` # comment`), by the same rule the cue parser uses.
pub(crate) fn directive<'a>(line: &'a str, key: &str) -> Option<(&'a str, &'a str)> {
    let rest = line.trim().strip_prefix('@')?;
    let (k, value) = rest.split_once(char::is_whitespace)?;
    if k != key {
        return None;
    }
    let value = value.trim_start();
    let end = value.find(" #").map_or(value, |i| &value[..i]).trim_end().len();
    Some((&value[..end], &value[end..]))
}

/// Where the Whisper sidecar lives: `$VG_SIDECAR_DIR`, else `sidecar/whisper` next to
/// (or above) the executable or the working directory.
pub fn find_sidecar() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("VG_SIDECAR_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(exe.ancestors().skip(1).map(Path::to_path_buf));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    roots
        .into_iter()
        .map(|r| r.join("sidecar/whisper"))
        .find(|d| d.join("pyproject.toml").exists())
        .context("couldn't find sidecar/whisper (set VG_SIDECAR_DIR to its path)")
}

#[derive(Clone, Debug)]
pub struct TranscribeOptions {
    pub model: String,
    /// `None` = auto-detect.
    pub language: Option<String>,
}

impl Default for TranscribeOptions {
    fn default() -> Self {
        TranscribeOptions { model: "large-v3-turbo".into(), language: None }
    }
}

/// Run Whisper on `audio`. `progress(seconds_done, seconds_total)`; `status(line)` gets other stderr lines.
pub fn transcribe(audio: &Path, opts: &TranscribeOptions, cancel: &Arc<AtomicBool>, mut progress: impl FnMut(f64, f64), mut status: impl FnMut(&str)) -> Result<Option<Transcript>> {
    use std::io::{BufRead, BufReader, Read};
    use std::process::{Command, Stdio};
    let dir = find_sidecar()?;
    let mut cmd = Command::new("uv");
    cmd.arg("run").arg("--quiet").arg("--project").arg(&dir).arg(dir.join("transcribe.py")).arg(audio);
    cmd.args(["--model", &opts.model]);
    if let Some(l) = &opts.language {
        cmd.args(["--language", l]);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    let mut child = cmd.spawn().context("running `uv` (install it from https://docs.astral.sh/uv/)")?;
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let mut err_tail: Vec<String> = Vec::new();
    for line in BufReader::new(child.stderr.take().unwrap()).lines().map_while(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        let mut parts = line.split_whitespace();
        if parts.next() == Some("progress") {
            let done = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let total = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
            progress(done, total);
        } else if !line.trim().is_empty() {
            status(&line);
            err_tail.push(line);
            if err_tail.len() > 12 {
                err_tail.remove(0);
            }
        }
    }
    let status_code = child.wait()?;
    let out = reader.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    if !status_code.success() {
        anyhow::bail!("transcription failed:\n{}", err_tail.join("\n"));
    }
    let t: Transcript = serde_json::from_str(out.trim()).context("the transcriber printed something that isn't a transcript")?;
    Ok(Some(t))
}

/// Paths written next to the audio: `<stem>.words.json` and (suggested) `<stem>.md`.
pub fn transcript_paths(audio: &Path) -> (PathBuf, PathBuf) {
    let stem = audio.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "narration".into());
    let dir = audio.parent().unwrap_or(Path::new("."));
    (dir.join(format!("{stem}.words.json")), dir.join(format!("{stem}.md")))
}

/// Save the words JSON and build a cue sheet whose paths are relative to `cue_dir`.
pub fn cue_sheet_for(transcript: &Transcript, audio: &Path, cue_dir: &Path, poses: Option<&PoseSet>, model: &str) -> Result<String> {
    let (words_file, _) = transcript_paths(audio);
    transcript.save(&words_file)?;
    let rel = |p: &Path| relative_path(p, cue_dir).to_string_lossy().to_string();
    let note = format!("Transcribed from {} with Whisper {model}.", audio.file_name().map(|n| n.to_string_lossy()).unwrap_or_default());
    Ok(vg_core::transcript::cue_sheet(transcript, &rel(audio), &rel(&words_file), poses, &note))
}

/// Everything an export needs; cheap to clone into a worker thread.
#[derive(Clone)]
pub struct ExportJob {
    pub timeline: Timeline,
    pub preset: Preset,
    pub poses: PoseSet,
    pub format: OutputFormat,
    pub output: PathBuf,
    pub audio: Option<PathBuf>,
    pub background: u32,
    /// Transcript word timings, for captions.
    pub words: Vec<Word>,
    /// The video track with its sources resolved (`resolve_track`). Only the
    /// preview MP4 shows it; the transparent formats stay pure overlays.
    pub video: video::VideoTrack,
}

/// The track with every clip's source resolved against `base`.
pub fn resolve_track(base: &Path, track: &video::VideoTrack) -> video::VideoTrack {
    let mut t = track.clone();
    for c in &mut t.clips {
        c.source = resolve(base, &c.source);
    }
    t
}

/// The clips an export of `duration` seconds lays under the avatar; each is probed for a sound to mix (unless muted). A missing or
/// unreadable file is an error rather than a silent gap.
fn video_layers(track: &video::VideoTrack, duration: f64) -> Result<Vec<vg_media::VideoLayer>> {
    let mut out = vec![];
    for c in track.clips.iter().filter(|c| c.start < duration && !c.is_empty()) {
        let name = c.source.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !c.source.exists() {
            anyhow::bail!("the video clip {name} at {} is missing ({})", vg_core::format_time(c.start), c.source.display());
        }
        let audio = !c.still && !c.muted && vg_media::probe_info(&c.source).with_context(|| format!("reading the video clip {name}"))?.audio.is_some();
        out.push(vg_media::VideoLayer {
            path: c.source.clone(),
            start: c.start,
            src_in: if c.still { 0.0 } else { c.src_in },
            // Past the end is cut by the encoder (the file ends with the avatar frames).
            len: c.len(),
            still: c.still,
            fade_in: c.fade_in,
            fade_out: c.fade_out,
            audio,
        });
    }
    Ok(out)
}

impl ExportJob {
    /// Where the captions file goes: next to the output, `<stem>.ass`.
    pub fn captions_path(&self) -> PathBuf {
        let stem = self.output.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "captions".into());
        self.output.with_file_name(format!("{stem}.ass"))
    }
}

/// Escape a path for an ffmpeg filter option value inside a filtergraph (two levels).
fn filter_path(p: &Path) -> String {
    let esc = |s: &str, special: &str| s.chars().fold(String::new(), |mut o, c| {
        if special.contains(c) {
            o.push('\\');
        }
        o.push(c);
        o
    });
    esc(&esc(&p.to_string_lossy(), "\\':"), "\\'[],;")
}

/// Render every frame and encode it. `progress(done, total)` is called per frame;
/// set `cancel` to stop (the partial output is deleted).
pub fn export(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    job: &ExportJob,
    cancel: &Arc<AtomicBool>,
    mut progress: impl FnMut(u64, u64),
) -> Result<Option<PathBuf>> {
    let (w, h, fps) = (job.preset.width, job.preset.height, job.preset.fps.max(1));
    let mut comp = Compositor::new(device, queue, w, h);
    comp.load_poses(&job.poses)?;
    if let Some(dir) = job.output.parent()
        && !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
    // Captions: an .ass file next to the output (burned into the preview MP4).
    let mut burn = None;
    if job.preset.captions.enabled {
        let chunks = vg_core::captions::chunks(&job.timeline, &job.words, &job.preset.captions);
        if !chunks.is_empty() {
            let path = job.captions_path();
            let ass = vg_core::captions::to_ass(&chunks, &job.preset.captions, w, h);
            std::fs::write(&path, ass).with_context(|| format!("writing {}", path.display()))?;
            if job.format == OutputFormat::PreviewMp4 {
                burn = Some(format!("ass={}", filter_path(&path)));
            }
        }
    }
    let video = match job.format {
        OutputFormat::PreviewMp4 => video_layers(&job.video, job.timeline.duration)?,
        _ => vec![],
    };
    let mut enc = Encoder::start(&EncodeSettings {
        format: job.format,
        output: job.output.clone(),
        width: w,
        height: h,
        fps,
        audio: job.audio.clone(),
        background: job.background,
        extra_filter: burn,
        video,
    })?;
    let total = (job.timeline.duration * fps as f64).ceil().max(1.0) as u64;
    let mut rgba = Vec::new();
    for i in 0..total {
        if cancel.load(Ordering::Relaxed) {
            enc.abort();
            return Ok(None);
        }
        let t = i as f64 / fps as f64;
        comp.draw(&frame_at(&job.timeline, &job.preset, &job.poses, t));
        comp.read_rgba(&mut rgba)?;
        enc.push(&rgba)?;
        progress(i + 1, total);
    }
    Ok(Some(enc.finish()?))
}
