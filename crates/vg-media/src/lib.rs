//! ffmpeg/ffprobe as subprocesses: encoding the avatar track, probing media,
//! and decoding frames and sound for the editor.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// ProRes 4444 with alpha in a .mov: large, lossless-ish, imports cleanly into Kdenlive/Resolve.
    ProRes4444,
    /// VP9 with alpha in a .webm: much smaller, slower to encode.
    WebmAlpha,
    /// Numbered PNG frames in a folder.
    PngSequence,
    /// H.264 (NVENC when available) over a solid background, with the narration muxed in: for review.
    PreviewMp4,
}

impl OutputFormat {
    pub const ALL: [OutputFormat; 4] = [Self::ProRes4444, Self::WebmAlpha, Self::PngSequence, Self::PreviewMp4];

    pub fn label(self) -> &'static str {
        match self {
            Self::ProRes4444 => "ProRes 4444 (.mov, alpha)",
            Self::WebmAlpha => "VP9 (.webm, alpha)",
            Self::PngSequence => "PNG sequence (alpha)",
            Self::PreviewMp4 => "Preview MP4 (with audio)",
        }
    }

    /// Default output name inside the export folder.
    pub fn default_name(self) -> &'static str {
        match self {
            Self::ProRes4444 => "avatar.mov",
            Self::WebmAlpha => "avatar.webm",
            Self::PngSequence => "avatar_frames",
            Self::PreviewMp4 => "preview.mp4",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "prores" | "mov" => Some(Self::ProRes4444),
            "webm" | "vp9" => Some(Self::WebmAlpha),
            "png" | "pngseq" => Some(Self::PngSequence),
            "preview" | "mp4" => Some(Self::PreviewMp4),
            _ => None,
        }
    }
}

pub struct EncodeSettings {
    pub format: OutputFormat,
    pub output: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Muxed into `PreviewMp4` only.
    pub audio: Option<PathBuf>,
    /// Background for `PreviewMp4`, as `0xRRGGBB`.
    pub background: u32,
    /// Extra video filter applied after compositing (`PreviewMp4` only), e.g. `ass='captions.ass'`.
    pub extra_filter: Option<String>,
    /// Video clips under the avatar (`PreviewMp4` only), in time order; the
    /// background colour shows where there are none.
    pub video: Vec<VideoLayer>,
}

/// One clip of the video track, for compositing under the avatar in the preview MP4.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoLayer {
    pub path: PathBuf,
    /// Where it starts on the output's timeline, seconds.
    pub start: f64,
    /// The part of the source that plays: `src_in..src_in + len`.
    pub src_in: f64,
    pub len: f64,
    /// A still image, shown for `len`.
    pub still: bool,
    /// Fades from and to black (and silence), seconds.
    pub fade_in: f64,
    pub fade_out: f64,
    /// Mix the clip's own sound in (it has an audio stream and isn't muted).
    pub audio: bool,
}

/// Fill `w`×`h` with the picture like a background ("cover"): crop the source
/// to the output's shape, centred, then scale. Cropping first keeps it cheap:
/// swscale converts to RGBA several times faster in one step than after a crop.
/// (ffmpeg has already turned phone video upright by then.)
fn cover_filter(w: u32, h: u32) -> String {
    format!("crop=w=min(iw\\,ih*{w}/{h}):h=min(ih\\,iw*{h}/{w}),scale={w}:{h}:flags=bilinear,setsar=1")
}

/// A seek point for `-ss`: ffmpeg starts at the first frame at or after it, so
/// a time on a frame (say 131/30 s, printed rounded up as 4.366667) must not
/// round past that frame to the next.
fn seek(t: f64) -> String {
    format!("{:.6}", (t - 1e-4).max(0.0))
}

/// Clips are opaque: an image's transparent parts show black (what fades go
/// to), not whatever is under the track. Premultiplying the alpha does it.
const FLATTEN: &str = "format=rgba,premultiply=inplace=1";

