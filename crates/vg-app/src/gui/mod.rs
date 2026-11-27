//! The videogen GUI. The Video tab: sidebar (poses, media) | preview | script,
//! with a timeline underneath. The Audio tab: the narration editor.

mod assist;
mod audio;
mod dialog;
mod export;
mod frames;
mod media;
mod music;
mod palette;
mod place;
mod prefs;
mod preview;
mod record;
mod recordings;
mod script;
mod settings;
pub mod shot;
mod studio;
mod theme;
mod timeline;
mod transcribe;
mod util;
mod video;

use anyhow::{Context, Result};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};
use util::{file_name, mtime};
use vg_core::cues::CueError;
use vg_core::paths::{canon, relative_path};
use vg_core::{CueSheet, PoseSet, Preset, Timeline};
use vg_project::{ProjectFile, build_timeline, resolve};

const STORAGE_LAST_PROJECT: &str = "last_project";
const STORAGE_PREFS: &str = "prefs";

const STARTER_SCRIPT: &str = "\
# New video. One line per sentence; put [pose] tags where the avatar should change.
# Click a pose on the left to insert its tag at the cursor (or at the playhead in Live mode).
# Set the narration with File > Import narration, or give lines times like [0:03.5].
@duration 10

[0:00] [wave] Hey everyone, welcome back!
[0:03] [explaining_idea] Today I'm trying something new.
[0:06] [excited] And it actually worked!
";

pub fn run(project: Option<PathBuf>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("videogen").with_inner_size([1500.0, 900.0]).with_min_inner_size([900.0, 560.0]),
        renderer: eframe::Renderer::Wgpu,
        // The UI repaints on demand (and ~60 Hz while playing); vsync would busy-spin a core on NVIDIA.
        vsync: false,
        ..Default::default()
    };
    eframe::run_native("videogen", options, Box::new(move |cc| Ok(Box::new(VgApp::new(cc, project)?))))
        .map_err(|e| anyhow::anyhow!("GUI failed: {e}"))
}

