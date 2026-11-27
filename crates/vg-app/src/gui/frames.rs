//! Video frames for the preview, decoded by ffmpeg off the UI thread.
//!
//! - **Paused or scrubbing:** the frame at the playhead is asked of a worker
//!   thread. Only the newest request counts (a scrub sends one per repaint) and
//!   it waits a moment for the pointer to settle. Frames are kept as textures in
//!   a small LRU cache, and the nearest cached frame of the same file is shown
//!   while the exact one loads.
//! - **Playing:** a stream per clip. ffmpeg decodes continuously from the
//!   playhead at the project's frame rate and the canvas's on-screen size; a
//!   reader thread keeps a few frames queued, and each repaint shows the one
//!   whose time has come, dropping any that are late. It restarts on a seek, a
//!   new clip or a size change. The next clip's stream starts shortly before
//!   the cut so it plays on time.
//!
//! Frames are cover-fit by ffmpeg to the canvas's shape, so they're drawn over
//! the whole canvas. Every ffmpeg is killed when it's no longer needed.

use eframe::egui;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use vg_media::VideoReader;

/// Textures kept for scrubbing.
const CACHE: usize = 40;
/// How long a scrub request waits for a newer one before decoding.
const DEBOUNCE: Duration = Duration::from_millis(20);
/// Frames a stream decodes ahead of the playhead.
const QUEUE: usize = 4;
/// A stream starts ahead of the playhead by about as long as ffmpeg takes to
/// deliver its first frame (learned as it goes), so that frame is on time
/// rather than late. This is where it starts, and its limits.
const LEAD: f64 = 0.4;
const LEAD_RANGE: (f64, f64) = (0.15, 0.9);
/// The next clip's stream starts this long before the cut.
const PREROLL: f64 = 1.0;
/// A file that couldn't be read is tried again after this long.
const RETRY: Duration = Duration::from_secs(5);

/// A cached frame: a file, a frame index (source seconds × fps) and a size.
#[derive(Clone, PartialEq, Eq)]
struct Key {
    path: PathBuf,
    frame: i64,
    size: [u32; 2],
}

/// Which source frame shows at `src` seconds, at `fps`.
fn frame_index(src: f64, fps: u32) -> i64 {
    // A hair of slack so times that land exactly on a frame don't round down.
    (src * fps.max(1) as f64 + 1e-6).floor() as i64
}

/// What to show under the avatar.
pub struct Want<'a> {
    pub path: &'a Path,
    /// Source seconds at the playhead.
    pub src: f64,
    pub still: bool,
    /// The project's frame rate.
    pub fps: u32,
    /// Decode size: the canvas on screen, in physical pixels.
    pub size: [u32; 2],
    /// While playing: the next clip, so its stream can start early.
    pub next: Option<Next<'a>>,
}

/// The clip after the playhead, while playing.
#[derive(Clone, Copy)]
pub struct Next<'a> {
    pub path: &'a Path,
    /// Its in-point, source seconds.
    pub src_in: f64,
    /// Seconds until it starts.
    pub secs: f64,
    pub fps: u32,
    pub size: [u32; 2],
}

pub enum Shown {
    /// A texture of `size` pixels; `exact` is false while a nearer frame loads.
    Frame { id: egui::TextureId, size: egui::Vec2, exact: bool },
    /// Nothing decoded yet.
    Loading,
    Missing,
    Unreadable(String),
}

#[derive(Default)]
struct State {
    want: Option<Request>,
    busy: bool,
    done: Vec<Done>,
    quit: bool,
}

struct Request {
    key: Key,
    t: f64,
    still: bool,
}

