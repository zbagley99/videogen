//! The transcribe window: run Whisper on the narration and replace the script
//! with a timed, pose-tagged cue sheet.

use super::util::{file_name, mtime};
use super::{VgApp, place, theme};
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use vg_core::Transcript;
use vg_project::TranscribeOptions;

pub(super) const MODELS: &[(&str, &str)] = &[
    ("large-v3-turbo", "best balance (default)"),
    ("large-v3", "most accurate, slower"),
    ("medium.en", "English only, lighter"),
    ("small.en", "English only, fastest"),
];

#[derive(Default)]
pub struct TranscribePanel {
    pub open: bool,
    pub audio: Option<PathBuf>,
    task: Option<Task>,
    /// A finished transcript waiting for "replace the unsaved script?" confirmation.
    pending: Option<(PathBuf, Transcript, bool)>,
}

impl TranscribePanel {
    pub fn busy(&self) -> bool {
        self.task.is_some()
    }
}

struct Task {
    audio: PathBuf,
    handle: std::thread::JoinHandle<anyhow::Result<Option<Transcript>>>,
    progress: Arc<Mutex<(f64, f64, String)>>,
    cancel: Arc<AtomicBool>,
    started: Instant,
}

pub fn window(app: &mut VgApp, ctx: &egui::Context) {
    finish(app, ctx);
    confirm(app, ctx);
    if !app.transcribe.open {
        return;
    }
    if app.transcribe.audio.is_none() {
        app.transcribe.audio = app.parsed.audio_path.clone();
    }
    let mut open = app.transcribe.open;
    let pos = app.place.place(ctx, "transcribe", egui::Align2::RIGHT_BOTTOM, egui::vec2(470.0, 380.0));
    place::at(egui::Window::new(theme::caption("Transcribe narration").color(theme::WHITE)), "transcribe", pos).open(&mut open).resizable(false).collapsible(false).default_width(440.0).show(ctx, |ui| {
        let busy = app.transcribe.task.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            egui::Grid::new("transcribe").num_columns(2).show(ui, |ui| {
                ui.label(theme::caption("Audio"));
                ui.horizontal(|ui| {
                    ui.set_max_width(300.0);
                    let name = app.transcribe.audio.as_deref().map(file_name).unwrap_or_else(|| "none chosen".into());
                    let full = app.transcribe.audio.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                    ui.add(egui::Label::new(egui::RichText::new(name).color(theme::WHITE)).truncate()).on_hover_text(full);
                    if theme::tool_button(ui, "Choose", None, false).clicked() {
                        let d = rfd::FileDialog::new().add_filter("audio", vg_project::transcripts::AUDIO_EXTS).set_directory(app.cue_dir());
                        super::dialog::open(app, super::dialog::Want::TranscribeAudio { open: false }, d, super::dialog::How::File);
                    }
                });
                ui.end_row();
                ui.label(theme::caption("Model"));
                egui::ComboBox::from_id_salt("model").selected_text(&app.prefs.transcribe_model).show_ui(ui, |ui| {
                    for (m, note) in MODELS {
                        ui.selectable_value(&mut app.prefs.transcribe_model, m.to_string(), format!("{m}  ({note})"));
                    }
                });
                ui.end_row();
                ui.label(theme::caption("Language"));
                ui.add(egui::TextEdit::singleline(&mut app.prefs.transcribe_language).hint_text("auto (or en, de, …)").desired_width(120.0));
                ui.end_row();
                ui.label(theme::caption("Poses"));
                ui.horizontal(|ui| {
                    ui.checkbox(&mut app.prefs.transcribe_autotag, "suggest pose tags");
                    if app.prefs.transcribe_autotag {
                        theme::segmented(ui, &mut app.prefs.transcribe_llm, &[(false, "Rules"), (true, "LLM")]);
                    }
                });
                ui.end_row();
            });
            theme::note(ui, "Runs Whisper locally on the GPU. The first run downloads the model (~1.6 GB for turbo). Writes <audio>.words.json and <audio>.md next to the audio. A recording that already has a saved transcript uses it; only Transcribe again runs Whisper over it.");
        });
        ui.separator();
        match &app.transcribe.task {
            Some(task) => {
                let (done, total, status) = task.progress.lock().map(|p| p.clone()).unwrap_or_default();
                let frac = if total > 0.0 { (done / total) as f32 } else { 0.0 };
                let text = if total > 0.0 { format!("{} / {}", vg_core::format_time(done), vg_core::format_time(total)) } else { format!("starting… {:.0}s", task.started.elapsed().as_secs_f32()) };
                ui.add(egui::ProgressBar::new(frac).text(text).animate(total <= 0.0));
                if !status.is_empty() {
                    theme::note(ui, status);
                }
                if theme::tool_button(ui, "Cancel", None, false).clicked() {
                    task.cancel.store(true, Ordering::Relaxed);
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
            None => match app.transcribe.audio.as_deref().and_then(saved) {
                Some((words, n, modified)) => {
                    let name = file_name(&words);
                    ui.label(egui::RichText::new(format!("Saved transcript: {name} · {n} words · {}", super::util::ago(modified))).size(11.0).color(theme::TEXT2));
                    ui.horizontal(|ui| {
                        if theme::tool_button(ui, "Use saved transcript", None, true).clicked() {
                            start(app, ctx);
                        }
                        if theme::tool_button(ui, "Transcribe again", None, false).clicked() {
                            run_whisper(app, ctx);
                        }
                    });
                }
                None => {
                    if theme::tool_button_enabled(ui, "Transcribe", None, true, app.transcribe.audio.is_some()).clicked() {
                        run_whisper(app, ctx);
                    }
                }
            },
        }
    });
    app.transcribe.open = open;
}