/// Per-user GUI preferences (not part of a project).
#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(default)]
pub struct Prefs {
    pub volume: f32,
    pub muted: bool,
    pub preview_bg: preview::Background,
    pub preview_bg_color: [u8; 3],
    pub preview_bg_image: Option<PathBuf>,
    pub live_tag: bool,
    /// Shade the areas the YouTube Shorts UI covers.
    pub show_guides: bool,
    /// Timeline drags pull toward word transitions, line cuts, pose changes and the playhead.
    pub snap_timeline: bool,
    /// Show the canvas grid and pull the avatar onto its lines while dragging.
    pub snap_canvas: bool,
    /// Grid cells across (and down) the canvas.
    pub grid_divs: u32,
    /// Microphone for recording (None: the system default).
    pub record_device: Option<String>,
    /// Count 3 s down before a take starts.
    pub record_countdown: bool,
    /// Transcribe a take as soon as it's recorded.
    pub record_transcribe: bool,
    /// The Video (poses, captions) or Audio (narration clean-up) tab.
    pub view: View,
    /// The OpenAI-compatible server that suggests poses, and the ones to choose from.
    pub llm_url: String,
    pub llm_endpoints: Vec<String>,
    /// Extra direction for the suggestions, and whether they replace existing tags.
    pub llm_style: String,
    pub llm_replace: bool,
    /// Whisper model and language ("" = detect) for transcribing, and how the
    /// new script gets its pose tags.
    pub transcribe_model: String,
    pub transcribe_language: String,
    pub transcribe_autotag: bool,
    pub transcribe_llm: bool,
    /// Projects opened or saved lately, newest first.
    pub recent_projects: Vec<PathBuf>,
    /// Where the project manager looks for projects and makes new ones (None: the working directory).
    pub projects_dir: Option<PathBuf>,
    /// More folders to look for transcripts in (besides the project's, the script's and the working directory).
    pub transcript_dirs: Vec<PathBuf>,
    /// Filler words for "Remove fillers", comma-separated.
    pub fillers: String,
    /// Remove the oldest transcripts when they take more room than this.
    pub transcript_limit: TranscriptLimit,
    /// Transcripts the limit never removes.
    pub transcript_keep: Vec<PathBuf>,
    /// Show the script as its raw source text instead of the line list.
    pub script_source: bool,
    /// Which tab the left sidebar shows.
    pub sidebar: media::Sidebar,
    /// What timeline drags snap to (when Snap is on).
    pub snap_to: video::SnapTo,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct TranscriptLimit {
    pub on: bool,
    pub mb: f64,
    /// Count (and remove) each transcript's recording too.
    pub with_audio: bool,
    /// Never remove a transcript a script points at.
    pub keep_used: bool,
}

impl Default for TranscriptLimit {
    fn default() -> Self {
        TranscriptLimit { on: false, mb: 500.0, with_audio: true, keep_used: true }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum View {
    #[default]
    Video,
    Audio,
}

impl Prefs {
    /// The playback volume, 0 while muted.
    pub fn out_volume(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }
}

impl Default for Prefs {
    fn default() -> Self {
        let cfg = crate::config::Config::load();
        Prefs { volume: 1.0, muted: false, preview_bg: preview::Background::Checker, preview_bg_color: [31, 122, 58], preview_bg_image: None, live_tag: false, show_guides: true, snap_timeline: true, snap_canvas: false, grid_divs: 12, record_device: None, record_countdown: true,
            record_transcribe: true,
            view: View::Video,
            llm_url: cfg.llm_url,
            llm_endpoints: cfg.llm_endpoints,
            llm_style: String::new(),
            llm_replace: false,
            transcribe_model: cfg.whisper_model,
            transcribe_language: String::new(),
            transcribe_autotag: true,
            transcribe_llm: false,
            recent_projects: Vec::new(),
            projects_dir: None,
            transcript_dirs: Vec::new(),
            fillers: cfg.fillers,
            transcript_limit: TranscriptLimit::default(),
            transcript_keep: Vec::new(),
            script_source: false,
            sidebar: media::Sidebar::Poses,
            snap_to: video::SnapTo::Captions,
        }
    }
}

/// The result of the last parse of the script.
#[derive(Default)]
pub struct Parsed {
    pub sheet: Option<CueSheet>,
    /// The last timeline that built without errors (kept while the script has errors, so the preview doesn't vanish mid-edit).
    pub timeline: Option<Timeline>,
    pub errors: Vec<CueError>,
    /// `@audio`, resolved.
    pub audio_path: Option<PathBuf>,
}

struct Status {
    text: String,
    error: bool,
    at: Instant,
}

pub struct VgApp {
    rs: egui_wgpu::RenderState,
    prefs: Prefs,

    // Project.
    project_path: Option<PathBuf>,
    project: ProjectFile,
    project_saved: ProjectFile,
    poses_path: Option<PathBuf>,
    poses: PoseSet,
    poses_dirty: bool,
    cue_path: Option<PathBuf>,
    cue_text: String,
    cue_saved_text: String,
    parsed: Parsed,
    audio: Option<audio::AudioInfo>,
    /// Word timings from `@words`, keyed by file and mtime.
    words: Option<(PathBuf, Option<SystemTime>, Vec<vg_core::Word>)>,
    /// Word timings edited in the timeline and not yet saved.
    words_dirty: bool,

    // Playback.
    time: f64,
    playing: bool,
    last_tick: Option<Instant>,
    player: Option<audio::Player>,

    // Panels.
    palette: palette::Palette,
    media: media::Media,
    dialogs: dialog::Dialogs,
    video: video::Video,
    /// For waking the UI from other threads.
    egui_ctx: egui::Context,
    preview: preview::Preview,
    script: script::ScriptEditor,
    timeline: timeline::TimelineView,
    export: export::ExportPanel,
    transcribe: transcribe::TranscribePanel,
    record: record::RecordPanel,
    studio: studio::Studio,
    assist: assist::AssistPanel,
    /// Where floating windows open.
    place: place::Placer,
    settings_open: bool,
    /// The Settings window (preferences, projects, transcripts, storage).
    prefs_win: prefs::PrefsWindow,
    /// The recordings browser (Audio tab › Open recording).
    recordings: recordings::Recordings,
    /// Background music (Audio tab): file import and link downloads.
    music: music::MusicPanel,

    status: Option<Status>,
    watch: Watch,
    confirm_close: bool,
    allow_close: bool,
}

/// Modification times of files we hot-reload.
#[derive(Default)]
struct Watch {
    last_check: Option<Instant>,
    poses: Vec<(PathBuf, Option<SystemTime>)>,
    cue: Option<(PathBuf, Option<SystemTime>)>,
}

impl VgApp {
    fn new(cc: &eframe::CreationContext<'_>, project: Option<PathBuf>) -> Result<Self> {
        let rs = cc.wgpu_render_state.clone().context("videogen needs the wgpu renderer")?;
        let prefs: Prefs = cc.storage.and_then(|s| eframe::get_value(s, STORAGE_PREFS)).unwrap_or_default();
        let last: Option<PathBuf> = cc.storage.and_then(|s| eframe::get_value(s, STORAGE_LAST_PROJECT));
        Ok(Self::with_state(&cc.egui_ctx, rs, prefs, project.or(last.filter(|p| p.exists()))))
    }

    /// Build the app on an existing egui context + wgpu state (also used for offscreen screenshots).
    pub fn with_state(ctx: &egui::Context, rs: egui_wgpu::RenderState, prefs: Prefs, project: Option<PathBuf>) -> Self {
        theme::install(ctx);
        let preview = preview::Preview::new(&rs, 1080, 1920);
        let default_project = ProjectFile {
            poses: PathBuf::from(crate::config::DEFAULT_POSES),
            cues: PathBuf::from("script.md"),
            preset: Preset::shorts(),
            export: Default::default(),
            media: vec![],
            video: Default::default(),
        };
        let mut app = VgApp {
            rs,
            prefs,
            project_path: None,
            project: default_project.clone(),
            project_saved: default_project,
            poses_path: None,
            poses: PoseSet::default(),
            poses_dirty: false,
            cue_path: None,
            cue_text: STARTER_SCRIPT.into(),
            cue_saved_text: String::new(),
            parsed: Parsed::default(),
            audio: None,
            words: None,
            words_dirty: false,
            time: 0.0,
            playing: false,
            last_tick: None,
            player: None,
            palette: Default::default(),
            media: Default::default(),
            dialogs: Default::default(),
            video: Default::default(),
            egui_ctx: ctx.clone(),
            preview,
            script: Default::default(),
            timeline: Default::default(),
            export: Default::default(),
            transcribe: Default::default(),
            record: Default::default(),
            studio: Default::default(),
            assist: Default::default(),
            place: Default::default(),
            settings_open: false,
            prefs_win: Default::default(),
            recordings: Default::default(),
            music: Default::default(),
            status: None,
            watch: Default::default(),
            confirm_close: false,
            allow_close: false,
        };
        let opened = match project {
            Some(p) => match app.open_project(ctx, &p) {
                Ok(()) => true,
                Err(e) => {
                    app.error(format!("{e:#}"));
                    false
                }
            },
            None => false,
        };
        if !opened {
            // Fresh start: pick up ./avatar/poses.ron when run from a project folder.
            let default_poses = PathBuf::from(crate::config::DEFAULT_POSES);
            if default_poses.exists()
                && let Err(e) = app.load_poses(ctx, &default_poses) {
                    app.error(format!("{e:#}"));
                }
            app.reparse();
        }
        app
    }

    // ---- status line ------------------------------------------------------

    fn info(&mut self, text: impl Into<String>) {
        self.status = Some(Status { text: text.into(), error: false, at: Instant::now() });
    }

    fn error(&mut self, text: impl Into<String>) {
        self.status = Some(Status { text: text.into(), error: true, at: Instant::now() });
    }

    // ---- project I/O ------------------------------------------------------

    /// Directory relative paths are resolved against: the project's folder, else the working directory.
    fn base(&self) -> PathBuf {
        self.project_path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
    }

    fn cue_dir(&self) -> PathBuf {
        self.cue_path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| self.base())
    }

    fn open_project(&mut self, ctx: &egui::Context, path: &Path) -> Result<()> {
        let pf = ProjectFile::load(path)?;
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let cue_path = resolve(&base, &pf.cues);
        let cue_text = if cue_path.exists() {
            std::fs::read_to_string(&cue_path).with_context(|| format!("reading {}", cue_path.display()))?
        } else {
            STARTER_SCRIPT.to_string()
        };
        self.stop();
        self.project_path = Some(path.to_path_buf());
        self.project = pf.clone();
        self.project_saved = pf.clone();
        self.cue_saved_text = if cue_path.exists() { cue_text.clone() } else { String::new() };
        self.cue_text = cue_text;
        self.cue_path = Some(cue_path);
        self.time = 0.0;
        self.audio = None;
        self.player = None;
        self.load_poses(ctx, &resolve(&base, &pf.poses))?;
        self.timeline.fit_all();
        prefs::projects::remember(self, path);
        self.info(format!("Opened {}", file_name(path)));
        Ok(())
    }

    fn save_project_to(&mut self, path: &Path) -> Result<()> {
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        // The script needs a home before the project can point at it.
        if self.cue_path.is_none() {
            let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "script".into());
            self.cue_path = Some(base.join(format!("{stem}.md")));
        }
        self.save_script()?;
        if self.studio.dirty() {
            self.studio.save()?;
        }
        studio::refresh_narration(self);
        if self.poses_dirty {
            self.save_poses()?;
        }
        let rel = |p: &Path| relative_path(p, &base);
        let mut pf = self.project.clone();
        pf.cues = rel(self.cue_path.as_ref().unwrap());
        // Media paths follow the project when it's saved somewhere else.
        if self.base() != base {
            pf.media = pf.media.iter().map(|m| rel(&resolve(&self.base(), m))).collect();
        }
        if let Some(pp) = &self.poses_path {
            pf.poses = rel(pp);
        }
        pf.save(path)?;
        self.project = pf.clone();
        self.project_saved = pf;
        self.project_path = Some(path.to_path_buf());
        prefs::projects::remember(self, path);
        self.prefs_win.transcripts.check_limit = true;
        self.info(format!("saved {}", path.display()));
        Ok(())
    }