/// Everything after the global options for the preview MP4: inputs, the filter
/// graph and the codecs (the output path is added by the caller).
///
/// Inputs: 0 the background colour, 1 the avatar frames on stdin, then one per
/// clip, then the narration. The graph lays each clip (cover-fit, faded,
/// shifted to its start) over the colour in turn, the avatar over that, then
/// the captions; the narration and the clips' sound are mixed.
fn preview_args(s: &EncodeSettings, nvenc: bool) -> Vec<std::ffi::OsString> {
    let mut a: Vec<std::ffi::OsString> = vec![];
    fn push(a: &mut Vec<std::ffi::OsString>, v: &[&str]) {
        a.extend(v.iter().map(std::ffi::OsString::from));
    }
    let (w, h) = (s.width, s.height);
    let size = format!("{w}x{h}");
    let fps = s.fps.max(1).to_string();
    let bg = format!("color=c=0x{:06x}:s={size}:r={fps}", s.background & 0xFF_FFFF);
    push(&mut a, &["-f", "lavfi", "-i", &bg]);
    push(&mut a, &["-f", "rawvideo", "-pix_fmt", "rgba", "-s", &size, "-r", &fps, "-i", "-"]);
    let secs = |v: f64| format!("{:.6}", v.max(0.0));
    for c in &s.video {
        if let (false, Some(hw)) = (c.still, hwaccel()) {
            push(&mut a, &["-hwaccel", hw]);
        }
        if c.still {
            push(&mut a, &["-loop", "1", "-framerate", &fps, "-t", &secs(c.len)]);
        } else {
            push(&mut a, &["-ss", &seek(c.src_in), "-t", &secs(c.len)]);
        }
        a.push("-i".into());
        a.push(c.path.clone().into_os_string());
    }
    let narration = s.audio.as_ref().map(|p| {
        a.push("-i".into());
        a.push(p.clone().into_os_string());
        2 + s.video.len()
    });

    let mut graph = vec![];
    let mut base = "0:v".to_string();
    let mut sounds = vec![];
    // Every sound in one format before mixing (a mono narration and a stereo clip, say).
    const AFMT: &str = "aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo";
    // (Also for the narration alone: ffmpeg's AAC encoder refuses some mono layouts.)
    let clip_sound = s.video.iter().any(|c| c.audio);
    if let Some(n) = narration {
        graph.push(format!("[{n}:a]{AFMT}[{}]", if clip_sound { "n" } else { "a" }));
        sounds.push("[n]".to_string());
    }
    for (k, c) in s.video.iter().enumerate() {
        let input = 2 + k;
        // Opaque yuv before the fades, so they go to black rather than to transparent.
        let opaque = if c.still { format!("{FLATTEN},format=yuv420p") } else { "format=yuv420p".into() };
        let mut v = format!("[{input}:v]setpts=PTS-STARTPTS,fps={fps},{},{opaque}", cover_filter(w, h));
        if c.fade_in > 0.0 {
            v += &format!(",fade=t=in:st=0:d={}", secs(c.fade_in));
        }
        if c.fade_out > 0.0 {
            v += &format!(",fade=t=out:st={}:d={}", secs(c.len - c.fade_out), secs(c.fade_out));
        }
        v += &format!(",setpts=PTS+{}/TB[c{k}]", secs(c.start));
        graph.push(v);
        // Before its first frame and after its last, the layer below shows through.
        graph.push(format!("[{base}][c{k}]overlay=eof_action=pass:format=auto[b{k}]"));
        base = format!("b{k}");
        if c.audio {
            let mut s = format!("[{input}:a]asetpts=PTS-STARTPTS,{AFMT}");
            if c.fade_in > 0.0 {
                s += &format!(",afade=t=in:st=0:d={}", secs(c.fade_in));
            }
            if c.fade_out > 0.0 {
                s += &format!(",afade=t=out:st={}:d={}", secs(c.len - c.fade_out), secs(c.fade_out));
            }
            s += &format!(",adelay={}:all=1[s{k}]", (c.start * 1000.0).round().max(0.0) as u64);
            graph.push(s);
            sounds.push(format!("[s{k}]"));
        }
    }
    let extra = s.extra_filter.as_ref().map(|f| format!(",{f}")).unwrap_or_default();
    graph.push(format!("[{base}][1:v]overlay=shortest=1:format=auto{extra},format=yuv420p[v]"));
    if clip_sound {
        // Summed at their own levels (no normalising, which would halve the narration).
        // Padded with silence so `-shortest` ends the file with the picture.
        graph.push(format!("{}amix=inputs={}:duration=longest:normalize=0,apad[a]", sounds.concat(), sounds.len()));
    }
    push(&mut a, &["-filter_complex", &graph.join(";"), "-map", "[v]"]);
    if clip_sound {
        push(&mut a, &["-map", "[a]", "-shortest"]);
    } else if narration.is_some() {
        push(&mut a, &["-map", "[a]"]);
    }
    if clip_sound || narration.is_some() {
        push(&mut a, &["-c:a", "aac", "-b:a", "192k"]);
    }
    if nvenc {
        push(&mut a, &["-c:v", "h264_nvenc", "-preset", "p5", "-cq", "21"]);
    } else {
        push(&mut a, &["-c:v", "libx264", "-preset", "fast", "-crf", "20"]);
    }
    a
}