struct Done {
    key: Key,
    image: Result<egui::ColorImage, String>,
    took: Duration,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

struct Cached {
    key: Key,
    tex: egui::TextureHandle,
    used: u64,
}

/// A clip being decoded continuously. Dropping it kills ffmpeg.
struct Stream {
    path: PathBuf,
    /// Source seconds of frame 0 (on the fps grid).
    start: f64,
    fps: u32,
    size: [u32; 2],
    rx: Receiver<(i64, egui::ColorImage)>,
    /// The next frame, taken from the queue but not due yet.
    ahead: Option<(i64, egui::ColorImage)>,
    /// Highest frame number received.
    got: Option<i64>,
    ended: bool,
    tex: Option<egui::TextureHandle>,
    shown: Option<i64>,
    started: Instant,
    _reader: VideoReader,
}

/// How decoding went, for the capture harness.
#[derive(Default, Clone, Debug)]
pub struct Stats {
    /// Stream frames put on screen.
    pub shown: u64,
    /// Stream frames skipped because they were already late.
    pub dropped: u64,
    /// Repaints while playing that had no frame for the playhead yet.
    pub waiting: u64,
    pub streams: u64,
    /// Time from starting a stream to its first frame.
    pub first_frame: Vec<Duration>,
    /// Scrub decode times.
    pub scrubs: Vec<Duration>,
}

pub struct Frames {
    shared: Arc<Shared>,
    worker: bool,
    cache: Vec<Cached>,
    clock: u64,
    stream: Option<Stream>,
    next: Option<Stream>,
    failed: HashMap<PathBuf, (String, Instant)>,
    /// The last source time asked for while playing, to tell a seek from playback.
    last_src: Option<(PathBuf, f64)>,
    /// How far ahead a stream starts, seconds.
    lead: f64,
    pub stats: Stats,
}

impl Stats {
    pub fn report(&self) -> String {
        let ms = |v: &[Duration]| v.iter().map(|d| format!("{}", d.as_millis())).collect::<Vec<_>>().join(" ");
        format!(
            "video frames: {} shown from {} stream(s), {} dropped late, {} repaints waiting · first frame ms: [{}] · scrub decode ms: [{}]",
            self.shown,
            self.streams,
            self.dropped,
            self.waiting,
            ms(&self.first_frame),
            ms(&self.scrubs)
        )
    }
}

impl Default for Frames {
    fn default() -> Self {
        Frames { shared: Default::default(), worker: false, cache: vec![], clock: 0, stream: None, next: None, failed: HashMap::new(), last_src: None, lead: LEAD, stats: Default::default() }
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().quit = true;
        self.shared.wake.notify_all();
    }
}

/// The part of a `tex`-shaped texture that covers a `dst`-shaped rectangle,
/// centred (UV coordinates). Frames are decoded in the canvas's shape already;
/// this keeps an older frame of another shape (a cached one, while the canvas
/// is resized or the format switched) from being stretched.
pub fn cover_uv(tex: egui::Vec2, dst: egui::Vec2) -> egui::Rect {
    let (ta, da) = (tex.x / tex.y.max(1.0), dst.x / dst.y.max(1.0));
    if !ta.is_finite() || !da.is_finite() || (ta - da).abs() < 1e-3 {
        return egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    }
    if ta > da {
        // Wider than the canvas: crop the sides.
        let w = da / ta;
        egui::Rect::from_min_max(egui::pos2((1.0 - w) / 2.0, 0.0), egui::pos2((1.0 + w) / 2.0, 1.0))
    } else {
        let h = ta / da;
        egui::Rect::from_min_max(egui::pos2(0.0, (1.0 - h) / 2.0), egui::pos2(1.0, (1.0 + h) / 2.0))
    }
}

/// Which queued frame to show for frame `target`: the newest one that's due
/// (the ones before it are late and dropped). `queue` is in order; None if
/// nothing is due yet.
fn pick_due(queue: &[i64], target: i64) -> Option<usize> {
    queue.iter().take_while(|&&n| n <= target).count().checked_sub(1)
}

impl Frames {
    /// The frame for `want`, starting whatever decoding it needs.
    pub fn frame(&mut self, ctx: &egui::Context, want: &Want, playing: bool) -> Shown {
        self.collect(ctx);
        if !want.path.exists() {
            self.stop_streams();
            return Shown::Missing;
        }
        if let Some((msg, at)) = self.failed.get(want.path) {
            if at.elapsed() < RETRY {
                return Shown::Unreadable(msg.clone());
            }
            self.failed.remove(want.path);
        }
        let key = Key { path: want.path.to_path_buf(), frame: if want.still { 0 } else { frame_index(want.src, want.fps) }, size: want.size };
        if playing && !want.still {
            if let Some(shown) = self.play(ctx, want, &key) {
                return shown;
            }
        } else {
            self.stop_streams();
        }
        if let Some(i) = self.cache.iter().position(|c| c.key == key) {
            self.clock += 1;
            self.cache[i].used = self.clock;
            let t = &self.cache[i].tex;
            return Shown::Frame { id: t.id(), size: t.size_vec2(), exact: true };
        }
        if !playing || want.still || self.stream.is_none() {
            let t = key.frame as f64 / want.fps.max(1) as f64;
            self.request(ctx, key.clone(), t, want.still);
        }
        // The nearest frame of the same file (the same size first) while it loads.
        let near = self.cache.iter().filter(|c| c.key.path == key.path).min_by_key(|c| ((c.key.size != key.size) as i64, (c.key.frame - key.frame).abs()));
        match near {
            Some(c) => Shown::Frame { id: c.tex.id(), size: c.tex.size_vec2(), exact: false },
            None => Shown::Loading,
        }
    }