    /// Save the project; `Ok(false)` when it has no file yet and a dialog is
    /// asking where to put it (`close` closes the app once that's saved).
    fn save_project(&mut self, close: bool) -> Result<bool> {
        match self.project_path.clone() {
            Some(p) => self.save_project_to(&p).map(|_| true),
            None => {
                self.save_project_as(close);
                Ok(false)
            }
        }
    }

    /// Ask where to save the project (`close` closes the app once that's saved).
    fn save_project_as(&mut self, close: bool) {
        let d = rfd::FileDialog::new().add_filter("videogen project", &["vgproj"]).set_file_name("video.vgproj").set_directory(self.base());
        dialog::open(self, dialog::Want::SaveProjectAs { close }, d, dialog::How::Save);
    }

    /// Save the script; `Ok(false)` when it has no file yet and a dialog is
    /// asking where to put it.
    fn save_script(&mut self) -> Result<bool> {
        let path = match &self.cue_path {
            Some(p) => p.clone(),
            None => {
                self.save_script_as(self.base());
                return Ok(false);
            }
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, &self.cue_text).with_context(|| format!("writing {}", path.display()))?;
        self.cue_saved_text = self.cue_text.clone();
        self.save_words()?;
        self.watch.cue = Some((path.clone(), mtime(&path)));
        Ok(true)
    }

    /// Ask where to save the script, starting in `dir`.
    fn save_script_as(&mut self, dir: PathBuf) {
        let d = rfd::FileDialog::new().add_filter("cue sheet", &["md", "txt"]).set_file_name("script.md").set_directory(dir);
        dialog::open(self, dialog::Want::SaveScriptAs, d, dialog::How::Save);
    }

    /// Write edited word timings back to the `@words` file (keeping its language/duration).
    fn save_words(&mut self) -> Result<()> {
        if !self.words_dirty {
            return Ok(());
        }
        let Some((path, _, words)) = &self.words else { return Ok(()) };
        let mut t = vg_core::Transcript::load(path).unwrap_or_default();
        let text = words.iter().map(|w| w.word.as_str()).collect::<Vec<_>>().join(" ");
        t.segments = vec![vg_core::transcript::Segment {
            start: words.first().map(|w| w.start).unwrap_or(0.0),
            end: words.last().map(|w| w.end).unwrap_or(0.0),
            text,
            words: words.clone(),
        }];
        t.save(path)?;
        let path = path.clone();
        if let Some(w) = &mut self.words {
            w.1 = mtime(&path);
        }
        self.words_dirty = false;
        Ok(())
    }

    /// Replace one line's word timings (from the timeline's word lane). Creates a
    /// words file (and its `@words` line) if the script has none yet.
    pub fn set_line_words(&mut self, span: usize, new: &[vg_core::Word]) {
        let Some(tl) = &self.parsed.timeline else { return };
        let Some(sp) = tl.spans.get(span).cloned() else { return };
        // No words file yet, or `@words` names one that doesn't exist: (re)point it.
        if self.words.as_ref().is_none_or(|(p, _, w)| w.is_empty() && !p.exists()) {
            let stem = self.cue_path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "script".into());
            let path = self.cue_dir().join(format!("{stem}.words.json"));
            let rel = relative_path(&path, &self.cue_dir());
            set_directive(&mut self.cue_text, "words", &rel.to_string_lossy());
            self.words = Some((path.clone(), mtime(&path), Vec::new()));
        }
        if let Some((_, _, words)) = &mut self.words {
            vg_core::captions::set_line_words(words, &sp, new);
        }
        self.words_dirty = true;
        self.reparse();
    }

    fn save_poses(&mut self) -> Result<()> {
        if let Some(p) = self.poses_path.clone() {
            self.poses.save(&p)?;
            self.poses_dirty = false;
            self.snapshot_pose_mtimes();
            self.info(format!("saved {}", p.display()));
        }
        Ok(())
    }