/// Frames in, video file out.
pub struct Encoder {
    child: Child,
    stdin: Option<ChildStdin>,
    frame_bytes: usize,
    output: PathBuf,
}

fn has_encoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.split_whitespace().nth(1) == Some(name)))
        .unwrap_or(false)
}

impl Encoder {
    pub fn start(s: &EncodeSettings) -> Result<Self> {
        let size = format!("{}x{}", s.width, s.height);
        let fps = s.fps.to_string();
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
        let raw_input = ["-f", "rawvideo", "-pix_fmt", "rgba", "-s", &size, "-r", &fps, "-i", "-"];
        match s.format {
            OutputFormat::PreviewMp4 => {
                cmd.args(preview_args(s, has_encoder("h264_nvenc")));
                cmd.arg(&s.output);
            }
            OutputFormat::ProRes4444 => {
                cmd.args(raw_input);
                cmd.args(["-c:v", "prores_ks", "-profile:v", "4444", "-pix_fmt", "yuva444p10le", "-alpha_bits", "16", "-vendor", "apl0"]);
                cmd.arg(&s.output);
            }
            OutputFormat::WebmAlpha => {
                cmd.args(raw_input);
                cmd.args(["-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-b:v", "0", "-crf", "30", "-row-mt", "1", "-deadline", "good", "-cpu-used", "4"]);
                cmd.arg(&s.output);
            }
            OutputFormat::PngSequence => {
                std::fs::create_dir_all(&s.output).with_context(|| format!("creating {}", s.output.display()))?;
                cmd.args(raw_input);
                cmd.args(["-c:v", "png"]).arg(s.output.join("frame_%05d.png"));
            }
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        if std::env::var_os("VG_DEBUG").is_some() {
            eprintln!("{cmd:?}");
        }
        let mut child = cmd.spawn().context("starting ffmpeg (is it installed and on PATH?)")?;
        let stdin = child.stdin.take();
        Ok(Encoder { child, stdin, frame_bytes: (4 * s.width * s.height) as usize, output: s.output.clone() })
    }

    /// One straight-alpha RGBA frame.
    pub fn push(&mut self, rgba: &[u8]) -> Result<()> {
        if rgba.len() != self.frame_bytes {
            bail!("frame is {} bytes, expected {}", rgba.len(), self.frame_bytes);
        }
        let stdin = self.stdin.as_mut().context("encoder already finished")?;
        if stdin.write_all(rgba).is_err() {
            // ffmpeg died; surface its stderr instead of a bare broken pipe.
            return Err(self.fail());
        }
        Ok(())
    }

    fn fail(&mut self) -> anyhow::Error {
        self.stdin = None;
        match wait_stderr(&mut self.child) {
            Ok(msg) => anyhow::anyhow!("ffmpeg failed: {msg}"),
            Err(e) => e,
        }
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        drop(self.stdin.take());
        let msg = wait_stderr(&mut self.child)?;
        if !msg.is_empty() {
            bail!("ffmpeg failed: {msg}");
        }
        Ok(self.output.clone())
    }

    /// Stop early (e.g. the user cancelled); the partial file is removed.
    pub fn abort(mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.output.is_dir() {
            let _ = std::fs::remove_dir_all(&self.output);
        } else {
            let _ = std::fs::remove_file(&self.output);
        }
    }
}

/// Wait for `child` to exit; `Ok("")` on success, `Ok(stderr)` on failure.
fn wait_stderr(child: &mut Child) -> Result<String> {
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut err);
    }
    let status = child.wait()?;
    if status.success() { Ok(String::new()) } else { Ok(if err.trim().is_empty() { status.to_string() } else { err.trim().to_string() }) }
}