    /// No clip at the playhead: stop decoding (the cache stays), except the
    /// next clip's stream while playing towards it.
    pub fn idle(&mut self, ctx: &egui::Context, next: Option<Next>) {
        self.collect(ctx);
        let keep = self.next.take();
        self.stop_streams();
        self.next = keep;
        self.preroll(ctx, next);
        self.last_src = None;
        self.shared.state.lock().unwrap().want = None;
    }

    /// Start the next clip's stream shortly before it's due (or drop it).
    fn preroll(&mut self, ctx: &egui::Context, next: Option<Next>) {
        match next {
            Some(n) if n.secs < PREROLL => {
                let fps = n.fps.max(1);
                let start = (frame_index(n.src_in, fps) as f64) / fps as f64;
                let have = self.next.as_ref().is_some_and(|s| s.path == n.path && (s.start - start).abs() < 1e-6 && s.size == n.size && s.fps == fps);
                if !have {
                    self.next = self.start_stream(ctx, n.path, start, fps, n.size);
                }
            }
            _ => self.next = None,
        }
    }

    /// Block until the requested frame is decoded (for offscreen captures).
    pub fn wait(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut s = self.shared.state.lock().unwrap();
        while (s.want.is_some() || s.busy) && Instant::now() < deadline {
            s = self.shared.wake.wait_timeout(s, Duration::from_millis(50)).unwrap().0;
        }
    }

    /// Is a scrub frame being decoded (or waiting to be)?
    pub fn scrubbing(&self) -> bool {
        let s = self.shared.state.lock().unwrap();
        s.busy || s.want.is_some() || !s.done.is_empty()
    }

    fn request(&mut self, ctx: &egui::Context, key: Key, t: f64, still: bool) {
        if !self.worker {
            self.worker = true;
            let (shared, ctx) = (self.shared.clone(), ctx.clone());
            std::thread::Builder::new().name("preview-frames".into()).spawn(move || worker(shared, ctx)).ok();
        }
        let mut s = self.shared.state.lock().unwrap();
        if s.want.as_ref().is_some_and(|w| w.key == key) {
            return;
        }
        s.want = Some(Request { key, t, still });
        drop(s);
        self.shared.wake.notify_all();
    }

    /// Take finished scrub frames into the cache.
    fn collect(&mut self, ctx: &egui::Context) {
        let done: Vec<Done> = std::mem::take(&mut self.shared.state.lock().unwrap().done);
        for d in done {
            self.stats.scrubs.push(d.took);
            match d.image {
                Ok(img) => {
                    let tex = ctx.load_texture(format!("video-{}-{}", d.key.path.display(), d.key.frame), img, egui::TextureOptions::LINEAR);
                    self.insert(d.key, tex);
                }
                Err(e) => {
                    self.failed.insert(d.key.path.clone(), (e, Instant::now()));
                }
            }
        }
    }

    fn insert(&mut self, key: Key, tex: egui::TextureHandle) {
        self.clock += 1;
        self.cache.retain(|c| c.key != key);
        if self.cache.len() >= CACHE
            && let Some(i) = self.cache.iter().enumerate().min_by_key(|(_, c)| c.used).map(|(i, _)| i) {
                self.cache.swap_remove(i);
            }
        self.cache.push(Cached { key, tex, used: self.clock });
    }