    fn load_poses(&mut self, ctx: &egui::Context, path: &Path) -> Result<()> {
        let poses = PoseSet::load(path)?;
        self.preview.load_poses(&poses)?;
        self.palette.load_thumbnails(ctx, &poses);
        self.poses = poses;
        self.poses_path = Some(path.to_path_buf());
        self.poses_dirty = false;
        self.snapshot_pose_mtimes();
        self.reparse();
        Ok(())
    }

    fn snapshot_pose_mtimes(&mut self) {
        let mut files: Vec<PathBuf> = (0..self.poses.poses.len()).map(|i| self.poses.path_of(i)).collect();
        files.extend(self.poses_path.clone());
        self.watch.poses = files.into_iter().map(|f| (f.clone(), mtime(&f))).collect();
    }

    /// Asks for the art folder, then where to save the library (see `dialog`).
    fn create_pose_library(&mut self) {
        let d = rfd::FileDialog::new().set_title("Folder of pose PNGs").set_directory(self.base());
        dialog::open(self, dialog::Want::PoseArt, d, dialog::How::Folder);
    }

    fn create_pose_library_at(&mut self, ctx: &egui::Context, art: &Path, out: PathBuf) -> Result<()> {
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let set = PoseSet::generate(art, &out)?;
        set.save(&out)?;
        self.load_poses(ctx, &out)?;
        self.info(format!("created {} with {} poses", out.display(), self.poses.poses.len()));
        Ok(())
    }