/// Run `cmd` to completion and return its stdout; on failure the error is
/// `failed` followed by ffmpeg's/ffprobe's stderr.
fn run(cmd: &mut Command, failed: impl FnOnce() -> String) -> Result<Vec<u8>> {
    let what = cmd.get_program().to_string_lossy().to_string();
    let out = cmd.output().with_context(|| format!("running {what}"))?;
    if !out.status.success() {
        bail!("{}: {}", failed(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

/// Duration of a media file in seconds.
pub fn probe_duration(path: &Path) -> Result<f64> {
    probe_info(path)?.duration.with_context(|| format!("no duration for {}", path.display()))
}

/// A media file's sound mixed to mono, as f32 samples at `rate` Hz.
pub fn decode_mono(path: &Path, rate: u32) -> Result<Vec<f32>> {
    let out = run(
        Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(path).args(["-vn", "-ac", "1", "-ar", &rate.to_string(), "-f", "f32le", "-"]),
        || format!("ffmpeg couldn't decode {}", path.display()),
    )?;
    Ok(out.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

/// Peak amplitude (0..1) per 1/`rate` seconds of a media file's audio, mixed to mono.
pub fn waveform_peaks(path: &Path, rate: u32) -> Result<Vec<f32>> {
    const SAMPLE_RATE: u32 = 16_000;
    let per_bucket = (SAMPLE_RATE / rate.max(1)).max(1) as usize;
    Ok(decode_mono(path, SAMPLE_RATE)?.chunks(per_bucket).map(|c| c.iter().fold(0.0, |m: f32, s| m.max(s.abs())).min(1.0)).collect())
}

/// One frame of a video at `t` seconds, scaled to fit `max` pixels, as PNG bytes.
pub fn video_frame_png(path: &Path, t: f64, max: u32) -> Result<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &format!("{t:.3}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", &format!("scale={max}:{max}:force_original_aspect_ratio=decrease"), "-f", "image2pipe", "-vcodec", "png", "-"])
        .output()
        .context("running ffmpeg")?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("ffmpeg couldn't read a frame of {}: {}", path.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

/// What ffprobe says about a media file, for showing it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaInfo {
    pub duration: Option<f64>,
    /// Overall bit rate, bits per second.
    pub bit_rate: Option<u64>,
    pub video: Option<VideoStream>,
    pub audio: Option<AudioStream>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoStream {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
    /// Clockwise degrees the picture should be turned (phone video).
    pub rotation: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioStream {
    pub codec: String,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
}

/// Probe a file's container and first video and audio streams.
pub fn probe_info(path: &Path) -> Result<MediaInfo> {
    let out = run(Command::new("ffprobe").args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"]).arg(path), || {
        format!("ffprobe couldn't read {}", path.display())
    })?;
    parse_probe(&String::from_utf8_lossy(&out))
}

fn parse_probe(json: &str) -> Result<MediaInfo> {
    let v: serde_json::Value = serde_json::from_str(json).context("parsing ffprobe output")?;
    let num = |x: &serde_json::Value| x.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| x.as_f64());
    let rate = |x: &serde_json::Value| {
        let s = x.as_str()?;
        let (a, b) = s.split_once('/').unwrap_or((s, "1"));
        let (a, b) = (a.parse::<f64>().ok()?, b.parse::<f64>().ok()?);
        (b > 0.0 && a > 0.0).then(|| a / b)
    };
    let format = &v["format"];
    let mut info = MediaInfo { duration: num(&format["duration"]), bit_rate: num(&format["bit_rate"]).map(|b| b as u64), video: None, audio: None };
    for s in v["streams"].as_array().into_iter().flatten() {
        let codec = s["codec_name"].as_str().unwrap_or("?").to_string();
        match s["codec_type"].as_str() {
            // Cover art in audio files is a "video" stream with one picture; skip it.
            Some("video") if info.video.is_none() && s["disposition"]["attached_pic"].as_i64() != Some(1) => {
                let rotation = s["side_data_list"].as_array().into_iter().flatten().find_map(|d| d["rotation"].as_i64()).or_else(|| s["tags"]["rotate"].as_str().and_then(|r| r.parse().ok())).unwrap_or(0) as i32;
                info.video = Some(VideoStream {
                    codec,
                    width: s["width"].as_u64().unwrap_or(0) as u32,
                    height: s["height"].as_u64().unwrap_or(0) as u32,
                    fps: rate(&s["avg_frame_rate"]).or_else(|| rate(&s["r_frame_rate"])),
                    rotation,
                });
            }
            Some("audio") if info.audio.is_none() => {
                info.audio = Some(AudioStream { codec, sample_rate: num(&s["sample_rate"]).map(|r| r as u32), channels: s["channels"].as_u64().map(|c| c as u32) });
            }
            _ => {}
        }
    }
    Ok(info)
}

/// Run a child process at low priority, so decoding for the preview never
/// competes with the UI (or anything else the user is doing).
fn low_priority(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe extern "C" {
            fn nice(inc: std::ffi::c_int) -> std::ffi::c_int;
        }
        // SAFETY: nice(2) is a plain syscall wrapper, safe between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                nice(10);
                Ok(())
            });
        }
    }
    cmd
}

/// The hardware decoder to ask ffmpeg for (`cuda` when an NVIDIA GPU is
/// usable), checked once. Codecs it can't decode fall back to software.
fn hwaccel() ->Option<&'static str> {
    static HW: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *HW.get_or_init(|| {
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-init_hw_device", "cuda=gpu", "-f", "lavfi", "-i", "color=s=16x16", "-frames:v", "1", "-f", "null", "-"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some("cuda")
    })
}