/// The transcript saved next to `audio` (`<stem>.words.json`): path, word
/// count and when it was written.
pub(super) fn saved(audio: &std::path::Path) -> Option<(PathBuf, usize, Option<std::time::SystemTime>)> {
    let (words, _) = vg_project::transcript_paths(audio);
    let t = Transcript::load(&words).ok()?;
    let n = t.words().len();
    (n > 0).then(|| (words.clone(), n, mtime(&words)))
}

/// Show the window for `audio`.
pub(super) fn open_for(app: &mut VgApp, audio: PathBuf) {
    app.transcribe.audio = Some(audio);
    app.transcribe.open = true;
}

/// Make a script for `app.transcribe.audio`: from its saved transcript when
/// there is one (Whisper only runs on audio that has none; re-running it is
/// the Transcribe again button's job).
pub(super) fn start(app: &mut VgApp, ctx: &egui::Context) {
    let Some(audio) = app.transcribe.audio.clone() else { return };
    let Some((words, _, _)) = saved(&audio) else { return run_whisper(app, ctx) };
    match Transcript::load(&words) {
        Ok(t) => deliver(app, ctx, audio, t, false),
        Err(e) => app.error(format!("{e:#}")),
    }
}

/// Run Whisper on `app.transcribe.audio` (replacing its saved transcript).
pub(super) fn run_whisper(app: &mut VgApp, ctx: &egui::Context) {
    if app.transcribe.task.is_some() {
        app.error("a transcription is already running");
        return;
    }
    let Some(audio) = app.transcribe.audio.clone() else { return };
    let opts = TranscribeOptions {
        model: app.prefs.transcribe_model.clone(),
        language: Some(app.prefs.transcribe_language.trim().to_string()).filter(|l| !l.is_empty()),
    };
    let progress = Arc::new(Mutex::new((0.0, 0.0, String::new())));
    let cancel = Arc::new(AtomicBool::new(false));
    let (p, c, ctx2, a) = (progress.clone(), cancel.clone(), ctx.clone(), audio.clone());
    let handle = std::thread::spawn(move || {
        let p2 = p.clone();
        let r = vg_project::transcribe(
            &a,
            &opts,
            &c,
            |done, total| {
                if let Ok(mut g) = p.lock() {
                    g.0 = done;
                    g.1 = total;
                }
            },
            |line| {
                if let Ok(mut g) = p2.lock() {
                    g.2 = line.chars().take(120).collect();
                }
            },
        );
        ctx2.request_repaint();
        r
    });
    app.transcribe.task = Some(Task { audio, handle, progress, cancel, started: Instant::now() });
}

fn finish(app: &mut VgApp, ctx: &egui::Context) {
    if !app.transcribe.task.as_ref().is_some_and(|t| t.handle.is_finished()) {
        return;
    }
    let task = app.transcribe.task.take().unwrap();
    match task.handle.join() {
        Ok(Ok(Some(t))) => deliver(app, ctx, task.audio, t, true),
        Ok(Ok(None)) => app.info("transcription cancelled"),
        Ok(Err(e)) => app.error(format!("{e:#}")),
        Err(_) => app.error("transcription crashed"),
    }
}