    /// Number a folder of new pose PNGs on from the art folder, move them in and
    /// add them to the library (the rest of it is left as it is).
    fn import_poses(&mut self) {
        if self.poses_path.is_none() {
            return self.error("open or create a pose library first");
        }
        let start = self.poses.poses.first().map(|_| self.poses.path_of(0)).and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| self.base());
        let d = rfd::FileDialog::new().set_title("Folder of new pose PNGs").set_directory(start);
        dialog::open(self, dialog::Want::ImportPoses, d, dialog::How::Folder);
    }

    fn import_poses_from(&mut self, ctx: &egui::Context, folder: &Path) -> Result<()> {
        let Some(path) = self.poses_path.clone() else { anyhow::bail!("open or create a pose library first") };
        if self.poses_dirty {
            self.save_poses()?;
        }
        let mut set = self.poses.clone();
        let added = set.import_folder(folder)?;
        set.save(&path)?;
        self.load_poses(ctx, &path)?;
        let names: Vec<String> = added.iter().map(|&i| self.poses.poses[i].tag.clone()).collect();
        self.info(format!("added {} poses: {} · describe them in each pose's inspector (right-click) so the LLM knows them", added.len(), names.join(", ")));
        Ok(())
    }

    fn import_audio(&mut self) {
        let d = rfd::FileDialog::new().set_title("Narration audio").add_filter("audio", util::AUDIO_EXTS).set_directory(self.cue_dir());
        dialog::open(self, dialog::Want::ImportNarration, d, dialog::How::File);
    }

    fn import_audio_from(&mut self, path: &Path) {
        self.set_narration(path);
        self.use_saved_words(path);
        media::add(self, &[path.to_path_buf()]);
    }

    /// Point `@words` at `audio`'s saved transcript, when it has one.
    fn use_saved_words(&mut self, audio: &Path) {
        if let Some((words, n, _)) = transcribe::saved(audio) {
            let rel = relative_path(&words, &self.cue_dir());
            set_directive(&mut self.cue_text, "words", &rel.to_string_lossy());
            self.reparse();
            self.info(format!("using the saved transcript ({n} words)"));
        }
    }

    /// Point the script's `@audio` at `path`.
    pub fn set_narration(&mut self, path: &Path) {
        let rel = relative_path(path, &self.cue_dir());
        set_directive(&mut self.cue_text, "audio", &rel.to_string_lossy());
        // The audio decides the length unless the script says otherwise.
        remove_directive(&mut self.cue_text, "duration");
        self.reparse();
    }

    /// Open a script (cue sheet) on its own.
    fn open_script(&mut self, p: &Path) -> Result<()> {
        let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        self.cue_text = text.clone();
        self.cue_saved_text = text;
        self.cue_path = Some(p.to_path_buf());
        self.watch.cue = Some((p.to_path_buf(), mtime(p)));
        self.reparse();
        self.timeline.fit_all();
        Ok(())
    }

    fn new_project(&mut self, ctx: &egui::Context) {
        self.stop();
        let poses_path = self.poses_path.clone();
        self.project_path = None;
        self.project = ProjectFile { preset: self.project.preset.clone(), media: vec![], video: Default::default(), ..self.project_saved.clone() };
        self.project_saved = self.project.clone();
        self.cue_path = None;
        self.cue_text = STARTER_SCRIPT.into();
        self.cue_saved_text.clear();
        self.audio = None;
        self.player = None;
        self.time = 0.0;
        // Keep the pose library: it's the same character across videos.
        if let Some(p) = poses_path {
            let _ = self.load_poses(ctx, &p);
        }
        self.reparse();
        self.timeline.fit_all();
    }

    /// A short dump of editable state, printed by `vg screenshot` after a drag or steps.
    pub fn debug_report(&self) -> String {
        let a = &self.project.preset.avatar;
        let c = &self.project.preset.captions;
        let mut out = format!("avatar x={:.4} y={:.4} height={:.4}\n", a.x, a.y, a.height);
        for (tag, s) in &a.poses {
            out += &format!("  pose {tag} x={:.4} y={:.4} height={:.4}\n", s.x, s.y, s.height);
        }
        out += &format!("caption x={:.4} y={:.4} size={:.4} width={:.4}\nwords_dirty={}\n", c.x, c.y, c.size, c.width, self.words_dirty);
        if let Some((p, _, w)) = &self.words {
            out += &format!("words {} ({}): ", p.display(), w.len());
            out += &w.iter().take(8).map(|w| format!("{}@{:.2}", w.word, w.start)).collect::<Vec<_>>().join(" ");
            out.push('\n');
        }
        if let Some(tl) = &self.parsed.timeline {
            out += "events: ";
            out += &tl.events.iter().map(|e| format!("{:.2}", e.t)).collect::<Vec<_>>().join(" ");
            out.push('\n');
        }
        out += &self.cue_text.lines().filter(|l| l.starts_with('@')).collect::<Vec<_>>().join("\n");
        if !self.studio.mix.tracks.is_empty() {
            out += &format!("\n{}", self.studio.report());
        }
        out += &format!("\n{}", timeline::debug(self));
        out
    }

    pub fn palette_select(&mut self, pose: Option<usize>) {
        self.palette.selected = pose.filter(|&p| p < self.poses.poses.len());
    }

    pub fn dirty(&self) -> bool {
        self.cue_text != self.cue_saved_text || self.project != self.project_saved || self.poses_dirty || self.words_dirty || self.studio.dirty()
    }

    // ---- script → timeline -------------------------------------------------

    pub fn reparse(&mut self) {
        let cue_dir = self.cue_dir();
        let (sheet, audio_path, _) = build_timeline(&self.cue_text, &cue_dir, &self.poses, None, &[]);
        // Word timings: reload when @words or the file changes.
        match vg_project::words_path(&self.cue_text, &cue_dir) {
            Some(wp) => {
                let t = mtime(&wp);
                let stale = self.words.as_ref().is_none_or(|(p, m, _)| *p != wp || (*m != t && !self.words_dirty));
                if stale {
                    match vg_core::Transcript::load(&wp) {
                        Ok(tr) => self.words = Some((wp, t, tr.words())),
                        Err(e) => {
                            self.words = Some((wp, t, Vec::new()));
                            self.error(format!("@words: {e:#}"));
                        }
                    }
                }
            }
            None => {
                self.words = None;
                self.words_dirty = false;
            }
        }
        // Load (or drop) the narration when @audio changes.
        if audio_path.as_ref() != self.audio.as_ref().map(|a| &a.path) {
            self.player = None;
            self.audio = None;
            if let Some(p) = &audio_path {
                match audio::AudioInfo::load(p) {
                    Ok(a) => self.audio = Some(a),
                    Err(e) => self.error(format!("narration: {e:#}")),
                }
            }
        }
        let words: &[vg_core::Word] = self.words.as_ref().map(|w| w.2.as_slice()).unwrap_or(&[]);
        let (_, _, tl) = build_timeline(&self.cue_text, &cue_dir, &self.poses, self.audio.as_ref().map(|a| a.duration), words);
        self.parsed.sheet = sheet;
        self.parsed.audio_path = audio_path;
        match tl {
            Ok(tl) => {
                self.parsed.timeline = Some(tl);
                self.parsed.errors.clear();
            }
            Err(errs) => self.parsed.errors = errs,
        }
        // Without a library every tag is "unknown": say the real problem once.
        if self.poses.poses.is_empty() && !self.parsed.errors.is_empty() {
            self.parsed.errors = vec![CueError { line_no: 0, message: "no pose library loaded, so tags can't resolve · File › New pose library from art folder".into() }];
        }
    }

    pub fn duration(&self) -> f64 {
        self.parsed.timeline.as_ref().map(|t| t.duration).unwrap_or(10.0)
    }

    // ---- transport ---------------------------------------------------------

    fn toggle_play(&mut self) {
        if self.playing {
            self.stop();
        } else {
            if self.time >= self.duration() - 1e-3 {
                self.time = 0.0;
            }
            self.playing = true;
            self.last_tick = Some(Instant::now());
            self.start_audio();
        }
    }

    fn stop(&mut self) {
        self.playing = false;
        self.last_tick = None;
        if let Some(p) = &self.player {
            p.pause();
        }
    }

    fn start_audio(&mut self) {
        let Some(info) = &self.audio else { return };
        if self.player.as_ref().map(|p| p.path()) != Some(info.path.as_path()) {
            match audio::Player::new(&info.path) {
                Ok(p) => self.player = Some(p),
                Err(e) => {
                    self.error(format!("audio: {e:#}"));
                    return;
                }
            }
        }
        let vol = self.prefs.out_volume();
        if let Some(p) = &self.player
            && let Err(e) = p.play_from(self.time, vol) {
                self.error(format!("audio: {e:#}"));
            }
    }

    pub fn seek(&mut self, t: f64) {
        self.time = t.clamp(0.0, self.duration());
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
        if let Some(last) = self.last_tick {
            self.time += (now - last).as_secs_f64();
        }
        self.last_tick = Some(now);
        if self.time >= self.duration() {
            self.time = self.duration();
            self.stop();
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(15));
    }

    // ---- hot reload --------------------------------------------------------

    fn poll_files(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        if self.watch.last_check.is_some_and(|t| now - t < std::time::Duration::from_secs(1)) {
            return;
        }
        self.watch.last_check = Some(now);
        // The narration was rewritten (e.g. by the Audio tab): reload its sound and waveform.
        if self.audio.as_ref().is_some_and(|a| a.path.exists() && mtime(&a.path) != a.mtime) {
            let playing = self.playing;
            self.stop();
            self.audio = None;
            self.player = None;
            self.reparse();
            if playing {
                self.toggle_play();
            }
        }
        let poses_changed = self.watch.poses.iter().any(|(p, t)| mtime(p) != *t);
        if poses_changed && !self.poses_dirty
            && let Some(p) = self.poses_path.clone() {
                match self.load_poses(ctx, &p) {
                    Ok(()) => self.info("pose library reloaded"),
                    Err(e) => {
                        self.snapshot_pose_mtimes();
                        self.error(format!("reloading poses: {e:#}"));
                    }
                }
            }
        if let Some(cp) = self.cue_path.clone() {
            let t = mtime(&cp);
            match &self.watch.cue {
                Some((p, old)) if *p == cp && *old != t => {
                    // Changed on disk (e.g. edited in another editor): reload if we have no unsaved edits.
                    if self.cue_text == self.cue_saved_text
                        && let Ok(text) = std::fs::read_to_string(&cp) {
                            self.cue_text = text.clone();
                            self.cue_saved_text = text;
                            self.reparse();
                            self.info("script reloaded from disk");
                        }
                    self.watch.cue = Some((cp, t));
                }
                Some((p, _)) if *p == cp => {}
                _ => self.watch.cue = Some((cp, t)),
            }
        }
    }

    // ---- UI ----------------------------------------------------------------

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let item = |ui: &mut egui::Ui, label: &str, key: &str| ui.add(egui::Button::new(label).shortcut_text(key).min_size(egui::vec2(240.0, 0.0)));
        if item(ui, "New project", "").clicked() {
            self.new_project(&ctx);
        }
        if item(ui, "Open project", "").clicked() {
            let d = rfd::FileDialog::new().add_filter("videogen project", &["vgproj"]).set_directory(self.base());
            dialog::open(self, dialog::Want::OpenProject, d, dialog::How::File);
        }
        ui.menu_button("Recent projects", |ui| {
            let recent: Vec<PathBuf> = self.prefs.recent_projects.iter().filter(|p| p.exists()).take(10).cloned().collect();
            if recent.is_empty() {
                ui.label(egui::RichText::new("none yet").color(theme::MUTED));
            }
            for p in recent {
                let name = p.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let dir = p.parent().map(util::short).unwrap_or_default();
                if ui.add(egui::Button::new(name).shortcut_text(dir).min_size(egui::vec2(320.0, 0.0))).clicked() {
                    prefs::projects::open(self, &ctx, &p);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Manage projects").clicked() {
                self.prefs_win.show_page(prefs::Page::Projects);
                ui.close();
            }
        });
        if item(ui, "Save project", "Ctrl+S").clicked()
            && let Err(e) = self.save_project(false) {
                self.error(format!("{e:#}"));
            }
        if item(ui, "Save project as", "").clicked() {
            self.save_project_as(false);
        }
        ui.separator();
        if item(ui, "Record narration", "Ctrl+R").clicked() {
            self.record.open = true;
        }
        if item(ui, "Suggest poses with an LLM", "Ctrl+G").clicked() {
            self.assist.open = true;
        }
        if item(ui, "Import narration", "").clicked() {
            self.import_audio();
        }
        if item(ui, "Transcribe narration", "Ctrl+T").clicked() {
            self.transcribe.open = true;
        }
        if item(ui, "Open script", "").clicked() {
            let d = rfd::FileDialog::new().add_filter("cue sheet", &["md", "txt"]).set_directory(self.cue_dir());
            dialog::open(self, dialog::Want::OpenScript, d, dialog::How::File);
        }
        if item(ui, "Save script as", "").clicked() {
            self.save_script_as(self.cue_dir());
        }
        ui.separator();
        if item(ui, "Open pose library", "").clicked() {
            let d = rfd::FileDialog::new().add_filter("pose library", &["ron"]).set_directory(self.base());
            dialog::open(self, dialog::Want::OpenPoseLibrary, d, dialog::How::File);
        }
        if item(ui, "New pose library from art folder", "").clicked() {
            self.create_pose_library();
        }
        if item(ui, "Import new poses from a folder", "").clicked() {
            self.import_poses();
        }
        if ui.add_enabled(self.poses_dirty, egui::Button::new("Save pose library").min_size(egui::vec2(240.0, 0.0))).clicked()
            && let Err(e) = self.save_poses() {
                self.error(format!("{e:#}"));
            }
    }

    fn can_export(&self) -> bool {
        self.parsed.timeline.is_some() && self.parsed.errors.is_empty() && !self.poses.poses.is_empty()
    }

    /// The header: wordmark, project, menus, preset toggle and the main actions.
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            // Wordmark with a little caption glow.
            let galley = ui.painter().layout_no_wrap("VIDEOGEN".into(), egui::FontId::proportional(15.0), theme::WHITE);
            let (rect, _) = ui.allocate_exact_size(galley.size() + egui::vec2(34.0, 0.0), egui::Sense::hover());
            let at = rect.left_center() + egui::vec2(0.0, -galley.size().y / 2.0);
            let mut job = egui::text::LayoutJob::default();
            job.append("VIDEOGEN", 0.0, egui::TextFormat { font_id: egui::FontId::proportional(15.0), color: theme::WHITE, extra_letter_spacing: 3.2, ..Default::default() });
            let tracked = ui.painter().layout_job(job);
            ui.painter().galley(at + egui::vec2(1.0, 0.0), tracked.clone(), egui::Color32::from_white_alpha(90));
            ui.painter().galley(at, tracked, theme::WHITE);

            ui.label(egui::RichText::new(self.project_name()).color(theme::TEXT2));
            if self.dirty() {
                ui.label(theme::caption("unsaved").color(theme::MUTED)).on_hover_text("Ctrl+S saves the project, script and pose tweaks.");
            }
            ui.add_space(6.0);
            ui.menu_button(theme::caption("File"), |ui| self.file_menu(ui));
            ui.menu_button(theme::caption("Settings"), |ui| {
                let item = |ui: &mut egui::Ui, label: &str, key: &str| ui.add(egui::Button::new(label).shortcut_text(key).min_size(egui::vec2(220.0, 0.0)));
                for (page, label, key) in [(prefs::Page::General, "Preferences", "Ctrl+,"), (prefs::Page::Projects, "Projects", ""), (prefs::Page::Transcripts, "Transcripts", ""), (prefs::Page::Storage, "Storage & cache", "")] {
                    if item(ui, label, key).clicked() {
                        self.prefs_win.show_page(page);
                        ui.close();
                    }
                }
            });
            ui.menu_button(theme::caption("View"), |ui| {
                if ui.add(egui::Button::new("Layout, motion and captions").shortcut_text("Ctrl+L").selected(self.settings_open)).clicked() {
                    self.settings_open = !self.settings_open;
                }
                if ui.button("Fit timeline").clicked() {
                    self.timeline.fit_all();
                }
                ui.checkbox(&mut self.prefs.show_guides, "Shorts safe area");
            });
            ui.add_space(10.0);
            let before = self.prefs.view;
            theme::segmented(ui, &mut self.prefs.view, &[(View::Video, "Video"), (View::Audio, "Audio")]);
            if before != self.prefs.view {
                self.switch_view();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let can_export = self.can_export();
                if theme::tool_button_enabled(ui, "Export", Some("Ctrl+E"), self.export.open, can_export)
                    .on_hover_text(if can_export { "Render the avatar track and captions." } else { "Error · fix the script first. The preview still works." })
                    .clicked()
                {
                    self.export.open = !self.export.open;
                }
                if theme::tool_button(ui, "Layout", Some("Ctrl+L"), self.settings_open).on_hover_text("Canvas, avatar placement, motion and captions.").clicked() {
                    self.settings_open = !self.settings_open;
                }
                if theme::tool_button(ui, "Record", Some("Ctrl+R"), self.record.open).clicked() {
                    self.record.open = !self.record.open;
                }
                if let Some(rec) = self.record.recording() {
                    let text = if rec.armed() { format!("REC {}", vg_core::format_time(rec.elapsed())) } else { "REC …".into() };
                    let r = ui.label(egui::RichText::new(text).monospace().size(12.0).color(theme::WHITE));
                    ui.painter().rect_filled(egui::Rect::from_center_size(r.rect.left_center() - egui::vec2(8.0, 0.0), egui::vec2(6.0, 6.0)), 0.0, theme::WHITE);
                    ui.add_space(16.0);
                }
                if theme::tool_button(ui, "Transcribe", Some("Ctrl+T"), self.transcribe.open).on_hover_text("Turn your recorded narration into a timed, pose-tagged script.").clicked() {
                    self.transcribe.open = !self.transcribe.open;
                }
                ui.add_space(12.0);
                let mut name = self.project.preset.name.clone();
                if theme::segmented(ui, &mut name, &[("long".to_string(), "Long 16:9"), ("shorts".to_string(), "Shorts 9:16")])
                    && let Some(p) = Preset::builtin().into_iter().find(|p| p.name == name) {
                        self.project.preset = p;
                    }
            });
        });
    }

    /// The status line: messages on the left, facts on the right.
    fn status_line(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            let small = |t: String, c: egui::Color32| egui::RichText::new(t).size(11.0).color(c);
            match &self.status {
                Some(s) if s.error => {
                    let r = ui.add(egui::Label::new(small(format!("Error · {}", s.text), theme::WHITE)).truncate().sense(egui::Sense::click()));
                    theme::dashed_rect(ui.painter(), r.rect.expand2(egui::vec2(6.0, 2.0)), egui::Stroke::new(1.0_f32, theme::BORDER));
                    if r.on_hover_text("Click to dismiss.").clicked() {
                        self.status = None;
                    }
                }
                Some(s) if s.at.elapsed().as_secs() < 10 => {
                    ui.add(egui::Label::new(small(s.text.clone(), theme::TEXT2)).truncate());
                }
                _ => {
                    let tags = self.parsed.timeline.as_ref().map(|t| t.events.len()).unwrap_or(0);
                    let lines = self.parsed.timeline.as_ref().map(|t| t.spans.len()).unwrap_or(0);
                    let narration = self.audio.as_ref().map(|a| file_name(&a.path)).unwrap_or_else(|| "no narration".into());
                    let words = if self.words.as_ref().is_some_and(|w| !w.2.is_empty()) { " · word timings" } else { "" };
                    ui.label(small(format!("{lines} lines · {tags} tags · {narration}{words}"), theme::MUTED));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let p = &self.project.preset;
                ui.label(small(format!("{} · {}×{} · {} fps · {} poses", p.name, p.width, p.height, p.fps, self.poses.poses.len()), theme::MUTED));
            });
        });
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let (layout, export, transcribe, record) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::L),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::E),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::T),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::R),
            )
        });
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma)) {
            self.prefs_win.open = !self.prefs_win.open;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::G)) {
            self.assist.open = !self.assist.open;
        }
        if record {
            record::shortcut(self, ctx);
        }
        let (v1, v2) = ctx.input_mut(|i| (i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num1), i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num2)));
        if v1 || v2 {
            let want = if v1 { View::Video } else { View::Audio };
            if want != self.prefs.view {
                self.prefs.view = want;
                self.switch_view();
            }
        }
        if layout {
            self.settings_open = !self.settings_open;
        }
        if export && self.can_export() {
            self.export.open = !self.export.open;
        }
        if transcribe {
            self.transcribe.open = !self.transcribe.open;
        }
        let save = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S));
        if save
            && let Err(e) = self.save_project(false) {
                self.error(format!("{e:#}"));
            }
        // The Audio tab has its own transport and keys.
        if self.prefs.view == View::Audio || typing {
            return;
        }
        let none = egui::Modifiers::NONE;
        let (play, home, back, fwd) = ctx.input_mut(|i| {
            (
                i.consume_key(none, egui::Key::Space),
                i.consume_key(none, egui::Key::Home),
                i.consume_key(none, egui::Key::ArrowLeft),
                i.consume_key(none, egui::Key::ArrowRight),
            )
        });
        if play {
            self.toggle_play();
        }
        if home {
            self.seek(0.0);
        }
        if back {
            self.seek(self.time - 1.0);
        }
        if fwd {
            self.seek(self.time + 1.0);
        }
        // Editor keys: J/K/L, edit points, frames, the end.
        let (j, k, l, up, down, prev_f, next_f, end) = ctx.input_mut(|i| {
            (
                i.consume_key(none, egui::Key::J),
                i.consume_key(none, egui::Key::K),
                i.consume_key(none, egui::Key::L),
                i.consume_key(none, egui::Key::ArrowUp),
                i.consume_key(none, egui::Key::ArrowDown),
                i.consume_key(none, egui::Key::Comma),
                i.consume_key(none, egui::Key::Period),
                i.consume_key(none, egui::Key::End),
            )
        });
        if j {
            self.seek(self.time - 5.0);
        }
        if k && self.playing {
            self.stop();
        }
        if l && !self.playing {
            self.toggle_play();
        }
        if up || down {
            // Edit points: pose changes and line starts.
            let mut pts: Vec<f64> = self.parsed.timeline.as_ref().map(|tl| tl.events.iter().map(|e| e.t).chain(tl.spans.iter().map(|s| s.start)).collect()).unwrap_or_default();
            pts.push(0.0);
            pts.push(self.duration());
            pts.sort_by(f64::total_cmp);
            let t = self.time;
            let target = if down { pts.iter().copied().find(|p| *p > t + 1e-3) } else { pts.iter().rev().copied().find(|p| *p < t - 1e-3) };
            if let Some(p) = target {
                self.seek(p);
            }
        }
        let frame = 1.0 / self.project.preset.fps.max(1) as f64;
        if prev_f {
            self.seek(((self.time / frame).round() - 1.0) * frame);
        }
        if next_f {
            self.seek(((self.time / frame).round() + 1.0) * frame);
        }
        if end {
            self.seek(self.duration());
        }
        // The timeline's own keys (split, delete, mute, undo…), in the section last clicked.
        timeline::keys(self, ctx);
        // Keyboard pose tagging: digits 1-9 pick the first nine poses.
        let digit = ctx.input(|i| {
            [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3, egui::Key::Num4, egui::Key::Num5, egui::Key::Num6, egui::Key::Num7, egui::Key::Num8, egui::Key::Num9]
                .iter()
                .position(|k| i.key_pressed(*k) && i.modifiers.is_none())
        });
        if let Some(d) = digit
            && d < self.poses.poses.len() {
                self.insert_pose_tag(ctx, d);
            }
    }

    /// Insert `[tag]` at the script cursor, or `[tag@time]` on the playhead's line in live mode.
    pub fn insert_pose_tag(&mut self, ctx: &egui::Context, pose: usize) {
        let short = self.poses.poses[pose].short_name().to_string();
        if self.prefs.live_tag {
            let line_no = self
                .parsed
                .timeline
                .as_ref()
                .and_then(|tl| tl.spans.iter().rev().find(|s| s.start <= self.time).map(|s| s.line_no));
            script::insert_timed_tag(&mut self.cue_text, line_no, &short, self.time);
        } else {
            self.script.insert_at_cursor(ctx, &mut self.cue_text, &format!("[{short}]"));
        }
        self.reparse();
    }

    /// Only one view plays at a time.
    fn switch_view(&mut self) {
        self.stop();
        self.studio.stop();
        if self.prefs.view == View::Video && studio::refresh_narration(self) {
            self.info("narration updated from the Audio tab");
        }
    }

    fn close_guard(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty() && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }
        if self.confirm_close {
            let modal = egui::Modal::new(egui::Id::new("confirm_close")).show(ctx, |ui| {
                ui.label(theme::caption("Unsaved changes").color(theme::WHITE));
                ui.label("Save the project (script, pose tweaks and settings) before closing?");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save and close").clicked() {
                        match self.save_project(true) {
                            Ok(true) if !self.dirty() => {
                                self.allow_close = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            Ok(_) => {}
                            Err(e) => self.error(format!("{e:#}")),
                        }
                        self.confirm_close = false;
                    }
                    if ui.button("Discard").clicked() {
                        self.allow_close = true;
                        self.confirm_close = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_close = false;
                    }
                });
            });
            if modal.should_close() {
                self.confirm_close = false;
            }
        }
    }

    fn update_title(&self, ctx: &egui::Context) {
        let title = format!("videogen · {}{}", self.project_name(), if self.dirty() { " · unsaved" } else { "" });
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    fn project_name(&self) -> String {
        self.project_path.as_deref().map(file_name).unwrap_or_else(|| "untitled".into())
    }
}

