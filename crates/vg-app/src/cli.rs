use crate::config::Config;
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use vg_core::{Action, PoseSet, Preset, format_time};
use vg_media::OutputFormat;
use vg_project::{ExportJob, ProjectFile, TranscribeOptions, build_timeline, resolve, words_path};

#[derive(Parser)]
#[command(name = "vg", version, about = "Pose-avatar overlays for self-narrated videos. Run without a command to open the GUI.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
    /// Project to open in the GUI.
    project: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage the pose library.
    Poses {
        #[command(subcommand)]
        command: PosesCmd,
    },
    /// List or bulk-edit transcripts (`*.words.json`) and the scripts that use them.
    Transcripts {
        #[command(subcommand)]
        command: TranscriptsCmd,
    },
    /// Record narration from the microphone to a WAV file (Enter stops).
    Record {
        /// Output file (default: the next free narration-take<N>.wav here).
        output: Option<PathBuf>,
        /// Input device name (see --list; default: the system default input).
        #[arg(long)]
        device: Option<String>,
        /// List input devices and exit.
        #[arg(long)]
        list: bool,
        /// Stop after this many seconds instead of waiting for Enter.
        #[arg(long)]
        seconds: Option<f64>,
    },
    /// Clean up a narration: run the voice chain and master it (writes a WAV).
    Mix {
        /// A recording, or a `.vgmix` edit saved by the GUI.
        input: PathBuf,
        /// Output (default: <input>.mix.wav).
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Effect preset for a recording: condenser, gentle, punchy, off.
        #[arg(long, default_value = "condenser")]
        preset: String,
        /// Shorten pauses longer than this many seconds.
        #[arg(long)]
        tighten: Option<f64>,
        /// Loudness target in LUFS.
        #[arg(long, default_value_t = -14.0, allow_hyphen_values = true)]
        lufs: f32,
        /// Turn effects off by name: highpass, denoise, gate, eq, deesser, compressor.
        #[arg(long, value_delimiter = ',')]
        off: Vec<String>,
    },
    /// Ask an LLM for pose tags for a script (prints them; --apply writes them).
    Suggest {
        /// A cue sheet (.md) or a project (.vgproj).
        input: PathBuf,
        #[arg(long)]
        poses: Option<PathBuf>,
        /// OpenAI-compatible server (llama.cpp, vLLM, Ollama…). Defaults to config `llm_url` / $VG_LLM.
        #[arg(long)]
        llm: Option<String>,
        /// Replace every pose tag instead of filling in untagged lines.
        #[arg(long)]
        replace: bool,
        /// Extra direction, e.g. "more reactions, fewer waves".
        #[arg(long, default_value = "")]
        style: String,
        /// Lines per request.
        #[arg(long, default_value_t = 20)]
        batch: usize,
        /// Write the tags into the script (or to --output).
        #[arg(long)]
        apply: bool,
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Parse a cue sheet and print the resolved pose timeline.
    Check {
        /// A cue sheet (.md/.txt) or a project (.vgproj).
        input: PathBuf,
        #[command(flatten)]
        opts: SourceOpts,
    },
    /// Render the avatar track to a video file.
    Render {
        /// A cue sheet (.md/.txt) or a project (.vgproj).
        input: PathBuf,
        #[command(flatten)]
        opts: SourceOpts,
        /// Output format: prores, webm, png, preview.
        #[arg(long, short)]
        format: Option<String>,
        /// Output path (default: out/<format default name> next to the input).
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Transcribe narration with local Whisper and write a cue sheet with suggested poses.
    Transcribe {
        audio: PathBuf,
        /// Cue sheet to write (default: <audio stem>.md next to the audio).
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Pose library for tag suggestions (default: avatar/poses.ron if present).
        #[arg(long)]
        poses: Option<PathBuf>,
        /// Whisper model: large-v3-turbo (default), medium.en, small.en, …
        #[arg(long)]
        model: Option<String>,
        /// Spoken language code (default: auto-detect).
        #[arg(long)]
        language: Option<String>,
        /// Don't suggest pose tags.
        #[arg(long)]
        no_tags: bool,
        /// Overwrite an existing cue sheet.
        #[arg(long)]
        force: bool,
    },
    /// Render the GUI offscreen to a PNG (for docs / testing; opens no window).
    #[command(hide = true)]
    Screenshot(crate::gui::shot::ShotArgs),
}

#[derive(clap::Args)]
struct SourceOpts {
    /// Pose library (default: avatar/poses.ron, or the project's).
    #[arg(long)]
    poses: Option<PathBuf>,
    /// `shorts`, `long`, or a preset .ron file (default: shorts, or the project's).
    #[arg(long)]
    preset: Option<String>,
    /// Override the length in seconds (default: @duration, the audio, or the last cue + 4 s).
    #[arg(long)]
    duration: Option<f64>,
}

#[derive(Subcommand)]
enum TranscriptsCmd {
    /// List the transcripts in the folders (default: the working directory).
    List {
        #[arg(default_value = ".")]
        dirs: Vec<PathBuf>,
    },
    /// Replace a word or phrase in transcripts (and the scripts that use them).
    Replace {
        find: String,
        with: String,
        /// Transcript files (`*.words.json`), or folders to take every one from.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Also match inside words (one-word searches only).
        #[arg(long)]
        partial: bool,
        #[arg(long)]
        match_case: bool,
        /// Leave the scripts alone.
        #[arg(long)]
        no_scripts: bool,
        /// Show what would change without writing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove filler words (whole words) from transcripts and their scripts.
    Fillers {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        #[arg(long)]
        words: Option<String>,
        #[arg(long)]
        no_scripts: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Move every word (and the scripts' line times) by SECONDS.
    Shift {
        #[arg(allow_hyphen_values = true)]
        seconds: f64,
        #[arg(required = true)]
        files: Vec<PathBuf>,
        #[arg(long)]
        no_scripts: bool,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum PosesCmd {
    /// Build a pose library from a folder of PNGs (tags come from file names).
    Init {
        art_dir: PathBuf,
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// List pose tags and aliases.
    List {
        #[arg(long)]
        poses: Option<PathBuf>,
        /// Print the library as JSON (tags, aliases, descriptions, when each fits).
        #[arg(long)]
        json: bool,
    },
    /// Add a folder of new pose PNGs: they're numbered on from the art folder,
    /// moved into it and appended to the library (existing poses untouched).
    Import {
        folder: PathBuf,
        #[arg(long)]
        poses: Option<PathBuf>,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load();
    match cli.command {
        None => crate::gui::run(cli.project),
        Some(Cmd::Transcribe { audio, output, poses, model, language, no_tags, force }) => {
            let model = model.unwrap_or_else(|| cfg.whisper_model.clone());
            let (_, default_out) = vg_project::transcript_paths(&audio);
            let output = output.unwrap_or(default_out);
            if output.exists() && !force {
                bail!("{} exists (use --force to overwrite, or -o to pick another name)", output.display());
            }
            let poses = if no_tags {
                None
            } else {
                let path = poses.unwrap_or_else(|| PathBuf::from(&cfg.poses));
                if path.exists() { Some(PoseSet::load(&path)?) } else { None }
            };
            let opts = TranscribeOptions { model: model.clone(), language };
            let cancel = Arc::new(AtomicBool::new(false));
            let started = std::time::Instant::now();
            let t = vg_project::transcribe(&audio, &opts, &cancel, |done, total| {
                if total > 0.0 {
                    eprint!("\r{:>3.0}%  {} / {}", 100.0 * done / total, format_time(done), format_time(total));
                }
            }, |line| eprintln!("{line}"))?
            .context("cancelled")?;
            eprintln!();
            let cue_dir = output.parent().map(Path::to_path_buf).unwrap_or_default();
            let text = vg_project::cue_sheet_for(&t, &audio, &cue_dir, poses.as_ref(), &model)?;
            std::fs::write(&output, &text).with_context(|| format!("writing {}", output.display()))?;
            let n = text.lines().filter(|l| l.starts_with('[')).count();
            println!("wrote {} ({n} lines, {:.1}s)", output.display(), started.elapsed().as_secs_f64());
            Ok(())
        }
        Some(Cmd::Record { output, device, list, seconds }) => {
            if list {
                for (i, name) in crate::recorder::input_names()?.iter().enumerate() {
                    println!("{name}{}", if i == 0 { "  (default)" } else { "" });
                }
                return Ok(());
            }
            let output = output.unwrap_or_else(|| crate::recorder::next_take_path(Path::new("."), "narration"));
            let rec = crate::recorder::Recording::start(device.as_deref(), &output, true)?;
            eprintln!("recording {} from {} at {} Hz · {}", output.display(), rec.device, rec.rate, if seconds.is_some() { "stops on its own" } else { "press Enter to stop" });
            let (tx, rx) = std::sync::mpsc::channel();
            if seconds.is_none() {
                std::thread::spawn(move || {
                    let _ = std::io::stdin().read_line(&mut String::new());
                    let _ = tx.send(());
                });
            }
            loop {
                if rx.try_recv().is_ok() || seconds.is_some_and(|s| rec.elapsed() >= s) {
                    break;
                }
                if let Some(e) = rec.shared().error() {
                    bail!("input error: {e}");
                }
                let peak = rec.shared().take_peak();
                let db = 20.0 * peak.max(1e-6).log10();
                let bars = ((db + 60.0) / 60.0 * 30.0).clamp(0.0, 30.0) as usize;
                eprint!("\r{}  {:<30} {:>6.1} dBFS{}", format_time(rec.elapsed()), "#".repeat(bars), db, if rec.shared().clipped() { "  CLIPPED" } else { "" });
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let take = rec.finish()?;
            eprintln!();
            println!("wrote {} ({:.1}s)", take.path.display(), take.duration);
            Ok(())
        }
        Some(Cmd::Mix { input, output, preset, tighten, lufs, off }) => {
            use vg_audio::{Cache, Mix, Preset};
            let started = std::time::Instant::now();
            let base = input.parent().map(Path::to_path_buf).unwrap_or_default();
            let mut mix = if input.extension().is_some_and(|e| e == "vgmix") {
                Mix::load(&input)?
            } else {
                let p = Preset::parse(&preset).with_context(|| format!("unknown preset {preset:?} (condenser, gentle, punchy, off)"))?;
                let dur = vg_media::probe_duration(&input)?;
                let mut m = Mix::for_recording(input.file_name().unwrap().into(), dur);
                m.tracks[0].effects = p.effects();
                m
            };
            mix.master.loudness = Some(lufs);
            for e in mix.tracks.iter_mut().flat_map(|t| t.effects.iter_mut()) {
                let key = e.fx.name().to_lowercase().replace(['-', ' '], "");
                if off.iter().any(|o| key.starts_with(&o.to_lowercase().replace(['-', ' '], "")) || (o == "denoise" && key == "noisereduction")) {
                    e.on = false;
                }
            }
            let mut sources = Cache::default();
            if let Some(max) = tighten {
                let src = sources.source(&base.join(&mix.tracks[0].source))?;
                let buf = vg_audio::render::assemble(&mix.tracks[0], &src);
                let n = vg_audio::edit::tighten(&mut mix.tracks[0], &buf, vg_audio::RATE as f32, max);
                eprintln!("shortened {n} pauses");
            }
            let r = vg_audio::render(&mix, &base, &mut sources, &|| false)?.context("cancelled")?;
            let output = output.unwrap_or_else(|| input.with_extension("mix.wav"));
            vg_audio::write_wav(&output, &r.wet)?;
            let s = &r.stats;
            let f = |l: Option<f32>| l.map(|l| format!("{l:.1} LUFS")).unwrap_or_else(|| "silent".into());
            println!(
                "wrote {} ({}) in {:.1}s · in {} → out {}, peak {:.1} dBFS, compression up to {:.1} dB",
                output.display(),
                format_time(r.wet.len() as f64 / vg_audio::RATE as f64),
                started.elapsed().as_secs_f64(),
                f(s.input_lufs),
                f(s.output_lufs),
                s.output_peak_db,
                s.compression_db
            );
            Ok(())
        }
        Some(Cmd::Screenshot(a)) => crate::gui::shot::screenshot(&a),
        Some(Cmd::Poses { command: PosesCmd::Init { art_dir, output, force } }) => {
            let output = output.unwrap_or_else(|| PathBuf::from(&cfg.poses));
            if output.exists() && !force {
                bail!("{} exists (use --force to overwrite)", output.display());
            }
            if let Some(dir) = output.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let set = PoseSet::generate(&art_dir, &output)?;
            set.save(&output)?;
            println!("wrote {} poses to {}", set.poses.len(), output.display());
            print_poses(&set);
            Ok(())
        }
        Some(Cmd::Poses { command: PosesCmd::Import { folder, poses } }) => {
            let poses = poses.unwrap_or_else(|| PathBuf::from(&cfg.poses));
            let mut set = PoseSet::load(&poses)?;
            let added = set.import_folder(&folder)?;
            set.save(&poses)?;
            for i in &added {
                let p = &set.poses[*i];
                println!("{:<32} {:<44} aliases: {}", p.tag, p.file.display(), p.aliases.join(", "));
            }
            println!("added {} poses to {} ({} in all)", added.len(), poses.display(), set.poses.len());
            Ok(())
        }
        Some(Cmd::Suggest { input, poses, llm, replace, style, batch, apply, output }) => {
            let llm = llm.unwrap_or_else(|| cfg.llm_url.clone());
            let (cue_path, pose_path) = if input.extension().is_some_and(|e| e == "vgproj") {
                let pf = ProjectFile::load(&input)?;
                let base = input.parent().unwrap_or(Path::new(".")).to_path_buf();
                (resolve(&base, &pf.cues), poses.unwrap_or_else(|| resolve(&base, &pf.poses)))
            } else {
                (input.clone(), poses.unwrap_or_else(|| PathBuf::from(&cfg.poses)))
            };
            let set = PoseSet::load(&pose_path)?;
            let script = std::fs::read_to_string(&cue_path).with_context(|| format!("reading {}", cue_path.display()))?;
            let mut client = vg_llm::Client::new(&llm);
            let described = set.poses.iter().filter(|p| !p.description.is_empty()).count();
            eprintln!("asking {} at {} ({described}/{} poses described)", client.model()?, client.base, set.poses.len());
            let mode = if replace { vg_llm::Mode::Replace } else { vg_llm::Mode::Untagged };
            let opts = vg_llm::SuggestOptions { mode, batch, style, ..Default::default() };
            let started = std::time::Instant::now();
            let (sugg, notes) = vg_llm::suggest(&mut client, &script, &set, &opts, &mut |d, t| eprint!("\rbatch {}/{t}", (d + 1).min(t)), &|| false)?;
            eprintln!("\r{} suggestions in {:.1}s ({:?} output)", sugg.len(), started.elapsed().as_secs_f64(), client.structure);
            for s in &sugg {
                let at = if s.word == 0 { "start".to_string() } else { format!("at \"{}\"", s.word_text) };
                println!("line {:>3}  {:<20} {:<16} {}", s.line_no, set.poses[s.pose].tag, at, s.reason);
            }
            for n in &notes {
                eprintln!("{} {}", if n.fixed { "fixed:" } else { "dropped:" }, n.text);
            }
            if apply {
                let out = vg_llm::suggest::apply(&script, &sugg, &set, mode)?;
                let dest = output.unwrap_or(cue_path);
                std::fs::write(&dest, out).with_context(|| format!("writing {}", dest.display()))?;
                println!("wrote {}", dest.display());
            }
            Ok(())
        }
        Some(Cmd::Transcripts { command }) => transcripts_cmd(command, &cfg),
        Some(Cmd::Poses { command: PosesCmd::List { poses, json } }) => {
            let poses = poses.unwrap_or_else(|| PathBuf::from(&cfg.poses));
            let set = PoseSet::load(&poses)?;
            if json {
                println!("{}", set.summary_json());
            } else {
                print_poses(&set);
            }
            Ok(())
        }
        Some(Cmd::Check { input, opts }) => {
            let src = Source::load(&input, &opts, Path::new(&cfg.poses))?;
            let tl = src.timeline()?;
            println!("{} at {}x{} {} fps, {}", src.preset.name, src.preset.width, src.preset.height, src.preset.fps, format_time(tl.duration));
            println!("{:>9}  start: {}", format_time(0.0), src.poses.poses[tl.initial_pose].tag);
            for ev in &tl.events {
                let what = match ev.action {
                    Action::Pose(p) => src.poses.poses[p].tag.clone(),
                    Action::Hide => "(hide)".into(),
                    Action::Show => "(show)".into(),
                };
                println!("{:>9}  {what}  (line {})", format_time(ev.t), ev.line_no);
            }
            Ok(())
        }
        Some(Cmd::Render { input, opts, format, output }) => {
            let src = Source::load(&input, &opts, Path::new(&cfg.poses))?;
            let timeline = src.timeline()?;
            let format = match format {
                Some(f) => OutputFormat::parse(&f).with_context(|| format!("unknown format `{f}` (prores, webm, png, preview)"))?,
                None => src.export_format,
            };
            let output = match (output, &src.export_output) {
                (Some(o), _) => o,
                (None, Some(o)) if format == src.export_format => o.clone(),
                (None, _) => src.base.join("out").join(format.default_name()),
            };
            let job = ExportJob {
                timeline,
                preset: src.preset,
                poses: src.poses,
                format,
                output,
                audio: src.audio,
                background: src.background,
                words: src.words,
                video: src.video,
            };
            let gpu = vg_render::Gpu::headless()?;
            let started = std::time::Instant::now();
            let cancel = Arc::new(AtomicBool::new(false));
            let mut last_pct = u64::MAX;
            let out = vg_project::export(&gpu.device, &gpu.queue, &job, &cancel, |done, total| {
                let pct = done * 100 / total;
                if pct != last_pct && (pct % 10 == 0 || done == total) {
                    eprint!("\r{pct:>3}%  frame {done}/{total}");
                    last_pct = pct;
                }
            })?;
            eprintln!();
            if let Some(out) = out {
                println!("wrote {} in {:.1}s", out.display(), started.elapsed().as_secs_f64());
            }
            Ok(())
        }
    }
}

fn print_poses(set: &PoseSet) {
    for p in &set.poses {
        if p.aliases.is_empty() {
            println!("  {}", p.tag);
        } else {
            println!("  {}  ({})", p.tag, p.aliases.join(", "));
        }
    }
}

/// A cue sheet plus its surroundings, from either a bare sheet or a project file.
struct Source {
    base: PathBuf,
    cue_path: PathBuf,
    text: String,
    poses: PoseSet,
    preset: Preset,
    duration: Option<f64>,
    words: Vec<vg_core::Word>,
    /// `--duration` beats `@duration`, which beats the audio length.
    forced_duration: Option<f64>,
    audio: Option<PathBuf>,
    export_format: OutputFormat,
    export_output: Option<PathBuf>,
    background: u32,
    /// The project's video track, sources resolved (shown in the preview MP4).
    video: vg_project::video::VideoTrack,
}

impl Source {
    fn load(input: &Path, opts: &SourceOpts, default_poses: &Path) -> Result<Self> {
        let is_project = input.extension().is_some_and(|e| e == "vgproj");
        let (base, cue_path, poses_path, mut preset, export, video) = if is_project {
            let pf = ProjectFile::load(input)?;
            let base = input.parent().unwrap_or(Path::new(".")).to_path_buf();
            let video = vg_project::resolve_track(&base, &pf.video);
            (base.clone(), resolve(&base, &pf.cues), resolve(&base, &pf.poses), pf.preset, Some(pf.export), video)
        } else {
            let base = input.parent().unwrap_or(Path::new(".")).to_path_buf();
            (base, input.to_path_buf(), default_poses.to_path_buf(), Preset::shorts(), None, Default::default())
        };
        let poses_path = opts.poses.clone().unwrap_or(poses_path);
        if let Some(p) = &opts.preset {
            preset = Preset::from_name_or_path(p)?;
        }
        let poses = PoseSet::load(&poses_path).with_context(|| "loading the pose library (create one with `vg poses init <art dir>`)")?;
        let text = std::fs::read_to_string(&cue_path).with_context(|| format!("reading {}", cue_path.display()))?;
        let cue_dir = cue_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let (_, audio, _) = build_timeline(&text, &cue_dir, &poses, None, &[]);
        let words = match words_path(&text, &cue_dir) {
            Some(p) => vg_core::Transcript::load(&p)?.words(),
            None => Vec::new(),
        };
        let duration = match (opts.duration, &audio) {
            (Some(d), _) => Some(d),
            (None, Some(a)) => Some(vg_media::probe_duration(a)?),
            (None, None) => None,
        };
        Ok(Source {
            export_output: export.as_ref().map(|e| resolve(&base, &e.output)),
            base,
            cue_path,
            text,
            poses,
            preset,
            duration,
            words,
            forced_duration: opts.duration,
            audio,
            export_format: export.as_ref().map(|e| e.format).unwrap_or(OutputFormat::ProRes4444),
            background: export.as_ref().map(|e| e.background).unwrap_or(0x1f7a3a),
            video,
        })
    }

    fn timeline(&self) -> Result<vg_core::Timeline> {
        let cue_dir = self.cue_path.parent().unwrap_or(Path::new("."));
        let (_, _, tl) = build_timeline(&self.text, cue_dir, &self.poses, self.duration, &self.words);
        let mut tl = tl.map_err(|errs| {
            let list: Vec<String> = errs.iter().map(|e| format!("  {}:{}", self.cue_path.display(), e)).collect();
            anyhow::anyhow!("the cue sheet has problems:\n{}", list.join("\n"))
        })?;
        if let Some(d) = self.forced_duration {
            tl.duration = d;
        }
        Ok(tl)
    }
}

fn transcripts_cmd(command: TranscriptsCmd, cfg: &Config) -> Result<()> {
    use vg_project::transcripts::{self as tr, Entry};
    // Files given one by one, or every transcript in the given folders.
    let pick = |files: &[PathBuf]| -> Vec<Entry> {
        let dirs: Vec<PathBuf> = files.iter().map(|f| if f.is_dir() { f.clone() } else { f.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf() }).collect();
        let want: Vec<PathBuf> = files.iter().filter(|f| !f.is_dir()).filter_map(|f| std::fs::canonicalize(f).ok()).collect();
        tr::scan(&dirs).into_iter().filter(|e| files.iter().any(|f| f.is_dir() && std::fs::canonicalize(f).ok() == e.path.parent().and_then(|p| std::fs::canonicalize(p).ok())) || std::fs::canonicalize(&e.path).is_ok_and(|c| want.contains(&c))).collect()
    };
    enum Op {
        Replace(String, String, tr::MatchOpts),
        Fillers(Vec<String>),
        Shift(f64),
    }
    let (entries, op, scripts, dry) = match command {
        TranscriptsCmd::List { dirs } => {
            let all = tr::scan(&dirs);
            for e in &all {
                let used = e.scripts.iter().map(|s| s.file_name().unwrap_or_default().to_string_lossy().to_string()).collect::<Vec<_>>().join(", ");
                println!("{:<32} {:>5} words  {:>9}  {:<3} {}", e.path.display(), e.word_count(), vg_core::format_time(e.transcript.duration), e.transcript.language, if used.is_empty() { "(no script)".into() } else { used });
            }
            eprintln!("{} transcripts", all.len());
            return Ok(());
        }
        TranscriptsCmd::Replace { find, with, files, partial, match_case, no_scripts, dry_run } => (pick(&files), Op::Replace(find, with, tr::MatchOpts { whole_word: !partial, match_case }), !no_scripts, dry_run),
        TranscriptsCmd::Fillers { files, words, no_scripts, dry_run } => {
            let words = words.unwrap_or_else(|| cfg.fillers.clone());
            (pick(&files), Op::Fillers(words.split(',').map(|w| w.trim().to_string()).filter(|w| !w.is_empty()).collect()), !no_scripts, dry_run)
        }
        TranscriptsCmd::Shift { seconds, files, no_scripts, dry_run } => (pick(&files), Op::Shift(seconds), !no_scripts, dry_run),
    };
    if entries.is_empty() {
        bail!("no transcripts found there");
    }
    let mut done = std::collections::HashSet::new();
    for e in &entries {
        let mut t = e.transcript.clone();
        let n = match &op {
            Op::Replace(f, w, m) => tr::replace(&mut t, f, w, *m),
            Op::Fillers(list) => tr::remove(&mut t, list),
            Op::Shift(s) => tr::shift(&mut t, *s),
        };
        println!("{}: {n}", e.path.display());
        if n > 0 && !dry {
            t.save(&e.path)?;
        }
        if !scripts {
            continue;
        }
        for s in &e.scripts {
            if !done.insert(s.clone()) {
                continue;
            }
            let text = std::fs::read_to_string(s)?;
            let (new, k) = match &op {
                Op::Replace(f, w, m) => tr::replace_in_script(&text, f, w, *m),
                Op::Fillers(list) => tr::remove_in_script(&text, list),
                Op::Shift(sec) => {
                    let new = vg_core::cues::remap_times(&text, |t| (t + sec).max(0.0)).map_err(|e| anyhow::anyhow!("{}: {e}", s.display()))?;
                    let k = usize::from(new != text);
                    (new, k)
                }
            };
            println!("  {}: {k}", s.display());
            if k > 0 && !dry {
                std::fs::write(s, new)?;
            }
        }
    }
    if dry {
        eprintln!("dry run: nothing was written");
    }
    Ok(())
}