    /// Stop the streams, keeping the frame on screen in the cache so pausing
    /// doesn't flash.
    fn stop_streams(&mut self) {
        self.next = None;
        if let Some(s) = self.stream.take()
            && let (Some(n), Some(tex)) = (s.shown, s.tex.clone()) {
                let frame = frame_index(s.start, s.fps) + n;
                self.insert(Key { path: s.path.clone(), frame, size: s.size }, tex);
            }
    }

    /// Playback: show the stream's frame for the playhead. None when the stream
    /// has nothing yet (the caller falls back to the cache).
    fn play(&mut self, ctx: &egui::Context, want: &Want, key: &Key) -> Option<Shown> {
        let fps = want.fps.max(1);
        // A jump (a seek, or a new clip that doesn't carry on from the last) restarts.
        let jumped = match &self.last_src {
            Some((p, t)) => p != want.path || want.src < t - 1e-3 || want.src > t + 0.5,
            None => false,
        };
        self.last_src = Some((want.path.to_path_buf(), want.src));
        let usable = |s: &Stream| {
            let n = frame_index(want.src, fps) - frame_index(s.start, fps);
            // (A stream that reached the end of its file holds its last frame.)
            // (It starts a little ahead, so the playhead may not have reached it yet.)
            s.path == want.path && s.size == want.size && s.fps == fps && n >= -((LEAD_RANGE.1 * fps as f64).ceil() as i64) - 1
                // Fallen well behind: start again from here.
                && !(!s.ended && s.started.elapsed() > Duration::from_secs(1) && s.got.is_some_and(|g| n > g + fps as i64))
        };
        if self.stream.as_ref().is_some_and(|s| !usable(s) || (jumped && s.shown.is_some())) {
            self.stream = None;
        }
        if self.stream.is_none() {
            // The prerolled next clip, if it's the one now playing.
            self.stream = self.next.take().filter(|s| usable(s));
        }
        if self.stream.is_none() {
            let start = (frame_index(want.src + self.lead, fps) as f64) / fps as f64;
            self.stream = self.start_stream(ctx, want.path, start, fps, want.size);
        }
        // Start the next clip early.
        self.preroll(ctx, want.next);

        let s = self.stream.as_mut()?;
        let target = key.frame - frame_index(s.start, fps);
        // Everything queued, oldest first; show the newest that's due.
        let mut queue: Vec<(i64, egui::ColorImage)> = s.ahead.take().into_iter().collect();
        loop {
            if queue.last().is_some_and(|(n, _)| *n > target) {
                break;
            }
            match s.rx.try_recv() {
                Ok(f) => {
                    s.got = Some(f.0);
                    queue.push(f);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    s.ended = true;
                    break;
                }
            }
        }
        let numbers: Vec<i64> = queue.iter().map(|f| f.0).collect();
        let mut rest = queue.into_iter();
        if let Some(show) = pick_due(&numbers, target) {
            let (n, img) = rest.nth(show).unwrap();
            self.stats.dropped += show as u64;
            if s.shown.is_none() {
                let took = s.started.elapsed();
                self.stats.first_frame.push(took);
                self.lead = (0.7 * self.lead + 0.3 * (took.as_secs_f64() + 0.05)).clamp(LEAD_RANGE.0, LEAD_RANGE.1);
            }
            match &mut s.tex {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => s.tex = Some(ctx.load_texture(format!("video-stream-{}", s.path.display()), img, egui::TextureOptions::LINEAR)),
            }
            s.shown = Some(n);
            self.stats.shown += 1;
        }
        s.ahead = rest.next();
        // Keep repainting at the stream's pace.
        ctx.request_repaint_after(Duration::from_millis(8));
        match (&s.tex, s.shown) {
            (Some(t), Some(n)) if n <= target => {
                let exact = n == target;
                if !exact {
                    self.stats.waiting += 1;
                }
                Some(Shown::Frame { id: t.id(), size: t.size_vec2(), exact })
            }
            _ => {
                self.stats.waiting += 1;
                None
            }
        }
    }