impl eframe::App for VgApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui_root(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_PREFS, &self.prefs);
        if let Some(p) = &self.project_path {
            eframe::set_value(storage, STORAGE_LAST_PROJECT, &canon(p));
        }
    }
}

impl VgApp {
    pub fn ui_root(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.tick(&ctx);
        dialog::poll(self, &ctx);
        self.poll_files(&ctx);
        self.handle_shortcuts(&ctx);
        self.close_guard(&ctx);
        self.update_title(&ctx);

        let bar = egui::Frame::new().fill(theme::BLACK).inner_margin(egui::Margin::symmetric(14, 0));
        egui::Panel::top("header").exact_size(46.0).frame(bar).show_inside(ui, |ui| self.header(ui));
        egui::Panel::bottom("status").exact_size(26.0).frame(bar).show_inside(ui, |ui| self.status_line(ui));
        if self.prefs.view == View::Audio {
            studio::show(self, ui);
        } else {
            studio::background(self, &ctx);
            egui::Panel::bottom("timeline").resizable(true).default_size(230.0).min_size(190.0).frame(theme::panel_frame()).show_inside(ui, |ui| {
                timeline::show(self, ui);
            });
            egui::Panel::left("poses").resizable(true).default_size(272.0).min_size(190.0).frame(theme::panel_frame()).show_inside(ui, |ui| {
                media::sidebar(self, ui);
            });
            egui::Panel::right("script").resizable(true).default_size(480.0).min_size(300.0).frame(theme::panel_frame()).show_inside(ui, |ui| {
                script::show(self, ui);
            });
            egui::CentralPanel::default().frame(theme::panel_frame()).show_inside(ui, |ui| {
                preview::show(self, ui);
            });
        }
        settings::window(self, &ctx);
        export::window(self, &ctx);
        transcribe::window(self, &ctx);
        record::window(self, &ctx);
        assist::window(self, &ctx);
        prefs::window(self, &ctx);
        recordings::window(self, &ctx);
    }
}

// ---- helpers ---------------------------------------------------------------

fn with_ext(p: PathBuf, ext: &str) -> PathBuf {
    if p.extension().is_some() { p } else { p.with_extension(ext) }
}

/// Set (or add) an `@key value` directive line at the top of a script.
fn set_directive(text: &mut String, key: &str, value: &str) {
    let prefix = format!("@{key}");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if let Some(l) = lines.iter_mut().find(|l| l.split_whitespace().next() == Some(prefix.as_str())) {
        *l = format!("{prefix} {value}");
    } else {
        // After leading comments.
        let at = lines.iter().position(|l| !l.trim_start().starts_with('#')).unwrap_or(lines.len());
        lines.insert(at, format!("{prefix} {value}"));
    }
    *text = lines.join("\n") + "\n";
}

fn remove_directive(text: &mut String, key: &str) {
    let prefix = format!("@{key}");
    let kept: Vec<&str> = text.lines().filter(|l| l.split_whitespace().next() != Some(prefix.as_str())).collect();
    *text = kept.join("\n") + "\n";
}