/// Open a script made from `t`, asking first when that would drop unsaved edits.
fn deliver(app: &mut VgApp, ctx: &egui::Context, audio: PathBuf, t: Transcript, fresh: bool) {
    if app.cue_text != app.cue_saved_text && !app.cue_text.is_empty() {
        app.transcribe.pending = Some((audio, t, fresh));
    } else {
        apply(app, ctx, audio, t, fresh);
    }
}

/// Unsaved script edits would be lost: ask first.
fn confirm(app: &mut VgApp, ctx: &egui::Context) {
    if app.transcribe.pending.is_none() {
        return;
    }
    let mut choice = None;
    egui::Modal::new(egui::Id::new("transcribe-replace")).show(ctx, |ui| {
        ui.heading("Replace the script?");
        ui.label(theme::caption("The transcript is ready, but the current script has unsaved changes."));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if theme::tool_button(ui, "Save it, then open the transcript", None, false).clicked() {
                choice = Some(true);
            }
            if theme::tool_button(ui, "Discard changes", None, false).clicked() {
                choice = Some(false);
            }
            if theme::tool_button(ui, "Cancel", None, false).clicked() {
                app.transcribe.pending = None;
            }
        });
    });
    if let Some(save) = choice {
        if save {
            match app.save_script() {
                Ok(true) => {}
                // Asking where to save it; the choice comes back after.
                Ok(false) => return,
                Err(e) => {
                    app.error(format!("{e:#}"));
                    return;
                }
            }
        }
        if let Some((audio, t, fresh)) = app.transcribe.pending.take() {
            apply(app, ctx, audio, t, fresh);
        }
    }
}

/// `fresh`: `t` just came from Whisper and is saved over `<stem>.words.json`;
/// otherwise it was read from there and the file is left alone.
fn apply(app: &mut VgApp, ctx: &egui::Context, audio: PathBuf, t: Transcript, fresh: bool) {
    let (words, mut cue_path) = vg_project::transcript_paths(&audio);
    // Don't clobber a sheet that's already there (e.g. an earlier, edited transcription).
    let stem = audio.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut n = 2;
    while cue_path.exists() {
        cue_path = cue_path.with_file_name(format!("{stem}-{n}.md"));
        n += 1;
    }
    let cue_dir = cue_path.parent().map(PathBuf::from).unwrap_or_default();
    // With the LLM, the rules stay out of it and the LLM runs on the new script.
    let llm = app.prefs.transcribe_autotag && app.prefs.transcribe_llm;
    let poses = (app.prefs.transcribe_autotag && !llm).then_some(&app.poses).filter(|p| !p.poses.is_empty());
    let sheet = if fresh {
        vg_project::cue_sheet_for(&t, &audio, &cue_dir, poses, &app.prefs.transcribe_model)
    } else {
        let rel = |p: &std::path::Path| vg_core::paths::relative_path(p, &cue_dir).to_string_lossy().to_string();
        let note = format!("Made from the saved transcript {}.", file_name(&words));
        Ok(vg_core::transcript::cue_sheet(&t, &rel(&audio), &rel(&words), poses, &note))
    };
    match sheet {
        Ok(text) => {
            if let Err(e) = std::fs::write(&cue_path, &text) {
                app.error(format!("writing {}: {e}", cue_path.display()));
                return;
            }
            app.cue_text = text.clone();
            app.cue_saved_text = text;
            app.cue_path = Some(cue_path.clone());
            app.prefs_win.transcripts.check_limit = true;
            app.prefs_win.transcripts.rescan = true;
            app.reparse();
            app.timeline.fit_all();
            app.transcribe.open = false;
            let lines = app.parsed.timeline.as_ref().map(|t| t.spans.len()).unwrap_or(0);
            app.info(format!("{} {lines} lines into {}", if fresh { "transcribed" } else { "loaded the saved transcript:" }, cue_path.display()));
            if llm {
                super::assist::start(app, ctx, vg_llm::Mode::Replace);
            }
        }
        Err(e) => app.error(format!("{e:#}")),
    }
}