    fn start_stream(&mut self, ctx: &egui::Context, path: &Path, start: f64, fps: u32, size: [u32; 2]) -> Option<Stream> {
        let mut reader = match VideoReader::start(path, start, fps, size[0], size[1]) {
            Ok(r) => r,
            Err(e) => {
                self.failed.insert(path.to_path_buf(), (format!("{e:#}"), Instant::now()));
                return None;
            }
        };
        let mut frames = reader.frames()?;
        let (tx, rx) = sync_channel(QUEUE);
        let ctx = ctx.clone();
        let dims = [size[0] as usize, size[1] as usize];
        std::thread::Builder::new()
            .name("preview-stream".into())
            .spawn(move || {
                let mut n = 0;
                // Ends when ffmpeg does, or is killed (the stream was dropped).
                while let Some(buf) = frames.next_frame() {
                    // Video is opaque, so straight and premultiplied alpha are the same.
                    let img = egui::ColorImage::from_rgba_premultiplied(dims, &buf);
                    if tx.send((n, img)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                    n += 1;
                }
            })
            .ok()?;
        self.stats.streams += 1;
        if std::env::var_os("VG_DEBUG").is_some() {
            eprintln!("video stream: {} from {start:.3}s at {}×{}", path.display(), size[0], size[1]);
        }
        Some(Stream { path: path.to_path_buf(), start, fps, size, rx, ahead: None, got: None, ended: false, tex: None, shown: None, started: Instant::now(), _reader: reader })
    }
}

/// The scrub decoder: waits for a request, lets newer ones replace it for a
/// moment, then decodes the newest.
fn worker(shared: Arc<Shared>, ctx: egui::Context) {
    loop {
        let mut s = shared.state.lock().unwrap();
        while s.want.is_none() && !s.quit {
            s = shared.wake.wait(s).unwrap();
        }
        if s.quit {
            return;
        }
        drop(s);
        std::thread::sleep(DEBOUNCE);
        let mut s = shared.state.lock().unwrap();
        let Some(req) = s.want.take() else { continue };
        s.busy = true;
        drop(s);
        let began = Instant::now();
        let [w, h] = req.key.size;
        let image = vg_media::frame_rgba_cover(&req.key.path, req.t, w, h, req.still)
            .map(|rgba| egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], &rgba))
            .map_err(|e| format!("{e:#}"));
        let mut s = shared.state.lock().unwrap();
        s.done.push(Done { key: req.key, image, took: began.elapsed() });
        s.busy = false;
        drop(s);
        shared.wake.notify_all();
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_crops_the_long_side_evenly() {
        // A 16:9 frame on a 9:16 canvas: only the middle strip shows.
        let uv = cover_uv(egui::vec2(1920.0, 1080.0), egui::vec2(540.0, 960.0));
        let w = (9.0 / 16.0) / (16.0 / 9.0);
        assert!((uv.width() - w).abs() < 1e-4 && (uv.center().x - 0.5).abs() < 1e-6 && uv.height() == 1.0);
        // A tall frame on a wide canvas: the top and bottom go.
        let uv = cover_uv(egui::vec2(720.0, 1280.0), egui::vec2(1920.0, 1080.0));
        assert!(uv.width() == 1.0 && (uv.center().y - 0.5).abs() < 1e-6 && uv.height() < 0.33);
        // Already the canvas's shape (what the decoder makes): the whole texture.
        assert_eq!(cover_uv(egui::vec2(540.0, 960.0), egui::vec2(270.0, 480.0)), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)));
    }

    #[test]
    fn frames_are_indexed_by_their_start() {
        assert_eq!(frame_index(0.0, 30), 0);
        assert_eq!(frame_index(1.0, 30), 30);
        // 0.1 * 30 is 2.9999… in floating point; still frame 3.
        assert_eq!(frame_index(0.1, 30), 3);
        assert_eq!(frame_index(0.0333, 30), 0);
    }

    #[test]
    fn late_frames_are_dropped_and_early_ones_wait() {
        // Nothing due yet.
        assert_eq!(pick_due(&[5, 6, 7], 4), None);
        // Exactly on time.
        assert_eq!(pick_due(&[5, 6, 7], 5), Some(0));
        // Two are late: show the newest due, drop the two before it.
        assert_eq!(pick_due(&[3, 4, 5, 6], 5), Some(2));
        // All late: show the last.
        assert_eq!(pick_due(&[1, 2], 9), Some(1));
        assert_eq!(pick_due(&[], 9), None);
    }
}