/// An ffmpeg reading `path` from `t` seconds, at low priority and on two
/// threads: the common part of a single frame and a stream. Stills ignore `t`.
/// `gpu` decodes on the GPU (when there is one); setting it up costs about a
/// quarter of a second, so it's for streams, not single frames.
fn decoder(path: &Path, t: f64, still: bool, gpu: bool) -> Command {
    let mut cmd = Command::new("ffmpeg");
    low_priority(&mut cmd);
    cmd.args(["-v", "error", "-nostdin", "-threads", "2", "-filter_threads", "1"]);
    if !still {
        if let (true, Some(hw)) = (gpu, hwaccel()) {
            cmd.args(["-hwaccel", hw]);
        }
        cmd.args(["-ss", &seek(t)]);
    }
    cmd.arg("-i").arg(path);
    cmd.args(["-an", "-sn"]);
    cmd
}

/// One frame at `t` seconds (any `t` for a still image), cover-fit to exactly
/// `w`×`h`, as RGBA pixels. Decoded on the CPU: for one frame that's quicker
/// than starting the GPU decoder.
pub fn frame_rgba_cover(path: &Path, t: f64, w: u32, h: u32, still: bool) -> Result<Vec<u8>> {
    // (A still's pixels come out premultiplied: over black they're the export's.)
    let vf = if still { format!("{},{FLATTEN}", cover_filter(w, h)) } else { cover_filter(w, h) };
    let out = decoder(path, t, still, false)
        .args(["-frames:v", "1", "-vf", &vf, "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdin(Stdio::null())
        .output()
        .context("running ffmpeg")?;
    if !out.status.success() || out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg couldn't read a frame of {} at {t:.2}s{}", path.display(), if err.trim().is_empty() { String::new() } else { format!(": {}", err.trim()) });
    }
    if out.stdout.len() != (w * h * 4) as usize {
        bail!("unexpected frame size from ffmpeg for {} ({} bytes for {w}×{h})", path.display(), out.stdout.len());
    }
    Ok(out.stdout)
}

/// A video decoded continuously from `start` seconds at `fps`, cover-fit to
/// `w`×`h`: frame `n` shows the source at `start + n / fps`. Decoded on the GPU
/// when there is one (with a software fallback). Hand `frames()` to
/// a reader thread; dropping this kills ffmpeg, which ends that thread's reads.
pub struct VideoReader {
    child: Child,
    frames: Option<Frames>,
}

/// The frames coming out of a `VideoReader`.
pub struct Frames {
    stdout: std::process::ChildStdout,
    frame_bytes: usize,
}

impl VideoReader {
    pub fn start(path: &Path, start: f64, fps: u32, w: u32, h: u32) -> Result<Self> {
        let vf = format!("fps={},{}", fps.max(1), cover_filter(w, h));
        let mut child = decoder(path, start, false, true)
            .args(["-vf", &vf, "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting ffmpeg (is it installed and on PATH?)")?;
        let stdout = child.stdout.take().context("no ffmpeg output")?;
        Ok(VideoReader { child, frames: Some(Frames { stdout, frame_bytes: (w * h * 4) as usize }) })
    }

    /// The frame source (once).
    pub fn frames(&mut self) -> Option<Frames> {
        self.frames.take()
    }
}

impl Frames {
    /// The next frame, or None at the end of the video (or once it's killed).
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        use std::io::Read;
        let mut buf = vec![0; self.frame_bytes];
        self.stdout.read_exact(&mut buf).ok().map(|_| buf)
    }
}

impl Drop for VideoReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn parses_ffprobe_json() {
        let j = r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"avg_frame_rate":"30000/1001","pix_fmt":"yuv420p","side_data_list":[{"rotation":-90}]},
            {"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2}],
            "format":{"format_name":"mov,mp4","duration":"12.5","bit_rate":"8000000"}}"#;
        let i = parse_probe(j).unwrap();
        assert_eq!(i.duration, Some(12.5));
        let v = i.video.unwrap();
        assert_eq!((v.width, v.height, v.rotation, v.codec.as_str()), (1920, 1080, -90, "h264"));
        assert!((v.fps.unwrap() - 29.97).abs() < 0.01);
        assert_eq!(i.audio.unwrap().sample_rate, Some(48000));
        // Cover art isn't a video.
        let j = r#"{"streams":[{"codec_type":"video","codec_name":"png","width":500,"height":500,"disposition":{"attached_pic":1}}],"format":{}}"#;
        assert!(parse_probe(j).unwrap().video.is_none());
    }
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    fn settings(video: Vec<VideoLayer>, audio: bool) -> EncodeSettings {
        EncodeSettings {
            format: OutputFormat::PreviewMp4,
            output: "out.mp4".into(),
            width: 1080,
            height: 1920,
            fps: 30,
            audio: audio.then(|| PathBuf::from("voice.wav")),
            background: 0x102030,
            extra_filter: Some("ass=a.ass".into()),
            video,
        }
    }

    fn args(s: &EncodeSettings) -> Vec<String> {
        preview_args(s, false).iter().map(|a| a.to_string_lossy().to_string()).collect()
    }

    fn graph(a: &[String]) -> &str {
        &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1]
    }

    #[test]
    fn no_clips_is_the_plain_overlay() {
        let a = args(&settings(vec![], true));
        assert_eq!(graph(&a), "[2:a]aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo[a];[0:v][1:v]overlay=shortest=1:format=auto,ass=a.ass,format=yuv420p[v]");
        assert!(a.windows(2).any(|w| w == ["-map", "[a]"]));
        assert!(a.iter().any(|x| x == "color=c=0x102030:s=1080x1920:r=30"));
    }

    #[test]
    fn clips_are_layered_faded_shifted_and_mixed() {
        let clip = VideoLayer { path: "a.mp4".into(), start: 1.0, src_in: 2.0, len: 5.0, still: false, fade_in: 0.5, fade_out: 0.0, audio: true };
        let still = VideoLayer { path: "b.png".into(), start: 7.0, src_in: 0.0, len: 4.0, still: true, fade_in: 0.0, fade_out: 1.0, audio: false };
        let a = args(&settings(vec![clip, still], true));
        let g = graph(&a);
        // Inputs: colour, stdin, the clip trimmed at its in-point, the looped still, then the narration.
        let joined = a.join(" ");
        assert!(joined.contains("-ss 1.999900 -t 5.000000 -i a.mp4"), "{joined}");
        assert!(joined.contains("-loop 1 -framerate 30 -t 4.000000 -i b.png"), "{joined}");
        assert!(joined.contains("-i voice.wav"));
        let mut parts: Vec<&str> = g.split(';').collect();
        assert_eq!(parts.len(), 8, "{g}");
        assert_eq!(parts.remove(0), "[4:a]aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo[n]");
        assert!(parts[0].starts_with("[2:v]setpts=PTS-STARTPTS,fps=30,crop=w=min(iw\\,ih*1080/1920):h=min(ih\\,iw*1920/1080),scale=1080:1920"));
        assert!(parts[0].ends_with(",setsar=1,format=yuv420p,fade=t=in:st=0:d=0.500000,setpts=PTS+1.000000/TB[c0]"), "{}", parts[0]);
        assert_eq!(parts[1], "[0:v][c0]overlay=eof_action=pass:format=auto[b0]");
        assert_eq!(parts[2], "[2:a]asetpts=PTS-STARTPTS,aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo,afade=t=in:st=0:d=0.500000,adelay=1000:all=1[s0]");
        assert!(parts[3].contains("premultiply=inplace=1,format=yuv420p,fade=t=out:st=3.000000:d=1.000000,setpts=PTS+7.000000/TB[c1]"), "{}", parts[3]);
        assert_eq!(parts[4], "[b0][c1]overlay=eof_action=pass:format=auto[b1]");
        assert_eq!(parts[5], "[b1][1:v]overlay=shortest=1:format=auto,ass=a.ass,format=yuv420p[v]");
        // The still has no sound to mix; the clip's sound joins the narration.
        assert_eq!(parts[6], "[n][s0]amix=inputs=2:duration=longest:normalize=0,apad[a]");
        assert!(a.windows(3).any(|w| w == ["-map", "[a]", "-shortest"]));
        // Muted (or silent) clips leave the narration as it was.
        let quiet = VideoLayer { path: "a.mp4".into(), start: 0.0, src_in: 0.0, len: 1.0, still: false, fade_in: 0.0, fade_out: 0.0, audio: false };
        let a = args(&settings(vec![quiet], true));
        assert!(!graph(&a).contains("amix") && graph(&a).starts_with("[3:a]aformat"));
        assert!(a.windows(2).any(|w| w == ["-map", "[a]"]));
    }
}
